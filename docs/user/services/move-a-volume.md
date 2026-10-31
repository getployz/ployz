---
title: Move a volume
description: Move a volume and the service that mounts it to another server.
---

A volume lives on one server. With Ployz Cloud you can move it, and the service that mounts it,
to another server. Your service stops only for the final copy.

Every server with storage needs a newer Ployz before any volume moves. Until then the volume's
**Copies** names the servers to upgrade, for example "Upgrade db-1 to a newer Ployz".

## Move a volume to another server

A volume moves onto its [mirror](mirror-a-volume.md), so first add a mirror on the server you
want it on.

1. Open the volume and click its mirror under **Elsewhere** in **Copies**.
2. Click **Make primary**. The mirror shows **Primary** in pink until you deploy.
3. Click **Deploy**.

A mirror on a server running 0.3.0-beta.6 offers no **Make primary** and reads "Make primary needs
a newer Ployz on web-2.": that release can take the volume but can't put the moved service back in
web traffic. Upgrade the server first.

<!-- screenshot: a mirror's panel with Make primary -->

Details lists the steps: "Stop · Final sync · Start", with "Catch up" first when the mirror last
refreshed over an hour ago. Your service keeps running while the deploy catches up. Writes pause
from Stop until Start, so the final sync carries every last change. If the mirror can't catch up,
the deploy fails and the old server keeps serving.

The deploy moves the volume before it starts your services. While it runs, the volume's tray, the
bottom bar and the deployment page say what it's doing, for example "Moving to web-2". A service
that takes a while to stop or start doesn't fail the move.

**Cancel** lets the step under way finish, and the rest waits for your next deploy. If the deploy
fails, the change that failed is dropped, and the changes it never reached wait for your next
deploy. A move that already handed the volume to its new server isn't dropped, since only finishing
it brings the volume back. See [A move failed partway](../troubleshooting/volumes.md#a-move-failed-partway).

## The old copy a move leaves

The old server keeps a read-only copy, like `data-web-1`, under **Elsewhere**. It serves nothing
and never refreshes. Moving back sends only what changed since. If an earlier move onto that server
failed, Ployz may send the whole volume instead.

- Its × deletes it right away, after you click **Delete**. This can't be undone.
- Its **Restore** stages a restore from its last point. See [Restore a volume](restore-a-volume.md).
  While a staged restore reads the copy, × says "A restore reads this copy."

A move or a new mirror deletes old copies on other servers first, and Details says so.

## Drain a server

**Drain** on a server's page moves each volume there off at the next deploy, to its mirror or else
to the first other server that runs services. The page says how many, like "Next deploy moves 2
volumes". Each environment's next deploy moves its own volumes. Drain's moves wait for every server
with storage to have a newer Ployz too. Drain skips a server running 0.3.0-beta.6. When a volume
then has nowhere to go but that server, Drain names it: "Upgrade web-2 to move data there". A volume
nowhere would take reads "data has nowhere to move".

## Move from the CLI

The CLI moves a volume at once, without waiting for a deploy:

```sh
ployz volume move data --to web-2 --wait
```

`--wait` prints each step as the move reaches it, and so does `ployz volume runs data RUN`:

```
Copying to web-2.
web stopped for the final copy.
Starting web on web-2.
The Move of Volume data onto web-2 is done: run 3f2a9c1e-7b4d-4e2a-9f1c-2b8d6e0a5c47.
web was stopped 26s.
```

The last line says how long your service was down. `ployz volume runs data` keeps it as the
move's message. If Ployz had to ask the new server again to start your service, for example
because Ployz restarted there, it can't tell exactly when the service started. The line then
gives an estimate and says why: `web was stopped about 43s (its start was retried)`. While the old replica is stopped, `ployz status` names it as not serving, with the
code it exited with. The row goes once the replica on `web-2` runs.

If `web` has one replica, its domain answers
[`503 · NOT_RUNNING`](../troubleshooting/app-not-reachable.md#this-app-isnt-running) from about when
the old replica stops until the replica on `web-2` serves. Clients can retry at once rather than
wait on the stopped replica.

The old server keeps an old copy, `data-web-1`. If the volume already has a mirror on `web-2`, the
move starts from it. To move to a third server, first remove the old copy with
the `ployz volume mirror rm` line the move prints.

`ployz volume move`, `release`, `restore`, `mirror`, `mirror rm` and `sync --full` each start a run
in Ployz Cloud and print its id. Add `--wait` to wait for it to finish. A volume takes one run at a
time, and none while a deploy runs in its environment. `ployz volume runs data` lists what ran on
the volume and how it ended. `ployz volume release data` stops a move or a mirror that's still
running.

A deploy never waits for a move. Until `web-2` is told to start your service, a deploy takes the
move over. Your service deploys on the old server, and Ployz asks for the move again once the
deploy ends. After that, a deploy follows the move and
starts your service on `web-2`.

In a [Protected environment](../environments/environments.md#protect-an-environment), a move,
restore, release, mirror removal or resize from a member, or from a token or an agent without
`environments:protect`, waits for an owner or admin to approve it. `ployz` waits for the approval.
Adding a mirror and syncing don't ask.

If a move stops partway or a command is refused, see
[Troubleshooting volumes](../troubleshooting/volumes.md).

## Good to know

- **A server must answer for its volume to move.** If the volume's server is down, neither a
  move nor a release can run, and the run says which server did not answer. Copies offers no move
  while it's silent, since it may still be writing. See
  [When its server goes down](volumes.md#when-its-server-goes-down).
