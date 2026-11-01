mod config;
mod control;
mod discovery;
mod runtime;
mod server;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use config::{Limits, Manifest};
use control::Controller;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
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
        None => home()?.join(".local/share/self-host-init"),
    })?;
    let config_path = absolute(
        cli.config
            .unwrap_or_else(|| workspace.join("services.json")),
    )?;
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
                println!(
                    "{:<38} {:<10} {:<14} CONTROL",
                    "SERVICE", "RUNTIME", "STATUS"
                );
                for s in state["services"].as_array().unwrap() {
                    println!(
                        "{:<38} {:<10} {:<14} {}",
                        s["id"].as_str().unwrap_or("?"),
                        s["runtime"].as_str().unwrap_or("?"),
                        s["status"].as_str().unwrap_or("?"),
                        if s["observe_only"] == true {
                            "observe"
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
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\"><plist version=\"1.0\"><dict><key>Label</key><string>io.self-host-init.controller</string><key>ProgramArguments</key><array>{strings}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",
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
    runtime::run(&[
        "launchctl".into(),
        "bootstrap".into(),
        format!("gui/{uid}"),
        plist.display().to_string(),
    ])
    .await?;
    println!("Controller installed. Dashboard: http://127.0.0.1:{port}");
    Ok(())
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
