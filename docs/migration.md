# Upgrade to Init

Version 0.5.0 names the product **Init**, the CLI `init`, and the desktop executable `init-desktop`. The repository is [djsydney04/init](https://github.com/djsydney04/init). Existing service declarations, workspace data, and workload ownership remain compatible.

## Install the user-local command

Install matching CLI and desktop releases using the [release guide](releases.md). The CLI archive includes both `init` and the previous `selfhost` executable; existing scripts can keep using `selfhost`. New commands and help use `init`, including `init init` to initialize a workspace.

Keep the executable in a user-owned directory. A downloaded installation can use `~/.local/bin/init`; `cargo install --path . --locked` installs into `~/.cargo/bin` by default. If your shell finds the operating system's command instead, invoke the installed path explicitly:

```sh
~/.local/bin/init --version
# Or, for a Cargo installation:
~/.cargo/bin/init --help
```

Never replace `/sbin/init` or `/usr/sbin/init`, and do not use `sudo` to overwrite an operating system executable with this service manager.

For an earlier Cargo installation, the Rust package name changes from `self-host-init` to `init-core`. Run `./scripts/setup.sh` from this checkout to transfer the installed executable names; it verifies existing targets belong to Init before replacing them. For a direct source upgrade, first verify your user-local `init` (if present) and `selfhost` executables belong to this project, then run `cargo install --path . --locked --force`. Cargo otherwise refuses to overwrite `selfhost` owned by the previous package.

## Keep existing workspaces in place

All interfaces use the same selection rules:

| Directories under the data home | Selected workspace |
| --- | --- |
| Neither directory exists | `init`, created when initialized |
| Only `init` exists | `init` |
| Only `self-host-init` exists | Existing `self-host-init`, reused without moving data |
| Both resolve to the same directory | The same workspace, using its `init` path |
| Both exist as separate directories | Selection fails; pass `--workspace` explicitly |

The data home is an absolute `$XDG_DATA_HOME` when set, otherwise `~/.local/share`. An empty `XDG_DATA_HOME` uses the home-directory default. For an existing installation, select its original path explicitly when needed:

```sh
init --workspace "$HOME/.local/share/self-host-init" status --json
init --workspace "$HOME/.local/share/self-host-init" serve --port 8787
```

Run the foreground `serve` example only after stopping the controller already using that workspace. Keep using the existing manifest, state, history database, and persistent volumes. Do not rename or move a runtime workspace as part of this branding change: Docker ownership is derived from its canonical path. A different path can identify a different controller and its resources.

On macOS, stop the old controller, install the matching new CLI and app, then run `init install-launchd` with the existing `--workspace` and `--config` flags if custom paths are in use. It replaces the old controller registration with `io.init.controller` and keeps a `bin/selfhost` compatibility executable. On Linux, update the existing unit's executable path and keep its original workspace/configuration paths; then restart that unit. See [background operation](operations.md#keep-the-scheduler-running).

## API and SDK names

The health response now reports `"app":"init"`. Init's desktop and machine-connection checks also recognize the previous `"app":"self-host-init"` identity, while still requiring the expected version, workspace, and configuration. Upgrade both ends of a connection together; recognizing an old name does not bypass version checks.

New clients send `X-Init-Token`. The controller also accepts `X-Selfhost-Token` for existing clients, with the same token checks. Send exactly one token header; mixed or duplicate headers are rejected. Requesting a token or retaining an old header name does not grant remote access; the controller remains on loopback.

All three SDK packages are `init-sdk`; Python and Rust import `init_sdk`, and JavaScript imports `init-sdk`. Python's `selfhost` module re-exports the same client classes. Existing Rust and JavaScript imports can use dependency aliases; see the [language quickstarts](../sdk/README.md). The shared controller package is `init-core`, imported as `init_core`.

Some storage identifiers intentionally keep their previous values: Docker container/network/volume prefixes and ownership labels, the SQLite application ID, and the macOS bundle identifier. They preserve ownership, recorded history, and desktop preferences. Changing them solely to remove an old name would break those upgrade contracts.
