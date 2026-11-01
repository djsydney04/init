use crate::{config::Service, runtime::run};
use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};

fn executable(name: &str) -> Option<String> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
}

fn command(base: &[String], args: &[&str]) -> Vec<String> {
    base.iter()
        .cloned()
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .collect()
}

/// Prefer a working local engine, then a Docker engine in an existing Lima VM.
/// This never starts a VM or changes the user's selected Docker context.
pub async fn engine_command() -> Vec<String> {
    if let Some(docker) = executable("docker") {
        let candidate = vec![docker];
        if run(&command(
            &candidate,
            &["info", "--format", "{{json .ServerVersion}}"],
        ))
        .await
        .is_ok()
        {
            return candidate;
        }
    }
    let lima = executable("limactl").or_else(|| {
        let path = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin/limactl");
        path.is_file().then(|| path.to_string_lossy().into_owned())
    });
    if let Some(lima) = lima
        && let Ok(output) = run(&[lima.clone(), "list".into(), "--json".into()]).await
    {
        for instance in json_records(&output) {
            if instance["status"].as_str() != Some("Running") {
                continue;
            }
            let Some(name) = instance["name"].as_str() else {
                continue;
            };
            if name.starts_with('-') || name.is_empty() {
                continue;
            }
            let candidate = vec![lima.clone(), "shell".into(), name.into(), "docker".into()];
            if run(&command(
                &candidate,
                &["info", "--format", "{{json .ServerVersion}}"],
            ))
            .await
            .is_ok()
            {
                return candidate;
            }
        }
    }
    vec!["docker".into()]
}

fn json_records(output: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(output) {
        Ok(Value::Array(records)) => records,
        Ok(record @ Value::Object(_)) => vec![record],
        _ => output
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect(),
    }
}

fn relevant(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('_', "-");
    [
        "actions.runner",
        "renovate",
        "repo-maintainer",
        "codex-review",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn service_id(runtime: &str, name: &str, seen: &mut HashSet<String>) -> String {
    let mut id = runtime.to_owned();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            id.push(character.to_ascii_lowercase());
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    // Labels are often longer than manifest IDs; keep a stable suffix to avoid collisions.
    if id.len() > 48 || seen.contains(&id) {
        let hash = name.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        id.truncate(39);
        id = format!("{}-{:08x}", id.trim_end_matches('-'), hash as u32);
    }
    id = id.trim_end_matches('-').to_owned();
    seen.insert(id.clone());
    id
}

fn observer(runtime: &str, name: &str, seen: &mut HashSet<String>) -> Value {
    json!({
        "id": service_id(&format!("{runtime}-"), name, seen),
        "name": name,
        "runtime": runtime,
        "kind": "daemon",
        "enabled": true,
        "observe_only": true,
        "description": "Discovered existing service; lifecycle is managed by its current supervisor."
    })
}

/// Existing services are always imported as observers. Credentials, environments,
/// process arguments, and container command lines are never inventoried.
pub async fn discover(engine: &[String]) -> Result<Vec<Service>> {
    let mut services = Vec::new();
    let mut seen = HashSet::new();
    discover_launchd(&mut services, &mut seen).await?;
    if let Ok(output) = run(&command(
        engine,
        &[
            "ps",
            "-a",
            "--format",
            "{\"name\":{{json .Names}},\"image\":{{json .Image}}}",
        ],
    ))
    .await
    {
        for container in json_records(&output) {
            let Some(name) = container["name"].as_str().filter(|name| !name.is_empty()) else {
                continue;
            };
            let mut item = observer("docker", name, &mut seen);
            item["container"] = json!(name);
            if let Some(image) = container["image"].as_str() {
                item["image"] = json!(image);
            }
            services.push(serde_json::from_value(item)?);
        }
    }
    let prefix = systemd_prefix(engine);
    if let Some(prefix) = prefix
        && let Ok(output) = run(&command(
            &prefix,
            &[
                "systemctl",
                "list-units",
                "--type=service",
                "--all",
                "--no-legend",
                "--no-pager",
                "--plain",
            ],
        ))
        .await
    {
        for unit in output
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|unit| unit.ends_with(".service") && relevant(unit))
        {
            let mut item = observer("systemd", unit, &mut seen);
            item["unit"] = json!(unit);
            item["command_prefix"] = json!(prefix);
            services.push(serde_json::from_value(item)?);
        }
    }
    Ok(services)
}

fn systemd_prefix(engine: &[String]) -> Option<Vec<String>> {
    if engine.first().is_some_and(|part| {
        PathBuf::from(part)
            .file_name()
            .is_some_and(|name| name == "limactl")
    }) && engine.get(1).is_some_and(|part| part == "shell")
        && engine.len() == 4
        && engine[3] == "docker"
    {
        Some(engine[..3].to_vec())
    } else if cfg!(target_os = "linux") {
        Some(Vec::new())
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
async fn discover_launchd(services: &mut Vec<Service>, seen: &mut HashSet<String>) -> Result<()> {
    let mut directories = vec![
        PathBuf::from("/Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchDaemons"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        directories.push(PathBuf::from(home).join("Library/LaunchAgents"));
    }
    let mut labels = HashSet::new();
    for directory in directories {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "plist")
            {
                continue;
            }
            let Ok(output) = run(&[
                "/usr/bin/plutil".into(),
                "-convert".into(),
                "json".into(),
                "-o".into(),
                "-".into(),
                path.to_string_lossy().into_owned(),
            ])
            .await
            else {
                continue;
            };
            let Ok(plist) = serde_json::from_str::<Value>(&output) else {
                continue;
            };
            let Some(label) = plist["Label"].as_str().filter(|label| relevant(label)) else {
                continue;
            };
            if !labels.insert(label.to_owned()) {
                continue;
            }
            let mut item = observer("launchd", label, seen);
            item["label"] = json!(label);
            let logs: Vec<String> = ["StandardOutPath", "StandardErrorPath"]
                .iter()
                .filter_map(|key| {
                    let log = plist[*key].as_str().filter(|path| !path.is_empty())?;
                    let path = PathBuf::from(log);
                    let path = if path.is_relative() {
                        PathBuf::from(plist["WorkingDirectory"].as_str()?).join(path)
                    } else {
                        path
                    };
                    Some(path.to_string_lossy().into_owned())
                })
                .collect();
            item["log_paths"] = json!(logs);
            services.push(serde_json::from_value(item)?);
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn discover_launchd(_: &mut Vec<Service>, _: &mut HashSet<String>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lima_json_and_service_ids_are_stable() {
        assert_eq!(
            json_records("{\"name\":\"a\"}\n{\"name\":\"b\"}\n").len(),
            2
        );
        assert_eq!(json_records("[{\"name\":\"a\"}]").len(), 1);
        let long = "actions.runner.some-owner-and-repository.some-rather-long-linux-runner.service";
        let first = service_id("systemd-", long, &mut HashSet::new());
        assert!(first.len() <= 48);
        assert_eq!(first, service_id("systemd-", long, &mut HashSet::new()));
        assert!(relevant("com.repo_maintainer"));
        assert!(!relevant("com.apple.finder"));
    }

    #[test]
    fn only_lima_engine_receives_a_remote_systemd_prefix() {
        let engine = vec![
            "/usr/local/bin/limactl".into(),
            "shell".into(),
            "workers".into(),
            "docker".into(),
        ];
        assert_eq!(systemd_prefix(&engine), Some(engine[..3].to_vec()));
    }
}
