---
title: Add many servers
description: Install once per host, review the waiting servers, then enroll them together.
---

Use one Fleet Token to prepare several Linux servers and review them before they join.
Each host needs the [server requirements and firewall ports](add-a-server.md#what-you-need).
Keep the token secret. Anyone with it can register hosts for you to review.

## Enroll on arrival

To join hosts automatically, turn on **Enroll on arrival** in **Add servers** and set
**Maximum servers** to a number from 1 to 1000. The CLI equivalent is
`ployz server token create --auto --max 3 --expires 7d`.
Anyone with this token can join servers up to that limit until it expires or you revoke it.

Arrival enrollment only joins an existing cluster. In a new organization, enroll the first
server yourself. Until then the install ends with
`No cluster yet. Enroll one server in Ployz Cloud to start it.` and `ployz status` on the host
shows `No cluster yet`. Ready hosts then join on their next check-in. Blocked hosts and hosts still
installing Docker or building ZFS keep waiting. Ployz keeps the hostnames and enables
Services, Web traffic, and Builds. It uses ZFS with a spare disk when available, or the main
disk otherwise. Hosts where ZFS cannot run, or its preparation failed, was skipped, or was
turned off, use Docker volumes.

Open **Tokens** to see **Auto up to N** and **x/N joined**. Open enrollments also reserve
capacity. Failed or cancelled enrollments free their capacity for another host. Each host
enrolls automatically at most once per token, and an enrollment you start under the token
counts as that once. After a failed or cancelled enrollment the host stays in Waiting until
you enroll it again. Ployz remembers this by the host's own key, so a host that left Waiting
and later checks in with the same token again still waits for you.
That includes a server you removed or forgot: once Ployz Cloud no longer lists it,
`sudo ployz server checkin --install` on the reset host puts it back in Waiting.
Successful joins count for the token's lifetime, including after their waiting rows age out
after seven days or a joined server is removed. Hosts beyond the limit stay in Waiting and
can be enrolled manually. Manual enrolls of its hosts use its capacity too and can take the
count past N. Revoke stops new arrivals while open enrollments finish.

## Prepare your hosts

1. Open **Servers** and click **Add servers**.
2. Name the token and choose an expiry of 1 day, 7 days or 30 days. Click **Create**.
3. Copy the install line and the token while they are visible. Ployz shows the token once. The
   **cloud-init** and **Terraform** tabs carry the same token in the line.
4. Run the **Shell** line on each host as root or with `sudo`, and paste the token when it asks.
   For user data, use the cloud-init or Terraform example instead; it has no terminal to ask at.
   The line returns once check-in is running.
5. Return to **Servers**. Hosts appear under **Waiting to join** in arrival order. **Installing Docker**
   shows preparation. **Ready** means Docker is installed. Where the
   host builds its own ZFS module, **Building ZFS** shows under **Ready** until the build finishes.
   An enrollment with ZFS volumes, and Enroll on arrival, wait for that build.

Preparation installs Docker before you enroll. It builds ZFS ahead where needed, without
creating a pool. A blocked host stays in Waiting with the reason. Fix the host and its next
check-in refreshes the row. Offline hosts have stopped checking in.

## Enroll the selection

1. Select the hosts you recognize by name, source IP and provider, then click **Enroll**.
   You can select hosts while they prepare. Blocked and Offline hosts cannot be selected. To
   enroll one host, click its name and then **Enroll** in its details.
2. For a new cluster, choose the **First server**. Everything else joins once it is up.
3. Choose **Volumes**. ZFS gives volumes size limits. ZFS uses the main disk. You can fall back
   to Docker where ZFS cannot run. **Docker only** has no managed volume size limits.
4. Keep **Hostnames**, or choose **Pattern** under **Names** and enter `web-{n}` to get
   `web-1`, `web-2`, and so on. Choose whether the servers run **Services**, **Web traffic**,
   and **Builds**.
5. Review the names, storage choices and any **Skip** reasons. For a selection across
   providers or networks, check that every source belongs to you. Click **Enroll N**.

When no server already in the organization has contacted Ployz Cloud for three hours, the sheet
says **Servers offline**. A running server contacts Cloud at least once an hour. Cloud can't join
hosts to a cluster it can't reach, so the enroll fails. If those servers were deleted, follow
**Forget Servers** to forget them first, then enroll. Servers that went down less than three hours
ago don't raise the warning yet. The enroll then fails with the same advice.

## Use spare disk

The Enroll sheet currently uses ZFS on the main disk. Hosts that check in do not yet
advertise support for creating a pool on a named spare disk, so the sheet does not offer
that choice. An empty spare disk in the report alone does not prove that support.

## Follow progress

The Enrolling list keeps each row in place. **Queued** means that host is still preparing.
It changes to **Installing** in the same row when preparation finishes. **Waiting on first**
means the first server has not finished. **In line** means it is waiting for a join slot;
when Cloud knows the position, the detail shows how many servers are ahead.

The first server joins, then the others join up to eight at a time. The header counts how many of
the attempts have joined. Every row draws six progress segments, including Docker-only servers.
**Joined** rows stay in the list for one hour. Older attempts leave the polling view. The Servers below it show what Cloud observes from your
running cluster. A new server takes no apps until you deploy.

**Cancel** on a row ends that attempt and revokes its enrollment token. A failed or cancelled
host returns to the bottom of Waiting. The live host status stays primary, with **Last enroll failed** or
**Last enroll cancelled** below it. Open the host to see how its last attempt ended (**Failed at**
a stage, **Failed**, or **Cancelled**) and its reason, when there is one. Both stay until you enroll the host again, however often it checks in. Fix the cause, then enroll the host again. Reloading restores the list and its order.

**Reject** turns away an unrecognized host. The same installation cannot come back with the
same Fleet Token. Reimaging a host creates a new identity, so review arrivals even when you
have rejected that hardware before. Once Cloud has heard a host report that it is joining,
rejecting it no longer stops the join. The host stays in the Enrolling list like any other row,
up to an hour after it ends.
A reject that lands after the server commits to joining but before Cloud hears so still cancels
it. Revoke a leaked Fleet Token from **Tokens**. This drops
its waiting hosts; servers that already joined stay.
