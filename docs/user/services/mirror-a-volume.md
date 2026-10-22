---
title: Mirror a volume
description: Keep a read-only copy of a volume on a second server.
---

A mirror is a read-only copy of a volume on a second server. Ployz Cloud refreshes it on every
deploy and every 15 minutes. It keeps a backup of your data on your own servers, lets you
[move the volume](move-a-volume.md) there, and gives the volume a
[restore point](restore-a-volume.md).

A volume has one mirror for now. Every server with storage needs a newer Ployz before a volume
can have one. Until then the volume's **Copies** only shows, and names the servers to upgrade,
for example "Upgrade db-1 to a newer Ployz". The 15-minute refresh waits too.

## Add a mirror

1. Open a managed volume after its first deploy and go to **Copies**.
2. Under **Elsewhere**, click **Add copy** and choose a server. It offers only servers that run
   services.
3. Click **Deploy**.

<!-- screenshot: a volume's Copies with a mirror under Elsewhere -->

Until you deploy, the new mirror shows in green and counts in the bottom bar and **Details**,
where you can discard it. Details reads "Seed" for it. The deploy copies the volume before it
starts your services.

## Check on a mirror

**Copies** lists where the volume's data lives. **Live** is the server your service writes on.
**Elsewhere** holds the mirror and any old copy a move left, each with when it last refreshed.

On the canvas the mirror hangs under its volume's tray as its own small card, for example
`data-web-2` with "Mirror · 4 minutes ago". A mirror more than an hour behind shows its age in
amber with a warning sign. A staged mirror shows **New** in green, a staged removal shows
**Removing** in red, and a mirror about to become the primary shows **Primary** in pink.

Click a mirror, on the canvas or under **Elsewhere**, to open it. It shows its server, when it last
refreshed, **Refresh now**, **Make primary** and **Remove mirror**. A mirror more than an hour
behind says **Refresh skipped** and why. Either its server is not answering or its last refresh
failed. It also says how much of its writes a failover would lose.

A mirror stays listed while its server is not answering. A mirror made with `ployz volume mirror`
is then known only from that server's last report, so its row reads "Last reported", with no age.
It may have been removed since. Copies can't make it primary, remove it or refresh it until its
server answers. If Copies knows of no copy outside `web-1` while another server hasn't reported,
**Elsewhere** says why, for example "web-2 is not answering, so Copies can't say what is outside
web-1".

## Refresh a mirror

Click **Refresh now** on the mirror. It sends the mirror what changed since its last refresh and
answers at once, for example "data-web-2 is pulling w-3-1760340000". The mirror reads
"Refreshing" while it downloads, and its age resets when the download lands.

If a move or release is running on the volume, **Refresh now** says so instead, since a move's
own copy refreshes the mirror. A download that fails is reported once, with its server's reason,
and the mirror shows the failure until a later refresh lands.

## Remove a mirror

1. Click the mirror and click **Remove mirror**. It shows in red until you deploy.
2. Click **Deploy**. Type your `project/environment`, like `my-app/production`, and click
   **Deploy**. The deploy asks because the mirror's data goes with it.

Details reads "Delete" for the removal. A staged mirror has no **Make primary** or **Remove
mirror**, and a new one has no **Refresh now**. To undo a staged change, discard it in Details.
See [Confirm a deploy that deletes data](../deploy/deployments.md#confirm-a-deploy-that-deletes-data).

## Mirror from the CLI

The CLI runs each of these at once, without waiting for a deploy:

```sh
ployz volume mirror data --to web-2     # copy data to web-2; it shows as data-web-2
ployz volume sync data                   # send the mirror what changed since its last refresh
ployz volume sync data --full            # copy everything again
ployz volume mirror rm data-web-2        # delete the mirror
```

`ployz volume sync` without `--full` works like **Refresh now** and prints what it did. If a
server doesn't answer in time, the sync says the server may still have taken it. Run
`ployz volume sync data` again to see where each mirror got to. Copying a big volume, for a mirror
or a move, can take hours. If a copy hasn't finished after six hours, the run fails and says so.

`ployz volume mirror rm` deletes the copy on that server, whether it's the mirror or an old copy.
Add `--if-old-copy` to delete it only while it is still an old copy. Every cleanup line Ployz
prints for an old copy includes it. A mirror that is the volume's only copy left is removed only
with `--confirm data`, because its data goes with it.

In a [Protected environment](../environments/environments.md#protect-an-environment),
`ployz volume mirror rm` from a member, or from a token or an agent without
`environments:protect`, waits for an owner or admin to approve it. Adding a mirror and syncing
don't ask.

## Good to know

- **A mirror isn't an offsite backup.** It holds the data as of its last refresh, on another of
  your servers. For a copy off your servers, see [Back up a database](databases.md#back-up-a-database).
- **A mirror that fell behind for good gets a full copy.** When the volume's server no longer
  holds any point the mirror has, the next refresh or move sends the whole volume.
- **Deleting a volume deletes its mirror.** So does deleting its environment.
