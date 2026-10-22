---
title: Troubleshooting volumes
description: Fix volume moves that stop partway, busy volumes, and moves cut short by a lost server.
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
  `web-2`, then starts your service again on `web-1`.
- **Once `web-2` was told to start your service**, the volume belongs to `web-2`. Ployz finishes
  the move there, and `ployz volume runs data` shows the release that finished it. If that release
  fails too, your next deploy starts your service on `web-2` and finishes the move.

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

Earlier moves left unfinished copies on more than one server. Ployz keeps the newest one. Run the
lines it prints in order. They remove each server it names, then release or move again. Removing a
mirror is refused with the same lines.

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
its page. `ployz server rm` can't reset a silent server, so from the CLI use
`ployz server forget web-1 --confirm dead`.

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

`web-2` still owes the move its finish, but it is silent or holds no copy. Bring `web-2` back and
deploy again. If it's gone for good, forget it, then run the `ployz volume restore` line.

## A move to another server says the volume already has a copy

```
error: data already has a copy on web-1, data-web-1
next: ployz volume mirror rm data-web-1 --if-old-copy
```

A volume holds one other copy, and `ployz volume move` won't delete it for you. Run the `next:`
line, then move again. If that copy is a mirror, the line has no `--if-old-copy`. In Ployz Cloud,
**Make primary** deletes old copies on other servers for you.

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
