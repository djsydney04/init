# Manage Init from a terminal

Install the [CLI and TUI](../README.md#cli-and-tui-only); both are included in the `init` executable. The CLI is useful for deployment files and scripts. The TUI provides interactive service management on the machine where you run it.

## Choose the controller workspace

Use the same state directory and manifest as your desktop or background controller:

```sh
init --workspace /absolute/workspace --config /absolute/workspace/services.yaml status
init --workspace /absolute/workspace --config /absolute/workspace/services.yaml tui
```

Without flags, Init uses its [default or existing legacy workspace](migration.md#keep-existing-workspaces-in-place). `--workspace` identifies a controller's state directory; named project workspaces are groups inside its manifest. `init projects list` lists those groups, and `init projects create build-tools --name "Build tools"` adds one. A deployment declaration's `project: build-tools` assigns a service to that group. The TUI currently shows all services in that manifest together.

CLI and TUI operations use the shared controller code and workspace files directly. They can coexist with the desktop and `serve`; workspace locks serialize changes. If another change holds the lock, inspect the error and retry after it finishes. Only one serving daemon can own the same workspace.

## CLI workflows

```sh
init doctor                    # Check the runtime and configuration
init status                    # List services and control modes
init status --json             # Structured state, including host_metrics
init plan                      # Inspect the intended configuration
init logs SERVICE --tail 100    # Replace SERVICE with a service ID
init deploy service.yaml       # Preview one deployment declaration
init deploy service.yaml --apply
```

The final command creates and starts the declared service. Review the preview before applying. Service lifecycle commands include `start`, `stop`, `restart`, and `run` for scheduled jobs; observer entries remain read only. `scale`, `budget`, and `allocate` control service caps and the shared resource pool. Use `init COMMAND --help` for that command's flags.

Follow the [complete declaration workflow](deployment.md) for YAML/JSON, project groups, persistent storage, and machine-readable errors. [GitHub deployments](github-deployments.md) covers `deploy-github` for public source builds and `deploy-image` for existing daemons.

## TUI controls

Run `init tui` in an interactive terminal. It refreshes its service snapshot every five seconds and shows status, CPU, memory, and resource limits.

| Key | Action |
| --- | --- |
| ↑ / ↓ or `j` / `k` | Select a service |
| Enter or `l` | Open the selected service's logs |
| `s` / `x` / `r` / `u` | Request start / stop / restart / run |
| `e` | Edit the selected service's limits |
| `b` | Edit the controller's resource budget |
| `a` | Preview switching allocation mode |
| `d` | Enter a deployment file path, then preview it |
| Enter in a confirmation | Apply the reviewed change |
| Esc | Cancel a dialog or close logs |
| `q` or Ctrl+C | Quit the TUI |

In a resource dialog, Tab or ↑/↓ selects the field, and ←/→ or `-`/`+` adjusts it. In a deployment preview, ↑/↓ or Page Up/Page Down scrolls the review; Enter applies and Esc cancels. The declaration flow deploys container images. Public GitHub builds currently use the CLI's `deploy-github` command.

Esc cancels before submission. While submitted work runs, other action keys are ignored; `q` or Ctrl+C restores the terminal and waits for that work to finish. Inside the deployment-file input, `q` is a normal filename character; use Esc to leave the input before quitting, or Ctrl+C.

The TUI does **not** run the scheduler or record utilization history. Keep `init serve` in another terminal or use the [macOS LaunchAgent](operations.md#macos-launchagent) or [Linux systemd controller](operations.md#linux-systemd-controller) for recurring jobs and history collection. Quitting the TUI leaves services running under their runtimes.

## Remote use and current limits

The terminal interfaces operate on the machine where they run. For a remote machine with Init installed, connect over SSH, then run the same commands there:

```sh
ssh -t your-user@your-host 'init tui'
```

The remote account needs Init on its `PATH`, the intended workspace, and access to its runtime. You can use its absolute executable path and workspace flags when those are not the defaults. See [SSH and Tailscale setup](remote-machines.md).

The TUI currently has no project picker, saved SSH machine picker, GitHub deployment form, per-core/network host monitor, or history charts. Use the desktop/web console for those visual workflows. Raw host measurements are available through `init status --json`; a one-off snapshot can leave CPU/network rates unknown because they need successive samples. The terminal table shows service measurements rather than the desktop's host-monitor view.
