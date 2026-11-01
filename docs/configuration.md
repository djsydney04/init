# Configure services with YAML or JSON

The CLI, TUI, desktop app, and optional web dashboard use the same manifest and Rust controller. A graphical interface is not required to configure, deploy, scale, or inspect services.

## Set up from a checkout

With Rust installed, build the CLI and initialize a YAML workspace:

```sh
./scripts/setup.sh --workspace ./local-workspace --config ./local-workspace/services.yaml --examples
```

The script installs `selfhost` from this checkout with the lockfile, initializes the manifest only if absent, and prints a plan. Examples are disabled until you edit them. `--discover` imports existing services as monitor-only. `--skip-install` reuses an installed binary; `--install-root DIR` changes the installation location.

To keep a hand-edited example instead of generating one:

```sh
mkdir -p ./local-workspace
cp examples/services.yaml ./local-workspace/services.yaml
./scripts/setup.sh --workspace ./local-workspace --config ./local-workspace/services.yaml
```

Edit the image, command, paths, and limits for your own services. Enable the ones you want, then review and apply:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml validate
./scripts/apply.sh --workspace ./local-workspace --config ./local-workspace/services.yaml
./scripts/apply.sh --workspace ./local-workspace --config ./local-workspace/services.yaml --apply
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml status
```

`apply.sh` defaults to planning. `--apply` changes enabled managed services only after planning succeeds. Docker must be running for Docker workloads. This script does not install a container engine or start a background controller.

`validate` checks the manifest without contacting Docker or native supervisors, so it is suitable for checking generated configuration in CI. The scripts return a nonzero exit code if initialization, planning, or applying fails. Run `./scripts/test-scripts.sh` to check the script behavior without starting services.

## Manifest format

Pass `--config path/to/services.yaml` or `.yml` to use YAML. Existing `.json` files continue to work. File extensions select the format; other extensions are rejected. Both formats deserialize into the same validated Rust schema. Invalid fields, duplicate keys, malformed documents, and files larger than 1 MiB are rejected.

Without `--config`, the CLI, desktop app, and scripts reuse the single existing `services.yaml`, `services.yml`, or `services.json` in the workspace. New workspaces use `services.yaml`. If more than one exists, specify `--config` so there is no ambiguous source of truth.

YAML accepts one document. Explicit CLI/UI configuration changes write the same format atomically with private file permissions. Values are preserved, but comments and custom formatting are not retained when those changes rewrite the manifest. Periodic allocation checks keep effective grants in separate state and leave an unchanged manifest intact. Keep credentials in a separate private `env_file`, not directly in the manifest.

```yaml
version: 1
engine:
  command: [docker]
budget:
  cpus: 2
  memory_mb: 2048
  max_parallel_jobs: 1
allocation:
  enabled: true
services:
  - id: worker
    name: Background worker
    runtime: docker
    kind: daemon
    enabled: true
    observe_only: false
    image: my-worker:local
    limits:
      cpus: 1.5
      memory_mb: 1024
    allocation:
      weight: 2
      min_cpus: 0.1
      min_memory_mb: 128
```

CPU values are cores, including fractions. Memory values are whole MiB. Commands are argument arrays passed directly to the program; shell expansion is not implicit. `observe_only` defaults to `true`, so lifecycle and scaling controls require an explicit `false`. An enabled managed Docker service needs an image and limits. Jobs use `kind: job` and an `interval_seconds` from 30 to 31,536,000 seconds.

The global budget covers managed workloads. Other host processes and monitor-only services remain outside it. Leave capacity for the operating system. Docker limits use the container engine’s capacity; Linux systemd applies CPU/memory controls to existing units. Native macOS launchd services support monitoring only.

## Manual and automatic allocation

With `allocation.enabled: false`, each service’s `limits` is its configured hard limit. Daemon reservations and running jobs must fit the workspace budget.

With `allocation.enabled: true`, `limits` is the ceiling. The Rust allocator gives enabled managed services their minimums, then shares spare CPU and memory by `weight` until services reach their ceilings. It includes idle jobs in the allocation and raises memory floors to protect observed usage with headroom. If the floors cannot fit, allocation fails instead of silently overcommitting. This is bounded sharing of limits, not prediction of future demand or a promise that arbitrary workloads will never run out of memory.

Preview before enabling automatic mode:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml allocate --enabled true
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml allocate --enabled true --apply
```

Scale a service ceiling, or change the overall budget:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml scale worker --cpus 1 --memory-mb 768
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml budget --cpus 2 --memory-mb 2048
```

The controller persists these changes to your manifest. Automatic mode recalculates effective limits; declared ceilings remain available in the configuration. To disable automatic mode, preview and apply `allocate --enabled false`. That can fail if the manual ceilings do not fit the budget.

## Run without a graphical interface

For one reconciliation of daemon state and due jobs, suitable for a timer or another supervisor:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml tick
```

For interactive terminal management:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml tui
```

The optional dashboard also runs the continuous scheduler:

```sh
selfhost --workspace ./local-workspace --config ./local-workspace/services.yaml serve --port 8787
```

Use a single workspace for the controller and its clients. The controller serializes mutations with a workspace lock. Keep any external configuration generation coordinated with it; replacing the manifest while a command is writing can lose your concurrent edit.
