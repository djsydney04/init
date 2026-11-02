//! The webview is a restricted client of the same local daemon as the CLI/TUI.

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use init_core::{
    config::Manifest, control::Controller, default_config_path, default_workspace, discovery,
    server,
};
use serde::Deserialize;
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

mod updater;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

#[derive(Parser)]
#[command(version, about = "Init native desktop app")]
struct Options {
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value_t = 8787, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

#[derive(Deserialize)]
struct Health {
    app: String,
    version: String,
    workspace: PathBuf,
    config: PathBuf,
}

struct OwnedDaemon {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

impl OwnedDaemon {
    async fn shutdown(self) -> Result<()> {
        let _ = self.stop.send(());
        self.task.await.context("Daemon task failed")?
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn parse_health(response: &[u8]) -> Result<Health> {
    let response = std::str::from_utf8(response).context("Invalid daemon health response")?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("Incomplete daemon health response")?;
    ensure!(
        headers.lines().next() == Some("HTTP/1.1 200 OK"),
        "Port is occupied by an incompatible daemon; stop it and reopen the app"
    );
    serde_json::from_str(body).context("Port is occupied by a service that is not Init")
}

fn probe(port: u16) -> Result<Option<Health>> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(None),
        Err(error) => return Err(error).context("Cannot connect to the local daemon"),
    };
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream.take(16385).read_to_end(&mut response)?;
    ensure!(
        response.len() <= 16384,
        "Daemon health response is too large"
    );
    Ok(Some(parse_health(&response)?))
}

fn validate_health(health: &Health, workspace: &Path, config: &Path) -> Result<()> {
    ensure!(
        init_core::is_controller_app(&health.app) && health.version == env!("CARGO_PKG_VERSION"),
        "An incompatible daemon is already running. Restart it with Init {} before opening this app",
        env!("CARGO_PKG_VERSION")
    );
    ensure!(
        fs::canonicalize(&health.workspace)? == fs::canonicalize(workspace)?
            && fs::canonicalize(&health.config)? == fs::canonicalize(config)?,
        "A different workspace is already using this port. Use --port to open this workspace separately"
    );
    Ok(())
}

async fn ensure_daemon(
    workspace: PathBuf,
    config: PathBuf,
    port: u16,
) -> Result<Option<OwnedDaemon>> {
    if let Some(health) = probe(port)? {
        validate_health(&health, &workspace, &config)?;
        return Ok(None);
    }
    if !config.exists() {
        let created = !workspace.exists();
        fs::create_dir_all(&workspace)?;
        #[cfg(unix)]
        if created {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700))?;
        }
        // An empty manifest never adopts or changes existing machine services.
        Manifest::new(discovery::engine_command().await).save(&config)?;
    }
    let controller = Arc::new(Mutex::new(Controller::new(
        workspace.clone(),
        config.clone(),
    )?));
    let (stop, stopped) = oneshot::channel();
    let daemon = OwnedDaemon {
        stop,
        task: tokio::spawn(server::serve_until(controller, port, async move {
            let _ = stopped.await;
        })),
    };
    let readiness = async {
        for _ in 0..80 {
            if let Some(health) = probe(port)? {
                return validate_health(&health, &workspace, &config);
            }
            ensure!(
                !daemon.task.is_finished(),
                "The local daemon exited before it became ready"
            );
            tokio::time::sleep(Duration::from_millis(125)).await;
        }
        bail!("The local daemon did not start within 10 seconds")
    }
    .await;
    if let Err(error) = readiness {
        daemon.shutdown().await.context("Daemon startup failed")?;
        return Err(error);
    }
    Ok(Some(daemon))
}

fn allowed_navigation(url: &tauri::Url, port: u16) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port_or_known_default() == Some(port)
        && url.username().is_empty()
        && url.password().is_none()
}

fn main() -> Result<()> {
    let options = Options::parse();
    let port = options.port;
    // Keep the runtime alive until the owned daemon finishes its current work.
    let runtime = tokio::runtime::Runtime::new()?;
    let startup = runtime.block_on(async move {
        let workspace = absolute(match options.workspace {
            Some(workspace) => workspace,
            None => default_workspace()?,
        })?;
        let config = absolute(match options.config {
            Some(config) => config,
            None => default_config_path(&workspace)?,
        })?;
        ensure_daemon(workspace, config, port).await
    });
    let (daemon, startup) = match startup {
        Ok(daemon) => (daemon, Ok(())),
        Err(error) => (None, Err(error)),
    };
    let owns_daemon = daemon.is_some();
    let app = tauri::Builder::default()
        .manage(updater::Updater::new(port, owns_daemon))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            updater::updater_status,
            updater::updater_install,
            updater::updater_skip
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.state::<updater::Updater>().installing()
            {
                api.prevent_close();
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let opened = startup.and_then(|_| {
                app.add_capability(updater::capability(port).as_str())?;
                WebviewWindowBuilder::new(
                    app,
                    "main",
                    WebviewUrl::External(format!("http://127.0.0.1:{port}/").parse()?),
                )
                .title("Init")
                .inner_size(1280., 840.)
                .min_inner_size(800., 600.)
                .on_navigation(move |url| allowed_navigation(url, port))
                .initialization_script(updater::bridge(port))
                .build()?;
                updater::start(app.handle().clone());
                Ok(())
            });
            if let Err(error) = opened {
                eprintln!("Init: {error:#}");
                let handle = app.handle().clone();
                app.dialog()
                    .message(format!("{error:#}"))
                    .title("Init could not start")
                    .kind(MessageDialogKind::Error)
                    .show(move |_| handle.exit(1));
            }
            Ok(())
        })
        .build(tauri::generate_context!());
    // Tauri's run() exits the process directly, bypassing asynchronous cleanup.
    let mut restart = None;
    let exit = app.map(|app| {
        let handle = app.handle().clone();
        let env = app.env();
        let exit = app.run_return(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<updater::Updater>();
                if state.installing()
                    && !state
                        .restart_requested
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    api.prevent_exit();
                }
            }
        });
        if handle
            .state::<updater::Updater>()
            .restart_requested
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            restart = Some(env);
        }
        exit
    });
    let shutdown = match daemon {
        Some(daemon) => runtime.block_on(daemon.shutdown()),
        None => Ok(()),
    };
    if let Some(env) = restart {
        if let Err(error) = shutdown {
            eprintln!("Init: daemon shutdown after update failed: {error:#}");
        }
        tauri::process::restart(&env);
    }
    shutdown?;
    ensure!(exit? == 0, "The desktop app could not start");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_confined_to_the_daemon_origin() {
        for url in [
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:8787/services?id=ci#logs",
        ] {
            assert!(allowed_navigation(&url.parse().unwrap(), 8787));
        }
        for url in [
            "http://127.0.0.1:8788/",
            "https://127.0.0.1:8787/",
            "http://localhost:8787/",
            "https://example.com/",
            "http://user@127.0.0.1:8787/",
            "file:///etc/passwd",
        ] {
            assert!(!allowed_navigation(&url.parse().unwrap(), 8787));
        }
    }

    #[test]
    fn health_requires_complete_successful_identity() {
        let valid = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"app\":\"self-host-init\",\"version\":\"0.2.0\",\"workspace\":\"/tmp/workspace\",\"config\":\"/tmp/workspace/services.json\"}";
        assert_eq!(parse_health(valid).unwrap().app, "self-host-init");
        assert!(parse_health(b"HTTP/1.1 404 Not Found\r\n\r\n{}").is_err());
        assert!(parse_health(b"HTTP/1.1 200 OK\r\n\r\n{}").is_err());
        assert!(parse_health(b"HTTP/1.1 200 OK\r\n").is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_startup_and_attachment_preserve_workspace_identity() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let config = workspace.join("services.yaml");
        Manifest::new(vec!["docker".into()]).save(&config).unwrap();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let daemon = ensure_daemon(workspace.clone(), config.clone(), port)
            .await
            .unwrap()
            .expect("desktop should own the new daemon");
        let health = probe(port).unwrap().unwrap();
        validate_health(&health, &workspace, &config).unwrap();
        assert_eq!(health.app, "init");
        for app in ["init", "self-host-init"] {
            let alias = Health {
                app: app.into(),
                version: health.version.clone(),
                workspace: workspace.clone(),
                config: config.clone(),
            };
            validate_health(&alias, &workspace, &config).unwrap();
        }
        let unrelated = Health {
            app: "unrelated".into(),
            version: health.version.clone(),
            workspace: workspace.clone(),
            config: config.clone(),
        };
        assert!(validate_health(&unrelated, &workspace, &config).is_err());
        let attachment = ensure_daemon(workspace.clone(), config.clone(), port)
            .await
            .unwrap();
        assert!(
            attachment.is_none(),
            "attaching must not take ownership of another daemon"
        );
        drop(attachment);
        assert!(probe(port).unwrap().is_some());
        assert!(Manifest::load(&config).unwrap().services.is_empty());

        let other = directory.path().join("other");
        fs::create_dir(&other).unwrap();
        let other_config = other.join("services.yaml");
        fs::copy(&config, &other_config).unwrap();
        assert!(validate_health(&health, &other, &other_config).is_err());
        let stale = Health {
            version: "0.1.0".into(),
            ..health
        };
        assert!(validate_health(&stale, &workspace, &config).is_err());
        daemon.shutdown().await.unwrap();
        assert!(probe(port).unwrap().is_none());
    }

    #[tokio::test]
    async fn desktop_shutdown_waits_for_owned_daemon_to_drain() {
        let (stop, stopped) = oneshot::channel();
        let (draining, started) = oneshot::channel();
        let (finish, finished) = oneshot::channel();
        let daemon = OwnedDaemon {
            stop,
            task: tokio::spawn(async move {
                stopped.await.unwrap();
                draining.send(()).unwrap();
                finished.await.unwrap();
                Ok(())
            }),
        };
        let shutdown = tokio::spawn(daemon.shutdown());
        started.await.unwrap();
        assert!(
            !shutdown.is_finished(),
            "desktop must wait for the daemon's current work"
        );
        finish.send(()).unwrap();
        shutdown.await.unwrap().unwrap();
    }
}
