# Desktop app and releases

Init uses Tauri 2: a Rust desktop executable with the operating system's webview. The controller, scheduler, allocator, CLI, and TUI run in Rust. The distributed application has no Node or Electron runtime, and its static frontend needs no package installation or build step.

## Install

Download the archive or installer for your operating system and CPU from [GitHub Releases](https://github.com/djsydney04/init/releases). The CLI archive contains `init`, which also provides the TUI through `init tui`, and the compatibility executable `selfhost`. Put them in a user-owned directory on your `PATH`, such as `~/.local/bin`. Use that explicit path if your shell resolves the operating system's `init`; never replace `/sbin/init` or `/usr/sbin/init`.

For macOS, open the DMG and drag **Init** into Applications. For Linux, install the `.deb` on a compatible Debian/Ubuntu system or make the AppImage executable and launch it. Linux downloads are built on Ubuntu 24.04 and require glibc 2.39 or newer. Desktop packages also need a graphical session and WebKitGTK 4.1; the CLI and TUI do not require desktop libraries. If AppImage reports that FUSE is unavailable, use its documented `--appimage-extract-and-run` option.

The installers are published as prereleases. They are **not signed with an Apple Developer identity or notarized**. macOS may require approval under System Settings → Privacy & Security before first launch. Desktop update packages have a separate cryptographic signature that Init verifies before installation. Do not disable operating system security checks globally.

Verify the downloaded file against the release's `SHA256SUMS`:

```sh
shasum -a 256 init-v0.5.0-macos-aarch64.tar.gz
```

On Linux, `sha256sum -c SHA256SUMS --ignore-missing` checks the downloads present in the current directory. A checksum detects a corrupted or changed download; it is not a publisher signature.

## Shared daemon and workspace

The desktop app uses the same default workspace as the CLI and TUI: `$XDG_DATA_HOME/init` when that environment variable is set, otherwise `~/.local/share/init`, with existing `self-host-init` directories reused in place. If both names identify separate directories, pass `--workspace` explicitly. It first verifies the local daemon's application identity, exact version, workspace, and configuration path. If port 8787 belongs to a different service, incompatible version, or different workspace, startup displays an error instead of opening that service.

If no daemon is running, the desktop app creates an empty manifest if necessary and starts the controller in its Rust process. It does not automatically import, adopt, or restart existing machine services. Quitting the app stops accepting new requests and scheduler ticks, lets current changes finish, then stops this in-process controller. Managed workloads keep running. If an independent daemon is already running, quitting the app leaves it running.

For continuous scheduling and utilization collection on macOS, install the CLI background service once:

```sh
init init --discover       # only if no manifest exists yet
init install-launchd
```

The generated LaunchAgent allows 120 seconds for a graceful stop. A forced kill, operating system shutdown deadline, or an external supervisor with a shorter stop timeout can still interrupt work; wait for resource changes to finish before forcing the process to exit.

On Linux, use the [complete systemd controller example](operations.md#linux-systemd-controller) to run `init serve` with an explicit account, workspace, environment, and restart policy. The desktop app connects to that daemon. The CLI and TUI use the same Rust controller and workspace files, with locks protecting changes. Only one daemon can serve a workspace at a time.

Custom workspaces and ports are supported:

```sh
init-desktop --workspace /absolute/workspace --port 8788
init --workspace /absolute/workspace serve --port 8788
init --workspace /absolute/workspace tui
```

The desktop window permits navigation only to its exact numeric loopback origin. Its native IPC permissions allow only desktop update status, installation, and session dismissal. It has no webview permissions for filesystem access or shell execution. The local API applies its own Host, Origin, and mutation-token checks.

## Upgrade

The desktop app checks GitHub for updates when it opens and every hour while it runs. A small notice at the bottom left shows **Update** and **Skip** when a newer release is available. **Update** downloads the package for the installed format and CPU, verifies its signature and signed version, installs it, and reopens Init automatically. The in-process controller finishes pending changes before Init restarts. **Skip** hides the notice until the next app launch, including across page reloads. Failed background checks stay quiet and retry on the next hourly check. A failed installation offers a retry.

Automatic updates support the macOS app, Linux AppImage, and Linux Debian package. Debian upgrades may ask for administrator authentication through the system package installer. The Linux desktop needs a graphical session for that prompt. Keep macOS apps and AppImages in a writable installation location.

The updater replaces the desktop installation. It does not replace separately installed CLI executables, SDKs, or independently managed controllers. If Init is connected to an independent controller, the update notice explains that the controller must be upgraded together with the app. Installation is blocked before any files change, preserving the working connection. Use the procedure below for that configuration.

Existing releases through v0.5.0 do not contain this updater. Install a release containing it once to enable future automatic updates.

Stop the old controller before replacing the executable. Replace `init`, the compatibility executable, and the desktop app with the same release version, then reinstall or restart the background service. Existing manifests, state, volumes, and workload data remain in their original workspace. Version 0.4.0 introduced the local history database; it does not reconstruct utilization from before collection began. The desktop refuses to connect to a different daemon version so UI and API contracts stay aligned. For the 0.5.0 rename, follow [Init naming and migration](migration.md).

## Build from source

Install Rust 1.89 or newer for the CLI/core, Rust 1.90 or newer for the desktop, and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/). On macOS, install the Xcode command line tools. On Debian/Ubuntu, install WebKitGTK 4.1 and the other packages listed in the release workflow.

```sh
cargo build --release --locked --bin init
cargo run --manifest-path desktop/Cargo.toml --locked
cargo install tauri-cli --version '=2.12.4' --locked
cd desktop
CI=true cargo tauri build --ci --no-sign --bundles app,dmg --config '{"bundle":{"createUpdaterArtifacts":false}}' -- --locked
```

This local packaging command disables updater artifact signing. Official releases require the updater signing key and use the checked-in configuration. On Linux use `--bundles deb,appimage` for the last command. The desktop crate has its own lockfile and is separate from the core crate, keeping CLI-only builds free of desktop dependencies. The desktop output is under `desktop/target/release/bundle/` unless `CARGO_TARGET_DIR` overrides it. The GitHub workflow sets that directory to the root `target/`.

Release CI uses Node 24 only to download and launch the official prebuilt Rust Tauri CLI through `npm exec --yes --package @tauri-apps/cli@2.12.4 -- tauri`. The exact bundler version is checked before compiling. This avoids compiling the packaging tool on every runner; it does not add Node to the app or introduce a JavaScript project. Local builds can continue using the Cargo installation shown above. Both installation methods are supported by the [Tauri CLI documentation](https://v2.tauri.app/reference/cli/).

The application icon uses the approved heavy uppercase **I** in a charcoal badge with one clipped corner. Its master PNG, desktop exports, charcoal and off-white palette, and generation prompt are documented in [Init identity](brand.md).

## Publish a version

Update the version in root `Cargo.toml`, `desktop/Cargo.toml`, `desktop/tauri.conf.json`, and all three SDK package manifests. Regenerate the relevant Cargo lockfiles and commit the change. Push a matching numeric version tag, such as `v0.6.0`. The release workflow tests both Rust crates and builds CLI/TUI archives and desktop installers on native macOS and Linux runners for x86_64 and ARM64. It uploads downloads with consistent filenames and SHA-256 checksums to a draft release, then publishes the prerelease after every build and upload succeeds.

GitHub Actions requires the repository secret `TAURI_SIGNING_PRIVATE_KEY`. The matching public key is embedded in `desktop/tauri.conf.json`. `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` is optional for a password-protected key. Keep an offline backup of the private key. Losing it prevents existing installations from accepting future updates. Never commit a private key or replace the public key for an existing installation without a key migration plan. The [Tauri updater documentation](https://v2.tauri.app/plugin/updater/) explains key generation and signing.

The workflow uses Tauri's original generated archives and `.sig` files. Repacking an archive after signing invalidates its signature. `scripts/updater-manifest.py` requires all six signed package variants before it creates `latest.json`. Payload URLs point to the versioned release. The workflow publishes that release before replacing the manifest on the dedicated `desktop-updates` prerelease. Init reads this fixed channel, so ordinary prereleases remain eligible without depending on GitHub's stable-only `/latest` URL. Publication is serialized, and an older version cannot replace a newer channel manifest. The channel's release notes retain its version independently of the replaceable asset. A failed upload can be retried even if GitHub removed the old asset. Channel lookup errors stop publication unless GitHub confirms that the channel does not exist.

Run `python3 scripts/test-updater-manifest.py` to check platform mappings, missing signatures, and channel version ordering. After a release, inspect `https://github.com/djsydney04/init/releases/download/desktop-updates/latest.json` and confirm its version and platform URLs match the published assets.

The workflow can also be dispatched with an existing version tag. It checks tag/version consistency before building and never runs against arbitrary untagged source. Draft release uploads can be retried. Published release assets are immutable: reruns reuse the published `latest.json` when they retry channel publication, rather than replacing packages that installed apps may already be downloading. A published legacy release without updater metadata requires a new version tag.

Apple signing and notarization are separate from updater signatures. A maintainer must supply their own Apple Developer identity/certificate and notarization credentials, configure the documented [Tauri macOS signing variables](https://v2.tauri.app/distribute/sign/macos/), and verify the resulting notarized installer before marking a release stable. Release builds must not pass `--no-sign`, because that flag also disables updater signatures. No Apple credentials or private signing keys are stored in this repository.

## SDK packages

Releases also include Python, JavaScript/TypeScript, and Rust SDK packages. The SDK job checks their HTTP contracts, builds and verifies the packages, and must pass alongside all desktop builds before publication. All app and SDK downloads, updater signatures, and the release's `latest.json` receive entries in `SHA256SUMS`.

Before tagging the next version, update `sdk/python/pyproject.toml`, `sdk/javascript/package.json`, and `sdk/rust/Cargo.toml` to match the controller, then regenerate `sdk/rust/Cargo.lock` with `cargo check --manifest-path sdk/rust/Cargo.toml`. Run `python3 scripts/check-sdk-versions.py vX.Y.Z` using Python 3.11+. Use matching numeric `X.Y.Z` package versions; GitHub's prerelease designation is independent of the package version. Publish a new tag after these changes merge; rebuilding an older tag uses its older workflow and will not add SDKs.

Download and verify the desired SDK asset, then install it locally:

```sh
python3 -m pip install ./init_sdk-X.Y.Z-py3-none-any.whl
npm install ./init-sdk-X.Y.Z.tgz
# Rust: unpack the crate, then point your Cargo dependency at that directory.
mkdir -p vendor
tar -xzf init-sdk-X.Y.Z.crate -C vendor
```

Rust applications can use `init-sdk = { path = "vendor/init-sdk-X.Y.Z" }` in `[dependencies]`, imported as `init_sdk`. Python also uses `init_sdk`; JavaScript imports `init-sdk`. The Python source archive is available for source builds. Each package contains its README and license. See [SDK documentation](../sdk/README.md) for examples, compatibility aliases, and the API reference.

This workflow publishes downloads to GitHub Releases only. It requires no SDK registry tokens and does not reserve names or publish packages on PyPI, npm, or crates.io. Registry publication can be added once package ownership and trusted publishing are configured.
