use serde::Serialize;
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const EXTERNAL_DAEMON: &str =
    "Close the separately started Init daemon, then reopen Init to update safely.";

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Status {
    Idle,
    Checking,
    Available {
        version: String,
    },
    Downloading {
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    Installing {
        version: String,
    },
    Restarting {
        version: String,
    },
    Error {
        version: String,
        message: String,
    },
}

impl Status {
    fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking
                | Self::Downloading { .. }
                | Self::Installing { .. }
                | Self::Restarting { .. }
        )
    }

    fn installing(&self) -> bool {
        matches!(
            self,
            Self::Downloading { .. } | Self::Installing { .. } | Self::Restarting { .. }
        )
    }
}

#[derive(Clone, Serialize)]
pub struct Snapshot {
    #[serde(flatten)]
    status: Status,
    dismissed: bool,
    blocked: Option<&'static str>,
}

struct Session {
    status: Status,
    dismissed: bool,
    update: Option<Arc<Update>>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            status: Status::Idle,
            dismissed: false,
            update: None,
        }
    }
}

impl Session {
    fn begin_check(&mut self) -> Option<Status> {
        if self.status.busy() {
            return None;
        }
        Some(std::mem::replace(&mut self.status, Status::Checking))
    }

    fn skip(&mut self) -> Result<(), String> {
        if !matches!(self.status, Status::Available { .. } | Status::Error { .. }) {
            return Err("An update cannot be skipped while it is running".into());
        }
        self.dismissed = true;
        Ok(())
    }

    fn begin_install(&mut self, version: String) -> Result<(), String> {
        if !matches!(self.status, Status::Available { .. } | Status::Error { .. }) {
            return Err("An update is already running or no update is available".into());
        }
        self.status = Status::Downloading {
            version,
            downloaded: 0,
            total: None,
        };
        self.dismissed = false;
        Ok(())
    }

    fn finish_install(&mut self, version: String, result: Result<(), String>) -> bool {
        self.status = match result {
            Ok(()) => Status::Restarting { version },
            Err(message) => Status::Error { version, message },
        };
        matches!(self.status, Status::Restarting { .. })
    }
}

pub struct Updater {
    session: Mutex<Session>,
    port: u16,
    owns_daemon: bool,
    pub restart_requested: AtomicBool,
}

impl Updater {
    pub fn new(port: u16, owns_daemon: bool) -> Self {
        Self {
            session: Mutex::new(Session::default()),
            port,
            owns_daemon,
            restart_requested: AtomicBool::new(false),
        }
    }

    pub fn installing(&self) -> bool {
        self.session.lock().unwrap().status.installing()
    }

    fn snapshot(&self) -> Snapshot {
        let session = self.session.lock().unwrap();
        Snapshot {
            status: session.status.clone(),
            dismissed: session.dismissed,
            blocked: (!self.owns_daemon).then_some(EXTERNAL_DAEMON),
        }
    }

    fn authorize(&self, window: &WebviewWindow) -> Result<(), String> {
        if window.label() == "main"
            && window
                .url()
                .is_ok_and(|url| super::allowed_navigation(&url, self.port))
        {
            Ok(())
        } else {
            Err("Updater access is restricted to Init's local window".into())
        }
    }
}

fn allowed_download(url: &tauri::Url, version: &str) -> bool {
    let prefix = format!("/djsydney04/init/releases/download/v{version}/");
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().starts_with(&prefix)
        && url.path().len() > prefix.len()
        && !url.path()[prefix.len()..].contains('/')
        && !url.path().contains('%')
}

async fn check(app: &AppHandle) {
    let state = app.state::<Updater>();
    let previous = match state.session.lock().unwrap().begin_check() {
        Some(previous) => previous,
        None => return,
    };
    let result = async {
        let updater = app
            .updater_builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        tokio::time::timeout(Duration::from_secs(30), updater.check())
            .await?
            .map_err(anyhow::Error::from)
    }
    .await;
    let mut session = state.session.lock().unwrap();
    match result {
        Ok(Some(mut update)) if allowed_download(&update.download_url, &update.version) => {
            update.timeout = Some(Duration::from_secs(10 * 60));
            session.status = Status::Available {
                version: update.version.clone(),
            };
            session.update = Some(Arc::new(update));
        }
        Ok(None) => {
            session.status = Status::Idle;
            session.update = None;
        }
        Ok(Some(_)) => {
            eprintln!("Init: rejected an update outside the versioned GitHub release");
            session.status = previous;
        }
        Err(error) => {
            eprintln!("Init: update check failed: {error}");
            session.status = previous;
        }
    }
}

async fn scheduled_checks<F, Fut>(mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        check().await;
    }
}

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        scheduled_checks(|| check(&app)).await;
    });
}

pub fn capability(port: u16) -> String {
    serde_json::json!({
        "identifier": "desktop-updater",
        "description": "Only Init update controls on the local daemon origin",
        "local": false,
        "remote": { "urls": [format!("http://127.0.0.1:{port}/*")] },
        "webviews": ["main"],
        "permissions": ["allow-updater-status", "allow-updater-install", "allow-updater-skip"]
    })
    .to_string()
}

pub fn bridge(port: u16) -> String {
    format!(
        r#"if (window.top === window && location.origin === "http://127.0.0.1:{port}") {{
        Object.defineProperty(window, '__INIT_UPDATER__', {{value: Object.freeze({{
            status: () => window.__TAURI_INTERNALS__.invoke('updater_status'),
            install: () => window.__TAURI_INTERNALS__.invoke('updater_install'),
            skip: () => window.__TAURI_INTERNALS__.invoke('updater_skip')
        }})}});
    }}"#
    )
}

#[tauri::command]
pub fn updater_status(
    window: WebviewWindow,
    state: State<'_, Updater>,
) -> Result<Snapshot, String> {
    state.authorize(&window)?;
    Ok(state.snapshot())
}

#[tauri::command]
pub fn updater_skip(window: WebviewWindow, state: State<'_, Updater>) -> Result<Snapshot, String> {
    state.authorize(&window)?;
    state.session.lock().unwrap().skip()?;
    Ok(state.snapshot())
}

#[tauri::command]
pub fn updater_install(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, Updater>,
) -> Result<Snapshot, String> {
    state.authorize(&window)?;
    if !state.owns_daemon {
        return Err(EXTERNAL_DAEMON.into());
    }
    let update = {
        let mut session = state.session.lock().unwrap();
        let update = session.update.clone().ok_or("No update is available")?;
        if !allowed_download(&update.download_url, &update.version) {
            return Err("Untrusted update download".into());
        }
        session.begin_install(update.version.clone())?;
        update
    };
    tauri::async_runtime::spawn(async move {
        let result = install(&app, update.clone())
            .await
            .map_err(|error| format!("{error:#}"));
        let state = app.state::<Updater>();
        let restart = state
            .session
            .lock()
            .unwrap()
            .finish_install(update.version.clone(), result);
        if restart {
            state.restart_requested.store(true, Ordering::SeqCst);
            // main drains the owned daemon before launching the new binary.
            app.exit(0);
        }
    });
    Ok(state.snapshot())
}

async fn install(app: &AppHandle, update: Arc<Update>) -> anyhow::Result<()> {
    let bytes = update
        .download(
            |chunk, total| {
                let state = app.state::<Updater>();
                if let Status::Downloading {
                    downloaded,
                    total: length,
                    ..
                } = &mut state.session.lock().unwrap().status
                {
                    *downloaded = downloaded.saturating_add(chunk as u64);
                    *length = total;
                }
            },
            || {},
        )
        .await?;
    app.state::<Updater>().session.lock().unwrap().status = Status::Installing {
        version: update.version.clone(),
    };
    tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_lasts_for_the_process_and_cannot_interrupt_an_install() {
        let mut session = Session {
            status: Status::Available {
                version: "0.6.0".into(),
            },
            ..Session::default()
        };
        session.skip().unwrap();
        assert!(session.dismissed);
        let previous = session.begin_check().unwrap();
        assert!(session.begin_check().is_none());
        session.status = previous;
        assert!(session.dismissed, "hourly checks must not undo Skip");
        assert!(
            !Session::default().dismissed,
            "reopening the app must show updates again"
        );
        session.begin_install("0.6.0".into()).unwrap();
        assert!(!session.dismissed);
        assert!(
            session.begin_install("0.6.0".into()).is_err(),
            "double clicks must not start another install"
        );
        assert!(session.skip().is_err());
        assert!(session.begin_check().is_none());
        assert!(!session.finish_install("0.6.0".into(), Err("Download failed".into())));
        assert_eq!(
            session.status,
            Status::Error {
                version: "0.6.0".into(),
                message: "Download failed".into()
            }
        );
        session.begin_install("0.6.0".into()).unwrap();
        assert!(matches!(
            session.status,
            Status::Downloading { downloaded: 0, .. }
        ));
        assert!(session.finish_install("0.6.0".into(), Ok(())));
        assert!(matches!(session.status, Status::Restarting { .. }));
        assert!(session.skip().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn checks_on_launch_and_every_hour_without_catchup_bursts() {
        use std::sync::atomic::AtomicUsize;
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let task = tokio::spawn(scheduled_checks(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            std::future::ready(())
        }));
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(3599)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(4 * 3600)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "wake from sleep must perform one check"
        );
        task.abort();
    }

    #[test]
    fn downloads_are_confined_to_the_announced_github_release() {
        assert!(allowed_download(
            &"https://github.com/djsydney04/init/releases/download/v0.6.0/Init.app.tar.gz"
                .parse()
                .unwrap(),
            "0.6.0"
        ));
        for url in [
            "http://github.com/djsydney04/init/releases/download/v0.6.0/a",
            "https://example.com/djsydney04/init/releases/download/v0.6.0/a",
            "https://github.com/other/init/releases/download/v0.6.0/a",
            "https://github.com/djsydney04/init/releases/download/v0.5.0/a",
            "https://github.com/djsydney04/init/releases/download/v0.6.0/a?x=1",
            "https://user@github.com/djsydney04/init/releases/download/v0.6.0/a",
            "https://github.com:444/djsydney04/init/releases/download/v0.6.0/a",
        ] {
            assert!(!allowed_download(&url.parse().unwrap(), "0.6.0"), "{url}");
        }
        let capability: serde_json::Value = serde_json::from_str(&capability(9988)).unwrap();
        assert_eq!(
            capability["remote"]["urls"],
            serde_json::json!(["http://127.0.0.1:9988/*"])
        );
        assert_eq!(capability["local"], false);
        assert_eq!(capability["webviews"], serde_json::json!(["main"]));
        assert_eq!(capability["permissions"].as_array().unwrap().len(), 3);
        assert!(
            capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p.as_str().unwrap().starts_with("allow-updater-"))
        );
    }
}
