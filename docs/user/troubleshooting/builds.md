---
title: Troubleshooting builds
description: Fix builds that fail, wait, run out of memory or disk, or can't run on GitHub Actions.
---

Start on the deployment's page. Why the build failed sits under the header, in red:

```text
Build failed. web: Build failed: the build failed: exited with exit status: 1. No Service, hook, or volume change was attempted.
```

The cause is in the [build log](../builds/overview.md#read-the-build-log): open the service's
tab, then **Build**, and find the line that starts with `ERROR`. A build on GitHub Actions
fails with `Build failed. web: a build step failed`; the run on GitHub, linked at the top of
the log, has the full output. Your current version keeps running.

Fix the cause and deploy again. **Retry** rebuilds the same commit and settings, so it only
helps when the fix was on a server.

## Railpack can't tell how to build my app

You'll see `Railpack preparation failed: exited with exit status: 1`. The lines above it say
why.

- In a monorepo, set the app's folder, like `/apps/web`, as its
  [root directory](../deploy/github.md#deploy-a-monorepo).
- Commit the files Railpack looks for, like `package.json` and its lockfile. See
  [Railpack's docs](https://railpack.com).
- If your app needs something Railpack can't guess, add a `railpack.json`, or
  [build with a Dockerfile](../builds/railpack-and-dockerfiles.md#build-with-a-dockerfile).

## The build passes, but my app never starts

The image starts the wrong command, or none, and the **Deploy** log shows the failure. Set
**Start command** under **Settings → Deploy**, like `node server.js`, and deploy. See
[Troubleshooting deployments](deployments.md) for other reasons.

## Ployz can't find my root directory or Dockerfile

You'll see `Could not acquire the pinned repository source.`,
`Dockerfile must be a file inside the repository.` or
`Build root must be a directory inside the repository.`

Check **Root directory** under **Settings → Source** and **Dockerfile path** under
**Settings → Build** against that commit. The Dockerfile path starts from the root directory:
with root `/apps/web`, a Dockerfile at `apps/web/Dockerfile` is just `Dockerfile`.

## Installing a private package fails

You'll see `401` or `403` while the build installs a private package or Git repository.
Builds have no SSH keys or logins of their own: give the build a token through a secret
variable. See [Install private packages](../builds/railpack-and-dockerfiles.md#install-private-packages).
A Dockerfile can't start `FROM` a private image either: use a public one.

## A build step fails with exit code 137

The build ran out of memory.

- Build on a bigger server, set as the service's
  [Preferred Builder](../builds/where-builds-run.md#prefer-a-builder-for-one-service), or on
  [GitHub Actions](../builds/where-builds-run.md#build-on-github-actions).
- Lower the server's **Builds at once**, so fewer builds share its memory.
- If the server caps build memory, raise `memory_bytes`. See
  [Build resources and cache](../builds/build-resources-and-cache.md#cap-cpu-memory-and-cache).
- For Node, add a variable like `NODE_OPTIONS=--max-old-space-size=2048`.

## My build shows "No output yet." for a long time

The build is waiting its turn.

- If the log shows `Building on GitHub Actions:`, follow its link: the run is waiting for a
  runner.
- If the server's page shows **Building N now**, raise its **Builds at once**, or let more
  servers run builds.
- Another deployment in the environment may still be rolling out.

To stop waiting, click **Cancel** on the deployment's page.

## No server can take my build

You'll see `No eligible Build Machine was selected; no build was started.`

- On **Servers**, check your servers are online.
- Turn on **Run builds here** on at least one server.
- If your servers mix x86 and ARM, see
  [When your servers mix x86 and ARM](../builds/railpack-and-dockerfiles.md#when-your-servers-mix-x86-and-arm).
- Or pick a [build order](../builds/where-builds-run.md#choose-the-build-order) that ends with
  GitHub.

## The build log says "GitHub skipped"

You'll see `GitHub skipped: …`, and the next builder in your build order takes the build. If
GitHub was last, the deployment fails with `…No other Builder in your Build Order can take it`.
Pick a [build order](../builds/where-builds-run.md#choose-the-build-order) that includes your
servers, or fix the reason after the colon:

- `GitHub builds need the Ployz GitHub App on acme/web`:
  [connect GitHub](../deploy/github.md#add-a-service-from-a-repository) and give the app the repository.
- `acme/web has no .github/workflows/ployz-build.yml on its default branch`: add it from
  **Organization → Builds**, and check it's enabled in the repository's **Actions** tab.
- `the Ployz GitHub App can't run Actions in acme/web`: approve **Actions: Read and write**
  for the Ployz GitHub App in GitHub.
- `GitHub builds one platform, and this Service runs on linux/amd64 and linux/arm64`: a
  service on both x86 and ARM servers builds on your servers.
- `no runner started the build in time`, `the run ended before it started the build`,
  `the run ran out of time` or `the run ended without pushing an image`: check the run on
  GitHub. If it's still queued, your GitHub account may be out of Actions minutes or at its
  limit of jobs.

## My build stops after 30 minutes

You'll see `the build exceeded its 1800s execution timeout and was terminated`. Make the build
faster, build on [GitHub Actions](../builds/where-builds-run.md#build-on-github-actions), which
allows 2 hours, or [give builds more time](../builds/build-resources-and-cache.md#give-builds-more-time)
on the server.

## My deploy fails on an ARM or x86 server

You'll see `Build for Service web contains linux/amd64; destination Machine … reports architecture aarch64`.
A Dockerfile image only runs on the kind of CPU it was built on, and nothing changed on your
servers. Switch the service to Railpack, or see
[When your servers mix x86 and ARM](../builds/railpack-and-dockerfiles.md#when-your-servers-mix-x86-and-arm).

`Railpack builds only linux/amd64 and linux/arm64` means a server has another kind of CPU,
like 32-bit ARM. Ployz can't build for it.

## The build server runs out of disk

You'll see `no space left on device`.

- Clear the build cache, CLI-only for now: run `ployz server build-cache-clear` on the server
  as root.
- Remove images you don't use with `docker image prune`.
- [Cap the build cache](../builds/build-resources-and-cache.md#cap-cpu-memory-and-cache), or
  build on a server with more disk, or on GitHub Actions.

## My repository is too big or uses submodules

You'll see one of these:

- `Source archive exceeds the source size limit.`: the commit is over 256 MB compressed or
  2 GB unpacked.
- `Source archive has too many or duplicate entries.`: it has more than 100,000 files and
  folders.
- `Git submodules are not supported for Cloud builds.` or
  `Git LFS content is not supported for Cloud builds.`

Move large files, like datasets and media, out of the repository and fetch them during the
build or when the app starts. Copy a submodule's code into the repository, or install it as a
[private package](../builds/railpack-and-dockerfiles.md#install-private-packages).

## My Git branch is gone

You'll see `The source branch no longer exists.` or `Reconnect web's branch before deploying.`
Pick a Git branch under **Settings → Source → Branch** and deploy.

## GitHub didn't answer

You'll see `GitHub didn't answer while reading the source. Deploy again.` or
`Repository is not connected to this Organization.` Deploy again. If it keeps failing, check
the Ployz GitHub App can still see the repository: **Create → GitHub repository → Set up
GitHub**.

## The build queue is full

You'll see `Build queue is full; execution was not attempted`. Deploy again once the server has
caught up, or let more servers run builds.

## Related

- [How builds work](../builds/overview.md)
- [Troubleshooting deployments](deployments.md): failed health checks and crashing services
