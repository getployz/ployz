---
title: Manage servers
description: Server status, what runs where, roles, upgrades, and removing a server.
---

The **Servers** page lists every server in your organization. Open one to see what runs on it,
turn builds on or off, or remove it.

## Check your servers

Each row shows a server's name, the services on it, what it can store, and its status. Above the
list, a summary reads **All 3 online**, or puts problems first, like **1 offline · 2 online**.

![The Servers page, listing each server, what runs on it and its status](../images/servers-page.png)

| Status | What it means |
| --- | --- |
| **Online** | The server is up and in touch with your other servers. |
| **Building** | Online, and running builds right now. |
| **Offline** | Your other servers can't reach it. See [A server shows Offline](../troubleshooting/servers.md#a-server-shows-offline). |
| **Unknown** | Ployz can't tell whether it's up. Check it as you would an offline one. |

**Managed volumes available** means the server can hold [managed volumes](../services/volumes.md).
**Docker only** means it was added with **Start without it** and holds plain Docker volumes only.

If the page shows **Can't reach your servers right now**, see
[Troubleshooting servers](../troubleshooting/servers.md#cant-reach-your-servers-right-now).

## See what runs on a server

Click a server. Its page shows its status and public IP address, then:

- **Running here**: click it to list the server's services. While the server is offline, each one
  says whether it's **Still on** another server or **Down**.
- **Services**: **Run services here** and **Drain**. See
  [Change what a server does](#change-what-a-server-does).
- **Builds**: **Run builds here** and **Builds at once**. See
  [Where builds run](../builds/where-builds-run.md#build-on-your-servers).

![A server's page: Running here, Builds and Remove server](../images/server-page.png)

An entry marked **Not in any Project** is left over from a deleted project or environment.
**Remove** deletes its containers and its volumes, data included, from every server.

## Change what a server does

Every server runs builds, runs services and takes web traffic. To turn one off, see
[Give servers different jobs](../services/scaling.md#give-servers-different-jobs).

Under **Services** on a server's page, turn off **Run services here** and nothing new starts on
the server. What already runs there stays until you drain it. Your next deploy also moves it.

To move everything off now, click **Drain** and confirm. The dialog lists what runs on the server,
and names apart what no project owns: draining leaves that where it is. Draining turns services off for the server and stops its global services there. Then it moves
each replicated service's containers to your other servers one at a time. Each new container runs
the same image, and each one starts and is healthy before the old container is removed.
Pre-deploy commands don't run again, and nothing is deployed. Drains in one organization run one
at a time: a second one reads **Waits for the drain on web-1 to finish**. You can leave the page
while one runs.

While a server drains, you can't turn **Run services here** back on for it: Cloud refuses the
change until the drain ends. Turning services on for another server waits until
the organization's drain ends. Other server settings, like **Run builds here**, never wait for a
drain.

A service stays running where it is, and is reported with the reason, when:

- it mounts a volume or a bind mount on the server (tmpfs mounts don't count);
- its containers run different versions, because a deploy hasn't finished: deploy it first;
- a server it involves can't be checked right now;
- no other server can take it.

When the drain ends, the **Drain** row counts what moved, stopped, stayed and failed, and how many
services no project owns it left alone. It lists each service with where it went or why it stayed.
A drain that stops early marks the service it was moving **Interrupted** and the ones it never
reached **Not attempted**; those still run on the server.
If anything stayed or failed, the drain stopped early, or it couldn't check what's left on the server, the button
reads **Drain again**. Turning services back on doesn't move anything back.

From the CLI, `ployz server drain web-2` does the same and prints the report. If anything stayed or
failed, it exits non-zero.

Turning web traffic off stops advertising the server, not serving from
it: your generated addresses stop pointing at it within the hour, or when you click **Check
again** under **Domain** in **Organization → General**. Until then, visitors still sent to it are
served from wherever your services run. [Removing the server](#remove-a-server) is what stops it
taking traffic.

## Upgrade Ployz on a server

Each server on the **Servers** page shows the Ployz release it runs. When a newer release is out on
your organization's channel (Stable unless you [choose Beta](#choose-stable-or-beta-releases)),
each server behind is tagged **→ `0.2.2`**, and the top of the page offers **Upgrade to `0.2.2`**.
Click it to upgrade every server that's behind, one at a time in name order. Your apps keep running
while they do. While an upgrade runs, the top of the page reads **Upgrading to `0.2.2` · 1 of 4**,
and the server upgrading is tagged **Upgrading**.

### Turn on automatic upgrades

Automatic upgrades are off until you turn them on. Click **Upgrades: Manual** at the top of the
**Servers** page (on a phone, its settings icon) and turn on **Upgrade automatically**. Any member can change it. Ployz starts
upgrading right away, then checks every hour, and the button then reads **Upgrades: Automatic**.
Turn the switch off to go back to upgrading by hand.

Servers that are offline or building are skipped and upgrade at a later check. An offline server
behind is tagged **→ `0.2.2` when back**, and its page reads **Upgrades to `0.2.2` when it's back**.

If a server's upgrade doesn't install, that server goes back to the release it ran before, and the
other servers wait: automatic upgrades skip that release until a newer one is out. That server is
tagged **Upgrade failed**, and the top of the page offers **Upgrade to `0.2.2`** to carry on yourself.
On the server's page, **Show details** has the exact error to copy, and **Try again** retries. Once an upgrade of that release succeeds, automatic upgrades pick it up again.

### Upgrade one server

To upgrade one server, open it: its **Ployz** section reads **`0.2.2` is out**. Click **Upgrade**.
Any member can upgrade servers while they're online and not building. One upgrade runs at a time
per organization, so **Upgrade** and **Try again** wait while one is running.

### Choose Stable or Beta releases

Click **Upgrades: Manual** (or **Upgrades: Automatic**) at the top of the **Servers** page and pick
**Releases: Stable** or **Beta**. Stable is the default; Beta gets fixes early, as prereleases like `0.2.3-beta.1`. Any
member can change it, and it applies to every upgrade, automatic or by hand. The **Servers** page
and each server's page then show the newest release on that channel.

Switching back to Stable never downgrades a server. A server on a beta ahead of Stable reads as
current and waits until Stable catches up.

### New major lines

Upgrades stay on your servers' release line (`0.x`). When a new line like 1.0 is published, the top
of the **Servers** page links **Ployz 1.0 is out** to its release notes. Nothing installs the new
line for you: your servers stay on 0.x and keep getting its upgrades.

### Upgrade from the CLI

To upgrade from the CLI, or to pick an exact version:

```sh
# stable, beta, or an exact version like 0.2.0, on web-1 then web-2
ployz server upgrade stable web-1 web-2
```

`ployz server upgrade` works the same way whether automatic upgrades are on or off: your apps keep
running, Ployz stops at the first server that fails, and a server whose upgrade fails goes back to
the release it ran before.

## Remove a server

> [!WARNING]
> Volumes stay on the removed server's disk, but your services lose them. Copy off any data you
> need first.

1. Open the server and click **Remove server**.
2. Ployz lists the volumes on the server. Type the server's name and click **Remove**.

<!-- screenshot: the Remove web-2? dialog listing one volume, with the name typed -->

[Drain the server](#change-what-a-server-does) first to move its services off. While services still
run there, **Remove server** says how many, next to a **Drain** button. If only services whose
volume is on the server are left, it names them: draining won't move them. It also names services
no project owns, which draining leaves. `ployz server rm`
warns about services still running there and prints the drain command.
Otherwise services that ran only on that server stop. Your next deploy replaces its replicas on your other
servers, except for services whose volume was on it (see
[When a server goes down](../services/scaling.md#when-a-server-goes-down)). Removing your last
server stops everything; your projects and settings stay, and the bottom bar shows **Add a server**
until you [add one](add-a-server.md).

If the server is unreachable, remove it without resetting it. This is CLI-only for now:

```sh
ployz server rm web-2 --no-reset --confirm web-2
```

Ployz stays installed on a removed server, so you can add it again later. To remove Ployz itself,
run `sudo ployz-uninstall` on the server after you remove it. Docker, your images and your volume
data stay on the disk.

## Forget servers you deleted

If you deleted all your servers at your provider, forget them:

1. Go to **Organization** and, under **Danger**, click **Forget Servers**. When the **Servers**
   page can't reach your servers, **Forget them and start over.** opens the same dialog.
2. Ployz lists the servers and their volumes. Type your organization's slug and click **Forget
   Servers**.

Ployz refuses while any server answers. Your projects, settings and deployment history stay, but
volume data on the forgotten servers can't be recovered. When you add a server again, Ployz deploys
your environments to it.
