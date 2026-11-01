use crate::config::{Limits, Service};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

const OWNER_LABEL: &str = "io.self-host-init.workspace";
const SERVICE_LABEL: &str = "io.self-host-init.service";
const SPEC_LABEL: &str = "io.self-host-init.spec";
const MAX_OUTPUT: usize = 1024 * 1024;

async fn read_bounded(mut source: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let length = source.read(&mut buffer).await?;
        if length == 0 {
            return Ok(output);
        }
        ensure!(
            output.len() + length <= MAX_OUTPUT,
            "command output exceeded 1 MiB; narrow the request"
        );
        output.extend_from_slice(&buffer[..length]);
    }
}

async fn execute(args: &[String], seconds: u64, combine_output: bool) -> Result<String> {
    let (program, arguments) = args.split_first().context("empty command")?;
    ensure!(
        !program.is_empty() && args.iter().all(|arg| !arg.contains('\0')),
        "invalid command argument"
    );
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let result = timeout(Duration::from_secs(seconds), async {
        let (status, stdout, stderr) = tokio::try_join!(
            async { Ok::<_, anyhow::Error>(child.wait().await?) },
            read_bounded(stdout),
            read_bounded(stderr),
        )?;
        ensure!(
            status.success(),
            "{program} failed ({}): {}",
            status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(2048)
                .collect::<String>()
                .trim()
        );
        let mut output = String::from_utf8_lossy(&stdout).into_owned();
        if combine_output {
            output.push_str(&String::from_utf8_lossy(&stderr));
        }
        Ok(output)
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            bail!("{program} timed out after {seconds}s")
        }
    }
}

/// Commands are argument arrays; no shell or interpolated command string is used.
pub async fn run(args: &[String]) -> Result<String> {
    execute(args, 30, false).await
}

fn append(base: &[String], args: &[String]) -> Vec<String> {
    base.iter().cloned().chain(args.iter().cloned()).collect()
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn absent(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("no such container")
        || message.contains("no such object")
        || message.contains("no such network")
        || message.contains("network") && message.contains("not found")
}

pub struct Docker {
    pub command: Vec<String>,
    pub owner: String,
    pub network: String,
}

impl Docker {
    pub fn new(command: Vec<String>, workspace: &Path) -> Self {
        let absolute = workspace.canonicalize().unwrap_or_else(|_| {
            if workspace.is_absolute() {
                workspace.to_owned()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(workspace)
            }
        });
        let owner = digest(absolute.to_string_lossy().as_bytes())[..16].to_owned();
        Self {
            command,
            network: format!("selfhost-{owner}"),
            owner,
        }
    }

    async fn call(&self, args: &[String]) -> Result<String> {
        run(&append(&self.command, args)).await
    }

    pub fn name(&self, service: &Service) -> String {
        if service.observe_only {
            service
                .container
                .clone()
                .unwrap_or_else(|| service.id.clone())
        } else {
            format!("selfhost-{}-{}", self.owner, service.id)
        }
    }

    pub async fn info(&self) -> Value {
        match self
            .call(&strings(&["info", "--format", "{{json .}}"]))
            .await
            .and_then(|output| Ok(serde_json::from_str::<Value>(&output)?))
        {
            Ok(info) => {
                json!({"available":true,"cpus":info["NCPU"],"memory_mb":info["MemTotal"].as_u64().map(|bytes| bytes / 1024 / 1024),"architecture":info["Architecture"],"server_version":info["ServerVersion"],"error":null})
            }
            Err(error) => {
                json!({"available":false,"cpus":null,"memory_mb":null,"error":error.to_string()})
            }
        }
    }

    pub async fn inventory(&self) -> Result<Vec<Value>> {
        let output = self.call(&strings(&["ps", "-a", "--format", "{\"ID\":{{json .ID}},\"Names\":{{json .Names}},\"Image\":{{json .Image}},\"State\":{{json .State}},\"Status\":{{json .Status}}}"])).await?;
        output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    }

    pub async fn inspect_name(&self, name: &str) -> Result<Option<Value>> {
        ensure!(
            !name.is_empty() && !name.starts_with('-'),
            "invalid container name"
        );
        match self
            .call(&strings(&[
                "container",
                "inspect",
                "--format",
                "{{json .}}",
                name,
            ]))
            .await
        {
            Ok(output) => {
                let value: Value = serde_json::from_str(&output)?;
                ensure!(value.is_object(), "unexpected Docker inspect response");
                Ok(Some(value))
            }
            Err(error) if absent(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn inspect(&self, service: &Service) -> Result<Option<Value>> {
        self.inspect_name(&self.name(service)).await
    }

    pub async fn stats(&self) -> Result<HashMap<String, Value>> {
        let output = self.call(&strings(&["stats", "--no-stream", "--format", "{\"Name\":{{json .Name}},\"CPUPerc\":{{json .CPUPerc}},\"MemUsage\":{{json .MemUsage}},\"MemPerc\":{{json .MemPerc}},\"NetIO\":{{json .NetIO}},\"BlockIO\":{{json .BlockIO}},\"PIDs\":{{json .PIDs}}}"])).await?;
        let mut stats = HashMap::new();
        for line in output.lines().filter(|line| !line.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)?;
            let Some(name) = record["Name"].as_str() else {
                continue;
            };
            let cpu = record["CPUPerc"].as_str().and_then(percent);
            let memory = record["MemUsage"]
                .as_str()
                .and_then(|value| value.split('/').next())
                .and_then(memory_mb);
            stats.insert(name.to_owned(), json!({"cpu_percent":cpu,"memory_mb":memory,"memory_usage":record["MemUsage"],"memory_percent":record["MemPerc"].as_str().and_then(percent),"net_io":record["NetIO"],"block_io":record["BlockIO"],"pids":record["PIDs"].as_str().and_then(|value| value.parse::<u64>().ok())}));
        }
        Ok(stats)
    }

    fn ensure_owned(&self, service: &Service, inspection: &Value) -> Result<()> {
        ensure!(
            service.runtime == "docker" && service.managed(),
            "{} is observe-only",
            service.id
        );
        ensure!(
            inspection["Config"]["Labels"][OWNER_LABEL].as_str() == Some(self.owner.as_str())
                && inspection["Config"]["Labels"][SERVICE_LABEL].as_str()
                    == Some(service.id.as_str()),
            "refusing to modify {}: container is not owned by this workspace and service",
            self.name(service)
        );
        Ok(())
    }

    fn ensure_spec(&self, service: &Service, inspection: &Value) -> Result<()> {
        ensure!(
            inspection["Config"]["Labels"][SPEC_LABEL].as_str()
                == Some(spec_hash(service)?.as_str()),
            "{} creation settings changed; stop and recreate it to apply the new configuration",
            service.id
        );
        Ok(())
    }

    async fn ensure_network(&self) -> Result<()> {
        let args = strings(&[
            "network",
            "inspect",
            "--format",
            "{{json .Labels}}",
            &self.network,
        ]);
        match self.call(&args).await {
            Ok(output) => {
                let labels: Value = serde_json::from_str(&output)?;
                ensure!(
                    labels[OWNER_LABEL].as_str() == Some(self.owner.as_str()),
                    "network {} belongs to another owner",
                    self.network
                );
                Ok(())
            }
            Err(error) if absent(&error) => {
                self.call(&strings(&[
                    "network",
                    "create",
                    "--driver",
                    "bridge",
                    "--label",
                    &format!("{OWNER_LABEL}={}", self.owner),
                    &self.network,
                ]))
                .await?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub async fn create(&self, service: &Service) -> Result<()> {
        ensure!(
            service.runtime == "docker" && service.managed(),
            "{} is observe-only",
            service.id
        );
        ensure!(
            self.inspect(service).await?.is_none(),
            "{} already exists; update its limits or stop and recreate it",
            self.name(service)
        );
        self.ensure_network().await?;
        let args = self.create_args(service)?;
        execute(&append(&self.command, &args), 180, false).await?;
        self.call(&strings(&["start", &self.name(service)])).await?;
        Ok(())
    }

    fn create_args(&self, service: &Service) -> Result<Vec<String>> {
        let limits = service
            .limits
            .as_ref()
            .context("managed service requires limits")?;
        limits.validate()?;
        let memory = format!("{}m", limits.memory_mb);
        let mut args = strings(&[
            "create",
            "--name",
            &self.name(service),
            "--label",
            &format!("{OWNER_LABEL}={}", self.owner),
            "--label",
            &format!("{SERVICE_LABEL}={}", service.id),
            "--label",
            &format!("{SPEC_LABEL}={}", spec_hash(service)?),
            "--network",
            &self.network,
            "--cpus",
            &limits.cpus.to_string(),
            "--memory",
            &memory,
            "--memory-swap",
            &memory,
            "--pids-limit",
            "512",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges:true",
            "--init",
            "--stop-timeout",
            "30",
            "--log-driver",
            "json-file",
            "--log-opt",
            "max-size=10m",
            "--log-opt",
            "max-file=3",
            "--restart",
            if service.job() {
                "no"
            } else {
                "unless-stopped"
            },
        ]);
        if let Some(path) = &service.env_file {
            args.extend(strings(&["--env-file", path]));
        }
        for volume in &service.volumes {
            args.extend(strings(&[
                "--mount",
                &format!(
                    "type={},source={},target={}{}",
                    if volume.source.starts_with('/') {
                        "bind"
                    } else {
                        "volume"
                    },
                    volume.source,
                    volume.target,
                    if volume.read_only { ",readonly" } else { "" }
                ),
            ]));
        }
        for port in &service.ports {
            args.extend(strings(&["--publish", port]));
        }
        args.push(
            service
                .image
                .clone()
                .context("managed Docker service requires image")?,
        );
        args.extend(service.command.iter().cloned());
        Ok(args)
    }

    pub async fn update(&self, service: &Service) -> Result<()> {
        let inspection = self
            .inspect(service)
            .await?
            .context("container does not exist")?;
        self.ensure_owned(service, &inspection)?;
        self.ensure_spec(service, &inspection)?;
        let limits = service
            .limits
            .as_ref()
            .context("managed service requires limits")?;
        limits.validate()?;
        let memory = format!("{}m", limits.memory_mb);
        let container_id = inspection["Id"]
            .as_str()
            .context("Docker inspect omitted container ID")?;
        self.call(&strings(&[
            "update",
            "--cpus",
            &limits.cpus.to_string(),
            "--memory",
            &memory,
            "--memory-swap",
            &memory,
            "--pids-limit",
            "512",
            container_id,
        ]))
        .await?;
        Ok(())
    }

    pub async fn lifecycle(&self, service: &Service, action: &str) -> Result<()> {
        ensure!(
            service.runtime == "docker" && service.managed(),
            "{} is observe-only",
            service.id
        );
        ensure!(
            matches!(action, "start" | "stop" | "restart" | "recreate"),
            "unsupported container action"
        );
        let inspection = self.inspect(service).await?;
        if let Some(inspection) = &inspection {
            self.ensure_owned(service, inspection)?;
        }
        let inspected_id = inspection.as_ref().and_then(|value| value["Id"].as_str());
        ensure!(
            inspection.is_none() || inspected_id.is_some(),
            "Docker inspect omitted container ID"
        );
        if action == "recreate" {
            if let Some(inspection) = &inspection {
                ensure!(
                    inspection["State"]["Running"].as_bool() == Some(false),
                    "stop {} before recreating it",
                    service.id
                );
                self.call(&strings(&["container", "rm", inspected_id.unwrap()]))
                    .await?;
            }
            self.create(service).await?;
        } else if action == "stop" {
            if inspection.is_some() {
                execute(
                    &append(
                        &self.command,
                        &strings(&["stop", "--time", "30", inspected_id.unwrap()]),
                    ),
                    45,
                    false,
                )
                .await?;
            }
        } else {
            if inspection.is_none() {
                self.create(service).await?;
            } else {
                self.update(service).await?;
                execute(
                    &append(&self.command, &strings(&[action, inspected_id.unwrap()])),
                    45,
                    false,
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn logs(&self, service: &Service, tail: usize) -> Result<String> {
        let args = strings(&[
            "logs",
            "--timestamps",
            "--tail",
            &tail.clamp(1, 5000).to_string(),
            &self.name(service),
        ]);
        execute(&append(&self.command, &args), 30, true).await
    }
}

fn spec_hash(service: &Service) -> Result<String> {
    // Presentation fields and adjustable limits do not require container replacement.
    Ok(digest(&serde_json::to_vec(
        &json!({"id":service.id,"runtime":service.runtime,"kind":service.kind,"image":service.image,"command":service.command,"env_file":service.env_file,"volumes":service.volumes,"ports":service.ports}),
    )?))
}
fn percent(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}
fn memory_mb(value: &str) -> Option<f64> {
    let value = value.trim();
    let split = value.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let number = value[..split].parse::<f64>().ok()?;
    let multiplier = match value[split..].trim().to_ascii_lowercase().as_str() {
        "b" => 1.,
        "kb" => 1000.,
        "kib" => 1024.,
        "mb" => 1000_f64.powi(2),
        "mib" => 1024_f64.powi(2),
        "gb" => 1000_f64.powi(3),
        "gib" => 1024_f64.powi(3),
        "tb" => 1000_f64.powi(4),
        "tib" => 1024_f64.powi(4),
        _ => return None,
    };
    (number.is_finite() && number >= 0.).then_some(number * multiplier / 1024. / 1024.)
}

fn systemctl(service: &Service, mutate: bool, verb: &str, extra: &[String]) -> Result<Vec<String>> {
    ensure!(
        service.runtime == "systemd",
        "only systemd services support this operation"
    );
    let unit = service.unit.as_deref().context("missing systemd unit")?;
    let mut args = service.command_prefix.clone();
    if mutate && service.scope != "user" {
        args.extend(strings(&["sudo", "-n"]));
    }
    args.push("systemctl".into());
    if service.scope == "user" {
        args.push("--user".into());
    }
    args.extend(strings(&[verb, unit]));
    args.extend_from_slice(extra);
    Ok(args)
}

pub async fn native_status(service: &Service) -> Result<Value> {
    if service.runtime == "systemd" {
        let args = systemctl(
            service,
            false,
            "show",
            &strings(&[
                "--property=Id,LoadState,ActiveState,SubState,MainPID,CPUUsageNSec,MemoryCurrent,MemoryMax,CPUQuotaPerSecUSec",
            ]),
        )?;
        let output = run(&args).await?;
        let fields: HashMap<_, _> = output
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        ensure!(
            fields.get("LoadState") != Some(&"not-found"),
            "systemd unit does not exist"
        );
        let active = fields.get("ActiveState").copied().unwrap_or("unknown");
        let sub = fields.get("SubState").copied().unwrap_or("unknown");
        let status = match active {
            "active" if sub == "running" => "running",
            "active" | "failed" => "exited",
            "inactive" => "stopped",
            _ => "unknown",
        };
        let memory = fields
            .get("MemoryCurrent")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|bytes| bytes as f64 / 1024. / 1024.);
        let limit_memory = fields
            .get("MemoryMax")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|bytes| bytes as f64 / 1024. / 1024.);
        let cpus = fields
            .get("CPUQuotaPerSecUSec")
            .and_then(|value| quota_cpus(value));
        Ok(
            json!({"status":status,"health":if active == "failed" { "unhealthy" } else { "none" },"active_state":active,"sub_state":sub,"pid":fields.get("MainPID").and_then(|value| value.parse::<u64>().ok()),"memory_mb":memory,"cpu_percent":null,"cpu_usage_ns":fields.get("CPUUsageNSec").and_then(|value| value.parse::<u64>().ok()),"actual_limits":{"cpus":cpus,"memory_mb":limit_memory}}),
        )
    } else if service.runtime == "launchd" {
        #[cfg(target_os = "macos")]
        {
            let label = service.label.as_deref().context("missing launchd label")?;
            let output = run(&strings(&["/bin/launchctl", "list"])).await?;
            let row = output.lines().find_map(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                (fields.len() == 3 && fields[2] == label).then_some(fields)
            });
            let Some(fields) = row else {
                return Ok(
                    json!({"status":"unknown","health":"none","pid":null,"detail":"LaunchAgent is not loaded in the current user session","actual_limits":null}),
                );
            };
            let pid = fields[0].parse::<u64>().ok();
            let exit_code = fields[1].parse::<i64>().ok();
            let usage = if pid.is_some() {
                run(&strings(&["/bin/ps", "-axo", "pid=,ppid=,%cpu=,rss="]))
                    .await
                    .ok()
            } else {
                None
            };
            let usage = pid.and_then(|pid| process_tree_usage(pid, usage.as_deref().unwrap_or("")));
            let cpu = usage.map(|(cpu, _)| cpu);
            let memory = usage.map(|(_, memory)| memory);
            Ok(
                json!({"status":if pid.is_some() { "running" } else { "stopped" },"health":if pid.is_none() && exit_code.is_some_and(|code| code != 0) { "unhealthy" } else { "none" },"pid":pid,"last_exit_code":exit_code,"cpu_percent":cpu,"memory_mb":memory,"actual_limits":null,"resource_scope":"process tree","detail":"Native macOS services are observed without hard resource enforcement"}),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            bail!("launchd requires macOS")
        }
    } else {
        bail!("unsupported native runtime")
    }
}

#[cfg(any(target_os = "macos", test))]
fn process_tree_usage(root: u64, output: &str) -> Option<(f64, f64)> {
    let rows: Vec<(u64, u64, f64, u64)> = output
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            Some((
                columns.next()?.parse().ok()?,
                columns.next()?.parse().ok()?,
                columns
                    .next()?
                    .parse::<f64>()
                    .ok()
                    .filter(|cpu| cpu.is_finite() && *cpu >= 0.)?,
                columns.next()?.parse().ok()?,
            ))
        })
        .collect();
    let mut descendants = std::collections::HashSet::from([root]);
    loop {
        let previous = descendants.len();
        for (pid, parent, _, _) in &rows {
            if descendants.contains(parent) {
                descendants.insert(*pid);
            }
        }
        if descendants.len() == previous {
            break;
        }
    }
    let owned: Vec<_> = rows
        .iter()
        .filter(|(pid, _, _, _)| descendants.contains(pid))
        .collect();
    (!owned.is_empty()).then(|| {
        (
            owned.iter().map(|(_, _, cpu, _)| cpu).sum(),
            owned.iter().map(|(_, _, _, kib)| *kib as f64 / 1024.).sum(),
        )
    })
}

fn quota_cpus(value: &str) -> Option<f64> {
    let mut seconds = 0.;
    let mut count = 0;
    for component in value.split_whitespace() {
        let split = component
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .unwrap_or(component.len());
        let number = component[..split].parse::<f64>().ok()?;
        let multiplier = match &component[split..] {
            "" | "us" | "µs" | "μs" => 0.000001,
            "ms" => 0.001,
            "s" => 1.,
            "min" => 60.,
            "h" => 3600.,
            "d" => 86400.,
            "w" => 604800.,
            _ => return None,
        };
        seconds += number * multiplier;
        count += 1;
    }
    (count > 0 && seconds.is_finite()).then_some(seconds)
}

pub async fn native_logs(service: &Service, tail: usize) -> Result<String> {
    let tail = tail.clamp(1, 5000);
    if service.runtime == "systemd" {
        let mut args = service.command_prefix.clone();
        args.push("journalctl".into());
        if service.scope == "user" {
            args.push("--user".into());
        }
        args.extend(strings(&[
            "--unit",
            service.unit.as_deref().context("missing systemd unit")?,
            "--lines",
            &tail.to_string(),
            "--no-pager",
            "--output",
            "short-iso",
        ]));
        run(&args).await
    } else if service.runtime == "launchd" {
        let mut logs = String::new();
        for path in &service.log_paths {
            let args = strings(&["/usr/bin/tail", "-n", &tail.to_string(), "--", path]);
            match run(&args).await {
                Ok(output) => {
                    ensure!(
                        logs.len() + output.len() <= MAX_OUTPUT,
                        "logs exceed 1 MiB; request fewer lines"
                    );
                    logs.push_str(&format!("{}\n{}", path, output));
                }
                Err(error) => logs.push_str(&format!("{path}: {error}\n")),
            }
        }
        if logs.is_empty() {
            logs = "No log files are configured for this LaunchAgent.\n".into();
        }
        Ok(logs)
    } else {
        bail!("unsupported native runtime")
    }
}

pub async fn native_scale(service: &Service) -> Result<()> {
    ensure!(service.managed(), "{} is observe-only", service.id);
    let limits: &Limits = service
        .limits
        .as_ref()
        .context("managed service requires limits")?;
    limits.validate()?;
    let extra = vec![
        format!("CPUQuota={}%", limits.cpus * 100.),
        format!("MemoryMax={}M", limits.memory_mb),
    ];
    run(&systemctl(service, true, "set-property", &extra)?).await?;
    Ok(())
}

pub async fn native_action(service: &Service, action: &str) -> Result<()> {
    ensure!(service.managed(), "{} is observe-only", service.id);
    ensure!(
        matches!(action, "start" | "stop" | "restart"),
        "unsupported systemd action"
    );
    if action != "stop" {
        native_scale(service).await?;
    }
    run(&systemctl(service, true, action, &[])?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> Service {
        serde_json::from_value(json!({"id":"worker","runtime":"docker","observe_only":false,"image":"alpine:3","limits":{"cpus":0.5,"memory_mb":256},"command":["sleep","60"],"volumes":[{"source":"cache","target":"/cache","read_only":true}]})).unwrap()
    }
    #[test]
    fn docker_creation_caps_resources_and_preserves_argument_boundaries() {
        let docker = Docker::new(strings(&["docker"]), Path::new("/tmp/selfhost-tests"));
        let mut service = service();
        service.command = strings(&["echo", "literal $(shell); spaces"]);
        let args = docker.create_args(&service).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--memory-swap", "256m"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--cpus", "0.5"]));
        assert!(args.windows(2).any(|pair| pair == ["--cap-drop", "ALL"]));
        assert!(
            args.iter()
                .any(|arg| arg == "type=volume,source=cache,target=/cache,readonly")
        );
        assert_eq!(args.last().unwrap(), "literal $(shell); spaces");
    }
    #[test]
    fn ownership_and_creation_drift_are_guarded_but_limits_are_adjustable() {
        let docker = Docker::new(strings(&["docker"]), Path::new("/tmp/selfhost-tests"));
        let mut service = service();
        let inspection = json!({"Config":{"Labels":{OWNER_LABEL:docker.owner,SERVICE_LABEL:service.id,SPEC_LABEL:spec_hash(&service).unwrap()}}});
        assert!(docker.ensure_owned(&service, &inspection).is_ok());
        assert!(docker.ensure_owned(&service, &json!({})).is_err());
        service.limits.as_mut().unwrap().cpus = 0.75;
        assert!(docker.ensure_spec(&service, &inspection).is_ok());
        service.image = Some("alpine:other".into());
        assert!(docker.ensure_spec(&service, &inspection).is_err());
        service.observe_only = true;
        assert!(docker.ensure_owned(&service, &inspection).is_err());
    }
    #[test]
    fn docker_and_systemd_resource_units_parse() {
        assert_eq!(memory_mb("1.5GiB"), Some(1536.));
        assert_eq!(memory_mb("500MiB"), Some(500.));
        assert_eq!(memory_mb("unlimited"), None);
        assert_eq!(quota_cpus("2s"), Some(2.));
        assert_eq!(quota_cpus("500ms"), Some(0.5));
        assert_eq!(quota_cpus("1s 500ms"), Some(1.5));
        assert_eq!(quota_cpus("1min 30s"), Some(90.));
        assert_eq!(quota_cpus("1500000us"), Some(1.5));
        assert_eq!(quota_cpus("1.500000s"), Some(1.5));
        assert_eq!(quota_cpus("1h 2min 3s"), Some(3723.));
        assert_eq!(quota_cpus("1500000"), Some(1.5));
        assert_eq!(quota_cpus("infinity"), None);
        assert_eq!(quota_cpus(""), None);
        assert_eq!(quota_cpus("-1s"), None);
        assert_eq!(quota_cpus("1s unexpected"), None);
    }
    #[test]
    fn native_telemetry_sums_unordered_descendants_and_ignores_other_trees() {
        let ps = "4 3 40.5 4096\n2 1 0.0 1024\n8 7 99.0 100000\n3 2 10.0 2048\n1 0 1.0 500\ninvalid row\n9 2 NaN 1000\n";
        assert_eq!(process_tree_usage(2, ps), Some((50.5, 7.)));
        assert_eq!(process_tree_usage(42, ps), None);
    }
    #[test]
    fn systemd_scaling_uses_scope_and_no_shell() {
        let mut service = service();
        service.runtime = "systemd".into();
        service.unit = Some("worker.service".into());
        service.command_prefix = strings(&["limactl", "shell", "workers"]);
        let args = systemctl(
            &service,
            true,
            "set-property",
            &strings(&["CPUQuota=50%", "MemoryMax=256M"]),
        )
        .unwrap();
        assert_eq!(
            args,
            strings(&[
                "limactl",
                "shell",
                "workers",
                "sudo",
                "-n",
                "systemctl",
                "set-property",
                "worker.service",
                "CPUQuota=50%",
                "MemoryMax=256M"
            ])
        );
        service.scope = "user".into();
        assert_eq!(
            systemctl(&service, true, "start", &[]).unwrap(),
            strings(&[
                "limactl",
                "shell",
                "workers",
                "systemctl",
                "--user",
                "start",
                "worker.service"
            ])
        );
    }
    #[tokio::test]
    async fn subprocesses_bound_output_and_report_failures() {
        assert_eq!(
            run(&strings(&["/bin/echo", "literal ; text"]))
                .await
                .unwrap()
                .trim(),
            "literal ; text"
        );
        assert!(
            run(&strings(&["/bin/sh", "-c", "exit 7"]))
                .await
                .unwrap_err()
                .to_string()
                .contains("7")
        );
        assert!(
            run(&strings(&["/bin/sh", "-c", "yes x | head -c 1100000"]))
                .await
                .unwrap_err()
                .to_string()
                .contains("1 MiB")
        );
    }
}
