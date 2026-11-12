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

## Mirror on servers that enforce device approval

With [device enforcement](../servers/manage-servers.md#require-a-devices-signature-for-restarts-and-joins)
on, a server copies a volume only to a server an approved device chose. Run
`ployz volume mirror data --to web-2` on an approved device. It signs the mirror, prints what it
approved and starts it without asking:

```
Approved: mirror data from web-1 to web-2. Later syncs run without asking until you remove the Mirror.
```

web-2 then pulls `data` only from web-1, and web-1 sends `data` only to web-2. Refreshes after a deploy, the 15-minute refresh, **Refresh now** and
`ployz volume sync data` all run without another approval.

`ployz volume sync data --full` deletes the mirror's copy and copies everything again, so it signs
a new approval the same way. Removing the mirror ends its approval for good. To mirror to that
server again, run `ployz volume mirror` again. A mirror takes up to 128 approvals. After that,
remove it and mirror again.

Ployz Cloud can't add a mirror on these servers yet. Adding one under **Copies** fails before it
changes anything. A mirror added before you turned enforcement on has no approval, so its
refreshes fail. Run `ployz volume sync data --full` on an approved device to approve it.

To delete a mirror, run `ployz volume mirror rm data-web-2` on an approved device. It signs the
removal, prints it and asks first:

```
Approved: delete data's mirror on web-2. web-1 keeps writing data.
Approve deleting data-web-2?
```

Add `--yes` to sign without asking. With no terminal to ask in and no `--yes`, it signs nothing
and deletes nothing. When no server writes the volume, the copy may be its last, so it asks for
`--confirm data` before it signs, and the line says so:

```
Approved: delete data's old copy on web-2. No Server writes data, so this may be its last copy, and its data is gone for good.
```

The run then says which servers recorded the removal, like
`Deleted data-web-2; web-2 and web-1 recorded the removal`. If web-1 didn't answer, it says web-1
has not yet. web-2 refuses the mirror's old approval either way. A copy that holds no data yet,
from a mirror whose first copy never finished, is deleted without an approval.

Ployz Cloud can't delete a copy that holds data on these servers on its own. **Remove** under
**Copies**, and a deploy that drops a mirror or an old copy, fail before they change anything and
name the command to run from an approved device. When you delete a volume, Ployz Cloud leaves its
mirrors' copies on servers that enforce. Turn enforcement off with `ployz device enforce --off` to
let Ployz Cloud clean them up, then turn it back on.

If `ployz volume mirror` can't reach your servers to ask whether they enforce, it warns and runs
without signing, and servers that enforce refuse it. A server from before Ployz 0.4 ignores the
approval and copies as before.

## Good to know

- **A mirror isn't an offsite backup.** It holds the data as of its last refresh, on another of
  your servers. For a copy off your servers, see [Back up a database](databases.md#back-up-a-database).
- **A mirror that fell behind for good gets a full copy.** When the volume's server no longer
  holds any point the mirror has, the next refresh or move sends the whole volume.
- **Deleting a volume deletes its mirror.** So does deleting its environment.
