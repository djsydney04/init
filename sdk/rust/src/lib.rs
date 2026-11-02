//! Async client for a local Self-host Init daemon. See the package README.
//!
//! Requests use JSON values so new daemon fields do not require regenerating the SDK.
//! Mutations fetch a fresh session token and are never retried. After a timeout,
//! inspect state before retrying: the daemon may still finish the operation.

use reqwest::{Client as HttpClient, Method};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

#[derive(Debug)]
pub enum Error {
    Transport(reqwest::Error),
    Api { status: u16, message: String },
    InvalidInput(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(f),
            Self::Api { status, message } => write!(f, "HTTP {status}: {message}"),
            Self::InvalidInput(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error)
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    http: HttpClient,
    base: String,
}

impl Client {
    /// Connect to numeric loopback. Timeout applies to each HTTP request.
    pub fn new(port: u16, timeout: Duration) -> Result<Self, Error> {
        if port == 0 || timeout.is_zero() {
            return Err(Error::InvalidInput("port and timeout must be positive"));
        }
        Ok(Self {
            http: HttpClient::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(timeout)
                .build()?,
            base: format!("http://127.0.0.1:{port}"),
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        token: Option<&str>,
    ) -> Result<Value, Error> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(token) = token {
            request = request.header("X-Selfhost-Token", token);
        }
        let response = request.send().await?;
        let status = response.status();
        let data: Value = response.json().await?;
        if !status.is_success() {
            return Err(Error::Api {
                status: status.as_u16(),
                message: data["error"]
                    .as_str()
                    .unwrap_or("Daemon request failed")
                    .to_owned(),
            });
        }
        if !data.is_object() {
            return Err(Error::Api {
                status: status.as_u16(),
                message: "Expected a JSON object".into(),
            });
        }
        Ok(data)
    }
    async fn get(&self, path: &str) -> Result<Value, Error> {
        self.send(Method::GET, path, None, None).await
    }
    async fn post(&self, path: &str, body: &Value) -> Result<Value, Error> {
        let session = self.get("/api/session").await?;
        let token = session["token"].as_str().ok_or_else(|| Error::Api {
            status: 200,
            message: "Missing session token".into(),
        })?;
        self.send(Method::POST, path, Some(body), Some(token)).await
    }
    fn service_path(id: &str) -> Result<String, Error> {
        if id.is_empty()
            || id.len() > 48
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(Error::InvalidInput("Invalid service ID"));
        }
        Ok(format!("/api/services/{id}"))
    }

    /// Daemon identity, version, workspace and configuration paths.
    pub async fn health(&self) -> Result<Value, Error> {
        self.get("/api/health").await
    }
    /// Current services, telemetry, projects, budgets and events.
    pub async fn state(&self) -> Result<Value, Error> {
        self.get("/api/state").await
    }
    /// Read 1–2000 recent log lines; this is not a live stream.
    pub async fn logs(&self, id: &str, tail: u16) -> Result<String, Error> {
        if !(1..=2000).contains(&tail) {
            return Err(Error::InvalidInput("tail must be between 1 and 2000"));
        }
        let data = self
            .get(&format!("{}/logs?tail={tail}", Self::service_path(id)?))
            .await?;
        data["logs"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Api {
                status: 200,
                message: "Missing log text".into(),
            })
    }
    /// Start, stop, restart, run a job, or apply optional service limits.
    pub async fn action(
        &self,
        id: &str,
        action: &str,
        limits: Option<&Value>,
    ) -> Result<Value, Error> {
        if !["start", "stop", "restart", "run", "apply"].contains(&action) {
            return Err(Error::InvalidInput("Unknown service action"));
        }
        if limits.is_some() && action != "apply" {
            return Err(Error::InvalidInput("limits are only supported by apply"));
        }
        let body = limits.map_or_else(|| json!({}), |limits| json!({"limits": limits}));
        self.post(&format!("{}/{action}", Self::service_path(id)?), &body)
            .await
    }
    /// Preview a deployment without changing anything.
    pub async fn preview_deployment(&self, service: &Value) -> Result<Value, Error> {
        self.post("/api/services", &json!({"service":service,"apply":false}))
            .await
    }
    /// Deploy a service, after reviewing preview_deployment's result.
    pub async fn deploy(&self, service: &Value) -> Result<Value, Error> {
        self.post("/api/services", &json!({"service":service,"apply":true}))
            .await
    }
    /// Preview a public GitHub repository build; apply explicitly after review.
    pub async fn deploy_github(
        &self,
        service: &Value,
        source: &Value,
        apply: bool,
    ) -> Result<Value, Error> {
        self.post(
            "/api/services",
            &json!({"service":service,"github":source,"apply":apply}),
        )
        .await
    }
    /// Create a workspace group with id, name, and optional description.
    pub async fn create_project(&self, project: &Value) -> Result<Value, Error> {
        self.post("/api/projects", project).await
    }
    /// Apply the global CPU/memory budget.
    pub async fn set_budget(&self, limits: &Value) -> Result<Value, Error> {
        self.post("/api/budget", limits).await
    }
    pub async fn allocation_plan(&self) -> Result<Value, Error> {
        self.get("/api/allocation").await
    }
    /// Preview optional enabled/budget settings. The apply field is forced false.
    pub async fn preview_allocation(&self, settings: &Value) -> Result<Value, Error> {
        self.allocation(settings, false).await
    }
    /// Commit optional enabled/budget settings. The apply field is forced true.
    pub async fn configure_allocation(&self, settings: &Value) -> Result<Value, Error> {
        self.allocation(settings, true).await
    }
    async fn allocation(&self, settings: &Value, apply: bool) -> Result<Value, Error> {
        let mut body = settings
            .as_object()
            .cloned()
            .ok_or(Error::InvalidInput("settings must be an object"))?;
        body.insert("apply".into(), json!(apply));
        self.post("/api/allocation", &Value::Object(body)).await
    }
}
