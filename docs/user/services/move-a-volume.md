---
title: Move a volume
description: Move a volume and the service that mounts it to another server.
---

A volume lives on one server. `ployz volume move` takes it, and the service that mounts it, to
another server:

```sh
ployz volume move data --to web-2
```

Ployz Cloud copies the volume to `web-2` while your service keeps running, then stops it on
`web-1` for a final sync and starts it on `web-2`. The service is down only for that last step.
The command prints the run's id. Add `--wait` to wait for it to finish.

## What a move needs

Before anything stops, Ployz Cloud asks `web-2` whether it would start your service, and asks again
just before the switch. The move is refused, and your service keeps running on `web-1`, when:

- **No approved version runs there.** `web-2` starts the service only from a version an approved
  device signed, or that a
  [deploy rule](../deploy/github.md#approve-a-rule-for-deploys-on-push) built. A rule's build
  runs only on the servers the rule lists. Deploy the service so a version covers `web-2`, then
  move again.
- **Other data moves with it.** The volume must be the only data the service mounts. A bind mount,
  another volume or a Docker volume can't move along, and the refusal names it.
- **Something else uses the volume.** Only your service's container may run on `web-1` with the
  volume mounted. The refusal names the others.
- **`web-2` runs an older Ployz.** Upgrade it first.

A move needs no approval of its own when the service may run in any region, as its last deploy's
approval showed. When the service is kept in a region, a move to a server in another region needs
a deploy that puts the service there first. Where an approval has an approved version to compare
with, such as the Deploy sheet in Ployz Cloud, it reads "moves data to syd". The deploy waits for
the move with the line that runs it. See
[When a volume is outside the region](regions.md#when-a-volume-is-outside-the-region).

A [drain](../servers/manage-servers.md#services-with-a-volume) moves each service with its volume
the same way, one at a time, and names each one it can't move, with the reason.

If you staged a mirror change before the move, the next deploy refuses it: the change was made for
the server the volume left. Discard it, edit the volume's mirrors again and deploy.

## The old copy a move leaves

The old server keeps a read-only copy, like `data-web-1`, under **Elsewhere**. It serves nothing
and never refreshes.

- Its × deletes it right away, after you click **Delete**. This can't be undone.
- Restoring its last point into a new volume is turned off in this version. See
  [Restore a volume](restore-a-volume.md).

A new mirror deletes old copies on other servers first, and Details says so.

`ployz volume move`, `release`, `restore`, `mirror`, `mirror rm` and `sync --full` each start a run
in Ployz Cloud and print its id. Add `--wait` to wait for it to finish. A volume takes one run at a
time, and none while a deploy runs in its environment. `ployz volume runs data` lists what ran on
the volume and how it ended. `ployz volume release data` stops a mirror that's still running.

In a [Protected environment](../environments/environments.md#protect-an-environment), a
move, restore, release, mirror removal or resize from a member, or from a token or an agent without
`environments:protect`, waits for an owner or admin to approve it. `ployz` waits for the approval.
Adding a mirror and syncing don't ask.

If a run stops partway or a command is refused, see
[Troubleshooting volumes](../troubleshooting/volumes.md).

## Good to know

- **Old copies from earlier moves stay.** A copy a move left before this version is still under
  **Elsewhere**, and you can restore from it or delete it.
