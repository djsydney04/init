use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, path::Path};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub cpus: f64,
    pub memory_mb: u64,
}

impl Limits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.cpus.is_finite() && self.cpus >= 0.01 && self.cpus <= 65536.,
            "cpus must be finite and between 0.01 and 65536"
        );
        ensure!(
            (6..=1 << 40).contains(&self.memory_mb),
            "memory_mb must be between 6 and 1099511627776"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub cpus: f64,
    pub memory_mb: u64,
    #[serde(default = "one")]
    pub max_parallel_jobs: usize,
}
fn one() -> usize {
    1
}
fn yes() -> bool {
    true
}
fn daemon() -> String {
    "daemon".into()
}
fn system() -> String {
    "system".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocationPolicy {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceAllocation {
    pub weight: f64,
    pub min_cpus: f64,
    pub min_memory_mb: u64,
}

impl Default for ServiceAllocation {
    fn default() -> Self {
        Self {
            weight: 1.,
            min_cpus: 0.1,
            min_memory_mb: 64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_copy: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub runtime: String,
    #[serde(default = "daemon")]
    pub kind: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "yes")]
    pub observe_only: bool,
    pub limits: Option<Limits>,
    #[serde(default)]
    pub allocation: ServiceAllocation,
    pub image: Option<String>,
    pub container: Option<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    pub env_file: Option<String>,
    #[serde(default)]
    pub volumes: Vec<Volume>,
    #[serde(default)]
    pub ports: Vec<String>,
    pub interval_seconds: Option<u64>,
    pub label: Option<String>,
    pub unit: Option<String>,
    #[serde(default)]
    pub command_prefix: Vec<String>,
    #[serde(default)]
    pub log_paths: Vec<String>,
    #[serde(default = "system")]
    pub scope: String,
}

impl Service {
    pub fn managed(&self) -> bool {
        !self.observe_only
    }
    pub fn job(&self) -> bool {
        self.kind == "job"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

impl Project {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_id(&self.id),
            "Invalid project workspace id: {}",
            self.id
        );
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 160
                && !self.name.contains('\0')
                && self.description.len() <= 2048
                && !self.description.contains('\0'),
            "Project workspace needs a name (up to 160 bytes) and a description of up to 2048 bytes"
        );
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub engine: Engine,
    pub budget: Budget,
    #[serde(default)]
    pub allocation: AllocationPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<Project>,
    pub services: Vec<Service>,
}

fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.@-".contains(&c))
}

pub fn validate_command(args: &[String]) -> Result<()> {
    ensure!(
        !args.is_empty() && args.iter().all(|x| !x.is_empty() && !x.contains('\0')),
        "command must be a nonempty argument array without NUL bytes"
    );
    Ok(())
}

impl Manifest {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            version: 1,
            engine: Engine { command },
            budget: Budget {
                cpus: 3.,
                memory_mb: 4096,
                max_parallel_jobs: 1,
            },
            allocation: AllocationPolicy::default(),
            projects: vec![],
            services: vec![],
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        use std::io::Read;
        let file = fs::File::open(path)
            .with_context(|| format!("Cannot read {}. Run selfhost init first.", path.display()))?;
        let mut bytes = Vec::new();
        file.take(crate::manifest_io::MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        let manifest: Self = crate::manifest_io::decode(path, &bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "manifest version must be 1");
        validate_command(&self.engine.command)?;
        Limits {
            cpus: self.budget.cpus,
            memory_mb: self.budget.memory_mb,
        }
        .validate()?;
        ensure!(
            self.budget.max_parallel_jobs > 0,
            "max_parallel_jobs must be positive"
        );
        let mut ids = HashSet::new();
        let mut targets = HashSet::new();
        ensure!(
            self.projects.len() <= 128,
            "A controller supports up to 128 project workspaces"
        );
        let mut projects = HashSet::new();
        for project in &self.projects {
            project.validate()?;
            ensure!(
                projects.insert(&project.id),
                "Duplicate project workspace: {}",
                project.id
            );
        }
        for s in &self.services {
            ensure!(
                valid_id(&s.id) && ids.insert(&s.id),
                "invalid or duplicate id: {}",
                s.id
            );
            if let Some(project) = &s.project {
                ensure!(
                    projects.contains(project),
                    "{}: unknown project workspace: {}",
                    s.id,
                    project
                );
            }
            ensure!(
                matches!(s.runtime.as_str(), "docker" | "launchd" | "systemd"),
                "{}: unsupported runtime",
                s.id
            );
            ensure!(
                matches!(s.kind.as_str(), "daemon" | "job"),
                "{}: invalid kind",
                s.id
            );
            if s.runtime == "launchd" {
                ensure!(
                    s.observe_only,
                    "{}: launchd currently supports observation only; native macOS services have no hard cgroup limits",
                    s.id
                );
                ensure!(
                    s.label.as_deref().is_some_and(safe_name),
                    "{}: invalid launchd label",
                    s.id
                );
                ensure!(
                    targets.insert(format!("launchd:{}", s.label.as_deref().unwrap())),
                    "duplicate launchd label"
                );
            }
            if s.runtime == "systemd" {
                ensure!(
                    s.unit.as_deref().is_some_and(safe_name),
                    "{}: invalid systemd unit",
                    s.id
                );
                ensure!(
                    s.unit
                        .as_deref()
                        .is_some_and(|unit| unit.ends_with(".service")),
                    "{}: use an explicit .service unit name",
                    s.id
                );
                ensure!(
                    s.scope == "system" || s.scope == "user",
                    "scope must be system or user"
                );
                ensure!(
                    targets.insert(format!(
                        "systemd:{:?}:{}:{}",
                        s.command_prefix,
                        s.scope,
                        s.unit.as_deref().unwrap()
                    )),
                    "duplicate systemd unit"
                );
                ensure!(
                    !s.job(),
                    "systemd jobs should use systemd timers; scheduled jobs use the Docker adapter"
                );
            }
            if !s.command.is_empty() {
                validate_command(&s.command)?;
            }
            ensure!(
                s.working_dir.is_none() || s.runtime == "docker",
                "{}: working_dir is supported only for Docker services",
                s.id
            );
            ensure!(
                s.working_dir
                    .as_ref()
                    .is_none_or(|path| path.starts_with('/')
                        && path.len() <= 1024
                        && !path.contains('\0')),
                "{}: working_dir must be an absolute path up to 1024 bytes",
                s.id
            );
            if !s.command_prefix.is_empty() {
                validate_command(&s.command_prefix)?;
            }
            if s.runtime == "docker" && s.observe_only {
                ensure!(
                    s.container.as_deref().is_some_and(safe_name),
                    "observed Docker services require container"
                );
                ensure!(
                    targets.insert(format!("docker:{}", s.container.as_deref().unwrap())),
                    "duplicate container"
                );
            }
            if s.managed() {
                let limits = s
                    .limits
                    .as_ref()
                    .context("managed services require limits")?;
                limits.validate()?;
                ensure!(
                    s.allocation.weight.is_finite()
                        && s.allocation.weight > 0.
                        && s.allocation.weight <= 1_000_000.,
                    "{}: allocation weight must be between 0 and 1000000",
                    s.id
                );
                Limits {
                    cpus: s.allocation.min_cpus,
                    memory_mb: s.allocation.min_memory_mb,
                }
                .validate()?;
                if self.allocation.enabled && s.enabled {
                    ensure!(
                        s.allocation.min_cpus <= limits.cpus
                            && s.allocation.min_memory_mb <= limits.memory_mb,
                        "{}: allocation minimum exceeds its ceiling; lower the minimum or raise limits",
                        s.id
                    );
                }
                if s.enabled && !self.allocation.enabled {
                    ensure!(
                        limits.cpus <= self.budget.cpus
                            && limits.memory_mb <= self.budget.memory_mb,
                        "{} exceeds workspace budget",
                        s.id
                    );
                }
                if s.runtime == "docker" {
                    let image = s
                        .image
                        .as_deref()
                        .context("managed Docker services require image")?;
                    ensure!(
                        !image.is_empty()
                            && !image.starts_with('-')
                            && !image.contains(char::is_whitespace)
                            && !image.contains('\0'),
                        "invalid image"
                    );
                    if s.job() {
                        ensure!(
                            s.interval_seconds
                                .is_some_and(|n| (30..=31536000).contains(&n)),
                            "jobs require interval_seconds between 30 and 31536000"
                        );
                    }
                    if let Some(path) = &s.env_file {
                        ensure!(
                            path.starts_with('/') && !path.contains('\0'),
                            "env_file must be absolute on the Docker client machine"
                        );
                    }
                    for v in &s.volumes {
                        ensure!(
                            !v.no_copy || !v.source.starts_with('/'),
                            "volume no_copy is supported only for named volumes"
                        );
                        ensure!(
                            (v.source.starts_with('/') || safe_name(&v.source))
                                && v.target.starts_with('/')
                                && !v.source.contains([',', '\0'])
                                && !v.target.contains([',', '\0']),
                            "invalid volume source/target"
                        );
                    }
                    for port in &s.ports {
                        let parts: Vec<_> = port.split(':').collect();
                        ensure!(
                            parts.len() == 3
                                && parts[0] == "127.0.0.1"
                                && parts[1].parse::<u16>().is_ok_and(|p| p > 0),
                            "ports must use 127.0.0.1:HOST:CONTAINER"
                        );
                        let guest: Vec<_> = parts[2].split('/').collect();
                        ensure!(
                            guest.len() <= 2
                                && guest[0].parse::<u16>().is_ok_and(|p| p > 0)
                                && (guest.len() == 1 || matches!(guest[1], "tcp" | "udp")),
                            "invalid published port"
                        );
                    }
                }
            }
        }
        if self.allocation.enabled {
            let managed: Vec<_> = self
                .services
                .iter()
                .filter(|s| s.managed() && s.enabled)
                .collect();
            ensure!(
                managed.iter().map(|s| s.allocation.min_cpus).sum::<f64>()
                    <= self.budget.cpus + 1e-9
                    && managed
                        .iter()
                        .map(|s| s.allocation.min_memory_mb as u128)
                        .sum::<u128>()
                        <= self.budget.memory_mb as u128,
                "Allocation minimums exceed the workspace budget; raise the budget or lower service minimums"
            );
            return Ok(());
        }
        let daemons: Vec<_> = self
            .services
            .iter()
            .filter(|s| s.managed() && s.enabled && !s.job())
            .collect();
        let cpus: f64 = daemons
            .iter()
            .map(|s| s.limits.as_ref().unwrap().cpus)
            .sum();
        let memory: u64 = daemons
            .iter()
            .map(|s| s.limits.as_ref().unwrap().memory_mb)
            .sum();
        ensure!(
            cpus <= self.budget.cpus + 1e-9 && memory <= self.budget.memory_mb,
            "daemon reservations exceed workspace budget"
        );
        for job in self
            .services
            .iter()
            .filter(|s| s.managed() && s.enabled && s.job())
        {
            let l = job.limits.as_ref().unwrap();
            ensure!(
                cpus + l.cpus <= self.budget.cpus + 1e-9
                    && memory + l.memory_mb <= self.budget.memory_mb,
                "{} cannot fit after daemon reservations",
                job.id
            );
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        atomic_bytes(path, &crate::manifest_io::encode(path, self)?)
    }
}

pub fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    atomic_bytes(path, &bytes)
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // A stale temp file is left untouched rather than following a symlink.
    let mut file = options.open(&tmp).with_context(|| format!("Cannot create {}; remove a stale temp file after checking no other controller is running", tmp.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn ensure_under_budget(cpus: f64, memory: u64, budget: &Budget) -> Result<()> {
    if cpus > budget.cpus + 1e-9 || memory > budget.memory_mb {
        bail!("workspace capacity exhausted ({cpus:.2} CPUs, {memory} MiB required)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Manifest {
        serde_json::from_value(serde_json::json!({"version":1,"engine":{"command":["docker"]},"budget":{"cpus":2,"memory_mb":1024,"max_parallel_jobs":1},"services":[{"id":"web","runtime":"docker","observe_only":false,"image":"nginx:alpine","limits":{"cpus":1,"memory_mb":512}}]})).unwrap()
    }
    #[test]
    fn rejects_overcommit_and_bad_limits() {
        let mut m = sample();
        m.services[0].limits.as_mut().unwrap().cpus = 3.;
        assert!(m.validate().is_err());
        m.services[0].limits.as_mut().unwrap().cpus = f64::NAN;
        assert!(m.validate().is_err());
    }
    #[test]
    fn validates_and_prevents_public_port_and_duplicate_targets() {
        let mut m = sample();
        assert!(m.validate().is_ok());
        m.services[0].ports.push("0.0.0.0:80:80".into());
        assert!(m.validate().is_err());
        m.services[0].ports.clear();
        let mut clone = m.services[0].clone();
        clone.id = "worker".into();
        m.services.push(clone);
        assert!(m.validate().is_ok());
    }
    #[test]
    fn atomic_manifest_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("services.json");
        sample().save(&p).unwrap();
        assert_eq!(Manifest::load(&p).unwrap().services.len(), 1);
    }
}
