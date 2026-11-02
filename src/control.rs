use crate::{
    allocation::{self, Allocation, AllocationInput},
    config::{self, Limits, Manifest, Service},
    github::GithubSource,
    monitor::HostMonitor,
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

struct CpuSample {
    identity: String,
    captured_at: Instant,
    counters: HashMap<u64, u64>,
}

fn cpu_delta_percent(
    previous: &HashMap<u64, u64>,
    current: &HashMap<u64, u64>,
    seconds: f64,
) -> Option<f64> {
    if current.is_empty() || !seconds.is_finite() || seconds <= 0. {
        return None;
    }
    let mut delta = 0_u64;
    for (pid, usage) in current {
        // New or restarted processes need a baseline before reporting CPU usage.
        delta = delta.checked_add(usage.checked_sub(*previous.get(pid)?)?)?;
    }
    Some(delta as f64 / 1e9 / seconds * 100.)
}

fn telemetry_eligible(service: &Value) -> bool {
    if service["runtime_active"] == true
        || matches!(
            service["active_state"].as_str(),
            Some("active" | "activating" | "deactivating" | "reloading")
        )
        || ["cpu_percent", "memory_mb", "process_count", "task_count"]
            .iter()
            .any(|field| service[*field].as_f64().is_some_and(|value| value > 0.))
    {
        return true;
    }
    if service["runtime_active"] == false {
        return false;
    }
    !matches!(
        service["status"].as_str(),
        Some("not created" | "created" | "stopped" | "exited" | "dead")
    )
}

fn monitored_usage(services: &[Value], target_keys: &[String]) -> Value {
    let mut seen = HashSet::new();
    let mut cpu_seen = HashSet::new();
    let mut memory_seen = HashSet::new();
    let mut cpu = 0.;
    let mut memory = 0.;
    let mut cpu_targets = 0;
    let mut memory_targets = 0;
    for (service, key) in services.iter().zip(target_keys) {
        seen.insert(key);
        if let Some(value) = service["cpu_percent"]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.)
            && cpu_seen.insert(key)
        {
            cpu += value;
            cpu_targets += 1;
        }
        if let Some(value) = service["memory_mb"]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.)
            && memory_seen.insert(key)
        {
            memory += value;
            memory_targets += 1;
        }
    }
    let measured = |field: &str, eligible_only: bool| {
        services
            .iter()
            .filter(|service| {
                (!eligible_only || telemetry_eligible(service))
                    && service[field]
                        .as_f64()
                        .is_some_and(|value| value.is_finite() && value >= 0.)
            })
            .count()
    };
    let eligible = services
        .iter()
        .filter(|service| telemetry_eligible(service))
        .count();
    let cpu_eligible = measured("cpu_percent", true);
    let memory_eligible = measured("memory_mb", true);
    let sampled_at = services
        .iter()
        .filter_map(|service| service["sampled_at"].as_u64())
        .min();
    json!({"scope":"monitored services","sampled_at":sampled_at,"cpu_percent":(cpu_targets>0).then_some(cpu),"cpu_cores":(cpu_targets>0).then_some(cpu/100.),"memory_mb":(memory_targets>0).then_some(memory),"cpu_measured_services":measured("cpu_percent",false),"memory_measured_services":measured("memory_mb",false),"service_count":services.len(),"eligible_service_count":eligible,"cpu_measured_eligible_services":cpu_eligible,"memory_measured_eligible_services":memory_eligible,"cpu_complete":eligible>0 && cpu_eligible==eligible,"memory_complete":eligible>0 && memory_eligible==eligible,"unique_targets":seen.len(),"cpu_basis":"100% = one logical CPU","memory_basis":"Observed service memory; not total host memory","memory_unit":"MiB"})
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
    fn deployment_fixture() -> (tempfile::TempDir, Controller) {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("engine.sh");
        fs::write(&script,r#"dir=$1; shift
printf '%s\n' "$*" >> "$dir/calls"
resolve() {
  if [ -f "$dir/$1.spec" ]; then printf '%s' "$1"; return; fi
  for file in "$dir"/*.id; do
    [ -f "$file" ] || continue
    if [ "$(cat "$file")" = "$1" ]; then key=${file##*/}; printf '%s' "${key%.id}"; return; fi
  done
  printf '%s' "$1"
}
render() {
  { read name; read cpu; read memory; read owner; read service; read spec; } < "$dir/$1.spec"
  nano=$(awk -v n="$cpu" 'BEGIN {printf "%.0f",n*1000000000}')
  running=false; state=created; restarting=false
  [ ! -f "$dir/$name.restarting" ] || restarting=true
  if [ -f "$dir/$name.running" ]; then running=true; state=running; fi
  printf '{"Id":"%s","Name":"/%s","Config":{"Labels":{"io.self-host-init.workspace":"%s","io.self-host-init.service":"%s","io.self-host-init.spec":"%s"}},"State":{"Running":%s,"Status":"%s","Restarting":%s},"HostConfig":{"NanoCpus":%s,"Memory":%s}}\n' "$(cat "$dir/$name.id")" "$name" "$owner" "$service" "$spec" "$running" "$state" "$restarting" "$nano" "$((memory*1048576))"
}
case "$1 $2" in
  'info --format') printf '%s\n' '{"NCPU":8,"MemTotal":8589934592}' ;;
  'image inspect') for image do :; done
    if [ "$image" = 'bad@image' ]; then echo 'invalid reference format' >&2; exit 1; fi
    printf '%s\n' '"cached-image"' ;;
  'ps -a') for file in "$dir"/*.spec; do
    [ -f "$file" ] || continue
    name=${file##*/}; name=${name%.spec}; state=exited
    [ ! -f "$dir/$name.running" ] || state=running
    printf '{"Names":"%s","State":"%s"}\n' "$name" "$state"
    done ;;
  'container inspect') for name do :; done
    name=$(resolve "$name")
    if [ ! -f "$dir/$name.spec" ]; then echo 'No such container' >&2; exit 1; fi
    if [ "$3" = --format ]; then render "$name"; else printf '['; render "$name"; printf ']'; fi ;;
  'container rm') name=$(resolve "$3")
    rm -f "$dir/$name.spec" "$dir/$name.id" "$dir/$name.running" "$dir/$name.restarting"
    printf 'remove %s\n' "$name" >> "$dir/mutations" ;;
  'network inspect') for network do :; done
    case "$network" in
      *-project-build-tools) printf '{"io.self-host-init.workspace":"%s","io.self-host-init.project":"build-tools"}\n' "$(cat "$dir/owner")" ;;
      *) printf '{"io.self-host-init.workspace":"%s"}\n' "$(cat "$dir/owner")" ;;
    esac ;;
  'volume inspect') for name do :; done
    if [ ! -f "$dir/$name.volume" ]; then echo 'No such volume' >&2; exit 1; fi
    cat "$dir/$name.volume" ;;
  'volume create')
    for name do :; done
    printf '{"io.self-host-init.workspace":"%s","io.self-host-init.service":"%s","io.self-host-init.purpose":"workspace"}\n' "$(cat "$dir/owner")" "$(cat "$dir/workspace-service")" > "$dir/$name.volume"
    printf 'volume %s\n' "$name" >> "$dir/mutations" ;;
  'stats --no-stream') shift; shift; shift; shift
    for name do printf '{"Name":"%s","CPUPerc":"1%%","MemUsage":"1MiB / 1024MiB"}\n' "$name"; done ;;
  *) case "$1" in
    rename)
      name=$(resolve "$2"); next=$3
      sed "1s/.*/$next/" "$dir/$name.spec" > "$dir/$next.spec"
      rm "$dir/$name.spec"
      mv "$dir/$name.id" "$dir/$next.id"
      if [ -f "$dir/$name.running" ]; then mv "$dir/$name.running" "$dir/$next.running"; fi
      printf 'rename %s %s\n' "$name" "$next" >> "$dir/mutations" ;;
    build|pull)
      for input do :; done
      if [ "$1" = build ] && [ "$input" = - ]; then
        cat > "$dir/build-input.tar"
        tar -xOf "$dir/build-input.tar" ./Dockerfile > "$dir/build-dockerfile" || exit 1
      fi
      if [ -f "$dir/fail-build" ]; then echo 'build failed' >&2; exit 1; fi
      printf 'image prepared\n' >> "$dir/mutations" ;;
    create)
      shift
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --name) name=$2; shift; shift ;;
          --cpus) cpu=$2; shift; shift ;;
          --memory) memory=${2%m}; shift; shift ;;
          --label) case "$2" in
            io.self-host-init.workspace=*) owner=${2#*=} ;;
            io.self-host-init.service=*) service=${2#*=} ;;
            io.self-host-init.spec=*) spec=${2#*=} ;;
            esac; shift; shift ;;
          --init) shift ;;
          --*) shift; shift ;;
          *) break ;;
        esac
      done
      printf '%s\n' "$name" "$cpu" "$memory" "$owner" "$service" "$spec" > "$dir/$name.spec"
      if [ -f "$dir/fail-next-restart" ]; then rm "$dir/fail-next-restart"; touch "$dir/$name.restarting"; fi
      count=0; [ ! -f "$dir/sequence" ] || count=$(cat "$dir/sequence")
      count=$((count+1)); printf '%s' "$count" > "$dir/sequence"
      printf '%s-id-%s' "$name" "$count" > "$dir/$name.id"
      printf 'create %s\n' "$name" >> "$dir/mutations" ;;
    start)
      name=$(resolve "$2")
      if [ -f "$dir/fail-next-start" ]; then rm "$dir/fail-next-start"; echo 'start failed once' >&2; exit 7; fi
      if [ -f "$dir/fail-start" ]; then echo 'start failed' >&2; exit 7; fi
      touch "$dir/$name.running"
      if [ -f "$dir/fail-config" ]; then touch "$dir/services.tmp"; fi
      printf 'start %s\n' "$name" >> "$dir/mutations" ;;
    update)
      shift
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --cpus) next_cpu=$2; shift; shift ;;
          --memory) next_memory=${2%m}; shift; shift ;;
          --*) shift; shift ;;
          *) name=$1; shift ;;
        esac
      done
      name=$(resolve "$name")
      render "$name" > /dev/null
      printf '%s\n' "$name" "$next_cpu" "$next_memory" "$owner" "$service" "$spec" > "$dir/$name.spec"
      printf 'update %s\n' "$name" >> "$dir/mutations" ;;
    stop)
      for name do :; done
      name=$(resolve "$name")
      rm -f "$dir/$name.running"
      printf 'stop %s\n' "$name" >> "$dir/mutations" ;;
    *) exit 1 ;;
  esac ;;
esac
"#).unwrap();
        let mut manifest = Manifest::new(vec![
            "/bin/sh".into(),
            script.display().to_string(),
            dir.path().display().to_string(),
        ]);
        manifest.budget.cpus = 2.;
        manifest.budget.memory_mb = 1024;
        let config = dir.path().join("services.json");
        manifest.save(&config).unwrap();
        let controller = Controller::new(dir.path().into(), config).unwrap();
        fs::write(dir.path().join("owner"), &controller.docker.owner).unwrap();
        (dir, controller)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_deployment_previews_and_replaces_without_changing_limits() {
        let (dir, mut c) = deployment_fixture();
        c.deploy(deployment("web", true)).await.unwrap();
        let before = c
            .docker
            .inspect(&c.service("web").unwrap())
            .await
            .unwrap()
            .unwrap();
        let calls = fs::read_to_string(dir.path().join("mutations")).unwrap();
        let config = fs::read(&c.config_path).unwrap();
        let preview = c
            .deploy_image("web", "example:new", false, Some(dir.path()), false)
            .await
            .unwrap();
        assert_eq!(preview["applied"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("mutations")).unwrap(),
            calls
        );
        assert_eq!(fs::read(&c.config_path).unwrap(), config);
        c.deploy_image("web", "example:new", true, Some(dir.path()), false)
            .await
            .unwrap();
        assert_eq!(
            c.service("web").unwrap().image.as_deref(),
            Some("example:new")
        );
        let after = c
            .docker
            .inspect(&c.service("web").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(before["Id"], after["Id"]);
        assert_eq!(before["HostConfig"], after["HostConfig"]);
        assert_eq!(after["State"]["Running"], true);
        assert!(
            !dir.path()
                .join(format!(
                    "{}-previous.spec",
                    c.docker.name(&c.service("web").unwrap())
                ))
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_deployment_restores_original_on_start_or_manifest_failure() {
        for failure in ["fail-next-start", "fail-next-restart", "fail-config"] {
            let (dir, mut c) = deployment_fixture();
            c.deploy(deployment("web", true)).await.unwrap();
            let original = c.service("web").unwrap();
            let before = c.docker.inspect(&original).await.unwrap().unwrap();
            let config = fs::read(&c.config_path).unwrap();
            fs::write(dir.path().join(failure), "").unwrap();
            let error = c
                .deploy_image("web", "example:new", true, None, false)
                .await
                .unwrap_err();
            assert!(
                format!("{error:#}").contains("original container and configuration restored"),
                "{error:#}"
            );
            let after = c.docker.inspect(&original).await.unwrap().unwrap();
            assert_eq!(before["Id"], after["Id"]);
            assert_eq!(after["State"]["Running"], true);
            assert_eq!(fs::read(&c.config_path).unwrap(), config);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_build_failure_leaves_running_service_untouched() {
        let (dir, mut c) = deployment_fixture();
        c.deploy(deployment("web", true)).await.unwrap();
        let original = c.service("web").unwrap();
        let before = c.docker.inspect(&original).await.unwrap().unwrap();
        fs::write(dir.path().join("fail-build"), "").unwrap();
        assert!(
            c.deploy_image("web", "example:new", true, Some(dir.path()), false)
                .await
                .is_err()
        );
        assert_eq!(c.docker.inspect(&original).await.unwrap().unwrap(), before);
        assert!(
            c.deploy_image("web", "-bad", true, None, false)
                .await
                .is_err()
        );
    }

    fn deployment(id: &str, apply: bool) -> DeploymentRequest {
        serde_json::from_value(json!({"apply":apply,"service":{"id":id,"name":"Custom service","image":"example/custom:tag","limits":{"cpus":0.5,"memory_mb":128}}})).unwrap()
    }

    #[cfg(unix)]
    fn github_deployment_fixture() -> (tempfile::TempDir, Controller, Vec<String>) {
        let (dir, controller) = deployment_fixture();
        let script = dir.path().join("git.sh");
        fs::write(
            &script,
            r#"dir=$1; shift
printf '%s\n' "$*" > "$dir/git-calls"
[ "$GIT_TERMINAL_PROMPT" = 0 ] || exit 8
[ "$GIT_CONFIG_NOSYSTEM" = 1 ] || exit 9
if [ -f "$dir/fail-clone" ]; then echo 'clone failed' >&2; exit 1; fi
for checkout do :; done
mkdir -p "$checkout/services/api"
mkdir -p "$checkout/services/api/.git"
printf 'public clone metadata\n' > "$checkout/services/api/.git/config"
printf 'FROM scratch\n' > "$checkout/services/api/Dockerfile"
printf 'ignored.txt\n' > "$checkout/services/api/.dockerignore"
printf 'only inside the archive\n' > "$checkout/services/api/ignored.txt"
if [ -f "$dir/fail-tar" ]; then ln -s "$checkout" "$checkout.tar"; fi
"#,
        )
        .unwrap();
        let git = vec![
            "/bin/sh".into(),
            script.display().to_string(),
            dir.path().display().to_string(),
        ];
        (dir, controller, git)
    }

    fn github_deployment(apply: bool) -> DeploymentRequest {
        serde_json::from_value(json!({"apply":apply,"github":{"repository":"https://github.com/example/application","reference":"release/v1","context":"services/api"},"service":{"id":"github-app","name":"GitHub application","limits":{"cpus":0.5,"memory_mb":128}}})).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn github_deployment_previews_without_clone_then_builds_before_container_creation() {
        let (dir, mut c, git) = github_deployment_fixture();
        let original = fs::read(&c.config_path).unwrap();
        let preview = c
            .deploy_with_git(github_deployment(false), &git)
            .await
            .unwrap();
        assert_eq!(preview["github"]["reference"], "release/v1");
        assert_eq!(
            preview["service"]["image"],
            format!("selfhost-github-{}/github-app:latest", c.docker.owner)
        );
        assert!(!dir.path().join("git-calls").exists());
        assert!(!dir.path().join("mutations").exists());
        assert_eq!(fs::read(&c.config_path).unwrap(), original);

        c.deploy_with_git(github_deployment(true), &git)
            .await
            .unwrap();
        let commands = fs::read_to_string(dir.path().join("git-calls")).unwrap();
        assert!(commands.contains("clone --template= --depth 1 --single-branch --no-tags --branch release/v1 -- https://github.com/example/application"));
        let mutations = fs::read_to_string(dir.path().join("mutations")).unwrap();
        assert!(mutations.find("image prepared").unwrap() < mutations.find("create ").unwrap());
        let build = fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(build.lines().any(
            |line| line.starts_with("build --quiet --tag selfhost-github-") && line.ends_with(" -")
        ));
        assert_eq!(
            fs::read_to_string(dir.path().join("build-dockerfile")).unwrap(),
            "FROM scratch\n"
        );
        let listing = runtime::run(&[
            "tar".into(),
            "-tf".into(),
            dir.path().join("build-input.tar").display().to_string(),
        ])
        .await
        .unwrap();
        assert!(listing.lines().any(|name| name == "./.dockerignore"));
        assert!(
            !listing
                .lines()
                .any(|name| name == "./.git" || name.starts_with("./.git/"))
        );
        assert!(!listing.lines().any(|name| name.ends_with(".tar")));
        assert!(
            c.manifest.services[0]
                .image
                .as_deref()
                .unwrap()
                .starts_with("selfhost-github-")
        );
        assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".github-build-")
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn github_clone_or_build_failure_preserves_manifest_and_has_no_container_or_volume_side_effects()
     {
        for failure in ["fail-clone", "fail-tar", "fail-build"] {
            let (dir, mut c, git) = github_deployment_fixture();
            let original = fs::read(&c.config_path).unwrap();
            fs::write(dir.path().join(failure), "").unwrap();
            let mut request = github_deployment(true);
            request.service.workspace = true;
            assert!(c.deploy_with_git(request, &git).await.is_err());
            assert_eq!(fs::read(&c.config_path).unwrap(), original);
            assert!(!dir.path().join("mutations").exists());
            assert!(c.manifest.services.is_empty());
            assert!(c.state.services.is_empty());
            assert!(!fs::read_dir(dir.path()).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".github-build-")
            }));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn github_validation_and_admission_happen_before_clone() {
        let (dir, mut c, git) = github_deployment_fixture();
        for request in [
            {
                let mut r = github_deployment(true);
                r.github.as_mut().unwrap().repository = "https://evil.test/repo".into();
                r
            },
            {
                let mut r = github_deployment(true);
                r.github.as_mut().unwrap().context = "../outside".into();
                r
            },
            {
                let mut r = github_deployment(true);
                r.service.limits.cpus = 3.;
                r
            },
        ] {
            assert!(c.deploy_with_git(request, &git).await.is_err());
            assert!(!dir.path().join("git-calls").exists());
            assert!(!dir.path().join("mutations").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_preview_is_read_only_and_success_persists_managed_service() {
        let (dir, mut c) = deployment_fixture();
        let original = fs::read(&c.config_path).unwrap();
        let plan = c.deploy(deployment("custom", false)).await.unwrap();
        assert_eq!(plan["applied"], false);
        assert_eq!(plan["service"]["runtime"], "docker");
        assert_eq!(plan["service"]["observe_only"], false);
        assert_eq!(fs::read(&c.config_path).unwrap(), original);
        assert!(!dir.path().join("mutations").exists());
        let result = c.deploy(deployment("custom", true)).await.unwrap();
        assert_eq!(result["applied"], true);
        let service = &Manifest::load(&c.config_path).unwrap().services[0];
        assert_eq!(service.image.as_deref(), Some("example/custom:tag"));
        assert!(c.docker.inspect(service).await.unwrap().unwrap()["State"]["Running"] == true);
        let mutations = fs::read_to_string(dir.path().join("mutations")).unwrap();
        assert!(
            c.deploy(deployment("custom", true))
                .await
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("mutations")).unwrap(),
            mutations
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_rejects_invalid_or_unsupported_input_before_runtime_mutation() {
        for field in ["runtime", "observe_only", "enabled", "unit", "container"] {
            let mut value = json!({"service":{"id":"bad","image":"example","limits":{"cpus":1,"memory_mb":128}}});
            value["service"][field] = json!("unsupported");
            assert!(
                serde_json::from_value::<DeploymentRequest>(value).is_err(),
                "{field}"
            );
        }
        let (dir, mut c) = deployment_fixture();
        let original = fs::read(&c.config_path).unwrap();
        for request in [
            {
                let mut r = deployment("new", true);
                r.service.command = vec!["x".into(); 65];
                r
            },
            {
                let mut r = deployment("new", true);
                r.service.ports = vec!["0.0.0.0:80:80".into()];
                r
            },
            {
                let mut r = deployment("new", true);
                r.service.limits.cpus = 3.;
                r
            },
            {
                let mut r = deployment("new", true);
                r.service.image = "bad@image".into();
                r
            },
        ] {
            assert!(c.deploy(request).await.is_err());
        }
        assert_eq!(fs::read(&c.config_path).unwrap(), original);
        assert!(!dir.path().join("mutations").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_failure_keeps_original_config_and_stops_partial_container() {
        for failure in ["fail-start", "fail-config"] {
            let (dir, mut c) = deployment_fixture();
            let original = fs::read(&c.config_path).unwrap();
            fs::write(dir.path().join(failure), "").unwrap();
            let error = c.deploy(deployment("partial", true)).await.unwrap_err();
            assert!(
                format!("{error:#}").contains("remains stopped"),
                "{error:#}"
            );
            assert_eq!(fs::read(&c.config_path).unwrap(), original);
            let service = deployment("partial", false)
                .service
                .into_service(&c.docker.owner)
                .unwrap();
            assert_eq!(
                c.docker.inspect(&service).await.unwrap().unwrap()["State"]["Running"],
                false
            );
            assert!(c.state.services.is_empty());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn automatic_deployment_failure_restores_existing_caps_and_preserves_ceilings() {
        let (dir, mut c) = deployment_fixture();
        let mut old = deployment("existing", true);
        old.service.limits = Limits {
            cpus: 2.,
            memory_mb: 1024,
        };
        c.deploy(old).await.unwrap();
        c.manifest.allocation.enabled = true;
        c.manifest.save(&c.config_path).unwrap();
        let original = fs::read(&c.config_path).unwrap();
        fs::write(dir.path().join("fail-start"), "").unwrap();
        let error = c.deploy(deployment("new", true)).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("previous caps restored"),
            "{error:#}"
        );
        assert_eq!(fs::read(&c.config_path).unwrap(), original);
        let limits = c.actual_limits(&c.manifest.services[0]).await.unwrap();
        assert_eq!(
            limits,
            Limits {
                cpus: 2.,
                memory_mb: 1024
            }
        );
        fs::remove_file(dir.path().join("fail-start")).unwrap();
        c.deploy(deployment("success", true)).await.unwrap();
        assert_eq!(c.manifest.services[0].limits.as_ref().unwrap().cpus, 2.);
        assert!(c.state.allocations["existing"].cpus < 2.);
        assert!(c.state.allocations.values().map(|l| l.cpus).sum::<f64>() <= 2.);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployed_job_runs_now_and_respects_parallel_job_admission() {
        let (_dir, mut c) = deployment_fixture();
        let mut job = deployment("job", true);
        job.service.kind = "job".into();
        job.service.interval_seconds = Some(60);
        c.deploy(job).await.unwrap();
        assert!(c.state.services["job"].last_started.is_some());
        let mut next = deployment("next", false);
        next.service.kind = "job".into();
        next.service.interval_seconds = Some(60);
        assert!(
            c.deploy(next)
                .await
                .unwrap_err()
                .to_string()
                .contains("Maximum parallel")
        );
        assert_eq!(c.manifest.services.len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deployment_validates_project_before_runtime_work_and_previews_group() {
        let (dir, mut c) = deployment_fixture();
        let mut request = deployment("grouped", false);
        request.service.project = Some("build-tools".into());
        assert!(c.deploy(request.clone()).await.is_err());
        assert!(!dir.path().join("calls").exists());
        c.create_project(config::Project {
            id: "build-tools".into(),
            name: "Build tools".into(),
            description: String::new(),
        })
        .unwrap();
        let preview = c.deploy(request.clone()).await.unwrap();
        assert_eq!(preview["project"]["id"], "build-tools");
        request.apply = true;
        c.deploy(request).await.unwrap();
        assert_eq!(
            c.manifest.services[0].project.as_deref(),
            Some("build-tools")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn isolated_workspace_is_previewed_empty_owned_and_retained_on_failure() {
        let (dir, mut c) = deployment_fixture();
        fs::write(dir.path().join("workspace-service"), "isolated").unwrap();
        let mut request = deployment("isolated", false);
        request.service.workspace = true;
        let preview = c.deploy(request.clone()).await.unwrap();
        assert_eq!(preview["workspace"]["reused"], false);
        assert_eq!(preview["workspace"]["created"], false);
        assert_eq!(preview["service"]["working_dir"], "/workspace");
        assert_eq!(preview["service"]["volumes"][0]["no_copy"], true);
        let volume = preview["workspace"]["volume"].as_str().unwrap().to_owned();
        assert!(!dir.path().join(format!("{volume}.volume")).exists());
        fs::write(dir.path().join(format!("{volume}.volume")), "{}").unwrap();
        assert!(
            c.deploy(request.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains("another")
        );
        fs::remove_file(dir.path().join(format!("{volume}.volume"))).unwrap();
        fs::write(dir.path().join("fail-start"), "").unwrap();
        request.apply = true;
        let error = c.deploy(request).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("retained with its data"),
            "{error:#}"
        );
        assert!(dir.path().join(format!("{volume}.volume")).exists());
        assert!(Manifest::load(&c.config_path).unwrap().services.is_empty());
        let mut reused = deployment("different", false);
        reused.service.workspace = true;
        let existing_volume = format!("selfhost-{}-different-workspace", c.docker.owner);
        fs::write(dir.path().join(format!("{existing_volume}.volume")),json!({"io.self-host-init.workspace":c.docker.owner,"io.self-host-init.service":"different","io.self-host-init.purpose":"workspace"}).to_string()).unwrap();
        let plan = c.deploy(reused).await.unwrap();
        assert_eq!(plan["workspace"]["reused"], true);
        assert!(
            plan["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str().unwrap().contains("retained data"))
        );
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
        assert_eq!(snapshot["services"][0]["cpu_cores"], 0.12);
        assert_eq!(
            snapshot["services"][0]["execution"]["target"],
            "external-worker"
        );
        assert_eq!(
            snapshot["services"][0]["execution"]["pid_namespace"],
            "engine host"
        );
        assert!(snapshot["services"][0]["workspace_storage"].is_null());
        assert_eq!(snapshot["telemetry"]["cpu_percent"], 12.);
        assert_eq!(snapshot["telemetry"]["memory_mb"], 123.);
        assert_eq!(snapshot["telemetry"]["cpu_measured_services"], 1);
        assert_eq!(snapshot["telemetry"]["service_count"], 2);
        assert_eq!(snapshot["telemetry"]["scope"], "monitored services");
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

    #[test]
    fn cpu_usage_requires_a_continuous_interval_and_resets_for_new_processes() {
        let previous = HashMap::from([(1, 250_000_000), (2, 500_000_000)]);
        assert_eq!(
            cpu_delta_percent(
                &previous,
                &HashMap::from([(1, 1_250_000_000), (2, 1_500_000_000)]),
                2.
            ),
            Some(100.)
        );
        assert_eq!(cpu_delta_percent(&previous, &previous, 2.), Some(0.));
        assert_eq!(cpu_delta_percent(&HashMap::new(), &previous, 2.), None);
        assert_eq!(
            cpu_delta_percent(&previous, &HashMap::from([(1, 100), (2, 600_000_000)]), 2.),
            None
        );
        assert_eq!(
            cpu_delta_percent(&previous, &HashMap::from([(3, 100)]), 2.),
            None
        );
        assert_eq!(cpu_delta_percent(&previous, &previous, 0.), None);
    }

    #[test]
    fn monitored_totals_preserve_unknowns_deduplicate_targets_and_use_actual_sample_age() {
        let services = vec![
            json!({"cpu_percent":150.,"memory_mb":128.,"sampled_at":100}),
            json!({"cpu_percent":150.,"memory_mb":128.,"sampled_at":101}),
            json!({"cpu_percent":null,"memory_mb":null,"sampled_at":null}),
        ];
        let usage = monitored_usage(&services, &["same".into(), "same".into(), "unknown".into()]);
        assert_eq!(usage["cpu_cores"], 1.5);
        assert_eq!(usage["memory_mb"], 128.);
        assert_eq!(usage["cpu_measured_services"], 2);
        assert_eq!(usage["service_count"], 3);
        assert_eq!(usage["sampled_at"], 100);
        let empty = monitored_usage(&[json!({})], &["missing".into()]);
        assert!(empty["cpu_percent"].is_null());
        assert!(empty["memory_mb"].is_null());
        assert!(empty["sampled_at"].is_null());
    }

    #[test]
    fn monitor_coverage_excludes_only_observed_inactive_services() {
        let services = vec![
            json!({"status":"running","enabled":false,"cpu_percent":10.,"memory_mb":32.,"sampled_at":100}),
            json!({"status":"not created","enabled":false}),
            json!({"status":"exited","active_state":"active","memory_mb":8.,"sampled_at":101}),
            json!({"status":"unavailable"}),
            json!({"status":"stopped","cpu_percent":0.,"memory_mb":0.,"sampled_at":102}),
        ];
        let keys = (0..services.len())
            .map(|index| index.to_string())
            .collect::<Vec<_>>();
        let usage = monitored_usage(&services, &keys);
        assert_eq!(usage["service_count"], 5);
        assert_eq!(usage["eligible_service_count"], 3);
        assert_eq!(usage["cpu_measured_eligible_services"], 1);
        assert_eq!(usage["memory_measured_eligible_services"], 2);
        assert_eq!(usage["cpu_complete"], false);
        assert_eq!(usage["memory_complete"], false);
        let inactive = monitored_usage(&services[1..2], &keys[1..2]);
        assert_eq!(inactive["eligible_service_count"], 0);
        assert_eq!(inactive["cpu_complete"], false);
        assert!(inactive["cpu_percent"].is_null());
        let complete = monitored_usage(&services[0..1], &keys[0..1]);
        assert_eq!(complete["cpu_complete"], true);
        assert_eq!(complete["memory_complete"], true);
        let alias = monitored_usage(
            &[json!({"status":"unavailable"}), services[0].clone()],
            &["same".into(), "same".into()],
        );
        assert_eq!(alias["cpu_percent"], 10.);
        assert_eq!(alias["unique_targets"], 1);
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentService {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default = "deployment_kind")]
    pub kind: String,
    pub limits: Limits,
    #[serde(default)]
    pub command: Vec<String>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub workspace: bool,
    #[serde(default)]
    pub ports: Vec<String>,
    pub env_file: Option<String>,
    #[serde(default)]
    pub volumes: Vec<config::Volume>,
    pub interval_seconds: Option<u64>,
    #[serde(default)]
    pub allocation: config::ServiceAllocation,
}

fn deployment_kind() -> String {
    "daemon".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentRequest {
    #[serde(default)]
    pub apply: bool,
    pub service: DeploymentService,
    pub github: Option<GithubSource>,
}

impl DeploymentService {
    fn into_service(mut self, owner: &str) -> Result<Service> {
        ensure!(
            self.project
                .as_ref()
                .is_none_or(|project| project.len() <= 48),
            "Project ID is too long"
        );
        ensure!(
            self.id.len() <= 48
                && self.name.len() <= 128
                && self.description.len() <= 1024
                && self.image.len() <= 512,
            "Deployment id, name, description or image is too long"
        );
        ensure!(
            !self.name.contains(char::is_control) && !self.description.contains('\0'),
            "Deployment name or description contains control characters"
        );
        ensure!(
            self.command.len() <= 64 && self.command.iter().all(|arg| arg.len() <= 1024),
            "Deployment command is limited to 64 arguments of at most 1024 bytes"
        );
        ensure!(
            self.ports.len() <= 32 && self.ports.iter().all(|port| port.len() <= 64),
            "Deployment supports at most 32 port mappings"
        );
        ensure!(
            self.volumes.len() <= 32
                && self
                    .volumes
                    .iter()
                    .all(|v| v.source.len() <= 1024 && v.target.len() <= 1024),
            "Deployment supports at most 32 volumes with paths up to 1024 bytes"
        );
        ensure!(
            self.env_file.as_ref().is_none_or(|path| path.len() <= 1024),
            "Environment file path is too long"
        );
        ensure!(
            self.kind == "job" || self.interval_seconds.is_none(),
            "Only jobs accept interval_seconds"
        );
        if self.workspace {
            ensure!(
                !self.volumes.iter().any(|v| v.target == "/workspace"),
                "The isolated workspace reserves the /workspace mount"
            );
            ensure!(
                self.working_dir
                    .as_deref()
                    .is_none_or(|path| path == "/workspace"),
                "The isolated workspace uses /workspace as its working directory"
            );
            self.working_dir = Some("/workspace".into());
            self.volumes.push(config::Volume {
                source: format!("selfhost-{owner}-{}-workspace", self.id),
                target: "/workspace".into(),
                read_only: false,
                no_copy: true,
            });
        }
        let mut value = serde_json::to_value(self)?;
        let fields = value
            .as_object_mut()
            .expect("deployment service serializes as an object");
        fields.remove("workspace");
        fields.insert("runtime".into(), json!("docker"));
        fields.insert("observe_only".into(), json!(false));
        fields.insert("enabled".into(), json!(true));
        Ok(serde_json::from_value(value)?)
    }
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
    cpu_samples: HashMap<String, CpuSample>,
    host_monitor: HostMonitor,
}

pub(crate) struct ControllerLock(File);

impl Drop for ControllerLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
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
            host_monitor: HostMonitor::default(),
        })
    }

    pub(crate) fn lock(&self) -> Result<ControllerLock> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.workspace.join("controller.lock"))?;
        f.try_lock_exclusive()
            .context("Another controller is changing this workspace; retry when it finishes")?;
        Ok(ControllerLock(f))
    }

    pub(crate) fn reload(&mut self) -> Result<()> {
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
        self.apply_allocation_with_deployment(candidate, None).await
    }

    async fn apply_allocation_with_deployment(
        &mut self,
        candidate: Manifest,
        deployment: Option<Service>,
    ) -> Result<Value> {
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
        let mut deployment_attempted = false;
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
            if let Some(deployment) = &deployment {
                let service = granted
                    .services
                    .iter()
                    .find(|s| s.id == deployment.id)
                    .unwrap();
                self.admission(&granted, Some(service), service.job())
                    .await?;
                deployment_attempted = true;
                self.docker.create(service).await?;
                let state = self.state.services.entry(service.id.clone()).or_default();
                state.paused = false;
                if service.job() {
                    state.last_started = Some(now());
                }
                self.event(&service.id, "deploy", "Service deployed", true);
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
            let mut deployment_note = String::new();
            if deployment_attempted {
                match self
                    .stop_failed_deployment(deployment.as_ref().unwrap())
                    .await
                {
                    Ok(note) => deployment_note = note,
                    Err(error) => failures.push(format!(
                        "Deployment container {} may still be running: {error:#}",
                        self.docker.name(deployment.as_ref().unwrap())
                    )),
                }
            }
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
                    "Allocation failed: {error:#}. Rollback incomplete: {}. {deployment_note} Inspect actual limits before retrying; original configuration was preserved",
                    failures.join("; "),
                );
            }
            return Err(error.context(format!(
                "Allocation cancelled; previous caps restored. {deployment_note}"
            )));
        }
        self.manifest = candidate;
        Ok(Self::allocation_json(&self.manifest, &plan))
    }

    async fn stop_failed_deployment(&self, service: &Service) -> Result<String> {
        let name = self.docker.name(service);
        if let Some(container) = self.docker.inspect(service).await? {
            if container["State"]["Running"] == true {
                self.docker.lifecycle(service, "stop").await?;
                ensure!(
                    self.docker
                        .inspect(service)
                        .await?
                        .is_none_or(|d| d["State"]["Running"] == false),
                    "Container did not stop"
                );
            }
            return Ok(format!(
                "Container {name} remains stopped for inspection; its processes may already have run. Original configuration was preserved. Downloaded images and network state may remain."
            ));
        }
        Ok(
            "No service declaration was saved. Downloaded images and network state may remain."
                .into(),
        )
    }

    pub async fn deploy(&mut self, request: DeploymentRequest) -> Result<Value> {
        self.deploy_with_git(request, &["git".into()]).await
    }

    async fn deploy_with_git(
        &mut self,
        mut request: DeploymentRequest,
        git: &[String],
    ) -> Result<Value> {
        let _lock = self.lock()?;
        self.reload()?;
        if let Some(source) = &request.github {
            source.validate()?;
            request.service.image = format!(
                "selfhost-github-{}/{}:latest",
                self.docker.owner, request.service.id
            );
        }
        let isolated_workspace = request.service.workspace;
        let service = request.service.into_service(&self.docker.owner)?;
        let workspace_volume = isolated_workspace
            .then(|| format!("selfhost-{}-{}-workspace", self.docker.owner, service.id));
        ensure!(
            self.manifest.services.len() < 256,
            "This workspace has reached the deployment limit of 256 services"
        );
        ensure!(
            !self.manifest.services.iter().any(|s| s.id == service.id),
            "Service {} already exists; use its existing controls",
            service.id
        );
        let mut candidate = self.manifest.clone();
        candidate.services.push(service.clone());
        candidate.validate()?;
        crate::manifest_io::encode(&self.config_path, &candidate)?;
        ensure!(
            self.docker.inspect(&service).await?.is_none(),
            "Container {} already exists; inspect it before retrying deployment",
            self.docker.name(&service)
        );
        let image_available = if request.github.is_some() {
            false
        } else {
            self.docker
                .image_available(service.image.as_deref().unwrap())
                .await?
        };
        let workspace_existing = if let Some(volume) = &workspace_volume {
            self.docker
                .workspace_volume(&service, volume, false)
                .await?
        } else {
            false
        };
        let plan = self.build_allocation(&candidate).await?;
        let allocation = plan
            .allocations
            .iter()
            .find(|row| row.id == service.id)
            .unwrap();
        let effective = Limits {
            cpus: allocation.cpus,
            memory_mb: allocation.memory_mb,
        };
        if !candidate.allocation.enabled {
            self.admission(&candidate, Some(&service), service.job())
                .await?;
        } else if service.job() {
            let running_jobs = self
                .docker
                .inventory()
                .await?
                .iter()
                .filter(|row| row["State"] == "running")
                .filter(|row| {
                    !candidate.services.iter().any(|s| {
                        s.managed()
                            && s.runtime == "docker"
                            && !s.job()
                            && self.docker.name(s) == row["Names"].as_str().unwrap_or_default()
                    })
                })
                .count();
            ensure!(
                running_jobs < candidate.budget.max_parallel_jobs,
                "Maximum parallel jobs reached; deploy this job after a running job finishes"
            );
        }
        let mut warnings = plan.warnings.clone();
        if request.github.is_some() {
            warnings.push("Apply clones the public GitHub branch/tag and runs its Dockerfile with this Docker engine. Review the repository before deploying it. Builds may download dependencies and leave image layers after failure; the checkout is removed. Private repositories require a local checkout.".into());
            warnings.push("GitHub builds archive the selected directory without applying .dockerignore exclusions. Matching files can still be copied into the image. Choose a narrow build directory, or use a local checkout and deploy-image --build-context for Docker's directory ignore rules.".into());
            warnings.push("Preview does not contact GitHub or build an image. The selected branch/tag can change before apply; success means the container started, not that the application passed a health check.".into());
        } else if !image_available {
            warnings.push("The image is not cached on this Docker engine. Deployment may download it; downloaded layers can remain after a failed deployment.".into());
        }
        if workspace_existing {
            warnings.push(format!("Workspace volume {} already exists. Deployment reuses its retained data; it will not be emptied or deleted.",workspace_volume.as_deref().unwrap()));
        }
        let mut execution = runtime::execution_location(
            &service,
            &candidate.engine.command,
            &self.docker.info().await,
        );
        execution["target"] = json!(self.docker.name(&service));
        execution["network"] = json!(self.docker.network_for(&service));
        let mut preview = json!({"ok":true,"applied":request.apply,"service":service,"container":self.docker.name(&service),"effective_limits":effective,"allocations":plan.allocations,"warnings":warnings,"execution":execution,"workspace":{"enabled":isolated_workspace,"volume":workspace_volume,"mount_path":if isolated_workspace {Some("/workspace")}else{None},"persistent":isolated_workspace,"created":false,"reused":workspace_existing},"budget":candidate.budget,"action":if service.job(){format!("Run job now and schedule every {} seconds",service.interval_seconds.unwrap())}else{"Create and start daemon".into()}});
        preview["project"] = json!(
            service
                .project
                .as_ref()
                .and_then(|id| candidate.projects.iter().find(|project| project.id == *id))
        );
        preview["network"] = json!(self.docker.network_for(&service));
        preview["github"] = json!(request.github);
        if !request.apply {
            return Ok(preview);
        }
        if let Some(source) = &request.github {
            let checkout = source.checkout(&self.workspace, git).await?;
            let archive = checkout.archive(&source.context).await?;
            self.docker
                .prepare_archived_image(service.image.as_deref().unwrap(), &archive)
                .await
                .context(
                    "GitHub image build failed; no service declaration or container was created",
                )?;
            drop(checkout);
        }
        if let Some(volume) = &workspace_volume {
            let existing=self.docker.workspace_volume(&service,volume,true).await.with_context(||format!("Could not provision workspace volume {volume}; any created volume and data are retained. No service declaration was saved"))?;
            preview["workspace"]["created"] = json!(!existing);
            preview["workspace"]["reused"] = json!(existing);
        }
        let retained_workspace = workspace_volume.as_ref().map_or(String::new(), |volume| {
            format!(" Workspace volume {volume} is retained with its data; it was not deleted.")
        });
        if candidate.allocation.enabled {
            self.apply_allocation_with_deployment(candidate, Some(service))
                .await
                .with_context(|| format!("Deployment did not complete.{retained_workspace}"))?;
        } else {
            let previous = self.state.clone();
            let result = async {
                self.docker.create(&service).await?;
                let state = self.state.services.entry(service.id.clone()).or_default();
                state.paused = false;
                if service.job() {
                    state.last_started = Some(now());
                }
                self.event(&service.id, "deploy", "Service deployed", true);
                self.persist()?;
                candidate.save(&self.config_path)?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let cleanup = self.stop_failed_deployment(&service).await;
                self.state = previous;
                let restored = self.persist();
                let cleanup=format!("{}{retained_workspace}",cleanup.unwrap_or_else(|err|format!("Container {} may still be running: {err:#}. Original configuration was preserved",self.docker.name(&service))));
                if let Err(state_error) = restored {
                    bail!(
                        "Deployment failed: {error:#}. {cleanup}. Private state could not be restored: {state_error:#}"
                    );
                }
                return Err(error.context(cleanup));
            }
            self.manifest = candidate;
        }
        Ok(preview)
    }

    /// Preview by default; replace one managed daemon without changing its limits or mounts.
    pub async fn deploy_image(
        &mut self,
        id: &str,
        image: &str,
        apply: bool,
        build_context: Option<&std::path::Path>,
        pull: bool,
    ) -> Result<Value> {
        let _lock = self.lock()?;
        self.reload()?;
        let declared = self.service(id)?;
        Self::validate_action(&declared, "recreate", None)?;
        ensure!(
            declared.runtime == "docker" && !declared.job(),
            "Image deployments support managed Docker daemons; jobs use their run controls"
        );
        ensure!(
            !image.is_empty()
                && !image.starts_with('-')
                && !image.contains('\0')
                && image.len() <= 1024,
            "Invalid image reference"
        );
        ensure!(
            !(pull && build_context.is_some()),
            "Choose either --pull or --build-context"
        );
        let mut candidate = self.manifest.clone();
        candidate
            .services
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap()
            .image = Some(image.into());
        candidate.validate()?;
        crate::manifest_io::encode(&self.config_path, &candidate)?;
        let effective = self.effective_manifest()?;
        let old = effective
            .services
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .clone();
        let mut new = old.clone();
        new.image = Some(image.into());
        self.admission(&effective, Some(&old), false).await?;
        ensure!(
            self.docker.inspect(&old).await?.is_some(),
            "Start this service once before deploying a replacement"
        );
        let mut result = json!({"ok":true,"applied":apply,"service":id,"previous_image":declared.image,"image":image,"limits":old.limits,"build_context":build_context,"pull":pull,"warnings":["Replacement briefly stops this service. Existing volumes are retained; application data migrations cannot be rolled back automatically.","Success means the container started, not that the application passed a health check."]});
        if !apply {
            return Ok(result);
        }
        self.docker
            .prepare_image(image, build_context, pull)
            .await?;
        let warning = self
            .docker
            .replace_image(&old, &new, || candidate.save(&self.config_path))
            .await?;
        self.manifest = candidate;
        self.state.services.entry(id.into()).or_default().paused = false;
        self.event(id, "deploy-image", "Service image deployed", true);
        self.persist()
            .context("Image deployment applied, but controller state could not be saved")?;
        if let Some(warning) = warning {
            result["warnings"]
                .as_array_mut()
                .unwrap()
                .push(json!(warning));
        }
        Ok(result)
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
        let (runtime, capacity, mut native_samples, host_metrics) = tokio::join!(
            self.docker.info(),
            host_capacity(),
            runtime::native_samples(&self.manifest.services),
            self.host_monitor.sample()
        );
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
        let (stats, stats_error) = match self.docker.stats_for(&running_names).await {
            Ok(stats) => (stats, None),
            Err(error) => (HashMap::new(), Some(error.to_string())),
        };
        let docker_sampled_at = now();
        let mut services = vec![];
        let mut target_keys = vec![];
        let active_ids: HashSet<_> = self
            .manifest
            .services
            .iter()
            .map(|service| service.id.clone())
            .collect();
        self.cpu_samples.retain(|id, _| active_ids.contains(id));
        for s in self.manifest.services.clone() {
            let effective = if self.manifest.allocation.enabled {
                self.state.allocations.get(&s.id)
            } else {
                s.limits.as_ref()
            };
            let mut view = json!({"id":s.id,"name":if s.name.is_empty(){s.id.clone()}else{s.name.clone()},"description":s.description,"runtime":s.runtime,"kind":s.kind,"enabled":s.enabled,"observe_only":s.observe_only,"limits":s.limits,"effective_limits":effective,"allocation":s.allocation,"status":"unknown","health":"unknown","cpu_percent":null,"memory_mb":null,"capabilities":{"scale":s.managed() && s.runtime!="launchd","lifecycle":s.managed() && s.runtime!="launchd"}});
            view["execution"] =
                runtime::execution_location(&s, &self.manifest.engine.command, &runtime);
            view["project"] = json!(s.project);
            view["image"] = json!(s.image);
            view["command"] = json!(s.command);
            view["ports"] = json!(s.ports);
            view["env_file"] = json!(s.env_file);
            view["network"] = if s.runtime == "docker" && s.managed() {
                json!(self.docker.network_for(&s))
            } else {
                Value::Null
            };
            view["working_dir"] = json!(s.working_dir);
            view["mounts"]=json!(s.volumes.iter().map(|volume|json!({"type":if volume.source.starts_with('/') {"bind"}else{"volume"},"source":volume.source,"target":volume.target,"read_only":volume.read_only,"no_copy":volume.no_copy,"persistent":true})).collect::<Vec<_>>());
            view["workspace_storage"] = if s.runtime == "docker" {
                s.volumes.iter().find(|volume|volume.target=="/workspace").map_or(Value::Null,|volume|json!({"type":if volume.source.starts_with('/') {"bind"}else{"volume"},"source":volume.source,"mount_path":volume.target,"persistent":true,"location":if volume.source.starts_with('/') {"engine host path"}else{"Docker engine volume"},"empty_on_first_create":volume.no_copy,"observed":false}))
            } else {
                Value::Null
            };
            view["pid"] = Value::Null;
            view["sampled_at"] = Value::Null;
            view["telemetry_error"] = Value::Null;
            view["cpu_sample_pending"] = json!(false);
            let mut target_key = format!("{}:{}", s.runtime, s.id);
            if s.runtime == "docker" {
                view["execution"]["target"] = json!(self.docker.name(&s));
            }
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
                        let container = d["Name"]
                            .as_str()
                            .map(|name| name.trim_start_matches('/').to_owned())
                            .unwrap_or_else(|| self.docker.name(&s));
                        target_key = format!("docker:{}", d["Id"].as_str().unwrap_or(&container));
                        view["container"] = json!(container);
                        view["container_id"] = d["Id"].clone();
                        view["started_at"] = d["State"]["StartedAt"].clone();
                        view["restart_count"] = d["RestartCount"].clone();
                        view["runtime_active"] = d["State"]["Running"].clone();
                        view["actual_working_dir"] = d["Config"]["WorkingDir"].clone();
                        view["actual_networks"] = json!(
                            d["NetworkSettings"]["Networks"]
                                .as_object()
                                .map(|networks| networks.keys().collect::<Vec<_>>())
                        );
                        view["actual_mounts"]=json!(d["Mounts"].as_array().into_iter().flatten().map(|mount|json!({"type":mount["Type"],"name":mount["Name"],"source":mount["Source"],"target":mount["Destination"],"read_only":mount["RW"].as_bool().map(|writable|!writable)})).collect::<Vec<_>>());
                        if !view["workspace_storage"].is_null()
                            && d["Mounts"].as_array().is_some_and(|mounts| {
                                mounts
                                    .iter()
                                    .any(|mount| mount["Destination"] == "/workspace")
                            })
                        {
                            view["workspace_storage"]["observed"] = json!(true);
                        }
                        view["execution"]["target"] = json!(container);
                        view["pid"] = json!(d["State"]["Pid"].as_u64().filter(|pid| *pid > 0));
                        view["status"] = d["State"]["Status"].clone();
                        view["health"] = d["State"]["Health"]["Status"]
                            .as_str()
                            .map_or(json!("no healthcheck"), |h| json!(h));
                        view["actual_limits"] = json!({"cpus":d["HostConfig"]["NanoCpus"].as_f64().unwrap_or(0.)/1e9,"memory_mb":d["HostConfig"]["Memory"].as_u64().unwrap_or(0)/1048576});
                        view["resource_scope"] = json!("container");
                        view["cpu_source"] = json!("Docker stats interval");
                        view["memory_source"] =
                            json!("Docker stats memory (cache excluded on Linux)");
                        if let Some(v) = stats.get(&container) {
                            view["cpu_percent"] = v["cpu_percent"].clone();
                            view["memory_mb"] = v["memory_mb"].clone();
                            view["task_count"] = v["pids"].clone();
                            view["sampled_at"] = json!(docker_sampled_at);
                        } else if d["State"]["Running"] == true {
                            view["telemetry_error"] = json!(stats_error.as_deref().unwrap_or(
                                "No current resource sample returned for this container"
                            ));
                        }
                    }
                    Ok(None) => view["status"] = json!("not created"),
                    Err(e) => {
                        view["status"] = json!("unavailable");
                        view["health"] = json!(e.to_string());
                        view["telemetry_error"] = json!(e);
                    }
                }
            } else {
                match native_samples
                    .remove(&s.id)
                    .unwrap_or_else(|| Err("Native service sampling did not complete".into()))
                {
                    Ok(sample) => {
                        if let Some(o) = sample.state.as_object() {
                            for (k, v) in o {
                                view[k] = v.clone();
                            }
                        }
                        view["sampled_at"] = json!(sample.sampled_at);
                        if let Some(counters) = sample.cpu_counters {
                            let identity = serde_json::to_string(&(
                                &s.runtime,
                                &s.command_prefix,
                                &s.scope,
                                &s.unit,
                                &s.label,
                                &view["pid"],
                            ))
                            .expect("sample identity serializes");
                            let current = CpuSample {
                                identity,
                                captured_at: sample.captured_at,
                                counters,
                            };
                            let cpu = self.cpu_samples.insert(s.id.clone(), current).and_then(
                                |previous| {
                                    let current = &self.cpu_samples[&s.id];
                                    (previous.identity == current.identity)
                                        .then(|| {
                                            cpu_delta_percent(
                                                &previous.counters,
                                                &current.counters,
                                                current
                                                    .captured_at
                                                    .duration_since(previous.captured_at)
                                                    .as_secs_f64(),
                                            )
                                        })
                                        .flatten()
                                },
                            );
                            view["cpu_percent"] = json!(cpu);
                            view["cpu_sample_pending"] = json!(cpu.is_none());
                        } else {
                            self.cpu_samples.remove(&s.id);
                        }
                        if s.runtime == "launchd" {
                            target_key = format!("launchd:{}", s.label.as_deref().unwrap_or(&s.id));
                        }
                        if s.runtime == "systemd" {
                            target_key = format!("systemd:{}", native_key(&s));
                        }
                    }
                    Err(e) => {
                        view["status"] = json!("unavailable");
                        view["health"] = json!(e.to_string());
                        view["telemetry_error"] = json!(e);
                        self.cpu_samples.remove(&s.id);
                    }
                }
            }
            if view["cpu_percent"].is_null() && view["memory_mb"].is_null() {
                view["sampled_at"] = Value::Null;
            }
            view["cpu_cores"] = json!(view["cpu_percent"].as_f64().map(|percent| percent / 100.));
            view["telemetry_eligible"] = json!(telemetry_eligible(&view));
            target_keys.push(target_key);
            services.push(view);
        }
        let timestamp = now();
        let telemetry = monitored_usage(&services, &target_keys);
        json!({"workspace":self.workspace,"config":self.config_path,"projects":self.manifest.projects,"budget":self.manifest.budget,"allocation":self.manifest.allocation,"capacity":capacity,"telemetry":telemetry,"host_metrics":host_metrics,"services":services,"events":self.state.events.iter().rev().take(50).collect::<Vec<_>>(),"runtime":runtime,"manifest_error":manifest_error,"timestamp":timestamp})
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
