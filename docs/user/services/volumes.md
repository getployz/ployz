---
title: Volumes
description: Keep files across deploys and restarts.
---

A deploy can replace your service's containers. A volume is a folder that survives:
whatever your service writes there is still there after deploys and restarts. Use one for
uploads, a SQLite file or a database's data. [Databases](databases.md) come with one.

Think of a volume as a disk plugged into one of your servers: every service that mounts it
runs on that server.

## Add a volume

1. Open your environment and click **Create**, then **Volume**. Right-clicking the canvas
   works too.
2. Enter a **Name**, like `uploads`, and a **Storage limit (GB)**. Names use lowercase letters,
   digits and hyphens. Anything else is converted, and the field shows the name it saves as. The
   storage limit is [fixed once you deploy](#choose-the-storage-limit), so leave room to grow.
   Click **Create volume**.
3. Click the new volume, then **Mount on a service**. Pick the service, enter a **Directory** inside
   its containers, like `/app/uploads`, and click **Mount**.
4. Click **Deploy**.

![A volume's panel: its storage limit, Advanced, and where it is mounted](../images/volume-panel.png)

The volume shows as a tray under the service's card, and your app reads and writes files at
that directory. Open the menu at the end of the mount's row and choose **Edit directory** to move it.
Manage mounts from the volume's panel. Mount forms appear when you choose an action, and keep
your entered directory if a write fails.

## Choose the storage limit

The limit starts at 5 GB. A volume can't grow past it: once it's full, writes fail. The whole
limit is set aside on the server's disk, so a deploy fails if no server has room for it.

You can change the limit under **Storage** in the volume's panel until you deploy the volume.
If that first deploy fails, the limit stays fixed in the panel. Once the volume deploys, you can
grow it on Ployz Cloud while its service keeps running. Type a larger **Storage limit** in the
panel and confirm **Grow volume**. The panel shows the resize until it ends. Before you confirm,
it refuses a smaller limit, a server whose Ployz is too old to grow it or that isn't reporting to
Cloud, and a size a server reports no room for. You can also grow it from the CLI:

```sh
# Grow the data volume to 20 GB on every server that holds a copy
ployz volume set data --size 20 --wait
```

Ployz grows each mirror first and the copy your service writes to last. If one server holding
a copy has no room, Ployz refuses the whole resize, names that server, and the volume keeps its
old limit. `ployz volume runs data` shows how a resize went. The new limit isn't a staged
change: `ployz diff` shows nothing, and your next deploy leaves the service's container alone.
A Docker volume has no limit to grow. A version saved before a resize, grow or shrink, deploys
at the size the volume holds now.

To shrink a deployed volume, run the same command with a smaller number:

```sh
# Shrink the data volume to 8 GB on every server that holds a copy
ployz volume set data --size 8 --wait
```

Ployz shrinks the copy your service writes to first, then each mirror. A volume can't shrink
below what it holds plus some headroom: 10% or 1 GB, whichever is larger. If a server refuses,
Ployz names it and the smallest size it accepts, rounded up to a whole GB, and stops. If the copy
your service writes to refuses, the volume keeps its old limit. If a mirror refuses after that
copy shrank, the volume has its new limit: free space in it, run `ployz volume sync data` so the
mirror's copy follows, then run the same `ployz volume set` again. A shrunk volume has less free
space, so it may fill up sooner.

Running `ployz volume set data --size 8` with the size the volume already has asks every server
to hold that size. That is how to finish a shrink a mirror refused or one that was interrupted:
a copy still larger is shrunk to it. It only ever lowers a copy. If a server holds less than the
volume's limit, Ployz refuses and tells you to run `ployz volume set` with the size that server
holds, then again with the size you want, to grow it. If Ployz has no fresh reading of a server's copy,
it refuses and asks you to try again in a moment.

The dashboard doesn't show how full a volume is yet:

```sh
# CLI-only for now: how full is web's volume?
ployz exec web -- df -h /app/uploads
```

## Share a volume

By default, only one container writes to a volume. A service with a volume runs one replica, and no
other service can mount it. To run more replicas, or to mount it in several services, turn on
shared writes in the volume's **Storage** → **Advanced** → **Allow shared writes**, or use the CLI:

```sh
ployz volume set uploads --shared-writes
```

Do this only if your app copes with several writers at once; a database's data directory
doesn't. Everything that mounts the volume still runs on its server.

## When its server goes down

The services that mount the volume stop until the server is back. Nothing moves to another
server on its own, and the CLI can't [move](move-a-volume.md) a volume while its server is down.
With a mirror, **Use mirror** under the volume's **Copies** moves it there at your next deploy,
losing the writes since the mirror's last refresh. To keep deploying your other services meanwhile,
see [When a server goes down](scaling.md#when-a-server-goes-down).

> [!WARNING]
> A volume lives on one server. Add a [mirror](move-a-volume.md#mirror-a-volume) to keep a backup
> of it on another of your servers. A mirror holds the data as of its last refresh, nothing
> newer, and Ployz doesn't switch to it on its own. A snapshot isn't a backup: it stays on the
> volume's own server and is lost with it.
>
> A mirror isn't an offsite backup either. Your servers often share one provider and one account.
> For a copy that survives losing them, see [Back up a database](databases.md#back-up-a-database).

## Copies

A volume's copies are the places its data lives besides the server that writes it. With Ployz
Cloud a volume can have a mirror, a read-only copy on a second server that refreshes on every
deploy and every 15 minutes. A move leaves an old copy on the server it left. Open the volume and
look under **Copies**. An old copy never refreshes, so it doesn't protect the data. Each server
records which of its copies is a mirror and which is an old copy, so a mirror made with
`ployz volume mirror` refreshes like any other, and a move from the CLI leaves an old copy too. A
mirror's age turns amber with a **⚠** once it is more than an hour old.

Copy changes, like a new mirror or a restore, go out with your next deploy. **Redeploy** on a past
deployment ships what that deployment froze, so it leaves them staged and says **Copies wait for
Deploy**. A restore that failed is taken back, and that deployment can't be redeployed. Restore
again instead.

A restore, or discarding one, is refused with "A Deploy or restore is starting. Try again." while
a deploy in the same environment is starting. Once a deploy has taken a restore, you can't discard
the restore until that deploy ends.

## Move a volume

With Ployz Cloud, **Make primary** on a mirror or `ployz volume move data --to web-2` moves a
volume and the service that mounts it to another server, stopping the service only for the last
copy. See [Move a volume](move-a-volume.md).

## When a volume has no writer

Each volume has one writer, the server that mounts it. A server you remove or reinstall turns
every volume it held into a copy, so the volume keeps its data but has no writer. A deploy of a
service that mounts such a volume refuses and names where the data is:

```
Volume data has no writer; web-2 holds its only copy. Make it the writer: ployz volume restore data --from web-2
```

Run the `ployz volume restore` line it prints to make that copy the writer again, then deploy.
While one of your servers is offline, the deploy can't tell whether that server holds the writer,
so it names that server instead and suggests no restore. See
[When a server goes down](scaling.md#when-a-server-goes-down).
If several servers hold copies, keep the newest and remove the others with the `ployz volume mirror rm` lines it names. A volume
that is [mid-move](move-a-volume.md#when-a-move-stops) refuses too, and names the
`ployz volume release` line that stops the move. A move that stopped before its first snapshot
has nothing to release, so it names the `ployz volume move` line that runs the move again.

## When your servers show Docker only

A volume needs a server that shows **Managed volumes available** on the **Servers** page. A
server added with **Start without it** shows **Docker only** instead. If none of your servers
has managed volumes, deploying a volume fails, including the one a database gets.

[Add a server](../servers/add-a-server.md) with managed volumes, or, before you deploy, open the
volume and tick **Use a plain Docker volume (not recommended)** under **Advanced**. It has no
storage limit, so it can fill the server's disk.

## Detach a volume

In the volume's panel, open the menu at the end of a mount's row
and choose **Unmount**, then deploy. The detached row shows what the next deploy removes.
The volume and its data stay.

## Delete a volume

Deleting a deployed volume deletes its data from your servers for good.

1. Open the volume and click **Delete volume** under **Danger**. **Keep volume** undoes it.
2. Click **Delete data**, or **Deploy**.
3. Ployz shows **Deletes data** with what goes. Type your `project/environment`, like
   `my-app/production`, and click **Deploy**.

If that deploy fails, deploying again deletes the data without asking again, because your
confirmation stays with the change. See
[Confirm a deploy that deletes data](../deploy/deployments.md#confirm-a-deploy-that-deletes-data).

A volume that was never deployed holds no data, so it goes on your next deploy without asking.
A mounted volume whose first deploy is still running counts as deployed, since that deploy may
already have created it. Saving or deploying its deletion asks first, the same way.

## Good to know

- **A branch gets an empty volume.** A [branch](../environments/environments.md) that copies
  a volume starts with no data, not a copy of yours.
- **Deleting a service keeps its volumes.** The exception is a [database](databases.md)
  deleted from the dashboard, which takes its volume with it.
