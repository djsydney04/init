//! Saved SSH destinations; HTTP travels over SSH stdio without opening a local port.
use anyhow::{Context, Result, bail, ensure};
use axum::{body::Bytes, extract::Query};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Method, Request, client::conn::http1};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    process::Command,
};

const MAX_RESPONSE: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub host: String,
    /// None respects the SSH config's port.
    pub ssh_port: Option<u16>,
    pub init_port: u16,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_id(&self.id),
            "Use a machine ID starting with a lowercase letter, followed by lowercase letters, digits or hyphens (up to 48 characters)"
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 128
                && !self.name.chars().any(char::is_control),
            "Enter a machine name (up to 128 characters)"
        );
        let (user, host) = self
            .host
            .split_once('@')
            .map_or((None, self.host.as_str()), |(user, host)| {
                (Some(user), host)
            });
        ensure!(
            self.host.len() <= 320
                && !host.is_empty()
                && !host.starts_with('-')
                && host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._:-[]".contains(&c)),
            "Enter an SSH alias, hostname or IP address, optionally prefixed by user@"
        );
        if let Some(user) = user {
            ensure!(
                !user.is_empty()
                    && !user.starts_with('-')
                    && user
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c)),
                "Invalid SSH username"
            );
        }
        ensure!(
            self.ssh_port != Some(0) && self.init_port > 0,
            "Ports must be between 1 and 65535"
        );
        Ok(())
    }

    fn ssh_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-T",
            "-a",
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oNoHostAuthenticationForLocalhost=no",
            "-oConnectTimeout=8",
            "-oConnectionAttempts=1",
            "-oServerAliveInterval=15",
            "-oServerAliveCountMax=2",
            "-oClearAllForwardings=yes",
            "-oPermitLocalCommand=no",
            "-oControlMaster=no",
            "-oControlPath=none",
            "-oForkAfterAuthentication=no",
            "-oProxyUseFdpass=no",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if let Some(port) = self.ssh_port {
            args.extend(["-p".into(), port.to_string()]);
        }
        args.extend([
            "-W".into(),
            format!("127.0.0.1:{}", self.init_port),
            "--".into(),
            self.host.clone(),
        ]);
        args
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub app: String,
    pub version: String,
    pub workspace: String,
    pub config: String,
}

impl Identity {
    fn check(&self) -> Result<()> {
        ensure!(
            self.app == "self-host-init",
            "The destination is not an Init controller"
        );
        ensure!(
            self.version == env!("CARGO_PKG_VERSION"),
            "Remote Init {} differs from this client {}; install matching versions",
            self.version,
            env!("CARGO_PKG_VERSION")
        );
        ensure!(
            !self.workspace.is_empty() && !self.config.is_empty(),
            "Remote controller did not identify its workspace"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub profile: Profile,
    pub identity: Option<Identity>,
    pub revision: String,
}

fn revision() -> String {
    format!("{:032x}", rand::random::<u128>())
}

pub struct Store {
    path: PathBuf,
    machines: Vec<Machine>,
}

impl Store {
    pub fn load(workspace: &Path) -> Result<Self> {
        let path = workspace.join("machines.json");
        let machines: Vec<Machine> = if path.exists() {
            let bytes = fs::read(&path)?;
            crate::manifest_io::decode(&path, &bytes)?
        } else {
            Vec::new()
        };
        ensure!(
            machines.len() <= 100,
            "At most 100 machine connections are supported"
        );
        let mut ids = std::collections::HashSet::new();
        for machine in &machines {
            machine.profile.validate()?;
            ensure!(ids.insert(&machine.profile.id), "Duplicate machine ID");
        }
        Ok(Self { path, machines })
    }
    pub fn list(&self) -> &[Machine] {
        &self.machines
    }
    pub fn get(&self, id: &str) -> Result<Machine> {
        self.machines
            .iter()
            .find(|m| m.profile.id == id)
            .cloned()
            .context("Machine connection not found")
    }
    fn save(&mut self, machines: Vec<Machine>) -> Result<()> {
        let bytes = crate::manifest_io::encode(&self.path, &machines)?;
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", rand::random::<u64>()));
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.path)?;
            Ok::<_, anyhow::Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result?;
        self.machines = machines;
        Ok(())
    }
    pub fn upsert(&mut self, profile: Profile) -> Result<()> {
        profile.validate()?;
        let mut machines = self.machines.clone();
        if let Some(entry) = machines.iter_mut().find(|m| m.profile.id == profile.id) {
            // Changing the destination requires an explicit new identity check.
            let same_target = entry.profile.host == profile.host
                && entry.profile.ssh_port == profile.ssh_port
                && entry.profile.init_port == profile.init_port;
            if !same_target {
                entry.identity = None;
                entry.revision = revision();
            }
            entry.profile = profile;
        } else {
            ensure!(
                machines.len() < 100,
                "At most 100 machine connections are supported"
            );
            machines.push(Machine {
                profile,
                identity: None,
                revision: revision(),
            });
        }
        self.save(machines)
    }
    pub fn verify(&mut self, profile: &Profile, identity: Identity) -> Result<()> {
        identity.check()?;
        let mut machines = self.machines.clone();
        let entry = machines
            .iter_mut()
            .find(|m| &m.profile == profile)
            .context("Connection changed during testing; test it again")?;
        if entry.identity.as_ref() != Some(&identity) {
            entry.revision = revision();
        }
        entry.identity = Some(identity);
        self.save(machines)
    }
    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.get(id)?;
        self.save(
            self.machines
                .iter()
                .filter(|m| m.profile.id != id)
                .cloned()
                .collect(),
        )
    }
}

/// Restrict forwarding to the dashboard API; never forward tokens, profiles or arbitrary URLs.
pub fn allowed_request(method: &Method, path: &str) -> bool {
    if method == Method::GET
        && [
            "/api/state",
            "/api/health",
            "/api/allocation",
            "/api/settings",
        ]
        .contains(&path)
    {
        return true;
    }
    if method == Method::GET && (path == "/api/history" || path.starts_with("/api/history?")) {
        return path.parse().ok().is_some_and(|uri| {
            Query::<crate::history::HistoryQuery>::try_from_uri(&uri)
                .is_ok_and(|Query(query)| query.validate().is_ok())
        });
    }
    if method == Method::POST
        && [
            "/api/services",
            "/api/projects",
            "/api/budget",
            "/api/allocation",
            "/api/settings",
        ]
        .contains(&path)
    {
        return true;
    }
    let Some(rest) = path.strip_prefix("/api/services/") else {
        return false;
    };
    let Some((id, action)) = rest.split_once('/') else {
        return false;
    };
    if !valid_id(id) {
        return false;
    }
    if method == Method::POST {
        return ["start", "stop", "restart", "run", "apply"].contains(&action);
    }
    if method != Method::GET {
        return false;
    }
    if action == "logs" {
        return true;
    }
    action.strip_prefix("logs?tail=").is_some_and(|tail| {
        tail.bytes().all(|c| c.is_ascii_digit())
            && tail.parse::<u16>().is_ok_and(|n| (1..=2000).contains(&n))
    })
}

pub struct Reply {
    pub status: u16,
    pub body: Value,
}

type Sender = http1::SendRequest<Full<Bytes>>;
async fn http_request(
    sender: &mut Sender,
    port: u16,
    method: &Method,
    path: &str,
    body: &[u8],
    token: Option<&str>,
) -> Result<Reply> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Accept", "application/json");
    if method == Method::POST {
        request = request.header("Content-Type", "application/json");
    }
    if let Some(token) = token {
        request = request.header("X-Selfhost-Token", token);
    }
    let response = sender
        .send_request(request.body(Full::new(Bytes::copy_from_slice(body)))?)
        .await?;
    let status = response.status().as_u16();
    ensure!(
        !(300..400).contains(&status),
        "Remote redirects are not followed"
    );
    let bytes = Limited::new(response.into_body(), MAX_RESPONSE)
        .collect()
        .await
        .map_err(|error| anyhow::anyhow!("Remote response failed or exceeded 8 MiB: {error}"))?
        .to_bytes();
    let body = serde_json::from_slice(&bytes).context("Remote controller returned invalid JSON")?;
    Ok(Reply { status, body })
}

async fn exchange(
    io: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    port: u16,
    expected: Option<&Identity>,
    method: &Method,
    path: &str,
    body: &[u8],
) -> Result<Reply> {
    let (mut sender, connection) = http1::handshake(TokioIo::new(io)).await?;
    let requests = async {
        let health =
            http_request(&mut sender, port, &Method::GET, "/api/health", &[], None).await?;
        ensure!(
            health.status == 200,
            "Remote Init health check failed ({})",
            health.status
        );
        let identity: Identity = serde_json::from_value(health.body.clone())
            .context("Destination did not return an Init identity")?;
        identity.check()?;
        if let Some(expected) = expected {
            ensure!(
                *expected == identity,
                "Remote controller identity changed; test this machine again before using it"
            );
        }
        if path == "/api/health" {
            return Ok(health);
        }
        let token = if method == Method::POST {
            let session =
                http_request(&mut sender, port, &Method::GET, "/api/session", &[], None).await?;
            ensure!(session.status == 200, "Remote session request failed");
            Some(
                session.body["token"]
                    .as_str()
                    .context("Remote session omitted token")?
                    .to_owned(),
            )
        } else {
            None
        };
        http_request(&mut sender, port, method, path, body, token.as_deref()).await
    };
    tokio::pin!(connection);
    tokio::select! {
        biased;
        result = requests => result,
        result = &mut connection => { result?; bail!("SSH connection closed before the request completed") }
    }
}

fn request_timeout_seconds(method: &Method, path: &str) -> u64 {
    if method == Method::POST && path == "/api/services" {
        // A GitHub deployment can spend five minutes cloning and thirty minutes building.
        2700
    } else if method == Method::POST {
        600
    } else {
        30
    }
}

pub async fn request(
    machine: &Machine,
    method: &Method,
    path: &str,
    body: &[u8],
    testing: bool,
) -> Result<Reply> {
    machine.profile.validate()?;
    ensure!(
        allowed_request(method, path),
        "This endpoint cannot be accessed over a machine connection"
    );
    ensure!(body.len() <= 16384, "Request exceeds 16 KiB");
    ensure!(
        testing || machine.identity.is_some(),
        "Test this machine connection before opening it"
    );
    ensure!(
        !testing || (method == Method::GET && path == "/api/health"),
        "Testing is read-only"
    );
    let mut child = Command::new("ssh")
        .args(machine.profile.ssh_args())
        .env("SSH_ASKPASS_REQUIRE", "never")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("OpenSSH could not start; install the SSH client on this computer")?;
    let io = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let stderr = child.stderr.take().unwrap();
    let expected = if testing {
        None
    } else {
        machine.identity.as_ref()
    };
    let seconds = request_timeout_seconds(method, path);
    let (result, diagnostic) = tokio::join!(
        async {
            let result = tokio::time::timeout(
                Duration::from_secs(seconds),
                exchange(io, machine.profile.init_port, expected, method, path, body),
            )
            .await;
            let _ = child.kill().await;
            result.context("SSH request timed out; an accepted change may still finish. Inspect the machine before retrying")?
        },
        async {
            let mut bytes = Vec::new();
            let _ = stderr.take(16384).read_to_end(&mut bytes).await;
            String::from_utf8_lossy(&bytes).trim().to_owned()
        }
    );
    // No mutation retries: the remote daemon owns any operation it already accepted.
    result.with_context(|| {
        if diagnostic.is_empty() {
            "SSH connection failed. Check SSH access, forwarding and the remote Init daemon".into()
        } else {
            format!("SSH connection failed: {diagnostic}")
        }
    })
}

pub fn tailscale_summary(value: &Value) -> Value {
    let status = value["BackendState"].as_str().unwrap_or("Unknown");
    let mut peers = Vec::new();
    if status == "Running"
        && let Some(entries) = value["Peer"].as_object()
    {
        for peer in entries.values().take(1000) {
            let dns = peer["DNSName"].as_str().unwrap_or("").trim_end_matches('.');
            let ip = peer["TailscaleIPs"]
                .as_array()
                .and_then(|ips| ips.iter().find_map(Value::as_str))
                .unwrap_or("");
            let host = if dns.is_empty() { ip } else { dns };
            if host.is_empty() {
                continue;
            }
            peers.push(json!({"name":peer["HostName"].as_str().unwrap_or(host),"host":host,"ip":ip,"os":peer["OS"].as_str().unwrap_or("Unknown"),"online":peer["Online"].as_bool()}));
        }
    }
    peers.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    json!({"installed":true,"status":status,"connected":status=="Running","peers":peers})
}

pub async fn tailscale_status() -> Result<Value> {
    #[cfg(target_os = "macos")]
    let executables = [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
    ]
    .as_slice();
    #[cfg(not(target_os = "macos"))]
    let executables = ["tailscale"].as_slice();
    for executable in executables {
        let mut child = match Command::new(executable)
            .args(["status", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        let mut output = child.stdout.take().unwrap().take(1024 * 1024 + 1);
        let read = output.read_to_end(&mut bytes);
        let result = tokio::time::timeout(Duration::from_secs(8), read).await;
        let _ = child.kill().await;
        result.context("Tailscale status timed out; open Tailscale and try again")??;
        ensure!(
            bytes.len() <= 1024 * 1024,
            "Tailscale response exceeds 1 MiB"
        );
        let value: Value = serde_json::from_slice(&bytes)
            .context("Open Tailscale, sign in and connect, then check again")?;
        return Ok(tailscale_summary(&value));
    }
    Ok(json!({"installed":false,"connected":false,"status":"Not installed","peers":[]}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_proxy_allows_clone_and_build_without_extending_other_requests() {
        assert_eq!(
            request_timeout_seconds(&Method::POST, "/api/services"),
            2700
        );
        assert_eq!(request_timeout_seconds(&Method::GET, "/api/services"), 30);
        assert_eq!(request_timeout_seconds(&Method::GET, "/api/state"), 30);
        assert_eq!(
            request_timeout_seconds(&Method::POST, "/api/services/web/start"),
            600
        );
        assert_eq!(
            request_timeout_seconds(&Method::POST, "/api/allocation"),
            600
        );
    }
    use std::sync::{Arc, Mutex};

    fn profile() -> Profile {
        Profile {
            id: "linux".into(),
            name: "Linux".into(),
            host: "operator@linux.example".into(),
            ssh_port: None,
            init_port: 8787,
        }
    }
    fn identity() -> Identity {
        Identity {
            app: "self-host-init".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            workspace: "/srv/init".into(),
            config: "/srv/init/services.yaml".into(),
        }
    }

    #[test]
    fn validates_destinations_and_never_builds_a_remote_shell_command() {
        for host in [
            "server",
            "user@server.tailnet.ts.net",
            "192.0.2.1",
            "::1",
            "work-mac",
        ] {
            let mut profile = profile();
            profile.host = host.into();
            profile.validate().unwrap();
            let args = profile.ssh_args();
            assert_eq!(
                &args[args.len() - 4..],
                &["-W", "127.0.0.1:8787", "--", host]
            );
            for option in [
                "-oBatchMode=yes",
                "-oStrictHostKeyChecking=yes",
                "-oNoHostAuthenticationForLocalhost=no",
                "-oPermitLocalCommand=no",
                "-oControlPath=none",
            ] {
                assert!(args.iter().any(|arg| arg == option));
            }
            assert!(!args.iter().any(|arg| arg == "-p"));
        }
        for host in [
            "-oProxyCommand=bad",
            "user@-bad",
            "user@host other",
            "$(command)",
            "host;command",
            "user@@host",
            "",
            "user@host\n",
        ] {
            let mut profile = profile();
            profile.host = host.into();
            assert!(profile.validate().is_err(), "{host}");
        }
        let mut profile = profile();
        profile.ssh_port = Some(2222);
        assert!(
            profile
                .ssh_args()
                .windows(2)
                .any(|args| args == ["-p", "2222"])
        );
        profile.init_port = 0;
        assert!(profile.validate().is_err());
    }

    #[test]
    fn saves_identity_and_invalidates_old_views_when_destination_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(dir.path()).unwrap();
        store.upsert(profile()).unwrap();
        assert!(store.get("linux").unwrap().identity.is_none());
        store.verify(&profile(), identity()).unwrap();
        let verified = store.get("linux").unwrap();
        let mut renamed = profile();
        renamed.name = "Build machine".into();
        store.upsert(renamed.clone()).unwrap();
        assert_eq!(store.get("linux").unwrap().revision, verified.revision);
        assert_eq!(
            Store::load(dir.path())
                .unwrap()
                .get("linux")
                .unwrap()
                .identity,
            Some(identity())
        );
        let mut changed = renamed.clone();
        changed.host = "another-machine".into();
        store.upsert(changed.clone()).unwrap();
        assert_ne!(store.get("linux").unwrap().revision, verified.revision);
        assert!(store.get("linux").unwrap().identity.is_none());
        assert!(store.verify(&renamed, identity()).is_err());
        store.verify(&changed, identity()).unwrap();
        let verified = store.get("linux").unwrap();
        let mut moved_workspace = identity();
        moved_workspace.workspace = "/srv/other".into();
        store.verify(&changed, moved_workspace).unwrap();
        assert_ne!(store.get("linux").unwrap().revision, verified.revision);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.path().join("machines.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        store.remove("linux").unwrap();
        assert!(Store::load(dir.path()).unwrap().list().is_empty());
    }

    #[test]
    fn proxy_only_accepts_dashboard_operations() {
        for path in [
            "/api/state",
            "/api/health",
            "/api/allocation",
            "/api/settings",
            "/api/services/web/logs?tail=200",
        ] {
            assert!(allowed_request(&Method::GET, path));
        }
        for path in [
            "/api/services",
            "/api/projects",
            "/api/budget",
            "/api/allocation",
            "/api/services/web/restart",
            "/api/settings",
        ] {
            assert!(allowed_request(&Method::POST, path));
        }
        for path in [
            "/api/session",
            "/api/machines",
            "https://example.com/api/state",
            "//example.com/api/state",
            "/api/services/../start",
            "/api/services/web/logs?tail=0",
            "/api/services/web/logs?tail=2001",
            "/api/services/web/logs?tail=20&other=1",
            "/api/state?other=1",
        ] {
            assert!(!allowed_request(&Method::GET, path), "{path}");
            assert!(!allowed_request(&Method::POST, path), "{path}");
        }
        assert!(!allowed_request(&Method::GET, "/api/services/web/stop"));
        assert!(!allowed_request(&Method::DELETE, "/api/services/web/stop"));
    }

    #[test]
    fn proxy_history_uses_shared_bounded_query_validation_and_settings_stays_exact() {
        for path in [
            "/api/history",
            "/api/history?window_seconds=86400&max_points=720",
            "/api/history?project=__default__&window_seconds=60&max_points=1",
            "/api/history?project=build-tools",
            "/api/history?service=worker&window_seconds=300&max_points=1440",
            "/api/history?service=%77orker",
        ] {
            assert!(allowed_request(&Method::GET, path), "{path}");
            assert!(!allowed_request(&Method::POST, path), "{path}");
        }
        for path in [
            "/api/history?window_seconds=0",
            "/api/history?window_seconds=86401",
            "/api/history?max_points=0",
            "/api/history?max_points=1441",
            "/api/history?project=tools&service=worker",
            "/api/history?service=../worker",
            "/api/history?service=%2Fworker",
            "/api/history?service=worker&service=other",
            "/api/history?window_seconds=60&window_seconds=300",
            "/api/history?unknown=1",
            "/api/history?end=18446744073709551615",
            "/api/history/../settings",
            "/api/settings?other=1",
            "/api/settings/extra",
            "/api/session",
            "/api/machines",
            "/api/machines/remote/proxy/history",
            "https://example.com/api/history",
            "//example.com/api/settings",
        ] {
            assert!(!allowed_request(&Method::GET, path), "{path}");
            assert!(!allowed_request(&Method::POST, path), "{path}");
        }
        assert!(allowed_request(&Method::GET, "/api/settings"));
        assert!(allowed_request(&Method::POST, "/api/settings"));
        assert!(!allowed_request(&Method::DELETE, "/api/settings"));
    }

    #[test]
    fn tailscale_returns_only_connection_metadata_and_handles_logged_out_clients() {
        let value = json!({"BackendState":"Running", "User":{"secret":"not exposed"}, "Peer":{
            "one":{"HostName":"Mac", "DNSName":"mac.example.ts.net.", "TailscaleIPs":["100.64.0.1"], "OS":"macOS", "Online":true, "PublicKey":"not exposed"},
            "two":{"HostName":"Linux", "TailscaleIPs":["100.64.0.2"], "OS":"linux", "Online":false},
            "three":{"HostName":"No address"}
        }});
        let summary = tailscale_summary(&value);
        assert_eq!(summary["peers"].as_array().unwrap().len(), 2);
        assert_eq!(summary["peers"][0]["host"], "100.64.0.2");
        assert_eq!(summary["peers"][0]["online"], false);
        assert_eq!(summary["peers"][1]["host"], "mac.example.ts.net");
        assert!(!summary.to_string().contains("not exposed"));
        let mut logged_out = value;
        logged_out["BackendState"] = json!("NeedsLogin");
        assert_eq!(tailscale_summary(&logged_out)["peers"], json!([]));
        assert_eq!(tailscale_summary(&json!({}))["connected"], false);
    }

    type Calls = Arc<Mutex<Vec<(Method, String, String, Option<String>, Vec<u8>)>>>;
    fn remote(identity: Identity, status: u16, body: String) -> (tokio::io::DuplexStream, Calls) {
        let (client, server) = tokio::io::duplex(4096);
        let calls: Calls = Arc::default();
        let seen = calls.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    let seen = seen.clone();
                    let identity = identity.clone();
                    let body = body.clone();
                    async move {
                        let (parts, incoming) = request.into_parts();
                        let bytes = incoming.collect().await.unwrap().to_bytes();
                        let path = parts.uri.to_string();
                        seen.lock().unwrap().push((
                            parts.method,
                            path.clone(),
                            parts.headers["host"].to_str().unwrap().into(),
                            parts
                                .headers
                                .get("x-selfhost-token")
                                .map(|v| v.to_str().unwrap().into()),
                            bytes.to_vec(),
                        ));
                        let (status, body) = match path.as_str() {
                            "/api/health" => (200, serde_json::to_string(&identity).unwrap()),
                            "/api/session" => (200, json!({"token":"remote-token"}).to_string()),
                            _ => (status, body),
                        };
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .body(Full::new(Bytes::from(body)))
                                .unwrap(),
                        )
                    }
                },
            );
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server), service)
                .await;
        });
        (client, calls)
    }

    #[tokio::test]
    async fn remote_mutation_checks_identity_uses_remote_token_and_never_retries() {
        let (io, calls) = remote(
            identity(),
            503,
            json!({"error":"operation failed"}).to_string(),
        );
        let reply = exchange(
            io,
            9876,
            Some(&identity()),
            &Method::POST,
            "/api/services/web/stop",
            b"{}",
        )
        .await
        .unwrap();
        assert_eq!(reply.status, 503);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].1, "/api/health");
        assert_eq!(calls[0].3, None);
        assert_eq!(calls[1].1, "/api/session");
        assert_eq!(calls[1].3, None);
        assert_eq!(calls[2].0, Method::POST);
        assert_eq!(calls[2].1, "/api/services/web/stop");
        assert_eq!(calls[2].3.as_deref(), Some("remote-token"));
        assert_eq!(calls[2].4, b"{}");
        assert!(calls.iter().all(|call| call.2 == "127.0.0.1:9876"));
    }

    #[tokio::test]
    async fn remote_identity_drift_blocks_mutations_before_fetching_a_token() {
        let mut changed = identity();
        changed.workspace = "/other/workspace".into();
        let (io, calls) = remote(changed, 200, "{}".into());
        assert!(
            exchange(
                io,
                8787,
                Some(&identity()),
                &Method::POST,
                "/api/services/web/start",
                b"{}"
            )
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("identity changed")
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn history_and_settings_use_the_selected_remote_controller_and_its_token() {
        for (method, path, payload, expected) in [
            (
                Method::GET,
                "/api/history?service=worker&window_seconds=86400&max_points=720",
                b"".as_slice(),
                json!({"scope":"service","id":"worker","points":[{"cpu_percent":12.}]}),
            ),
            (
                Method::POST,
                "/api/settings",
                b"{\"max_parallel_jobs\":3}".as_slice(),
                json!({"ok":true,"controller":{"max_parallel_jobs":3}}),
            ),
        ] {
            assert!(allowed_request(&method, path));
            let (io, calls) = remote(identity(), 200, expected.to_string());
            let reply = exchange(io, 9876, Some(&identity()), &method, path, payload)
                .await
                .unwrap();
            assert_eq!(reply.body, expected);
            let calls = calls.lock().unwrap();
            let forwarded = calls.last().unwrap();
            assert_eq!(forwarded.0, method);
            assert_eq!(forwarded.1, path);
            assert_eq!(forwarded.2, "127.0.0.1:9876");
            assert_eq!(forwarded.4, payload);
            if method == Method::POST {
                assert_eq!(calls.len(), 3);
                assert_eq!(forwarded.3.as_deref(), Some("remote-token"));
            } else {
                assert_eq!(calls.len(), 2);
                assert!(forwarded.3.is_none());
            }
        }
    }

    #[tokio::test]
    async fn remote_reads_preserve_logs_and_bound_untrusted_responses() {
        let (io, calls) = remote(identity(), 200, json!({"logs":"hello\nworld"}).to_string());
        let reply = exchange(
            io,
            8787,
            Some(&identity()),
            &Method::GET,
            "/api/services/web/logs?tail=200",
            &[],
        )
        .await
        .unwrap();
        assert_eq!(reply.body["logs"], "hello\nworld");
        assert_eq!(calls.lock().unwrap().len(), 2);
        for (status, body, expected) in [
            (302, "{}".into(), "redirects"),
            (200, "not json".into(), "invalid JSON"),
            (200, "x".repeat(MAX_RESPONSE + 1), "8 MiB"),
        ] {
            let (io, _) = remote(identity(), status, body);
            let error = exchange(io, 8787, Some(&identity()), &Method::GET, "/api/state", &[])
                .await
                .err()
                .unwrap();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }
}
