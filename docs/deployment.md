# Deploy from scripts, agents, or the terminal

Use `init deploy` for the same reviewed Docker deployment that the desktop UI provides. It accepts a **single service declaration**, previews by default, and creates the service only with `--apply`. The TUI offers the same file → preview → apply flow.

This declaration flow deploys container images. Public GitHub builds use the separate `init deploy-github` command described in [GitHub deployments](github-deployments.md); `init deploy-image` updates an existing managed daemon.

You need the `init` executable and a working Docker-compatible engine configured in the target workspace. Existing native services remain managed through their manifest and service controls; this deploy command creates Docker services. Preview contacts the engine to inspect images, ownership, capacity, and live resource usage. It does not pull images, create containers/networks/volumes, or save the declaration.

## One complete agent workflow

Run these commands from your project directory. Use a new workspace path for this first-run example; `init` refuses to overwrite an existing manifest. The checks below use Python 3. Deployment may download the chosen image and starts its command immediately.

```sh
set -eu
task_workspace="$PWD/.init-agent-example"
init --workspace "$task_workspace" init
init --workspace "$task_workspace" projects create build-tools --name "Build tools"
init --workspace "$task_workspace" projects list

cat > worker.yaml <<'YAML'
id: example-worker
name: Example worker
project: build-tools
image: alpine:3
limits:
  cpus: 0.25
  memory_mb: 128
workspace: true
command: [sleep, '3600']
YAML

init --workspace "$task_workspace" deploy worker.yaml > preview.json
python3 -c 'import json; p=json.load(open("preview.json")); assert p["ok"] and not p["applied"]; print(p["action"], p["warnings"])'

# Review the target, effective_limits, allocations, warnings, and storage first.
init --workspace "$task_workspace" deploy worker.yaml --apply > deployed.json
python3 -c 'import json; p=json.load(open("deployed.json")); assert p["ok"] and p["applied"]; print(p["container"])'
init --workspace "$task_workspace" status --json
```

The success result includes `service`, `container`, `execution`, `network`, `project`, `effective_limits`, `allocations`, `workspace`, `warnings`, and `applied`. Apply validates again against current state; a preview does not reserve capacity. A service ID must be new. To change an existing service, use its existing `scale`, lifecycle, or manifest/recreate controls.

`workspace: true` provisions the service's own persistent, initially empty Docker volume at `/workspace` and uses it as the working directory. Retained owned volumes are reused with an explicit warning, preserving their data. The volume lives on the Docker engine's host or VM, not automatically in a folder on this computer. A named project groups services and selects its private managed network; it does not share their storage or create a separate CPU budget.

## JSON, stdin, templates, and schema

```sh
init deploy --example > worker.yaml
init deploy --example --format json > worker.json
init deploy --schema > deployment.schema.json

cat worker.yaml | init --workspace "$task_workspace" deploy - > preview.json
cat worker.json | init --workspace "$task_workspace" deploy - --format json --apply
init deploy --help
```

Schema and example commands work without an initialized workspace. Files use `.yaml`, `.yml`, or `.json`; `--format yaml|json` overrides format selection. Stdin accepts YAML and JSON by default. Input is limited to 1 MiB. Unknown fields, duplicate typed fields, and unsupported service fields fail validation. The input is the bare API `service` object, **not** a full workspace manifest or an `{ "apply": true, "service": ... }` request. Mutation intent belongs in the explicit CLI flag.

The JSON Schema describes the declaration shape. The shared Rust controller remains authoritative for project references, field combinations, image references, ownership, admission, and budget checks. `command` is an argument array; no shell is added. If a shell is needed, declare it explicitly as part of the image command. `env_file` must be an absolute path on the Docker **client** machine; it references a file rather than embedding secret contents. Explicit bind-mount sources refer to the Docker engine's host.

## Scheduled jobs

Add `kind: job` and `interval_seconds` to a declaration. For example:

```yaml
id: example-job
name: Scheduled workspace task
project: build-tools
image: alpine:3
kind: job
interval_seconds: 3600
limits:
  cpus: 0.25
  memory_mb: 128
workspace: true
command: [sh, -c, 'date -u | tee -a /workspace/runs.log']
```

Save this as `job.yaml`, preview with `init --workspace "$task_workspace" deploy job.yaml`, then add `--apply` when ready. Apply runs the job immediately and saves its interval. Recurring runs require a controller daemon, for example `init --workspace "$task_workspace" serve --port 8788`. Job parallelism and the same workspace admission budget apply. Stop the controller with Ctrl+C when finished; persistent workloads and volumes are retained.

## Machine-readable failures

Deploy and project commands write one JSON success value to stdout. Failures leave stdout empty, write a JSON error to stderr, and return a nonzero exit status. `--help` and `--version` retain their normal CLI text output; `--example` writes the selected declaration format.

| Exit | Meaning |
| --- | --- |
| `0` | Success, including a read-only deployment preview |
| `2` | Invalid CLI arguments, unreadable/malformed/oversized declaration, or workspace configuration input |
| `1` | Shared controller rejection or runtime failure, including semantic declaration checks, unknown projects, ownership, admission, engine errors, or persistence failure |

Errors have this shape:

```json
{"ok":false,"error":{"code":"command_failed","message":"Actionable controller error and context"}}
```

`code` is `invalid_arguments`, `invalid_input`, or `command_failed`. Do not retry an apply blindly after a failure: the error explains whether a stopped container, retained data, or downloaded image layers may remain. Check `status --json` and the reported container before choosing the next action.

A shell can preserve the exit status and inspect the error:

```sh
if init --workspace "$task_workspace" deploy missing.yaml > result.json 2> error.json; then
  cat result.json
else
  task_status=$?
  cat error.json >&2
  exit "$task_status"
fi
```

## Deploy in the TUI

Run `init --workspace "$task_workspace" tui` in an interactive terminal and press **d**. Enter an absolute deployment-file path or one relative to the process's starting directory; Unicode and spaces are supported. Backspace removes a character, Ctrl+U clears the field, and Esc cancels.

Enter loads and validates the declaration, then shows the shared normalized deployment preview. Use ↑/↓, Page Up/Down, and Home to review it. **Enter in this review applies the exact declaration that was previewed; Esc cancels.** Editing the file after preview does not change the reviewed payload. Preview failure clears the pending operation and leaves the error visible; press d to retry. No service is created merely by loading a file.
