---
title: Add many servers
description: Install once per host, review the waiting servers, then enroll them together.
---

Use one Fleet Token to prepare several Linux servers and review them before they join.
Each host needs the [server requirements and firewall ports](add-a-server.md#what-you-need).
Keep the token secret. Anyone with it can register hosts for you to review.

## Prepare your hosts

1. Open **Servers** and click **Add servers**.
2. Name the token and choose an expiry of 1 day, 7 days or 30 days. Click **Create**.
3. Copy the install line while it is visible. The **Shell**, **cloud-init** and **Terraform**
   tabs use the same token. Ployz shows its secret once.
4. Run the line on each host as root or with `sudo`. For user data, use the cloud-init or
   Terraform example instead. The line returns once check-in is running.
5. Return to **Servers**. Hosts appear under **Waiting** in arrival order. **Installing Docker**
   and **Building ZFS** show preparation. **Ready** means preparation has finished.

Preparation installs Docker before you enroll. It builds ZFS ahead where needed, without
creating a pool. A blocked host stays in Waiting with the reason. Fix the host and its next
check-in refreshes the row. Offline hosts have stopped checking in.

## Enroll the selection

1. Select the hosts you recognize by name, source IP and provider, then click **Enroll**.
   You can select hosts while they prepare. Blocked and Offline hosts cannot be selected.
   With just one waiting host in a new organization, the Enroll sheet opens automatically.
2. For a new cluster, choose the **First server**. Everything else joins once it is up.
3. Choose **Volumes**. ZFS gives volumes size limits. You can use a spare disk and fall back
   to Docker where ZFS cannot run. **Docker only** has no managed volume size limits.
4. Keep **Hostnames**, or choose **Pattern** under **Names** and enter `web-{n}` to get
   `web-1`, `web-2`, and so on. Choose whether the servers run **Services**, **Web traffic**,
   and **Builds**.
5. Review the names, storage choices and any **Skip** reasons. For a selection across
   providers or networks, check that every source belongs to you. Click **Enroll N**.

## Follow progress

The Enrolling list keeps each row in place. **Queued** means that host is still preparing.
It changes to **Installing** in the same row when preparation finishes. **Waiting on first**
means the first server has not finished. **In line** means it is waiting for a join slot;
when Cloud knows the position, the detail shows how many servers are ahead.

The first server joins, then the others join up to eight at a time. **Joining N / 8** includes
servers going live. Every row draws six progress segments, including Docker-only servers.
**Joined** rows stay in the list. The Servers below it show what Cloud observes from your
running cluster. A new server takes no apps until you deploy.

**Cancel** on a row ends that attempt and revokes its enrollment token. A failed or cancelled
host returns to the bottom of Waiting. Failed rows show the stage and reason. Fix the cause,
then select and enroll the host again. Reloading restores the list and its order.

**Reject** turns away an unrecognized host. The same installation cannot come back with the
same Fleet Token. Reimaging a host creates a new identity, so review arrivals even when you
have rejected that hardware before. Revoke a leaked Fleet Token from **Tokens**. This drops
its waiting hosts; servers that already joined stay.
