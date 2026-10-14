---
title: Troubleshooting deployments
description: Fix failed builds, failed health checks, failed pre-deploy commands, crashing services and pushes that don't deploy.
---

Start on the deployment's page. Why it failed is under the header, in red, with the service's
name first. That service's tab has its **Build** and **Deploy** logs. See
[Deployments](../deploy/deployments.md).

## Following a deploy in the CLI

`ployz deploy` and `ployz up` show progress on stderr. On a terminal, the live block
ends with the full Service and Server rows and a summary. In CI or redirected output,
Ployz prints changes as lines and says `still waiting:` after 30 seconds without a
change. To capture those lines through a pipe, use `ployz deploy 2>&1 | cat`.
`--json` keeps the result on stdout; progress stays on stderr. `--events FILE` also
records the existing Deployment events as NDJSON. Source builds appear against their
Service before Server placement is known. Once execution starts, the rows identify the
Servers where work runs.

A failed deploy shows the deepest cause, available failed-container log tails, and
`inspect:` and `retry:` commands. A Deployment result that did not fully succeed exits
with code 3. A warning that names an incomplete Server observation also makes an
otherwise successful invocation exit 3 when that Server was relevant to the selected
Services or their dependencies, or to observed work the deploy affected. An Entry's
Down observation is evidence from that Entry, not proof that somebody stopped the
Server. Missing telemetry and unknown membership values alone do not establish Down.

Ctrl-C stops following a Cloud Deployment and exits 130; an active Cloud Deployment
keeps running. The command returns its last known result when one is available, including
a completion observed during interruption. An active
Cloud request can take up to 15 seconds to return. For local execution, Ctrl-C requests
cancellation and waits for owned work and cleanup to settle. Server enrollment that
already committed remains committed; follow the recovery hint if Global catch-up or
ingress work was interrupted.

## My build failed

`Build failed. web: …`

The deployment stopped before it changed anything, so your old version keeps running. Open the
service's **Build** log and read the end of it: the failing step is there.
[Troubleshooting builds](builds.md) covers the common causes.

## My new version never becomes healthy

`container … failed health monitoring: timed out; last check: HTTP 503`, or
`running (health: unhealthy); last check exited 127: sh: pg_isready: not found`

A new replica didn't pass its **Healthcheck** in time. Ployz stopped it and kept the old version
serving. The message ends with what the last check said: a status code, `connection refused`,
or a command's exit code and output.

- **Check the command.** Exit code 127 means the shell didn't find it in the image. Run it in
  the image yourself, and use `127.0.0.1` to reach the service from inside its container.
- **Listen on the checked port.** The health check calls the port your domains send traffic to.
  With no domain, or domains on different ports, it calls `PORT`, `8080` unless you set it. Make
  your app listen there, or set the domain's port or `PORT` to the port it uses.
- **Listen on `0.0.0.0`**, not `127.0.0.1`. See
  [This app didn't respond](app-not-reachable.md#this-app-didnt-respond).
- **Answer with a 2xx status, not a redirect** to `https://` or `/login`. See
  [Deployments](../deploy/deployments.md).
- **Give it time to boot.** If you lowered **Healthcheck timeout** under **Settings → Deploy**,
  raise it. Otherwise make your app start faster.

## My running service fails its healthcheck

A replica passed its **Healthcheck** once and fails it now. It keeps getting traffic, so the
service stays **Online**, with an amber **⚠** on its card.

- Read the service's **Logs** for what changed: a dependency down, a full disk, a slow request.
- If the check is wrong, fix it under **Settings → Deploy** and click **Deploy**.

## My app exits as soon as it starts

**The Container exited**, **The Container is restarting** or **The Container stopped**, with
`container … failed health monitoring: exited with code 1` or ending in `restarting`

The new replica stopped right after it started, and the old version kept running. Any exit code
can show here.

- Read the **Deploy** log for the error your app printed before it stopped.
- Check the service's **Variables** for one your app needs that's missing or wrong.
- Fix the **Start command**, or clear it to run the image's own command.
- Code `137` usually means the replica went over its **Memory limit**. Raise or clear it under
  **Settings → Scale**.

## A service waits on another that never comes up

`dependency … failed health gate`

This service references another in its variables, and that one didn't come up healthy, so
Ployz didn't start this one. Fix the service the message names first; its **Logs** say why.

## My pre-deploy command failed

`hook container … failed: exited with code 1`, or `failed: timed out`

None of the service's replicas were replaced, so the old version keeps running.

- Read the **Deploy** log: the command's output is there.
- `timed out` means it ran longer than 5 minutes. Make it faster, or run the slow part outside
  the deploy.
- To try the command in a running replica, use the CLI (CLI-only for now):

  ```sh
  ployz exec api npm run migrate
  ```

## My service shows Crashed or Stopped

None of the service's replicas is running. **Crashed** means one exited with an error or keeps
restarting; **Stopped** means they all exited cleanly and weren't restarted. During a first
deploy a card can read **Not running** or **Crashed** for a few seconds: if the deployment ends
**Deployed**, there's nothing to fix.

- Read the last lines in the service's **Logs**. If they end with no error, it may have run out of
  memory: raise **Memory limit**.
- A start command that finishes, instead of running a server, restarts over and over or stops.
  Set **Start command** to the command that runs your app.
- If it stopped because of something outside your app, like a database that was down, restart
  it. That's CLI-only for now:

  ```sh
  ployz service restart api
  ```

## My deployment stays Queued

- Another deployment of the environment is running. Yours runs after it; cancel that one from
  **Deployments** if it's stuck.
- It's building on GitHub Actions, and the run hasn't started or finished. Its **Build** log shows
  where it is.
- The bottom bar shows **Add a server**: your organization has no server yet.
  [Add one](../servers/add-a-server.md).
- Nothing else is running: click **Deploy now** on the deployment's page.

## My push didn't deploy

- **Auto-deploy** is off under **Settings → Source**, or the push went to another Git branch.
- **Watch paths** are set and the push changed none of those files.
- **Wait for CI** is on and a check failed, or no checks ran. Re-run the check on GitHub, or push
  again.
- The repository is public and connected without the Ployz GitHub App. See
  [Deploy a public repository without the app](../deploy/github.md#deploy-a-public-repository-without-the-app).
- Your organization has no servers. [Add one](../servers/add-a-server.md).

## My new image didn't deploy

Pushing an image to your registry deploys nothing, and a server that already has a tag, like
`latest`, keeps its copy. Give each release its own tag and change **Container image** to it. See
[Ship a new version of your image](../deploy/docker-image.md#ship-a-new-version-of-your-image).

## Deploy says a service has nothing to run

`api has nothing to run yet: add an image or connect a repository`

The service has no source, and its card shows **No source**. Ployz won't deploy the environment
until it has one. Open its **Settings → Source** and click **Git repository** or **Container
image**, or delete the service.

## My uploaded code is gone

`api has no source to build: its last upload is gone and no image is left to reuse. Upload it again or add an image.`

Run `ployz up` again from your app's directory, or click **Add an image to api** on the
deployment's page.

## Deploy asks me to confirm it deletes data

**Deletes data** or **Data loss**

This deploy would delete a volume's data: a volume you deleted, a database service, which takes
its volume with it, or a mirror or detached copy you removed. **Data loss** means it also uses a
mirror while the primary isn't answering, so changes after the mirror's last refresh are lost.

- If you meant it, type your `project/environment` and click **Deploy**.
- If you didn't, cancel, then open **Details** in the bottom bar and click **Discard** beside the
  volume or service.

## My services show Can't reach servers

**Can't reach servers**

Ployz can't reach your servers, so it can't deploy to them or read their logs. See
[Troubleshooting servers](servers.md). Once a server answers again, click **Redeploy** on the
deployment that failed.
