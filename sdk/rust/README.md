# Self-host Init — Rust SDK

Rust 1.89+, async HTTP via reqwest, JSON via serde_json. Add to your application's `Cargo.toml` using your actual checkout path:

```toml
[dependencies]
self-host-init-sdk = { path = "/absolute/path/to/self-host-init/sdk/rust" }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Start `selfhost serve`, then:

```rust,no_run
use self_host_init_sdk::{Client, Error};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(8787, Duration::from_secs(30))?;
    println!("{}", client.health().await?);
    println!("{}", client.state().await?["services"]);
    let service = json!({
        "id": "worker",
        "image": "your-org/worker:1.0",
        "limits": {"cpus": 1, "memory_mb": 256}
    });
    println!("{}", client.preview_deployment(&service).await?);
    // After reviewing the plan:
    // client.deploy(&service).await?;
    // println!("{}", client.logs("worker", 100).await?);
    // client.action("worker", "stop", None).await?;
    match client.action("missing-service", "start", None).await {
        Err(Error::Api { status, message }) => eprintln!("{status}: {message}"),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    Ok(())
}
```

Responses are `serde_json::Value`; logs return `String`. Preview and commit are separate named methods in Rust. Mutations retrieve session tokens automatically and are never retried. After a timeout, inspect state before repeating an operation. Increase the timeout for image pulls. The client uses numeric loopback, disables proxies, and does not follow redirects.

[Complete operations, request fields, and connection behavior](https://github.com/djsydney04/self-host-init/blob/main/sdk/README.md).
