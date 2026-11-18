---
title: Restore a volume
description: Make a copy the volume again when its server is gone. Restoring a point into a new volume is turned off in this version.
---

A volume's [mirror](mirror-a-volume.md) holds the volume's files as of its latest refresh, taken on
each deploy and every 15 minutes. An old copy keeps the last point it held.

## Restoring a point is turned off in this version

In this version of Ployz you can't restore a point into a new volume. Under **Copies**, a point
says "Restoring a point into a new volume is turned off in this version of Ployz." in place of
**Restore**. A server puts one volume's data under another name only for a deploy that a device
signed for it, and Ployz Cloud has no such signature to give yet.

To go back to a mirror's last point, [restore the volume in place](#restore-a-volume-in-place).
To keep the current data too, dump it first. See
[Back up a database](databases.md#back-up-a-database).

## Restore a volume in place

When the server that wrote a volume is gone, make a copy on another server the volume again. In
Ployz Cloud, **Use mirror** does this for a mirror. See
[When its server goes down](volumes.md#when-its-server-goes-down). For any copy, run:

```sh
ployz volume restore data --from web-2
```

Restore needs `web-2` to hold the only copy of the volume. If other servers hold copies, it is
refused and names them. Remove them with `ployz volume mirror rm`, or remove their server, first.
A mirror holds the data as of its last refresh, so restoring one loses the writes after it. The
run says when that was: `data restored on web-2 from <time>; writes after that time are lost`.

Restore doesn't start your service, so deploy it next. The [agent](../account/agent-sidebar.md)
can run the same restore. In a
[Protected environment](../environments/environments.md#protect-an-environment), a restore from a
member, or from a token or an agent without `environments:protect`, waits for an owner or admin to
approve it.

Without Ployz Cloud, the CLI runs the restore itself, with the same checks. It refuses while any
server doesn't answer, and while another server holds a copy. Without Cloud, `ployz server rm` is
the way to drop another copy. It prints
`Restored data on web-2 from <time>; writes after that time are lost.` and exits once the copy is
the writer. Run again, it says the writer is already there. See
[When a volume has no writer](volumes.md#when-a-volume-has-no-writer).

## Good to know

- **A point is volume data, not a database point in time.** It holds the files as the mirror last
  copied them. A database may need to recover on first start, as after a power cut. For a clean
  dump, see [Back up a database](databases.md#back-up-a-database).
- **Points stay on your servers.** A mirror isn't an offsite backup, and S3 backups are not here
  yet. For a copy off your servers, see [Back up a database](databases.md#back-up-a-database).
