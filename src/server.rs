//! The local dashboard shares the controller used by the CLI.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{config::Limits, control::Controller};

#[derive(Clone)]
struct Access {
    port: u16,
    token: Arc<str>,
}

#[derive(Clone)]
struct AppState {
    controller: Arc<Mutex<Controller>>,
    access: Access,
}

fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({"error": message.to_string()}))).into_response()
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
    match state.controller.lock().await.set_budget(limits).await {
        Ok(result) => Json(result).into_response(),
        Err(err) => error(StatusCode::CONFLICT, format!("{err:#}")),
    }
}

pub async fn serve(controller: Arc<Mutex<Controller>>, port: u16) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let port = listener.local_addr()?.port();
    let token: String = rand::random::<[u8; 32]>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let access = Access {
        port,
        token: token.into(),
    };
    let state = AppState {
        controller: controller.clone(),
        access: access.clone(),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/style.css", get(css))
        .route("/app.js", get(js))
        .route("/api/session", get(session))
        .route("/api/state", get(snapshot))
        .route("/api/services/{id}/logs", get(logs))
        .route("/api/services/{id}/{action}", post(action))
        .route("/api/budget", post(budget))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "Unknown endpoint") })
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(access, boundary))
        .with_state(state);
    let scheduler = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Ok(mut controller) = controller.try_lock()
                && let Err(error) = controller.tick().await
            {
                eprintln!("Scheduler: {error:#}");
            }
        }
    });
    println!("Self-host Init dashboard: http://127.0.0.1:{port}");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    scheduler.abort();
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
}
