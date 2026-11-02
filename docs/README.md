<img src="../assets/init-logo.png" alt="Init logo" width="64" height="64">

# Init documentation

Init runs services on your own Linux machine or Mac. Start with one local controller, then add machines and automation as needed. Its primary executable is `init`; the previous `selfhost` command remains an alias.

| Your task | Guide |
| --- | --- |
| Install Init and deploy your first web service | [Getting started](getting-started.md) |
| Install desktop/CLI packages, upgrade, or build from source | [Desktop app and releases](releases.md) |
| Upgrade an existing installation to Init without moving its data | [Init naming and migration](migration.md) |
| Connect a Linux server or Mac, including Tailscale setup | [Remote machines](remote-machines.md) |
| Deploy a public GitHub repository or automate updates with Actions | [GitHub deployments](github-deployments.md) |
| Inspect CPU cores, memory, swap, and network interfaces | [Host monitoring](monitoring.md) |
| Deploy image declarations from scripts, agents, or the TUI | [CLI and TUI deployment](deployment.md) |
| Configure settings and persistent service utilization | [Settings and history](configuration.md#controller-settings-and-utilization-history) |
| Declare services, ports, storage, jobs, and resource budgets | [Configuration](configuration.md) |
| Keep the controller running, manage credentials, and recover services | [Operations](operations.md) |
| Automate Init through Python, JavaScript/TypeScript, or Rust | [SDKs](../sdk/README.md) |
| Understand the controller, runtime adapters, and scheduling model | [Architecture](architecture.md) |
| Use the logo, palette, and desktop icon assets | [Init identity](brand.md) |

Each controller owns one machine's workspace and resource pool. The console can switch machines over SSH. Keep the controller on loopback; use the Machines connection flow to reach it remotely.
