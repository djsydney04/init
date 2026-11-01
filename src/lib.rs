//! Shared service control, allocation and runtime adapters for all interfaces.
pub mod allocation;
pub mod config;
pub mod control;
pub mod discovery;
pub mod manifest_io;
pub mod runtime;
pub mod server;

pub fn default_workspace() -> anyhow::Result<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        let path = std::path::PathBuf::from(path);
        anyhow::ensure!(
            path.is_absolute(),
            "XDG_DATA_HOME must be an absolute path, or pass --workspace"
        );
        return Ok(path.join("self-host-init"));
    }
    Ok(std::path::PathBuf::from(
        std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME is not set; supply an explicit workspace"))?,
    )
    .join(".local/share/self-host-init"))
}

pub fn default_config_path(workspace: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let files: Vec<_> = ["services.yaml", "services.yml", "services.json"]
        .into_iter()
        .map(|name| workspace.join(name))
        .filter(|path| path.exists())
        .collect();
    anyhow::ensure!(
        files.len() <= 1,
        "Multiple manifests found in {}; pass --config explicitly",
        workspace.display()
    );
    Ok(files
        .into_iter()
        .next()
        .unwrap_or_else(|| workspace.join("services.yaml")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_config_preserves_json_and_rejects_ambiguity() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            default_config_path(dir.path()).unwrap(),
            dir.path().join("services.yaml")
        );
        std::fs::write(dir.path().join("services.json"), b"{}").unwrap();
        assert_eq!(
            default_config_path(dir.path()).unwrap(),
            dir.path().join("services.json")
        );
        std::fs::write(dir.path().join("services.yaml"), b"{}").unwrap();
        assert!(default_config_path(dir.path()).is_err());
    }
}
