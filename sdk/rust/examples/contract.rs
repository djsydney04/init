use self_host_init_sdk::{Client, Error};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::var("SELFHOST_TEST_PORT")?.parse()?;
    let client = Client::new(port, Duration::from_secs(30))?;
    assert_eq!(client.health().await?["app"], "self-host-init");
    assert_eq!(client.state().await?["services"][0]["id"], "worker");
    assert_eq!(client.logs("worker", 12).await?, "hello\n");
    assert_eq!(client.allocation_plan().await?, json!({"services": []}));
    for action in ["start", "stop", "restart", "run"] {
        assert_eq!(client.action("worker", action, None).await?["ok"], true);
    }
    let limits = json!({"cpus":1,"memory_mb":256});
    assert_eq!(
        client.action("worker", "apply", Some(&limits)).await?["ok"],
        true
    );
    let service = json!({"id":"worker","image":"example/worker:1","limits":limits});
    assert_eq!(client.preview_deployment(&service).await?["applied"], false);
    assert_eq!(client.deploy(&service).await?["applied"], true);
    assert_eq!(
        client
            .create_project(&json!({"id":"dev","name":"Development"}))
            .await?["ok"],
        true
    );
    assert_eq!(client.set_budget(&limits).await?["ok"], true);
    assert_eq!(
        client.preview_allocation(&json!({"enabled":true})).await?["applied"],
        false
    );
    assert_eq!(
        client
            .configure_allocation(&json!({"enabled":true}))
            .await?["applied"],
        true
    );
    for (id, expected) in [("blocked", 409), ("expired", 403)] {
        assert!(
            matches!(client.action(id, "start", None).await, Err(Error::Api {status, ..}) if status == expected)
        );
    }
    assert!(Client::new(0, Duration::from_secs(1)).is_err());
    assert!(Client::new(port, Duration::ZERO).is_err());
    assert!(client.logs("worker", 0).await.is_err());
    assert!(client.logs("../worker", 12).await.is_err());
    assert!(client.action("worker", "destroy", None).await.is_err());
    assert!(
        client
            .action("worker", "start", Some(&limits))
            .await
            .is_err()
    );
    let short = Client::new(port, Duration::from_millis(50))?;
    assert!(
        matches!(short.action("slow", "start", None).await, Err(Error::Transport(error)) if error.is_timeout())
    );
    Ok(())
}
