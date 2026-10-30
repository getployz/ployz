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
row is Servers, not replicas. A deploy first downloads every image it runs onto each
Server, then creates the Volumes it needs, before any Server starts work. Until then each
row reads `waiting 0/3 (pulling <image>)`, or `(pulling <image> or preparing Volumes)` when
the deploy has Volumes to create, and a large image can hold it there for minutes. A Service
whose pull policy is Never has nothing to pull and reads plain `waiting`. A booting Service counts each Server whose new
Containers are ready, so it can read `booting 1/3` while the others boot. Once every
Server is ready it reads `switching N/N` until they switch together, then
`switched N/N`. Under Stop first or Rolling, a new Container that fails its health check, or
can't start after Ployz stopped the old one, makes the row read `rolling back` while Ployz undoes
that Server's change, such as `rolling back (1 failed)`. It counts the Servers
whose new Container already serves, as in `rolling back 1/2`, and shows no count when none does. A row whose new Containers already serve shows a `draining old` line
while the deploy removes Containers it no longer needs, which waits until every Server routes to the new ones. Old copies that a
replacement leaves behind drain on each Server after the switch, so they show in
`ployz ps`, not in the live block. A Service the deploy removes reads `waiting to remove`,
then `stopping` and `removing`, then `removed`, and the summary counts it as removed. A Service
the deploy keeps but only clears an old copy of reads `removing old`, then `switched`. When the
deploy leaves old replicas running, the row reads like `switched 3/3, 2 left running` and the
summary adds `2 copies left running`; see
[Old and new versions answer at once](#old-and-new-versions-answer-at-once). The block
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
`retry: ployz deploy`. Nor does a deploy Ployz refused for something in your own
configuration, such as two of your services claiming the same hostname, two services that
depend on each other, or a service you asked to remove that your configuration still
contains. Its message says what to change, and deploying again without a change fails the
same way. A hostname another service still publishes keeps the retry, even when this
deploy moves it: a deploy of that service can drop the hostname before you try again. A refused `ployz up` gets no `retry: ployz up` for the same
reason. An image that won't pull offers `undo: ployz history undo N` to go back
to the version before it, even when another Server's failure picked the `inspect:` line. A failed `ployz up` retries with `ployz up`, which uploads your fixed
directory; `ployz deploy` would ship the last upload that built. `up` leaves out the `Open`
lines when the deploy failed. When a newer deployment, such as a push, takes the place of the
one you were following without shipping all of its services, the CLI says
`Deployment #3 was superseded by #4 (push bbe4f9e).` and points at `ployz deployment show 4`.
When a newer deploy takes over the environment while one running on your machine is still going,
yours stops at its next step, and the CLI says `Deployment #3 was replaced by #4 before it
finished.` and points at `ployz deployment show 4`. What #3 changed before it stopped shows as
unknown.

A failed deploy exits with code 1. A Deployment that applied but whose result is missing
a Server, or that left an old container running because its Server didn't answer, exits
with code 3. So does a removal while a Server that may run one of the service's replicas is
down or can't be checked: it removes the replicas on the Servers that answer, reads
`rmme: removed 2/3; web-2 didn't answer, the next full deploy removes its copy once it is back`,
or `rmme: removed 0/0; couldn't check web-2, the next full deploy removes any copy there` when
it found none, and the service is removed either way. Your next deploy of the whole environment
removes the replica on web-2 once it is back. A warning that names an incomplete Server observation also
makes an otherwise successful invocation exit 3 when that Server was relevant to the selected
Services or their dependencies, or to observed work the deploy affected. An Entry's
Down observation is evidence from that Entry, not proof that somebody stopped the
Server. Missing telemetry and unknown membership values alone do not establish Down.
A Server whose membership the Entry doesn't recognize (`server ls` shows a state such as
`Left`) isn't asked either, so the CLI reports it as an incomplete observation, not as Down:
`this Entry doesn't recognize its membership, so it wasn't asked`. From 0.3.10 that counts as a
Server observation gap, so an otherwise successful deploy exits 3 when that Server is relevant,
as for a Down one. Before 0.3.10 it printed the Server's ID and exited 0.

When the CLI says `Server machine-2: deployment observations are incomplete`, it includes
distinct failure causes in that Server's line and does not repeat its container or volume
omissions. The Server's observation can be incomplete even if the project has no volumes.
`--json` keeps every warning, including the individual omissions and failures.
If the CLI has no name for an observation failure yet, plain output prints the warning at
the next progress update or heartbeat, using the Machine ID if it still lacks a name.
Interactive progress can show it at once. If following stops first while progress is showing,
the warning still prints.

A failure on a Server that isn't relevant to the deploy is not a gap, so exit codes ignore it.
A deploy planned by 0.3.11 or later names that Server: `volume observation failed on Server
machine-3: …`. It prints the Machine ID instead when another Server has the same name, and
when an earlier release planned the deploy.

If Ployz cannot read every required Server's Containers and Volumes while deploying a Service
with an ingress hostname, it warns that hostname conflicts were not fully checked. Resolve the
observation warnings, then deploy again.

If Cloud stops answering while the CLI follows a Deployment, the command exits 1 with
`Stopped following Deployment #N; it keeps running in Ployz Cloud.` and an `inspect:` line
for `ployz deployment show N`. Ctrl-C stops following a Cloud Deployment and exits 130; an active Cloud Deployment
keeps running. The command returns its last known result when one is available, including
a completion observed during interruption. An active
Cloud request can take up to 15 seconds to return. For local execution, Ctrl-C requests
cancellation and waits for the steps under way and their cleanup to settle. Server enrollment that
already committed remains committed; follow the recovery hint if Global catch-up or
web traffic setup was interrupted.

## My deploy failed partway

**Failed**, with some services on the new version and others on the old one

Where a new replica didn't come up, the old ones keep serving. Ployz doesn't roll back. A service
that already switched keeps the new version, services that depend on the failed one stay as they
were, and services that don't depend on it may have switched already.

> [!WARNING]
> Ployz doesn't undo a pre-deploy command. If a migration ran and the deploy then failed, your
> database keeps the change while the old version of your app runs against it.

- Fix the cause with the entries below, and deploy again.
- To try the fix in a [branch](../environments/environments.md) first, click
  **Fix it on a branch** on the deployment's page.

## My build failed

`Build failed. web: …`

The deployment stopped before it changed anything, so your old version keeps running. Open the
service's build card on the deployment's page and read the end of its log: the failing step is
there.
[Troubleshooting builds](builds.md) covers the common causes.

## My new version never becomes healthy

`container … failed health monitoring: did not become healthy within 30s; last check: HTTP 503`, or
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
  [Deploy without downtime](../deploy/zero-downtime.md#keep-your-app-serving-through-a-deploy).
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

## My new limit didn't apply

**web: Server edge did not apply the new limits. The Container keeps its previous limits.**,
with the server's reason as the cause, and `retry: ployz deploy …`

A deploy that only changes the CPU limit or raises the Memory limit applies it to the running
replica in place (see [Changing a limit](../services/settings.md#changing-a-limit)). Here the
server refused before changing anything, so the replica keeps its old limits. The
usual cause is a Container changed on the server outside Ployz, for example with `docker update`:
the server refuses a Memory limit below the one the Container runs with, since lowering it could
kill the Container. Ployz has no command that only replaces a replica. Deploy a change to another
setting of the service at the same time, such as a variable, and the deploy replaces the replica
with the new limits.

**web: Server edge found the Container not running, so the new limits do not apply.**

The replica had stopped before the server could change it. Run the `retry:` command. A replica
still stopped is replaced with one that has the new limits, and one its restart policy brought
back gets the new limits in place.

**web: Server edge could not confirm the new limits.**, with Docker's reason or the lost
connection as the cause

The server asked Docker for the change and got an error, read back other limits, found the
replica stopped right after the change, or didn't answer in time. Which limits the replica runs with now is not known. Run the `retry:` command: a
replica already on the new limits passes as up to date, and any other is updated again.

## My running service fails its healthcheck

A replica passed its **Healthcheck** once and fails it now. It keeps getting traffic, so the
service stays **Online**, with an amber **⚠** on its card.

- Read the service's **Logs** for what changed: a dependency down, a full disk, a slow request.
- If the check is wrong, fix it under **Settings → Deploy** and click **Deploy**.

## My app exits as soon as it starts

**The Container exited with code 1**, **The Container exited with code 1 and is restarting**,
**The Container is restarting** or **The Container stopped**

The new replica stopped right after it started, and the old version kept running. Any exit code
can show here. The cause is the last line your app printed, from the same logs the service's card
on the deployment's page shows; it's left out when the logs couldn't be read, or couldn't be checked for Sealed values.

- Read the service's card on the deployment's page for the rest of what your app printed before
  it stopped.
- Check the service's **Variables** for one your app needs that's missing or wrong.
- Fix the **Start command**, or clear it to run the image's own command.
- Code `137` usually means the replica went over its **Memory limit**. Raise or clear it under
  **Settings → Scale**.

## A service waits on another that never comes up

`dependency … failed health gate`

This service references another in its variables, and that one didn't come up healthy, so
Ployz didn't start this one. Fix the service the message names first; its **Logs** say why.

If the cause says a server `did not answer`, that server didn't answer while Ployz checked the
other service. From 0.3.12, a server whose Ployz is stopped is skipped instead, and the deploy
names it. See [When a server goes down](../services/scaling.md#when-a-server-goes-down).

## My setup or pre-deploy command failed

`The setup or pre-deploy command failed` with `exited with code 1`, or `timed out`

None of the service's replicas were replaced, so the old version keeps running. Before a service
starts, Ployz runs its pre-deploy command, then, in a new environment until the service first
deploys successfully, its setup commands. The first one that fails stops the deploy.

- Read the service's card on the deployment's page: the command's output is there.
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
While a deploy starts the service and none of its replicas exists yet, including while its
pre-deploy command runs, the card reads a grey **Starting**. Until the dashboard knows whether
the running deploy starts it, as just after you open the page, the line shimmers instead, and
reads a grey **Status unavailable** if the deployment can't be read. A replica that crashes during a
first deploy can read **Crashed** for a few seconds: if the deployment ends **Deployed**, there's
nothing to fix.

- Read the last lines in the service's **Logs**. If they end with no error, it may have run out of
  memory: raise **Memory limit**.
- A start command that finishes, instead of running a server, restarts over and over or stops.
  Set **Start command** to the command that runs your app.
- If it stopped because of something outside your app, like a database that was down, restart
  it: open the service, click **⋮**, then **Restart**.

A service with some replicas still serving can crash in the rest. `ployz status` names those
replicas. See
[Status says a service serves fewer copies than it should](#status-says-a-service-serves-fewer-copies-than-it-should).

## Status says a service serves fewer copies than it should

```text
api   Serving 0 of 2 copies. web-1 exited with code 1, web-2 restarting.
```

`ployz status` prints this when fewer of a service's replicas serve than its last deploy gave it.
It names each replica that doesn't serve, with the state `ployz ps` shows for it. A deploy that
ended **Deployed** doesn't watch its replicas after they start, so a replica that crashes later
shows up here.

- **Restarting, exited, unhealthy, or `running, starting`.** The replica crashes or fails its
  health check. `running, starting` clears on its own once the replica passes its check within its
  start period. If it stays, read why in its logs, then fix the app and deploy again:

  ```sh
  ployz logs api
  ```

  `restarted 6 times, up 3 s` means Docker restarted the replica after it crashed, less than 10
  seconds ago, and it is running again. Status doesn't count it as serving until it stays up for
  10 seconds, so a replica that crashes every few seconds shows here even while it is briefly
  running. A replica that crashes more than 10 seconds after each start reads as serving for most
  of each run, so check its logs if one keeps coming and going.

  `exited with code 0` means your app stopped on its own without an error. A start command that
  finishes, instead of running a server, does that.
- **Draining.** The replica is out of routing but still running, and too few replicas in routing
  are left. An older Ployz daemon could leave replicas like that after it restarted.
  `ployz deploy api` deploys what is staged for the service. With nothing staged, it starts the
  replicas the service is short:

  ```sh
  ployz deploy api
  ```

The line goes away once enough replicas serve. A replica removed outside Ployz, for example with
`docker rm`, is unseen, so status can't name it.

## Status says a service runs no copies

`api = No copies running. For a global Service, no Server matches its placement.`

`ployz status` prints this when a deployed service has no replica on any of your servers. It
checks only when every server answered and none is down. Removing the only server a service ran
on with `ployz server rm` leaves it like this, since removal doesn't move replicas. Deploy the
service to start its replicas on the servers you have:

```sh
ployz deploy api
```

If you removed the service from your config, status suggests plain `ployz deploy` instead, which
finishes removing it.

A global service runs only on servers its placement matches. When none matches, deploying it
places no replica and the line stays. Change its placement or a server's labels, then deploy.

## My service shows Not seen running

Part of your servers' report hasn't arrived yet, and none of the replicas in the part that did
is running. A replica may still be running where the report is missing, so the service doesn't
count as down. Wait a few seconds for the rest of the report.

## My service shows Server offline

Every replica of the service is on a server that's **Offline**, so the service is down. See
[A server shows Offline](servers.md#a-server-shows-offline) and
[When a server goes down](../services/scaling.md#when-a-server-goes-down). A service with replicas
on other servers keeps serving from them, and reads **Degraded** while fewer serve than it asks
for.

## Old and new versions answer at once

`If web-2 restarts and confirms the new copies within a few minutes of starting, it takes its replaced copies out of traffic. Otherwise they keep answering beside the new copies. Either way they keep running, and a left worker still takes jobs, until the next deploy.`
or `my-app-production/web left running on web-2 (web-2 did not answer). The next deploy cleans it up.`

A deploy couldn't reach web-2 to retire its old replica, because web-2 was cut off or powered
off. If web-2 restarts and confirms the new replicas within a few minutes of starting, it takes
its old replica out of traffic on its own, but the replica keeps running. That includes a replica a
deploy replaced while the server was offline, even with nothing changed. Otherwise the replica
answers beside the new ones, so visitors get either version. That happens when web-2 comes back
from a network cut without restarting, when it can't confirm the new replicas in time, when the
server running a newer replica doesn't answer, when the newer replicas differ from each other
because a limit change reached only some of them, or when the deploy only raised a memory or CPU
limit and the replica isn't one too many. Either way an old worker takes jobs until the next
deploy. A server takes out only its own replicas, so an extra replica on another server stays
until that server restarts. Then deploy again.

1. Wait until web-2 shows online on the **Servers** page.
2. Deploy again. With nothing staged, click **Redeploy** on the newest deployment, or run:

   ```sh
   ployz deploy web
   ```

The deploy stops the old replica, and the warning doesn't come back.

## Some requests get the wrong answer after a server lost power

`proxy on web-1 did not confirm`

After web-2 lost power and came back, some requests through your other servers get the wrong
answer: an old version of your app, a redirect, or a 404 from something else on web-2. Deploys can
wait several minutes and then fail with the message above. On an earlier Ployz release, web-2 could
lose its last changes when the power went, and your other servers kept an out-of-date view of its
replicas. Ployz now saves each change on the server before the others see it, so this no longer
starts. A view that went out of date before you upgraded stays until web-2 reports those replicas
again.

1. [Upgrade Ployz](../servers/manage-servers.md#upgrade-ployz-on-a-server) on every server.
2. [Restart](../services/services.md#restart-stop-or-open-a-shell) each service that runs on
   web-2: open the service, click **⋮**, then **Restart**. Or run:

   ```sh
   ployz service restart web
   ```

The restart makes web-2 report its replicas again, and every server sends requests to them. A
deploy or **Redeploy** with nothing changed doesn't help, because it leaves the replicas as they are.
If some requests still reach web-2's old replicas a minute later, restart the service again.

A server whose root disk uses ZFS, Btrfs or tmpfs can still lose its last changes in a power cut. If that
happens, restart the services on it, as above. `ployz server ls` names each such server, and
`ployz server show` adds a `store` line that says so.

## My deploy failed when a server lost power or its network

`cause: operation timed out` or `cause: stream closed because of a broken pipe`

The server your CLI connects through stopped answering, so the deploy failed and exited 1, within
about a minute and a half. Services it had already switched run the new version, and the others
keep their old replicas. Once the server is back, deploy again to finish the rest:

```sh
ployz deploy
```

When the connection drops in the middle of a step, the deploy can't tell whether the server did
it. It says so instead of blaming the step: `db: The connection to Server web-1 dropped during
pull image, so it may have happened anyway`, with `cause: connection lost`. Your image and your
change are fine, so the deploy doesn't point you at them. A step that created or started a
replica may have left the new replica running beside the old one. If only Ployz on that server
restarted, as after a crash, the deploy connects to it again for up to a minute: it removes the new
replica, or carries on and finishes. Otherwise the new replica takes no traffic, `ployz status`
names it as `1 copy no deploy brought into routing takes no traffic`, and the next deploy removes
it.

## A server was removed during my deploy

**Server web-2 was removed while the deploy ran**

The server was removed while the deploy started replicas on it, so the deploy failed. Your app
didn't crash: removing the server stopped its replicas. A server removed while the deploy was only
stopping its old replicas doesn't fail the deploy. The deploy removes any replica it had started
on that server, as it does after any failed start. Deploy again to run the new version on the
servers you kept:

```sh
ployz deploy
```

A server on Ployz 0.3.1 or earlier reports this as **The Container exited with code 0**, with
your app's shutdown lines as the cause. A client on 0.3.1 or earlier reads a newer server's
answer the same way.

## My upload or log stream fails on a slow connection

`keep-alive timed out`

Your connection moves less than about 17 KB/s (140 kbit/s), so the server's answers to the CLI's
checks wait behind your upload or download for more than a minute, and the CLI gives up as if the
server had stopped answering. It happens during `ployz up` and builds that upload your code, and
when a busy `ployz logs -f` waits on you, such as while you scroll it in `less`.

Run the command again from a faster connection, such as a CI runner.

## A server hasn't picked up the new version

`web = web-2 has not picked up the new copies. Check its network.`

`ployz status` prints this when web-2 is up but its proxy hasn't picked up `web`'s current copies,
so visitors who reach web-2 can get the old version. It doesn't flag a copy a deploy reported as
left running: that copy keeps serving beside the new ones until the next deploy, and the deploy's
exit 3 line is what says so. See
[Old and new versions answer at once](#old-and-new-versions-answer-at-once).

- **Check web-2's network.** Your servers need UDP port 51820 open between them. See
  [Troubleshooting servers](servers.md).
- **Wait for it to rejoin.** Once web-2 catches up, the line goes away. A server that's down
  shows as **Down** instead, and catches up once it's back.

After a server rejoins, until every proxy catches up, usually under a minute, status can name
servers that still see another one as Down. Run it again before you act.

A `read` or `deploy` token without server access doesn't show this line yet, because Ployz Cloud
reads for it.

## A server routes a replica at an old address

`web = web-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.`

`ployz status` prints this when web-3's proxy sends a replica's traffic to an address that the
server running the replica doesn't report for it. This happens when the replica's address changed,
or the replica was removed, and web-3 hasn't caught up, so visitors who reach web-3 can fail to
reach `web`. A server that doesn't answer the check adds no line. For a removed replica, status
names no server when any server that may have run the replica didn't answer or is Down.

Status skips this check while a deploy is in progress. Right after a deploy or a move, a proxy that
hasn't caught up can still route a replica that was just removed, so the line can show briefly and
go away by itself. Run status again before you act.

When status warns that `web-3 has not confirmed what its proxy serves`, one of these happened:

- Web-3's daemon has just started.
- Web-3 wrote a new proxy config without reaching the proxy.
- Web-3's proxy restarted, and web-3 hasn't loaded its config into the new proxy yet.
- Status couldn't read what web-3 routes within 3 seconds, which a slow link between servers can
  cause.

Until web-3's proxy loads a config web-3 can see, status doesn't check what web-3 routes.
The warning goes away once the proxy answers, usually within 2 seconds of a daemon restart or
upgrade. If it stays, web-3's proxy isn't answering its daemon, or web-3 can't load its config
into the proxy. See [Troubleshooting servers](servers.md).

- **Check web-3's network.** Your servers need UDP port 51820 open between them. See
  [Troubleshooting servers](servers.md).
- **Wait for it to catch up.** Once web-3's proxy routes the replica at its current address, the
  line goes away.

A `read` or `deploy` token without server access doesn't show this line yet.

## A newer deploy is already serving

`error: web: A newer deploy is already serving my-app-production/web.`

Another deploy brought a newer version of `web` online while yours ran. Yours fails and exits 1.
It removed the replicas it had started for `web`, and the newer version keeps serving. When the
service deploys **Stop first** ([Service settings](../services/settings.md)), yours also removes
the old replica it stopped, and says
`A newer deploy is already serving; the old Container will not serve again.` A deploy that loses
this way shows no log tail, because nothing failed to start. `ployz deployment show` lists the
service as `web: a newer deploy serves it`. If you want your version, deploy again.

## A stopped replica won't start

`Container … is out of rotation and cannot start again. Deploy the Service again.`

`ployz service start` brings back only replicas you stopped. A deploy retired this one, so it
can't serve again. Deploy the service instead:

```sh
ployz deploy web
```

## A started replica gets no requests

`Container … is left Pending and not routed: no Deploy brought it In. Deploy the Service again to route it.`

A deploy that failed left this replica behind before it ever served. `ployz service start` and
`ployz service restart` run it, but never send it requests, because two versions would answer at
once. The command exits 3. Deploy the service to put a replica in service:

```sh
ployz deploy web
```

## A restarted replica is left out of routing

`Container … is left Held and not routed: copy … of app/web is In at gen 3, newer than this copy's gen 2. The newer copy serves the Service; ployz deploy web replaces this one.`

A later deploy changed the service and started a newer replica, but this older one still runs the
old version. `ployz service restart` and `ployz service start` keep it out of routing, because the
newer version serves the service. The command exits 3. Deploy the service to replace the old
replica:

```sh
ployz deploy web
```

A replica a deploy kept as it was, such as the existing ones when you only raised the replica
count, goes back into routing. To tell the two apart, Ployz asks the server that runs the newer
replica for its full settings, variable values and config file contents included. So if a deploy
changed only those and couldn't replace the replica on a server, for example because that server
was unreachable, the old replica serves there once the server is back, until a restart or the next
deploy, and a restart keeps it out. If that server doesn't answer within a few seconds, Ployz
can't tell, and the restart routes the old replica as before. Deploy the service to replace it.

## Ployz restarted on a server during my deploy

`cause: connection to the daemon closed` or `cause: stream closed because of a broken pipe`

Ployz restarted on the server your deploy connects through, as it does during an upgrade or after
a crash. The deploy connects to it again for up to a minute. If the restart didn't cut off a step's
answer, the deploy carries on and finishes. If it did, as for a replica being created at that
moment, the deploy removes the new replicas, exits 1 within about a minute, and the deployment
reads Failed. Deploy again, or redeploy the failed deployment, and Ployz finishes the switch and
removes anything the failed deploy left behind:

```sh
ployz deploy
```

Do it right away when the service deploys **Stop first**, the default for a single replica with a
volume ([Service settings](../services/settings.md)): the failed deploy may already have stopped the
old replica, so the service is down until the next deploy. Otherwise a replica that was serving
before the deploy keeps serving, sometimes beside a new one. A service's first deploy has no old
replica: it serves only if its new replica was already taking traffic before the failure; otherwise deploy again.

When Ployz restarts on another server the deploy uses, the deploy waits for it and carries on.

A server on an earlier Ployz release keeps the connection open when it restarts, so the deploy
hangs on `still waiting:` instead of failing. That includes the restart that upgrades a server to
this release. Stop the command and deploy again.

## My deployment stays Queued

- Another deployment of the environment is running. Yours runs after it; cancel that one from
  **Deployments** if it's stuck.
- It's building on GitHub Actions, and the run hasn't started or finished. The service's build
  card on the deployment's page shows where it is.
- The bottom bar shows **Add servers**: your organization has no server yet.
  [Add one](../servers/add-a-server.md). **Deploy now** says
  `This Organization has no servers yet, so nothing deployed. Add one, then deploy again.`
- Nothing else is running: click **Deploy now** on the deployment's page.

## My push didn't deploy

- **Auto-deploy** is off under **Settings → Source**, or the push went to another Git branch.
- **Watch paths** are set and the push changed none of those files.
- **Wait for CI** is on: the deployment reads **Waiting for CI** until the checks pass, or
  **CI failed** when one failed. If no checks ran, it waits for your next push. Re-run the check
  on GitHub, or push again. See [Wait for CI](../deploy/github.md#wait-for-ci).
- The repository is public and connected without the Ployz GitHub App. See
  [Deploy a public repository without the app](../deploy/github.md#deploy-a-public-repository-without-the-app).
- Your organization has no servers. [Add one](../servers/add-a-server.md). A deploy you start
  says `This Organization has no servers yet, so nothing deployed. Add one, then deploy again.`

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

A server that can't look up a hostname while pulling says so instead:

**Failed at pull**, with `Host registry.example.invalid does not resolve, so the image could not be pulled. Check the image's registry, or this Server's DNS`

The host is usually the registry in the image name, so check that part for typos. It can also be
the registry's sign-in service or a proxy the server pulls through. If the name is right, the
server's DNS can't resolve it.

## My server's disk is full

**Failed at pull**, with `Server machine-3's disk is full: 48 MB free under /var/lib/containerd`

The named server ran out of disk space while it downloaded an image, from its registry or from
another server that already had it. A deploy downloads every image it
needs before it changes any service, so none of the deploy's services changed and the versions
already running keep running. The cause line under it is Docker's own error.

The directory is the one on the full disk. Docker keeps images under `/var/lib/containerd`, and
everything else under `/var/lib/docker`. These can be on different disks.

- Free space on that server: remove files you don't need, or remove old images with
  `docker image prune`.
- Give the server a bigger disk if it fills up again. If you mount a data disk for Docker, mount
  one at `/var/lib/containerd` too. A data disk at `/var/lib/docker` alone doesn't hold images.
- Run the deploy again.

You can catch a filling disk before a deploy fails on it. `ployz status` adds the line
`machine-3 = Its disk is nearly full: 48 MB free for Docker. Pulling an image onto it can fail until
you free space.`, and `ployz server ls` warns `Server machine-3's disk is nearly full: …`. Both flag a server with less than 1 GB free under Docker's root directory, usually
`/var/lib/docker`. They don't measure `/var/lib/containerd` when it is on a different disk. In
`ployz status --json` the row has reason `disk_full`, the server in `machine`, and the free space in
`free_bytes`. It doesn't change status's exit code. A server that is down, can't reach Docker, or
doesn't answer within 3 seconds isn't checked, and nothing says so. Signed in with a Read only or
Deploy token, status asks Cloud instead of your servers, and checks no disk.

## Deploy says a service has nothing to run

`api has nothing to run yet: add an image, connect a repository, or run ployz up in its directory`

The service has no source, and its card shows **No source**. Ployz won't deploy the environment
until it has one. Open its **Settings → Source** and click **GitHub repository** or **Container
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

## A waiting deploy says its upload is chosen when it starts

`uploaded by nick, sha256 3f2a9c81b0d4 for now; chosen again when it starts`

In Ployz Cloud: **Uploaded by nick for now; chosen again when it starts**

A deploy without a new upload builds each service from the upload it last ran. If you deploy
while another deployment is still running, yours waits, and which upload each service ran is
known only once that one ends. When yours starts, its page and `ployz deployment show` show the
upload it runs, which can differ from the one it showed while it waited. To ship the code in your
directory instead, run `ployz up`.

## A volume change didn't finish when my deploy failed

**Not reached** beside a volume change in **Details**, `Restore failed. Restore again.`, or
`the restore waits for the next Deploy.`

A failed deploy drops the volume change it failed on. Volume changes it never got to go back to
your staged changes, and **Details** marks them **Not reached**. Deploy again to run them. What
the dropped change leaves behind depends on how far it got:

- A move that failed before the handover leaves the volume on its old server, with all its data.
  Stage the move again when you're ready.
- A move that failed after the handover is not undone. The volume belongs to the new server, and
  your service stays down until the move finishes there. The move stays in your changes, so
  deploy again to finish it. See [A move failed partway](volumes.md#a-move-failed-partway).
- A restore that failed is taken back. Your services mount the old volume again, and the new
  volume goes away. If Ployz can't take it back when the deploy fails, your next deploy fails with
  `Restore failed. Restore again.` and takes it back then. Restore again once you fix the cause.
  Redeploying that deployment is refused.
- A deploy that shipped a restore it couldn't claim fails before it copies anything, with
  `the restore waits for the next Deploy.` The restore stays staged, so deploy again.

## Deploy asks me to confirm it deletes data

**Deletes data**, **Data loss** or **Next deploy deletes data**

This deploy would delete a volume's data: a volume you deleted, a database service, which takes
its volume with it, or a mirror or detached copy you removed. **Data loss** means it also uses a
mirror while the primary isn't answering, so changes after the mirror's last refresh are lost.

- If you meant it, type your `project/environment` and click **Deploy**.
- If you didn't, cancel, then open **Details** in the bottom bar, open the volume's or service's
  **⋮** menu and choose **Discard**.
- From **Save only**, confirming saves without deleting anything yet. The next deploy deletes the
  data, including one a push starts.

## A deploy says deleting a volume's data failed

`postgres-data: deleting its data failed: it is still in use on Server web-1`

You deleted a volume, and the deploy couldn't delete its data on a server, so the data is still
there. The end of the message says why:

- `Server web-1 did not answer` or `the Cluster could not be reached`. Wait until the server is
  back online on the **Servers** page.
- `it is still in use on Server web-1`. Something on that server still uses the volume. Stop what
  uses it.
- `Server web-1 refused`. See [Troubleshooting servers](servers.md).

Then deploy again to delete the data.

## My services show Can't reach servers

**Can't reach servers**

Ployz can't reach your servers, so it can't deploy to them or read their logs. See
[Troubleshooting servers](servers.md). Once a server answers again, click **Redeploy** on the
deployment that failed.

## My deploy says a server holds no Sealing Key

`this Machine holds no Sealing Key`, or `does not open with this Cluster's Sealing Key`

Your servers can't open a sealed value. Usually this happens after you lose every server and add
one again.

- The first message: the server shows **Needs recovery**. Give it your recovery code with
  `ployz recover`, then deploy again. See
  [When a server shows Needs recovery](../servers/recovery-code.md#when-a-server-shows-needs-recovery).
- The second message names a variable sealed with a key no server has any more. Delete the
  variable and add it again with its value, then deploy again.
