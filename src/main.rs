mod tui;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use self_host_init::{
    config::{Limits, Manifest},
    control::{AllocationRequest, Controller, DeploymentRequest},
    default_config_path, default_workspace, discovery, runtime, server,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(
    name = "selfhost",
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
    let cli = Cli::parse();
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
            "Created {} ({} services). Run selfhost plan, then selfhost serve.",
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

async fn install_launchd(workspace: &Path, config: &Path, port: u16) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "launchd installation is supported only on macOS"
    );
    ensure!(port > 0, "port must be positive");
    let bin = workspace.join("bin/selfhost");
    fs::create_dir_all(bin.parent().unwrap())?;
    let current = std::env::current_exe()?;
    // Replace atomically: the currently running controller can continue using its inode.
    let staged = bin.with_extension("new");
    fs::copy(&current, &staged)?;
    fs::rename(staged, &bin)?;
    let logs = workspace.join("logs");
    fs::create_dir_all(&logs)?;
    let agents = home()?.join("Library/LaunchAgents");
    fs::create_dir_all(&agents)?;
    let plist = agents.join("io.self-host-init.controller.plist");
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
    fs::write(
        &plist,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>io.self-host-init.controller</string><key>ProgramArguments</key><array>{strings}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ExitTimeOut</key><integer>120</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",
            xml(&logs.join("controller.log").display().to_string()),
            xml(&logs.join("controller.error.log").display().to_string())
        ),
    )?;
    let uid = runtime::run(&["id".into(), "-u".into()])
        .await?
        .trim()
        .to_string();
    let _ = runtime::run(&[
        "launchctl".into(),
        "bootout".into(),
        format!("gui/{uid}/io.self-host-init.controller"),
    ])
    .await;
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
    let _ = runtime::run(&[
        "launchctl".into(),
        "bootout".into(),
        format!("gui/{uid}/io.self-host-init.controller"),
    ])
    .await;
    let plist = home()?.join("Library/LaunchAgents/io.self-host-init.controller.plist");
    if plist.exists() {
        fs::remove_file(plist)?;
    }
    println!("Controller removed. Workloads, volumes and workspace data retained.");
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
