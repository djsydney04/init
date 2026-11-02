<img src="assets/init-logo.png" alt="Init logo" width="96" height="96">

# Init

Init is a local service manager for Docker and native services. Its primary command is `init`, with `selfhost` retained as a compatibility alias. Install it in a user-owned directory; see [upgrading to Init](docs/migration.md) for existing installations and command-name conflicts.

The web and desktop console opens to live CPU and memory charts, service status, and service controls. **Deploy service** takes two steps: choose a machine and a starter, container image, or public GitHub repository, then review and deploy. Names and small resource limits are filled in for you; **Customize** holds the optional settings. **GitHub setup** generates a workflow to build and update an existing daemon on your own runner. See [GitHub deployments](docs/github-deployments.md).

A Rust control plane for services running on one machine, with a Tauri desktop app, CLI, TUI, and optional local web interface. Declare services in YAML or JSON, apply CPU and memory limits, and schedule jobs within a workspace budget.

Docker supplies container isolation and resource limits. Existing Linux systemd units can opt into lifecycle and resource management. macOS launchd services can join the inventory as observers while they keep their current installation. The controller, scheduler, and HTTP server are Rust; the dashboard is bundled HTML, CSS, and JavaScript.

## Start here

Follow [Getting started](docs/getting-started.md) for a complete first deployment, or use the [documentation index](docs/README.md) to find setup, Tailscale, monitoring, and operating guides.

Prebuilt desktop installers and CLI/TUI archives are available from [Releases](https://github.com/djsydney04/init/releases). The desktop checks for updates on launch and hourly, with **Update** and **Skip** in a bottom-left notice. Updates verify Init's signature before installation and reopen the app automatically. The installers are not Apple-notarized; see [installation and release details](docs/releases.md).

To configure everything from a checkout without opening a UI:

```sh
./scripts/setup.sh --examples
# Edit the generated services.yaml; examples start disabled.
./scripts/apply.sh
./scripts/apply.sh --apply
init tui
```

The first apply command only prints a plan. Pass `--config` and `--workspace` to use a configuration tracked in your own repository. [Configuration and scripts](docs/configuration.md) covers a complete noninteractive setup.

Or install the CLI directly:

```sh
git clone https://github.com/djsydney04/init.git
cd init
cargo install --path . --locked
init init --discover
init doctor
init status
init serve
```

Open [http://127.0.0.1:8787](http://127.0.0.1:8787). Discovery inventories existing services; it does not move or stop them. The dashboard and controller run locally as your user.

The interface defaults to charcoal, with an optional light theme. Workspaces brings a project's services, execution targets, private storage, utilization, and recent activity together. The service list puts running services first, supports sorting by name or measured CPU/memory, and offers direct lifecycle and Logs controls. Resources and Settings apply to the selected controller, across its projects. Updates and open log views refresh automatically.

The daemon records service CPU and memory every 15 seconds into a rolling 24-hour history that survives reloads and controller restarts. Missing measurements and time before collection began appear as gaps. These charts cover monitored services. The separate [Host monitor](docs/monitoring.md) shows logical CPU cores, physical memory and swap, and per-interface traffic across the controller's host. Docker VM capacity and service resource reservations remain distinct.

Settings controls concurrent jobs, the interface theme, and refresh frequency, and shows controller paths, runtime, enforcement, and history status. Job concurrency is saved to the manifest; theme and refresh preferences stay in the current browser or desktop webview. CPU and memory budgets remain in Resources.

Choose **Machines** to manage another Linux machine or Mac over SSH, with optional Tailscale discovery. Install the same Init version on each destination and test the connection before opening its services. [Remote machine setup](docs/remote-machines.md) covers SSH keys, Tailscale, and keeping each controller running.

Choose **Deploy service** to add a Docker image or build a public GitHub repository from the app. Use **Deploy on** to choose this computer or a tested SSH machine. Choose the Web server starter, paste your own container image, or choose **From GitHub** for a repository with a Dockerfile. The starter uses [NGINX’s unprivileged image](https://github.com/nginx/docker-nginx-unprivileged) so it runs with Init’s existing container permissions. Init fills in a unique service name and small CPU/memory limits. Open **Customize** for ports, storage, scheduling, a workspace group, command arguments, environment-file paths, or mounts. Choose **Review & deploy**, check the destination and settings, then **Deploy service**. Checking the plan does not download source, execute a build, or start anything. A daemon starts immediately; a scheduled job runs once and then follows its configured interval. The declaration is saved to the same YAML or JSON manifest used by the CLI and TUI.

For scripts and agents, `init deploy service.yaml` prints the same reviewed plan without changing anything; add `--apply` to deploy. Use `init deploy - --format json` for stdin, `init deploy --schema` for the input contract, and `init deploy --example` for a starting declaration. Success and errors are machine-readable JSON. In the TUI, press `d` to load a declaration, review it, and confirm. See [deployment from CLI, TUI, and agents](docs/deployment.md).

Named project workspaces group services on separate Docker networks. Each deployed service can also have its own empty persistent storage volume at `/workspace`, with no host folders shared by default. Project groups share the global resource pool; persistent volumes are retained until explicitly removed. [Workspace and storage configuration](docs/configuration.md#project-workspaces-and-private-storage) covers the same setup in YAML and the permissions required by non-root images.

Build with Rust 1.89 or newer. Once installed, `init` has no Python or Node runtime requirement. To run directly from a checkout, use `cargo run -- COMMAND`, or `cargo build --release` followed by `target/release/init COMMAND`. The shared Rust package is `init-core`, imported as `init_core`; the desktop executable is `init-desktop`. The desktop uses Tauri and the operating system webview; its separate crate keeps desktop dependencies out of CLI builds.

Fresh installations use `~/.local/share/init` (or `$XDG_DATA_HOME/init` when set). Existing `self-host-init` workspace directories are reused in place. If both names identify different directories, select one with `--workspace`; do not move a live workspace to rename it. New workspaces use `services.yaml`; existing `services.json` manifests are detected automatically. If multiple default manifests exist, specify `--config` explicitly. The desktop, CLI, and TUI share this selection. Use `init --workspace /absolute/path …` to keep another workspace, or `init --config /absolute/path/services.yaml …` for a different manifest. See [workspace compatibility](docs/migration.md#keep-existing-workspaces-in-place).

Review that manifest before enabling workloads. [examples/services.json](examples/services.json) shows web and worker daemons, automation jobs, and native observers. Copy the entries you want into your manifest, or initialize a new workspace with `init init --examples`. Its example entries are disabled until you supply their images, paths, and credentials. Initialization never overwrites an existing manifest. Add your own image or existing systemd unit; there is no catalog restriction.

```sh
init plan
init apply
init run renovate
init logs renovate --tail 100
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

Managed containers retain stable ownership names and labels across upgrades, including the compatibility prefix `selfhost-<workspace-hash>-<service-id>`, and have a workspace-specific Docker network. Unrelated containers and native services remain under their existing managers. Managed containers drop Linux capabilities, disallow privilege escalation, and have no Docker socket unless you explicitly mount one. Images must work within those constraints.

## Scale services and the resource pool

```sh
init scale renovate --cpus 1.5 --memory-mb 2048
init budget --cpus 4 --memory-mb 6144
```

In manual mode, `scale` changes one managed service's caps and `budget` changes the admission pool. Budget changes reject overcommit. Configured daemon caps plus active managed jobs must fit before another job starts. In automatic mode these commands also redistribute effective caps within the pool. Neither mode resizes a VM. `max_parallel_jobs` caps concurrent jobs. Observers are outside this pool.

Enable automatic allocation when you want the controller to share that pool:

```sh
init allocate --enabled true          # preview
init allocate --enabled true --apply  # enable and apply
```

In automatic mode, service `limits` are ceilings. Each service has `allocation.weight`, `min_cpus`, and `min_memory_mb`. The Rust allocator reserves the minimums, then shares spare CPU and memory by weight up to each ceiling. It checks live memory use with 25% headroom, decreases caps before increasing others, and refuses a plan that cannot fit. Effective caps are stored separately from the declared ceilings. The running controller rechecks every 15 seconds. This is bounded weighted sharing, rather than prediction of future load; idle jobs participate so they can be admitted safely.

The desktop resource panel provides sliders, manual/automatic selection, a preview, and explicit apply. The TUI offers the same operations with keyboard controls: `b` for workspace budget, `e` for service limits, `a` to preview switching allocation mode, and Enter to confirm.

CPU amounts measure CPU time, including fractions of a core; they do not reserve dedicated physical cores. Docker enforces each container's caps, and systemd uses cgroup limits for opted-in units. The pool is admission control over those caps, so unrelated host processes still need headroom. See [Docker's resource constraints](https://docs.docker.com/engine/containers/resource_constraints/).

## Commands

For local automation, use the documented [Python, JavaScript/TypeScript, and Rust SDKs](sdk/README.md). They share the controller's service actions, logs, deployment previews, workspaces, and resource settings.

Run these subcommands as `init COMMAND`; workspace initialization is `init init`.

```text
init [--discover] [--examples] Create a workspace with observers and/or examples
discover                      Show discovered automation without changing it
doctor                        Check configuration and runtime availability
validate                      Validate YAML/JSON without contacting runtimes
plan                          Show the intended managed workload configuration
apply [SERVICE]               Reconcile enabled managed daemons and resource caps
deploy FILE|- [--apply] [--format yaml|json]
                              Preview or deploy one service declaration
deploy --schema|--example     Print the deployment contract or an example
projects list                 List named project workspaces as JSON
projects create ID --name NAME [--description TEXT]
                              Create a named project workspace
status [--json]               Inspect the inventory
logs SERVICE [--tail N]       Read recent service logs
start|stop|restart SERVICE    Control a managed Docker service or systemd unit
run SERVICE                   Start a managed one-shot job
recreate SERVICE              Explicitly replace a managed Docker container
deploy-github SERVICE REPOSITORY [--reference REF] [--context DIR] [--apply]
                              Preview or create a daemon from a public GitHub Dockerfile
deploy-image SERVICE IMAGE [--build-context DIR|--pull] [--apply]
                              Preview or update an existing managed Docker daemon
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

Each controller manages one machine; the console can switch between controllers over SSH. GitHub source builds use the configured Docker engine. Init has no multi-node scheduling, secret store, or automatic migration of existing installations. Access to the controller account or Docker daemon grants control over its workloads; keep the dashboard on loopback and keep credentials out of the manifest and Git.
