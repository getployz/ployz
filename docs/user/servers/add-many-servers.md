---
title: Add many servers
description: Install once per server, review the waiting servers, then enroll them together.
---

Use one Fleet Token to prepare several Linux servers and review them before they join.
Each server needs the [server requirements and firewall ports](add-a-server.md#what-you-need).
Keep the token secret. Anyone with it can register servers for you to review.

## Enroll on arrival

To join servers automatically, turn on **Enroll on arrival** in **Add servers** and set
**Maximum servers** to a number from 1 to 1000. The CLI equivalent is
`ployz server token create --auto --max 3 --expires 7d`.
Once the organization has a server, anyone with this token can join servers up to that limit
until it expires or you revoke it.

Enroll on arrival only adds servers to an organization that already has one. In a new
organization, enroll the first server yourself. Until then the install ends with
`No cluster yet. Enroll one server in Ployz Cloud to start it.` and `ployz status` on the server
shows `No cluster yet`. Ready servers then join on their next check-in. Blocked servers and servers
still installing Docker or building ZFS keep waiting. Ployz keeps the hostnames, numbering any that
another server already has, and enables
Services, Web traffic, and Builds. It uses ZFS with a spare disk when available, or the main
disk otherwise. Servers where ZFS cannot run, or its preparation failed, was skipped, or was
turned off, use Docker volumes.

Open **Tokens** to see **Auto up to N** and **x/N joined**. Open enrollments also reserve
capacity. Failed or cancelled enrollments free their capacity for another server. Each server
enrolls automatically at most once per token, and an enrollment you start under the token
counts as that once. After a failed or cancelled enrollment the server stays in Waiting until
you enroll it again. Ployz remembers this by the server's own key, so a server that left Waiting
and later checks in with the same token again still waits for you.
That includes a server you removed or forgot: once Ployz Cloud no longer lists it,
`sudo ployz server checkin --install` on the reset server puts it back in Waiting.
Successful joins count for the token's lifetime, including after their waiting rows age out
after seven days or a joined server is removed. Servers beyond the limit stay in Waiting and
can be enrolled manually. Manual enrolls of its servers use its capacity too and can take the
count past N. Revoke stops new arrivals while open enrollments finish.

## Prepare your servers

1. Open **Servers** and click **Add servers**. Name the token, choose an expiry of 1 day, 7 days
   or 30 days, and click **Create**.
2. Copy the install line and the token while they are visible. Ployz shows the token once. The
   **cloud-init** tab carries the same token in the line.
3. Run the **Shell** line on each server as root or with `sudo`, and paste the token when it asks.
   For user data, use the cloud-init example instead; it has no terminal to ask at.
   The line returns once check-in is running.
4. Return to **Servers**. Servers appear under **Waiting to join** in arrival order. **Installing Docker**
   shows preparation. **Ready** means Docker is installed. Where the
   server builds its own ZFS module, **Building ZFS** shows under **Ready** until the build finishes.
   An enrollment with ZFS volumes, and Enroll on arrival, wait for that build.

Preparation installs Docker before you enroll. It builds ZFS ahead where needed, without
creating a pool. A blocked server stays in Waiting with the reason. Fix the server and its next
check-in refreshes the row. Offline servers have stopped checking in.

## Enroll the selection

1. Select the servers you recognize by name, source IP and provider, then click **Enroll**.
   You can select servers while they prepare. Blocked and Offline servers cannot be selected. To
   enroll one server, click its name and then **Enroll** in its details.
2. In a new organization, choose the **First server**. Everything else joins once it is up.
3. Choose **Volumes**. ZFS gives volumes size limits. ZFS uses the main disk. You can fall back
   to Docker where ZFS cannot run. **Docker only** has no managed volume size limits.
4. Keep **Hostnames**, or choose **Pattern** under **Names** and enter `web-{n}` to get
   `web-1`, `web-2`, and so on. A hostname becomes a valid server name: lowercase, with each
   run of other characters turned into one hyphen, so `MY-SERVER` becomes `my-server` and
   `my_box.lan` becomes `my-box-lan`. A hostname with no a-z letters or digits gets a name such as
   `server-0f1e2d3c`. A name another server already has gets the next free number: a second
   `my-server` becomes `my-server-2`, and `web-{n}` skips the numbers already taken. Choose
   whether the servers run **Services**, **Web traffic**,
   and **Builds**. Review the names, storage choices and any **Skip** reasons, then click
   **Enroll N**. For a selection across providers or networks, first check that every source
   belongs to you.

**Update ployz** as a skip reason means that server's `ployz` is too old to enroll. Copy a
current install line from **Add servers** or `ployz server token create`, run it on the server,
then enroll it. An older line installs the release it names.

When no server already in the organization has contacted Ployz Cloud for three hours, the sheet
says **Servers offline**. A running server contacts Cloud at least once an hour. Cloud can't join
servers to ones it can't reach, so the enroll fails. If those servers were deleted, follow
**Forget Servers** to forget them first, then enroll. Servers that went down less than three hours
ago don't raise the warning yet. The enroll then fails with the same advice.

## Use spare disk

The Enroll sheet puts ZFS on the main disk. Enroll on arrival and `ployz server enroll` use an
empty spare disk when the server has one; see
[Servers waiting to join](manage-servers.md#servers-waiting-to-join) for `--no-spare-disk`.

## Follow progress

The Enrolling list keeps each row in place. **Queued** means that server is still preparing.
It changes to **Installing** in the same row when preparation finishes. **Waiting on first**
means the first server has not finished. **In line** means it is waiting for a join slot;
when Cloud knows the position, the detail shows how many servers are ahead.

The first server joins, then the others join up to eight at a time. An add that stops before its
server starts to join, for example one the CLI refuses because the server still participates,
gives its slot back as soon as the CLI reports the failure. With `DO_NOT_TRACK=1` the CLI
sends no report, so the slot frees 10 minutes after the add last used it. One case can let a ninth through: rerunning
an add's token while that server is still joining, when the rerun stops before it joins. The
server already joining still finishes. The header counts how many of
the attempts have joined. Every row draws six progress segments, including Docker-only servers.
**Joined** rows stay in the list for one hour. Older attempts leave the polling view. The Servers below it show what Cloud observes from your
running servers. A new server takes no apps until you deploy.

**Cancel** on a row ends that attempt and revokes its enrollment token. Once Cloud has sent the
server its join, Cancel refuses with `web-1 may already be in the Cluster: Cloud sent its join.
Nothing cancelled.` and the attempt runs on. Once the server has joined, remove it with
`ployz server rm web-1`. A failed or cancelled
server returns to the bottom of Waiting. The live server status stays primary, with **Last enroll failed** or
**Last enroll cancelled** below it. Open the server to see how its last attempt ended (**Failed at**
a stage, **Failed**, or **Cancelled**) and its reason, when there is one. Both stay until you enroll the server again, however often it checks in. Fix the cause, then enroll the server again. Reloading restores the list and its order.

**Reject** turns away an unrecognized server. The same installation cannot come back with the
same Fleet Token. Reimaging a server creates a new identity, so review arrivals even when you
have rejected that hardware before. Once Cloud has sent a server its join, or heard it report
that it is joining, rejecting it no longer stops the join. The server stays in the Enrolling list
like any other row, up to an hour after it ends. Remove it with `ployz server rm` once it has
joined. Revoke a leaked Fleet Token from **Tokens**. This drops
its waiting servers; servers that already joined stay.
