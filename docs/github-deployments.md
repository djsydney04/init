# Deploy from GitHub with Init

Init is the service console; the command remains `selfhost` to avoid conflicting
with the operating system's `init` command. GitHub deployment uses the same
workspace, engine, limits and volumes as the console.

Choose the path that fits your application:

| You want to… | Use |
| --- | --- |
| Create a service from a public repository containing a Dockerfile | [Deploy a public repository](#deploy-a-public-repository) |
| Automatically update an existing daemon on trusted branch pushes, or build a private repository | [GitHub Actions on your own runner](#github-actions-on-your-own-runner) |
| Run an image you already published | **Deploy service** → **Your own app** |

## Deploy a public repository

The selected controller needs `git` and `tar` on its PATH, access to GitHub, and a working Docker-compatible engine that can reach the registries referenced by your Dockerfile. The repository must be public and have a Dockerfile at the root of its selected build directory. Init does not require a GitHub account connection, token, manually prepared checkout, or self-hosted runner for this path.

1. Choose **Deploy service**, then select the destination in **Deploy on**. A remote machine must already have a tested [SSH connection](remote-machines.md).
2. Choose **From GitHub** and paste the repository URL, such as `https://github.com/your-org/your-app`. Use a repository URL rather than a file or browser `tree` link.
3. Leave **Branch or tag** blank for the repository's default branch, or name a branch or tag. Leave **Build directory** as `.` for a root Dockerfile; use a relative directory such as `apps/web` for a monorepo.
4. Check the generated service name. Under **Customize**, set the ports your app needs, resource limits, environment-file path, project, and persistent storage.
5. Choose **Review & deploy**, check the repository, ref, build directory, and destination, then choose **Deploy service**.

The review validates configuration without downloading source or executing the Dockerfile. Final deployment clones the repository into a temporary private directory inside the selected controller's workspace, packages the chosen build directory, and streams it to the configured engine over standard input using [Docker's tar build context](https://docs.docker.com/build/concepts/context/#local-tarballs). It then starts the service from the generated local image and saves its declaration. Init removes the temporary checkout and archive after the build or an ordinary failure. A successful review cannot verify that the repository/ref exists or that its build will succeed. Build failures are reported by the deployment result.

The streamed context works with a Docker CLI inside a Lima VM without a shared checkout mount. The selected Docker client still needs access to any runtime environment files and bind mounts you configure. The separate `deploy-image --build-context` command and Actions runner below use ordinary local build paths; those paths must be available to their configured Docker client.

**Direct GitHub builds do not apply `.dockerignore` exclusions.** The tar-input path sends the selected directory, and files matching `.dockerignore` patterns can also be included in the image by `COPY .`. Init excludes the cloned repository's root `.git` metadata, but does not implement Docker's ignore-pattern filtering. Keep runtime credentials in separate private environment files. If your Dockerfile relies on `.dockerignore`, use a local checkout with `deploy-image --build-context` or the Actions workflow below; both use Docker's normal directory-context build path.

Only deploy source you trust: Dockerfile build steps execute during deployment. For public clones, Init disables configured Git credential helpers, hooks, and global configuration; it never prompts for or saves credentials and does not initialize submodules. This form does not handle private repository credentials, custom Dockerfile paths, build arguments, or build secrets. Build those applications in your own CI or with the runner path below. Application credentials should use a private runtime environment file on the selected Docker client machine, kept out of Git and the manifest.

This creates a service once; branch pushes do not automatically update it. Configure the Actions workflow below for recurring deployments. Source building also consumes machine capacity beyond the new service's runtime resource limits. For persistent applications, [back up their data](operations.md#troubleshooting-and-recovery) before updates.

Verify the service in **Services**, inspect **Logs**, and test its published port on the engine machine. A remote machine's loopback port is not forwarded to your dashboard computer. Builds can take several minutes; Init allows up to five minutes each for cloning and packaging, and thirty minutes for the image build. If the build fails, check the public URL, branch/tag, Dockerfile location, engine connectivity, and error output; then correct the draft and review again. If the browser or SSH connection times out, inspect the destination's service list before retrying because an accepted deployment can continue.

### Deploy through the CLI

Run on the controller machine with the intended workspace selected. Replace the sample repository, ref, and container port with your application's values:

```sh
# Preview: validates settings without cloning, building, or starting a container.
selfhost deploy-github my-app https://github.com/your-org/your-app --reference main --context . --cpus 0.5 --memory-mb 256 --port 127.0.0.1:8080:8080

# Use the same settings with --apply to clone, build, save, and start the service.
selfhost deploy-github my-app https://github.com/your-org/your-app --reference main --context . --cpus 0.5 --memory-mb 256 --port 127.0.0.1:8080:8080 --apply
selfhost logs my-app --tail 100
```

Omit `--reference` for the default branch. Repeat `--port` for additional loopback mappings; `--project PROJECT_ID` selects an existing project workspace. This command creates a **new** daemon; use `deploy-image` or the workflow below to update an existing one. See `selfhost deploy-github --help` for the installed version's options.

## GitHub Actions on your own runner

### One-time setup

1. Deploy a **daemon** through **Deploy service** in Init. Set its ports, storage
   and limits, then review and deploy it. Jobs and monitor-only services do not
   support this replacement workflow.
2. Install a [GitHub self-hosted runner](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)
   on the machine running Init, under the same user account. Give it a unique
   label such as `init-production`. The runner needs `selfhost` on its PATH and
   access to the configured Docker engine, manifest, environment files and build
   checkout. With a VM engine, ensure the checkout path is shared with that VM.
3. In Init, open **GitHub setup**, select the service and trusted deployment
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

### What the workflow does

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

The commands described here require a build containing these features; an older
release may not include them. Build the matching source with
`cargo build --release --locked` and install `target/release/selfhost` for the runner. Check
`selfhost deploy-image --help` before enabling the workflow.

### Failure and recovery

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
