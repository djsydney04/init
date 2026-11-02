# Desktop app and releases

Self-host Init uses Tauri 2: a Rust desktop executable with the operating system's webview. The controller, scheduler, allocator, CLI, and TUI run in Rust. The distributed application has no Node or Electron runtime, and its static frontend needs no package installation or build step.

## Install

Download the archive or installer for your operating system and CPU from [GitHub Releases](https://github.com/djsydney04/self-host-init/releases). The CLI archive contains `selfhost`, which also provides the TUI through `selfhost tui`. Put that executable in a directory on your `PATH`.

For macOS, open the DMG and drag **Self-host Init** into Applications. For Linux, install the `.deb` on a compatible Debian/Ubuntu system or make the AppImage executable and launch it. Linux downloads are built on Ubuntu 24.04 and require glibc 2.39 or newer. Desktop packages also need a graphical session and WebKitGTK 4.1; the CLI and TUI do not require desktop libraries. If AppImage reports that FUSE is unavailable, use its documented `--appimage-extract-and-run` option.

The initial installers are **unsigned and not notarized** and are published as prereleases. macOS may require approval under System Settings → Privacy & Security before first launch. There is no automatic updater. Do not disable operating system security checks globally.

Verify the downloaded file against the release's `SHA256SUMS`:

```sh
shasum -a 256 selfhost-v0.4.0-macos-aarch64.tar.gz
```

On Linux, `sha256sum -c SHA256SUMS --ignore-missing` checks the downloads present in the current directory. A checksum detects a corrupted or changed download; it is not a publisher signature.

## Shared daemon and workspace

The desktop app uses the same default workspace as the CLI and TUI: `$XDG_DATA_HOME/self-host-init` when that environment variable is set, otherwise `~/.local/share/self-host-init`. It first verifies the local daemon's application identity, exact version, workspace, and configuration path. If port 8787 belongs to a different service, incompatible version, or different workspace, startup displays an error instead of opening that service.

If no daemon is running, the desktop app creates an empty manifest if necessary and starts the controller in its Rust process. It does not automatically import, adopt, or restart existing machine services. Quitting the app stops accepting new requests and scheduler ticks, lets current changes finish, then stops this in-process controller. Managed workloads keep running. If an independent daemon is already running, quitting the app leaves it running.

For continuous scheduling and utilization collection on macOS, install the CLI background service once:

```sh
selfhost init --discover       # only if no manifest exists yet
selfhost install-launchd
```

The generated LaunchAgent allows 120 seconds for a graceful stop. A forced kill, operating system shutdown deadline, or an external supervisor with a shorter stop timeout can still interrupt work; wait for resource changes to finish before forcing the process to exit.

On Linux, run `selfhost serve` under your service manager. The desktop app connects to that daemon. The CLI and TUI use the same Rust controller and workspace files, with locks protecting changes. Only one daemon can serve a workspace at a time.

Custom workspaces and ports are supported:

```sh
selfhost-desktop --workspace /absolute/workspace --port 8788
selfhost --workspace /absolute/workspace serve --port 8788
selfhost --workspace /absolute/workspace tui
```

The desktop window permits navigation only to its exact numeric loopback origin. It has no webview permissions for filesystem access, shell execution, or native IPC. The local API applies its own Host, Origin, and mutation-token checks.

## Upgrade

Stop the old controller before replacing the executable. Replace `selfhost` and the desktop app with the same release version, then reinstall or restart the background service. Existing manifests, state, volumes, and workload data remain in the workspace. Version 0.4.0 creates a local history database when collection starts; it does not reconstruct earlier utilization. The desktop refuses to connect to a different daemon version so UI and API contracts stay aligned.

## Build from source

Install Rust 1.89 or newer for the CLI/core, Rust 1.90 or newer for the pinned desktop bundler, and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/). On macOS, install the Xcode command line tools. On Debian/Ubuntu, install WebKitGTK 4.1 and the other packages listed in the release workflow.

```sh
cargo build --release --locked --bin selfhost
cargo run --manifest-path desktop/Cargo.toml --locked
cargo install tauri-cli --version '=2.12.4' --locked
cd desktop
CI=true cargo tauri build --ci --no-sign --bundles app,dmg -- --locked
```

On Linux use `--bundles deb,appimage` for the last command. The desktop crate has its own lockfile and is separate from the core crate, keeping CLI-only builds free of desktop dependencies. The desktop output is under `desktop/target/release/bundle/` unless `CARGO_TARGET_DIR` overrides it. The GitHub workflow sets that directory to the root `target/`.

Release CI uses Node 24 only to download and launch the official prebuilt Rust Tauri CLI through `npm exec --yes --package @tauri-apps/cli@2.12.4 -- tauri`. The exact bundler version is checked before compiling. This avoids compiling the packaging tool on every runner; it does not add Node to the app or introduce a JavaScript project. Local builds can continue using the Cargo installation shown above. Both installation methods are supported by the [Tauri CLI documentation](https://v2.tauri.app/reference/cli/).

The application icon is the MIT-licensed Phosphor **stack-fill** glyph; the original vector and license are included in `desktop/icons/`. Regenerate desktop icons with `cargo tauri icon icons/stack-fill.svg --output icons` from the desktop directory.

## Publish a version

Update the version in root `Cargo.toml`, `desktop/Cargo.toml`, and `desktop/tauri.conf.json`, regenerate both Cargo lockfiles, and commit the change. Push a matching version tag, such as `v0.4.0`. The release workflow tests both Rust crates and builds CLI/TUI archives and desktop installers on native macOS and Linux runners for x86_64 and ARM64. It uploads downloads with consistent filenames and SHA-256 checksums to a draft release, then publishes the prerelease after every build and upload succeeds.

The workflow can also be dispatched with an existing version tag to rebuild that exact source. It checks tag/version consistency before building and never runs against arbitrary untagged source.

Signed stable distribution is a separate release configuration. For Apple signing and notarization, a maintainer must supply their own Apple Developer identity/certificate and notarization credentials, configure the documented [Tauri macOS signing variables](https://v2.tauri.app/distribute/sign/macos/), remove `--no-sign`, and verify the resulting notarized installer before marking a release stable. No Apple credentials or signing keys are stored in this repository.
