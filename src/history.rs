//! Persistent observed utilization. Missing measurements remain missing, including across restarts.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const RETENTION_SECONDS: u64 = 86_400;
pub const SAMPLE_INTERVAL_SECONDS: u64 = 15;
pub const BUCKET_SECONDS: u64 = 60;
const MAX_DATABASE_BYTES: u64 = 512 * 1024 * 1024;
const APPLICATION_ID: u32 = 0x53484948;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryQuery {
    pub project: Option<String>,
    pub service: Option<String>,
    pub window_seconds: u64,
    pub max_points: usize,
    pub end: Option<u64>,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            project: None,
            service: None,
            window_seconds: RETENTION_SECONDS,
            max_points: 720,
            end: None,
        }
    }
}

impl HistoryQuery {
    pub fn validate(&self) -> Result<()> {
        self.validate_at(now())
    }

    fn validate_at(&self, at: u64) -> Result<()> {
        ensure!(
            self.project.is_none() || self.service.is_none(),
            "Choose a project or a service, not both"
        );
        if let Some(project) = &self.project {
            ensure!(
                project == "__default__" || valid_id(project),
                "Invalid history project id"
            );
        }
        if let Some(service) = &self.service {
            ensure!(valid_id(service), "Invalid history service id");
        }
        ensure!(
            (60..=RETENTION_SECONDS).contains(&self.window_seconds),
            "History window must be between 60 and 86400 seconds"
        );
        ensure!(
            (1..=1440).contains(&self.max_points),
            "History max_points must be between 1 and 1440"
        );
        if let Some(end) = self.end {
            ensure!(
                end <= at && end > at.saturating_sub(RETENTION_SECONDS),
                "History end must be within the retained 24 hours and cannot be in the future"
            );
        }
        Ok(())
    }

    fn scope(&self) -> (&str, &str) {
        if let Some(id) = &self.service {
            ("service", id)
        } else if let Some(id) = &self.project {
            ("project", id)
        } else {
            ("all", "")
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HistoryPoint {
    /// Bucket boundary; first/last_sample_at are the actual measurement timestamps.
    pub sampled_at: u64,
    pub first_sample_at: Option<u64>,
    pub last_sample_at: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub memory_mb: Option<f64>,
    pub cpu_min: Option<f64>,
    pub cpu_max: Option<f64>,
    pub memory_min: Option<f64>,
    pub memory_max: Option<f64>,
    pub samples: u64,
    pub eligible_samples: u64,
    pub cpu_samples: u64,
    pub memory_samples: u64,
    pub cpu_complete: bool,
    pub memory_complete: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct HistoryCoverage {
    pub buckets: usize,
    pub observed_buckets: usize,
    pub cpu_buckets: usize,
    pub memory_buckets: usize,
    pub samples: u64,
    pub eligible_samples: u64,
    pub cpu_samples: u64,
    pub memory_samples: u64,
    pub first_sample_at: Option<u64>,
    pub last_sample_at: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub scope: String,
    pub id: Option<String>,
    pub start: u64,
    pub end: u64,
    pub retention_seconds: u64,
    pub sample_interval_seconds: u64,
    pub bucket_seconds: u64,
    pub aggregation: &'static str,
    pub membership: &'static str,
    pub cpu_basis: &'static str,
    pub memory_basis: &'static str,
    pub earliest_at: Option<u64>,
    pub latest_at: Option<u64>,
    pub status: &'static str,
    pub error: Option<String>,
    pub coverage: HistoryCoverage,
    pub points: Vec<HistoryPoint>,
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct Metric {
    sum: f64,
    count: u64,
    min: Option<f64>,
    max: Option<f64>,
}

impl Metric {
    fn add(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
        self.min = Some(self.min.map_or(value, |v| v.min(value)));
        self.max = Some(self.max.map_or(value, |v| v.max(value)));
    }
    fn merge(&mut self, other: &Self) {
        self.sum += other.sum;
        self.count += other.count;
        self.min = min_option(self.min, other.min);
        self.max = max_option(self.max, other.max);
    }
    fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

fn min_option<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a < b { a } else { b }),
        (a, b) => a.or(b),
    }
}
fn max_option<T: PartialOrd + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a > b { a } else { b }),
        (a, b) => a.or(b),
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
struct Bucket {
    samples: u64,
    eligible_samples: u64,
    cpu: Metric,
    memory: Metric,
    first_sample_at: Option<u64>,
    last_sample_at: Option<u64>,
}

impl Bucket {
    fn add(&mut self, sample: &Observation) {
        self.samples += 1;
        self.eligible_samples += u64::from(sample.eligible);
        if let Some(value) = sample.cpu {
            self.cpu.add(value);
        }
        if let Some(value) = sample.memory {
            self.memory.add(value);
        }
        self.first_sample_at = min_option(self.first_sample_at, sample.sampled_at);
        self.last_sample_at = max_option(self.last_sample_at, sample.sampled_at);
    }
    fn merge(&mut self, other: &Self) {
        self.samples += other.samples;
        self.eligible_samples += other.eligible_samples;
        self.cpu.merge(&other.cpu);
        self.memory.merge(&other.memory);
        self.first_sample_at = min_option(self.first_sample_at, other.first_sample_at);
        self.last_sample_at = max_option(self.last_sample_at, other.last_sample_at);
    }
    fn point(self, sampled_at: u64) -> HistoryPoint {
        HistoryPoint {
            sampled_at,
            first_sample_at: self.first_sample_at,
            last_sample_at: self.last_sample_at,
            cpu_percent: self.cpu.mean(),
            memory_mb: self.memory.mean(),
            cpu_min: self.cpu.min,
            cpu_max: self.cpu.max,
            memory_min: self.memory.min,
            memory_max: self.memory.max,
            samples: self.samples,
            eligible_samples: self.eligible_samples,
            cpu_samples: self.cpu.count,
            memory_samples: self.memory.count,
            cpu_complete: self.samples > 0 && self.cpu.count == self.samples,
            memory_complete: self.samples > 0 && self.memory.count == self.samples,
        }
    }
}

#[derive(Default, Clone)]
struct Observation {
    eligible: bool,
    cpu: Option<f64>,
    memory: Option<f64>,
    sampled_at: Option<u64>,
}

impl Observation {
    fn from_service(service: &Value, captured_at: u64) -> Self {
        let eligible = service["telemetry_eligible"].as_bool().unwrap_or(true);
        let sampled_at = service["sampled_at"]
            .as_u64()
            .filter(|at| *at > 0 && *at <= captured_at && captured_at - at <= 30);
        let metric = |key| {
            service[key]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0. && *v <= 1e15)
        };
        let cpu = (eligible && sampled_at.is_some())
            .then(|| metric("cpu_percent"))
            .flatten();
        let memory = (eligible && sampled_at.is_some())
            .then(|| metric("memory_mb"))
            .flatten();
        Self {
            eligible,
            cpu,
            memory,
            sampled_at: (cpu.is_some() || memory.is_some())
                .then_some(sampled_at)
                .flatten(),
        }
    }

    fn aggregate(targets: &BTreeMap<String, Self>) -> Self {
        let active: Vec<_> = targets.values().filter(|target| target.eligible).collect();
        let sum = |cpu| {
            let values: Option<Vec<f64>> = active
                .iter()
                .map(|s| if cpu { s.cpu } else { s.memory })
                .collect();
            values
                .filter(|v| !v.is_empty())
                .map(|v| v.iter().sum::<f64>())
                .filter(|v| v.is_finite())
        };
        let cpu = sum(true);
        let memory = sum(false);
        Self {
            eligible: !active.is_empty(),
            cpu,
            memory,
            sampled_at: (cpu.is_some() || memory.is_some())
                .then(|| active.iter().filter_map(|s| s.sampled_at).min())
                .flatten(),
        }
    }
}

/// Each operation opens a short-lived connection; no runtime-wide database lock is retained.
#[derive(Clone)]
pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    pub fn new(workspace: &Path) -> Self {
        Self {
            path: workspace.join("history.sqlite3"),
        }
    }

    pub fn open(workspace: &Path) -> Result<Self> {
        let store = Self::new(workspace);
        store.writer()?;
        Ok(store)
    }

    fn writer(&self) -> Result<Connection> {
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let created = match options.open(&self.path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error).context("Cannot create utilization history"),
        };
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .context("Cannot open utilization database")?;
        connection.busy_timeout(Duration::from_millis(250))?;
        if !created {
            Self::validate_database(&connection)?;
        }
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version <= 1,
            "Utilization history was created by a newer version; preserve it before recovery"
        );
        let page_size: u64 = connection.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let page_count: u64 = connection.pragma_query_value(None, "page_count", |r| r.get(0))?;
        ensure!(
            page_count <= MAX_DATABASE_BYTES / page_size,
            "Utilization database exceeds 512 MiB; preserve it before recovery"
        );
        connection.pragma_update(None, "max_page_count", MAX_DATABASE_BYTES / page_size)?;
        // DELETE mode avoids an unbounded WAL pinned by another process's reader.
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA journal_size_limit=0;
            CREATE TABLE IF NOT EXISTS history_buckets(scope TEXT NOT NULL,id TEXT NOT NULL,bucket INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(scope,id,bucket)) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS history_retention ON history_buckets(bucket);
            CREATE TABLE IF NOT EXISTS history_captures(at INTEGER PRIMARY KEY);
            PRAGMA user_version=1;")?;
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        Ok(connection)
    }

    fn validate_database(connection: &Connection) -> Result<()> {
        let application_id: u32 =
            connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
        ensure!(
            application_id == APPLICATION_ID,
            "history.sqlite3 is not a Self Host history database; preserve it and choose another workspace or move it before retrying"
        );
        let version: u32 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version == 1,
            "Unsupported utilization history schema; preserve the database before recovery"
        );
        connection
            .prepare("SELECT scope,id,bucket,data FROM history_buckets LIMIT 0")
            .context("Invalid utilization history schema; preserve the database before recovery")?;
        connection
            .prepare("SELECT at FROM history_captures LIMIT 0")
            .context("Invalid utilization history schema; preserve the database before recovery")?;
        Ok(())
    }

    fn reader(&self) -> Result<Option<Connection>> {
        match fs::metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            result => {
                result.context("Cannot read utilization history")?;
            }
        }
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        Self::validate_database(&connection)?;
        Ok(Some(connection))
    }

    pub fn capture(&self, snapshot: &Value) -> Result<bool> {
        let captured_at = snapshot["timestamp"]
            .as_u64()
            .context("History capture needs a timestamp")?;
        ensure!(
            captured_at > 0 && captured_at < i64::MAX as u64,
            "Invalid history capture timestamp"
        );
        let services = snapshot["services"]
            .as_array()
            .context("History capture needs service observations")?;
        ensure!(
            services.len() <= 4096,
            "History capture exceeds 4096 service observations"
        );
        let mut observations = vec![];
        let mut groups: BTreeMap<String, BTreeMap<String, Observation>> = BTreeMap::new();
        groups.insert(String::new(), BTreeMap::new());
        groups.insert("__default__".into(), BTreeMap::new());
        for project in snapshot["projects"].as_array().into_iter().flatten() {
            if let Some(id) = project["id"].as_str() {
                groups.entry(id.to_owned()).or_default();
            }
        }
        for service in services {
            let id = service["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .context("Invalid history service id")?;
            let sample = Observation::from_service(service, captured_at);
            observations.push(("service", id.to_owned(), sample.clone()));
            let project = service["project"].as_str().unwrap_or("__default__");
            let target = service["telemetry_target"].as_str().unwrap_or(id);
            for group in ["", project] {
                let entry = groups
                    .entry(group.to_owned())
                    .or_default()
                    .entry(target.to_owned())
                    .or_default();
                entry.eligible |= sample.eligible;
                entry.cpu = entry.cpu.or(sample.cpu);
                entry.memory = entry.memory.or(sample.memory);
                entry.sampled_at = min_option(entry.sampled_at, sample.sampled_at);
            }
        }
        for (id, targets) in groups {
            observations.push((
                if id.is_empty() { "all" } else { "project" },
                id,
                Observation::aggregate(&targets),
            ));
        }
        let mut connection = self.writer()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let latest: Option<u64> =
            transaction.query_row("SELECT MAX(at) FROM history_captures", [], |r| r.get(0))?;
        if latest.is_some_and(|at| captured_at <= at) {
            return Ok(false);
        }
        let cutoff = captured_at.saturating_sub(RETENTION_SECONDS);
        transaction.execute(
            "DELETE FROM history_buckets WHERE bucket < ?1",
            [cutoff.div_ceil(BUCKET_SECONDS) * BUCKET_SECONDS],
        )?;
        transaction.execute("DELETE FROM history_captures WHERE at < ?1", [cutoff])?;
        transaction.execute("INSERT INTO history_captures(at) VALUES(?1)", [captured_at])?;
        let bucket_at = captured_at / BUCKET_SECONDS * BUCKET_SECONDS;
        {
            let mut read = transaction.prepare_cached(
                "SELECT data FROM history_buckets WHERE scope=?1 AND id=?2 AND bucket=?3",
            )?;
            let mut write = transaction.prepare_cached("INSERT INTO history_buckets(scope,id,bucket,data) VALUES(?1,?2,?3,?4) ON CONFLICT(scope,id,bucket) DO UPDATE SET data=excluded.data")?;
            // Only the current minute's small records are rewritten, never the retained history.
            for (scope, id, sample) in observations {
                let previous: Option<String> = read
                    .query_row(params![scope, id, bucket_at], |r| r.get(0))
                    .optional()?;
                let mut bucket: Bucket = previous
                    .map(|data| serde_json::from_str(&data))
                    .transpose()?
                    .unwrap_or_default();
                bucket.add(&sample);
                write.execute(params![
                    scope,
                    id,
                    bucket_at,
                    serde_json::to_string(&bucket)?
                ])?;
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn query(&self, query: HistoryQuery) -> Result<HistoryResponse> {
        self.query_at(query, now())
    }

    fn query_at(&self, query: HistoryQuery, at: u64) -> Result<HistoryResponse> {
        query.validate_at(at)?;
        let (scope, id) = query.scope();
        let end = query.end.unwrap_or(at);
        // Exclude a partial leading minute rather than include observations outside the window.
        let start = end
            .saturating_add(1)
            .saturating_sub(query.window_seconds)
            .max(at.saturating_sub(RETENTION_SECONDS))
            .div_ceil(BUCKET_SECONDS)
            * BUCKET_SECONDS;
        let bucket_seconds = (end
            .saturating_add(1)
            .saturating_sub(start)
            .div_ceil(BUCKET_SECONDS) as usize)
            .div_ceil(query.max_points)
            .max(1) as u64
            * BUCKET_SECONDS;
        let mut buckets = BTreeMap::<u64, Bucket>::new();
        let (mut earliest_at, mut latest_at) = (None, None);
        if let Some(connection) = self.reader()? {
            let cutoff = at
                .saturating_sub(RETENTION_SECONDS)
                .div_ceil(BUCKET_SECONDS)
                * BUCKET_SECONDS;
            let mut statement = connection.prepare("SELECT bucket,data FROM history_buckets WHERE scope=?1 AND id=?2 AND bucket>=?3 AND bucket<=?4 ORDER BY bucket")?;
            let rows = statement.query_map(params![scope, id, cutoff, end], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (bucket_at, data) = row?;
                let bucket: Bucket = serde_json::from_str(&data).context(
                    "Invalid utilization history bucket; preserve the database before recovery",
                )?;
                // A historical end cannot accurately split a minute that contains newer samples.
                if bucket.last_sample_at.is_some_and(|last| last > end) {
                    continue;
                }
                earliest_at = min_option(earliest_at, bucket.first_sample_at);
                latest_at = max_option(latest_at, bucket.last_sample_at);
                if bucket_at >= start {
                    let key = start + (bucket_at - start) / bucket_seconds * bucket_seconds;
                    buckets.entry(key).or_default().merge(&bucket);
                }
            }
        }
        let mut points = vec![];
        let mut coverage = HistoryCoverage::default();
        for timestamp in (start..=end).step_by(bucket_seconds as usize) {
            let point = buckets
                .remove(&timestamp)
                .unwrap_or_default()
                .point(timestamp);
            coverage.observed_buckets += usize::from(point.samples > 0);
            coverage.cpu_buckets += usize::from(point.cpu_samples > 0);
            coverage.memory_buckets += usize::from(point.memory_samples > 0);
            coverage.samples += point.samples;
            coverage.eligible_samples += point.eligible_samples;
            coverage.cpu_samples += point.cpu_samples;
            coverage.memory_samples += point.memory_samples;
            coverage.first_sample_at = min_option(coverage.first_sample_at, point.first_sample_at);
            coverage.last_sample_at = max_option(coverage.last_sample_at, point.last_sample_at);
            points.push(point);
        }
        coverage.buckets = points.len();
        Ok(HistoryResponse {
            scope: scope.to_owned(),
            id: (!id.is_empty()).then(|| id.to_owned()),
            start,
            end,
            retention_seconds: RETENTION_SECONDS,
            sample_interval_seconds: SAMPLE_INTERVAL_SECONDS,
            bucket_seconds,
            aggregation: "Arithmetic mean of actual valid captures; min/max are observed extremes. Missing buckets are null. Aggregate captures require every eligible unique target to report the metric.",
            membership: "Project membership at capture time; historical services are not reassigned retrospectively.",
            cpu_basis: "100% = one logical CPU",
            memory_basis: "Observed service memory in MiB; not whole-host memory",
            earliest_at,
            latest_at,
            status: if latest_at.is_some() {
                "collecting"
            } else {
                "empty"
            },
            error: None,
            coverage,
            points,
        })
    }

    pub fn status(&self) -> Result<Value> {
        let mut earliest_at: Option<u64> = None;
        let mut latest_at: Option<u64> = None;
        let at = now();
        if let Some(connection) = self.reader()? {
            (earliest_at, latest_at) = connection.query_row(
                "SELECT MIN(at),MAX(at) FROM history_captures WHERE at>=?1 AND at<=?2",
                params![at.saturating_sub(RETENTION_SECONDS), at],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        }
        Ok(
            json!({"status":if latest_at.is_some(){"collecting"}else{"empty"},"error":null,"retention_seconds":RETENTION_SECONDS,"sample_interval_seconds":SAMPLE_INTERVAL_SECONDS,"bucket_seconds":BUCKET_SECONDS,"earliest_at":earliest_at,"latest_at":latest_at,"max_database_bytes":MAX_DATABASE_BYTES,"persistent":true}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 1_800_000_000;

    fn service(
        id: &str,
        project: Option<&str>,
        at: u64,
        cpu: Option<f64>,
        memory: Option<f64>,
    ) -> Value {
        json!({"id":id,"project":project,"telemetry_target":id,"telemetry_eligible":true,
            "sampled_at":at,"cpu_percent":cpu,"memory_mb":memory})
    }

    fn capture(store: &HistoryStore, at: u64, services: Vec<Value>) -> bool {
        store.capture(&json!({"timestamp":at,"services":services,"projects":[{"id":"tools"},{"id":"other"}]})).unwrap()
    }

    fn query(
        store: &HistoryStore,
        service: Option<&str>,
        project: Option<&str>,
        at: u64,
    ) -> HistoryResponse {
        store
            .query_at(
                HistoryQuery {
                    service: service.map(str::to_owned),
                    project: project.map(str::to_owned),
                    window_seconds: 300,
                    max_points: 1440,
                    ..Default::default()
                },
                at,
            )
            .unwrap()
    }

    #[test]
    fn persisted_means_extremes_and_gaps_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        assert!(capture(
            &store,
            BASE + 1,
            vec![service("worker", None, BASE + 1, Some(10.), Some(32.))]
        ));
        assert!(capture(
            &store,
            BASE + 16,
            vec![service("worker", None, BASE + 16, Some(30.), Some(64.))]
        ));
        drop(store);
        let store = HistoryStore::open(dir.path()).unwrap();
        capture(
            &store,
            BASE + 181,
            vec![service("worker", None, BASE + 181, None, Some(128.))],
        );
        let result = query(&store, Some("worker"), None, BASE + 240);
        assert_eq!(result.points.len(), 5);
        let first = &result.points[0];
        assert_eq!(first.sampled_at, BASE);
        assert_eq!(first.cpu_percent, Some(20.));
        assert_eq!((first.cpu_min, first.cpu_max), (Some(10.), Some(30.)));
        assert_eq!(first.memory_mb, Some(48.));
        assert_eq!(
            (first.first_sample_at, first.last_sample_at),
            (Some(BASE + 1), Some(BASE + 16))
        );
        assert_eq!((first.samples, first.cpu_samples), (2, 2));
        assert!(first.cpu_complete);
        let gap = &result.points[1];
        assert_eq!((gap.cpu_percent, gap.memory_mb), (None, None));
        assert_eq!((gap.first_sample_at, gap.last_sample_at), (None, None));
        assert_eq!(gap.samples, 0);
        let memory_only = &result.points[3];
        assert_eq!(
            (memory_only.cpu_percent, memory_only.memory_mb),
            (None, Some(128.))
        );
        assert!(!memory_only.cpu_complete);
        assert!(memory_only.memory_complete);
        assert_eq!(
            (result.earliest_at, result.latest_at),
            (Some(BASE + 1), Some(BASE + 181))
        );
        assert_eq!(
            (
                result.coverage.samples,
                result.coverage.cpu_samples,
                result.coverage.memory_samples
            ),
            (3, 2, 3)
        );
    }

    #[test]
    fn aggregate_requires_full_coverage_deduplicates_targets_and_preserves_membership() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let a = service("worker", Some("tools"), BASE + 1, Some(20.), Some(10.));
        let mut alias = a.clone();
        alias["id"] = json!("alias");
        let missing = service("unknown", None, BASE + 1, None, Some(2.));
        capture(&store, BASE + 1, vec![a.clone(), alias, missing]);
        let all = query(&store, None, None, BASE + 1);
        let point = all.points.last().unwrap();
        assert_eq!(point.cpu_percent, None);
        assert_eq!(point.memory_mb, Some(12.));
        assert_eq!((point.cpu_samples, point.memory_samples), (0, 1));
        let tools = query(&store, None, Some("tools"), BASE + 1);
        assert_eq!(tools.points.last().unwrap().cpu_percent, Some(20.));
        let mut moved = a;
        moved["project"] = json!("other");
        moved["sampled_at"] = json!(BASE + 61);
        capture(&store, BASE + 61, vec![moved]);
        let tools = query(&store, None, Some("tools"), BASE + 61);
        assert_eq!(tools.points[3].cpu_percent, Some(20.));
        assert_eq!(tools.points[4].cpu_percent, None);
        let other = query(&store, None, Some("other"), BASE + 61);
        assert_eq!(other.points[3].cpu_percent, None);
        assert_eq!(other.points[4].cpu_percent, Some(20.));
        assert_eq!(
            query(&store, Some("worker"), None, BASE + 61)
                .coverage
                .cpu_samples,
            2
        );
    }

    #[test]
    fn actual_zero_is_kept_while_stale_inactive_and_invalid_values_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        capture(
            &store,
            BASE + 1,
            vec![service("worker", None, BASE + 1, Some(0.), Some(0.))],
        );
        assert_eq!(
            query(&store, Some("worker"), None, BASE + 1)
                .points
                .last()
                .unwrap()
                .cpu_percent,
            Some(0.)
        );
        capture(
            &store,
            BASE + 61,
            vec![service("worker", None, BASE + 1, Some(20.), Some(32.))],
        );
        let mut inactive = service("worker", None, BASE + 121, Some(0.), Some(0.));
        inactive["telemetry_eligible"] = json!(false);
        capture(&store, BASE + 121, vec![inactive]);
        capture(
            &store,
            BASE + 181,
            vec![service("worker", None, BASE + 181, Some(-1.), Some(-1.))],
        );
        let result = query(&store, Some("worker"), None, BASE + 181);
        for point in &result.points[2..] {
            assert_eq!((point.cpu_percent, point.memory_mb), (None, None));
            assert_eq!((point.first_sample_at, point.last_sample_at), (None, None));
        }
    }

    #[test]
    fn duplicate_and_older_captures_cannot_double_count_or_rewind_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let original = service("worker", None, BASE + 16, Some(10.), Some(32.));
        assert!(capture(&store, BASE + 16, vec![original.clone()]));
        assert!(!capture(&store, BASE + 16, vec![original]));
        assert!(!capture(
            &store,
            BASE + 1,
            vec![service("worker", None, BASE + 1, Some(999.), Some(999.))]
        ));
        let result = query(&store, Some("worker"), None, BASE + 16);
        assert_eq!(result.coverage.samples, 1);
        assert_eq!(result.points.last().unwrap().cpu_percent, Some(10.));
    }

    #[test]
    fn retention_prunes_old_rows_and_rollups_stay_within_requested_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        capture(
            &store,
            BASE + 1,
            vec![service("old", None, BASE + 1, Some(1.), Some(1.))],
        );
        // Reads hide expired data even when the daemon has been stopped and cannot prune.
        let expired = store
            .query_at(HistoryQuery::default(), BASE + RETENTION_SECONDS + 61)
            .unwrap();
        assert_eq!(expired.coverage.samples, 0);
        capture(
            &store,
            BASE + RETENTION_SECONDS + 61,
            vec![service(
                "new",
                None,
                BASE + RETENTION_SECONDS + 61,
                Some(2.),
                Some(2.),
            )],
        );
        let connection = store.reader().unwrap().unwrap();
        let old_rows: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_buckets WHERE id='old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_rows, 0);
        drop(connection);
        let result = store
            .query_at(HistoryQuery::default(), BASE + RETENTION_SECONDS + 61)
            .unwrap();
        assert_eq!(result.bucket_seconds, 120);
        assert_eq!(result.points.len(), 720);
        assert_eq!(result.coverage.cpu_samples, 1);
        assert!(
            result
                .points
                .iter()
                .all(|p| p.first_sample_at.is_none_or(|at| at > BASE + 61))
        );
        let compact = store
            .query_at(
                HistoryQuery {
                    max_points: 7,
                    ..Default::default()
                },
                BASE + RETENTION_SECONDS + 61,
            )
            .unwrap();
        assert!(compact.points.len() <= 7);
        assert_eq!(compact.coverage.cpu_samples, 1);
    }

    #[test]
    fn queries_are_bounded_and_read_only_before_collection_starts() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let result = store.query_at(HistoryQuery::default(), BASE).unwrap();
        assert_eq!(result.points.len(), 720);
        assert_eq!(result.status, "empty");
        assert!(
            result
                .points
                .iter()
                .all(|p| p.cpu_percent.is_none() && p.first_sample_at.is_none())
        );
        assert!(!store.path.exists());
        for query in [
            HistoryQuery {
                window_seconds: RETENTION_SECONDS + 1,
                ..Default::default()
            },
            HistoryQuery {
                max_points: 0,
                ..Default::default()
            },
            HistoryQuery {
                max_points: 1441,
                ..Default::default()
            },
            HistoryQuery {
                project: Some("tools".into()),
                service: Some("worker".into()),
                ..Default::default()
            },
            HistoryQuery {
                service: Some("../worker".into()),
                ..Default::default()
            },
            HistoryQuery {
                end: Some(BASE + 1),
                ..Default::default()
            },
        ] {
            assert!(store.query_at(query, BASE).is_err());
        }
        assert!(!store.path.exists());
    }

    #[test]
    fn unrelated_database_is_never_claimed_or_changed() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let connection = Connection::open(&store.path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE valuable(data TEXT); INSERT INTO valuable VALUES('retained');",
            )
            .unwrap();
        drop(connection);
        let before = fs::read(&store.path).unwrap();
        assert!(HistoryStore::open(dir.path()).is_err());
        assert!(store.status().is_err());
        assert!(
            store
                .capture(&json!({"timestamp":BASE,"services":[]}))
                .is_err()
        );
        assert_eq!(fs::read(&store.path).unwrap(), before);
    }

    #[test]
    fn matching_application_id_with_wrong_schema_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        let connection = Connection::open(&store.path).unwrap();
        connection
            .pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE valuable(data TEXT); INSERT INTO valuable VALUES('retained');",
            )
            .unwrap();
        drop(connection);
        let before = fs::read(&store.path).unwrap();
        let error = store
            .capture(&json!({"timestamp":BASE,"services":[]}))
            .unwrap_err();
        assert!(format!("{error:#}").contains("Invalid utilization history schema"));
        assert_eq!(fs::read(&store.path).unwrap(), before);
    }

    #[test]
    fn database_growth_is_capped_and_no_wal_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(dir.path()).unwrap();
        capture(
            &store,
            BASE + 1,
            vec![service("worker", None, BASE + 1, Some(1.), Some(2.))],
        );
        let connection = store.writer().unwrap();
        let pages: u64 = connection
            .pragma_query_value(None, "max_page_count", |r| r.get(0))
            .unwrap();
        let page_size: u64 = connection
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .unwrap();
        let journal: String = connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(pages * page_size, MAX_DATABASE_BYTES);
        assert_eq!(journal, "delete");
        drop(connection);
        assert!(!dir.path().join("history.sqlite3-wal").exists());
        assert!(!dir.path().join("history.sqlite3-journal").exists());
        assert!(fs::metadata(&store.path).unwrap().len() <= MAX_DATABASE_BYTES);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&store.path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[tokio::test]
    async fn history_failure_does_not_prevent_controller_start_or_live_status() {
        use crate::{config::Manifest, control::Controller};
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("services.yaml");
        Manifest::new(vec!["missing-history-test-engine".into()])
            .save(&config)
            .unwrap();
        fs::write(dir.path().join("history.sqlite3"), "unrelated file").unwrap();
        let mut controller = Controller::new(dir.path().into(), config).unwrap();
        assert_eq!(controller.history_status()["status"], "error");
        assert!(controller.capture_history().await.is_err());
        assert!(controller.history_status()["error"].is_string());
        assert!(controller.history(HistoryQuery::default()).is_err());
        assert!(
            controller.snapshot().await["services"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("history.sqlite3")).unwrap(),
            "unrelated file"
        );
    }
}
