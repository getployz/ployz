---
title: Restore a volume
description: Bring a volume back to an earlier point as a new volume, or make a copy the volume when its server is gone.
---

A volume's [mirror](mirror-a-volume.md) gives it a restore point: the mirror's latest refresh,
taken on each deploy and every 15 minutes. An old copy keeps the last point it held. Restoring a
point makes a new volume with that point's files and moves your service onto it. The original
volume stays until you delete it.

Restore works on managed volumes in Ployz Cloud. Every server with storage needs a newer Ployz
first. Until then the points list without **Restore**. A volume with no mirror and no old copy has no points. Add a mirror
with **Add copy** under the volume's **Copies**. See [Mirror a volume](mirror-a-volume.md).

## In Ployz Cloud

Open the volume and click its mirror under **Copies**. **Restore point** shows when the mirror's
latest point was taken. A mirror keeps only its latest point. An old copy shows **Restore** on its
row under **Elsewhere**.

**Restore** on a point stages the restore. It counts in the bottom bar and Details like any other
change. Nothing runs on click. Details shows the new volume, named after the original and the
point's time, for example `pg-data-1027`, and says in red what you give up: "Changes after 10:27
are not in pg-data-1027". To drop the restore, discard it in Details.

Copies, Details, the **Data loss** confirm and the approval card give a point's time in your own
time zone. A point from an earlier day shows its date too, like "8 Oct, 10:27". Copies, Details,
the **Data loss** confirm and the approval card add the date within a minute of midnight if they're
still open.

When an agent deploys a restore, its approval card says the same under the detach, like
**Detaches volume pg-data from postgres**, then "Changes after 10:27 are not in pg-data-1027".
When `ployz` asks you to approve the deploy in a terminal, it prints the same
line under the detach, with the full date and time in your computer's time zone, like
`Changes after 2026-10-08T10:27:00+02:00 are not in pg-data-1027`. Approving accepts that loss for
this restore point only.

Deploy runs it before your services. Ployz copies the point to the original's server, makes it the
new volume, and starts your service on it. The original keeps its data and its mirror; it shows as
not mounted. Delete it from its **Settings** tab when you no longer need it.

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

## Good to know

- **A point is volume data, not a database point in time.** It holds the files as the mirror last
  copied them. A database may need to recover on first start, as after a power cut. For a clean
  dump, see [Back up a database](databases.md#back-up-a-database).
- **One restore per volume at a time.** Restore is greyed out while the volume's placement deploys.
- **A copy being deleted has no point to restore.** Restore on it says "This copy is being deleted."
- **The new volume needs its own room.** It has the original's size limit. When the original's
  server cannot fit a second copy, the deploy fails and says how much space is missing.
- **Every server must answer to stage a restore.** While one is offline, or its report hasn't
  arrived, the points list without **Restore**. A restore staged just as a server stops answering
  is refused with "Your servers aren't answering. Try again once they are." The deploy would fail,
  since it can't tell what that server holds.
- **A restore the deploy would refuse isn't staged.** The point says why in place of **Restore**,
  like "Volume restore is not available on web-2." for a server that holds a copy but whose Ployz
  doesn't report it can take part, or "data has two writers, data-web-1 and data-web-2". Staging refuses the same way
  and names the server's version when Ployz knows it, like "Volume restore is not in Ployz
  0.3.0-beta.1 on web-2 yet.", or "Volume restore needs a newer Ployz on web-2. 0.3.1 is out." once
  a newer release is out.
- **An old primary that came back holds a restore back.** A server that held the volume before a
  move or **Use mirror** can answer again with its old copy. The point then says "data-web-1 is an
  old primary that came back; restore once a mirror refresh makes it a mirror" in place of
  **Restore**. **Refresh now** on the mirror, or the refresh every 15 minutes, makes that copy a
  mirror, and **Restore** returns.
- **The original's server must answer** for the copy to run. If the point's mirror is gone by then,
  the deploy fails and names the point.
- **Points stay on your servers.** A mirror isn't an offsite backup, and S3 backups are not here
  yet. For a copy off your servers, see [Back up a database](databases.md#back-up-a-database).
