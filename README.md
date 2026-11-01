# self-host-init

A Rust control plane for services running on one machine, with a Tauri desktop app, CLI, TUI, and optional local web interface. Declare services in YAML or JSON, apply CPU and memory limits, and schedule jobs within a workspace budget.

Docker supplies container isolation and resource limits. Existing Linux systemd units can opt into lifecycle and resource management. macOS launchd services can join the inventory as observers while they keep their current installation. The controller, scheduler, and HTTP server are Rust; the dashboard is bundled HTML, CSS, and JavaScript.

## Start here

Prebuilt desktop installers and CLI/TUI archives are available from [Releases](https://github.com/djsydney04/self-host-init/releases). The initial desktop release is unsigned and not Apple-notarized; see [installation and release details](docs/releases.md).

To configure everything from a checkout without opening a UI:

```sh
./scripts/setup.sh --examples
# Edit the generated services.yaml; examples start disabled.
./scripts/apply.sh
./scripts/apply.sh --apply
selfhost tui
```

The first apply command only prints a plan. Pass `--config` and `--workspace` to use a configuration tracked in your own repository. [Configuration and scripts](docs/configuration.md) covers a complete noninteractive setup.

Or install the CLI directly:

```sh
git clone https://github.com/djsydney04/self-host-init.git
cd self-host-init
cargo install --path . --locked
selfhost init --discover
selfhost doctor
selfhost status
selfhost serve
```

Open [http://127.0.0.1:8787](http://127.0.0.1:8787). Discovery inventories existing services; it does not move or stop them. The dashboard and controller run locally as your user.

The charcoal interface separates Services, Activity Monitor, Resources, Events, and Workspaces. Services show their running state and execution target, so host services and containers in a VM are easy to distinguish. Activity Monitor shows measured CPU and memory use across the monitored services; resource limits and budget reservations stay in Resources. Updates and open log views refresh automatically.

Choose **Deploy service** to add a Docker image from the app. Set its name, CPU and memory limits, and optional loopback port mapping. Advanced fields accept command arguments, a private environment-file path, and volume mounts. Review the deployment plan before confirming. A daemon starts immediately; a scheduled job runs once and then follows its configured interval. The declaration is saved to the same YAML or JSON manifest used by the CLI and TUI.

Named project workspaces group services on separate Docker networks. Each deployed service can also have its own empty persistent storage volume at `/workspace`, with no host folders shared by default. Project groups share the global resource pool; persistent volumes are retained until explicitly removed. [Workspace and storage configuration](docs/configuration.md#project-workspaces-and-private-storage) covers the same setup in YAML and the permissions required by non-root images.

Build with Rust 1.89 or newer. Once installed, `selfhost` has no Python or Node runtime requirement. To run directly from a checkout, use `cargo run -- COMMAND`, or `cargo build --release` followed by `target/release/selfhost COMMAND`. The desktop uses Tauri and the operating system webview; its separate crate keeps desktop dependencies out of CLI builds.

The default workspace is `~/.local/share/self-host-init` (or `$XDG_DATA_HOME/self-host-init` when set). New workspaces use `services.yaml`; existing `services.json` manifests are detected automatically. If multiple default manifests exist, specify `--config` explicitly. The desktop, CLI, and TUI share this selection. Use `selfhost --workspace /absolute/path …` to keep another workspace, or `selfhost --config /absolute/path/services.yaml …` for a different manifest.

Review that manifest before enabling workloads. [examples/services.json](examples/services.json) shows web and worker daemons, automation jobs, and native observers. Copy the entries you want into your manifest, or initialize a new workspace with `selfhost init --examples`. Its example entries are disabled until you supply their images, paths, and credentials. Initialization never overwrites an existing manifest. Add your own image or existing systemd unit; there is no catalog restriction.

```sh
selfhost plan
selfhost apply
selfhost run renovate
selfhost logs renovate --tail 100
```

Set `observe_only: false` to opt a service into management, and `enabled: true` to permit it to run. `apply` creates missing Docker daemons and applies managed services' resource caps. Use `start` to start an existing stopped daemon. Docker jobs run with `run` or when due through the scheduler; every managed job needs `interval_seconds` of at least 30. `serve` runs the scheduler alongside the dashboard; `tick` runs one scheduling pass.

## What you can manage

| Workload | Inventory/status | Logs | Lifecycle | Enforced CPU/memory |
| --- | --- | --- | --- | --- |
| Managed Docker daemon | Yes | Yes | Start, stop, restart, apply | Yes |
| Managed Docker job | Yes | Yes | Run, stop | Yes |
| Existing Docker container (`observe_only`) | Yes | Yes | Read only | Existing limits only |
| Existing systemd unit (`observe_only: false`) | Yes | Yes | Start, stop, restart, apply | Yes, with sufficient permissions |
| launchd / systemd observer | Yes | Where available | Read only | No |

Managed containers have stable `selfhost-<workspace-hash>-<service-id>` names, ownership labels, and a workspace-specific Docker network. Unrelated containers and native services remain under their existing managers. Managed containers drop Linux capabilities, disallow privilege escalation, and have no Docker socket unless you explicitly mount one. Images must work within those constraints.

## Scale services and the resource pool

```sh
selfhost scale renovate --cpus 1.5 --memory-mb 2048
selfhost budget --cpus 4 --memory-mb 6144
```

In manual mode, `scale` changes one managed service's caps and `budget` changes the admission pool. Budget changes reject overcommit. Configured daemon caps plus active managed jobs must fit before another job starts. In automatic mode these commands also redistribute effective caps within the pool. Neither mode resizes a VM. `max_parallel_jobs` caps concurrent jobs. Observers are outside this pool.

Enable automatic allocation when you want the controller to share that pool:

```sh
selfhost allocate --enabled true          # preview
selfhost allocate --enabled true --apply  # enable and apply
```

In automatic mode, service `limits` are ceilings. Each service has `allocation.weight`, `min_cpus`, and `min_memory_mb`. The Rust allocator reserves the minimums, then shares spare CPU and memory by weight up to each ceiling. It checks live memory use with 25% headroom, decreases caps before increasing others, and refuses a plan that cannot fit. Effective caps are stored separately from the declared ceilings. The running controller rechecks every 15 seconds. This is bounded weighted sharing, rather than prediction of future load; idle jobs participate so they can be admitted safely.

The desktop resource panel provides sliders, manual/automatic selection, a preview, and explicit apply. The TUI offers the same operations with keyboard controls: `b` for workspace budget, `e` for service limits, `a` to preview switching allocation mode, and Enter to confirm.

CPU amounts measure CPU time, including fractions of a core; they do not reserve dedicated physical cores. Docker enforces each container's caps, and systemd uses cgroup limits for opted-in units. The pool is admission control over those caps, so unrelated host processes still need headroom. See [Docker's resource constraints](https://docs.docker.com/engine/containers/resource_constraints/).

## Commands

```text
init [--discover] [--examples] Create a workspace with observers and/or examples
discover                      Show discovered automation without changing it
doctor                        Check configuration and runtime availability
validate                      Validate YAML/JSON without contacting runtimes
plan                          Show the intended managed workload configuration
apply [SERVICE]               Reconcile enabled managed daemons and resource caps
status [--json]               Inspect the inventory
logs SERVICE [--tail N]       Read recent service logs
start|stop|restart SERVICE    Control a managed Docker service or systemd unit
run SERVICE                   Start a managed one-shot job
recreate SERVICE              Explicitly replace a managed Docker container
scale SERVICE --cpus N --memory-mb N
                              Change a managed service's caps
budget --cpus N --memory-mb N  Change the workspace resource pool
allocate [--enabled true|false] [--apply]
                              Preview or apply weighted resource allocation
tui                           Interactive terminal interface
tick                          Run one scheduling pass
serve [--port 8787]            Serve the local dashboard and scheduler
install-launchd [--port 8787]   Run the controller as a macOS user LaunchAgent
uninstall-launchd             Remove that controller LaunchAgent
```

Changing limits can be applied in place. Image, command, environment, volume, or port changes to an existing managed container require `recreate`; the controller reports drift instead of silently replacing it. [Operations](docs/operations.md) covers credentials, existing systemd units, Lima guest paths, and bringing services into the workspace. [Architecture](docs/architecture.md) explains the manifest and scheduling model.

## Development

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo run --locked -- --help
bash scripts/test-scripts.sh
```

This is a single-machine controller. It has no multi-node scheduling, secret store, image builder, or automatic migration of existing installations. Access to the controller account or Docker daemon grants control over its workloads; keep the dashboard on loopback and keep credentials out of the manifest and Git.
