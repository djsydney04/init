# Operations

## Inventory the machine

```sh
selfhost init --discover
selfhost discover
selfhost doctor
selfhost status --json
```

Discovery imports existing services as observers. Review generated IDs, labels, and paths in the workspace manifest. It prefers a working local Docker engine, then an engine in an already-running Lima VM; it does not start a VM. Docker discovery inventories containers. Native discovery looks for familiar automation names such as Actions runners, Renovate, repository maintainers, and codex-review. Add other native services directly to the manifest.

A service is not automatically moved into a container or brought under resource control. Existing services remain with their current supervisor until you explicitly opt in to management. Discovery does not inventory credential values or process arguments.

## Add your own services

Copy an entry from `examples/services.json` into the workspace manifest and give it a unique ID. Docker services accept any compatible image, an optional command array, environment file, mounts, and loopback port mappings. Set `observe_only: false`, configure `limits`, then set `enabled: true`. Managed jobs also require an `interval_seconds` from 30 to 31536000. The example custom worker and repository-maintainer images are placeholders for your own builds.

```sh
selfhost plan
selfhost doctor
selfhost apply
selfhost run renovate
selfhost logs renovate --tail 100
selfhost status
```

An absolute mount source is a path on the Docker host. A plain name such as `renovate-cache` creates/uses a Docker named volume; use distinct names between workspaces unless sharing is intentional. Container writable layers are disposable; use mounts for data that must survive replacement. Bind paths must exist on the runtime host. Pin external images to a tested version or digest before using them for important work.

Managed containers drop all Linux capabilities and use `no-new-privileges`. Use images that run without added capabilities; the web example uses [unprivileged NGINX](https://github.com/nginx/docker-nginx-unprivileged), which listens on port 8080. Privileged containers and extra capability configuration are outside this initial adapter. A Docker socket bind grants access to the engine and can create containers outside the workspace budget, so account for that separately if your CI requires it.

## Scale a service or the pool

```sh
selfhost scale renovate --cpus 1.5 --memory-mb 2048
selfhost budget --cpus 4 --memory-mb 6144
```

`scale` changes one managed service and applies its caps. `budget` changes the workspace's admission pool without rescaling its services. Validation rejects overcommitted daemon reservations and jobs that can never fit after those reservations. Active jobs consume their configured caps until they finish; an idle daemon still consumes its reservation. Edit `budget.max_parallel_jobs` in the manifest to change the scheduler's concurrency cap.

Choose caps that allow a normal run to complete. Lowering a memory cap below current usage can terminate processes. Keep the pool below the Docker host or VM allocation, leaving room for its OS, engine, controller, and observed services. The pool does not resize a Lima VM, reserve dedicated cores, or cap unrelated host processes.

## Manage an existing systemd unit

Install the unit through its usual installer first. This controller does not generate arbitrary service units. Add the existing unit with explicit limits and opt it in:

```json
{
  "id": "worker-unit",
  "runtime": "systemd",
  "kind": "daemon",
  "enabled": true,
  "observe_only": false,
  "unit": "my-worker.service",
  "scope": "system",
  "command_prefix": [],
  "limits": {"cpus": 1, "memory_mb": 1024}
}
```

For system scope, lifecycle operations and scaling use noninteractive `sudo -n systemctl`. The operator must already have permission for the trusted unit; commands fail if authentication would be required. Use root-owned, trusted system units and keep write access to the manifest restricted. User units can use `scope: "user"` with the user manager instead.

`apply` and `scale` set persistent systemd `CPUQuota` and `MemoryMax` properties. One configured CPU corresponds to a quota of 100% of one CPU's time. These limits stay in systemd until changed, even if the controller stops. See [systemd resource controls](https://www.freedesktop.org/software/systemd/man/latest/systemd.resource-control.html) and [`systemctl set-property`](https://www.freedesktop.org/software/systemd/man/latest/systemctl.html).

Child processes inherit the unit's cgroup. Containers a CI runner starts through a separate Docker daemon belong to that daemon's cgroups and need their own caps; limiting the runner unit does not automatically limit those containers.

For a unit in a Lima guest, set `command_prefix` to `["/absolute/path/to/limactl", "shell", "YOUR_VM"]`. macOS launchd entries remain observers; they do not receive Linux cgroup limits or lifecycle controls.

## Use a Lima Docker engine

The following examples use `YOUR_VM` as a placeholder. Find the installed executable and confirm the VM and engine are reachable:

```sh
command -v limactl
limactl list
limactl shell YOUR_VM docker info
limactl shell YOUR_VM printenv HOME
```

Set `engine.command` to the absolute executable path followed by `shell`, `YOUR_VM`, and `docker`. Use the guest `HOME` from the last command to fill guest paths. When a VM has no host filesystem mounts, a macOS path is not available to its Docker CLI or daemon. Environment files are read by the Docker CLI inside the guest; bind paths belong to its daemon. See [Lima filesystem mounts](https://lima-vm.io/docs/config/mount/).

Copy a custom image's build context into the guest and build there:

```sh
limactl shell YOUR_VM mkdir -p /tmp/self-host-build
limactl copy -r /absolute/path/to/repo-maintainer YOUR_VM:/tmp/self-host-build/
limactl shell YOUR_VM docker build -t repo-maintainer:local /tmp/self-host-build/repo-maintainer
```

Use `repo-maintainer:local` in the manifest. Rebuilding a tag does not replace an existing container; use `recreate` after preparing the updated image.

## Credentials

Copy `examples/secrets.env.example` into a private configuration directory, give it mode `600`, and edit only the variables the workload needs. Keep token values out of `services.json`, command arguments, images, and Git.

```sh
mkdir -p ~/.config/self-host-init
cp examples/secrets.env.example ~/.config/self-host-init/renovate.env
chmod 600 ~/.config/self-host-init/renovate.env
```

For local Docker, set `env_file` to that absolute host path. For Lima, transfer the file and install it in the guest:

```sh
limactl shell YOUR_VM sh -c 'mkdir -p "$HOME/.config/self-host-init"'
limactl copy ~/.config/self-host-init/renovate.env YOUR_VM:/tmp/self-host-init-renovate.env
limactl shell YOUR_VM sh -c 'install -m 600 /tmp/self-host-init-renovate.env "$HOME/.config/self-host-init/renovate.env"'
limactl shell YOUR_VM rm /tmp/self-host-init-renovate.env
```

Then set `env_file` to the absolute guest path, such as `/home/YOUR_GUEST_USER/.config/self-host-init/renovate.env`. Use separate files and scoped credentials for each service. Docker administrators can inspect container environments; the controller is not a secret store. Logs can contain application output and should be kept private. See [Lima's file-copy reference](https://lima-vm.io/docs/reference/limactl_copy/).

## Keep the scheduler running

`serve` must stay running for dashboard access, remote connections, and future scheduled job starts. Run one controller per workspace and use the same workspace and manifest for its clients. Before starting a background controller, stop any foreground `serve` using that workspace and port.

```sh
selfhost serve --port 8787
```

### macOS LaunchAgent

As the signed-in macOS user, initialize the workspace once, then install the per-user controller LaunchAgent:

```sh
selfhost init                 # skip if a manifest already exists
selfhost install-launchd --port 8787
curl --fail http://127.0.0.1:8787/api/health
```

Use the same `--workspace` and `--config` flags on `install-launchd` when you have custom paths. The command copies the current binary into `WORKSPACE/bin/selfhost` and writes `~/Library/LaunchAgents/io.self-host-init.controller.plist`. It is a user LaunchAgent; it needs that user's graphical login session. Logs are `WORKSPACE/logs/controller.log` and `controller.error.log`. The health response identifies the application, version, workspace, and configuration.

The VM and Docker engine must also be running. Installing the controller does not change their allocation or startup policy. Use absolute paths in runtime command prefixes because LaunchAgents have a different environment from your terminal. After upgrading the CLI, run `install-launchd` again to refresh the copied binary.

```sh
selfhost uninstall-launchd
```

Removing the controller LaunchAgent does not remove workload containers, data, or systemd properties. Stopping `serve` pauses future scheduled starts; already-running services continue under their runtime. For a maintenance window, disable scheduled jobs in the manifest and stop managed workloads explicitly.

### Linux systemd controller

This example assumes a systemd Linux server with an existing service-owning account named `deploy`, home `/home/deploy`, and a CLI installed by Cargo at `/home/deploy/.cargo/bin/selfhost`. Replace **every** `deploy` account/path below with your actual account and paths. For a downloaded CLI, use its stable absolute location in `ExecStart`. Do not run the controller as root just to bypass engine permissions.

As `deploy`, initialize once and check the exact workspace before installing the unit:

```sh
/home/deploy/.cargo/bin/selfhost --workspace /home/deploy/.local/share/self-host-init --config /home/deploy/.local/share/self-host-init/services.yaml init
/home/deploy/.cargo/bin/selfhost --workspace /home/deploy/.local/share/self-host-init --config /home/deploy/.local/share/self-host-init/services.yaml doctor
```

Skip `init` if the manifest already exists. The account needs write access to its workspace, read access to its environment files, and working access to the configured Docker engine. Engine access is powerful: use your engine's supported account/socket setup. Native systemd workloads separately require the [unit-specific management permissions](#manage-an-existing-systemd-unit) described above.

As an administrator, create `/etc/systemd/system/selfhost.service` with `sudoedit`:

```ini
[Unit]
Description=Init local service controller
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=deploy
WorkingDirectory=/home/deploy
Environment=HOME=/home/deploy
Environment=PATH=/home/deploy/.cargo/bin:/usr/local/bin:/usr/bin:/bin
ExecStart=/home/deploy/.cargo/bin/selfhost --workspace /home/deploy/.local/share/self-host-init --config /home/deploy/.local/share/self-host-init/services.yaml serve --port 8787
Restart=on-failure
RestartSec=5
TimeoutStopSec=120
UMask=0077

[Install]
WantedBy=multi-user.target
```

System services do not read your shell startup files. If the engine requires `DOCKER_HOST`, `DOCKER_CONTEXT`, or other settings, add the corresponding `Environment=` values for this account, or use an absolute `engine.command` in the manifest. For a rootless Docker socket, its user engine must also stay available after logout. Check that `git` and `tar` are in this PATH for source deployments, and that `ssh` and `tailscale` are available for remote machines and discovery. Keep workload secrets in private `env_file` files, not in the unit.

Validate the file, start it, and inspect the result:

```sh
sudo systemd-analyze verify /etc/systemd/system/selfhost.service
sudo systemctl daemon-reload
sudo systemctl enable --now selfhost.service
sudo systemctl status selfhost.service --no-pager
curl --fail http://127.0.0.1:8787/api/health
sudo journalctl -u selfhost.service -n 100 --no-pager
```

Expect an active service and a health response containing `"app":"self-host-init"` with the intended workspace and configuration paths. The controller remains on `127.0.0.1:8787`; [Machines](remote-machines.md) reaches it through SSH. No public dashboard port is needed.

If startup fails, the journal normally identifies a missing binary/manifest, unavailable engine, port conflict, or another daemon serving the workspace. Re-run `doctor` as the unit's account with the same environment. Correct file ownership rather than running a second controller as another user. After a binary upgrade, use `sudo systemctl restart selfhost.service`. After changing the unit, run `daemon-reload` before restarting. To remove its startup policy, use `sudo systemctl disable --now selfhost.service`; this keeps the workspace and running workload containers.

## Replace a container

Image/configuration drift requires deliberate replacement. Back up state first and verify that persistent data is in a bind directory or named volume:

```sh
selfhost stop web
selfhost recreate web
```

`recreate` requires a stopped, owned container and starts its replacement. Replacement discards the old container's writable layer. Mount data is retained. Updated environment file contents also require recreation for a daemon, because Docker reads that file when creating the container. Observed services cannot be recreated.

## Move an existing service gradually

Keep it as an observer first. For a container migration, create a separate disabled managed entry; prepare its image, data, credentials, and caps. Test without conflicting with production. Stop the original supervisor when ready for cutover, then enable the replacement. Avoid two CI runners with the same registration or two jobs writing to the same checkout.

For an existing systemd unit, opt it in directly after checking the unit and permissions. There is no automatic conversion of launchd/systemd units, runner registration, host paths, or credentials.

## Troubleshooting and recovery

| Symptom | Check |
| --- | --- |
| Docker unavailable | Start the VM/engine and run the configured `engine.command` followed by `info`. |
| Environment file missing | Test its absolute path on the Docker CLI machine; a host copy alone is insufficient for Lima. |
| Job cannot start | Compare daemon caps plus active job caps with the pool and `max_parallel_jobs`. |
| Container name collision | Inspect its ownership; choose a unique ID rather than deleting another workspace's container. |
| Native service unavailable | Confirm its label/unit, scope, permissions, and `command_prefix`. |
| Image/config drift | Prepare the image and persistent data, then use `recreate`. |
| Scaling rejects a limit | Confirm its value and that daemon reservations plus jobs fit the workspace budget. |

Back up the workspace manifest, scheduler state, and persistent workload directories. Keep credential backups encrypted and separate. Reinstalling this repository restores controller code; it does not restore workload data or secrets. Review `status` and `logs` after restarting the engine or controller.
