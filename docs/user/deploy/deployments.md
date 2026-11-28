---
title: Deployments
description: Stage changes, deploy them, follow each deployment, and confirm a deploy that deletes data.
---

Nothing you change in the dashboard reaches your servers until you deploy it. Your edits pile up
as staged changes, like a draft, and **Deploy** makes your servers match. Each deploy becomes a
deployment, with its own page and logs.

## Stage changes

Settings save as you change them. Config files save when you click **Save**. Your services keep
running as they were. A staged field
turns pink until you deploy it (hover it to see what's running now), and the bottom bar counts
your changes, like **3 changes**.

![The bottom bar with one change to deploy](../images/bottom-bar-changes.png)

Deleting a service, volume, variable or domain is staged too; discard it and it comes back.

A few settings apply the moment you change them instead, and Discard doesn't undo them:
**Auto-deploy**, **Wait for CI**, **Watch paths**, **Preferred Builder** and a new secret for a
private image.

## Deploy your changes

1. In the bottom bar, click **Details** to review each change. Add a
   **Message (optional)** if you like.
2. Click **Deploy changes**. **Deploy** in the bottom bar skips the review.

![Details: a message box, each staged change, and Discard all, Save only and Deploy changes](../images/review-changes.png)

Ployz opens the new deployment's page. A deploy covers the whole environment, not only what you
changed: every GitHub service moves to the newest commit on its Git branch, and if you've added
or lost a server, replicas spread over the servers you have now (if a service's volume was on a
lost server, see [When a server goes down](../services/scaling.md#when-a-server-goes-down)).
Everything else keeps running as it is. Deploying only some services, such as with
`ployz deploy web`, also deploys the services they reference, with their saved changes, and the
deployment's page lists them. Each runs the image you approve for it, as a service you name
does, so if `web` reads `api` and `api`'s tag has moved, `api` runs the new image. A secret that is **Not set** never stops a deploy: the
deploy skips it, and any variable that references it, and warns. A
[broken reference](../services/variables.md#reference-another-services-variable), to a service or
variable that isn't there, doesn't either: the deploy leaves that variable out the same way, or
leaves the reference empty in a config file, along with any reference that reads it, and the
deployment's page and `ployz deploy` name each one.

A deploy of the whole environment, from **Deploy** or plain `ployz deploy`, removes every service
running in the environment that the environment doesn't hold: the ones you removed, such as with
`ployz service rm`, and any other, such as one started by another tool. A deploy a Git push starts
does the same when it ships every service. `ployz deploy` and `ployz deployment show` name each
service it removes, such as `db: removed 1/1 (not in this environment)`, and `ployz deploy --plan`
warns that it will. Their volumes and data stay. A deploy that can't check a server that should
answer skips pruning, still deploys, and says `Prune skipped: not every Server answered`. A server
Ployz already knows is down doesn't stop the prune: the replicas on the other servers are removed,
as below.

Before Ployz Cloud queues a whole-environment deploy that prunes, it asks your servers what runs
there. A prune that removes a service is destructive, like `ployz service rm`:

- Where destructive actions ask first, and for every deploy an agent makes, what you review before
  you say yes lists each service the prune removes, such as **Removes service extra2**.
- A [Deploy token](../account/permissions.md) can't remove one. Its deploy is refused with
  `Needs changes:destroy: this deploy removes extra2, running in this environment but not part of it. Deploy with --no-prune to keep them.`
  When nothing else runs, it deploys as usual.
- When Cloud can't ask every server that should answer, a Deploy token's deploy is refused the same
  way. Any other deploy asks first where destructive actions do, and what it shows says it also
  removes any other service running in the environment. A server Ployz already knows is down isn't
  asked. When a server didn't answer because large packets to it are dropped, the refusal and the
  review say so, such as
  `Couldn't check every server: large packets to web-3 are dropped on the mesh path`. See
  [Large packets to a server are dropped](../troubleshooting/servers.md#large-packets-to-a-server-are-dropped).
- A redeploy of a deploy that pruned prunes again, and asks or is refused the same way, by what runs
  now. `ployz deployment redeploy` takes no `--no-prune`, so a Deploy token's refused redeploy says
  `Redeploy #16 once every server answers, or have an owner or admin redeploy it.`, or, when it
  names a service it removes, `Have an owner or admin redeploy #16.`

A service started after the deploy was queued, and before it ran, is removed without being listed.

Removing a service while a server that may run one of its replicas is down, or can't be checked,
still removes the replicas on the servers that answer. The service reads removed and names that
server, such as
`rmme: removed 2/3; web-2 didn't answer, the next full deploy removes its copy once it is back`,
and the deploy exits 3. When Ployz found no replica on the servers it could check, the service
reads `rmme: removed 0/0; couldn't check web-2, the next full deploy removes any copy there`.
Either way the service is removed from the environment. A replica on web-2 may serve again once
that server is back, until your next deploy of the whole environment removes it.

To keep those other services, pick **Deploy, keep other services** in the bottom bar's **⋮** menu,
or run `ployz deploy --no-prune` (`ployz up --no-prune` too). It still removes the services you
removed, and names each one it keeps, such as `postgres: running, not in this environment; kept.`

Deploying only some services never removes another: after `ployz service rm big`,
`ployz deploy big` fails with `Service big is staged for removal; a full Deploy removes it`, and
plain `ployz deploy` removes it.

**Save only** in **Details**, in the bottom bar's **⋮** menu, or on **Deploy** while you hold Alt,
records your changes and optional message without deploying them. Ployz shows **Saved, not
deployed**. Your
next deploy takes them along, including one started by a push to GitHub; unsaved changes stay
behind. Saving again with no changes creates no new version. The CLI calls this
`ployz deploy --skip-deploy`, and accepts `--message`.

To undo, open a change's **⋮** menu and choose **Discard**, or open **⋮** in the bottom bar and
choose **Discard all changes**.
**Discard all changes** returns everything to your latest saved version, including a staged
rollback. Discarding a service or volume you never deployed deletes it. Discard does not save a
version. When part of your latest saved version is not deployed, for example because its deploy failed,
the bottom bar shows **Save #4 not deployed** with **Deploy** instead of a change count. A deploy
saves your changes before it starts, so one that ran nothing, for example because it was refused
while a server didn't answer, still leaves them saved. The bar then says **Save #4 · its deploy ran
nothing**, and `ployz diff` says `Saved as version #4. Its deploy ran nothing, so none of these
changes reached a server.` After you discard such changes, `ployz diff` and `ployz discard` name the
save as one whose deploy ran nothing beside each change they could bring back. While a
deploy of it is queued or running, for example `ployz deploy web` deploying web and the services
it references, the bar shows that deploy's status instead, **Queued**, **Preparing** or **Deploying · Save #4**, with **Deploy
next** for the rest. A deploy still waiting for CI isn't queued yet, so the bar keeps **Save #4 not
deployed**, or **Save #4 · its deploy ran nothing** after one that ran nothing. Neither its **⋮** menu nor Details offers **Discard all** then, since nothing is left to
discard.
A new service's change starts with what it runs, its image or its repository and branch, such as
`web.image: null → "nginx:1"`. Those rows have no Discard of their own: discard the service.
`ployz discard web.image` says so and names the command, `ployz discard web`.
If you discard saved changes back to what is deployed, Details still shows the draft reversal.
Saving records that reversal; Discard on its row returns it to the latest saved value.
In the CLI, `ployz discard` with no path drops every staged change in the environment, including
changes someone else staged from the dashboard or an agent, and lists each one it dropped. Pass a
path, such as `ployz discard web.env.KEY`, to drop only that change. It also names each saved change
it took out and the command that brings it back, such as `ployz discard web`. `ployz diff` lists
those rows and `ployz status` counts them. Plain `ployz discard` returns each service to what is
deployed, or, for a service a queued or running deploy is changing, to the value that deploy is
bringing in. `ployz discard --target review` matches the dashboard's **Discard all changes** and
goes back to your latest saved version.

The dashboard's **Deploy** needs a staged change. To ship the same commit and settings again,
click **Redeploy** on the newest deployment; see
[History and rollback](history-and-rollback.md#redeploy-or-cancel-a-deployment). To move GitHub
services to their branch's latest commit with nothing staged, use the CLI:

```sh
ployz deploy
```

## Deploy from an approved device

A server runs a service that reaches your data only from a spec an
[approved device](../servers/manage-servers.md#what-needs-an-approved-device) or
[your passkey](#deploy-from-the-dashboard) signed, or under an
[automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push). A service reaches data
when it opens a sealed value, mounts a volume, pulls its image with a login, or asks for more of
its server than a plain container gets: a host port, a host path, added privileges or devices.
Run `ployz deploy` on that device. It works out what each service will run, prints what it signs for each service,
and asks before it signs with your device key:

```text
Approved web: nginx:1.29-alpine → @sha256:3f1c2a9b8e7d…
  variables DATABASE_HOST=db.internal, DATABASE_URL (sealed), URL="postgres://app:" + (sealed "DB_PASSWORD") + "@db.internal/app"
  command ["nginx","-g","daemon off;"]
  ports [{"container_port":80,"hostname":"web.example.com","http_protocol":"http","load_balancer_port":443,"mode":"ingress"}]
Approved worker: ghcr.io/acme/worker@sha256:9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b (adds NET_ADMIN, host port 9000)
  cap_add ["NET_ADMIN"]
  ...
Approve web, worker in namespace shop-production? [y/N]
```

The first line of each service names the image and anything the service asks of the server:
privileged mode, devices and GPUs, added capabilities, PID mode, sysctls, host ports, bind mounts,
Docker volumes it names itself or gives a driver or options beyond a plain or tmpfs volume, the
ingress network and its pre-deploy hook. An image you wrote by digest
shows its whole digest. The lines under it list every setting the signature covers that isn't Ployz's default, by its name in the spec:
variables, the start command and entrypoint, user, mounts, config files, ports, privileges, the
hook and the rest. A setting added to Ployz later shows there too, and so does a `false` or empty
value that overrides one the hook would inherit. A text value with anything but letters, digits
and `-_./:@+` shows in double quotes. A sealed value shows by the name it was sealed as, never
its value, and a value built from text and sealed parts shows its text, quoted. Since the
approval shows a sealed value's name and not its contents, Ployz Cloud could replace a sealed
value before a deploy you approve.
The question names the namespace the signature is for. A deploy for a
[branch](../environments/environments.md) names the branch's own namespace, so if it names
another environment's, answer no.

A volume Ployz manages shows as one line for each place a service mounts it, saying whether it's a
managed or a Docker volume and whose data it holds, with its subpath, labels and the rest of what
the mount signs:

```text
  mounts managed volume uploads at /app/uploads, read-write: a new, empty volume
  mounts managed volume previews at /srv/previews, read-write: data made as previews, 3 Nov 14:02 UTC, 16 days ago (Ployz Cloud calls it thumbs)
  mounts Docker volume shop-production_vol-db at /var/lib/postgresql/data, subpath pgdata, read-write: data from before this version of Ployz recorded where data began (Ployz Cloud calls it db)
```

The deploy that first mounts a volume makes it new and empty, and every later deploy names that
data: the name it had then and when it began, with its name now in brackets when it changed. A
volume from before this version is named as your servers name it, with Ployz Cloud's name in
brackets.
Renaming a service or a volume keeps the data. A
[branch](../environments/environments.md) makes each of its volumes new on its first deploy.

Before it signs, `ployz deploy` checks each mount against two things: the last version of the
service you approved, checked against your servers' keys, and what your servers run now. When
both agree and nothing else about the mount changed, the mounts fold into one line:

```text
  2 volumes keep the data you approved (signed 3 Nov 14:02 UTC)
```

Anything else prints in full and needs your yes, before the usual question:

```text
Needs your yes:
  files: mounts managed volume thumbs at /srv/files, read-write: different data than the version you approved: was previews, 3 Nov 14:02 UTC, now thumbs, 9 Oct 08:15 UTC
Use the data marked above? [y/N]
```

That covers data that differs from your approved version, data your approved version mounted but
your servers no longer run, a move to another driver, the first time a service mounts data that
already exists, and a new volume that takes a path where other data was. The line says which
service uses the data now, or that no server lists a container using it. A server that doesn't
answer is named, since it may hold other data. Try again when it answers, or remove it with
`ployz server rm`. Answer no and nothing is signed. With no terminal, the deploy stops before it
signs.

A volume from before this version passes only when the version you approved mounts it, or a
server lists a container that uses it. Otherwise the deploy stops and names the service and
volume. If a server didn't answer, try again when it does, or remove it with `ployz server rm`. If
the service's old container is gone, start it once from its last deploy, or restore the data into
a new volume.

`ployz deploy` reads your servers' keys and containers over a route whose server answers for
itself: `--connect`, a context, or the local daemon. Through Ployz Cloud alone it can't, so it
warns, every mount prints in full and asks, and a volume from before this version is refused.
Retry with `--connect ssh://<server>`.

The signature doesn't cover where a service runs, how many replicas it runs, its
[count for each region](../services/regions.md#run-a-service-in-more-than-one-region), the order a
deploy replaces them in, its pull policy, its CPU and memory limits or the largest size of a
managed volume. None of them show here, and Ployz Cloud could change them on a deploy you approve.
That can stop or slow a service but can't reach more of your data. A service that mounts a volume
other than a tmpfs, whether a bind mount, an external volume or a volume Ployz manages, or that
asks anything of the host (privileged mode, devices, added capabilities, PID mode, sysctls, host
ports, the ingress or tunnel network, a privileged hook), keeps the servers it may run on in the
signature: its placement and the names of its regions. Two servers can hold a volume of the same
name with different contents, and a host-level setting acts on whichever server runs it. Only the
replica count inside each region stays open. A volume the service declares but never mounts
doesn't count.
Only a `node.id` constraint pins a service to one server. A region name limits where the service
runs only as far as the servers' labels are honest, and Ployz Cloud can relabel a server, the same
way it can mislabel one today.
Ployz Cloud relays the plan, so read these lines before you answer: a setting you didn't make
shows here even if Cloud put it there. A control character in a value prints as its escape,
such as `\u{1b}`, so a value can't redraw the lines around it. Answer no and nothing is signed
or deployed, and the command exits with 130. Without a terminal, `ployz deploy` refuses before it
signs, unless it only ships new images: see
[Deploy a new image without a terminal](#deploy-a-new-image-without-a-terminal).

Each server checks every container it creates against what you signed: the image, command,
variables and sealed values, mounts, config files, ports, privileges, restart policy and hook. Ployz Cloud
carries the signature but can't change what it covers. A server logs each one, such as
`web admitted by signed version from [Device(3x4sgmns4e)] digest=cb8d3c6ef656`.

A server keeps no list of the versions it ran. Each new container it creates, on any server,
needs the signature with it, which Ployz Cloud sends as described below. A version stops
counting when you remove every device that signed it.

A server refuses a version signed more than 10 minutes ahead of its own clock. If a deploy says
so, check the clock on the device that signed it and on the server, for example with
`timedatectl`, then deploy again.

Ployz Cloud keeps every version you sign, and each deployment records the image each signed
service ran. **Roll back** or **Redeploy** runs the images that deployment ran and sends their
signatures, so every server runs them, even one that never ran that version, and even after
the tag moved. A service that deployment ran unsigned runs the image its settings named
then: a digest as written, or a tag as it points now. A deploy that changes only how many
copies run, where they run or their CPU and memory limits runs the image the service last
ran, with its signature, as long as your settings still name the same tag. Change anything else, such as a variable, and it runs the image your
settings name now. Ployz Cloud can replay a version you signed, but it can't sign one.

A deployment made before Ployz Cloud kept signed versions has no signature of its own. Which
images a rollback or redeploy of it runs depends on how it was deployed:

- **Signed on a device with an older `ployz`.** Ployz Cloud kept the image that deploy pinned
  each service to, so a rollback runs those images even after the tag moved. This holds for
  every deployment an older `ployz` signed, however recent.
- **Not signed by a device.** Nothing recorded the images it ran. Each service runs the image
  your settings named then: an image written by digest runs that digest, and a tag runs the
  image it points to now.

Either way, Ployz Cloud sends a signature for a service only when a device has since signed
that exact version: the same image and settings, in the same environment. A server refuses the
rest of its services that reach data. Run `ployz deploy` on an approved device to sign them.

A `ployz` from before signed versions sends a kind of signature Ployz Cloud no longer keeps.
Ployz Cloud refuses its deploy before anything is queued and names the update to run:

```text
This ployz signs deploys in an older form this Config Store no longer takes, so nothing was deployed. Update ployz, then deploy again.
next: curl -fsSL https://ployz.sh | sh -s -- beta
```

A server also keeps which devices approved each container it made, and checks they're still
approved before it creates the container and each time the dashboard starts it again. Once
you remove all of them, that container won't start from the dashboard. It keeps running if
it's running. Deploy it again to sign it with a device you kept.

Every image runs by digest, since a tag names whatever the registry holds under it now. For
a tag on a public registry, `ployz deploy` reads the digest from the registry itself and
signs it, so `web` above keeps `nginx:1.29-alpine` in your settings and runs that exact
image. The next deploy reads the tag again, so a tag pushed again runs its new image only
after you deploy. The digest is the tag's multi-arch index, which works across CPU
architectures. If you write a digest yourself, write the index digest, not one platform's.

A private image can't be read without credentials, so write its digest. `ployz deploy` stops
before anything is queued:

```text
ghcr.io wants credentials to read web's image, so ployz can't pin it. Write its digest.
next: ployz set 'web.image=ghcr.io/acme/web@sha256:…'
```

If the registry doesn't answer within 10 seconds, `ployz deploy` stops and names it.

A narrowed deploy, such as `ployz deploy web`, checks only the services it ships. A service
that builds from source builds under its
[automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push), which `ployz deploy` signs
with the deploy when it needs to. When no rule can cover it, `ployz deploy` stops before it signs:
`No automatic deploy rule covers web as it stands, so your servers won't build it. Turn on Deploy every push, then deploy again.`
It also stops when you turned automatic deploys off, and when it can't read the rule, saying so.
A Git push that builds from source with no rule, or under one you turned off, fails at its build,
and so does a dashboard **Deploy** with no rule. Your servers refuse it with
`This server runs only builds an approved device starts.`

If `ployz deploy` can't reach your servers before it signs, it warns and deploys
without signing, and the servers refuse it.

### Deploy a new image without a terminal

A script or CI job has no terminal to ask in, so it can't approve a new version. It can ship a new
image of one you approved. Name each image with `--image` and add `--yes`:

```sh
ployz deploy web --image web=ghcr.io/acme/web:1.5 --yes
```

`--yes` signs a new version only of each service `--image` names: the last version a person
approved of it, with only its image changed. It saves the image to the service's settings, prints
the same lines it would ask about, and signs without asking. `PLOYZ_AUTO_CONFIRM=1` works the same
as `--yes`, and `--json` needs `--yes` too. Anything else changed in a service you name, such as a
variable or a volume, stops the deploy and names what changed. Run that deploy at a terminal, then
CI can ship images on top of it again. `--forget-older` always needs a terminal.

Every other service the deploy ships keeps the versions it already runs under, and nothing new is
signed for it. So `--yes` ships nothing else that is staged: when any other change waits to deploy,
in any service, volume or config, the deploy stops before it signs and names what changed:

```text
error: api has a staged change at api.startCommand that no person approved. --yes and CI or agent keys ship only the images --image names. Deploy it from a terminal. Nothing signed or deployed.
next: ployz deploy api
```

Deploy those changes at a terminal first, or discard them.

A deploy that stops while it signs or uploads, such as one that says `Nothing signed or
deployed.`, sets back each image it saved over another image. It warns instead when it can't: for
a service that had no image, since the new image replaced its source, or when setting it back
fails. If someone else edits the environment just as the deploy stages its images, the deploy
stops with nothing staged and asks you to run it again. `ployz diff` then shows what is
staged. Once the deploy is sent to be queued, the image you named stays in the service's settings
even if the deploy is refused or fails, and the next deploy ships it. If
you set an image with `ployz set web.image=...`, deploy it with
`--image web=...` in the same command rather than `--yes` alone: a deploy that ships a saved image
`--image` doesn't name stops before it signs.

A service built from source, by an automatic deploy rule, or deployed before Ployz signed deploys
has no approved version to start from. Switch it to an image and deploy it once at a terminal
first.

### Deploy from CI with a key

A CI key does the same without an approved device. Make one from your approved device with
`ployz key add --ci`; see [Add a CI or agent key](../servers/manage-servers.md#add-a-ci-or-agent-key).
It prints `PLOYZ_KEY=...` once. Store that value and a `PLOYZ_TOKEN` as secrets in your CI, and
deploy with `--image`:

```yaml
# GitHub Actions
- run: ployz deploy web --image web=ghcr.io/acme/web:${{ github.sha }}
  env:
    PLOYZ_TOKEN: ${{ secrets.PLOYZ_TOKEN }}
    PLOYZ_KEY: ${{ secrets.PLOYZ_KEY }}
```

With `PLOYZ_KEY` set, `ployz deploy --image` signs with the key instead of a device, and needs no
`--yes`. A server runs a CI key's version only when it builds on one a person approved, and only
within the environments, services and volumes the key was given, so a leaked CI key can't start a
service no person approved. Every server needs the current version of Ployz for this, and
`ployz deploy` names any older one so you can upgrade it. Removing the device that approved a
version stops CI deploys built on it, and stops scaling and moving the services that run it, until
a person deploys from a terminal again.

### Deploy with no key

A deploy from a computer with no device key or `PLOYZ_KEY` doesn't queue unsigned. When your passkey can sign every service it leaves unsigned,
`ployz deploy` and `ployz up` print a link to an approval page, and so does a coding agent's
deploy. Open it, check what it destroys and the lines your passkey signs, and tap once. The
deploy then starts as whoever asked. See [Deploy without a key](../cli/overview.md#deploy-without-a-key)
for how the CLI waits.

When a service builds from GitHub and no [automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push)
approves it as it stands, the page shows the rule too, and the same tap signs it. The deploy then
builds that service, and later pushes to its branch deploy with no prompt. A deploy with only such
services to sign still gets a link.

A deploy from an upload, as `ployz up` sends one, either queues at once or waits for approval like
any other deploy. Either way, it then fails at its build with
`This server runs only builds an approved device starts.`

Otherwise it's refused with what to do, and no link opens. A private tag needs its image written
by digest. When Ployz
Cloud can't reach every server, the deploy queues as before and each server checks it. A coding
agent's deploy that still has services to sign is refused instead.

### Upgrade servers before you sign

A server on Ployz 0.3 checks no signature, and a server on 0.4.0-beta.2 or earlier reads only
the older kind, so each refuses every version a newer `ployz` signs, and every rollback or
redeploy Ployz Cloud sends it. Upgrade each such server first, then update `ployz` on your
devices. Before it signs, `ployz deploy` asks every server whether it checks signed versions,
which a server advertises as `ployz.container.signed-version.v1`. When one doesn't, the deploy
signs nothing and names the server to upgrade:

```text
error: Upgrade web-1 first: it runs an older ployz that can't check this device's signature, so it would refuse this deploy. Nothing signed or deployed.
next: ployz server upgrade beta web-1
```

When a server doesn't say whether it checks signed versions, and doesn't answer when asked, the
deploy signs nothing:

```text
error: Couldn't confirm web-2 can check this device's signature: it didn't answer. Nothing signed or deployed.
inspect: ployz status
```

When no server answers at all, the deploy signs nothing and says so:

```text
error: Couldn't reach your servers, so this device can't sign the deploy. Nothing deployed.
inspect: ployz status
```

### After a server upgrade, deploy each service once

A server starts a container only from a deploy an approved device signed. A container made before the server upgraded has no record of one, so the server won't
start it again. If a service stops, or a deploy that stops the old copy first fails, the old copy
stays stopped. The line reads `Could not restore the old Container.`, and a start from the
dashboard is refused too: `This container was made before this server's upgrade ...`. Deploy
the service again with a current `ployz`, which signs it, and the new copy runs.
Deploy each service once after you upgrade its server, so a restart never needs it.

Upgrading from 0.4.0-beta.2 or earlier, upgrade every server first, then deploy each environment
once with your passkey or `ployz deploy`. That deploy restarts each service once, including ones
that hold a volume. Until then, names from before the upgrade aren't
[kept signed](../services/private-networking.md#good-to-know).

### Deploy from the dashboard

**Deploy** at the bottom of an environment asks for your passkey once for the services a
server runs only signed. A sheet lists what your passkey
signs for each one, with its image by digest. Ployz Cloud looks up the digest of each tag, so
check it. **Approve with passkey** signs them all in one prompt, and the deploy starts.

When what the deploy saves isn't what your
[automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push) approves, the same prompt
signs the rule too, and the sheet shows it the way Save does. A deploy with nothing else to sign
asks for the rule alone. When the prompt can't sign the rule, the deploy goes on without it and
tells you why, and pushes hold until you approve it.

The sheet checks each mount the way [`ployz deploy` does](#deploy-from-an-approved-device), with your
servers' keys and containers as Ployz Cloud relays them. Mounts that keep the data you approved
fold into one line. Lines that need your yes show under **Needs your yes**, above the button, and
approving says yes to them. A volume from before this version that nothing vouches for stops the
deploy before it asks, with the same words as the CLI.

These need no prompt:

- A service that reaches no data: no sealed value, no volume, no login for its image, and nothing
  of its server beyond a plain container. Once it has run from a signed deploy or an automatic
  deploy rule, even one that has since ended, it asks like the rest: its name
  [stays signed](../services/private-networking.md#good-to-know).
- A version someone already signed: a change of only replicas, placement or limits, and a
  rollback or redeploy of a signed deployment.

Your servers must trust your passkey. Add it under **Account**, **Passkeys**. Before it asks,
Deploy stops and says why, and deploys nothing, when:

- you turned automatic deploys off: `Automatic deploys are off for web. Turn on Deploy every push to deploy it.`
- a service pulls a private tag: `web pulls a private tag. Write its image by digest (image@sha256:…).`
- a server runs an older `ployz`: `Upgrade web-1 first. Its ployz can't check your passkey.`
- Ployz Cloud can't read your servers: `Couldn't reach your servers. Try again.` A server that
  doesn't answer is named instead: `Couldn't reach web-1. Try again.`
- no server trusts your passkey: `Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.`

If your changes moved after you looked, Deploy asks nothing and says
`Your changes moved since you looked. Check them, then deploy again.` If the Config Store refuses
the signed deploy and your passkey must sign again, the sheet shows what you would sign now and
offers **Sign and deploy again**. If no passkey is needed now, the deploy starts.
**Cancel** deploys nothing and keeps your changes staged. A deploy that deletes data asks you to
confirm that too, and your one prompt covers it. Cancel that and the next deploy asks for your
passkey again.

If Ployz Cloud doesn't answer after you approve, the sheet says
`Couldn't reach Ployz Cloud. Try again.` **Try again** doesn't ask for your passkey.

**Create and deploy** on a new branch, **Deploy again** after a sync and **Redeploy** on a log
card ask the same way.

A [switch to or from a Cloudflare Tunnel](../services/cloudflare-tunnel.md) stops at a service
that needs your passkey. Deploy that service from the dashboard, then switch again.

A server refuses a container that reaches data from any deploy no person signed, and says what
the service reaches:

```
This service mounts a volume, so this server runs it only from a deploy an approved device signed.
```

The deployment page shows why, and what already runs keeps running. That covers an organization
token, `ployz up`, a restore, a first deploy to a new server, and a redeploy or rollback of a
deployment nobody signed, from its page or by [the agent](../account/agent-sidebar.md). Deploy
from the dashboard, run `ployz deploy` on an approved device, or push under an
[automatic deploy rule](github.md#approve-a-rule-for-deploys-on-push). Running
`ployz deploy` with a device key in a script approves whatever is saved in Ployz Cloud at that
moment, without anyone looking.

## When another deployment is running

An environment runs one deployment at a time. While one runs, **Deploy** reads **Deploy next**
and yours waits as **Queued**. If you deploy again while one is waiting, the newest one takes its
place, and the one it replaced reads **Superseded**.

A deploy started outside that queue can still reach a service after a newer deploy did. It fails for that service and leaves the newer version serving.
See [A newer deploy is already serving](../troubleshooting/deployments.md#a-newer-deploy-is-already-serving).

On the canvas, **Deploying** shows only on the services the running deployment restarts. A service
it leaves running keeps its staged changes on its card.

## Follow a deployment

While a deployment runs, the bottom bar shows it. Once it ends, the bar shows **Deployed** for a
few seconds, or **Failed** until you dismiss it, with **Logs** to open its page. **Failed** stays
after a reload and in other tabs, as long as the failed deployment is the newest one.

**Runs**, under **Deployments** in the sidebar, lists every deployment, newest first. A
deployment's page shows its status, who started it and, if it failed, the reason in red under the
header. A service that failed on its own reads Failed, and its failure is the reason, even when
you cancelled the deployment while it ran. A cancelled deployment shows no reason when nothing in
it failed on its own. `ployz deployment show` does the same, and `ployz deploy` then ends with
`Deployment #7 was cancelled.` Below that is one card for each build, service, volume and config it
touched, laid out left to right in the order they deploy. Click a card to see each server's progress and its
[logs](../observe/logs.md). A build's card shows the build output, and a service's card shows what
its new containers printed. A failed deployment opens on the card that failed.

![A failed deployment: its cards, with the failed service's servers and logs](../images/deployment-run.png)

| Status | What it means |
| --- | --- |
| **Waiting for CI** | Held until the push's checks pass. See [Wait for CI](github.md#wait-for-ci). |
| **CI failed** | A check failed, so the commit doesn't deploy. |
| **Queued** | Waiting for another deployment, or for GitHub Actions to build it |
| **Preparing** | Started: Ployz unpacks its upload, builds and plans. `ployz deployment ls` says `preparing`. |
| **Deploying** | Rolling out |
| **Deployed** | Every change it could make applied. **Deployed, 2 waiting** means two services have something that can't start yet. A whole service waits when it mounts a [volume with no writer](../services/volumes.md#when-a-volume-has-no-writer), or one on a [server that doesn't run services](../services/scaling.md#give-servers-different-jobs) or that is [down or not answering](../services/scaling.md#when-a-server-goes-down), runs in a [region](../services/regions.md) that has no server or that its volume is outside, or depends on a service that waits. It's left as it was and reads **Not attempted**. A service with counts in several regions can deploy its new replicas in some regions while [a region's replicas wait](../services/regions.md#when-no-server-is-in-the-region). Its line says how many replicas wait and how many still run the old version. The deployment says what each service waits on, and `ployz deploy` exits 3. Deploy again once the cause is fixed. `ployz deployment ls` says `applied, 2 waiting`. |
| **Nothing deployed, 2 waiting** | The only services with changes waited, whole or in some regions, so nothing changed on your servers and the Version isn't marked **Live**. `ployz deployment ls` says `nothing applied, 2 waiting`. |
| **Failed** | Something stopped it. A service marked **Not attempted** never started: it waited on a failed service, or it was left waiting as under **Deployed**, and the deployment still says what it waits on. A service you removed reads **Removed** even when a server that may run it is down or couldn't be checked: the deployment names that server, exits 3, and your next deploy of the whole environment removes the replica there. A service that already switched to its new replicas before the failure reads **Deployed**, even when an old replica it was removing is left running. Your next deploy of the whole environment removes that replica; a deploy of only some services, including one a Git push starts that doesn't ship this service, leaves it running. |
| **Cancelling** | A cancel is on its way. |
| **Cancelled** | Someone cancelled it. A service it stopped partway reads **Cancelled**, not **Failed**. |
| **Superseded** | A newer deployment took its place while it was queued. |
| **Unknown** | Ployz can't tell what applied. Redeploy it. |

If a deployment fails, see [My deploy failed partway](../troubleshooting/deployments.md#my-deploy-failed-partway).

### Cleanup

After a deployment from the dashboard or GitHub ran, a **Cleanup** card appears after its last
service. It stops the old replicas the deployment couldn't stop while it ran, for example because
a server was cut off. Click it to see each old replica that is still running and why.

| Cleanup | What it means |
| --- | --- |
| **Waiting** | Retrying the old replicas still running. The card updates without a reload. |
| **Done** | Every old replica stopped |
| **Left running on server-3** | After three tries over a few minutes, these old replicas still run. A server cut off before or during the deployment shows here too, since an old replica may still run on it. So does a service that failed to deploy: cleanup leaves its old replica, which may be the one serving. Cleanup names those at once, since trying again changes nothing. |
| **A newer Deployment took over** | You deployed again before cleanup finished. The newer deployment's own cleanup takes over. |
| **Failed** | Cleanup itself hit an error, shown on the card. A cleanup Cloud lost track of also ends here, after about an hour, and lists the old replicas it last saw running. |

A replica left running keeps serving alongside the new ones once its server is back. Deploy again
to stop it. CLI deployments clean up while they run, report a replica left running at the end, and
show no **Cleanup** card.

Work your cancel stopped reads **Cancelled**. Work that failed on its own reads Failed, even in a
cancelled deployment. So does work Ployz stopped for another reason, such as another service
failing. A service reads **Cancelled** when your cancel stopped its work, even if another service
failed.

Work Ployz stopped because the deployment lost its runner reads **Interrupted**. That happens when
another deployment takes the environment over, or when the runner stops reporting in time. The
deployment then reads **Unknown**. A build reads **Interrupted** when its deployment ended, or lost
its runner, before the build reported how it ended. Nothing says whether the build itself would
have failed, so deploy again (`ployz up` for an upload) to build it.

To follow and inspect deployments from the CLI, see
[Follow and inspect deployments](../cli/overview.md#follow-and-inspect-deployments).

## Confirm a deploy that deletes data

When a deploy would delete a volume's data, like a deleted volume or database, a removed mirror or
an old copy a move replaces, **Deletes data** lists what goes. When it makes a mirror the primary
because the primary's server isn't answering, the title reads **Data loss**, since the writes after
the mirror's last refresh are lost. Type your `project/environment`, like `my-app/production`, and
click **Deploy**. If you didn't mean it, cancel, then discard the change in **Details**.

**Save only** asks the same way, under **Next deploy deletes data**. Saving deletes nothing and
starts no deploy. Your confirmation is saved with the change, so the next deploy deletes that data
without asking again. That holds for a deploy you start and for one a Git push starts.

Deploying only some services, such as `ployz deploy web`, saves your changes too, so with a deleted
volume staged it asks the same way. It keeps that volume's data; your next deploy of the whole
environment deletes it without asking again.

Ployz asks again only when what it would delete changed since you confirmed. That happens when
another server starts holding that data, or someone stages a different copy change on that volume.
A Git push can't ask, so it keeps the data instead, and so does a push when Ployz can't reach your
servers to check. Your next deploy then asks.

If Ployz can't check your servers for that data, the deploy refuses and deletes nothing. It names
the server it couldn't reach or that didn't answer, or the server that answered with an error, and
shows any error on the `cause:` line. Try again once that server is back or the error is fixed. For the commands that
accept the loss from the CLI, see
[Follow and inspect deployments](../cli/overview.md#follow-and-inspect-deployments).

Next: [deploy without downtime](zero-downtime.md).
