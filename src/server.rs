//! The local dashboard shares the controller used by the CLI.

use std::{future::Future, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, RawQuery, Request, State,
        rejection::{JsonRejection, QueryRejection},
    },
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
    history::HistoryQuery,
    machines,
    settings::SettingsUpdate,
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
    machines: Arc<Mutex<machines::Store>>,
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
        let tokens: Vec<_> = headers
            .get_all("x-init-token")
            .iter()
            .chain(headers.get_all("x-selfhost-token").iter())
            .collect();
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
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"));
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

async fn updater_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/updater.js"),
    )
}

async fn monitor_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/monitor.js"),
    )
}

async fn machines_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/machines.js"),
    )
}

async fn logo() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png")],
        include_bytes!("../assets/init-logo.png").as_slice(),
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
        json!({"app":"init","version":env!("CARGO_PKG_VERSION"),"workspace":state.workspace,"config":state.config}),
    )
}

async fn settings(State(state): State<AppState>) -> Response {
    let mut controller = state.controller.lock().await;
    if let Err(err) = controller.reload() {
        return error(StatusCode::CONFLICT, format!("{err:#}"));
    }
    Json(controller.settings()).into_response()
}

async fn history(
    State(state): State<AppState>,
    query: Result<Query<HistoryQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return error(StatusCode::BAD_REQUEST, rejection.body_text()),
    };
    if let Err(err) = query.validate() {
        return error(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    match state.controller.lock().await.history(query) {
        Ok(result) => Json(result).into_response(),
        Err(err) => error(StatusCode::SERVICE_UNAVAILABLE, format!("{err:#}")),
    }
}

async fn update_settings(
    State(state): State<AppState>,
    payload: Result<Json<SettingsUpdate>, JsonRejection>,
) -> Response {
    let Json(update) = match payload {
        Ok(body) => body,
        Err(rejection) => return error(rejection.status(), rejection.body_text()),
    };
    if let Err(err) = update.validate() {
        return error(StatusCode::BAD_REQUEST, format!("{err:#}"));
    }
    mutation(state.mutations.clone(), async move {
        match state.controller.lock().await.update_settings(update) {
            Ok(result) => Json(result).into_response(),
            Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
        }
    })
    .await
}

async fn list_machines(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"machines":state.machines.lock().await.list()}))
}

async fn save_machine(
    State(state): State<AppState>,
    payload: Result<Json<machines::Profile>, JsonRejection>,
) -> Response {
    let Json(profile) = match payload {
        Ok(value) => value,
        Err(error) => return self::error(error.status(), error.body_text()),
    };
    match state.machines.lock().await.upsert(profile) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(error) => self::error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn remove_machine(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.machines.lock().await.remove(&id) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(error) => self::error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    }
}

async fn test_machine(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let machine = match state.machines.lock().await.get(&id) {
        Ok(machine) => machine,
        Err(error) => return self::error(StatusCode::NOT_FOUND, error),
    };
    let result = async {
        let reply = machines::request(&machine, &Method::GET, "/api/health", &[], true).await?;
        let identity: machines::Identity = serde_json::from_value(reply.body)?;
        state
            .machines
            .lock()
            .await
            .verify(&machine.profile, identity.clone())?;
        Ok::<_, anyhow::Error>(identity)
    }
    .await;
    match result {
        Ok(identity) => Json(json!({"ok":true,"identity":identity})).into_response(),
        Err(error) => self::error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
    }
}

async fn remote_request(
    State(state): State<AppState>,
    Path((id, path)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let machine = match state.machines.lock().await.get(&id) {
        Ok(machine) => machine,
        Err(error) => return self::error(StatusCode::NOT_FOUND, error),
    };
    if headers
        .get("x-init-machine-revision")
        .and_then(|value| value.to_str().ok())
        != Some(machine.revision.as_str())
    {
        return error(
            StatusCode::CONFLICT,
            "This machine connection changed. Open it again from Machines before continuing",
        );
    }
    let path = format!(
        "/api/{path}{}",
        query.map(|q| format!("?{q}")).unwrap_or_default()
    );
    let write = method == Method::POST;
    let operation = async move {
        match machines::request(&machine, &method, &path, &body, false).await {
            Ok(reply) => (
                StatusCode::from_u16(reply.status).unwrap_or(StatusCode::BAD_GATEWAY),
                Json(reply.body),
            )
                .into_response(),
            Err(error) => self::error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
        }
    };
    // Keep accepted remote writes alive through client disconnect and local shutdown.
    if write {
        mutation(state.mutations.clone(), operation).await
    } else {
        operation.await
    }
}

async fn tailscale() -> Response {
    match machines::tailscale_status().await {
        Ok(value) => Json(value).into_response(),
        Err(error) => self::error(StatusCode::BAD_GATEWAY, format!("{error:#}")),
    }
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
    let machines = Arc::new(Mutex::new(machines::Store::load(&workspace)?));
    let state = AppState {
        controller: controller.clone(),
        mutations: mutations.clone(),
        access: access.clone(),
        workspace,
        config,
        machines,
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/style.css", get(css))
        .route("/app.js", get(js))
        .route("/updater.js", get(updater_js))
        .route("/monitor.js", get(monitor_js))
        .route("/machines.js", get(machines_js))
        .route("/init-logo.png", get(logo))
        .route("/api/session", get(session))
        .route("/api/state", get(snapshot))
        .route("/api/health", get(health))
        .route("/api/settings", get(settings).post(update_settings))
        .route("/api/history", get(history))
        .route("/api/machines", get(list_machines).post(save_machine))
        .route("/api/machines/tailscale", get(tailscale))
        .route("/api/machines/{id}/test", post(test_machine))
        .route("/api/machines/{id}/remove", post(remove_machine))
        .route(
            "/api/machines/{id}/proxy/{*path}",
            get(remote_request).post(remote_request),
        )
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
    let history_controller = controller.clone();
    let mut history_stop = scheduler_stop.clone();
    let history_sampler = tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(2),
            Duration::from_secs(15),
        );
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = history_stop.changed() => break,
                _ = interval.tick() => {},
            }
            // Queue behind an active mutation rather than starving history on aligned ticks.
            let mut controller = tokio::select! {
                biased;
                _ = history_stop.changed() => break,
                controller = history_controller.lock() => controller,
            };
            if let Err(error) = controller.capture_history().await {
                eprintln!("Utilization history: {error:#}");
            }
        }
    });
    let scheduler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = scheduler_stop.changed() => break,
                _ = interval.tick() => {},
            }
            // Queue fairly with telemetry and requests so slow snapshots cannot starve jobs.
            let mut controller = tokio::select! {
                biased;
                _ = scheduler_stop.changed() => break,
                controller = controller.lock() => controller,
            };
            // Finish this tick's mutations before observing the next stop signal.
            if let Err(error) = controller.tick().await {
                eprintln!("Scheduler: {error:#}");
            }
        }
    });
    println!("Init dashboard: http://127.0.0.1:{port}");
    let shutdown_stop = stop.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            shutdown_stop.send_replace(true);
        })
        .await;
    stop.send_replace(true);
    let scheduler_result = scheduler.await;
    let history_result = history_sampler.await;
    let mut mutations = mutations.lock().await;
    let mut mutation_failure = None;
    while let Some(result) = mutations.join_next().await {
        if let Err(error) = result {
            mutation_failure = Some(error);
        }
    }
    // Release explicitly: a concurrent fork can briefly retain a copied file descriptor.
    FileExt::unlock(&daemon_lock)?;
    scheduler_result?;
    history_result?;
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
            headers.insert("x-init-token", token.parse().unwrap());
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
        request.append("x-init-token", HeaderValue::from_static("test-secret"));
        assert!(authorized(&request, &Method::POST, &access()).is_err());
        let mut request = headers("127.0.0.1:8787", None, Some("test-secret"));
        request.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(authorized(&request, &Method::POST, &access()).is_err());
        assert!(authorized(&request, &Method::GET, &access()).is_err());
    }

    #[test]
    fn accepts_one_token_spelling_and_rejects_mixed_or_duplicate_credentials() {
        for name in ["x-init-token", "x-selfhost-token"] {
            let mut request = headers("127.0.0.1:8787", None, None);
            request.insert(name, HeaderValue::from_static("test-secret"));
            assert!(authorized(&request, &Method::POST, &access()).is_ok());
            request.append(name, HeaderValue::from_static("test-secret"));
            assert!(authorized(&request, &Method::POST, &access()).is_err());
        }
        for legacy in ["test-secret", "wrong"] {
            let mut request = headers("127.0.0.1:8787", None, Some("test-secret"));
            request.insert("x-selfhost-token", legacy.parse().unwrap());
            assert!(authorized(&request, &Method::POST, &access()).is_err());
        }
    }

    #[tokio::test]
    async fn updater_asset_allows_native_ipc_without_broad_network_access() {
        let app = Router::new()
            .route("/updater.js", get(updater_js))
            .layer(middleware::from_fn_with_state(access(), boundary));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/updater.js")
                    .header("host", "127.0.0.1:8787")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        let policy = response.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert_eq!(
            policy
                .split(';')
                .map(str::trim)
                .find(|directive| directive.starts_with("connect-src ")),
            Some("connect-src 'self' ipc: http://ipc.localhost")
        );
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("window.__INIT_UPDATER__")
        );
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

    #[tokio::test]
    async fn settings_and_history_routes_validate_and_preserve_unrelated_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.yaml");
        crate::config::Manifest::new(vec!["missing-engine".into()])
            .save(&config)
            .unwrap();
        let controller = Arc::new(Mutex::new(
            Controller::new(dir.path().into(), config.clone()).unwrap(),
        ));
        let state = AppState {
            controller: controller.clone(),
            mutations: Arc::new(Mutex::new(JoinSet::new())),
            access: access(),
            workspace: dir.path().into(),
            config: config.clone(),
            machines: Arc::new(Mutex::new(machines::Store::load(dir.path()).unwrap())),
        };
        let app = Router::new()
            .route("/api/settings", get(settings).post(update_settings))
            .route("/api/history", get(history))
            .layer(middleware::from_fn_with_state(access(), boundary))
            .with_state(state);
        async fn request(
            app: &Router,
            method: Method,
            path: &str,
            body: &str,
            token: bool,
        ) -> (StatusCode, Value) {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "127.0.0.1:8787")
                .header("content-type", "application/json");
            if token {
                request = request.header("x-selfhost-token", "test-secret");
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::from(body.to_owned())).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            (status, serde_json::from_slice(&bytes).unwrap())
        }
        let before = std::fs::read(&config).unwrap();
        let (status, settings) = request(&app, Method::GET, "/api/settings", "", false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(settings["controller"]["max_parallel_jobs"], 1);
        assert_eq!(settings["history"]["retention_seconds"], 86400);
        assert_eq!(
            settings["paths"]["config"],
            config.to_string_lossy().as_ref()
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                "/api/settings",
                r#"{"max_parallel_jobs":2}"#,
                false
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &app,
                Method::POST,
                "/api/settings",
                r#"{"max_parallel_jobs":0}"#,
                true
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert!(
            request(
                &app,
                Method::POST,
                "/api/settings",
                r#"{"max_parallel_jobs":2,"engine":["sh"]}"#,
                true
            )
            .await
            .0
            .is_client_error()
        );
        assert_eq!(std::fs::read(&config).unwrap(), before);
        assert_eq!(
            request(
                &app,
                Method::GET,
                "/api/history?window_seconds=0",
                "",
                false
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (status, empty) = request(
            &app,
            Method::GET,
            "/api/history?window_seconds=60&max_points=2",
            "",
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            empty["points"]
                .as_array()
                .unwrap()
                .iter()
                .all(|point| point["cpu_percent"].is_null() && point["memory_mb"].is_null())
        );
        assert!(!dir.path().join("history.sqlite3").exists());
        let (status, result) = request(
            &app,
            Method::POST,
            "/api/settings",
            r#"{"max_parallel_jobs":2}"#,
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["controller"]["max_parallel_jobs"], 2);
        assert_eq!(
            crate::config::Manifest::load(&config)
                .unwrap()
                .budget
                .max_parallel_jobs,
            2
        );
        std::fs::write(dir.path().join("history.sqlite3"), "unrelated data").unwrap();
        assert_eq!(
            request(
                &app,
                Method::GET,
                "/api/history?window_seconds=60",
                "",
                false
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("history.sqlite3")).unwrap(),
            "unrelated data"
        );
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
