# Architecture

`self-host-init` keeps a desired inventory in a YAML or JSON manifest and asks the installed runtime for actual state. The Rust controller does not implement a container runtime. Docker creates containers, networks, storage mounts, and resource controls. The systemd adapter can observe or manage existing units; the launchd adapter observes existing macOS services.

```text
YAML / JSON ──> CLI / TUI / Tauri / web ──> controller
                                              │
                         ┌────────────────────┼────────────────────┐
                         ▼                    ▼                    ▼
                    Docker engine        launchd observer     systemd adapter
                    managed / observed    existing macOS       existing Linux
                         │
                    workspace network
                    selfhost-<hash>-<service-id>
```

## Workspace

The default directory is `~/.local/share/self-host-init`, with an `XDG_DATA_HOME` override. New manifests use `services.yaml`; existing YAML, YML, or JSON defaults are detected consistently by every interface. Explicit `--config` selects a different file. Controller state records scheduling history and effective allocations. Keep this directory private and back it up alongside persistent workload data.

Container names are `selfhost-<workspace-hash>-<service-id>` and the network is `selfhost-<workspace-hash>`. The hash comes from the absolute workspace path. Ownership labels identify both workspace and service; the controller refuses to modify an unrelated same-name container. IDs can be reused in separate workspaces. Named volume sources are literal Docker volume names, so use distinct names to keep their data separate or intentionally share a volume.

Optional named projects group services within that controller workspace. Each project uses an owned `selfhost-<workspace-hash>-project-<project-id>` network; services without a project use the default network. Project membership changes require recreation of an existing container. Projects share the global budget. The deployment API can also create an owned private volume per service, mount it at `/workspace`, and prevent Docker from copying image contents into the new volume. Persistent volumes are retained after deployment failures or container removal. See [workspace and storage configuration](configuration.md#project-workspaces-and-private-storage).

## Manifest, version 1

```json
{
  "version": 1,
  "engine": {"command": ["docker"]},
  "budget": {"cpus": 3, "memory_mb": 4096, "max_parallel_jobs": 1},
  "services": [
    {
      "id": "worker",
      "runtime": "docker",
      "kind": "daemon",
      "enabled": true,
      "observe_only": false,
      "image": "my-worker:local",
      "limits": {"cpus": 1, "memory_mb": 512}
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `engine.command` | Argument array used before Docker subcommands. Use `["docker"]` locally or `["/absolute/path/to/limactl", "shell", "YOUR_VM", "docker"]` for a Lima guest. |
| `budget` | Admission caps for managed service CPU, memory, and concurrent Docker jobs. |
| `id` | Unique identifier of at most 48 characters, starting with a lowercase letter and containing lowercase letters, digits, or hyphens. Used in commands and managed container names. |
| `name`, `description` | Optional human-readable inventory labels. |
| `projects`, service `project` | Named project definitions and a service's optional project ID. References must name an existing project. |
| `runtime` | `docker`, `launchd`, or `systemd`. |
| `kind` | `daemon` for persistent work or `job` for a one-shot task. |
| `enabled` | Whether the service can be reconciled or scheduled. Disabled entries remain inventory. |
| `observe_only` | Defaults to `true`. Set `false` to opt a Docker service or existing systemd unit into lifecycle and resource management. launchd always stays observed. |
| `image` | Required image for a managed Docker workload. Build/pull it in the selected Docker daemon. |
| `container` | Existing Docker name or ID for a Docker observer. |
| `limits` | Managed service CPU cap and memory cap in MiB; required when `observe_only` is `false`. CPU must be between 0.01 and 65536; memory must be at least 6 MiB. |
| `command` | Optional argument array replacing the image's default command. No host shell expansion. |
| `working_dir` | Optional absolute container working directory. |
| `env_file` | Docker CLI environment file path. With Lima this must exist inside the guest. |
| `volumes` | List of `{ "source": "/data/path", "target": "/data", "read_only": false }` mounts. An absolute source is a bind path on the Docker host; a plain name is a Docker named volume. Optional `no_copy: true` keeps a newly created named volume empty. |
| `ports` | Docker publish mappings such as `127.0.0.1:8000:8000`. |
| `interval_seconds` | Required for managed Docker jobs, between 30 and 31536000. A scheduling pass starts a due job only when admission checks pass. Use systemd timers for native jobs. |
| `label`, `log_paths` | launchd label and optional host log files. |
| `unit`, `command_prefix` | systemd unit and optional argument prefix, such as `["/absolute/path/to/limactl", "shell", "YOUR_VM"]`. |
| `scope` | systemd `system` (default) or `user`. System management uses noninteractive sudo; user scope uses `systemctl --user`. |

Use absolute paths. Environment files belong outside the repository. The manifest is executable configuration: anyone able to edit image, command, mounts, or runtime command prefixes can change what the controller executes.

## Reconciliation and scheduling

`plan` reads desired configuration without starting workloads. `apply` reconciles enabled managed daemons. Existing matching containers can receive updated CPU/memory caps. A different image, command, environment file path, mounts, or ports requires `recreate` so an ordinary apply cannot discard state. Opted-in systemd units receive CPU and memory properties through their existing manager.

Jobs use the same capped containers. Before starting one, the controller checks its current state, rejects overlap, checks `max_parallel_jobs`, and adds its requested caps to enabled daemon caps and active jobs. A due job that cannot fit waits for a later tick. Admission uses configured caps rather than current utilization, so an idle daemon still consumes its allowance.

Never-run and oldest-run jobs get priority when multiple jobs are due. State survives controller restarts, and workspace file locks serialize mutations across the CLI and dashboard. Running owned Docker containers still count after their declarations are disabled or removed. Adopted systemd identities are retained in state and their actual quotas count until they stop, including during startup and shutdown. An unlimited adopted unit blocks new work until finite limits are applied. Initial observers remain outside the pool.

In manual mode, `scale` applies one service's configured caps and `budget` changes the shared admission pool. With `allocation.enabled: true`, configured limits become ceilings, and the controller shares the pool with bounded weighted allocation above each service's minimums. Live memory raises a service's floor to measured usage plus 25% headroom. Ceilings and effective grants are stored separately. Reductions happen before increases, and failures attempt to restore the previous caps. See [declarative allocation configuration](configuration.md). This capacity ledger is not an aggregate cgroup around unrelated services.

The controller is a single-machine scheduler. Keep one long-running `serve` process per workspace. Observers are visible but outside the managed budget. Their resource use, the controller, Docker engine, and any VM still need headroom. On macOS the Linux VM is an additional ceiling: a pool cannot create capacity beyond the VM's CPU and memory allocation. [Docker documents CPU and memory limits](https://docs.docker.com/engine/containers/resource_constraints/); [Lima documents how host paths are exposed to a guest](https://lima-vm.io/docs/config/mount/).

## Live monitoring

Snapshots report measured CPU and memory separately from limits and reservations. CPU uses 100% for one logical core. Docker reports engine measurements; native observers use available process or systemd counters, with interval CPU deltas and an unknown first sample where needed. Missing counters remain unknown. Execution metadata identifies the host, Lima guest, or remote target rather than treating every PID as local.

The Activity Monitor totals cover monitored services only. Coverage counts expose missing measurements; these are not whole-machine totals. The frontend keeps timestamped samples in memory for at most one hour, clears them on reload, and leaves gaps when measurements are unavailable. Project filters scope the graphs and tables, while resource allocation remains global.

## Remote machine connections

The local console can select another controller through a saved SSH connection. Each destination retains its own manifest, runtime adapters, scheduler, and resource budget. Dashboard requests travel through OpenSSH stdio forwarding to the remote loopback listener; writes obtain the remote session token on the same connection. Remote identity and saved-profile revisions prevent silently redirecting an open view to another workspace. Tailscale is optional address discovery and private network connectivity, not a new scheduling backend. See [remote machine setup](remote-machines.md).
