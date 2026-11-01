# self-host-init

A Rust control plane for services running on one machine. Put web apps, workers, CI runners, Renovate, repository maintenance, and your own tools in one workspace. Give each managed service CPU and memory caps, schedule jobs within a shared resource pool, and inspect everything from a local dashboard or CLI.

Docker supplies container isolation and resource limits. Existing Linux systemd units can opt into lifecycle and resource management. macOS launchd services can join the inventory as observers while they keep their current installation. The controller, scheduler, and HTTP server are Rust; the dashboard is bundled HTML, CSS, and JavaScript.

## Start here

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

Build with a current stable Rust toolchain. Once installed, `selfhost` has no Python or Node runtime requirement. To run directly from a checkout, use `cargo run -- COMMAND`, or `cargo build --release` followed by `target/release/selfhost COMMAND`.

The default workspace is `~/.local/share/self-host-init`; its `services.json` is the source of truth. Use `selfhost --workspace /absolute/path …` to keep another workspace, or `selfhost --config /absolute/path/services.json …` for a different manifest. Global options go before the command.

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

`scale` changes one managed service's caps. `budget` changes the workspace pool; it does not redistribute caps or resize a VM. Budget changes reject overcommit. Configured daemon caps plus active managed jobs must fit before another job starts. `max_parallel_jobs` in the manifest also caps concurrent jobs. Observers are outside this pool.

CPU amounts measure CPU time, including fractions of a core; they do not reserve dedicated physical cores. Docker enforces each container's caps, and systemd uses cgroup limits for opted-in units. The pool is admission control over those caps, so unrelated host processes still need headroom. See [Docker's resource constraints](https://docs.docker.com/engine/containers/resource_constraints/).

## Commands

```text
init [--discover] [--examples] Create a workspace with observers and/or examples
discover                      Show discovered automation without changing it
doctor                        Check configuration and runtime availability
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
```

This is a single-machine controller. It has no multi-node scheduling, secret store, image builder, or automatic migration of existing installations. Access to the controller account or Docker daemon grants control over its workloads; keep the dashboard on loopback and keep credentials out of the manifest and Git.
