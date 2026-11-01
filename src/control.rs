use crate::{
    allocation::{self, Allocation, AllocationInput},
    config::{self, Limits, Manifest, Service},
    runtime::{self, Docker},
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn native_key(s: &Service) -> String {
    serde_json::to_string(&(&s.command_prefix, &s.scope, &s.unit)).expect("string tuple serializes")
}

fn active_key(runtime: &str, id: &str) -> String {
    format!("{runtime}:{id}")
}

async fn host_capacity() -> Value {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).ok();
    #[cfg(target_os = "macos")]
    let memory = runtime::run(&["/usr/sbin/sysctl".into(), "-n".into(), "hw.memsize".into()])
        .await
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|bytes| bytes / 1048576);
    #[cfg(not(target_os = "macos"))]
    let memory = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")
                    .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
            })
        })
        .map(|kib| kib / 1024);
    json!({"cpus":cpus,"memory_mb":memory})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    fn fixture(
        services: Value,
        running: Vec<(&str, &str, f64, u64)>,
    ) -> (tempfile::TempDir, Controller) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        fs::write(
            &script,
            r#"dir=$1; shift
case "$1 $2" in
  'info --format') printf '%s\n' '{"NCPU":8,"MemTotal":8589934592}' ;;
  'ps -a') cat "$dir/inventory" ;;
  'container inspect') for name do :; done; cat "$dir/$name.json" ;;
  'systemctl show') cat "$dir/native" ;;
  *) printf '%s\n' 'unexpected mutation' >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        let manifest: Manifest = serde_json::from_value(json!({
            "version":1,"engine":{"command":["/bin/sh",script,dir.path()]},
            "budget":{"cpus":2,"memory_mb":768,"max_parallel_jobs":1},"services":services
        }))
        .unwrap();
        let config = dir.path().join("services.json");
        manifest.save(&config).unwrap();
        let controller = Controller::new(dir.path().to_owned(), config).unwrap();
        let mut inventory = String::new();
        for (name, id, cpus, memory) in running {
            inventory.push_str(&json!({"Names":name}).to_string());
            inventory.push('\n');
            let inspection = json!({"Config":{"Labels":{"io.self-host-init.workspace":controller.docker.owner,"io.self-host-init.service":id}},"State":{"Running":true},"HostConfig":{"NanoCpus":cpus*1e9,"Memory":memory*1048576}});
            fs::write(
                dir.path().join(format!("{name}.json")),
                json!([inspection]).to_string(),
            )
            .unwrap();
        }
        fs::write(dir.path().join("inventory"), inventory).unwrap();
        (dir, controller)
    }

    fn services() -> Value {
        json!([
            {"id":"web","runtime":"docker","observe_only":false,"image":"example","limits":{"cpus":1,"memory_mb":256}},
            {"id":"job-a","runtime":"docker","kind":"job","interval_seconds":60,"observe_only":false,"image":"example","limits":{"cpus":1,"memory_mb":256}},
            {"id":"job-b","runtime":"docker","kind":"job","interval_seconds":60,"observe_only":false,"image":"example","limits":{"cpus":1,"memory_mb":256}}
        ])
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restart_and_recreate_cannot_bypass_job_admission() {
        for action in ["run", "start", "restart", "recreate"] {
            let (_dir, mut c) = fixture(services(), vec![("job-a", "job-a", 1., 256)]);
            let error = c
                .action("job-b", action, None)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("Maximum parallel"), "{action}: {error}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removed_running_services_still_consume_capacity() {
        let (_dir, c) = fixture(json!([]), vec![("orphan", "removed", 1.5, 512)]);
        let mut m = c.manifest.clone();
        m.budget.cpus = 1.;
        assert!(
            c.admission(&m, None, false)
                .await
                .unwrap_err()
                .to_string()
                .contains("capacity exhausted")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_scaling_counts_other_running_jobs_and_preserves_config_on_failure() {
        let (_dir, mut c) = fixture(services(), vec![("job-a", "job-a", 1., 512)]);
        // Configured job cap is 256 MiB, but actual cap was changed externally.
        let error = c
            .action(
                "web",
                "apply",
                Some(Limits {
                    cpus: 1.,
                    memory_mb: 512,
                }),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("capacity exhausted"), "{error}");
        assert_eq!(
            Manifest::load(&c.config_path).unwrap().services[0]
                .limits
                .as_ref()
                .unwrap()
                .memory_mb,
            256
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_controller_rejected_and_observer_immutable() {
        let (dir, mut c) = fixture(
            json!([{"id":"external","runtime":"docker","container":"external","observe_only":true}]),
            vec![],
        );
        let guard = c.lock().unwrap();
        let mut second = Controller::new(dir.path().into(), c.config_path.clone()).unwrap();
        assert!(
            second
                .action("external", "stop", None)
                .await
                .unwrap_err()
                .to_string()
                .contains("Another controller")
        );
        FileExt::unlock(&guard).unwrap();
        drop(guard);
        let error = c.action("external", "stop", None).await.unwrap_err();
        assert!(error.to_string().contains("observe-only"), "{error:#}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_units_keep_actual_quota_when_disabled_removed_or_retargeted() {
        let (dir, mut c) = fixture(json!([]), vec![]);
        fs::write(dir.path().join("native"),"LoadState=loaded\nActiveState=deactivating\nSubState=stop-sigterm\nMainPID=123\nMemoryCurrent=10000000\nMemoryMax=536870912\nCPUQuotaPerSecUSec=1500ms\n").unwrap();
        let native:Service=serde_json::from_value(json!({"id":"native","runtime":"systemd","unit":"old.service","enabled":false,"observe_only":false,"command_prefix":c.manifest.engine.command,"limits":{"cpus":0.5,"memory_mb":128}})).unwrap();
        let mut m = c.manifest.clone();
        m.budget.cpus = 1.;
        m.services.push(native.clone());
        assert!(
            c.admission(&m, None, false).await.is_err(),
            "disabled running native cap must still count"
        );
        c.state
            .native_units
            .insert(native_key(&native), native.clone());
        m.services.clear();
        assert!(
            c.admission(&m, None, false).await.is_err(),
            "removed native cap must still count"
        );
        let mut retarget = native;
        retarget.unit = Some("new.service".into());
        retarget.enabled = true;
        m.services.push(retarget.clone());
        m.budget.cpus = 1.75;
        m.budget.memory_mb = 768;
        assert!(
            c.admission(&m, Some(&retarget), false).await.is_err(),
            "retargeting must not replace the old unit's actual cap"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn changing_runtime_does_not_replace_an_orphan_containers_actual_quota() {
        let (dir, mut c) = fixture(json!([]), vec![("old-container", "native", 1.5, 512)]);
        fs::write(dir.path().join("native"),"LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\nMemoryCurrent=0\nMemoryMax=134217728\nCPUQuotaPerSecUSec=500ms\n").unwrap();
        let native:Service=serde_json::from_value(json!({"id":"native","runtime":"systemd","unit":"new.service","enabled":true,"observe_only":false,"command_prefix":c.manifest.engine.command,"limits":{"cpus":0.5,"memory_mb":128}})).unwrap();
        c.manifest.budget.cpus = 1.5;
        c.manifest.services.push(native.clone());
        assert!(c.admission(&c.manifest, Some(&native), true).await.is_err());
    }

    #[cfg(unix)]
    fn allocator_fixture() -> (tempfile::TempDir, Controller) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("native.sh");
        fs::write(&script,r#"dir=$1; shift
if [ "$1" = sudo ]; then shift; shift; fi
case "$1 $2" in
  'info --format') exit 1 ;;
  'systemctl show') cat "$dir/$3" ;;
  'systemctl set-property')
    unit=$3; shift; shift; shift
    cpu=0; memory=0
    for arg do
      case "$arg" in
        CPUQuota=*) cpu=$(awk -v n="${arg#CPUQuota=}" 'BEGIN {gsub("%","",n); printf "%.0f", n*10000}') ;;
        MemoryMax=*) memory=${arg#MemoryMax=}; memory=${memory%M} ;;
      esac
    done
    printf '%s %s %s\n' "$unit" "$cpu" "$memory" >> "$dir/mutations"
    if [ -f "$dir/fail" ] && [ "$unit" = a.service ]; then exit 7; fi
    printf 'LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=123\nMemoryCurrent=1048576\nMemoryMax=%s\nCPUQuotaPerSecUSec=%sus\n' "$((memory*1048576))" "$cpu" > "$dir/$unit"
    if [ -f "$dir/fail-after" ] && [ "$unit" = a.service ]; then rm "$dir/fail-after"; exit 7; fi ;;
  *) exit 1 ;;
esac
"#).unwrap();
        let services:Vec<_>=["a","b"].into_iter().map(|id|json!({"id":id,"runtime":"systemd","unit":format!("{id}.service"),"observe_only":false,"command_prefix":["/bin/sh",script,dir.path()],"limits":{"cpus":1,"memory_mb":512}})).collect();
        let manifest:Manifest=serde_json::from_value(json!({"version":1,"engine":{"command":["/bin/sh",script,dir.path()]},"budget":{"cpus":2,"memory_mb":1024},"services":services})).unwrap();
        let config = dir.path().join("services.json");
        manifest.save(&config).unwrap();
        for id in ["a", "b"] {
            fs::write(dir.path().join(format!("{id}.service")),"LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=123\nMemoryCurrent=1048576\nMemoryMax=536870912\nCPUQuotaPerSecUSec=1s\n").unwrap();
        }
        let controller = Controller::new(dir.path().into(), config).unwrap();
        (dir, controller)
    }

    #[cfg(unix)]
    fn auto_candidate(c: &Controller) -> Manifest {
        let mut manifest = c.manifest.clone();
        manifest.allocation.enabled = true;
        for service in &mut manifest.services {
            service.limits = Some(Limits {
                cpus: 2.,
                memory_mb: 1024,
            });
        }
        manifest.services[0].allocation.weight = 3.;
        manifest
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allocation_decreases_before_increases_and_keeps_ceilings() {
        let (dir, mut c) = allocator_fixture();
        let result = c.apply_allocation(auto_candidate(&c)).await.unwrap();
        assert!(result["enabled"].as_bool().unwrap());
        let log = fs::read_to_string(dir.path().join("mutations")).unwrap();
        assert!(
            log.lines().next().unwrap().starts_with("b.service"),
            "{log}"
        );
        assert!(
            log.lines().last().unwrap().starts_with("a.service"),
            "{log}"
        );
        assert_eq!(c.manifest.services[0].limits.as_ref().unwrap().cpus, 2.);
        assert!(c.state.allocations["a"].cpus < 2.);
        assert!(c.effective_manifest().unwrap().validate().is_ok());
        c.apply_allocation(c.manifest.clone()).await.unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("mutations")).unwrap(),
            log,
            "Unchanged allocations must not rewrite native limits on every scheduler tick"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allocation_failure_restores_previous_caps_without_saving_policy() {
        let (dir, mut c) = allocator_fixture();
        fs::write(dir.path().join("fail"), "").unwrap();
        let error = c.apply_allocation(auto_candidate(&c)).await.unwrap_err();
        assert!(error.to_string().contains("restored"), "{error:#}");
        assert!(!Manifest::load(&c.config_path).unwrap().allocation.enabled);
        let status = runtime::native_status(&c.manifest.services[1])
            .await
            .unwrap();
        assert_eq!(status["actual_limits"]["cpus"], 1.);
        assert_eq!(status["actual_limits"]["memory_mb"], 512.);
        assert!(c.state.allocations.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allocation_restores_partially_applied_engine_commands_and_failed_config_writes() {
        for partial_engine_failure in [true, false] {
            let (dir, mut c) = allocator_fixture();
            let original = fs::read(&c.config_path).unwrap();
            if partial_engine_failure {
                fs::write(dir.path().join("fail-after"), "").unwrap();
            } else {
                fs::write(c.config_path.with_extension("tmp"), "block write").unwrap();
            }
            let error = c.apply_allocation(auto_candidate(&c)).await.unwrap_err();
            assert!(error.to_string().contains("restored"), "{error:#}");
            assert_eq!(fs::read(&c.config_path).unwrap(), original);
            for s in &c.manifest.services {
                let limits = c.actual_limits(s).await.unwrap();
                assert_eq!(limits.cpus, 1., "{}", s.id);
                assert_eq!(limits.memory_mb, 512, "{}", s.id);
            }
            assert!(c.state.allocations.is_empty());
            assert!(
                serde_json::from_slice::<State>(&fs::read(dir.path().join("state.json")).unwrap())
                    .unwrap()
                    .allocations
                    .is_empty()
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rebalance_preserves_hand_authored_yaml_and_only_persists_private_grants() {
        let (dir, mut c) = allocator_fixture();
        let manifest = auto_candidate(&c);
        c.config_path = dir.path().join("services.yaml");
        let authored = format!(
            "# Operator notes must survive scheduler ticks.\n{}",
            serde_saphyr::to_string(&manifest).unwrap()
        );
        fs::write(&c.config_path, &authored).unwrap();
        c.reload().unwrap();
        let modified = fs::metadata(&c.config_path).unwrap().modified().unwrap();
        let stale = c.config_path.with_extension("tmp");
        fs::write(&stale, "untouched stale temp file").unwrap();

        c.apply_allocation(c.manifest.clone()).await.unwrap();

        assert_eq!(fs::read_to_string(&c.config_path).unwrap(), authored);
        assert_eq!(
            fs::metadata(&c.config_path).unwrap().modified().unwrap(),
            modified
        );
        assert_eq!(
            fs::read_to_string(stale).unwrap(),
            "untouched stale temp file"
        );
        let persisted: State =
            serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
        assert_eq!(persisted.allocations.len(), 2);
        assert!(persisted.allocations["a"].cpus > persisted.allocations["b"].cpus);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allocation_rejects_unlimited_stopped_units_and_invalid_actions_without_mutation() {
        let (dir, mut c) = allocator_fixture();
        fs::write(dir.path().join("a.service"),"LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\nMemoryCurrent=0\nMemoryMax=infinity\nCPUQuotaPerSecUSec=infinity\n").unwrap();
        assert!(
            c.apply_allocation(auto_candidate(&c))
                .await
                .unwrap_err()
                .to_string()
                .contains("unlimited")
        );
        assert!(!dir.path().join("mutations").exists());
        auto_candidate(&c).save(&c.config_path).unwrap();
        assert!(
            c.action("missing", "start", None)
                .await
                .unwrap_err()
                .to_string()
                .contains("Unknown service")
        );
        let error = c.action("a", "run", None).await.unwrap_err();
        assert!(error.to_string().contains("systemd supports"), "{error:#}");
        assert!(!dir.path().join("mutations").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allocation_preview_is_read_only_and_detects_live_memory_pressure() {
        let (dir, mut c) = allocator_fixture();
        let original = fs::read(&c.config_path).unwrap();
        c.configure_allocation(AllocationRequest {
            enabled: Some(true),
            ..AllocationRequest::default()
        })
        .await
        .unwrap();
        assert_eq!(fs::read(&c.config_path).unwrap(), original);
        assert!(!dir.path().join("mutations").exists());
        fs::write(dir.path().join("a.service"),"LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=123\nMemoryCurrent=500000000\nMemoryMax=536870912\nCPUQuotaPerSecUSec=1s\n").unwrap();
        assert!(
            c.configure_allocation(AllocationRequest {
                enabled: Some(true),
                apply: true,
                ..AllocationRequest::default()
            })
            .await
            .is_err()
        );
        assert_eq!(fs::read(&c.config_path).unwrap(), original);
        assert!(!dir.path().join("mutations").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_samples_observers_by_actual_name_and_skips_missing_templates() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        fs::write(&script,r#"dir=$1; shift
printf '%s\n' "$*" >> "$dir/calls"
case "$1 $2" in
  'info --format') printf '%s\n' '{"NCPU":8,"MemTotal":8589934592}' ;;
  'container inspect')
    for name do :; done
    if [ "$name" = abc123 ]; then printf '%s\n' '{"Name":"/external-worker","State":{"Status":"running","Running":true},"HostConfig":{"NanoCpus":500000000,"Memory":536870912}}'; else printf '%s\n' 'No such container' >&2; exit 1; fi ;;
  'stats --no-stream') printf '%s\n' '{"Name":"external-worker","CPUPerc":"12%","MemUsage":"123MiB / 512MiB"}' ;;
  *) exit 1 ;;
esac
"#).unwrap();
        let manifest:Manifest=serde_json::from_value(json!({"version":1,"engine":{"command":["/bin/sh",script,dir.path()]},"budget":{"cpus":2,"memory_mb":1024},"services":[{"id":"observed","runtime":"docker","container":"abc123","observe_only":true},{"id":"template","runtime":"docker","enabled":false,"observe_only":false,"image":"example","limits":{"cpus":1,"memory_mb":512}}]})).unwrap();
        let config = dir.path().join("services.json");
        manifest.save(&config).unwrap();
        let mut controller = Controller::new(dir.path().into(), config).unwrap();
        let snapshot = controller.snapshot().await;
        assert_eq!(snapshot["services"][0]["memory_mb"], 123.);
        assert_eq!(snapshot["services"][0]["cpu_percent"], 12.);
        assert_eq!(snapshot["services"][1]["status"], "not created");
        let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|line| line.starts_with("container inspect "))
                .count(),
            2,
            "{calls}"
        );
        let sample = calls
            .lines()
            .find(|line| line.starts_with("stats "))
            .unwrap();
        assert!(sample.ends_with(" external-worker"), "{sample}");
        assert!(!sample.contains("abc123") && !sample.contains("template"));
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct JobState {
    paused: bool,
    last_started: Option<u64>,
    last_finished: Option<u64>,
    last_exit: Option<i64>,
    #[serde(default)]
    completion: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    services: BTreeMap<String, JobState>,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    native_units: BTreeMap<String, Service>,
    #[serde(default)]
    allocations: BTreeMap<String, Limits>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AllocationRequest {
    pub enabled: Option<bool>,
    pub apply: bool,
    pub budget: Option<Limits>,
}

struct AllocationPlan {
    allocations: Vec<Allocation>,
    warnings: Vec<String>,
    current: BTreeMap<String, Limits>,
}

pub struct Controller {
    pub workspace: PathBuf,
    pub config_path: PathBuf,
    pub manifest: Manifest,
    pub docker: Docker,
    state: State,
    cpu_samples: HashMap<String, (Instant, u64)>,
}

impl Controller {
    pub fn new(workspace: PathBuf, config_path: PathBuf) -> Result<Self> {
        fs::create_dir_all(&workspace)?;
        let manifest = Manifest::load(&config_path)?;
        let docker = Docker::new(manifest.engine.command.clone(), &workspace);
        let state_path = workspace.join("state.json");
        let state = if state_path.exists() {
            serde_json::from_slice(&fs::read(state_path)?)
                .context("Invalid state.json; preserve it for diagnosis before recovery")?
        } else {
            State::default()
        };
        Ok(Self {
            workspace,
            config_path,
            manifest,
            docker,
            state,
            cpu_samples: HashMap::new(),
        })
    }

    fn lock(&self) -> Result<File> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.workspace.join("controller.lock"))?;
        f.try_lock_exclusive()
            .context("Another controller is changing this workspace; retry when it finishes")?;
        Ok(f)
    }

    fn reload(&mut self) -> Result<()> {
        self.manifest = Manifest::load(&self.config_path)?;
        self.docker = Docker::new(self.manifest.engine.command.clone(), &self.workspace);
        let path = self.workspace.join("state.json");
        if path.exists() {
            self.state = serde_json::from_slice(&fs::read(path)?)?;
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        config::atomic_json(&self.workspace.join("state.json"), &self.state)
    }

    fn event(&mut self, id: &str, action: &str, message: &str, ok: bool) {
        self.state.events.push(
            json!({"timestamp":now(),"service":id,"action":action,"message":message,"ok":ok}),
        );
        if self.state.events.len() > 200 {
            self.state.events.drain(..self.state.events.len() - 200);
        }
    }

    fn service(&self, id: &str) -> Result<Service> {
        self.manifest
            .services
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .with_context(|| format!("Unknown service {id}"))
    }

    fn validate_action(service: &Service, action: &str, limits: Option<&Limits>) -> Result<()> {
        ensure!(
            service.managed(),
            "{} is observe-only; explicitly configure management before changing it",
            service.id
        );
        ensure!(
            service.enabled || action == "stop",
            "{} is disabled in the manifest",
            service.id
        );
        ensure!(
            matches!(
                action,
                "start" | "stop" | "restart" | "run" | "apply" | "recreate"
            ),
            "Unsupported action"
        );
        ensure!(
            limits.is_none() || action == "apply",
            "Limits are accepted only for apply"
        );
        ensure!(
            service.runtime != "systemd" || !matches!(action, "run" | "recreate"),
            "systemd supports start/stop/restart/apply"
        );
        ensure!(
            service.job() || action != "run",
            "run is for jobs; use start for daemons"
        );
        if let Some(limits) = limits {
            limits.validate()?;
        }
        Ok(())
    }

    fn effective_manifest(&self) -> Result<Manifest> {
        let mut manifest = self.manifest.clone();
        if manifest.allocation.enabled {
            for service in manifest
                .services
                .iter_mut()
                .filter(|s| s.managed() && s.enabled)
            {
                service.limits = Some(
                    self.state
                        .allocations
                        .get(&service.id)
                        .with_context(|| {
                            format!(
                                "{} has no allocation; apply automatic allocation first",
                                service.id
                            )
                        })?
                        .clone(),
                );
            }
            // Admission validates the granted caps, rather than the ceilings.
            manifest.allocation.enabled = false;
        }
        Ok(manifest)
    }

    async fn build_allocation(&self, manifest: &Manifest) -> Result<AllocationPlan> {
        manifest.validate()?;
        let eligible: Vec<_> = manifest
            .services
            .iter()
            .filter(|s| s.enabled && s.managed())
            .collect();
        let info = self.docker.info().await;
        if eligible.iter().any(|s| s.runtime == "docker") {
            ensure!(
                info["available"] == true,
                "Docker is unavailable; automatic allocation has not changed any caps"
            );
            ensure!(
                manifest.budget.cpus <= info["cpus"].as_f64().unwrap_or(0.)
                    && manifest.budget.memory_mb as f64 <= info["memory_mb"].as_f64().unwrap_or(0.),
                "Workspace budget exceeds Docker engine capacity; resize the engine or lower the budget"
            );
        }
        let stats = if info["available"] == true {
            self.docker.stats().await.unwrap_or_default()
        } else {
            HashMap::new()
        };
        let mut current = BTreeMap::new();
        let mut usage = HashMap::new();
        let mut warnings = vec![];
        let mut reserved_cpu = 0.;
        let mut reserved_memory = 0_u64;
        if info["available"] == true {
            for row in self.docker.inventory().await? {
                let name = row["Names"]
                    .as_str()
                    .context("Docker inventory missing Names")?;
                let d = self.inspect_raw(name).await?;
                if d["Config"]["Labels"]["io.self-host-init.workspace"].as_str()
                    != Some(&self.docker.owner)
                {
                    continue;
                }
                let running = d["State"]["Running"] == true;
                let actual = Limits {
                    cpus: d["HostConfig"]["NanoCpus"].as_f64().unwrap_or(0.) / 1e9,
                    memory_mb: d["HostConfig"]["Memory"]
                        .as_u64()
                        .unwrap_or(0)
                        .div_ceil(1048576),
                };
                let configured = eligible.iter().find(|s| {
                    s.runtime == "docker"
                        && self.docker.name(s) == name
                        && d["Config"]["Labels"]["io.self-host-init.service"].as_str()
                            == Some(s.id.as_str())
                });
                if let Some(s) = configured {
                    ensure!(
                        actual.cpus > 0. && actual.memory_mb > 0,
                        "{} has unlimited caps; apply finite manual limits before enabling automatic allocation",
                        s.id
                    );
                    if actual.cpus > 0. && actual.memory_mb > 0 {
                        current.insert(s.id.clone(), actual.clone());
                    }
                    if running {
                        if let Some(memory) = stats.get(name).and_then(|v| v["memory_mb"].as_f64())
                        {
                            usage.insert(s.id.clone(), memory);
                        } else {
                            // Missing telemetry must never authorize a memory reduction.
                            usage.insert(s.id.clone(), actual.memory_mb as f64 / 1.25);
                            warnings.push(format!("{}: memory telemetry unavailable; preserving its current memory cap", s.id));
                        }
                    }
                } else if running {
                    ensure!(
                        actual.cpus > 0. && actual.memory_mb > 0,
                        "Owned container {name} has unlimited resources; stop it before allocating capacity"
                    );
                    reserved_cpu += actual.cpus;
                    reserved_memory = reserved_memory
                        .checked_add(actual.memory_mb)
                        .context("Reserved memory overflow")?;
                }
            }
        }
        let mut native = self.state.native_units.clone();
        for s in manifest
            .services
            .iter()
            .filter(|s| s.runtime == "systemd" && s.managed())
        {
            native.insert(native_key(s), s.clone());
        }
        for s in native.values() {
            let status = runtime::native_status(s).await?;
            let consumes = !matches!(status["active_state"].as_str(), Some("inactive" | "failed"))
                || status["pid"].as_u64().unwrap_or(0) > 0
                || status["memory_mb"].as_f64().unwrap_or(0.) > 0.;
            let actual = status["actual_limits"]["cpus"]
                .as_f64()
                .zip(status["actual_limits"]["memory_mb"].as_f64())
                .map(|(cpus, memory)| Limits {
                    cpus,
                    memory_mb: memory.ceil() as u64,
                });
            let configured = eligible
                .iter()
                .find(|x| x.runtime == "systemd" && native_key(x) == native_key(s));
            if let Some(configured) = configured {
                ensure!(
                    actual
                        .as_ref()
                        .is_some_and(|l| l.cpus > 0. && l.memory_mb > 0),
                    "{} has unlimited caps; apply finite manual limits before enabling automatic allocation",
                    configured.id
                );
                if let Some(actual) = actual.as_ref() {
                    current.insert(configured.id.clone(), actual.clone());
                }
                if consumes {
                    if let Some(memory) = status["memory_mb"].as_f64() {
                        usage.insert(configured.id.clone(), memory);
                    } else {
                        usage.insert(
                            configured.id.clone(),
                            actual.as_ref().unwrap().memory_mb as f64 / 1.25,
                        );
                        warnings.push(format!(
                            "{}: memory telemetry unavailable; preserving its current memory cap",
                            configured.id
                        ));
                    }
                }
            } else if consumes {
                let actual=actual.context("An adopted systemd unit has unlimited caps; set finite limits or stop it before allocating")?;
                ensure!(
                    actual.cpus > 0. && actual.memory_mb > 0,
                    "An adopted systemd unit has invalid caps; set finite limits or stop it before allocating"
                );
                reserved_cpu += actual.cpus;
                reserved_memory = reserved_memory
                    .checked_add(actual.memory_mb)
                    .context("Reserved memory overflow")?;
            }
        }
        ensure!(
            reserved_cpu <= manifest.budget.cpus && reserved_memory <= manifest.budget.memory_mb,
            "Running removed or disabled services exhaust the budget; stop them or raise the budget before allocating"
        );
        if reserved_cpu > 0. || reserved_memory > 0 {
            warnings.push(format!("Removed or disabled running services reserve {reserved_cpu:.3} CPUs and {reserved_memory} MiB"));
        }
        let inputs: Vec<_> = eligible
            .iter()
            .map(|s| {
                let ceiling = s
                    .limits
                    .as_ref()
                    .expect("manifest validates managed limits");
                AllocationInput {
                    id: s.id.clone(),
                    weight: s.allocation.weight,
                    min_cpus: s.allocation.min_cpus,
                    min_memory_mb: s.allocation.min_memory_mb,
                    max_cpus: ceiling.cpus,
                    max_memory_mb: ceiling.memory_mb,
                    memory_usage_mb: usage.get(&s.id).copied(),
                }
            })
            .collect();
        let allocations = if manifest.allocation.enabled {
            allocation::allocate(
                &inputs,
                manifest.budget.cpus - reserved_cpu,
                manifest.budget.memory_mb - reserved_memory,
            )?
        } else {
            // Disabling automatic allocation restores the declared caps safely.
            let mut rows = vec![];
            for input in &inputs {
                ensure!(
                    input
                        .memory_usage_mb
                        .is_none_or(|m| (m * 1.25).ceil() <= input.max_memory_mb as f64),
                    "{}: declared memory cap is below current usage plus 25% headroom",
                    input.id
                );
                rows.push(Allocation {
                    id: input.id.clone(),
                    cpus: input.max_cpus,
                    memory_mb: input.max_memory_mb,
                    weight: input.weight,
                    min_cpus: input.min_cpus,
                    min_memory_mb: input.min_memory_mb,
                });
            }
            rows
        };
        Ok(AllocationPlan {
            allocations,
            warnings,
            current,
        })
    }

    fn allocation_json(manifest: &Manifest, plan: &AllocationPlan) -> Value {
        json!({"enabled":manifest.allocation.enabled,"budget":manifest.budget,"allocations":plan.allocations,"warnings":plan.warnings})
    }

    pub async fn allocation_plan(&mut self) -> Result<Value> {
        self.reload()?;
        let plan = self.build_allocation(&self.manifest).await?;
        Ok(Self::allocation_json(&self.manifest, &plan))
    }

    pub async fn configure_allocation(&mut self, request: AllocationRequest) -> Result<Value> {
        let _lock = self.lock()?;
        self.reload()?;
        let mut candidate = self.manifest.clone();
        if let Some(enabled) = request.enabled {
            candidate.allocation.enabled = enabled;
        }
        if let Some(budget) = request.budget {
            budget.validate()?;
            candidate.budget.cpus = budget.cpus;
            candidate.budget.memory_mb = budget.memory_mb;
        }
        if !request.apply {
            return Ok(Self::allocation_json(
                &candidate,
                &self.build_allocation(&candidate).await?,
            ));
        }
        let result = self.apply_allocation(candidate).await;
        self.event(
            "workspace",
            "allocation",
            &result
                .as_ref()
                .map(|_| "Applied resource allocation".to_owned())
                .unwrap_or_else(|e| format!("{e:#}")),
            result.is_ok(),
        );
        self.persist()?;
        result
    }

    pub async fn rebalance(&mut self) -> Result<Value> {
        self.configure_allocation(AllocationRequest {
            apply: true,
            ..AllocationRequest::default()
        })
        .await
    }

    async fn scale_existing(&self, service: &Service, old: Option<&Limits>) -> Result<()> {
        let limits = service
            .limits
            .as_ref()
            .context("Allocation omitted limits")?;
        if old.is_some_and(|l| limits.memory_mb < l.memory_mb) {
            let memory = if service.runtime == "docker" {
                let d = self.docker.inspect(service).await?;
                if d.as_ref().is_some_and(|d| d["State"]["Running"] == true) {
                    Some(self.docker.stats().await?.get(&self.docker.name(service)).and_then(|v|v["memory_mb"].as_f64()).context("Memory telemetry unavailable; refusing to lower a running container's cap")?)
                } else {
                    None
                }
            } else {
                let status = runtime::native_status(service).await?;
                let consumes =
                    !matches!(status["active_state"].as_str(), Some("inactive" | "failed"))
                        || status["pid"].as_u64().unwrap_or(0) > 0
                        || status["memory_mb"].as_f64().unwrap_or(0.) > 0.;
                if consumes {
                    Some(status["memory_mb"].as_f64().context(
                        "Memory telemetry unavailable; refusing to lower a running unit's cap",
                    )?)
                } else {
                    None
                }
            };
            ensure!(
                memory.is_none_or(|m| (m * 1.25).ceil() <= limits.memory_mb as f64),
                "{} grew beyond the proposed memory cap plus headroom; allocation was cancelled",
                service.id
            );
        }
        if service.runtime == "docker" {
            if self.docker.inspect(service).await?.is_some() {
                self.docker.update(service).await?;
            }
        } else {
            runtime::native_scale(service).await?;
        }
        Ok(())
    }

    async fn actual_limits(&self, service: &Service) -> Result<Limits> {
        if service.runtime == "docker" {
            let inspection = self
                .docker
                .inspect(service)
                .await?
                .context("Container disappeared during allocation")?;
            Ok(Limits {
                cpus: inspection["HostConfig"]["NanoCpus"]
                    .as_f64()
                    .context("Missing CPU cap")?
                    / 1e9,
                memory_mb: inspection["HostConfig"]["Memory"]
                    .as_u64()
                    .context("Missing memory cap")?
                    .div_ceil(1048576),
            })
        } else {
            let status = runtime::native_status(service).await?;
            Ok(Limits {
                cpus: status["actual_limits"]["cpus"]
                    .as_f64()
                    .context("Missing CPU cap")?,
                memory_mb: status["actual_limits"]["memory_mb"]
                    .as_f64()
                    .context("Missing memory cap")?
                    .ceil() as u64,
            })
        }
    }

    async fn apply_allocation(&mut self, candidate: Manifest) -> Result<Value> {
        let plan = self.build_allocation(&candidate).await?;
        let configuration_changed =
            serde_json::to_value(&candidate)? != serde_json::to_value(&self.manifest)?;
        let previous_state = self.state.clone();
        let mut granted = candidate.clone();
        for row in &plan.allocations {
            granted
                .services
                .iter_mut()
                .find(|s| s.id == row.id)
                .unwrap()
                .limits = Some(Limits {
                cpus: row.cpus,
                memory_mb: row.memory_mb,
            });
        }
        granted.allocation.enabled = false;
        let mut applied = plan.current.clone();
        let result = async {
            // Decrease both resource dimensions across the workspace before any increase.
            for s in granted.services.iter().filter(|s| s.enabled && s.managed()) {
                let Some(old) = applied.get(&s.id).cloned() else {
                    continue;
                };
                let target = s.limits.as_ref().unwrap();
                let reduced = Limits {
                    cpus: old.cpus.min(target.cpus),
                    memory_mb: old.memory_mb.min(target.memory_mb),
                };
                if old.cpus - reduced.cpus > 1e-9 || reduced.memory_mb != old.memory_mb {
                    let mut transition = s.clone();
                    transition.limits = Some(reduced.clone());
                    // A failed engine command may have applied some properties.
                    applied.insert(s.id.clone(), reduced);
                    self.scale_existing(&transition, Some(&old)).await?;
                }
            }
            self.admission(&granted, None, false).await?;
            for s in granted.services.iter().filter(|s| s.enabled && s.managed()) {
                let old = applied.get(&s.id).cloned();
                let target = s.limits.as_ref().unwrap();
                if old.as_ref().is_none_or(|l| {
                    (l.cpus - target.cpus).abs() > 1e-9 || l.memory_mb != target.memory_mb
                }) {
                    self.admission(&granted, Some(s), false).await?;
                    applied.insert(s.id.clone(), s.limits.clone().unwrap());
                    self.scale_existing(s, old.as_ref()).await?;
                }
            }
            self.state.allocations = plan
                .allocations
                .iter()
                .map(|r| {
                    (
                        r.id.clone(),
                        Limits {
                            cpus: r.cpus,
                            memory_mb: r.memory_mb,
                        },
                    )
                })
                .collect();
            for s in granted
                .services
                .iter()
                .filter(|s| s.managed() && s.runtime == "systemd")
            {
                self.state.native_units.insert(native_key(s), s.clone());
            }
            // Commit grants first so a crash cannot enable a policy without its caps.
            self.persist()?;
            if configuration_changed {
                candidate.save(&self.config_path)?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            let mut failures = vec![];
            // Rollback uses the same reduction-first order; never claim a partial apply succeeded.
            for reductions in [true, false] {
                if !reductions && !failures.is_empty() {
                    break;
                }
                for (id, old) in &plan.current {
                    if !applied.contains_key(id) {
                        continue;
                    }
                    let mut s = candidate
                        .services
                        .iter()
                        .find(|s| s.id == *id)
                        .unwrap()
                        .clone();
                    let current = match self.actual_limits(&s).await {
                        Ok(current) => current,
                        Err(e) => {
                            failures.push(format!("{id}: cannot inspect actual caps: {e:#}"));
                            continue;
                        }
                    };
                    let desired = if reductions {
                        Limits {
                            cpus: current.cpus.min(old.cpus),
                            memory_mb: current.memory_mb.min(old.memory_mb),
                        }
                    } else {
                        old.clone()
                    };
                    if desired == current {
                        continue;
                    }
                    s.limits = Some(desired.clone());
                    match self.scale_existing(&s, Some(&current)).await {
                        Ok(()) => {
                            applied.insert(id.clone(), desired);
                        }
                        Err(e) => failures.push(format!("{id}: {e:#}")),
                    }
                }
            }
            self.state = previous_state;
            if let Err(error) = self.persist() {
                failures.push(format!("allocation state: {error:#}"));
            }
            if !failures.is_empty() {
                bail!(
                    "Allocation failed: {error:#}. Rollback incomplete: {}. Inspect actual limits before retrying",
                    failures.join("; ")
                );
            }
            return Err(error.context("Allocation cancelled; previous caps restored"));
        }
        self.manifest = candidate;
        Ok(Self::allocation_json(&self.manifest, &plan))
    }

    async fn inspect_raw(&self, name: &str) -> Result<Value> {
        let mut args = self.docker.command.clone();
        args.extend(["container".into(), "inspect".into(), name.into()]);
        let raw = runtime::run(&args).await?;
        let rows: Vec<Value> = serde_json::from_str(&raw)?;
        rows.into_iter()
            .next()
            .context("Docker returned no container")
    }

    /// Reserve enabled daemons even while stopped. Count every running owned
    /// container, including services removed from the manifest, against admission.
    async fn admission(
        &self,
        manifest: &Manifest,
        target: Option<&Service>,
        starting: bool,
    ) -> Result<()> {
        manifest.validate()?;
        let mut cpu = 0.;
        let mut memory = 0;
        let mut running_jobs = 0;
        let mut active = HashSet::new();
        let has_docker = manifest
            .services
            .iter()
            .any(|s| s.managed() && s.runtime == "docker")
            || target.is_some_and(|s| s.runtime == "docker");
        let info = self.docker.info().await;
        if has_docker {
            ensure!(
                info["available"] == true,
                "Docker is unavailable: {}",
                info["error"]
            );
            ensure!(
                manifest.budget.cpus <= info["cpus"].as_f64().unwrap_or(0.)
                    && manifest.budget.memory_mb as f64 <= info["memory_mb"].as_f64().unwrap_or(0.),
                "Workspace budget exceeds Docker engine capacity; adjust budget or resize the engine separately"
            );
        }
        if info["available"] == true {
            for row in self.docker.inventory().await? {
                let name = row["Names"]
                    .as_str()
                    .context("Docker inventory missing Names")?;
                let d = self.inspect_raw(name).await?;
                if d["Config"]["Labels"]["io.self-host-init.workspace"].as_str()
                    != Some(&self.docker.owner)
                    || d["State"]["Running"] != true
                {
                    continue;
                }
                let id = d["Config"]["Labels"]["io.self-host-init.service"]
                    .as_str()
                    .context("Owned container has no service label")?;
                active.insert(active_key("docker", id));
                let configured = manifest
                    .services
                    .iter()
                    .find(|s| s.id == id && s.runtime == "docker" && s.managed());
                let actual_cpu = d["HostConfig"]["NanoCpus"].as_f64().unwrap_or(0.) / 1e9;
                let actual_memory = d["HostConfig"]["Memory"]
                    .as_u64()
                    .unwrap_or(0)
                    .div_ceil(1048576);
                ensure!(
                    actual_cpu > 0. && actual_memory > 0,
                    "Owned container {name} has unlimited resources; stop it before admitting work"
                );
                let replacement = target
                    .filter(|s| s.id == id && s.runtime == "docker")
                    .and_then(|s| s.limits.as_ref());
                let desired = configured
                    .filter(|s| !s.job() && s.enabled)
                    .and_then(|s| s.limits.as_ref());
                let l = replacement.or(desired);
                cpu += if replacement.is_some() {
                    l.unwrap().cpus
                } else {
                    actual_cpu.max(l.map_or(0., |x| x.cpus))
                };
                memory += if replacement.is_some() {
                    l.unwrap().memory_mb
                } else {
                    actual_memory.max(l.map_or(0, |x| x.memory_mb))
                };
                if configured.is_none_or(|s| s.job()) {
                    running_jobs += 1;
                }
            }
        }
        // Keep adopted native units in the ledger even if their declaration is
        // disabled or removed. A controller restart must not release their quota.
        let mut native = self.state.native_units.clone();
        for s in manifest
            .services
            .iter()
            .filter(|s| s.runtime == "systemd" && s.managed())
        {
            native.insert(native_key(s), s.clone());
        }
        for s in native.values() {
            let observed = runtime::native_status(s).await?;
            let state = observed["active_state"].as_str().unwrap_or("unknown");
            let consumes = !matches!(state, "inactive" | "failed")
                || observed["pid"].as_u64().unwrap_or(0) > 0
                || observed["memory_mb"].as_f64().unwrap_or(0.) > 0.;
            if !consumes {
                continue;
            }
            let configured = manifest
                .services
                .iter()
                .find(|x| x.runtime == "systemd" && x.managed() && native_key(x) == native_key(s));
            let replacement = target
                .filter(|t| t.runtime == "systemd" && native_key(t) == native_key(s))
                .and_then(|t| t.limits.as_ref());
            let desired = configured
                .filter(|s| s.enabled)
                .and_then(|s| s.limits.as_ref());
            let actual_cpu = observed["actual_limits"]["cpus"].as_f64();
            let actual_memory = observed["actual_limits"]["memory_mb"]
                .as_f64()
                .map(|m| m.ceil() as u64);
            if let Some(l) = replacement {
                cpu += l.cpus;
                memory += l.memory_mb;
            } else {
                ensure!(
                    actual_cpu.is_some_and(|c| c > 0.) && actual_memory.is_some_and(|m| m > 0),
                    "Managed systemd unit {} has unlimited resources; apply its finite limits before admitting work",
                    s.id
                );
                cpu += actual_cpu.unwrap().max(desired.map_or(0., |l| l.cpus));
                memory += actual_memory
                    .unwrap()
                    .max(desired.map_or(0, |l| l.memory_mb));
            }
            if let Some(configured) = configured {
                active.insert(active_key("systemd", &configured.id));
            }
        }
        for s in manifest
            .services
            .iter()
            .filter(|s| s.managed() && s.enabled && !s.job())
        {
            if active.contains(&active_key(&s.runtime, &s.id)) {
                continue;
            }
            let l = s.limits.as_ref().unwrap();
            cpu += l.cpus;
            memory += l.memory_mb;
        }
        if let Some(t) = target
            .filter(|s| s.job() && !active.contains(&active_key(&s.runtime, &s.id)) && starting)
        {
            ensure!(
                running_jobs < manifest.budget.max_parallel_jobs,
                "Maximum parallel jobs reached; job will wait"
            );
            let l = t.limits.as_ref().unwrap();
            cpu += l.cpus;
            memory += l.memory_mb;
        }
        config::ensure_under_budget(cpu, memory, &manifest.budget)
    }

    pub async fn logs(&self, id: &str, tail: usize) -> Result<String> {
        let s = self.service(id)?;
        let raw = if s.runtime == "docker" {
            self.docker.logs(&s, tail.clamp(1, 1000)).await?
        } else {
            runtime::native_logs(&s, tail.clamp(1, 1000)).await?
        };
        Ok(raw)
    }

    pub async fn action(
        &mut self,
        id: &str,
        action: &str,
        limits: Option<Limits>,
    ) -> Result<Value> {
        let _lock = self.lock()?;
        self.reload()?;
        Self::validate_action(&self.service(id)?, action, limits.as_ref())?;
        if self.manifest.allocation.enabled && action != "stop" && action != "apply" {
            self.apply_allocation(self.manifest.clone()).await?;
        }
        let result = self.action_inner(id, action, limits).await;
        self.event(
            id,
            action,
            result
                .as_ref()
                .map(|_| "Completed".to_string())
                .unwrap_or_else(|e| e.to_string())
                .as_str(),
            result.is_ok(),
        );
        self.persist()?;
        result
    }

    async fn action_inner(
        &mut self,
        id: &str,
        action: &str,
        limits: Option<Limits>,
    ) -> Result<Value> {
        let mut s = self.service(id)?;
        Self::validate_action(&s, action, limits.as_ref())?;
        let mut candidate = self.manifest.clone();
        let mut changing_limits = limits.is_some();
        if let Some(l) = limits {
            l.validate()?;
            s.limits = Some(l);
            *candidate.services.iter_mut().find(|x| x.id == id).unwrap() = s.clone();
            candidate.validate()?;
        }
        if candidate.allocation.enabled && action == "apply" {
            self.apply_allocation(candidate.clone()).await?;
            changing_limits = false;
        }
        if candidate.allocation.enabled && action != "stop" {
            candidate = self.effective_manifest()?;
            s = candidate
                .services
                .iter()
                .find(|x| x.id == id)
                .unwrap()
                .clone();
        }
        candidate.validate()?;
        // Reducing an existing native cap is safe even while other units are
        // still being enrolled. This lets bulk apply converge from unlimited
        // units without admitting any new workload during the transition.
        let native_reduction = if s.runtime == "systemd" && action == "apply" {
            let actual = runtime::native_status(&s).await?;
            let l = s.limits.as_ref().unwrap();
            actual["actual_limits"]["cpus"]
                .as_f64()
                .is_none_or(|c| l.cpus <= c)
                && actual["actual_limits"]["memory_mb"]
                    .as_f64()
                    .is_none_or(|m| l.memory_mb as f64 <= m)
        } else {
            false
        };
        if action != "stop" && !native_reduction {
            self.admission(
                &candidate,
                Some(&s),
                s.job() && matches!(action, "run" | "start" | "restart" | "recreate"),
            )
            .await?;
        }
        if s.runtime == "systemd" {
            ensure!(
                !matches!(action, "run" | "recreate"),
                "systemd supports start/stop/restart/apply"
            );
            if action != "stop" {
                runtime::native_scale(&s).await?;
                self.state.native_units.insert(native_key(&s), s.clone());
            }
            if action != "apply" {
                runtime::native_action(&s, action).await?;
            }
        } else if s.runtime == "docker" {
            let d = self.docker.inspect(&s).await?;
            match action {
                "stop" => {
                    if d.is_some() {
                        self.docker.lifecycle(&s, "stop").await?;
                    }
                    self.state.services.entry(id.into()).or_default().paused = true;
                }
                "apply" => {
                    if d.is_some() {
                        self.docker.update(&s).await?;
                    } else if !s.job() {
                        self.docker.create(&s).await?;
                        self.state.services.entry(id.into()).or_default().paused = false;
                    }
                }
                "run" | "start" if s.job() => {
                    ensure!(
                        d.as_ref().is_none_or(|v| v["State"]["Running"] != true),
                        "Job {id} is already running"
                    );
                    if d.is_some() {
                        self.docker.lifecycle(&s, "recreate").await?;
                    } else {
                        self.docker.create(&s).await?;
                    }
                    let state = self.state.services.entry(id.into()).or_default();
                    state.paused = false;
                    state.last_started = Some(now());
                }
                "run" => bail!("run is for jobs; use start for daemons"),
                "recreate" => {
                    ensure!(d.is_some(), "Container does not exist; use start");
                    self.docker.lifecycle(&s, "recreate").await?;
                }
                "start" | "restart" => {
                    if d.is_some() {
                        self.docker.update(&s).await?;
                        self.docker.lifecycle(&s, action).await?;
                    } else {
                        self.docker.create(&s).await?;
                    }
                    self.state.services.entry(id.into()).or_default().paused = false;
                }
                _ => bail!("Unsupported Docker action"),
            }
        } else {
            bail!("Runtime does not support management");
        }
        if action == "recreate" {
            self.state.services.entry(id.into()).or_default().paused = false;
        }
        if s.job() && matches!(action, "restart" | "recreate") {
            let state = self.state.services.entry(id.into()).or_default();
            state.paused = false;
            state.last_started = Some(now());
        }
        if changing_limits
            && candidate
                .services
                .iter()
                .find(|x| x.id == id)
                .unwrap()
                .limits
                .as_ref()
                .map(|x| (x.cpus, x.memory_mb))
                != self
                    .manifest
                    .services
                    .iter()
                    .find(|x| x.id == id)
                    .unwrap()
                    .limits
                    .as_ref()
                    .map(|x| (x.cpus, x.memory_mb))
        {
            candidate.save(&self.config_path)?;
            self.manifest = candidate;
        }
        Ok(json!({"ok":true,"service":id,"action":action}))
    }

    pub async fn set_budget(&mut self, limits: Limits) -> Result<Value> {
        let _lock = self.lock()?;
        self.reload()?;
        limits.validate()?;
        let mut candidate = self.manifest.clone();
        candidate.budget.cpus = limits.cpus;
        candidate.budget.memory_mb = limits.memory_mb;
        if candidate.allocation.enabled {
            return self.apply_allocation(candidate).await;
        }
        self.admission(&candidate, None, false).await?;
        candidate.save(&self.config_path)?;
        self.manifest = candidate;
        self.event("workspace", "budget", "Updated admission budget", true);
        self.persist()?;
        Ok(json!({"ok":true,"budget":self.manifest.budget}))
    }

    /// Reconciliation creates enabled daemons and starts due jobs. Docker retains
    /// jobs after exit, so a controller restart cannot lose an in-flight job.
    pub async fn tick(&mut self) -> Result<()> {
        let _lock = self.lock()?;
        self.reload()?;
        if self.manifest.allocation.enabled
            && let Err(error) = self.apply_allocation(self.manifest.clone()).await
        {
            self.event("workspace", "allocation", &format!("{error:#}"), false);
            self.persist()?;
            return Err(error);
        }
        let mut services = self.manifest.services.clone();
        // Oldest/never-run jobs get first refusal so an interval shorter than
        // a job's runtime cannot starve every job after it in the manifest.
        services.sort_by_key(|s| {
            (
                s.job(),
                self.state
                    .services
                    .get(&s.id)
                    .and_then(|state| state.last_started)
                    .unwrap_or(0),
            )
        });
        for s in services.iter().filter(|s| s.enabled && s.managed()) {
            if self.state.services.get(&s.id).is_some_and(|x| x.paused) {
                continue;
            }
            if s.runtime == "systemd" {
                // Reapply persistent resource settings only with explicit apply;
                // observing the unit never changes the host.
                continue;
            }
            if s.runtime != "docker" {
                continue;
            }
            let d = match self.docker.inspect(s).await {
                Ok(d) => d,
                Err(e) => {
                    self.event(&s.id, "inspect", &e.to_string(), false);
                    continue;
                }
            };
            if s.job() {
                if let Some(d) = &d {
                    if d["State"]["Running"] == true {
                        continue;
                    }
                    let finished = d["State"]["FinishedAt"].as_str().unwrap_or("");
                    let key = format!("{}:{finished}", d["Id"].as_str().unwrap_or(""));
                    let state = self.state.services.entry(s.id.clone()).or_default();
                    if !finished.is_empty()
                        && !finished.starts_with("0001")
                        && state.completion != key
                    {
                        state.completion = key;
                        state.last_finished = Some(now());
                        state.last_exit = d["State"]["ExitCode"].as_i64();
                        self.event(
                            &s.id,
                            "finished",
                            &format!("Exit code {}", d["State"]["ExitCode"]),
                            d["State"]["ExitCode"] == 0,
                        );
                    }
                }
                if self
                    .state
                    .services
                    .get(&s.id)
                    .and_then(|x| x.last_started)
                    .is_some_and(|t| now().saturating_sub(t) < s.interval_seconds.unwrap_or(3600))
                {
                    continue;
                }
                // Persist completed status before potentially replacing container.
                self.persist()?;
                if let Err(e) = self.action_inner(&s.id, "run", None).await {
                    self.event(&s.id, "waiting", &e.to_string(), false);
                } else {
                    self.event(&s.id, "run", "Scheduled job started", true);
                }
            } else if d.is_none() {
                if let Err(e) = self.action_inner(&s.id, "start", None).await {
                    self.event(&s.id, "start", &e.to_string(), false);
                } else {
                    self.event(&s.id, "start", "Daemon created", true);
                }
            }
            self.persist()?;
        }
        self.persist()
    }

    pub async fn snapshot(&mut self) -> Value {
        let manifest_error = self.reload().err().map(|e| e.to_string());
        let runtime = self.docker.info().await;
        let mut inspections = HashMap::new();
        let mut running_names = vec![];
        for service in self
            .manifest
            .services
            .iter()
            .filter(|s| s.runtime == "docker")
        {
            let inspection = if runtime["available"] == true {
                self.docker
                    .inspect(service)
                    .await
                    .map_err(|e| e.to_string())
            } else {
                Err(format!("Docker is unavailable: {}", runtime["error"]))
            };
            if let Ok(Some(d)) = &inspection
                && d["State"]["Running"] == true
                && let Some(name) = d["Name"].as_str()
            {
                running_names.push(name.trim_start_matches('/').to_owned());
            }
            inspections.insert(service.id.clone(), inspection);
        }
        let stats = self
            .docker
            .stats_for(&running_names)
            .await
            .unwrap_or_default();
        let mut services = vec![];
        for s in self.manifest.services.clone() {
            let effective = if self.manifest.allocation.enabled {
                self.state.allocations.get(&s.id)
            } else {
                s.limits.as_ref()
            };
            let mut view = json!({"id":s.id,"name":if s.name.is_empty(){s.id.clone()}else{s.name.clone()},"description":s.description,"runtime":s.runtime,"kind":s.kind,"enabled":s.enabled,"observe_only":s.observe_only,"limits":s.limits,"effective_limits":effective,"allocation":s.allocation,"status":"unknown","health":"unknown","cpu_percent":null,"memory_mb":null,"capabilities":{"scale":s.managed() && s.runtime!="launchd","lifecycle":s.managed() && s.runtime!="launchd"}});
            let state = self.state.services.get(&s.id).cloned().unwrap_or_default();
            view["last_run"] = json!(state.last_started);
            view["last_exit"] = json!(state.last_exit);
            view["paused"] = json!(state.paused);
            view["next_run"] = json!(if s.job() && !state.paused {
                state
                    .last_started
                    .map(|t| t + s.interval_seconds.unwrap_or(3600))
            } else {
                None
            });
            if s.runtime == "docker" {
                match inspections
                    .remove(&s.id)
                    .expect("Docker entries were inspected")
                {
                    Ok(Some(d)) => {
                        view["status"] = d["State"]["Status"].clone();
                        view["health"] = d["State"]["Health"]["Status"]
                            .as_str()
                            .map_or(json!("no healthcheck"), |h| json!(h));
                        view["actual_limits"] = json!({"cpus":d["HostConfig"]["NanoCpus"].as_f64().unwrap_or(0.)/1e9,"memory_mb":d["HostConfig"]["Memory"].as_u64().unwrap_or(0)/1048576});
                        if let Some(v) = d["Name"]
                            .as_str()
                            .and_then(|name| stats.get(name.trim_start_matches('/')))
                        {
                            view["cpu_percent"] = v["cpu_percent"].clone();
                            view["memory_mb"] = v["memory_mb"].clone();
                        }
                    }
                    Ok(None) => view["status"] = json!("not created"),
                    Err(e) => {
                        view["status"] = json!("unavailable");
                        view["health"] = json!(e.to_string());
                    }
                }
            } else {
                match runtime::native_status(&s).await {
                    Ok(v) => {
                        if let Some(o) = v.as_object() {
                            for (k, v) in o {
                                view[k] = v.clone();
                            }
                        }
                    }
                    Err(e) => {
                        view["status"] = json!("unavailable");
                        view["health"] = json!(e.to_string());
                    }
                }
            }
            if s.runtime == "systemd"
                && let Some(usage) = view["cpu_usage_ns"].as_u64()
            {
                let sample_time = Instant::now();
                if let Some((previous_time, previous_usage)) =
                    self.cpu_samples.insert(s.id.clone(), (sample_time, usage))
                {
                    let elapsed = sample_time.duration_since(previous_time).as_secs_f64();
                    if usage >= previous_usage && elapsed > 0. {
                        view["cpu_percent"] =
                            json!((usage - previous_usage) as f64 / 1e9 / elapsed * 100.);
                    }
                }
            }
            services.push(view);
        }
        json!({"workspace":self.workspace,"budget":self.manifest.budget,"allocation":self.manifest.allocation,"capacity":host_capacity().await,"services":services,"events":self.state.events.iter().rev().take(50).collect::<Vec<_>>(),"runtime":runtime,"manifest_error":manifest_error,"timestamp":now()})
    }

    pub async fn plan(&mut self) -> Result<Value> {
        self.reload()?;
        let mut rows = vec![];
        for s in &self.manifest.services {
            let action = if !s.enabled {
                "disabled"
            } else if s.observe_only {
                "observe"
            } else if s.runtime == "systemd" {
                "apply persistent CPU/memory limits"
            } else if self.docker.inspect(s).await?.is_some() {
                "update CPU/memory limits"
            } else if s.job() {
                "schedule job"
            } else {
                "create daemon"
            };
            rows.push(json!({"id":s.id,"action":action,"limits":s.limits}));
        }
        Ok(json!({"budget":self.manifest.budget,"services":rows}))
    }
}
