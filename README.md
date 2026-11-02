<img src="assets/init-logo.png" alt="Init logo" width="96" height="96">

# Init

Init is a local service manager for Docker and native services. Its primary command is `init`, with `selfhost` retained as a compatibility alias. Install it in a user-owned directory; see [upgrading to Init](docs/migration.md) for existing installations and command-name conflicts.

A Rust control plane for services running on one machine, with a Tauri desktop app, CLI, TUI, and optional local web interface. Declare services in YAML or JSON, apply CPU and memory limits, and schedule jobs within a workspace budget.

Docker supplies container isolation and resource limits. Existing Linux systemd units can opt into lifecycle and resource management. macOS launchd services can join the inventory as observers while they keep their current installation. The controller, scheduler, and HTTP server are Rust; the dashboard is bundled HTML, CSS, and JavaScript.

## Start here

Follow [Getting started](docs/getting-started.md) for a complete first deployment, or use the [documentation index](docs/README.md) to find setup, Tailscale, monitoring, and operating guides.

## Install

Choose the setup you want. The TUI is included in the CLI executable; it does not need a separate package. The desktop checks GitHub for updates on launch and hourly, then offers **Update** or **Skip** in a compact notice. Updates verify the package signature and reopen Init automatically. See [automatic updates and manual controller upgrades](docs/releases.md#upgrade).

| Setup | Install | Start with |
| --- | --- | --- |
| [Desktop only](#desktop-only) | Init desktop package, including its own controller | Open **Init** |
| [CLI and TUI only](#cli-and-tui-only) | `init` CLI archive | `init tui` or individual commands |
| [Desktop and CLI](#desktop-and-cli) | Both packages from the same release | Either interface, using the same workspace |

Downloads are on [Releases](https://github.com/djsydney04/init/releases). Choose the suffix for your computer:

| Computer | Package suffix | Desktop format |
| --- | --- | --- |
| macOS, Apple silicon | `macos-aarch64` | `.dmg` |
| macOS, Intel | `macos-x86_64` | `.dmg` |
| Linux, x86-64 | `linux-x86_64` | `.deb` or `.AppImage` |
| Linux, ARM64 | `linux-aarch64` | `.deb` or `.AppImage` |

Linux downloads require glibc 2.39 or newer, such as Ubuntu 24.04. Container workloads also need a running Docker-compatible engine that your account can use. Init does not install the engine. The controller and terminal interfaces need no Node or Python runtime. Published packages are release snapshots; build from source below for changes on `main` that have not been released yet.

### Desktop only

1. Download `init-desktop-VERSION-PLATFORM.dmg`, `.deb`, or `.AppImage` from the selected release and verify it against that release's `SHA256SUMS`.
2. On macOS, open the DMG and drag **Init** into Applications, then open it. On Debian/Ubuntu, install the downloaded `.deb`; alternatively, make the AppImage executable and run it. For example, from your download directory on Linux x86-64:

   ```sh
   sudo apt install ./init-desktop-v0.5.0-linux-x86_64.deb
   # Or use the AppImage instead:
   chmod +x init-desktop-v0.5.0-linux-x86_64.AppImage
   ./init-desktop-v0.5.0-linux-x86_64.AppImage
   ```

3. Init creates an empty workspace on first launch and starts its controller. If a matching controller is already running for that workspace, the app connects to it.

Linux desktop packages need a graphical session and WebKitGTK 4.1. The v0.5.0 desktop packages are prereleases, unsigned and not Apple-notarized; see [download verification and platform requirements](docs/releases.md#install) for first-launch details. Installing the desktop app does not put the CLI on your shell's `PATH`.

### CLI and TUI only

Download `init-VERSION-PLATFORM.tar.gz` and `SHA256SUMS` from the same release. This example installs the v0.5.0 Apple silicon archive; change the filename for your platform using the table above. Run from the directory containing your downloads, after [verifying the checksum](docs/releases.md#install):

```sh
mkdir -p "$HOME/.local/bin"
tar -xzf init-v0.5.0-macos-aarch64.tar.gz -C "$HOME/.local/bin" init selfhost
export PATH="$HOME/.local/bin:$PATH"
init --version
init --help
```

Add that `export PATH` line to your shell's startup file (`~/.zshrc` for Zsh or `~/.bashrc` for interactive Bash) to keep it in future terminals. The archive includes `selfhost` for existing scripts. Keep both binaries in a user-owned directory; if `command -v init` finds the operating system's command, use `~/.local/bin/init` explicitly. Never replace `/sbin/init` or `/usr/sbin/init`.

For a new workspace:

```sh
init init
init doctor
init status
init tui
```

If a manifest already exists, skip `init init`. Use `init init --discover` instead of plain initialization when you want existing services added as observers; this does not adopt, move, or stop them. Press `q` to leave the TUI. It does not run scheduled jobs or collect history in the background: keep `init serve` running in another terminal, or install the [macOS LaunchAgent](docs/operations.md#macos-launchagent) / [Linux systemd controller](docs/operations.md#linux-systemd-controller). `serve` also provides the optional dashboard at [http://127.0.0.1:8787](http://127.0.0.1:8787).

To install the current source with Git and Rust 1.89 or newer:

```sh
git clone https://github.com/djsydney04/init.git
cd init
cargo install --path . --locked
export PATH="$HOME/.cargo/bin:$PATH"
init --version
```

Cargo installs both commands in `~/.cargo/bin` by default. To initialize and print a plan from a checkout in one step, use `./scripts/setup.sh --examples`; examples start disabled. Review `./scripts/apply.sh` before using `./scripts/apply.sh --apply`. [Configuration and scripts](docs/configuration.md) covers custom `--workspace` and `--config` paths.

### Desktop and CLI

Install the desktop package and CLI archive from the **same release**, using the two paths above. There is no separate combined installer. Both use the same default workspace and manifest; see [workspace compatibility](docs/migration.md#keep-existing-workspaces-in-place) for existing installations. The CLI and TUI can run alongside the desktop; changes are protected by workspace locks.

Before installing a background controller, quit **Init** and stop any foreground `init serve` for this workspace. This releases the port and workspace if the desktop was hosting its own controller. On macOS, initialize once if needed, then run:

```sh
init install-launchd --port 8787
```

Open **Init** afterward; it connects to that background controller. Quitting the app leaves this independent controller running. On Linux, follow the [systemd setup](docs/operations.md#linux-systemd-controller). Use the same explicit `--workspace` and `--config` paths for every interface when keeping a custom workspace. Only one serving daemon can own a workspace at a time.

After upgrading the CLI on macOS, run `init install-launchd` again with the same paths and port to update the copied controller binary. On Linux, restart the existing controller service after replacing its binary. Reload or reopen the desktop afterward: a running controller continues serving the UI embedded in its old executable until it is restarted. See [upgrade details](docs/releases.md#upgrade).

## Manage from the terminal

The CLI supports inventory, logs, lifecycle actions, CPU/memory limits, budget allocation, named workspace creation, deployment declarations, and public GitHub deployments. Commands that support previews require an explicit apply step before changing anything.

The TUI provides workspace and tested-machine pickers, service search and sorting, refreshed logs, lifecycle controls, resource budgets, and reviewed deployment from a file. Use `w` to choose a workspace, `n` to create one, `m` to choose this computer or a saved tested SSH machine, `/` to search, and `o` to change the sort order. Selection stays with the same service when polling reorders the list. Deployment files without a `project` inherit the selected named workspace; the review shows the effective destination and group.

Use `h` for the host monitor with total and per-core CPU, physical memory, swap, and per-interface network rates. Its recent traces stay in the current TUI session and reset on a machine switch. `?` opens keyboard help, and F5 refreshes the current view. Existing service and resource controls remain available. Enter confirms a review; Esc cancels it. See the [terminal guide](docs/terminal.md) for the full keymap, remote setup, and scheduling requirements.

## Desktop and web console

The web and desktop console is organized around workspaces: the sidebar lists each workspace with its services, and each workspace has Overview, Services, Usage and Activity views. Below the workspaces, the machine selector stays visible and **Machine tools** holds the machine-wide pages: Host monitor, Resources, GitHub setup, Machines and Settings. **Add service** — in the page header or under any workspace in the sidebar — takes two steps: choose a machine and a starter, container image, or public GitHub repository, then review and deploy. Names and small resource limits are filled in for you; **Customize** holds the optional settings. **GitHub setup** generates a workflow to build and update an existing daemon on your own runner. See [GitHub deployments](docs/github-deployments.md).

The interface defaults to charcoal, with an optional light theme. Workspaces brings a project's services, execution targets, private storage, utilization, and recent activity together. The service list puts running services first, supports sorting by name or measured CPU/memory, and offers direct lifecycle and Logs controls. Resources and Settings apply to the selected controller, across its projects. Updates and open log views refresh automatically.

The daemon records service CPU and memory every 15 seconds into a rolling 24-hour history that survives reloads and controller restarts. Missing measurements and time before collection began appear as gaps. These charts cover monitored services. The separate [Host monitor](docs/monitoring.md) shows logical CPU cores, physical memory and swap, and per-interface traffic across the controller's host. Docker VM capacity and service resource reservations remain distinct.

Settings controls concurrent jobs, the interface theme, and refresh frequency, and shows controller paths, runtime, enforcement, and history status. Job concurrency is saved to the manifest; theme and refresh preferences stay in the current browser or desktop webview. CPU and memory budgets remain in Resources.

Choose **Machines** to manage another Linux machine or Mac over SSH, with optional Tailscale discovery. Install the same Init version on each destination and test the connection before opening its services. [Remote machine setup](docs/remote-machines.md) covers SSH keys, Tailscale, and keeping each controller running.

Choose **New workspace** in the sidebar to group related services. **Create on** defaults to the machine you are viewing; you can also choose this computer or another tested SSH connection. After creation, Init opens the workspace on that machine. If creation fails, your entries and destination stay in the form so you can correct the issue and try again.

Choose **Add service** to add a Docker image or build a public GitHub repository from the app. Use **Deploy on** to choose this computer or a tested SSH machine. Choose the Web server starter, paste your own container image, or choose **From GitHub** for a repository with a Dockerfile. The starter uses [NGINX’s unprivileged image](https://github.com/nginx/docker-nginx-unprivileged) so it runs with Init’s existing container permissions. Init fills in a unique service name and small CPU/memory limits. Open **Customize** for ports, storage, scheduling, a workspace group, command arguments, environment-file paths, or mounts. Choose **Review & deploy**, check the destination and settings, then **Deploy service**. Checking the plan does not download source, execute a build, or start anything. A daemon starts immediately; a scheduled job runs once and then follows its configured interval. The declaration is saved to the same YAML or JSON manifest used by the CLI and TUI.

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
python3 scripts/test-tui.py target/debug/init
bash scripts/test-scripts.sh
```

Each controller manages one machine; the console can switch between controllers over SSH. GitHub source builds use the configured Docker engine. Init has no multi-node scheduling, secret store, or automatic migration of existing installations. Access to the controller account or Docker daemon grants control over its workloads; keep the dashboard on loopback and keep credentials out of the manifest and Git.
