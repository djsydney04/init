# Service SDKs

Small clients for the existing local HTTP API. Python, JavaScript/TypeScript, and Rust support the same controller operations. They do not launch a daemon, install Docker, or manage services independently of the controller.

Start `selfhost serve`, then use one of the [Python](python/README.md), [JavaScript/TypeScript](javascript/README.md), or [Rust](rust/README.md) quickstarts. Packages can be installed from this checkout; registry publication is not required. SDK version 0.3.x targets controller 0.3.x. Check `health()` before using a different controller version; future API compatibility is not guaranteed.

## Operations

| Operation | Python | JS / TypeScript | Rust |
| --- | --- | --- | --- |
| Identity / paths / version | `health()` | `health()` | `health().await` |
| Service inventory and telemetry | `state()` | `state()` | `state().await` |
| Recent log text | `logs(id, tail=200)` | `logs(id, tail=200)` | `logs(id, tail).await` |
| Service lifecycle | `action(id, action)` | `action(id, action)` | `action(id, action, None).await` |
| Apply service limits | `action(id, "apply", limits=limits)` | `action(id, "apply", limits)` | `action(id, "apply", Some(&limits)).await` |
| Preview deployment | `deploy(service)` | `deploy(service)` | `preview_deployment(&service).await` |
| Deploy | `deploy(service, apply=True)` | `deploy(service, {apply: true})` | `deploy(&service).await` |
| Create workspace group | `create_project(project)` | `createProject(project)` | `create_project(&project).await` |
| Apply global budget | `set_budget(limits)` | `setBudget(limits)` | `set_budget(&limits).await` |
| Current allocation plan | `allocation_plan()` | `allocationPlan()` | `allocation_plan().await` |
| Preview automatic allocation | `configure_allocation(enabled=True)` | `configureAllocation({enabled:true})` | `preview_allocation(&settings).await` |
| Apply automatic allocation | `configure_allocation(enabled=True, apply=True)` | `configureAllocation({enabled:true, apply:true})` | `configure_allocation(&settings).await` |

Actions are `start`, `stop`, `restart`, `run`, and `apply`. `run` is for jobs. Observer services remain read only, and the daemon enforces enabled state, ownership, capacity, and runtime restrictions. Limits use `{cpus: 1, memory_mb: 256}`; memory is in MiB. Log tail must be 1–2000. Logs are a snapshot, not a streaming connection.

## Service declaration

Minimal deployment input:

```json
{"id":"worker","image":"your-org/worker:1.0","limits":{"cpus":1,"memory_mb":256}}
```

IDs start with a lowercase letter and contain only lowercase letters, digits, and hyphens (maximum 48 characters). Optional fields are `name`, `description`, `project`, `kind` (`daemon` or `job`), `command` (argument array), `working_dir`, `workspace` (private persistent volume), `ports` (loopback mappings), `env_file`, `volumes`, `interval_seconds`, and `allocation`. A job requires an interval of at least 30 seconds. A project must already exist; create it with `{id, name, description?}`. Volume objects use `source`, `target`, optional `read_only` and `no_copy` booleans. See [configuration](../docs/configuration.md) for runtime path semantics and [the controller's deployment schema](../src/control.rs).

Preview returns the controller's normalized plan, warnings, and `applied:false`. Review it before making the explicit apply call. Applying revalidates current state; a preview does not reserve resources. Existing workspaces may require a separate user decision; the client returns the daemon's error rather than bypassing it.

Responses remain plain JSON objects (Python dictionaries / JavaScript objects / Rust `serde_json::Value`) except logs, which return a string. TypeScript includes types for request shapes. No generated copy of the entire telemetry schema is maintained.

## Connection and errors

All clients connect to `http://127.0.0.1:8787` by default (Rust takes an explicit port). Supply a different port for another local daemon. Remote hosts, TLS, and browser cross-origin access are not supported by this local controller. The JavaScript package targets Node.js 22+, not a web browser.

Each mutation fetches a fresh `/api/session` token and sends it in `X-Selfhost-Token`. Redirects are not followed. The token is local request protection, not remote authentication. Treat access to this daemon as access to its workloads.

The default timeout is 30 seconds in Python, 30,000 milliseconds in JavaScript; Rust takes a `Duration`. It applies separately to session retrieval and the operation. Increase it for slow image pulls. **No mutation is automatically retried**, including on 403, network failure, or timeout. The controller may finish an accepted operation after the client disconnects: inspect state before deciding to repeat it.

Python and JavaScript expose `ApiError.status`; Python also has `.message`. Rust exposes `Error::Api { status, message }`. Validation conflicts preserve the daemon's error text. Transport/timeout failures use native Python/JavaScript exceptions and Rust `Error::Transport`. Invalid client parameters fail before network access. The daemon validates complete deployment and resource payloads.

## Test

From the repository root:

```sh
python3 sdk/tests/contract_server.py python3 sdk/python/test_contract.py
python3 sdk/tests/contract_server.py node sdk/javascript/test_contract.mjs
python3 sdk/tests/contract_server.py cargo run --locked --manifest-path sdk/rust/Cargo.toml --example contract
cargo clippy --locked --manifest-path sdk/rust/Cargo.toml --all-targets -- -D warnings
```

Each test uses a disposable loopback server, checks the request bodies and session tokens, exercises every operation, and verifies that failed mutations are not replayed. No real workloads are changed.
