# Deploy from GitHub with Init

Init is the service console; the command remains `selfhost` to avoid conflicting
with the operating system's `init` command. GitHub deployment uses the same
workspace, engine, limits and volumes as the console.

## One-time setup

1. Deploy a **daemon** through **Deploy service** in Init. Set its ports, storage
   and limits, then review and deploy it. Jobs and monitor-only services do not
   support this replacement workflow.
2. Install a [GitHub self-hosted runner](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)
   on the machine running Init, under the same user account. Give it a unique
   label such as `init-production`. The runner needs `selfhost` on its PATH and
   access to the configured Docker engine, manifest, environment files and build
   checkout. With a VM engine, ensure the checkout path is shared with that VM.
3. In Init, open **Deploy from GitHub**, select the service and trusted deployment
   branch, and download the workflow. Commit it at
   `.github/workflows/deploy-init.yml` in the application's repository. The
   repository must have a Dockerfile in its root.
4. Create the GitHub environment named in the workflow (default `production`).
   Restrict it to the deployment branch; add required reviewers if your GitHub
   plan supports them. Give this runner access only to repositories you trust.
5. Run the workflow manually on that branch for the first deployment. Subsequent
   pushes to that branch build and deploy automatically. The workflow has no
   pull-request trigger. Do not add one: code on this runner can access its host.

The UI generates a file for you to review and commit. It does not connect your
GitHub account, register runners, create environments or publish a workflow.
No registry account, cloud service or public controller endpoint is required.
Do not commit environment files or credentials. The saved workflow includes your
local workspace and configuration paths; use repository variables for those
paths if you prefer not to include them in source control.

## What the workflow does

The workflow checks out the selected branch, builds `init-SERVICE:GITHUB_SHA`
with the **configured engine**, and replaces that service. GitHub concurrency
allows one deployment at a time and does not cancel an in-progress replacement.
A unique runner label ties it to the correct machine. The `--workspace` and
`--config` arguments tie it to the same controller configuration.

The command is also useful without GitHub:

```sh
# Read-only preview: no build, download or container change.
selfhost deploy-image web init-web:next --build-context .

# Build first, then replace the service.
selfhost deploy-image web init-web:next --build-context . --apply

# Or pull an already published image before replacing it.
selfhost deploy-image web ghcr.io/example/web:COMMIT --pull --apply
```

The `deploy-image` command is new in this change and is not in the v0.3.0 binary.
Until a release contains it, build this branch with `cargo build --release --locked`
and install `target/release/selfhost` for the runner. Check
`selfhost deploy-image --help` before enabling the workflow.

## Failure and recovery

Builds and pulls complete before stopping the existing service. Replacement has
brief downtime. The original container is stopped and renamed to
`<container-name>-previous`, preserving its filesystem and identity until the
replacement starts and the manifest saves. A startup or save failure restores
that original container and its previous running/stopped state. Limits, ports,
mounts and workspace membership are preserved. Success checks that the container
remains running without a restart after one second; it does not certify application health.

Volumes are shared with the replacement. Application writes, schema migrations,
and external side effects cannot be rolled back automatically. Use backups and
compatible migrations for persistent data. After a crash or incomplete rollback,
inspect the named backup and container IDs reported by the command before retrying;
Init refuses to overwrite an existing backup. A successful deployment whose old
backup could not be removed reports a cleanup warning. No command removes volumes.

Image builds may leave image layers. Image cleanup and application health gates
are intentionally left to the repository's existing operational policy.
