# Connect Linux machines and Macs

Choose **Machines** in Init to save SSH connections, test them, and open a remote machine's services. The **Machine** selector in the sidebar switches the entire console: services, logs, usage, workspaces, budgets, deployment, and GitHub setup. Each machine has its own Init controller and resource pool. This does not distribute jobs across machines.

## 1. Prepare the destination

Install the same Init version as your local controller. Use the CLI archive for the destination's OS and architecture from [Releases](https://github.com/djsydney04/init/releases), or build from source as described in the [README](../README.md).

Run these commands **on the destination**, as the account that will own the services:

```sh
init init --discover
init serve
```

Review the discovered manifest before enabling managed workloads. Keep `serve` running: it owns the scheduler as well as the dashboard. It listens only on `127.0.0.1:8787`. Use `serve --port 8788` if another controller already uses that port, and enter that port in the connection form. Do not expose Init through a public proxy or Tailscale Funnel.

For a persistent controller, follow the [macOS LaunchAgent](operations.md#macos-launchagent) or complete [Linux systemd example](operations.md#linux-systemd-controller), including its account, workspace, environment, and startup checks. SSH does not install or start the controller. Container-engine permissions and systemd permissions still belong to the remote controller account. macOS launchd entries remain observers; connecting by SSH does not add lifecycle or resource controls to them.

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

[Tailscale](https://tailscale.com/download) connects machines on different networks. Install it **natively on the computer running your local Init controller and on every destination**, then sign in to the same tailnet. Init uses each machine's normal SSH server. The dashboard stays on loopback; no router port-forwarding is required.

### Install and sign in on Linux

For a distribution supported by [Tailscale's Linux installer](https://tailscale.com/docs/install/linux), run in that machine's terminal:

```sh
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
tailscale status
tailscale ip -4
```

The installer installs Tailscale system packages. `up` prints a sign-in URL; open it and authenticate to your intended tailnet. The final commands should show the connected device and its `100.x.y.z` address. Distribution-specific package commands are available in [Tailscale's stable package instructions](https://pkgs.tailscale.com/stable/) if you prefer those to the installer.

### Install and sign in on macOS

Install the standalone app recommended by [Tailscale's macOS guide](https://tailscale.com/docs/install/mac). Open Tailscale, complete its VPN configuration onboarding, and sign in to the same tailnet. Keep the app connected. The standard GUI app works with Init's ordinary SSH connection; a separate `tailscaled` installation is unnecessary.

If `tailscale` is available in your Terminal, use `tailscale status` and `tailscale ip -4`. With the normal `/Applications/Tailscale.app` installation, Init also checks its bundled CLI. You can inspect the same installation explicitly:

```sh
/Applications/Tailscale.app/Contents/MacOS/Tailscale status
/Applications/Tailscale.app/Contents/MacOS/Tailscale ip -4
```

### Verify the network and SSH separately

Use the destination's Tailscale IP from `ip -4`, or its MagicDNS name when enabled. In this example, replace `100.101.102.103` and `deploy` with the destination's real address and OS account:

```sh
# On the computer running your local Init controller:
tailscale ping 100.101.102.103
ssh deploy@100.101.102.103
ssh -o BatchMode=yes deploy@100.101.102.103 true
```

On macOS, use the app's bundled CLI path for `ping` if needed. A Tailscale ping confirms a Tailscale route; the SSH commands additionally check the SSH server, host key, OS account, and key/agent authentication. The batch-mode command must exit successfully without prompting before Init can use the connection. Install an SSH public key on the destination through its normal authorized-key process if needed; keep its private key on the local computer.

Your tailnet's [network access policy](https://tailscale.com/docs/reference/ssh-over-tailscale) and destination firewall must permit the source to reach the destination's normal SSH port, usually TCP 22. This is **ordinary SSH over Tailscale**; enabling **Tailscale SSH** and its separate SSH policy is optional and is not required for this setup. The [Tailscale CLI reference](https://tailscale.com/docs/reference/tailscale-cli) describes `status`, `ip`, and `ping` diagnostics.

### Discover the connected machine in Init

In **Machines**, choose **Find Tailscale machines**. Init reads `tailscale status --json` from the local installation and displays visible peers, OS, and online status. **Use address** fills in a Tailscale DNS name or IP; add the destination's SSH username if it differs from your local one. A peer appearing online does not prove SSH or Init is ready: test the saved connection.

Discovery is optional. You can enter a Tailscale IP/name, LAN address, or SSH alias directly. Init does not sign in to Tailscale, install it, or modify its access rules. On macOS it also looks for the Tailscale app's bundled CLI and standard Homebrew locations; on Linux the CLI must be on the controller's PATH.

## 3. Save, test, and open

1. Enter a friendly name, a unique connection ID, and `user@host` or an SSH alias.
2. Leave **SSH port** blank to use your SSH config (normally 22). **Init port** is the loopback port on the destination, normally 8787.
3. Choose **Save connection**, then **Test connection**. Check the reported Init version and workspace.
4. Choose **Open services**. The banner and machine selector identify the destination for all actions.

Connections are saved in `machines.json` inside the **local** Init workspace with owner-only permissions. Removing a saved connection does not change remote services. Changing its destination requires another test. Init checks the remote application, version, workspace, and manifest path on each connection; if they change, actions stop until you test and reopen it. An already open browser tab cannot silently use a newly edited connection.

Switching machines reloads the console and keeps workspace selections and deployment drafts separate. SSH failures remain errors for the selected machine; Init does not fall back to local services. Machines and Tailscale discovery always refer to the local controller, even while viewing a remote machine.

**Add service** also has a **Deploy on** picker. Choose this computer or a tested SSH connection; Init opens that machine’s deployment draft with its own workspaces and resource limits. Unverified connections must be tested in **Machines** first. Pick a starter or paste an image, then choose **Review & deploy**. Optional settings live under **Customize**. The review shows the destination before the final **Deploy service** action. Switching machines saves the previous draft separately and requires a fresh preview.

Paths, environment files, runner setup, resource usage, and service ports refer to the selected machine and its configured engine. A container port bound to that machine's loopback is **not** forwarded to the computer viewing the dashboard. Use your own SSH port forwarding when you need to open a service's web interface.

## Troubleshooting

- **Host key verification failed:** connect in Terminal, verify the host identity, and resolve the known-hosts entry. Do not disable host-key checking.
- **Permission denied:** check the username and that the controller's SSH agent or configured identity works without a prompt.
- **Forwarding prohibited / connection refused:** allow SSH TCP forwarding to the loopback Init port; ensure `init serve` is running there.
- **Different version / identity changed:** install matching versions, confirm the workspace and manifest, then test and reopen the connection.
- **Tailscale missing or disconnected:** run the appropriate `status` command above, open/sign in to Tailscale, then check again. On systemd Linux, check `systemctl status tailscaled`; on macOS, check that its VPN is connected. If the device's key expired, reauthenticate using Tailscale's prompts. You can still use normal SSH over your LAN.
- **Tailscale ping works but SSH fails:** confirm normal Remote Login/OpenSSH is running and check network policy, host firewall, SSH port, account, and keys. A working tailnet route does not authenticate SSH.
- **Terminal works but Init cannot discover/connect:** check the controller's PATH and SSH agent environment. A background controller does not inherit interactive shell setup. Restart it after correcting its environment, then test again.
- **A write timed out:** inspect the destination before retrying. Init never automatically retries a remote mutation; a change already accepted by the remote daemon can finish after the browser or SSH connection closes.

Under the hood, Init sends a bounded HTTP exchange through `ssh -W 127.0.0.1:PORT`. It fetches the remote session token inside that same SSH connection for writes. Only the dashboard's service/resource endpoints can be forwarded. The existing local Host, Origin, and session-token protections remain in force. No additional listening port or remote shell helper is created.
