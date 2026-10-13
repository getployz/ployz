---
title: Restore a volume
description: Bring a volume back to an earlier point, as a new volume, from its mirror or an old copy.
---

A volume with a mirror has restore points: the copies the mirror took on each deploy and every
15 minutes. An old copy keeps the last point it held. Restoring one makes a new volume with that point's files and moves your service onto
it. The original volume stays until you delete it.

Restore works on managed volumes in Ployz Cloud, once every server that can hold managed volumes
runs Ployz 0.2.5 or newer. Until then the points list without **Restore**. A volume with no mirror and no old copy has no points. Add a mirror
with **Add copy** under the volume's **Copies**. See [Move a volume](move-a-volume.md).

## In Ployz Cloud

Open the volume and click its mirror under **Copies**. **Restore point** shows when the mirror's
latest point was taken. A mirror keeps only its latest point. An old copy shows **Restore** on its
row under **Elsewhere**.

**Restore** on a point stages the restore. It counts in the bottom bar and Details like any other
change. Nothing runs on click. Details shows the new volume, named after the original and the
point's time, for example `pg-data-1027`, and says in red what you give up: "Changes after 10:27
are not in pg-data-1027". To drop the restore, discard it in Details.

Deploy runs it before your services. Ployz copies the point to the original's server, makes it the
new volume, and starts your service on it. The original keeps its data and its mirror; it shows as
not mounted. Delete it from its **Settings** tab when you no longer need it.

## Good to know

- **A point is volume data, not a database point in time.** It holds the files as the mirror last
  copied them. A database may need to recover on first start, as after a power cut. For a clean
  dump, see [Back up a database](databases.md#back-up-a-database).
- **One restore per volume at a time.** Restore is greyed out while the volume's placement deploys.
- **The new volume needs its own room.** It has the original's size limit. When the original's
  server cannot fit a second copy, the deploy fails and says how much space is missing.
- **The original's server must answer** for the copy to run. If the point's mirror is gone by then,
  the deploy fails and names the point.
- **S3 backups are not here yet.** Keep your own copies off your servers meanwhile.
