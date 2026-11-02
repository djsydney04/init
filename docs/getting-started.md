# Deploy your first service

This guide starts one local Init controller and an NGINX container, then verifies both. For a remote Linux server or Mac, finish this guide on that destination before [connecting it over SSH or Tailscale](remote-machines.md).

## 1. Install the controller and container engine

Install the CLI archive for your OS and architecture from [Releases](https://github.com/djsydney04/init/releases), and put `init` on your `PATH`. See [installation and upgrade details](releases.md) for desktop packages and download verification. New features described on this branch require a matching build; an older release may not include them.

You also need a running Docker-compatible engine to deploy containers. Install Docker Engine on Linux or use a working Docker Desktop/Lima engine on macOS. The account running Init must already be able to use that engine. Init does not install or start it.

For a source installation, run these commands in a terminal with Git and Rust 1.89 or newer installed:

```sh
git clone https://github.com/djsydney04/init.git
cd init
cargo install --path . --locked
```

Cargo installs `init` and the compatibility alias `selfhost` into `~/.cargo/bin` by default. Use `~/.cargo/bin/init --help` if your shell finds the operating system's `init` instead; downloaded installations can use `~/.local/bin/init`. Never replace `/sbin/init` or `/usr/sbin/init`. There is no Node or Python runtime dependency for the controller.

## 2. Create a workspace and start Init

Run as the account that will own the services:

```sh
init init
init doctor
init status
init serve --port 8787
```

`init init` creates an empty `services.yaml` and state directory at `~/.local/share/init`, or `$XDG_DATA_HOME/init` if set. An existing `self-host-init` directory under the same data home is reused without moving it; see [migration](migration.md) if both directories exist. Initialization never overwrites an existing manifest: if one already exists, continue with `init doctor`. Use `init init --discover` instead when you want existing services imported as observers.

Keep this terminal running and open [http://127.0.0.1:8787](http://127.0.0.1:8787). `serve` runs both the dashboard and scheduler. To select another workspace, use the same `--workspace /absolute/path` on every command, including `serve`.

If `doctor` cannot reach Docker, start the engine and inspect the `engine.command` in the generated manifest. With Lima, use [the engine and path checks](operations.md#use-a-lima-docker-engine). If the port is occupied, choose `serve --port 8788` and open that port instead.

## 3. Review and deploy a web service

1. Choose **Add service** (in the header, or under a workspace in the sidebar) and leave **Deploy on** set to this computer.
2. Choose **Web server**. Init fills in an unprivileged NGINX image, a unique service name, resource limits, and the port mapping `127.0.0.1:8080:8080`.
3. Open **Customize** if port 8080 is already in use, or to change limits and storage. Leave enough CPU and memory in the pool for the operating system and other workloads.
4. Choose **Review & deploy**, check the destination, image, limits, and ports, then choose **Deploy service**.

Review only checks the plan. The final action can download the image, creates the service, starts the container, and saves its declaration to the manifest. No registry account is required for the starter image. Run images and source repositories you trust.

In another terminal on the engine machine:

```sh
curl --fail http://127.0.0.1:8080/
init status
```

Expect the NGINX welcome page and a running service. Open its **Logs** view for startup output. If deployment fails, read the reported error and check `init doctor`; a port conflict requires choosing a free host port. With a VM engine, the port belongs to the VM; use that engine's port forwarding to reach it from the host.

## 4. Keep the controller running

Closing the foreground controller leaves existing containers running, but pauses future scheduled job starts. Use the [macOS LaunchAgent](operations.md#macos-launchagent) or [Linux systemd unit](operations.md#linux-systemd-controller) for continuous scheduling.

Next, [deploy your own GitHub application](github-deployments.md), [connect another machine](remote-machines.md), or open [Host monitor](monitoring.md) to inspect machine usage. Use [Configuration](configuration.md) when you prefer YAML and CLI automation.
