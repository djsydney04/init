use crate::{
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
        drop(guard);
        assert!(
            c.action("external", "stop", None)
                .await
                .unwrap_err()
                .to_string()
                .contains("observe-only")
        );
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

#[derive(Default, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    services: BTreeMap<String, JobState>,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    native_units: BTreeMap<String, Service>,
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
        ensure!(
            s.managed(),
            "{id} is observe-only; explicitly configure management before changing it"
        );
        ensure!(
            s.enabled || action == "stop",
            "{id} is disabled in the manifest"
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
        let mut candidate = self.manifest.clone();
        if let Some(l) = limits {
            l.validate()?;
            s.limits = Some(l);
            *candidate.services.iter_mut().find(|x| x.id == id).unwrap() = s.clone();
            candidate.validate()?;
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
        if candidate
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
        let stats = self.docker.stats().await.unwrap_or_default();
        let mut services = vec![];
        for s in self.manifest.services.clone() {
            let mut view = json!({"id":s.id,"name":if s.name.is_empty(){s.id.clone()}else{s.name.clone()},"description":s.description,"runtime":s.runtime,"kind":s.kind,"enabled":s.enabled,"observe_only":s.observe_only,"limits":s.limits,"status":"unknown","health":"unknown","cpu_percent":null,"memory_mb":null,"capabilities":{"scale":s.managed() && s.runtime!="launchd","lifecycle":s.managed() && s.runtime!="launchd"}});
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
                match self.docker.inspect(&s).await {
                    Ok(Some(d)) => {
                        view["status"] = d["State"]["Status"].clone();
                        view["health"] = d["State"]["Health"]["Status"]
                            .as_str()
                            .map_or(json!("no healthcheck"), |h| json!(h));
                        view["actual_limits"] = json!({"cpus":d["HostConfig"]["NanoCpus"].as_f64().unwrap_or(0.)/1e9,"memory_mb":d["HostConfig"]["Memory"].as_u64().unwrap_or(0)/1048576});
                        if let Some(v) = stats.get(&self.docker.name(&s)) {
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
        json!({"workspace":self.workspace,"budget":self.manifest.budget,"services":services,"events":self.state.events.iter().rev().take(50).collect::<Vec<_>>(),"runtime":runtime,"manifest_error":manifest_error,"timestamp":now()})
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
