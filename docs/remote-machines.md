# Connect Linux machines and Macs

Choose **Machines** in Init to save SSH connections, test them, and open a remote machine's services. The machine selector switches the entire console: services, logs, usage, workspaces, budgets, deployment, and GitHub setup. Each machine has its own Init controller and resource pool. This does not distribute jobs across machines.

## 1. Prepare the destination

Install the same Init version as your local controller. Use the CLI archive for the destination's OS and architecture from [Releases](https://github.com/djsydney04/self-host-init/releases), or build from source as described in the [README](../README.md).

Run these commands **on the destination**, as the account that will own the services:

```sh
selfhost init --discover
selfhost serve
```

Review the discovered manifest before enabling managed workloads. Keep `serve` running: it owns the scheduler as well as the dashboard. It listens only on `127.0.0.1:8787`. Use `serve --port 8788` if another controller already uses that port, and enter that port in the connection form. Do not expose Init through a public proxy or Tailscale Funnel.

For a persistent macOS user controller, `selfhost install-launchd` installs a LaunchAgent. For Linux, run `selfhost serve` under your existing service supervisor; configure its workspace, user, environment, and restart policy explicitly. SSH does not install or start the controller. Container-engine permissions and systemd permissions still belong to the remote controller account. macOS launchd entries remain observers; connecting by SSH does not add lifecycle or resource controls to them.

Enable the destination's normal SSH server:

- **Mac:** System Settings → General → Sharing → Remote Login. Allow the account you intend to use.
- **Linux:** enable OpenSSH using your distribution's instructions. The account must be allowed to forward TCP connections to `127.0.0.1:8787` (or your chosen Init port).

From the computer running your local Init, connect once in Terminal:

```sh
ssh your-user@your-machine
```

Verify the server's fingerprint using a trusted source before accepting its host key. Configure an SSH key or agent so later connections do not need password or passphrase prompts. Init uses the local OpenSSH client, its SSH config, agent, and known-hosts file. It never saves private keys or passwords and never accepts a new host key automatically. SSH config aliases, custom ports, and jump hosts work through the normal OpenSSH configuration.

The desktop app needs the same usable SSH environment as your Terminal. If your agent is available only inside a shell, configure an identity in `~/.ssh/config` and make it available to the desktop session.

## 2. Optionally connect over Tailscale

[Tailscale](https://tailscale.com/download) is a good fit for machines on different networks. Install it on the local computer and each destination, sign in to the same tailnet, and allow SSH access between the intended machines in your tailnet's access rules. No public dashboard port or router port-forwarding is required.

This integration uses [ordinary SSH over Tailscale](https://tailscale.com/docs/reference/ssh-over-tailscale). You do **not** need to enable Tailscale SSH. Keeping the normal SSH server works with both Linux and standard macOS Tailscale installations. Tailscale's managed SSH server has [additional platform requirements](https://tailscale.com/docs/features/tailscale-ssh).

In **Machines**, choose **Find Tailscale machines**. Init reads `tailscale status --json` from the local installation and displays visible peers, OS, and online status. **Use address** fills in a Tailscale DNS name or IP; add the destination's SSH username if it differs from your local one. A peer appearing online does not prove SSH or Init is ready: test the saved connection.

Discovery is optional. You can enter a Tailscale IP/name, LAN address, or SSH alias directly. Init does not sign in to Tailscale, install it, or modify its access rules. On macOS it also looks for the Tailscale app's bundled CLI and standard Homebrew locations; on Linux the CLI must be on the controller's PATH.

## 3. Save, test, and open

1. Enter a friendly name, a unique connection ID, and `user@host` or an SSH alias.
2. Leave **SSH port** blank to use your SSH config (normally 22). **Init port** is the loopback port on the destination, normally 8787.
3. Choose **Save connection**, then **Test connection**. Check the reported Init version and workspace.
4. Choose **Open services**. The banner and machine selector identify the destination for all actions.

Connections are saved in `machines.json` inside the **local** Init workspace with owner-only permissions. Removing a saved connection does not change remote services. Changing its destination requires another test. Init checks the remote application, version, workspace, and manifest path on each connection; if they change, actions stop until you test and reopen it. An already open browser tab cannot silently use a newly edited connection.

Switching machines reloads the console and keeps workspace selections and deployment drafts separate. SSH failures remain errors for the selected machine; Init does not fall back to local services. Machines and Tailscale discovery always refer to the local controller, even while viewing a remote machine.

**Deploy service** also has a **Deploy on** picker. Choose this computer or a tested SSH connection; Init opens that machine’s deployment draft with its own workspaces and resource limits. Unverified connections must be tested in **Machines** first. Pick a starter or paste an image, then choose **Review & deploy**. Optional settings live under **Customize**. The review shows the destination before the final **Deploy service** action. Switching machines saves the previous draft separately and requires a fresh preview.

Paths, environment files, runner setup, resource usage, and service ports refer to the selected machine and its configured engine. A container port bound to that machine's loopback is **not** forwarded to the computer viewing the dashboard. Use your own SSH port forwarding when you need to open a service's web interface.

## Troubleshooting

- **Host key verification failed:** connect in Terminal, verify the host identity, and resolve the known-hosts entry. Do not disable host-key checking.
- **Permission denied:** check the username and that the controller's SSH agent or configured identity works without a prompt.
- **Forwarding prohibited / connection refused:** allow SSH TCP forwarding to the loopback Init port; ensure `selfhost serve` is running there.
- **Different version / identity changed:** install matching versions, confirm the workspace and manifest, then test and reopen the connection.
- **Tailscale missing or disconnected:** install/open Tailscale and sign in, then check again. You can still use normal SSH over your LAN.
- **A write timed out:** inspect the destination before retrying. Init never automatically retries a remote mutation; a change already accepted by the remote daemon can finish after the browser or SSH connection closes.

Under the hood, Init sends a bounded HTTP exchange through `ssh -W 127.0.0.1:PORT`. It fetches the remote session token inside that same SSH connection for writes. Only the dashboard's service/resource endpoints can be forwarded. The existing local Host, Origin, and session-token protections remain in force. No additional listening port or remote shell helper is created.
