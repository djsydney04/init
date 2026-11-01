//! The local dashboard shares the controller used by the CLI.

use std::{future::Future, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::Mutex, task::JoinSet};

use crate::{
    config::{Limits, Project},
    control::{AllocationRequest, Controller, DeploymentRequest},
};

#[derive(Clone)]
struct Access {
    port: u16,
    token: Arc<str>,
}

#[derive(Clone)]
struct AppState {
    controller: Arc<Mutex<Controller>>,
    mutations: Arc<Mutex<JoinSet<()>>>,
    access: Access,
    workspace: std::path::PathBuf,
    config: std::path::PathBuf,
}

fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({"error": message.to_string()}))).into_response()
}

async fn mutation(
    tasks: Arc<Mutex<JoinSet<()>>>,
    operation: impl Future<Output = Response> + Send + 'static,
) -> Response {
    let (complete, response) = tokio::sync::oneshot::channel();
    {
        let mut tasks = tasks.lock().await;
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                eprintln!("Mutation task failed: {error}");
            }
        }
        // The daemon owns accepted mutations even if their HTTP client leaves.
        tasks.spawn(async move {
            let _ = complete.send(operation.await);
        });
    }
    response.await.unwrap_or_else(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The mutation task failed; inspect the daemon log",
        )
    })
}

fn authorized(headers: &HeaderMap, method: &Method, access: &Access) -> Result<(), &'static str> {
    let hosts: Vec<_> = headers.get_all(header::HOST).iter().collect();
    if hosts.len() != 1 {
        return Err("Exactly one trusted Host header is required");
    }
    let host = hosts[0].to_str().map_err(|_| "Invalid Host header")?;
    if host != format!("127.0.0.1:{}", access.port)
        && host != format!("localhost:{}", access.port)
        && !(access.port == 80 && ["127.0.0.1", "localhost"].contains(&host))
    {
        return Err("Untrusted Host header");
    }
    let origins: Vec<_> = headers.get_all(header::ORIGIN).iter().collect();
    if origins.len() > 1
        || origins
            .first()
            .is_some_and(|origin| origin.to_str().ok() != Some(format!("http://{host}").as_str()))
    {
        return Err("Request origin does not match this dashboard");
    }
    if headers
        .get("sec-fetch-site")
        .is_some_and(|site| site == "cross-site")
    {
        return Err("Cross-site requests are not allowed");
    }
    if ![Method::GET, Method::HEAD, Method::OPTIONS].contains(method) {
        let tokens: Vec<_> = headers.get_all("x-selfhost-token").iter().collect();
        if tokens.len() != 1 || !token_matches(tokens[0].as_bytes(), access.token.as_bytes()) {
            return Err("Missing or invalid session token; refresh the dashboard");
        }
    }
    Ok(())
}

fn token_matches(candidate: &[u8], expected: &[u8]) -> bool {
    if candidate.len() != expected.len() {
        return false;
    }
    candidate
        .iter()
        .zip(expected)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

async fn boundary(State(access): State<Access>, request: Request, next: Next) -> Response {
    let mut response = match authorized(request.headers(), request.method(), &access) {
        Ok(()) => next.run(request).await,
        Err(message) => error(StatusCode::FORBIDDEN, message),
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"));
    response
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../assets/style.css"),
    )
}

async fn js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/app.js"),
    )
}

async fn session(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"token": state.access.token.as_ref()}))
}

async fn snapshot(State(state): State<AppState>) -> Json<Value> {
    Json(state.controller.lock().await.snapshot().await)
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(
        json!({"app":"self-host-init","version":env!("CARGO_PKG_VERSION"),"workspace":state.workspace,"config":state.config}),
    )
}

async fn allocation_plan(State(state): State<AppState>) -> Response {
    match state.controller.lock().await.allocation_plan().await {
        Ok(plan) => Json(plan).into_response(),
        Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
    }
}

async fn allocation(
    State(state): State<AppState>,
    payload: Result<Json<AllocationRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    mutation(state.mutations.clone(), async move {
        match state
            .controller
            .lock()
            .await
            .configure_allocation(request)
            .await
        {
            Ok(plan) => Json(plan).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

async fn deploy(
    State(state): State<AppState>,
    payload: Result<Json<DeploymentRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    mutation(state.mutations.clone(), async move {
        match state.controller.lock().await.deploy(request).await {
            Ok(result) => Json(result).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

async fn create_project(
    State(state): State<AppState>,
    payload: Result<Json<Project>, JsonRejection>,
) -> Response {
    let Json(project) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    mutation(state.mutations.clone(), async move {
        match state.controller.lock().await.create_project(project) {
            Ok(result) => Json(result).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionBody {
    limits: Option<Limits>,
}

async fn action(
    State(state): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    payload: Result<Json<ActionBody>, JsonRejection>,
) -> Response {
    if !["start", "stop", "restart", "run", "apply"].contains(&action.as_str()) {
        return error(StatusCode::NOT_FOUND, "Unknown service action");
    }
    let Json(body) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    if action != "apply" && body.limits.is_some() {
        return error(StatusCode::BAD_REQUEST, "Limits are only accepted by apply");
    }
    mutation(state.mutations.clone(), async move {
        match state
            .controller
            .lock()
            .await
            .action(&id, &action, body.limits)
            .await
        {
            Ok(result) => Json(result).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(default = "default_tail")]
    tail: usize,
}

fn default_tail() -> usize {
    200
}

async fn logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Response {
    if !(1..=2000).contains(&query.tail) {
        return error(
            StatusCode::BAD_REQUEST,
            "Log tail must be between 1 and 2000 lines",
        );
    }
    match state.controller.lock().await.logs(&id, query.tail).await {
        Ok(logs) => Json(json!({"logs": logs})).into_response(),
        Err(err) => error(StatusCode::BAD_REQUEST, format!("{err:#}")),
    }
}

async fn budget(
    State(state): State<AppState>,
    payload: Result<Json<Limits>, JsonRejection>,
) -> Response {
    let Json(limits) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    mutation(state.mutations.clone(), async move {
        match state.controller.lock().await.set_budget(limits).await {
            Ok(result) => Json(result).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

pub async fn serve(controller: Arc<Mutex<Controller>>, port: u16) -> Result<()> {
    #[cfg(unix)]
    let (mut interrupt, mut terminate) = (
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
    );
    serve_until(controller, port, async move {
        #[cfg(unix)]
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

pub async fn serve_until(
    controller: Arc<Mutex<Controller>>,
    port: u16,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    serve_listener(controller, listener, shutdown).await
}

async fn serve_listener(
    controller: Arc<Mutex<Controller>>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let daemon_lock = {
        let controller = controller.lock().await;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(controller.workspace.join("daemon.lock"))?
    };
    daemon_lock
        .try_lock_exclusive()
        .map_err(|error| anyhow::anyhow!("Another daemon is serving this workspace: {error}"))?;
    let port = listener.local_addr()?.port();
    let token: String = rand::random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let access = Access {
        port,
        token: token.into(),
    };
    let (workspace, config) = {
        let controller = controller.lock().await;
        (controller.workspace.clone(), controller.config_path.clone())
    };
    let mutations = Arc::new(Mutex::new(JoinSet::new()));
    let state = AppState {
        controller: controller.clone(),
        mutations: mutations.clone(),
        access: access.clone(),
        workspace,
        config,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/style.css", get(css))
        .route("/app.js", get(js))
        .route("/api/session", get(session))
        .route("/api/state", get(snapshot))
        .route("/api/health", get(health))
        .route("/api/allocation", get(allocation_plan).post(allocation))
        .route("/api/services", post(deploy))
        .route("/api/projects", post(create_project))
        .route("/api/services/{id}/logs", get(logs))
        .route("/api/services/{id}/{action}", post(action))
        .route("/api/budget", post(budget))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "Unknown endpoint") })
        .layer(DefaultBodyLimit::max(16384))
        .layer(middleware::from_fn_with_state(access, boundary))
        .with_state(state);
    let (stop, mut scheduler_stop) = tokio::sync::watch::channel(false);
    let scheduler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = scheduler_stop.changed() => break,
                _ = interval.tick() => {},
            }
            if *scheduler_stop.borrow() {
                break;
            }
            // Finish this tick's mutations before observing the next stop signal.
            if let Ok(mut controller) = controller.try_lock()
                && let Err(error) = controller.tick().await
            {
                eprintln!("Scheduler: {error:#}");
            }
        }
    });
    println!("Self-host Init dashboard: http://127.0.0.1:{port}");
    let shutdown_stop = stop.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            shutdown_stop.send_replace(true);
        })
        .await;
    stop.send_replace(true);
    let scheduler_result = scheduler.await;
    let mut mutations = mutations.lock().await;
    let mut mutation_failure = None;
    while let Some(result) = mutations.join_next().await {
        if let Err(error) = result {
            mutation_failure = Some(error);
        }
    }
    scheduler_result?;
    if let Some(error) = mutation_failure {
        return Err(error.into());
    }
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn access() -> Access {
        Access {
            port: 8787,
            token: "test-secret".into(),
        }
    }

    fn headers(host: &str, origin: Option<&str>, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            headers.insert(header::ORIGIN, origin.parse().unwrap());
        }
        if let Some(token) = token {
            headers.insert("x-selfhost-token", token.parse().unwrap());
        }
        headers
    }

    #[test]
    fn blocks_dns_rebinding_and_invalid_origins() {
        for host in [
            "evil.example:8787",
            "127.0.0.1.evil.example:8787",
            "127.0.0.1:80",
            "127.0.0.1:8787@evil.example",
        ] {
            assert!(authorized(&headers(host, None, None), &Method::GET, &access()).is_err());
        }
        for origin in [
            "https://127.0.0.1:8787",
            "http://evil.example",
            "null",
            "http://localhost:8787",
        ] {
            assert!(
                authorized(
                    &headers("127.0.0.1:8787", Some(origin), Some("test-secret")),
                    &Method::POST,
                    &access()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn accepts_browser_and_token_authenticated_cli_requests() {
        assert!(
            authorized(
                &headers("localhost:8787", None, None),
                &Method::GET,
                &access()
            )
            .is_ok()
        );
        assert!(
            authorized(
                &headers(
                    "127.0.0.1:8787",
                    Some("http://127.0.0.1:8787"),
                    Some("test-secret")
                ),
                &Method::POST,
                &access()
            )
            .is_ok()
        );
        assert!(
            authorized(
                &headers("127.0.0.1:8787", None, Some("test-secret")),
                &Method::POST,
                &access()
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_missing_wrong_duplicate_tokens_and_cross_site_requests() {
        for token in [None, Some("wrong"), Some("")] {
            assert!(
                authorized(
                    &headers("127.0.0.1:8787", None, token),
                    &Method::POST,
                    &access()
                )
                .is_err()
            );
        }
        let mut request = headers("127.0.0.1:8787", None, Some("test-secret"));
        request.append("x-selfhost-token", HeaderValue::from_static("test-secret"));
        assert!(authorized(&request, &Method::POST, &access()).is_err());
        let mut request = headers("127.0.0.1:8787", None, Some("test-secret"));
        request.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(authorized(&request, &Method::POST, &access()).is_err());
        assert!(authorized(&request, &Method::GET, &access()).is_err());
    }

    #[tokio::test]
    async fn middleware_checks_authorization_before_running_mutation() {
        let app = Router::new()
            .route("/change", post(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn_with_state(access(), boundary));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/change")
                    .header("host", "127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["x-frame-options"], "DENY");
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/change")
                    .header("host", "127.0.0.1:8787")
                    .header("x-selfhost-token", "test-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_drains_scheduler_and_http_mutations_while_retaining_daemon_lock() {
        use crate::config::Manifest;
        use std::fs;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn request(
            port: u16,
            method: &str,
            path: &str,
            token: Option<&str>,
        ) -> Result<String> {
            let mut stream =
                tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
            let auth = token
                .map(|token| format!("x-selfhost-token: {token}\r\n"))
                .unwrap_or_default();
            let body = if method == "POST" { "{}" } else { "" };
            stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
            let mut response = String::new();
            stream.read_to_string(&mut response).await?;
            Ok(response)
        }

        for (automatic, disconnect) in [(true, false), (false, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("engine.sh");
            fs::write(&script,r#"dir=$1; shift
if [ "$1" = sudo ]; then shift; shift; fi
case "$1 $2" in
  'info --format') exit 1 ;;
  'systemctl show') cat "$dir/native" ;;
  'systemctl set-property')
    shift; shift; shift
    for arg do
      case "$arg" in
        CPUQuota=*) cpu=$(awk -v n="${arg#CPUQuota=}" 'BEGIN {gsub("%","",n); printf "%.0f", n*10000}') ;;
        MemoryMax=*) memory=${arg#MemoryMax=}; memory=${memory%M} ;;
      esac
    done
    printf 'LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=123\nMemoryCurrent=1048576\nMemoryMax=%s\nCPUQuotaPerSecUSec=%sus\n' "$((memory*1048576))" "$cpu" > "$dir/native"
    printf '%s\n' mutation >> "$dir/mutations"
    touch "$dir/entered"
    while [ ! -f "$dir/release" ]; do sleep 0.01; done ;;
  *) exit 1 ;;
esac
"#).unwrap();
            fs::write(dir.path().join("native"),"LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=123\nMemoryCurrent=1048576\nMemoryMax=536870912\nCPUQuotaPerSecUSec=1s\n").unwrap();
            let prefix = json!(["/bin/sh", script, dir.path()]);
            let manifest:Manifest=serde_json::from_value(json!({"version":1,"engine":{"command":prefix},"budget":{"cpus":0.5,"memory_mb":512},"allocation":{"enabled":automatic},"services":[{"id":"native","runtime":"systemd","unit":"native.service","observe_only":false,"command_prefix":prefix,"limits":{"cpus":if automatic {1.} else {0.5},"memory_mb":512}}]})).unwrap();
            let config = dir.path().join("services.json");
            manifest.save(&config).unwrap();
            let controller = Arc::new(Mutex::new(
                Controller::new(dir.path().into(), config).unwrap(),
            ));
            // Keep the port reserved until the daemon owns its listening socket.
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let (stop, shutdown) = tokio::sync::oneshot::channel();
            let mut daemon = tokio::spawn(serve_listener(controller, listener, async {
                let _ = shutdown.await;
            }));
            let response = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if daemon.is_finished() {
                        panic!("Daemon stopped during startup: {:?}", (&mut daemon).await);
                    }
                    if let Ok(response) = request(port, "GET", "/api/session", None).await {
                        break response;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let session: Value =
                serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            let mut mutation = if automatic {
                None
            } else {
                let token = session["token"].as_str().unwrap().to_owned();
                Some(tokio::spawn(async move {
                    request(port, "POST", "/api/services/native/apply", Some(&token)).await
                }))
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                while !dir.path().join("entered").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if disconnect {
                mutation.take().unwrap().abort();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            stop.send(()).unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(75), &mut daemon)
                    .await
                    .is_err(),
                "shutdown cancelled an active mutation"
            );
            let lock = fs::OpenOptions::new()
                .write(true)
                .open(dir.path().join("daemon.lock"))
                .unwrap();
            assert!(
                lock.try_lock_exclusive().is_err(),
                "workspace lock was released before its mutation finished"
            );
            fs::write(dir.path().join("release"), "").unwrap();
            if let Some(mutation) = mutation {
                assert!(
                    mutation
                        .await
                        .unwrap()
                        .unwrap()
                        .starts_with("HTTP/1.1 200 OK")
                );
            }
            tokio::time::timeout(Duration::from_secs(5), daemon)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            lock.try_lock_exclusive().unwrap();
            FileExt::unlock(&lock).unwrap();
            let state: Value =
                serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
            if automatic {
                assert_eq!(state["allocations"]["native"]["cpus"], 0.5);
            }
            assert_eq!(state["native_units"].as_object().unwrap().len(), 1);
            assert_eq!(
                fs::read_to_string(dir.path().join("mutations"))
                    .unwrap()
                    .lines()
                    .count(),
                1
            );
            assert!(
                tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_err()
            );
        }
    }
}
