---
title: Troubleshooting deployments
description: Fix failed builds, missing images, failed health checks, failed pre-deploy commands, crashing services and pushes that don't deploy.
---

Start on the deployment's page. Why it failed is under the header, in red, with the service's
name first. The page opens on the card that failed, with its servers and logs. See
[Deployments](../deploy/deployments.md).

## Following a deploy in the CLI

`ployz deploy` and `ployz up` show progress on stderr. On a terminal, the live block
shows one row per Service, grouped under the depth at which it starts. The count on a
row is Servers, not replicas. A booting Service counts each Server whose new
Containers are ready, so it can read `booting 1/3` while the others boot. Once every
Server is ready it reads `switching N/N` until they switch together, then
`switched N/N`. A row whose new Containers already serve shows a `draining old` line
while the deploy removes Containers it no longer needs, which waits until every Server routes to the new ones. Old copies that a
replacement leaves behind drain on each Server after the switch, so they show in
`ployz ps`, not in the live block. A Service the deploy removes reads `waiting to remove`,
then `stopping` and `removing`, then `removed`, and the summary counts it as removed. A Service
the deploy keeps but only clears an old copy of reads `removing old`, then `switched`. The block
ends with a summary that counts the Servers involved. In CI or redirected output,
Ployz prints changes as lines and says `still waiting:` after 30 seconds without a
change. To capture those lines through a pipe, use `ployz deploy 2>&1 | cat`.
`--json` keeps the result on stdout; progress stays on stderr. `--events FILE` also
records the existing Deployment events as NDJSON, each naming the Deployment's `id` and
`number`. Source builds appear against their
Service before the Deploy is planned, without a depth. A failure names the Server
where it happened.

A failed deploy shows the deepest cause, available failed-container log tails, and
`inspect:` and `retry:` commands. It adds at most one `inspect:` command per failed Service,
chosen from every Server where that Service failed, in this order:

1. `ployz logs --deployment N --build <service>` when its build failed.
2. `ployz logs <service>` when any of them failed while starting, health-checking, running a
   hook for, stopping or removing a Container, or failed at an unknown step and has a
   Container log.
3. Otherwise, `ployz get <service>.image` when any of them failed to pull the image. A Service
   built from source has no `image` setting, so it gets no `inspect:` line.
4. Otherwise no `inspect:` line. A failure before any Container exists, such as creating the
   Container or preparing its Volumes, leaves nothing new to inspect: `ployz logs` would show
   only the old Container.

So a Service that failed to pull on one Server and to start on another shows only
`ployz logs <service>`.

A failed build or an image that won't pull fails the same way again, so neither offers
`retry: ployz deploy`. An image that won't pull offers `undo: ployz history undo N` to go back
to the version before it, even when another Server's failure picked the `inspect:` line. A failed `ployz up` retries with `ployz up`, which uploads your fixed
directory; `ployz deploy` would ship the last upload that built. `up` leaves out the `Open`
lines when the deploy failed. When a newer deployment, such as a push, takes the place of the
one you were following without shipping all of its services, the CLI says
`Deployment #3 was superseded by #4 (push bbe4f9e).` and points at `ployz deployment show 4`.

A failed deploy exits with code 1. A Deployment that applied but whose result is missing
a Server, or that left an old container running because its Server didn't answer, exits
with code 3. A warning that names an incomplete Server observation also
makes an otherwise successful invocation exit 3 when that Server was relevant to the selected
Services or their dependencies, or to observed work the deploy affected. An Entry's
Down observation is evidence from that Entry, not proof that somebody stopped the
Server. Missing telemetry and unknown membership values alone do not establish Down.

If Cloud stops answering while the CLI follows a Deployment, the command exits 1 with
`Stopped following Deployment #N; it keeps running in Ployz Cloud.` and an `inspect:` line
for `ployz deployment show N`. Ctrl-C stops following a Cloud Deployment and exits 130; an active Cloud Deployment
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

## My new version runs out of memory

**The Container ran out of memory under its 16 MiB memLimit**, with the health check's last
result as the cause, and `next: ployz set 'web.memLimit=32MiB' …`

Docker killed a process in the new replica for going over the service's **memLimit**. Ployz
stopped the replica and kept the old version serving. The `next:` command doubles the limit,
up to the most **memLimit** allows, 1024 GB; set the size your app needs. A service already at
1024 GB gets no `next:` command: it has to use less memory. Nor does a service without a
**memLimit**.

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

## My setup or pre-deploy command failed

`The setup or pre-deploy command failed` with `exited with code 1`, or `timed out`

None of the service's replicas were replaced, so the old version keeps running. Before a service
starts, Ployz runs its pre-deploy command, then, the first time a new environment deploys it, its
setup commands. The first one that fails stops the deploy.

- Read the **Deploy** log: the command's output is there.
- `timed out` means it ran longer than 5 minutes. Make it faster, or run the slow part outside
  the deploy.
- To try the command in a running replica, use the CLI (CLI-only for now):

  ```sh
  ployz exec api npm run migrate
  ```

## My service shows Crashed or Stopped

None of the service's replicas is running. **Crashed** means one exited with an error or keeps
restarting; **Stopped** means they all exited cleanly, or you stopped them, and weren't restarted.
A replica you stopped reads **Stopped** whatever code your app exited with, and `ployz ps` shows
it as `stopped`. One that exited on its own before your stop reached it still reads **Crashed**.
During a first
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

## My service shows Server offline

Every replica of the service is on a server that's **Offline**, so the service is down. See
[A server shows Offline](servers.md#a-server-shows-offline) and
[When a server goes down](../services/scaling.md#when-a-server-goes-down). A service with replicas
on other servers keeps serving from them, and reads **Degraded** while fewer serve than it asks
for.

## My deployment stays Queued

- Another deployment of the environment is running. Yours runs after it; cancel that one from
  **Deployments** if it's stuck.
- It's building on GitHub Actions, and the run hasn't started or finished. Its **Build** log shows
  where it is.
- The bottom bar shows **Add servers**: your organization has no server yet.
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

## My image isn't found

**Failed at pull**, with `Image nginx:1.27-alpne not found, or its registry needs credentials`

Your servers couldn't download the image, so the deploy stopped before any of its services
changed. A version that was already running keeps running. A registry answers the same way for a tag that
doesn't exist and for a private image you have no access to.

- Check the name and tag under **Settings → Source**. Look for typos, and check that the tag is
  published on the registry.
- If the image is private, add **Registry credentials**, or replace a token that expired. See
  [Use a private image](../deploy/docker-image.md#use-a-private-image).

## My server's disk is full

**Failed at pull**, with `The Server's disk is full: 48 MB free under /var/lib/docker`

The server ran out of disk space while it downloaded an image. A deploy downloads every image it
needs before it changes any service, so none of the deploy's services changed and the versions
already running keep running. The cause line under it is Docker's own error.

- Free space on that server: remove files you don't need, or remove old images with
  `docker image prune`.
- Give the server a bigger disk if it fills up again.
- Run the deploy again.

## Deploy says a service has nothing to run

`api has nothing to run yet: add an image, connect a repository, or run ployz up in its directory`

The service has no source, and its card shows **No source**. Ployz won't deploy the environment
until it has one. Open its **Settings → Source** and click **Git repository** or **Container
image**, or delete the service. A service you uploaded with `ployz up` has no source in a new
branch: run `ployz up --env <branch>` from your app's directory.

## My uploaded code is gone

`api has no source to build: its last upload is gone and no image is left to reuse. Upload it again or add an image.`

Ployz keeps a service's latest upload, up to 95 MiB, until you upload again or delete the service
or its environment. You see this when that upload is gone and no image of it is left.

Run `ployz up` again from your app's directory, or click **Add an image to api** on the
deployment's page.

## Deploy says a service must build again from an earlier upload

`web must build again from an earlier upload than this Deployment's, and a Deployment builds from one upload. Deploy it on its own: ployz deploy web`

You uploaded `api` and `web` separately, and a deploy of both changed something `web`'s build
reads, such as one of its variables. Each service keeps its own upload, but one deploy builds from
only one of them. Services that don't need a new build reuse their images, so this only stops when
the service with the older upload must build again. Run the command it shows, and `web` builds
from its own upload.

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
