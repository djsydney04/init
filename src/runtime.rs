use crate::config::{Limits, Service};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

const OWNER_LABEL: &str = "io.self-host-init.workspace";
const SERVICE_LABEL: &str = "io.self-host-init.service";
const SPEC_LABEL: &str = "io.self-host-init.spec";
const PURPOSE_LABEL: &str = "io.self-host-init.purpose";
const PROJECT_LABEL: &str = "io.self-host-init.project";
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
    execute_command(args, seconds, combine_output, false, None).await
}

pub(crate) async fn run_git(args: &[String]) -> Result<String> {
    execute_command(args, 300, false, true, None).await
}

pub(crate) async fn archive_context(context: &Path, archive: &Path) -> Result<()> {
    let context = std::path::absolute(context)?;
    let archive = std::path::absolute(archive)?;
    execute(
        &strings(&[
            "tar",
            "-C",
            context.to_str().context("Build path must be UTF-8")?,
            "-cf",
            archive.to_str().context("Archive path must be UTF-8")?,
            "--exclude=./.git",
            ".",
        ]),
        300,
        false,
    )
    .await
    .context("Could not archive GitHub build context")?;
    Ok(())
}

async fn execute_command(
    args: &[String],
    seconds: u64,
    combine_output: bool,
    git: bool,
    stdin: Option<std::fs::File>,
) -> Result<String> {
    let (program, arguments) = args.split_first().context("empty command")?;
    ensure!(
        !program.is_empty() && args.iter().all(|arg| !arg.contains('\0')),
        "invalid command argument"
    );
    let mut command = Command::new(program);
    command.args(arguments);
    if git {
        // Disable configured Git credential helpers, URL rewrites and hooks for public clones.
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
    }
    let mut child = command
        .stdin(stdin.map_or_else(Stdio::null, Stdio::from))
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

fn lima_instance(command: &[String]) -> Option<&str> {
    (command
        .first()
        .and_then(|part| Path::new(part).file_name())
        .is_some_and(|name| name == "limactl")
        && command.get(1).is_some_and(|part| part == "shell"))
    .then(|| command.get(2).map(String::as_str))
    .flatten()
    .filter(|instance| !instance.is_empty() && !instance.starts_with('-'))
}

fn engine_location(command: &[String], info: &Value) -> Value {
    let instance = lima_instance(command);
    let label = match instance {
        Some(instance) => format!("Lima VM · {instance}"),
        None => info["server_name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map_or_else(
                || "Configured Docker engine".to_owned(),
                |name| format!("Docker engine · {name}"),
            ),
    };
    json!({"kind":if instance.is_some(){"lima"}else{"engine"},"label":label,"instance":instance,"supervisor":"docker","target":null,"pid_namespace":"engine host"})
}

/// Display only verified target identity; command prefixes can contain credentials.
pub fn execution_location(
    service: &Service,
    engine_command: &[String],
    engine_info: &Value,
) -> Value {
    let mut location = if service.runtime == "docker" {
        engine_location(engine_command, engine_info)
    } else if service.runtime == "launchd" || service.command_prefix.is_empty() {
        json!({"kind":"host","label":if cfg!(target_os="macos"){"This Mac"}else{"This host"},"instance":null,"supervisor":service.runtime,"pid_namespace":"service host"})
    } else if let Some(instance) = lima_instance(&service.command_prefix) {
        json!({"kind":"lima","label":format!("Lima VM · {instance}"),"instance":instance,"supervisor":service.runtime,"pid_namespace":"service host"})
    } else {
        json!({"kind":"prefix","label":"Configured execution target","instance":null,"supervisor":service.runtime,"pid_namespace":"service host"})
    };
    location["target"] = json!(match service.runtime.as_str() {
        "docker" => service.container.as_deref().unwrap_or(&service.id),
        "systemd" => service.unit.as_deref().unwrap_or(&service.id),
        _ => service.label.as_deref().unwrap_or(&service.id),
    });
    location["scope"] = json!(match service.runtime.as_str() {
        "docker" => "container",
        "systemd" => service.scope.as_str(),
        _ => "current user session",
    });
    location
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

    pub fn network_for(&self, service: &Service) -> String {
        service.project.as_ref().map_or_else(
            || self.network.clone(),
            |project| format!("selfhost-{}-project-{project}", self.owner),
        )
    }

    pub async fn info(&self) -> Value {
        match self
            .call(&strings(&["info", "--format", "{{json .}}"]))
            .await
            .and_then(|output| Ok(serde_json::from_str::<Value>(&output)?))
        {
            Ok(info) => {
                let mut result = json!({"available":true,"cpus":info["NCPU"],"memory_mb":info["MemTotal"].as_u64().map(|bytes| bytes / 1024 / 1024),"architecture":info["Architecture"],"server_version":info["ServerVersion"],"server_name":info["Name"],"operating_system":info["OperatingSystem"],"context":info["ClientInfo"]["Context"],"error":null});
                result["execution"] = engine_location(&self.command, &result);
                result
            }
            Err(error) => {
                let mut result = json!({"available":false,"cpus":null,"memory_mb":null,"server_name":null,"operating_system":null,"context":null,"error":error.to_string()});
                result["execution"] = engine_location(&self.command, &result);
                result
            }
        }
    }

    pub async fn image_available(&self, image: &str) -> Result<bool> {
        ensure!(
            !image.is_empty() && !image.starts_with('-') && !image.contains('\0'),
            "Invalid image reference"
        );
        match self
            .call(&strings(&[
                "image",
                "inspect",
                "--format",
                "{{json .Id}}",
                image,
            ]))
            .await
        {
            Ok(_) => Ok(true),
            Err(error)
                if {
                    let message = error.to_string().to_ascii_lowercase();
                    message.contains("no such image") || message.contains("no such object")
                } =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn workspace_volume(
        &self,
        service: &Service,
        name: &str,
        create: bool,
    ) -> Result<bool> {
        ensure!(
            service.runtime == "docker" && service.managed(),
            "{} is observe-only",
            service.id
        );
        ensure!(
            name == format!("selfhost-{}-{}-workspace", self.owner, service.id),
            "Invalid workspace volume name"
        );
        let inspect = strings(&["volume", "inspect", "--format", "{{json .Labels}}", name]);
        let verify = |output: &str| -> Result<()> {
            let labels: Value = serde_json::from_str(output)?;
            ensure!(
                labels[OWNER_LABEL].as_str() == Some(self.owner.as_str())
                    && labels[SERVICE_LABEL].as_str() == Some(service.id.as_str())
                    && labels[PURPOSE_LABEL].as_str() == Some("workspace"),
                "Volume {name} belongs to another workspace or service; choose another service ID"
            );
            Ok(())
        };
        match self.call(&inspect).await {
            Ok(output) => {
                verify(&output)?;
                return Ok(true);
            }
            Err(error)
                if {
                    let message = error.to_string().to_ascii_lowercase();
                    message.contains("no such volume") || message.contains("no such object")
                } => {}
            Err(error) => return Err(error),
        }
        if create {
            self.call(&strings(&[
                "volume",
                "create",
                "--label",
                &format!("{OWNER_LABEL}={}", self.owner),
                "--label",
                &format!("{SERVICE_LABEL}={}", service.id),
                "--label",
                &format!("{PURPOSE_LABEL}=workspace"),
                name,
            ]))
            .await?;
            // Verify again: Docker can return a volume created concurrently by another owner.
            verify(&self.call(&inspect).await?)?;
        }
        Ok(false)
    }

    pub async fn inventory(&self) -> Result<Vec<Value>> {
        let output = self.call(&strings(&["ps", "-a", "--filter", &format!("label={OWNER_LABEL}={}", self.owner), "--format", "{\"ID\":{{json .ID}},\"Names\":{{json .Names}},\"Image\":{{json .Image}},\"State\":{{json .State}},\"Status\":{{json .Status}}}"])).await?;
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
        let names = self
            .inventory()
            .await?
            .into_iter()
            .filter(|row| row["State"] == "running")
            .filter_map(|row| row["Names"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        self.stats_for(&names).await
    }

    pub async fn stats_for(&self, names: &[String]) -> Result<HashMap<String, Value>> {
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        ensure!(
            names
                .iter()
                .all(|name| !name.is_empty() && !name.starts_with('-') && !name.contains('\0')),
            "Invalid container name for resource sampling"
        );
        let mut selected = names.to_vec();
        selected.sort();
        selected.dedup();
        let mut args = strings(&[
            "stats",
            "--no-stream",
            "--format",
            "{\"Name\":{{json .Name}},\"CPUPerc\":{{json .CPUPerc}},\"MemUsage\":{{json .MemUsage}},\"MemPerc\":{{json .MemPerc}},\"NetIO\":{{json .NetIO}},\"BlockIO\":{{json .BlockIO}},\"PIDs\":{{json .PIDs}}}",
        ]);
        args.extend(selected);
        let output = self.call(&args).await?;
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

    async fn ensure_network(&self, service: &Service) -> Result<()> {
        let network = self.network_for(service);
        let args = strings(&[
            "network",
            "inspect",
            "--format",
            "{{json .Labels}}",
            &network,
        ]);
        let output = match self.call(&args).await {
            Ok(output) => output,
            Err(error) if absent(&error) => {
                let mut args = strings(&[
                    "network",
                    "create",
                    "--driver",
                    "bridge",
                    "--label",
                    &format!("{OWNER_LABEL}={}", self.owner),
                ]);
                if let Some(project) = &service.project {
                    args.extend(strings(&["--label", &format!("{PROJECT_LABEL}={project}")]));
                }
                args.push(network.clone());
                self.call(&args).await?;
                self.call(&strings(&[
                    "network",
                    "inspect",
                    "--format",
                    "{{json .Labels}}",
                    &network,
                ]))
                .await?
            }
            Err(error) => return Err(error),
        };
        let labels: Value = serde_json::from_str(&output)?;
        ensure!(
            labels[OWNER_LABEL].as_str() == Some(self.owner.as_str())
                && labels[PROJECT_LABEL].as_str() == service.project.as_deref(),
            "network {} belongs to another workspace or project",
            network
        );
        Ok(())
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
        self.ensure_network(service).await?;
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
            &self.network_for(service),
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
        if let Some(project) = &service.project {
            args.extend(strings(&["--label", &format!("{PROJECT_LABEL}={project}")]));
        }
        for volume in &service.volumes {
            args.extend(strings(&[
                "--mount",
                &format!(
                    "type={},source={},target={}{}{}",
                    if volume.source.starts_with('/') {
                        "bind"
                    } else {
                        "volume"
                    },
                    volume.source,
                    volume.target,
                    if volume.read_only { ",readonly" } else { "" },
                    if volume.no_copy { ",volume-nocopy" } else { "" }
                ),
            ]));
        }
        for port in &service.ports {
            args.extend(strings(&["--publish", port]));
        }
        if let Some(path) = &service.working_dir {
            args.extend(strings(&["--workdir", path]));
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

    /// Build/pull before touching the running container, using the configured engine.
    pub async fn prepare_image(
        &self,
        image: &str,
        build_context: Option<&Path>,
        pull: bool,
    ) -> Result<()> {
        ensure!(
            !image.is_empty() && !image.starts_with('-') && !image.contains('\0'),
            "Invalid image reference"
        );
        if let Some(context) = build_context {
            // Keep the shared path spelling: host symlinks can resolve outside a VM mount.
            let context = std::path::absolute(context).context("Build context is unavailable")?;
            ensure!(context.is_dir(), "Build context must be a directory");
            execute(
                &append(
                    &self.command,
                    &strings(&[
                        "build",
                        "--quiet",
                        "--tag",
                        image,
                        context.to_str().context("Build path must be UTF-8")?,
                    ]),
                ),
                1800,
                false,
            )
            .await?;
        } else if pull {
            execute(
                &append(&self.command, &strings(&["pull", "--quiet", image])),
                600,
                false,
            )
            .await?;
        }
        ensure!(
            self.image_available(image).await?,
            "Image is not cached; use --pull or --build-context"
        );
        Ok(())
    }

    /// Send the context over stdin so VM engines do not need a host filesystem mount.
    pub(crate) async fn prepare_archived_image(&self, image: &str, archive: &Path) -> Result<()> {
        ensure!(
            !image.is_empty() && !image.starts_with('-') && !image.contains('\0'),
            "Invalid image reference"
        );
        let input = std::fs::File::open(archive).context("Build context archive is unavailable")?;
        execute_command(
            &append(
                &self.command,
                &strings(&["build", "--quiet", "--tag", image, "-"]),
            ),
            1800,
            false,
            false,
            Some(input),
        )
        .await?;
        ensure!(
            self.image_available(image).await?,
            "Build completed without a cached image"
        );
        Ok(())
    }

    /// Keep the old container intact until its replacement and manifest are committed.
    pub async fn replace_image(
        &self,
        old: &Service,
        new: &Service,
        commit: impl FnOnce() -> Result<()>,
    ) -> Result<Option<String>> {
        let original = self
            .inspect(old)
            .await?
            .context("Start this service once before deploying a replacement")?;
        self.ensure_owned(old, &original)?;
        self.ensure_spec(old, &original)?;
        let id = original["Id"]
            .as_str()
            .context("Docker inspect omitted container ID")?;
        let running = original["State"]["Running"]
            .as_bool()
            .context("Docker inspect omitted running state")?;
        let name = self.name(old);
        let backup = format!("{name}-previous");
        ensure!(
            self.inspect_name(&backup).await?.is_none(),
            "Previous deployment backup {backup} exists; inspect and recover it before deploying again"
        );
        if running {
            self.lifecycle(old, "stop").await?;
        }
        if let Err(error) = self.call(&strings(&["rename", id, &backup])).await {
            if running {
                self.call(&strings(&["start", id]))
                    .await
                    .context("Rename failed and original container could not restart")?;
            }
            return Err(error);
        }
        let result = async {
            self.create(new).await?;
            // Docker's init process can start successfully before the application fails to exec.
            tokio::time::sleep(Duration::from_secs(1)).await;
            ensure!(
                self.inspect(new).await?.is_some_and(|value| {
                    value["State"]["Running"] == true
                        && value["State"]["Restarting"] != true
                        && value["RestartCount"].as_u64().unwrap_or(0) == 0
                }),
                "Replacement exited or restarted during startup"
            );
            commit()
        }
        .await;
        if let Err(error) = result {
            let rollback = async {
                if let Some(replacement) = self.inspect(new).await? {
                    self.ensure_owned(new, &replacement)?;
                    if replacement["State"]["Running"] == true {
                        self.lifecycle(new, "stop").await?;
                    }
                    let replacement_id = replacement["Id"]
                        .as_str()
                        .context("Replacement has no container ID")?;
                    self.call(&strings(&["container", "rm", replacement_id]))
                        .await?;
                }
                self.call(&strings(&["rename", id, &name])).await?;
                if running {
                    self.call(&strings(&["start", id])).await?;
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            return match rollback {
                Ok(()) => Err(error.context("Deployment failed; original container and configuration restored. Shared volumes may have been changed by the replacement")),
                Err(rollback) => Err(error.context(format!("Rollback incomplete: {rollback:#}. Original container ID {id}, backup name {backup}; inspect before retrying. Volumes retained"))),
            };
        }
        Ok(self
            .call(&strings(&["container", "rm", id]))
            .await
            .err()
            .map(|error| {
                format!(
                    "Deployment succeeded, but stopped backup {backup} needs cleanup: {error:#}"
                )
            }))
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
    let mut spec = json!({"id":service.id,"runtime":service.runtime,"kind":service.kind,"image":service.image,"command":service.command,"env_file":service.env_file,"volumes":service.volumes,"ports":service.ports});
    if let Some(path) = &service.working_dir {
        spec["working_dir"] = json!(path);
    }
    if let Some(project) = &service.project {
        spec["project"] = json!(project);
    }
    Ok(digest(&serde_json::to_vec(&spec)?))
}
fn percent(value: &str) -> Option<f64> {
    value
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.)
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

pub struct NativeSample {
    pub state: Value,
    pub cpu_counters: Option<HashMap<u64, u64>>,
    pub captured_at: Instant,
    pub sampled_at: u64,
}

impl NativeSample {
    fn new(state: Value, cpu_counters: Option<HashMap<u64, u64>>) -> Self {
        Self {
            state,
            cpu_counters,
            captured_at: Instant::now(),
            sampled_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }
}

pub async fn native_samples(services: &[Service]) -> HashMap<String, Result<NativeSample, String>> {
    let mut samples = HashMap::new();
    let launchd: Vec<_> = services
        .iter()
        .filter(|service| service.runtime == "launchd")
        .collect();
    if !launchd.is_empty() {
        #[cfg(target_os = "macos")]
        samples.extend(launchd_samples(&launchd).await);
        #[cfg(not(target_os = "macos"))]
        samples.extend(
            launchd
                .into_iter()
                .map(|service| (service.id.clone(), Err("launchd requires macOS".into()))),
        );
    }
    let mut services = services
        .iter()
        .filter(|service| service.runtime == "systemd")
        .cloned();
    let mut pending = tokio::task::JoinSet::new();
    loop {
        while pending.len() < 8 {
            let Some(service) = services.next() else {
                break;
            };
            pending.spawn(async move {
                let sample = native_status(&service)
                    .await
                    .map(|state| {
                        let counters = state["cpu_usage_ns"]
                            .as_u64()
                            .map(|usage| HashMap::from([(0, usage)]));
                        NativeSample::new(state, counters)
                    })
                    .map_err(|error| error.to_string());
                (service.id, sample)
            });
        }
        let Some(result) = pending.join_next().await else {
            break;
        };
        match result {
            Ok((id, sample)) => {
                samples.insert(id, sample);
            }
            Err(error) => eprintln!("Native resource sampling failed: {error}"),
        }
    }
    samples
}

fn accounting_counter(value: &str) -> Option<u64> {
    value.parse().ok().filter(|value| *value != u64::MAX)
}

pub async fn native_status(service: &Service) -> Result<Value> {
    if service.runtime == "systemd" {
        let args = systemctl(
            service,
            false,
            "show",
            &strings(&[
                "--property=Id,LoadState,ActiveState,SubState,MainPID,TasksCurrent,CPUUsageNSec,MemoryCurrent,MemoryMax,CPUQuotaPerSecUSec",
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
            .and_then(|value| accounting_counter(value))
            .map(|bytes| bytes as f64 / 1024. / 1024.);
        let limit_memory = fields
            .get("MemoryMax")
            .and_then(|value| accounting_counter(value))
            .map(|bytes| bytes as f64 / 1024. / 1024.);
        let cpus = fields
            .get("CPUQuotaPerSecUSec")
            .and_then(|value| quota_cpus(value));
        let cpu_usage = fields
            .get("CPUUsageNSec")
            .and_then(|value| accounting_counter(value));
        let telemetry_error = if matches!(
            active,
            "active" | "activating" | "deactivating" | "reloading"
        ) && (cpu_usage.is_none() || memory.is_none())
        {
            Some("systemd CPU or memory accounting is unavailable for this unit")
        } else {
            None
        };
        Ok(
            json!({"status":status,"health":if active == "failed" { "unhealthy" } else { "none" },"active_state":active,"sub_state":sub,"pid":fields.get("MainPID").and_then(|value| value.parse::<u64>().ok()).filter(|pid|*pid>0),"task_count":fields.get("TasksCurrent").and_then(|value|accounting_counter(value)),"memory_mb":memory,"cpu_percent":null,"cpu_usage_ns":cpu_usage,"telemetry_error":telemetry_error,"resource_scope":"systemd unit cgroup","cpu_source":"CPUUsageNSec interval delta","memory_source":"systemd MemoryCurrent","actual_limits":{"cpus":cpus,"memory_mb":limit_memory}}),
        )
    } else if service.runtime == "launchd" {
        #[cfg(target_os = "macos")]
        {
            launchd_samples(&[service])
                .await
                .remove(&service.id)
                .context("Missing launchd sample")?
                .map(|sample| sample.state)
                .map_err(anyhow::Error::msg)
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
struct ProcessSample {
    parent: u64,
    cpu_ns: u64,
    rss_kib: u64,
}

#[cfg(any(target_os = "macos", test))]
fn cpu_time_ns(value: &str) -> Option<u64> {
    let (days, time) = value.split_once('-').map_or((0, value), |(days, time)| {
        (days.parse::<u64>().unwrap_or(u64::MAX), time)
    });
    let mut seconds = days.checked_mul(86400)? as f64;
    let parts: Vec<_> = time.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    for (index, part) in parts.iter().enumerate() {
        let number = part.parse::<f64>().ok()?;
        if !number.is_finite() || number < 0. || (index > 0 && number >= 60.) {
            return None;
        }
        seconds += number * 60_f64.powi((parts.len() - index - 1) as i32);
    }
    let nanos = seconds * 1e9;
    (nanos < u64::MAX as f64).then_some(nanos.round() as u64)
}

#[cfg(any(target_os = "macos", test))]
fn process_samples(output: &str) -> HashMap<u64, ProcessSample> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((
                parts.next()?.parse().ok()?,
                ProcessSample {
                    parent: parts.next()?.parse().ok()?,
                    cpu_ns: cpu_time_ns(parts.next()?)?,
                    rss_kib: parts.next()?.parse().ok()?,
                },
            ))
        })
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn process_tree(
    root: u64,
    processes: &HashMap<u64, ProcessSample>,
) -> Option<(HashMap<u64, u64>, f64)> {
    processes.get(&root)?;
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for (pid, process) in processes {
        children.entry(process.parent).or_default().push(*pid);
    }
    let mut pending = vec![root];
    let mut counters = HashMap::new();
    let mut memory = 0.;
    while let Some(pid) = pending.pop() {
        if counters.contains_key(&pid) {
            continue;
        }
        let Some(process) = processes.get(&pid) else {
            continue;
        };
        counters.insert(pid, process.cpu_ns);
        memory += process.rss_kib as f64 / 1024.;
        pending.extend(children.get(&pid).into_iter().flatten().copied());
    }
    Some((counters, memory))
}

#[cfg(target_os = "macos")]
async fn launchd_samples(services: &[&Service]) -> HashMap<String, Result<NativeSample, String>> {
    let listed = run(&strings(&["/bin/launchctl", "list"]))
        .await
        .map_err(|error| error.to_string());
    let rows: HashMap<String, (Option<u64>, Option<i64>)> = listed
        .as_ref()
        .ok()
        .into_iter()
        .flat_map(|output| output.lines())
        .filter_map(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            (parts.len() == 3).then(|| {
                (
                    parts[2].to_owned(),
                    (parts[0].parse().ok(), parts[1].parse().ok()),
                )
            })
        })
        .collect();
    let needs_processes = services.iter().any(|service| {
        service
            .label
            .as_ref()
            .and_then(|label| rows.get(label))
            .is_some_and(|(pid, _)| pid.is_some())
    });
    let processes = if needs_processes {
        run(&strings(&["/bin/ps", "-axo", "pid=,ppid=,time=,rss="]))
            .await
            .map(|output| process_samples(&output))
            .map_err(|error| error.to_string())
    } else {
        Ok(HashMap::new())
    };
    services.iter().map(|service| {
        let sample = (|| {
            let _ = listed.as_ref().map_err(Clone::clone)?;
            let label=service.label.as_ref().ok_or_else(||"Missing launchd label".to_owned())?;
            let Some((pid,exit_code))=rows.get(label) else {
                return Ok(NativeSample::new(json!({"status":"unknown","health":"none","pid":null,"actual_limits":null,"detail":"LaunchAgent is not loaded in this user session","resource_scope":"process tree","telemetry_error":"LaunchAgent is not loaded in this user session"}),None));
            };
            let usage=pid.and_then(|pid|processes.as_ref().ok().and_then(|rows|process_tree(pid,rows)));
            let mut state=json!({"status":if pid.is_some(){"running"}else{"stopped"},"health":if pid.is_none() && exit_code.is_some_and(|code|code!=0){"unhealthy"}else{"none"},"pid":pid,"last_exit_code":exit_code,"cpu_percent":if pid.is_none(){Some(0.)}else{None},"memory_mb":if pid.is_none(){Some(0.)}else{usage.as_ref().map(|(_,memory)|*memory)},"actual_limits":null,"resource_scope":"process tree","cpu_source":"Process CPU-time interval delta","memory_source":"Process tree resident set (RSS)","detail":"Native macOS services are observed without hard resource enforcement","telemetry_error":null});
            if pid.is_some() && usage.is_none() {
                state["telemetry_error"]=json!(processes.as_ref().err().cloned().unwrap_or_else(||"Process changed or exited during sampling".into()));
            }
            let counters=usage.map(|(counters,_)|counters);
            let mut pids:Vec<_>=counters.as_ref().into_iter().flat_map(|counters|counters.keys().copied()).collect();
            pids.sort_unstable();
            state["process_ids"]=json!(pids);
            state["process_count"]=if pid.is_none(){json!(0)}else{counters.as_ref().map_or(Value::Null,|counters|json!(counters.len()))};
            Ok(NativeSample::new(state,counters))
        })();
        (service.id.clone(),sample)
    }).collect()
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
    #[cfg(unix)]
    #[tokio::test]
    async fn inventory_and_sampling_are_scoped_without_changing_engine_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        std::fs::write(&script,r#"dir=$1; shift
printf '%s\n' "$*" >> "$dir/calls"
case "$1" in
  ps) printf '%s\n' '{"Names":"owned-running","State":"running"}' '{"Names":"owned-stopped","State":"exited"}' ;;
  stats) printf '%s\n' '{"Name":"owned-running","CPUPerc":"25%","MemUsage":"128MiB / 512MiB","PIDs":"2"}' ;;
  info) printf '%s\n' '{"NCPU":8,"MemTotal":8589934592}' ;;
  *) exit 1 ;;
esac
"#).unwrap();
        let docker = Docker::new(
            vec![
                "/bin/sh".into(),
                script.display().to_string(),
                dir.path().display().to_string(),
            ],
            dir.path(),
        );
        assert!(docker.stats_for(&[]).await.unwrap().is_empty());
        assert!(!dir.path().join("calls").exists());
        let stats = docker.stats().await.unwrap();
        assert_eq!(stats["owned-running"]["memory_mb"], 128.);
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(
            calls
                .lines()
                .next()
                .unwrap()
                .contains(&format!("--filter label={OWNER_LABEL}={}", docker.owner)),
            "{calls}"
        );
        let sample = calls
            .lines()
            .find(|line| line.starts_with("stats "))
            .unwrap();
        assert!(sample.ends_with(" owned-running"), "{sample}");
        assert!(!sample.contains("owned-stopped"));
        assert!(docker.stats_for(&["--all".into()]).await.is_err());
        let info = docker.info().await;
        assert_eq!(info["cpus"], 8);
        assert_eq!(info["memory_mb"], 8192);
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
        let ps = "4 3 0:40.50 4096\n2 1 0:00.00 1024\n8 7 1:39.00 100000\n3 2 0:10.00 2048\n1 0 0:01.00 500\ninvalid row\n9 2 NaN 1000\n";
        let rows = process_samples(ps);
        let (counters, memory) = process_tree(2, &rows).unwrap();
        assert_eq!(memory, 7.);
        assert_eq!(counters.len(), 3);
        assert_eq!(counters.values().sum::<u64>(), 50_500_000_000);
        assert!(process_tree(42, &rows).is_none());
        assert_eq!(cpu_time_ns("157:06.89"), Some(9_426_890_000_000));
        assert_eq!(cpu_time_ns("1-02:03:04"), Some(93_784_000_000_000));
        for invalid in ["NaN", "-1:00", "1:60", "infinity"] {
            assert_eq!(cpu_time_ns(invalid), None);
        }
        assert_eq!(accounting_counter("18446744073709551615"), None);
        assert_eq!(accounting_counter("0"), Some(0));
    }
    #[test]
    fn execution_targets_do_not_guess_locality_or_expose_command_credentials() {
        let mut s = service();
        let lima = strings(&["/opt/bin/limactl", "shell", "workers", "docker"]);
        assert_eq!(
            execution_location(&s, &lima, &json!({}))["label"],
            "Lima VM · workers"
        );
        let remote = execution_location(
            &s,
            &strings(&["docker"]),
            &json!({"server_name":"build-host"}),
        );
        assert_eq!(remote["kind"], "engine");
        assert_eq!(remote["pid_namespace"], "engine host");
        s.runtime = "systemd".into();
        s.command_prefix = strings(&["ssh", "-i", "/private/key", "user@example"]);
        let location = execution_location(&s, &[], &json!({}));
        assert_eq!(location["kind"], "prefix");
        assert!(!location.to_string().contains("private/key"));
        assert!(!location.to_string().contains("user@example"));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn image_preflight_distinguishes_absence_invalid_references_and_connection_errors() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        std::fs::write(
            &script,
            r#"for image do :; done
case "$image" in
  present) printf '%s\n' '"sha256:known"' ;;
  missing) printf '%s\n' 'No such image: missing' >&2; exit 1 ;;
  invalid) printf '%s\n' 'invalid reference format' >&2; exit 1 ;;
  *) printf '%s\n' 'Cannot connect to Docker daemon' >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        let docker = Docker::new(
            vec!["/bin/sh".into(), script.display().to_string()],
            dir.path(),
        );
        assert!(docker.image_available("present").await.unwrap());
        assert!(!docker.image_available("missing").await.unwrap());
        assert!(docker.image_available("invalid").await.is_err());
        assert!(docker.image_available("offline").await.is_err());
    }
    #[test]
    fn clean_workspaces_set_working_directory_without_image_copy_up_or_old_hash_drift() {
        let original = service();
        let expected=digest(&serde_json::to_vec(&json!({"id":original.id,"runtime":original.runtime,"kind":original.kind,"image":original.image,"command":original.command,"env_file":original.env_file,"volumes":original.volumes,"ports":original.ports})).unwrap());
        assert_eq!(spec_hash(&original).unwrap(), expected);
        let mut s = original.clone();
        s.working_dir = Some("/workspace".into());
        s.volumes[0].no_copy = true;
        let docker = Docker::new(strings(&["docker"]), Path::new("/tmp/workspace-test"));
        let args = docker.create_args(&s).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--workdir", "/workspace"])
        );
        assert!(args.iter().any(|arg| arg.contains(",volume-nocopy")));
        assert_ne!(spec_hash(&s).unwrap(), expected);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn project_networks_are_distinct_and_require_matching_workspace_and_project_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        std::fs::write(&script, r#"dir=$1; shift
printf '%s\n' "$*" >> "$dir/calls"
case "$1 $2" in
  'network inspect') if [ -f "$dir/labels" ]; then cat "$dir/labels"; else printf '%s\n' 'No such network' >&2; exit 1; fi ;;
  'network create') cp "$dir/expected" "$dir/labels"; printf '%s\n' 'created' ;;
  *) exit 1 ;;
esac
"#).unwrap();
        let docker = Docker::new(
            vec![
                "/bin/sh".into(),
                script.display().to_string(),
                dir.path().display().to_string(),
            ],
            dir.path(),
        );
        let mut s = service();
        let legacy_hash = spec_hash(&s).unwrap();
        assert_eq!(docker.network_for(&s), docker.network);
        s.project = Some("build-tools".into());
        let network = format!("selfhost-{}-project-build-tools", docker.owner);
        assert_eq!(docker.network_for(&s), network);
        assert_ne!(spec_hash(&s).unwrap(), legacy_hash);
        let args = docker.create_args(&s).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["--network", &network]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--label", "io.self-host-init.project=build-tools"])
        );
        let labels = json!({OWNER_LABEL:docker.owner,PROJECT_LABEL:"build-tools"});
        std::fs::write(dir.path().join("expected"), labels.to_string()).unwrap();
        docker.ensure_network(&s).await.unwrap();
        docker.ensure_network(&s).await.unwrap();
        for foreign in [
            json!({OWNER_LABEL:docker.owner}),
            json!({OWNER_LABEL:docker.owner,PROJECT_LABEL:"other"}),
            json!({OWNER_LABEL:"foreign",PROJECT_LABEL:"build-tools"}),
        ] {
            std::fs::write(dir.path().join("labels"), foreign.to_string()).unwrap();
            assert!(docker.ensure_network(&s).await.is_err());
        }
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("network create "))
                .count(),
            1
        );
        assert!(calls.contains(&format!(
            "--label {OWNER_LABEL}={} --label {PROJECT_LABEL}=build-tools {network}",
            docker.owner
        )));
        assert!(!calls.contains("network rm"));
        s.project = Some("other".into());
        assert_ne!(docker.network_for(&s), network);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_volumes_are_owner_checked_and_preview_never_creates_or_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        std::fs::write(&script,r#"dir=$1; shift
printf '%s\n' "$*" >> "$dir/calls"
case "$1 $2" in
  'volume inspect') if [ -f "$dir/labels" ]; then cat "$dir/labels"; else printf '%s\n' 'No such volume' >&2; exit 1; fi ;;
  'volume create') cp "$dir/expected" "$dir/labels"; printf '%s\n' 'created' ;;
  *) exit 1 ;;
esac
"#).unwrap();
        let docker = Docker::new(
            vec![
                "/bin/sh".into(),
                script.display().to_string(),
                dir.path().display().to_string(),
            ],
            dir.path(),
        );
        let s = service();
        let name = format!("selfhost-{}-{}-workspace", docker.owner, s.id);
        std::fs::write(
            dir.path().join("expected"),
            json!({OWNER_LABEL:docker.owner,SERVICE_LABEL:s.id,PURPOSE_LABEL:"workspace"})
                .to_string(),
        )
        .unwrap();
        assert!(!docker.workspace_volume(&s, &name, false).await.unwrap());
        assert!(!dir.path().join("labels").exists());
        assert!(!docker.workspace_volume(&s, &name, true).await.unwrap());
        assert!(docker.workspace_volume(&s, &name, false).await.unwrap());
        std::fs::write(dir.path().join("labels"), "{}").unwrap();
        assert!(
            docker
                .workspace_volume(&s, &name, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("choose another service ID")
        );
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("volume create "))
                .count(),
            1
        );
        assert!(!calls.contains("volume rm"));
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
    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_systemd_accounting_is_null_with_an_explicit_reason() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("systemd.sh");
        std::fs::write(&script,"printf '%s\\n' 'LoadState=loaded' 'ActiveState=active' 'SubState=running' 'MainPID=123' 'CPUUsageNSec=18446744073709551615' 'MemoryCurrent=18446744073709551615' 'TasksCurrent=18446744073709551615'\n").unwrap();
        let mut s = service();
        s.runtime = "systemd".into();
        s.unit = Some("worker.service".into());
        s.command_prefix = vec!["/bin/sh".into(), script.display().to_string()];
        let state = native_status(&s).await.unwrap();
        assert_eq!(state["status"], "running");
        assert!(state["cpu_usage_ns"].is_null());
        assert!(state["memory_mb"].is_null());
        assert!(state["task_count"].is_null());
        assert!(
            state["telemetry_error"]
                .as_str()
                .unwrap()
                .contains("accounting is unavailable")
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
