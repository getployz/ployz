---
title: Move a volume
description: Move a volume and the service that mounts it to another server.
---

A volume lives on one server. Moving it, and the service that mounts it, to another server is
turned off in this version of Ployz. A server starts a new container only for a deploy an approved
device signed or a rule approved, and a move has to start the service on the new server. Ployz
Cloud refuses a move before anything stops:

```
Moving a volume to another server is turned off in this version of Ployz, so data can't move to web-2.
```

A volume's mirror offers no **Make primary**, and `ployz volume move` is refused the same way.
[Drain](../servers/manage-servers.md#change-what-a-server-does) can't move a volume off a server
either, and says which volume holds it up.

To put a volume on another server today, [mirror it](mirror-a-volume.md) there, stop the service,
run `ployz volume sync data`, and [restore the mirror](restore-a-volume.md#restore-a-volume-in-place)
once it holds the only copy. Then deploy from an approved device.

## The old copy a move leaves

The old server keeps a read-only copy, like `data-web-1`, under **Elsewhere**. It serves nothing
and never refreshes.

- Its × deletes it right away, after you click **Delete**. This can't be undone.
- Restoring its last point into a new volume is turned off in this version. See
  [Restore a volume](restore-a-volume.md).

A new mirror deletes old copies on other servers first, and Details says so.

`ployz volume release`, `restore`, `mirror`, `mirror rm` and `sync --full` each start a run
in Ployz Cloud and print its id. Add `--wait` to wait for it to finish. A volume takes one run at a
time, and none while a deploy runs in its environment. `ployz volume runs data` lists what ran on
the volume and how it ended. `ployz volume release data` stops a mirror that's still running.

In a [Protected environment](../environments/environments.md#protect-an-environment), a
restore, release, mirror removal or resize from a member, or from a token or an agent without
`environments:protect`, waits for an owner or admin to approve it. `ployz` waits for the approval.
Adding a mirror and syncing don't ask.

If a run stops partway or a command is refused, see
[Troubleshooting volumes](../troubleshooting/volumes.md).

## Good to know

- **Old copies from earlier moves stay.** A copy a move left before this version is still under
  **Elsewhere**, and you can restore from it or delete it.
