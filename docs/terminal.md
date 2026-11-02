# Manage Init from a terminal

Install the [CLI and TUI](../README.md#cli-and-tui-only); both are included in the `init` executable. The CLI is useful for deployment files and scripts. The TUI manages services and workspaces on this computer or a saved tested SSH machine. New features on `main` require a matching source build until a release includes them.

## Choose the controller workspace

Use the same state directory and manifest as your desktop or background controller:

```sh
init --workspace /absolute/workspace --config /absolute/workspace/services.yaml status
init --workspace /absolute/workspace --config /absolute/workspace/services.yaml tui
```

Without flags, Init uses its [default or existing legacy workspace](migration.md#keep-existing-workspaces-in-place). `--workspace` identifies a controller's state directory; named project workspaces are groups inside its manifest. In the TUI, `w` chooses All services, Default, or a named workspace, including an empty group. `n` opens a workspace form with ID, name, and description. Tab selects a field, Enter creates the workspace, and Esc cancels. A failed request retains the entries for correction.

`init projects list` lists named groups, and `init projects create build-tools --name "Build tools"` adds one from the CLI. A declaration's `project: build-tools` assigns a service to that group. When you deploy a file from a named workspace in the TUI, an omitted `project` defaults to the selected workspace; an explicit value in the file wins. Check the project and machine in the preview before confirming.

Local CLI and TUI operations use the shared controller code and workspace files directly. They can coexist with the desktop and `serve`; workspace locks serialize changes. If another change holds the lock, inspect the error and retry after it finishes. Only one serving daemon can own the same workspace. Remote TUI operations use the selected machine's running controller over SSH.

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

Run `init tui` in an interactive terminal. It refreshes every five seconds. The header identifies the selected machine and workspace. The service list shows status, CPU, memory, and resource limits, with a compact layout for narrower terminals. The interface uses charcoal and off-white.

| Key | Action |
| --- | --- |
| ↑ / ↓ or `j` / `k` | Select a service |
| `w` | Choose a workspace |
| `n` | Create a workspace |
| `m` | Choose this computer or a saved SSH machine |
| `/` | Search service names, IDs, and descriptions |
| `o` | Cycle state, name, CPU, and memory sorting |
| `h` | Switch between services and host monitoring |
| `?` | Open keyboard help for the current view |
| F5 | Refresh the current view |
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

Search accepts `q` as text. Enter keeps the filter, Esc restores the previous filter, and Ctrl+U clears the input. Sorting keeps unknown measurements after measured values; polling preserves the selected service ID so a reordered row does not change an action's target.

Logs refresh automatically while open. ↑/↓, `j`/`k`, or Page Up/Page Down scrolls and pauses following; Home goes to the top. `f` or End resumes following the newest lines. F5 refreshes immediately. Esc or `l` returns to services. Closing logs or changing machine prevents a late response from reopening an old log view.

Esc cancels before submission. While submitted work runs, other action keys are ignored; `q` or Ctrl+C restores the terminal and waits for that work to finish. Inside search, workspace forms, and deployment-file input, `q` is a normal character. Use Esc to leave the input before quitting, or Ctrl+C. If the terminal is smaller than 65 × 15, the TUI preserves the current form or review and blocks hidden submissions until you resize it.

## Host activity

Press `h` to inspect the selected controller's host. The view includes total and logical-core CPU bars, physical memory, available/cache/swap figures, and each network interface's receive/transmit rates and cumulative counters. ↑/↓ or Page Up/Page Down scrolls through many cores and interfaces.

Recent CPU, memory, and interface traces retain up to 60 samples in the current session. Switching machine clears them. CPU and network rates need successive readings; missing measurements and collection failures remain visible rather than appearing as measured zero. Host measurements cover the OS running the controller, which can differ from the Docker VM. See [host monitoring](monitoring.md) for units and measurement boundaries.

## Keep scheduling active

The TUI does **not** run the scheduler or record utilization history. Keep `init serve` in another terminal or use the [macOS LaunchAgent](operations.md#macos-launchagent) or [Linux systemd controller](operations.md#linux-systemd-controller) for recurring jobs and history collection. Quitting the TUI leaves services running under their runtimes.

## Saved machines and remote use

Press `m` to choose this computer or a saved tested SSH connection. The picker reads connections from the original local workspace, even while you manage a remote machine. Save and test connections through **Machines** in the desktop/web console first; untested entries cannot be opened. Each destination needs a matching Init controller running. See [SSH and Tailscale setup](remote-machines.md).

Changing machines clears the previous inventory, selection, and host traces before loading the new destination. Connection failures keep that destination selected with its error; they do not fall back to local services. Connection identity and revision are checked again before each request. A changed connection requires selecting the tested destination again. Machine changes are blocked while submitted work runs.

Deployment files are read on the computer running the TUI. Paths inside the declaration, such as environment files and bind mounts, must refer to the selected destination and its engine as described in the [deployment guide](deployment.md). Review the target before applying. An accepted remote mutation may still finish after a connection error; inspect that machine before retrying.

For direct terminal access to a remote machine with Init installed, SSH there and run the same CLI or TUI commands:

```sh
ssh -t your-user@your-host 'init tui'
```

The remote account needs Init on its `PATH`, the intended workspace, and access to its runtime. Use its absolute executable path and workspace flags when those are not the defaults.

The TUI does not yet create/test saved SSH connections, provide a GitHub deployment form, or browse recorded 24-hour service history. Use the desktop/web console for those workflows, or `init deploy-github` for source builds. `init status --json` also exposes raw host measurements; a one-off snapshot can leave CPU/network rates unknown because they need successive samples.
