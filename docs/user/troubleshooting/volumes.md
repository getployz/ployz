---
title: Troubleshooting volumes
description: Fix volume moves that stop partway or are refused, busy volumes, volumes holding other data, and moves cut short by a lost server.
---

Fixes for [moving](../services/move-a-volume.md), [mirroring](../services/mirror-a-volume.md) and
restoring a volume. The examples move the volume `data` from `web-1` to `web-2`.
`ployz volume runs data` shows what ran on a volume and how it ended.

## A move failed partway

`ployz volume runs data` shows the move as failed, with what went wrong. A move can stop when a
server or Ployz Cloud goes away during it. What happens next depends on how far it got.

- **Before your service stopped on `web-1`**, it keeps running there with all its data. Run the
  move again when you're ready.
- **After it stopped, and before `web-2` was told to start it**, Ployz deletes the copy on
  `web-2`, then starts your service again on `web-1`. This is also what happens when `web-2`
  refuses your service at its last check before the switch. The run says why; see
  [A move says the other server can't start the service](#a-move-says-the-other-server-cant-start-the-service).
- **Once `web-2` was told to start your service**, the volume belongs to `web-2`. Ployz finishes
  the move there, and `ployz volume runs data` shows the release that finished it. If that release
  fails too, or `web-2` won't start your service, your next deploy starts it on `web-2` and
  finishes the move.
  The release pulls your service's image on `web-2` when it isn't there, so if the move failed
  because your registry didn't answer, `ployz volume release data` finishes it once the registry
  is back.

If the release put the volume back on `web-1` and your service didn't start there, the run says
"web-1 holds data again, but its Service didn't start there". Deploy, and the deploy starts it.

To finish the cleanup now, or to stop a move or a mirror that's still running, release the volume:

```sh
ployz volume release data
```

If Ployz Cloud lost the run itself, the cleanup starts on its own within about an hour. With no
run in progress, a release repairs a move that stopped. If the copies changed since you started
the release, it deletes nothing, leaves your service stopped, and asks you to release again.

## A volume command says the volume is busy

```
data is busy with a mirror; ployz volume runs data 8c41d2e0-5a7f-4b19-a3e6-0f2d9b7c1e58 follows it, and ployz volume release data stops it
```

A volume runs one move, mirror, restore or release at a time. Follow the run with the
`ployz volume runs` line, or stop it with `ployz volume release data`, then try again. A mirror
removal can't be stopped and says it "finishes on its own". A release in progress only says how to
follow it.

## A volume command says the volume is deploying

```
data is deploying; the Deploy ends its run
```

A deploy is running in the volume's environment. Run the command again once the deploy ends.

## A move ended when you deployed

```
a Deploy took this run over
```

A deploy never waits for a move. If you deploy before `web-2` is told to start your service, the
deploy takes the move over. Your service deploys on `web-1`, and Ployz asks for the move again
once the deploy ends. Nothing is lost.

## A deploy says the volume is starting on another server

```
data is starting on web-2; it finishes there
```

A move is past the point where `web-2` was told to start your service, so it can only finish
there. A deploy follows the move and starts your service on `web-2`, but a deploy with a volume
change staged on `data` fails with this message, and the change waits for your next deploy. A
release is refused the same way while the move runs. Wait for the move to end, then deploy.

If the deploy fails with "data is finishing its move on web-2; Deploy again", a release is still
starting `web-2`. Deploy again once the release ends.

## A deploy says the volume was placed on another server

```
Volume data was placed on web-1, which can't run this service. Let web-1 run it, or remove data and add it again. It holds no data yet.
```

A new volume starts on the server its first deploy chose, even when that deploy stopped before
the volume was made. A step it sent may still arrive there, so no other server starts the volume
beside it. Let `web-1` run the service again, by its labels, region or `--accepts-services`, or
free space on it when the message says it has no room. Since the volume holds no data yet, you can
also remove it and add it again, and its next deploy picks a server afresh.

If a deploy fails with "Another deploy placed volume data on a different server first. Deploy
again.", two deploys chose a server for it at once. Deploy again, and it starts where the first
one chose.

## A command says the volume finishes on another server

```
data finishes on web-2; ployz volume release data finishes it now
```

A move stopped after `web-2` was told to start your service, and isn't finished yet. Until it is,
a move, a mirror, a full sync, a resize, a restore point's restore and removing the copy on
`web-2` are refused. Run the
`ployz volume release data` line to finish it. A plain `ployz volume sync` still works. While
Ployz Cloud can't read your servers, removing the volume's mirror is refused until it can.

Restoring the volume stays open, unless a release is waiting for a silent server. Once a restore
takes over, the move no longer needs finishing.

## Release says the move stopped before its first snapshot

```
data on web-1 stopped before its first snapshot; ployz volume move data --to web-2
```

The move stopped before it copied anything, so there is nothing to release. Your data is still on
`web-1`. Run the `ployz volume move` line it prints.

## Release says the volume has unfinished cuts

```
data has unfinished cuts on web-1 and web-3, so release cannot finish it on web-2; ployz server rm web-1, then ployz server rm web-3, then ployz volume release data
```

Earlier moves left unfinished copies on other servers, and they block the move from finishing on
`web-2`. Run the lines it prints in order. They remove each server it names, then release.
Removing a mirror is refused with the same lines.

When no move is owed a finish, a release with unfinished copies on more than one server says it
"cannot pick a writer" instead. It keeps the newest copy, and its lines remove each server whose
copy it can't keep, then release or move again.

## Release says another copy is still a writer

```
data-web-1 is still a writer copy, so data can't finish on web-2; ployz volume restore data --from web-1 keeps web-1's data
```

Another server still holds a copy your service may have written to. To keep that server's data,
run the `ployz volume restore` line it prints, then deploy. Removing a mirror is refused the same
way.

## A release waits for a server to answer

```
waiting for web-1 to answer (check 1 of 8)
```

A release needs every server to answer. It waits up to 15 minutes for a silent one. When Ployz
Cloud can't read your servers at all, the run shows "waiting for the Servers to answer". While it
waits, restoring the volume is refused.

If the server stays silent, the release fails and your service stays down:

- "web-1 did not answer for 15m; data still finishes on web-2; ployz volume release data tries
  again" when the move belongs to `web-2`.
- "web-3 did not answer; data's copies are unknown" when it doesn't.

Bring the server back and run `ployz volume release data` again. If the server is gone for good,
[forget it](../servers/manage-servers.md#forget-a-server-that-is-gone) with **Forget server** on
its page, or from the CLI with `ployz server forget web-1 --confirm dead`.

Once you forget `web-1`, the release fails with "data has no stopped Service container", or "data
changed since this release was planned; release it again" if you started it yourself. Either way,
make the copy on `web-2` the volume, then deploy your service:

```sh
ployz volume restore data --from web-2
```

## A volume has no writer after you removed a server mid-move

```
data has no writer and no stopped Service on its copies; ployz volume restore data --from web-2
```

Removing a server never waits for a move. A move or mirror to or from that server is released,
and your service stays on the old server. If the server you removed was that old server, nothing
is left to start your service on. If you removed `web-2` after it was told to start your service,
you see:

```
data's writer was on web-2 (removed). Restore from web-1's copy: ployz volume restore data --from web-1
```

Run the `ployz volume restore` line it prints, then deploy. A removed server is owed no finish, so
moves of the volume are open again.

## A deploy says the volume finishes on a server that did not answer

```
data finishes on web-2, which did not answer. Bring web-2 back or forget it with ployz server forget web-2 --confirm dead. Restore from web-1's copy: ployz volume restore data --from web-1
```

`web-2` still owes the move its finish, but it is silent. Bring `web-2` back and deploy again. If
it's gone for good, forget it, then run the `ployz volume restore` line.

If `web-2` answers but holds no copy, the deploy says "data finishes on web-2, which holds no copy
to start" instead. Remove `web-2` with the `ployz server rm` line it prints, then run the
`ployz volume restore` line.

## A deploy says the volume holds another volume's data

```
Volume 'data' holds another Volume's data under the same name, not the data this Service was approved for
```

A server opens a volume's data only for a service whose approval names that volume. An earlier
volume with the same name, or another volume's copy, left this data on `web-1`, so your service
doesn't start there and nothing is deleted. Make the copy you want the volume, then deploy:

```sh
ployz volume restore data --from web-2
```

A move or restore that would make a copy the volume, and a new volume made under that name, are
refused the same way when a service on the server, running or stopped, was approved for other data
under that name: "Volume data is mounted by a Container approved for other data". Remove that
service from the server, or restore the data it was approved for. A mirror is never refused this
way, since no service opens it.

An external volume is one made outside Ployz, so a service never opens a volume Ployz provisioned
through an external mount, even one Docker shows without labels:

```
Volume 'data' is a Ployz Provisioned Volume, and an External mount opens only a Volume made outside Ployz
```

Mount it as a provisioned volume, or give the external volume another name.

## A volume change says Docker is not answering

```
Docker is not answering on this Machine, so Volume data keeps its copies as they are until it does
```

A deploy that makes a new volume says "so no new data is made under Volume 'data' until it does".

A server changes which copy is the volume, or makes a new one, only while Docker can say which
services use it. Start Docker on that server, or wait for it to come back, then run the command
again. In Ployz Cloud, a move, release or restore tries again on its own and fails with "Docker
isn't answering on web-1" only if Docker stays down.

## A move to another server says the volume already has a copy

```
error: data already has a copy on web-1, data-web-1
next: ployz volume mirror rm data-web-1 --if-old-copy
```

A volume holds one other copy, and `ployz volume move` won't delete it for you. Run the `next:`
line, then move again. If that copy is a mirror, the line has no `--if-old-copy`.

## A move says the other server can't start the service

```
web-2 can't start web: no approved version of web runs there. Deploy web so it can, then move data
```

Before a move stops anything, `web-2` checks it would start your service. Nothing stopped, and
your service keeps running on `web-1`. What to do depends on the reason:

- **No approved version runs there.** Deploy the service, then move again.
- **`web-2` can't pull the image**, as in "image pull failed". The registry refused the service's
  login. Fix the service's registry credential, deploy, then move again.
- **The deploy rule that built it doesn't list `web-2`.** Add `web-2` to the
  [rule](../deploy/github.md#approve-a-rule-for-deploys-on-push), or deploy the service again, then move again.
- **Another container uses the volume**, as in "web and worker all use data on web-1". Stop the
  others, then move again.
- **The service mounts other data**, as in "web also mounts uploads, which can't move with data".
  Remove that mount from the service, deploy, then move again.
- **`web-2` runs an older Ployz.** Upgrade it, then move again.

## A move says the service's image is no longer on the server

```
image sha256:3f…, which this Writer was promoted to run, is no longer on this Server, and a start after its promote pulls nothing
```

`web-2` gets your service's image before it takes over the volume, and remembers which image
that was. If `web-2` restarted during the switch, it starts your service from that same image
and doesn't pull again, so a registry outage or a tag pushed since can't change what runs. This
message means the image was deleted from `web-2` in between. Deploy the service, and the deploy
starts it on `web-2` with its image.

A move or a release that would start your service on a server whose Ployz doesn't remember the
image this way starts nothing there and asks you to upgrade that server: `Volume move needs a newer
Ployz on web-2`. A move stops before it copies anything.

## A move says the volume plugin runs an older build

```
the volume plugin on this Server promoted the copy without recording the start's image, so it runs an older build than this daemon. Restart ployz-volume-plugin on this Server, then retry
```

The server's Ployz was upgraded, but its volume plugin still runs the old build, which doesn't
remember the service's image when it switches the volume. Restart it with
`systemctl restart ployz-volume-plugin` on that server, then retry the move.

## A deploy refuses a mirror change after a move

```
data moved to another server after its mirrors were edited. Discard that change, edit its mirrors again and deploy
```

You edited the volume's mirrors, then moved it before deploying. Discard the change in Ployz
Cloud, edit the mirrors again and deploy.

## Deleting an old copy says it is no longer an old copy

```
data-web-1 is no longer an old copy, so nothing was deleted
```

A mirror run made that copy the volume's mirror before the delete reached its server, so Ployz
kept it. A deploy that deletes an old copy or removes a mirror is refused the same way when the
copy changed. To remove the mirror, use **Remove mirror** on it. A server with Ployz 0.3.0-beta.6
or older deletes the copy anyway.

A Ployz Cloud from before `--if-old-copy` refuses a `ployz volume mirror rm --if-old-copy` line and
deletes nothing.

## A move or mirror says the copy may hold writes the writer doesn't have

```
this Machine's copy of … may hold writes … doesn't have, so the pull keeps it rather than replace it with a full copy; delete it with `ployz volume mirror rm` to take a fresh one
```

The server you moved or mirrored to already held a copy of the volume that may hold writes no
other server has, such as a copy a server kept when you added it back, and
the writer can't send changes on top of it. Ployz keeps that copy and stops instead of replacing
it. Delete it with `ployz volume mirror rm` only once you no longer need what it holds. The next
move or mirror then copies the volume in full.

A server running Ployz 0.4.0-beta.2 or earlier replaces such a copy with a full one instead, so
Ployz Cloud asks you to upgrade it first. See
[Ployz asks you to upgrade a server first](#ployz-asks-you-to-upgrade-a-server-first).

A mirror last refreshed by a Ployz before 0.3.0, such as a 0.3.0 beta, has no record of what it
received, so Ployz counts it as a copy that may hold such writes. Once its writer drops that
mirror's newest snapshot, every refresh stops with this message. Ployz before 0.3.0 is outside the
stable promise: delete the mirror with `ployz volume mirror rm`, then mirror again.

A mirror's server records what it received only once the writer confirms it. If the writer drops
that snapshot while the mirror's server can't reach it, the confirmation fails, the server forgets
what it received, and it counts the mirror as a copy that may hold such writes. Every later refresh
then stops with this message. Delete the mirror with `ployz volume mirror rm`, then mirror again.

## A full sync says the copy may hold writes no other server has

```
error: data-web-1 may hold writes no other Server has, so sync --full keeps it
next: ployz volume mirror rm data-web-1 --if-old-copy
```

`ployz volume sync --full` deletes the mirror's copy before it copies the volume again, so it
won't run on an old copy or on a copy that may hold writes no other server has. Nothing was
deleted. Run the `next:` line only once you no longer need what that copy holds, then mirror again.
If the copy is a mirror, the line has no `--if-old-copy`.

A server running Ployz 0.4.0-beta.2 or earlier gets no full sync. See
[Ployz asks you to upgrade a server first](#ployz-asks-you-to-upgrade-a-server-first).

## Ployz asks you to upgrade a server first

```
error: Upgrade web-1 first. Its Ployz would delete data-web-1 even if it holds writes no other Server has, so nothing was changed
error: Upgrade web-1 first. Its Ployz would replace data-web-1 with a fresh copy even if it holds writes no other Server has, so nothing was changed
```

A server running Ployz 0.4.0-beta.2 or earlier deletes any copy Ployz Cloud names, and replaces a
copy it can't add to with a full one, whatever the copy holds. It can't say whether the copy holds
writes no other server has, so Ployz Cloud sends it nothing that would delete or replace a copy.
Nothing was changed. Upgrade the server, then run the command again.

Ployz Cloud checks the server's Ployz again as it sends each such step. If the server went back to
such a Ployz after the command started, the command stops there with `..., so it wasn't asked to`
in place of `so nothing was changed`. The copy is as it was, and steps that came before it, like a
snapshot of the writer, stay done.

That covers:

- `ployz volume mirror rm`, **Delete copy** in Ployz Cloud and a deploy's delete of a copy there.
- `ployz volume sync --full` onto it.
- `ployz volume leftovers rm` of a copy there, which says `Its Ployz would delete
  shop-production_vol-3f2a's copy`.
- A mirror, a move or a mirror refresh onto a copy there, unless the pull starts from the
  writer's newest snapshot and so only adds to the copy. A refresh says `data-web-1 refused the
  pull:` before the message.

A release keeps the copies on such a server and still starts your service again. The run ends done
and names each copy, like `kept data-web-1, since web-1's Ployz would delete it even if it holds
writes no other Server has (upgrade web-1, then ployz volume mirror rm data-web-1 deletes it)`. The
cleanup after a deleted volume keeps them too.

## A delete queued before an upgrade deletes nothing

```
data's delete was queued by an older Ployz Cloud, which didn't record the copy it deletes, so nothing was deleted. Delete the copy again.
```

A delete waiting when Ployz Cloud was upgraded has no record of the copy you confirmed, so Ployz
deletes nothing. Delete the copy again to confirm it as it is now.

## Ployz keeps a copy that may hold writes no other server has

```
data's copy on web-1 may hold writes no other Server has; only ployz volume mirror rm deletes it
```

A server running a Ployz later than 0.4.0-beta.2 deletes such a copy only when someone chose to
delete it: you ran `ployz volume mirror rm`, or you confirmed a deploy whose **Deletes data** listed
it. Anything else that would delete it stops with this message, and the server keeps the copy as it
was. Run `ployz volume mirror rm` on the copy once you no longer need what it holds, then run the
command again.

A release that meets such a copy keeps it and still starts your service again. The run ends done and
names the copy, like `kept data-web-1, which may hold writes no other Server has (ployz volume
mirror rm data-web-1 deletes it)`.

## A delete says the copy changed since it was confirmed

```
data-web-2 changed since its delete was confirmed, so nothing was deleted. Run ployz volume mirror rm data-web-2 to delete it as it is now
```

A delete removes only the copy you confirmed, as its server showed it then: the same slot, the same
newest snapshot, and the same answer to whether it may hold writes no other server has. If the
copy was replaced, refreshed or written to before the delete ran, Ployz deletes nothing. Run the
command again to see the copy as it is now. A deploy's delete says `Deploy again to confirm it as it
is now` instead, and the deploy asks again with the copy's new details.

The server checks again as it deletes the copy, so a refresh that lands after the delete was sent
keeps the copy too.

**Delete copy** in Ployz Cloud deletes the copy its dialog showed. If the server reports another
by the time you confirm, it says `web-1's copy changed since you opened Delete, so nothing was
deleted. Look at it again before you delete it`. Open **Delete copy** again.

## Copies left behind by a deleted volume

After a deploy deletes a volume or an environment, Ployz deletes the volume's mirrors on your other
servers. It keeps each copy that may hold writes no other server has, such as an old copy a move
left or a copy a server kept when you added it back. It also keeps every copy
on a server running Ployz 0.4.0-beta.2 or earlier, since that server would delete it whatever it
holds. When the cleanup deletes some copies, its run names each one it kept, like `kept
data-web-1, which may hold writes no other Server has (ployz volume leftovers rm
shop-production_vol-3f2a --on web-1 deletes it)`.

List the copies Ployz kept:

```
ployz volume leftovers
```

Each row shows the copy's server, slot, last snapshot, size, and whether it may hold writes no other
server has. In Ployz Cloud, a server's page lists its copies under **Copies of deleted volumes**,
and **Copy delete command** copies the command for one. To delete one, run `ployz volume leftovers rm` with the volume and server from the
list. The first run shows the copy and deletes nothing:

```
error: shop-production_vol-3f2a on web-1: slot 7, last snapshot dep-1763600000 (2025-11-20 01:33 UTC), up to 10.0 GB; may hold writes no other Server has. Run ployz volume leftovers rm shop-production_vol-3f2a --on web-1 --confirm 7-1234-written to delete it
```

Run the line it prints once you no longer need what the copy holds. `ployz` shows what the delete
destroys and asks you to say yes. If the copy changed since you looked, the delete stops again and
shows it as it is now. An agent can't delete such a copy while a person in the organization could
say yes. Its refusal names the line for a person to run.

When a server refuses for a reason your Ployz Cloud doesn't know, the run shows the server's own
message after `web-1 refused:`. Upgrade Ployz Cloud if the message asks you to.

### With a self-hosted Ployz Cloud from before keep-written

```
web-1 is busy with data
```

A self-hosted Ployz Cloud from before servers kept these copies sends every delete the same way and
can't read why the server refused. It shows this message, or `web-1 is still busy with data`, for
a delete of any copy that may hold writes no other server has. Every old copy a move left is one.
That covers `ployz volume mirror rm`, `ployz volume sync --full`, a deploy that deletes an old copy
or a mirror, the cleanup after a deleted volume and, rarely, `ployz volume release`. The server
keeps the copy and nothing is deleted. Trying again gives the same message, and the cleanup tries
again after each deploy and each hour. The server's own message, `Volume … on this Machine may hold writes no other
Machine has, so it was kept`, never reaches that Ployz Cloud. Upgrade Ployz Cloud, then run the
command again.
