---
title: Move a volume
description: Copy a volume to another server, and move it and its service there.
---

A volume lives on one server. With Ployz Cloud you can keep a read-only copy of it on a second
server, called a mirror, and move the volume and the service that mounts it to another server.
Without Cloud, volumes stay on the server that has them.

## In Ployz Cloud

Open a managed volume after its first deploy. **Copies** lists the places its data lives. **Live**
is the server your service writes it on. **Elsewhere** holds its mirror and any old copy a move
left, each with when it last refreshed. Under **Elsewhere**, **Add copy** adds a mirror while the
volume has none.

Every server with storage needs a newer Ployz before any volume moves or mirrors. Until then
Copies only shows, and names the servers to upgrade, for example "Upgrade db-1 to a newer Ployz".
**Drain**'s moves and the 15-minute refresh wait too.

What moves data waits for your next deploy, which runs it before your services. That covers
**Add copy**, **Make primary** and **Remove mirror**. Until then the change shows in its colour.
Pink is a new primary, green a new copy, and red a copy going away. It counts in the bottom bar and
Details, and you drop it in Details. A deploy that removes a mirror asks you to type your
`project/environment` first, because its data goes with it. See
[Confirm a deploy that deletes data](../deploy/deployments.md#confirm-a-deploy-that-deletes-data).

- **Add copy** offers only servers that run services. A volume has one mirror for now.
- **Make primary**, on a mirror, moves the volume and its service to the mirror's server. The
  deploy sends what changed since the last refresh, then pauses writes for the final sync.
- A move leaves the old server holding an old copy. It serves nothing and never refreshes. Its ×
  deletes it right away, after you confirm **Delete copy**. Its **Restore** stages a restore from its
  last point. While a staged restore reads the copy, × says "A restore reads this copy."
- A move or mirror onto a server that holds an old copy deletes that copy first. Details says so.
- When the primary's server stops answering, Copies says "web-1 is not answering" and when
  changes are lost from, for example "Changes after 10:36 lost". **Use mirror**, or **Make
  primary** on the mirror, moves the volume there at the next deploy, which asks you to confirm
  **Data loss**.
- **Drain** on a server page also moves each primary there off at the next deploy, to its mirror
  or else to the first other server that runs services. The page says "Next deploy moves 2
  volumes". Each environment's next deploy moves them.

Details lists the steps each change takes at the next deploy. Making a mirror the primary reads
"Stop · Final sync · Start", with "Catch up" first when the mirror last refreshed over an hour
ago. Writes pause from Stop until Start, so the final sync carries every last change. A new mirror reads "Seed", and removing a copy reads "Delete".

Mirrors refresh on every deploy and every 15 minutes. While a deploy moves a volume, its tray, the
bottom bar and the deployment page say what it is doing, for example "Moving to web-2". Cancel
lets the step under way finish, and the rest waits for your next deploy. If the deploy fails, the
change that failed is dropped and the changes it never reached wait for your next deploy again.
A move that already handed the volume to its new server is not dropped, since only finishing it
brings the volume back; see [When a move stops](#when-a-move-stops).

On the canvas the mirror hangs under its volume's tray as its own small card, for example
`data-web-2` with "Mirror · 4 minutes ago". A mirror more than an hour behind shows its age in
amber with a warning sign. A staged mirror shows **New** in green, a staged removal shows
**Removing** in red, and a mirror about to become the primary shows **Primary** in pink. Old copies
stay in the volume's Copies. A staged move shows on the tray, for example "Moving to web-2".

Click a mirror, on the canvas or under **Elsewhere**, to open it. It shows its server, when it last
refreshed, **Refresh now**, **Make primary** and **Remove mirror**. Refresh now runs a refresh at
once. The mirror reads "Refreshing", then "Mirror refreshed" pops up and its age resets. If
anything else is running on the volume, Refresh now says so instead of queueing. A mirror more than
an hour behind says **Refresh skipped** and why. Either its server is not answering or its last
refresh failed. It also says how much of its writes a failover would lose. A staged mirror has none
of these actions, so discard it in Details.

A mirror or an old copy also gives the volume restore points. See [Restore a volume](restore-a-volume.md).

The commands below do the same from the CLI, one run at a time, without waiting for a deploy.

## Mirror a volume

A mirror holds the volume's data as of its last sync. From the CLI it refreshes only when you sync
it. With Ployz Cloud it also refreshes on every deploy and every 15 minutes. A volume has at most
one.

```
ployz volume mirror data --to web-2     # copy data to web-2; it shows as data-web-2
ployz volume sync data                   # send what changed since the last sync
ployz volume mirror rm data-web-2        # delete the mirror
ployz volume runs data                   # what ran on this volume, and how it ended
```

Each command starts a run in Ployz Cloud and prints its id. Add `--wait` to wait for it to
finish. One run at a time per volume. A second one is refused and says how to stop the first:

```
VolumeBusy: data is busy with a mirror; ployz volume release data stops it
```

## Move a volume to another server

```
ployz volume move data --to web-2
```

Your service keeps running while Ployz copies the volume to `web-2` in rounds. Then Ployz takes
the service on the old server out of ingress, lets requests in flight finish for up to 3 seconds,
and stops it. It takes a last snapshot of the volume. `web-2` pulls that snapshot and starts the
service there. Then Ployz removes the old server's container. Your service is down only from the
stop to the start on `web-2`. A service that takes a while to stop or start doesn't fail the
move. Ployz waits for Docker, up to ten minutes for each stop or start.

The old server keeps a read-only copy, shown as `data-web-1`. Moving back sends only what
changed since. If the volume already has a mirror on `web-2`, the move starts from it.

A deploy never waits for a move. Until `web-2` is told to start your service, a deploy takes the
move over: the run ends with "a Deploy took this run over", your service deploys on the old
server, and the move is requested again once the deploy ends. From the moment `web-2` is told to
start your service, a deploy follows the move and starts your service on `web-2`. A deploy with a
volume change staged on that volume fails instead, with "data is starting on web-2; it finishes
there", and the change waits for your next deploy.

## When a move stops

A move can stop if a server or Ployz Cloud goes away during it. `ployz volume runs data` shows
why. What happens next depends on how far it got.

- **Before Ployz stops your service on the old server**, your service keeps running there, with
  all its data. The run fails and names what went wrong. Run the move again when you're ready.
- **After it stops and before `web-2` starts it**, Ployz deletes the copy on `web-2` first, then
  starts your service again on the old server. The run fails and names what went wrong.
- **Once `web-2` is told to start your service**, the volume belongs to the new server. Ployz
  finishes the move there: `web-2` starts your service and the old server's container goes. The
  run fails and names what went wrong, and `ployz volume runs data` shows the release that
  finished it.

If Ployz Cloud lost the run itself, the cleanup starts within about an hour. To start it now, or
to stop a move or a mirror that's still running, release the volume:

```
ployz volume release data
```

Release stops the run on the volume and starts your service again on the old server, with all
its data. Before `web-2` starts your service, Release deletes the copy on `web-2` first. With no
run in progress, Release repairs a move that stopped. Once `web-2` is told to start your
service, the move can only finish there, so Release is refused:

```
data is starting on web-2; it finishes there
```

Finishing on `web-2` still needs the old server to answer for its container. If the old server
dies, the move fails, and your service stays down until it comes back or you restore the volume.

### When you remove a server mid-move

Removing a server never waits for a move. A move or mirror still running to or from that server
is released, and your service stays on the old server. If the old server is the one you removed,
the release fails and names the copy to restore from:

```
data has no writer and no stopped Service on its copies; ployz volume restore data --from web-2
```

If you remove `web-2` after it was told to start your service, the move fails and names the copy
left:

```
data's writer was on web-2 (removed). Restore from web-1's copy: ployz volume restore data --from web-1
```

## Restore a volume

When the server that wrote a volume is gone, make a copy on another server the writer:

```
ployz volume restore data --from web-2
```

Restore needs `web-2` to hold the only copy of the volume. If other servers hold copies, it is
refused and names them; remove them with `ployz volume mirror rm` first. A mirror holds the data as of
its last sync, so restoring one loses the writes after it, and the run says when that was:
`data restored on web-2 from <time>; writes after that time are lost`. Restore doesn't start your service.
Deploy it next.

## Good to know

- **A server must answer for its volume to move.** If the volume's server is down, neither a
  move nor a release can run, and the run says which server did not answer. See
  [When its server goes down](volumes.md#when-its-server-goes-down).
- **A mirror is a backup, not an offsite one.** It holds the data as of its last sync, on another
  of your servers. For a copy off your servers, see [Back up a database](databases.md#back-up-a-database).
- **A mirror that fell behind for good** is refused with `data-web-2 diverged`. Run
  `ployz volume sync data --full` to copy everything again.
- **Deleting a volume deletes its mirror.** So does deleting its environment. A mirror that is
  the volume's only copy left is removed only with `--confirm data`, because its data goes with
  it.
