mod tui;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use init_core::{
    config::{Limits, Manifest, Project},
    control::{AllocationRequest, Controller, DeploymentRequest, DeploymentService},
    default_config_path, default_workspace, discovery, manifest_io, runtime, server,
};
use serde_json::json;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(
    name = "init",
    bin_name = "init",
    version,
    about = "Local infrastructure for containers and native services"
)]
struct Cli {
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize an isolated workspace; existing services remain observe-only.
    Init {
        #[arg(long)]
        discover: bool,
        #[arg(long)]
        examples: bool,
    },
    /// Print an inventory suitable for importing into a manifest (read-only).
    Discover,
    /// Check backend availability, manifest, and host capacity.
    Doctor,
    /// Validate a JSON or YAML manifest without contacting service runtimes.
    Validate,
    /// Show the changes apply would make, without changing workloads.
    Plan,
    /// Apply enabled managed service declarations and CPU/memory limits.
    Apply {
        service: Option<String>,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Logs {
        service: String,
        #[arg(long, default_value_t = 200)]
        tail: usize,
    },
    Start {
        service: String,
    },
    Stop {
        service: String,
    },
    Restart {
        service: String,
    },
    /// Execute a job now (also resumes its interval schedule).
    Run {
        service: String,
    },
    /// Replace a STOPPED container using the current manifest; retain volumes.
    Recreate {
        service: String,
    },
    /// Preview or deploy a new image for an existing managed Docker daemon.
    DeployImage {
        service: String,
        image: String,
        #[arg(long)]
        apply: bool,
        /// Build with the configured Docker engine before stopping the service.
        #[arg(long, conflicts_with = "pull")]
        build_context: Option<PathBuf>,
        /// Pull the image before stopping the service.
        #[arg(long)]
        pull: bool,
    },
    /// Preview or deploy a new service from a public GitHub repository's Dockerfile.
    DeployGithub {
        service: String,
        repository: String,
        /// Branch or tag; omitted uses the repository's default branch.
        #[arg(long)]
        reference: Option<String>,
        /// Directory containing Dockerfile, relative to the repository root.
        #[arg(long, default_value = ".")]
        context: String,
        #[arg(long, default_value_t = 0.5)]
        cpus: f64,
        #[arg(long, default_value_t = 256)]
        memory_mb: u64,
        /// Loopback port mapping, for example 127.0.0.1:8080:80 (repeatable).
        #[arg(long = "port")]
        ports: Vec<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        apply: bool,
    },
    /// Change a managed service's hard limits live and persist its declaration.
    Scale {
        service: String,
        #[arg(long)]
        cpus: f64,
        #[arg(long)]
        memory_mb: u64,
    },
    /// Change the workspace admission budget; allocations must still fit.
    Budget {
        #[arg(long)]
        cpus: f64,
        #[arg(long)]
        memory_mb: u64,
    },
    /// Preview or apply bounded weighted allocation across managed services.
    Allocate {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Preview a Docker deployment declaration; --apply explicitly creates it.
    #[command(
        after_help = "Examples:\n  init deploy worker.yaml\n  init deploy worker.yaml --apply\n  cat worker.yaml | init deploy -\n  init deploy --schema\n  init deploy --example --format json\n\nFILE is a bare deployment service declaration, not a complete workspace manifest.\nSuccess is JSON on stdout. Errors are JSON on stderr (2 = input, 1 = runtime)."
    )]
    Deploy {
        #[arg(value_name = "FILE", required_unless_present_any = ["schema", "example"], conflicts_with_all = ["schema", "example"])]
        file: Option<PathBuf>,
        /// Create and start the service after validating it; omitted means preview.
        #[arg(long, conflicts_with_all = ["schema", "example"])]
        apply: bool,
        /// Print the JSON Schema without loading a workspace or contacting Docker.
        #[arg(long, conflicts_with = "example")]
        schema: bool,
        /// Print a complete example without loading a workspace.
        #[arg(long)]
        example: bool,
        /// Override input format; stdin accepts YAML or JSON by default.
        #[arg(long, value_enum)]
        format: Option<DeclarationFormat>,
    },
    /// List or create named project workspaces; output is JSON.
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Open the interactive terminal interface.
    Tui,
    /// Reconcile daemons and due jobs once (useful with external timers).
    Tick,
    /// Start the loopback dashboard and continuous job scheduler.
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    /// Install this controller as a per-user macOS background service.
    InstallLaunchd {
        #[arg(long, default_value_t = 8787)]
        port: u16,
    },
    UninstallLaunchd,
}

#[derive(Subcommand)]
enum ProjectCommand {
    List,
    Create {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum DeclarationFormat {
    Json,
    Yaml,
}

struct AgentError {
    code: &'static str,
    message: String,
    exit: i32,
}

impl AgentError {
    fn input(error: impl std::fmt::Display) -> Self {
        Self {
            code: "invalid_input",
            message: error.to_string(),
            exit: 2,
        }
    }
    fn runtime(error: anyhow::Error) -> Self {
        Self {
            code: "command_failed",
            message: format!("{error:#}"),
            exit: 1,
        }
    }
    fn print(&self) {
        eprintln!(
            "{}",
            json!({"ok":false,"error":{"code":self.code,"message":self.message}})
        );
    }
}

fn declaration_path(path: &Path, format: Option<DeclarationFormat>) -> PathBuf {
    match format {
        Some(DeclarationFormat::Json) => PathBuf::from("deployment.json"),
        Some(DeclarationFormat::Yaml) => PathBuf::from("deployment.yaml"),
        None => path.to_owned(),
    }
}

fn read_deployment(
    mut reader: impl Read,
    path: &Path,
    format: Option<DeclarationFormat>,
) -> Result<DeploymentService> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(manifest_io::MAX_MANIFEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    manifest_io::decode(&declaration_path(path, format), &bytes)
}

pub(crate) fn load_deployment_file(path: &Path) -> Result<DeploymentService> {
    read_deployment(
        fs::File::open(path)
            .with_context(|| format!("Cannot read deployment {}", path.display()))?,
        path,
        None,
    )
}

fn example_deployment() -> DeploymentService {
    serde_json::from_str(include_str!("../examples/deploy-service.json"))
        .expect("embedded deployment example is valid")
}

fn deployment_schema() -> serde_json::Value {
    json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"Init deployment service",
        "description":"A bare service declaration for init deploy. Applying requires the CLI --apply flag. Runtime admission, ownership, and workspace/project checks remain authoritative.",
        "type":"object","additionalProperties":false,"required":["id","image","limits"],
        "properties":{
            "id":{"type":"string","pattern":"^[a-z][a-z0-9-]{0,47}$"},
            "name":{"type":"string","maxLength":128,"default":""},
            "description":{"type":"string","maxLength":1024,"default":""},
            "image":{"type":"string","minLength":1,"maxLength":512,"description":"Container image for this declaration. Public GitHub repository builds use the separate deploy-github command."},
            "project":{"type":["string","null"],"description":"Existing project workspace ID."},
            "kind":{"enum":["daemon","job"],"default":"daemon"},
            "limits":{"type":"object","additionalProperties":false,"required":["cpus","memory_mb"],"properties":{"cpus":{"type":"number","minimum":0.01,"maximum":65536},"memory_mb":{"type":"integer","minimum":6,"maximum":1099511627776_u64}}},
            "command":{"type":"array","maxItems":64,"items":{"type":"string","maxLength":1024},"default":[]},
            "workspace":{"type":"boolean","default":false,"description":"Provision a persistent private volume at /workspace with no initial image copy-up."},
            "working_dir":{"type":["string","null"],"description":"Absolute path inside the container; /workspace is supplied when workspace=true."},
            "ports":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":64},"default":[],"description":"Loopback-only mappings such as 127.0.0.1:8080:80."},
            "env_file":{"type":["string","null"],"maxLength":1024,"description":"Existing environment file path, never its contents."},
            "volumes":{"type":"array","maxItems":32,"default":[],"items":{"type":"object","additionalProperties":false,"required":["source","target"],"properties":{"source":{"type":"string","maxLength":1024},"target":{"type":"string","maxLength":1024},"read_only":{"type":"boolean","default":false},"no_copy":{"type":"boolean","default":false}}}},
            "interval_seconds":{"type":["integer","null"],"minimum":30,"maximum":31536000,"description":"Required for jobs; jobs run immediately on apply, then on this interval while a daemon is running."},
            "allocation":{"type":"object","additionalProperties":false,"properties":{"weight":{"type":"number","default":1},"min_cpus":{"type":"number","default":0.1},"min_memory_mb":{"type":"integer","default":64}}}
        }
    })
}

async fn agent_command(cli: Cli) -> std::result::Result<(), AgentError> {
    if let Command::Deploy { schema: true, .. } = &cli.command {
        return print_json(&deployment_schema()).map_err(AgentError::runtime);
    }
    if let Command::Deploy {
        example: true,
        format,
        ..
    } = &cli.command
    {
        let path = declaration_path(Path::new("deployment.yaml"), *format);
        let bytes =
            manifest_io::encode(&path, &example_deployment()).map_err(AgentError::runtime)?;
        use std::io::Write;
        std::io::stdout()
            .write_all(&bytes)
            .map_err(|error| AgentError::runtime(error.into()))?;
        return Ok(());
    }
    let workspace = absolute(
        cli.workspace
            .map_or_else(default_workspace, Ok)
            .map_err(AgentError::input)?,
    )
    .map_err(AgentError::input)?;
    let config = absolute(match cli.config {
        Some(path) => path,
        None => default_config_path(&workspace).map_err(AgentError::input)?,
    })
    .map_err(AgentError::input)?;
    let mut controller = Controller::new(workspace, config)
        .map_err(|error| AgentError::input(format!("{error:#}")))?;
    let value = match cli.command {
        Command::Deploy {
            file: Some(file),
            apply,
            format,
            ..
        } => {
            let service = if file == Path::new("-") {
                read_deployment(
                    std::io::stdin().lock(),
                    Path::new("deployment.yaml"),
                    format,
                )
            } else {
                fs::File::open(&file)
                    .with_context(|| format!("Cannot read deployment {}", file.display()))
                    .and_then(|reader| read_deployment(reader, &file, format))
            }
            .map_err(|error| AgentError::input(format!("{error:#}")))?;
            controller
                .deploy(DeploymentRequest {
                    apply,
                    service,
                    github: None,
                })
                .await
                .map_err(AgentError::runtime)?
        }
        Command::Projects {
            command: ProjectCommand::List,
        } => json!({"ok":true,"projects":controller.manifest.projects}),
        Command::Projects {
            command:
                ProjectCommand::Create {
                    id,
                    name,
                    description,
                },
        } => {
            let project = Project {
                id,
                name,
                description,
            };
            project.validate().map_err(AgentError::input)?;
            controller
                .create_project(project)
                .map_err(AgentError::runtime)?
        }
        _ => unreachable!(),
    };
    print_json(&value).map_err(AgentError::runtime)
}

fn is_agent_invocation(args: &[std::ffi::OsString]) -> bool {
    let mut args = args.iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--workspace" || arg == "--config" {
            args.next();
            continue;
        }
        let value = arg.to_string_lossy();
        if value.starts_with('-') {
            continue;
        }
        return matches!(value.as_ref(), "deploy" | "projects");
    }
    false
}

fn home() -> Result<PathBuf> {
    Ok(std::env::var_os("HOME")
        .context("HOME must be set, or specify --workspace")?
        .into())
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) if is_agent_invocation(&args) && error.use_stderr() => {
            AgentError {
                code: "invalid_arguments",
                message: error.to_string(),
                exit: 2,
            }
            .print();
            std::process::exit(2);
        }
        Err(error) => error.exit(),
    };
    if matches!(
        cli.command,
        Command::Deploy { .. } | Command::Projects { .. }
    ) {
        if let Err(error) = agent_command(cli).await {
            error.print();
            std::process::exit(error.exit);
        }
        return Ok(());
    }
    let workspace = absolute(match cli.workspace {
        Some(p) => p,
        None => default_workspace()?,
    })?;
    let config_path = absolute(match cli.config {
        Some(path) => path,
        None => default_config_path(&workspace)?,
    })?;
    if let Command::Init { discover, examples } = cli.command {
        ensure!(
            !config_path.exists(),
            "{} already exists; init never overwrites a manifest",
            config_path.display()
        );
        fs::create_dir_all(&workspace)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
        }
        let engine = discovery::engine_command().await;
        let mut m = if examples {
            serde_json::from_str::<Manifest>(include_str!("../examples/services.json"))?
        } else {
            Manifest::new(engine.clone())
        };
        m.engine.command = engine.clone();
        if discover {
            m.services.extend(discovery::discover(&engine).await?);
        }
        m.save(&config_path)?;
        println!(
            "Created {} ({} services). Run init plan, then init serve.",
            config_path.display(),
            m.services.len()
        );
        return Ok(());
    }
    if matches!(cli.command, Command::Discover) {
        let engine = if config_path.exists() {
            Manifest::load(&config_path)?.engine.command
        } else {
            discovery::engine_command().await
        };
        return print_json(
            &json!({"engine":{"command":engine},"services":discovery::discover(&engine).await?}),
        );
    }
    if let Command::InstallLaunchd { port } = cli.command {
        Manifest::load(&config_path)?;
        return install_launchd(&workspace, &config_path, port).await;
    }
    if matches!(cli.command, Command::UninstallLaunchd) {
        return uninstall_launchd().await;
    }
    if matches!(cli.command, Command::Validate) {
        let manifest = Manifest::load(&config_path)?;
        println!(
            "Valid manifest: {} ({} services)",
            config_path.display(),
            manifest.services.len()
        );
        return Ok(());
    }
    let mut controller = Controller::new(workspace, config_path)?;
    match cli.command {
        Command::Doctor => {
            let snapshot = controller.snapshot().await;
            let info = &snapshot["runtime"];
            let exceeds = info["available"] == true
                && (controller.manifest.budget.cpus > info["cpus"].as_f64().unwrap_or(0.)
                    || controller.manifest.budget.memory_mb as f64
                        > info["memory_mb"].as_f64().unwrap_or(0.));
            print_json(
                &json!({"manifest":"valid","runtime":info,"budget_exceeds_engine":exceeds,"observers_outside_budget":true,"native_macos_hard_limits":false,"services":snapshot["services"]}),
            )?;
            if controller
                .manifest
                .services
                .iter()
                .any(|s| s.managed() && s.runtime == "docker")
            {
                ensure!(
                    info["available"] == true && !exceeds,
                    "Docker backend or budget needs attention"
                );
            }
        }
        Command::Plan => print_json(&controller.plan().await?)?,
        Command::Apply { service } => {
            let ids = service.map(|s| vec![s]).unwrap_or_else(|| {
                controller
                    .manifest
                    .services
                    .iter()
                    .filter(|s| s.enabled && s.managed())
                    .map(|s| s.id.clone())
                    .collect()
            });
            for id in ids {
                print_json(&controller.action(&id, "apply", None).await?)?;
            }
        }
        Command::Status { json: as_json } => {
            let state = controller.snapshot().await;
            if as_json {
                print_json(&state)?;
            } else {
                println!(
                    "Workspace: {}   Pool: {} CPUs / {} MiB",
                    controller.workspace.display(),
                    controller.manifest.budget.cpus,
                    controller.manifest.budget.memory_mb
                );
                let width = state["services"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|s| s["id"].as_str())
                    .map(str::len)
                    .max()
                    .unwrap_or(7)
                    .max(7);
                println!(
                    "{:<width$} {:<10} {:<14} CONTROL",
                    "SERVICE", "RUNTIME", "STATUS"
                );
                for s in state["services"].as_array().unwrap() {
                    println!(
                        "{:<width$} {:<10} {:<14} {}",
                        s["id"].as_str().unwrap_or("?"),
                        s["runtime"].as_str().unwrap_or("?"),
                        s["status"].as_str().unwrap_or("?"),
                        if s["observe_only"] == true {
                            "monitor"
                        } else if s["enabled"] == false {
                            "disabled"
                        } else {
                            "managed"
                        }
                    );
                }
            }
        }
        Command::Logs { service, tail } => println!("{}", controller.logs(&service, tail).await?),
        Command::Start { service } => {
            print_json(&controller.action(&service, "start", None).await?)?
        }
        Command::Stop { service } => print_json(&controller.action(&service, "stop", None).await?)?,
        Command::Restart { service } => {
            print_json(&controller.action(&service, "restart", None).await?)?
        }
        Command::Run { service } => print_json(&controller.action(&service, "run", None).await?)?,
        Command::Recreate { service } => {
            print_json(&controller.action(&service, "recreate", None).await?)?
        }
        Command::DeployImage {
            service,
            image,
            apply,
            build_context,
            pull,
        } => print_json(
            &controller
                .deploy_image(&service, &image, apply, build_context.as_deref(), pull)
                .await?,
        )?,
        Command::DeployGithub {
            service,
            repository,
            reference,
            context,
            cpus,
            memory_mb,
            ports,
            project,
            apply,
        } => {
            let request: DeploymentRequest = serde_json::from_value(json!({
                "apply":apply,
                "github":{"repository":repository,"reference":reference,"context":context},
                "service":{"id":service,"limits":{"cpus":cpus,"memory_mb":memory_mb},"ports":ports,"project":project}
            }))?;
            print_json(&controller.deploy(request).await?)?;
        }
        Command::Scale {
            service,
            cpus,
            memory_mb,
        } => print_json(
            &controller
                .action(&service, "apply", Some(Limits { cpus, memory_mb }))
                .await?,
        )?,
        Command::Budget { cpus, memory_mb } => {
            print_json(&controller.set_budget(Limits { cpus, memory_mb }).await?)?
        }
        Command::Allocate { apply, enabled } => print_json(
            &controller
                .configure_allocation(AllocationRequest {
                    enabled,
                    apply,
                    ..Default::default()
                })
                .await?,
        )?,
        Command::Tui => tui::run(controller).await?,
        Command::Tick => controller.tick().await?,
        Command::Serve { port } => server::serve(Arc::new(Mutex::new(controller)), port).await?,
        _ => unreachable!(),
    }
    Ok(())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

const CONTROLLER_LABELS: [&str; 2] = ["io.init.controller", "io.self-host-init.controller"];

async fn stop_controller_registrations(
    launchctl: &[String],
    uid: &str,
    max_wait: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + max_wait;
    let (program, prefix) = launchctl
        .split_first()
        .context("Missing launchctl command")?;
    let command = |action: &str, label: &str| {
        let mut command = tokio::process::Command::new(program);
        command
            .args(prefix)
            .args([action, &format!("gui/{uid}/{label}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        command
    };
    for label in CONTROLLER_LABELS {
        // Already absent registrations can make bootout fail; verify absence below.
        let _ = tokio::time::timeout_at(deadline, command("bootout", label).output())
            .await
            .with_context(|| format!("Timed out stopping {label}; no new controller was started"))?
            .with_context(|| format!("Cannot stop {label}"))?;
    }
    let mut pending = CONTROLLER_LABELS.to_vec();
    loop {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Controller registrations still loaded after {max_wait:?}: {}; no new controller was started",
            pending.join(", ")
        );
        let mut remaining = Vec::new();
        for label in CONTROLLER_LABELS {
            let output = tokio::time::timeout_at(deadline, command("print", label).output())
                .await
                .with_context(|| {
                    format!("Timed out checking {label} while awaiting removal of {}; no new controller was started", pending.join(", "))
                })?
                .with_context(|| format!("Cannot check whether {label} was removed"))?;
            match output.status.code() {
                Some(113) => {} // launchctl's missing-service status.
                Some(0) => remaining.push(label),
                _ => anyhow::bail!(
                    "Cannot verify removal of {label} ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                        .chars()
                        .take(2048)
                        .collect::<String>()
                        .trim()
                ),
            }
        }
        if remaining.is_empty() {
            return Ok(());
        }
        pending = remaining;
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(100)).min(deadline),
        )
        .await;
    }
}

fn remove_controller_plists(agents: &Path) -> Result<()> {
    for label in CONTROLLER_LABELS {
        let plist = agents.join(format!("{label}.plist"));
        if plist.try_exists()? {
            fs::remove_file(plist)?;
        }
    }
    Ok(())
}

async fn install_launchd(workspace: &Path, config: &Path, port: u16) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "launchd installation is supported only on macOS"
    );
    ensure!(port > 0, "port must be positive");
    let bin = workspace.join("bin/init");
    fs::create_dir_all(bin.parent().unwrap())?;
    let current = std::env::current_exe()?;
    // Replace atomically: the currently running controller can continue using its inode.
    for destination in [&bin, &workspace.join("bin/selfhost")] {
        let staged = destination.with_extension("new");
        fs::copy(&current, &staged)?;
        fs::rename(staged, destination)?;
    }
    let logs = workspace.join("logs");
    fs::create_dir_all(&logs)?;
    let agents = home()?.join("Library/LaunchAgents");
    fs::create_dir_all(&agents)?;
    let plist = agents.join("io.init.controller.plist");
    let args = [
        bin.display().to_string(),
        "--workspace".into(),
        workspace.display().to_string(),
        "--config".into(),
        config.display().to_string(),
        "serve".into(),
        "--port".into(),
        port.to_string(),
    ];
    let strings = args
        .iter()
        .map(|s| format!("<string>{}</string>", xml(s)))
        .collect::<String>();
    let uid = runtime::run(&["id".into(), "-u".into()])
        .await?
        .trim()
        .to_string();
    // Match the existing controller's 120 second ExitTimeOut, plus removal grace.
    stop_controller_registrations(&["launchctl".into()], &uid, Duration::from_secs(125)).await?;
    let legacy_plist = agents.join("io.self-host-init.controller.plist");
    if legacy_plist.try_exists()? {
        fs::remove_file(legacy_plist)?;
    }
    fs::write(
        &plist,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>io.init.controller</string><key>ProgramArguments</key><array>{strings}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ExitTimeOut</key><integer>120</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",
            xml(&logs.join("controller.log").display().to_string()),
            xml(&logs.join("controller.error.log").display().to_string())
        ),
    )?;
    // bootout can return before launchd finishes removing the old registration.
    bootstrap_launchd(
        &[
            "launchctl".into(),
            "bootstrap".into(),
            format!("gui/{uid}"),
            plist.display().to_string(),
        ],
        Duration::from_secs(5),
    )
    .await?;
    println!("Controller installed. Dashboard: http://127.0.0.1:{port}");
    Ok(())
}

async fn bootstrap_launchd(command: &[String], max_wait: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + max_wait;
    let mut last_error = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, runtime::run(command)).await {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => break,
        }
        tokio::time::sleep_until(
            (tokio::time::Instant::now() + Duration::from_millis(250)).min(deadline),
        )
        .await;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("launchctl bootstrap timed out")))
        .with_context(|| format!("controller bootstrap did not succeed within {max_wait:?}"))
}

async fn uninstall_launchd() -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "launchd is supported only on macOS"
    );
    let uid = runtime::run(&["id".into(), "-u".into()])
        .await?
        .trim()
        .to_string();
    stop_controller_registrations(&["launchctl".into()], &uid, Duration::from_secs(125)).await?;
    remove_controller_plists(&home()?.join("Library/LaunchAgents"))?;
    println!("Controller removed. Workloads, volumes and workspace data retained.");
    Ok(())
}

#[cfg(test)]
mod deployment_tests {
    use super::*;

    #[test]
    fn canonical_and_compatibility_invocations_publish_init_help_and_the_same_deployment_contract()
    {
        for executable in ["init", "selfhost"] {
            let help = Cli::try_parse_from([executable, "--help"])
                .err()
                .unwrap()
                .to_string();
            assert!(help.contains("Usage: init"), "{help}");
            assert!(!help.contains("selfhost"), "{help}");
            let version = Cli::try_parse_from([executable, "--version"])
                .err()
                .unwrap()
                .to_string();
            assert_eq!(version, format!("init {}\n", env!("CARGO_PKG_VERSION")));
            assert!(matches!(
                Cli::try_parse_from([executable, "deploy", "-", "--apply"])
                    .unwrap()
                    .command,
                Command::Deploy { apply: true, .. }
            ));
        }
    }

    #[test]
    fn declarations_share_yaml_json_schema_and_reject_embedded_apply_or_unknown_fields() {
        let json = read_deployment(
            include_bytes!("../examples/deploy-service.json").as_slice(),
            Path::new("worker.json"),
            None,
        )
        .unwrap();
        let yaml = read_deployment(
            include_bytes!("../examples/deploy-service.yaml").as_slice(),
            Path::new("worker.yaml"),
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&json).unwrap(),
            serde_json::to_value(&yaml).unwrap()
        );
        let schema = deployment_schema();
        for key in serde_json::to_value(&json)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
        {
            assert!(schema["properties"].get(key).is_some(), "{key}");
        }
        for invalid in [
            br#"{"id":"worker","image":"alpine:3","limits":{"cpus":1,"memory_mb":128},"apply":true}"#.as_slice(),
            b"id: worker\nid: replacement\nimage: alpine:3\nlimits: {cpus: 1, memory_mb: 128}\n".as_slice(),
            b"id: worker\nimage: alpine:3\nlimits: {cpus: 1, memory_mb: 128}\nunknown: true\n".as_slice(),
        ] {
            assert!(read_deployment(invalid,Path::new("stdin.yaml"),None).is_err());
        }
        assert!(
            read_deployment(
                vec![b' '; manifest_io::MAX_MANIFEST_BYTES + 1].as_slice(),
                Path::new("large.yaml"),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn deploy_flags_require_a_file_and_only_explicit_apply_changes_mode() {
        let cli = Cli::try_parse_from(["selfhost", "deploy", "-"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Deploy {
                apply: false,
                file: Some(_),
                ..
            }
        ));
        let cli = Cli::try_parse_from(["selfhost", "deploy", "worker.json", "--apply"]).unwrap();
        assert!(matches!(cli.command, Command::Deploy { apply: true, .. }));
        for args in [
            vec!["selfhost", "deploy"],
            vec!["selfhost", "deploy", "--example", "--apply"],
            vec!["selfhost", "deploy", "worker.yaml", "--schema"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
        assert!(Cli::try_parse_from(["selfhost", "deploy", "--schema"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "selfhost",
                "projects",
                "create",
                "tools",
                "--name",
                "Build tools"
            ])
            .is_ok()
        );
        let strings = |args: &[&str]| {
            args.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        assert!(is_agent_invocation(&strings(&[
            "selfhost",
            "--workspace",
            "/tmp/location",
            "deploy",
            "bad.yaml"
        ])));
        assert!(!is_agent_invocation(&strings(&[
            "selfhost",
            "--workspace",
            "deploy",
            "status"
        ])));
    }

    #[tokio::test]
    async fn malformed_input_is_classified_without_contacting_runtime_or_changing_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.yaml");
        Manifest::new(vec!["this-engine-must-not-run".into()])
            .save(&config)
            .unwrap();
        let before = fs::read(&config).unwrap();
        let file = dir.path().join("worker.yaml");
        fs::write(&file, "id: broken\nunknown: true\n").unwrap();
        let cli = Cli {
            workspace: Some(dir.path().into()),
            config: Some(config.clone()),
            command: Command::Deploy {
                file: Some(file),
                apply: true,
                schema: false,
                example: false,
                format: None,
            },
        };
        let error = agent_command(cli).await.err().unwrap();
        assert_eq!(error.exit, 2);
        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("unknown field") || error.message.contains("missing field"));
        assert_eq!(fs::read(config).unwrap(), before);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn fake_registrations(dir: &Path, mode: &str) -> Vec<String> {
        let script = dir.join("registrations.sh");
        fs::write(
            &script,
            r#"dir=$1
mode=$2
action=$3
target=$4
printf '%s %s\n' "$action" "$target" >> "$dir/commands"
case "$action" in
    bootout) exit 1 ;;
    bootstrap) printf started > "$dir/new-controller"; exit 0 ;;
    print)
        case "$target" in */io.init.controller) exit 113 ;; esac
        case "$mode" in
            stuck) exit 0 ;;
            denied) printf 'permission denied\n' >&2; exit 1 ;;
            hung) exec /bin/sleep 10 ;;
            delayed)
                count=0
                if [ -f "$dir/checks" ]; then read -r count < "$dir/checks"; fi
                count=$((count + 1))
                printf '%s\n' "$count" > "$dir/checks"
                if [ "$count" -lt 3 ]; then exit 0; fi
                exit 113 ;;
        esac ;;
esac
exit 91
"#,
        )
        .unwrap();
        vec![
            "/bin/sh".into(),
            script.display().to_string(),
            dir.display().to_string(),
            mode.into(),
        ]
    }

    #[tokio::test]
    async fn replacement_waits_for_legacy_registration_to_disappear_before_bootstrap() {
        let dir = tempfile::tempdir().unwrap();
        let command = fake_registrations(dir.path(), "delayed");
        stop_controller_registrations(&command, "501", Duration::from_secs(2))
            .await
            .unwrap();
        let mut bootstrap = command;
        bootstrap.extend(["bootstrap".into(), "gui/501".into()]);
        bootstrap_launchd(&bootstrap, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("checks")).unwrap(),
            "3\n"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("new-controller")).unwrap(),
            "started"
        );
        let calls = fs::read_to_string(dir.path().join("commands")).unwrap();
        assert!(calls.starts_with(
            "bootout gui/501/io.init.controller\nbootout gui/501/io.self-host-init.controller\n"
        ));
        assert!(calls.ends_with("print gui/501/io.self-host-init.controller\nbootstrap gui/501\n"));
    }

    #[tokio::test]
    async fn replacement_fails_without_bootstrap_when_removal_cannot_be_verified() {
        for mode in ["stuck", "denied", "hung"] {
            let dir = tempfile::tempdir().unwrap();
            let command = fake_registrations(dir.path(), mode);
            let start = std::time::Instant::now();
            let replacement: Result<()> = async {
                stop_controller_registrations(&command, "501", Duration::from_millis(250)).await?;
                let mut bootstrap = command;
                bootstrap.extend(["bootstrap".into(), "gui/501".into()]);
                bootstrap_launchd(&bootstrap, Duration::from_secs(1)).await
            }
            .await;
            let error = format!("{:#}", replacement.unwrap_err());
            assert!(
                error.contains("io.self-host-init.controller"),
                "{mode}: {error}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "{mode} must be bounded"
            );
            assert!(
                !dir.path().join("new-controller").exists(),
                "{mode} must block bootstrap"
            );
            assert!(
                !fs::read_to_string(dir.path().join("commands"))
                    .unwrap()
                    .contains("bootstrap")
            );
        }
    }

    #[tokio::test]
    async fn controller_registration_cleanup_targets_both_product_names_and_retains_unrelated_agents()
     {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("launchctl.sh");
        let log = dir.path().join("commands");
        fs::write(
            &script,
            "log=$1\nshift\nprintf '%s %s\\n' \"$1\" \"$2\" >> \"$log\"\nif [ \"$1\" = print ]; then exit 113; fi\nexit 1\n",
        )
        .unwrap();
        stop_controller_registrations(
            &[
                "/bin/sh".into(),
                script.display().to_string(),
                log.display().to_string(),
            ],
            "501",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(log).unwrap(),
            "bootout gui/501/io.init.controller\nbootout gui/501/io.self-host-init.controller\nprint gui/501/io.init.controller\nprint gui/501/io.self-host-init.controller\n"
        );
        for name in [
            "io.init.controller.plist",
            "io.self-host-init.controller.plist",
            "io.unrelated.controller.plist",
        ] {
            fs::write(dir.path().join(name), "existing registration").unwrap();
        }
        remove_controller_plists(dir.path()).unwrap();
        assert!(!dir.path().join("io.init.controller.plist").exists());
        assert!(
            !dir.path()
                .join("io.self-host-init.controller.plist")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("io.unrelated.controller.plist")).unwrap(),
            "existing registration"
        );
        remove_controller_plists(dir.path()).unwrap();
    }

    fn fake_bootstrap(dir: &Path, failures: usize) -> Vec<String> {
        let script = dir.join("bootstrap.sh");
        fs::write(
            &script,
            r#"count=0
if [ -f "$1" ]; then read -r count < "$1"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$1"
if [ "$count" -le "$2" ]; then
    printf '%s\n' 'Bootstrap failed: 5: Input/output error' >&2
    exit 5
fi
"#,
        )
        .unwrap();
        vec![
            "/bin/sh".into(),
            script.display().to_string(),
            dir.join("attempts").display().to_string(),
            failures.to_string(),
        ]
    }

    #[tokio::test]
    async fn bootstrap_retries_transient_launchd_registration_failure() {
        let dir = tempfile::tempdir().unwrap();
        bootstrap_launchd(&fake_bootstrap(dir.path(), 2), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("attempts")).unwrap(),
            "3\n"
        );
    }

    #[tokio::test]
    async fn bootstrap_permanent_failure_is_bounded_and_preserves_last_error() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let error = bootstrap_launchd(
            &fake_bootstrap(dir.path(), 9999),
            Duration::from_millis(650),
        )
        .await
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        let attempts: usize = fs::read_to_string(dir.path().join("attempts"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!((2..=4).contains(&attempts));
        let message = format!("{error:#}");
        assert!(message.contains("within 650ms"), "{message}");
        assert!(
            message.contains("Bootstrap failed: 5: Input/output error"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn bootstrap_deadline_also_bounds_a_hung_command() {
        let start = std::time::Instant::now();
        let error = bootstrap_launchd(
            &["/bin/sleep".into(), "10".into()],
            Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(format!("{error:#}").contains("launchctl bootstrap timed out"));
    }
}
