---
title: Manage servers
description: Server status, what runs where, roles, upgrades, and removing a server.
---

The **Servers** page lists every server in your organization. Open one to see what runs on it,
turn builds on or off, or remove it.

## Check your servers

Each row shows a server's name and the services on it, up to three by name and then a count like
**+2**. A green dot means the server is online, and the row says nothing more. A row names its
status only when someone should look.

![The Servers page, listing each server, what runs on it and its status](../images/servers-page.png)

| Status | What it means |
| --- | --- |
| Green dot, no word | The server is up and in touch with your other servers. |
| **Building** | Online, and running builds right now. |
| **Offline** | Your other servers can't reach it. See [A server shows Offline](../troubleshooting/servers.md#a-server-shows-offline). |
| **Unknown** | Ployz can't tell whether it's up. Check it as you would an offline one. |
| **Upgrading** | Ployz is upgrading it. See [Upgrade Ployz on a server](#upgrade-ployz-on-a-server). |
| **Upgrade failed** | Its last upgrade didn't finish: it failed, stopped partway, or Cloud couldn't tell how it ended. A failed upgrade goes back to the release it ran before. **Show details** on its page has where it stopped and any error. |

When the list is wide enough, each row also shows how much of the server's CPU and memory is in
use right now. The meters stay empty until the server sends a reading, and an offline server shows
none. A dash means no recent reading reached Ployz for that meter: the server sent none, or its
clock runs behind, so its readings look old. The server's page has disk and the full picture.

A server that can hold [managed volumes](../services/volumes.md) shows nothing about storage.
**Docker only** means it was added with **Docker only** storage, or fell back to it because it
can't run ZFS, and holds plain Docker volumes only. See
[Start without ZFS](add-a-server.md#start-without-zfs).
**Volume support unknown** means Ployz couldn't tell what it can store.

If the page shows **Can't reach your servers right now**, see
[Troubleshooting servers](../troubleshooting/servers.md#cant-reach-your-servers-right-now).

## Servers waiting to join

A server you installed with a Fleet Token checks in with Ployz Cloud and waits for you to enroll
it. [Add many servers](add-many-servers.md) shows how in the dashboard. From the CLI, list the
ones waiting:

```sh
ployz server ls --waiting
```

Each row shows the start of the server's id, its name, its provider, the IP address Ployz Cloud saw
it check in from, its status and how long ago it first checked in. **Installing Docker** and
**Building ZFS** mean it is still getting ready. You can enroll it already, and the enrollment
waits until it is ready for its storage. **Ready** means preparation has finished. **Docker install failed**
means Ployz couldn't install Docker there. **Blocked** means something on the server stops Ployz
from using it, and **Offline** means it stopped checking in.

To see one in full, with its CPU, memory, disks, Docker, the storage it would get and anything
blocking it:

```sh
ployz server show web-3
```

`ployz server show` shows a server that has joined the same way, so one command covers both: its
OS, versions, addresses, storage, CPU, memory and disk, and how long a round trip to each other
server takes over the mesh, the median of its last few minutes. Add `--json` for everything it knows, such as the features its daemon supports. It
also takes a waiting server's id, or the start of one at least 8 characters long, such as the
start the list prints. If two waiting servers share a name or the start you typed, it lists their
ids instead.

To enroll waiting servers, select their names or hostnames with a quoted glob:

```sh
ployz server enroll 'host-*' --name 'web-{n}' --status
```

Ployz Cloud plans the names, the first server and storage. The CLI prints that plan, source IPs
and anything skipped, then asks before starting. When your organization has no servers yet, the
oldest selected server goes first. Use `--first host-a` with a name glob or `--all` to choose
another selected server. Put the glob before `--first`.
Bare `--first` enrolls only the oldest match. `--all` selects every waiting server and prints
how many came from each provider, including **Unknown**.

`--name 'web-{n}'` numbers the planned names. Without it, each server is named after its
hostname, lowercased with other characters turned into hyphens (`MY-SERVER` becomes `my-server`).
Selection takes names or hostnames, never IDs. `--storage zfs` is the default. It uses a spare
disk when available and falls back to Docker volumes where ZFS cannot run. Add
`--no-spare-disk` to keep ZFS on the main disk, or `--no-fallback` to skip servers that cannot
use ZFS. `--storage none` uses Docker volumes. Each enrolled server runs services, takes web
traffic and runs builds.

Add `--yes` to accept the printed plan without a prompt. Without a terminal or with `--json`,
Ployz requires `--yes` and otherwise returns the plan with `confirmation_required`.
To enroll every waiting server without a prompt, for example in a script:

```sh
ployz server enroll --all --yes --json
```

A command that skips any servers exits 3 and includes the reasons in its JSON result.
If Cloud starts different names or roles, or omits a planned server, the CLI reports the
changes and exits 3. Enrollment continues in Cloud.

`--status` follows each attempt and prints a line when its stage changes. **Queued** means
the server is still preparing its storage. **In line · 3 servers ahead** means it is waiting
for a join slot. If Cloud cannot see a current position, the CLI says **Join position unknown**.
A failed attempt prints the server's error. Ctrl-C stops following while enrollment continues.
With a selection, `--status` starts matching servers that are not already enrolling after
confirmation, then follows both existing and new attempts. Bare `--first --status` stays
limited to the oldest matching server. An unknown state makes the command exit 3 once the
other attempts end.
Resume open attempts without starting new ones:

```sh
ployz server enroll --status
ployz server enroll --status --json
```

To turn away servers you don't want, name them, give a pattern or give one server's id. The start
of an id works too, at least 8 characters long and matching only one server. A name rejects every
waiting server with that name. Patterns match names only, and case matters. An exact id wins over
a server whose name is that id, a server named exactly what you typed wins over an id that starts
that way, and either wins over the pattern it spells, so `db[1]` means that name. Ployz lists what
matches with each one's id and asks before it does anything:

```sh
ployz server reject 'web-*'
```

Answer `y` and it prints `2 rejected. They won't come back under the same Fleet Token.` A rejected
server stops checking in, and asking again with the same Fleet Token gets it turned away. To keep
out every server that holds a Fleet Token, revoke the token. If one left the list before Ployz
Cloud got to it, the count says so, as in `1 of 2 rejected; the rest had already left the list.`
Add `--yes` to skip the question, for example in a script. Without `--yes` outside a terminal it
fails with `confirmation_required` and rejects nothing.

A server whose enrollment has started has left the waiting list, but `ployz server reject` still
matches it by its planned name or its id. Rejecting it cancels the enrollment and turns the server
away under its Fleet Token, where Cancel in the dashboard returns it to Waiting. Once Ployz Cloud has sent its join, the server may already be in the Cluster, so Ployz
doesn't reject it. It prints `web-1 may already be in the Cluster: Cloud sent its join` and rejects
the rest. Once that server has joined, remove it with `ployz server rm`. `--json` lists the
enrollments it cancels under `enrolling` and the ones it left alone under `joining`.

`ployz server reject` sends servers to Ployz Cloud in batches of 1000. If Cloud refuses the first
batch, for example with a 403 because you are not a member, the reject fails with that error and
nothing was rejected. If a timeout, a dropped connection or a Cloud error stops a batch, the reject
stops with exit code 3 and prints how many went through, how many may or may not have been rejected
and how many were not. This includes a timeout on the first batch, which leaves that whole batch
uncertain. `--json` lists those ids under `uncertain` and `not_rejected`. Run the same reject again
to finish. If Cloud refuses a later batch, the reject also stops with exit code 3, and that batch
and the rest count as not rejected.

A name with characters a terminal would act on, such as color codes, prints with `�` in their place.
Copying it back from the screen won't match, so take the id or the exact name from
`ployz server ls --waiting --json`. A command Ployz suggests for such a name quotes it as `$'...'`,
which bash and zsh read but other shells may not.

## See what runs on a server

Click a server. Its page shows its status and public IP address, then:

- **Resources**: meters for CPU, memory and disk now, then charts of CPU, memory and network over
  the range you pick, from **1h** to **30d**. The meters and the values above each chart always
  show the last few minutes, whatever the range; a dash means no recent reading, either because the
  server sent none or because its clock runs behind.
  Network shows each interface in its own color, out as a solid line and in as a dashed one. The
  disk bar splits used space into **Volumes**, **Mirrors**, **Logs and metrics** and **Other**,
  then **Free**. Space the server couldn't attribute to one of those shows as **Used**. A stretch
  the server didn't measure is hatched **Not measured**. A server on an older Ployz version reads
  **Upgrade web-1 to see its metrics**. If it doesn't reply, the section reads **web-1 didn't answer**.
  While the server is offline, the page leaves Resources out.

  ![A server's Resources: CPU, memory and disk meters over CPU, memory and network charts](../images/server-resources.png)

- **Running here**: the server's services, with the CPU and memory each uses on this server now,
  refreshed when you open or return to the page. A service with no recent readings on this server
  shows **–**. Past five, **All 12 services** opens the
  full list, with a filter. While the server is offline, each one says whether it's **Still on**
  another server or **Down**.
- **Services**: **Run services here** and **Drain**. See
  [Change what a server does](#change-what-a-server-does).
- **Builds**: **Run builds here** and **Builds at once**. See
  [Where builds run](../builds/where-builds-run.md#build-on-your-servers).

![A server's page: Running here with each service's CPU and memory, then Services](../images/server-running-here.png)

An entry marked **Not in any Project** is left over from a deleted project or environment; see
[Clean up after a deleted environment](#clean-up-after-a-deleted-environment).

When you restore a server from a snapshot of its disk, Ployz picks its services and web server back
up as they were when the snapshot was taken. Your next deploy updates them as usual.

## Reach a server from the CLI

When the CLI reaches your servers through Ployz Cloud, only owners and admins can run these commands:
`exec`, `service start`, `service stop`, `service restart`,
`service port-forward`, `server ls`, `server show`, `server logs`, `server set`,
`server drain`, `server upgrade`, `server rm`, `server forget <server>` and `server clean`
without `--namespace`. A member who runs one gets this, and the command exits 1:

```text
Server access needs an admin. Ask an owner or admin of acme.
```

Members can still deploy, and still see logs and restart services on these pages. `logs`, `ps`
and `metrics` still work for them in the CLI: Ployz Cloud reads the servers for them and shows
each server's newest 200 lines at most, so `logs --follow` needs an owner or admin. An admin
moved to member loses this access within 5 minutes.

### Approve a device

`exec` and `service port-forward` need an approved device. Until yours is approved, they refuse
and point you at `ployz device request`. Every other command works as before.

Ployz 0.3.4 and earlier have no `device` command. Once your servers run 0.3.5, an older CLI's
`exec` and `service port-forward` through Ployz Cloud are refused with `needs Full access`.
Over `--connect ssh://` they still work.
[Upgrade the CLI](../cli/overview.md#install-the-cli) first, then run `ployz device request`.

```sh
ployz device request
```

It prints a code, such as `mmg42klgyq`. Anyone in the Organization can request one. An owner or
admin approves it from a device that's already approved:

```sh
ployz device approve mmg42klgyq
```

For your first device, approve it over SSH on a server. Run `ployz login` there first, then the
same `ployz device approve`. A request lasts 24 hours.

`approve` checks that the key Ployz Cloud returns matches the code, then adds it to every server
and lists what each server did. A server that's down when you approve doesn't get the device.
Run `approve` again once it's back.

```sh
ployz device ls
ployz device rm mmg42klgyq
```

`device ls` shows each device and the servers missing it. `device rm` takes it off every server
that answers.

## Change what a server does

Every server runs builds, runs services and takes web traffic. To turn one off, see
[Give servers different jobs](../services/scaling.md#give-servers-different-jobs). The
[Ployz agent](../account/agent-sidebar.md) can change these too, but it can't turn web traffic
back on: that starts the server's web server, which the switch below and `ployz server set` do.

When `ployz server set` changes a label, `--accepts-builds`, `--accepts-services` or
`--accepts-ingress`, it waits for the Server your CLI connects through to list the change before
reporting the update. An immediate deploy through that same Server then sees it. If that Server
answers but doesn't list the change in time, the command warns you. When it turned web traffic on
and that setting is what went unlisted, or the wait lost its connection, it also reports the web
server start as unfinished. The target Server's record
may have changed; check `ployz server ls` before deploying. A deploy through another Server, or from
Ployz Cloud, can still plan from the old settings until that Server lists the change.

Under **Services** on a server's page, turn off **Run services here** and nothing new starts on
the server. What already runs there stays until you drain it, or until your next deploy moves it
to your other servers. If no other server runs services, deploys fail until you turn the switch
back on or add a server.

To move everything off now, click **Drain** and confirm. The dialog lists what runs on the server and
badges what won't move: **Skipped** when no project owns it, which draining leaves where it is,
**Stays** when the last drain left it because its data is on the server, and **Stops** for a global
service that runs on no other server, which the drain removes.
Draining turns services off for the server and stops its global services there. Then it moves
each replicated service's containers to your other servers one at a time. Each new container runs
the same image, and each one starts, is healthy and is routed by every server's proxy before the
old container is taken out of routing and removed.
Pre-deploy commands don't run again, and nothing is deployed. The drain starts once you confirm:
the dialog is the approval. If a global service's last other copy stopped while the dialog was open,
the drain doesn't start; open it again to see what stops now. Drains in one organization run one
at a time: a second one reads **Queued · Waiting on web-1**. While a drain runs, the
**Drain** row counts the services left to move and lists them as **Pending**; each one leaves the
list as it leaves the server. You can leave the page while one runs.
If your servers don't answer when a drain's turn comes, as while one restarts, the drain reads
**Waiting for your servers to answer** and tries again 20 seconds after each try. It starts once
they answer. While it waits, it doesn't hold up the organization's other drains: they can start and
run, and it keeps that label rather than **Queued**. It waits up to 15 minutes from when you
confirmed, and other drains ending don't extend that. If your servers still don't answer by
then, the drain doesn't start and reads **Drain didn't start. Try again.** Nothing on the server
changed. A drain whose turn comes more than 15 minutes after you confirmed still starts if your
servers answer at once. If they don't, it doesn't start.

While a server drains, you can't turn **Run services here** back on for it: Cloud refuses the
change until the drain ends. Turning services on for another server waits until
the organization's drain ends. Other server settings, like **Run builds here**, never wait for a
drain.

A service stays running where it is, and is reported with the reason, when:

- it mounts a volume or a bind mount on the server (tmpfs mounts don't count);
- its containers run different versions, because a deploy hasn't finished: deploy it first;
- a server it involves can't be checked right now;
- no other server can take it;
- you stopped it with `ployz service stop`: start or remove it first;
- its container here is the last one serving: deploy it to another server first.

When the drain ends, the **Drain** row counts what moved, stopped, stayed and failed, and how many
services no project owns it left alone. It lists each service with where it went or why it stayed.
A drain that stops early marks the service it was moving **Interrupted** and the ones it never
reached **Not attempted**; those still run on the server. If a server's proxy doesn't confirm the
switch in time, the moved service's old container keeps running on the drained server and the row says
so; the next drain removes it, along with any old container an earlier drain left there. While an old
container is out of routing but still running, `ployz ps` marks it **draining**. `ployz status`
names a draining container only when the service has too few others serving.
If anything stayed or failed, the drain stopped early, or it couldn't check what's left on the server, the button
reads **Drain again**. One failure doesn't count: a drain also moves containers off other servers whose
services are off, and when one of those moves fails without leaving anything behind, its row says the
service still runs on that server, but that failure alone doesn't make this server's drain incomplete. Turning services back on doesn't
move anything back.

`ployz server drain web-2` does the same from the CLI and exits non-zero if anything stayed or
failed, apart from that other server's move. It waits for an approval first when a global service would run on no other server (see
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first)).

Under **Web traffic** on a server's page, **Takes web traffic** is a switch. On, the line under it
reads **Your domains point at 203.0.113.10**: the server's address. Turn it off and a dialog lists the
domains pointed at your servers. When another server takes web traffic, it keeps serving them; when
this is the only one, the dialog warns that they stop working. Confirm with **Stop**: the row reads
**Stopping…** while the web server on it stops, and then **Web traffic is off**. Your
generated addresses stop pointing at it within the hour, or when you click **Check again** under
**Domain** in **Organization → General**. Until then, anyone sent to it gets no answer on ports 80
and 443; to keep the internet out for good, also close them in its firewall.

Turn the switch on and the row reads **Starting the web server…** until the web server runs there,
then your domains point at it. When the web server can't start, say because something else on the
server holds port 80, the switch goes back off and the section says why: **Couldn't start web
traffic · Port 80 is in use on web-1. Free it, then try again.** Fix the cause and click **Retry**.
If it still can't start, the section says why again and the switch stays off.

From the CLI, `ployz server set web-1 --accepts-ingress=false` turns the role off but leaves the web
server running; use the switch to stop it. `--accepts-ingress=true` turns the role on before it
starts the web server, so while the web server starts, the line under the switch reads **The web server
isn't running, so your domains don't point here.** When the role was off and the command sees that no
web server is left on the server, it turns the role back off and says why. That happens when something
else holds the port: `Web traffic stays off on web-1: port 80 or 443 is in use. Free it, then try
again.` It also happens when the web server stopped right after it started and Ployz removed it. The
command exits with code 3, and the line under the switch reads **Web traffic is off**. If turning the
role back off fails too, it says so and prints the command that turns it off.

Any other failed start leaves the role on, says `Server updated; the Ingress Proxy did not follow`
with the cause, and exits with code 3. That covers an image that won't pull, any other error from
Docker, a web server Ployz couldn't remove, a server that stopped answering, and a role that was
already on. It also covers a server that still has a web server after the failure: an older one Ployz
started again, or one another `ployz` or Ployz Cloud started there meanwhile. Until a web server runs
there, the page reads **The web server isn't running, so your domains don't point here.** To try again
from Ployz Cloud, turn the switch off, then on.

In both cases the `next:`, `inspect:` and `retry:` lines select the server the way you typed it, and
the command leaves out a `retry:` that would repeat its `next:`. They use its Machine ID when the same
command renamed it with `--name`, since the name you typed no longer selects it. `inspect:` and
`retry:` also use the Machine ID for another server, since two servers can share a name.

## Upgrade Ployz on a server

Each server's page shows the Ployz release it runs. When a newer release is out on your
organization's channel (Stable unless you [choose Beta](#choose-stable-or-beta-releases)) and a
server is behind, the top of the **Servers** page offers **Upgrade to `0.3.1`**.
Click it to upgrade every server that's behind, one at a time in name order. Your apps keep running
while they do. While an upgrade runs, the top of the page reads **Upgrading 1/4**, and the server
upgrading reads **Upgrading**.

When you upgrade from `0.3.0-beta.6`, where [the first server needs
SSH](#servers-on-02-or-030-beta6-and-earlier), upgrade every server back to back: upgraded servers don't
route traffic to apps on a server still on `0.3.0-beta.6` until it upgrades too
([#2892](https://github.com/getployz/ployz/issues/2892)). As each server restarts, its domains can
answer `503` briefly, for about 6 seconds when the app runs on another server.

### Turn on automatic upgrades

Automatic upgrades are off until you turn them on. Click the **Upgrade settings** icon at the top
of the **Servers** page and turn on **Upgrade automatically**. Only an owner or admin, or [Ployz support](../account/organizations.md#let-ployz-support-in), can change it. Ployz
starts upgrading right away, then checks every hour. Turn the switch off to go back to upgrading by
hand.

Servers that are offline or building are skipped and upgrade at a later check. An offline server
that's behind reads **Upgrades to `0.3.1` when it's back** on its page.

If a server's upgrade doesn't install, that server goes back to the release it ran before, and the
other servers wait: automatic upgrades skip that release until a newer one is out. That server
reads **Upgrade failed**, and the top of the page offers **Upgrade to `0.3.1`** to carry on yourself.
On the server's page, **Show details** has the exact error to copy, and **Try again** retries. Once an upgrade of that release succeeds, automatic upgrades pick it up again.

Every member of the organization gets one email about it, naming the server, the release, and
whether it went back to the release it ran before. Members who set **Failed automatic upgrades** to **None** under
[Notifications](../account/notifications.md) don't. An upgrade you start by hand sends no email.

### Upgrade one server

To upgrade one server, open it: its **Ployz** section reads **`0.3.1` is out**. Click **Upgrade**.
Only an owner or admin, or [Ployz support](../account/organizations.md#let-ployz-support-in), can upgrade servers, and only while they're online and not building. For a
member, **Upgrade** and **Try again** are disabled and say so. One upgrade runs at a time
per organization, so **Upgrade** and **Try again** wait while one is running.

### Choose Stable or Beta releases

Click the **Upgrade settings** icon at the top of the **Servers** page and set **Releases** to
**Stable** or **Beta**. Stable is the default; Beta gets fixes early, as prereleases like `0.3.2-beta.1`. Only an
owner or admin, or [Ployz support](../account/organizations.md#let-ployz-support-in), can change it, and it applies to every upgrade, automatic or by hand. The **Servers** page
and each server's page then show the newest release on that channel.

Switching back to Stable never downgrades a server. A server on a beta ahead of Stable reads as
current and waits until Stable catches up.

From the CLI, `ployz server upgrade-policy show` and `set` read and change the same two settings.

### New major lines

Upgrades stay on your servers' release line (`0.x`). When a new line like 1.0 is published, the top
of the **Servers** page links **Ployz 1.0 is out** to its release notes. Nothing installs the new
line for you: your servers stay on 0.x and keep getting its upgrades.

### Upgrade from the CLI

`ployz server upgrade stable web-1 web-2` upgrades web-1, then web-2. When you're signed in,
Ployz Cloud runs the upgrade, so it follows your organization's channel. Naming the other channel fails with `channel_mismatch`. Ployz Cloud upgrades only to the newest
release on that channel and never moves a server to an older release than it runs. To get
prereleases, [choose Beta](#choose-stable-or-beta-releases).

With `--connect` and a server's SSH address, the upgrade runs from your computer instead of Ployz
Cloud. There you can name an exact version like `0.3.0-beta.7`, even one older than the server
runs:

```sh
ployz server upgrade 0.3.0-beta.7 web-1 --connect ssh://root@203.0.113.10
```

A version older than the server runs is a downgrade, and Ployz asks before it starts. A newer CLI
or Ployz Cloud may need something the older release lacks. On 0.3.0-beta.6 or earlier, they can't
reach the server through the relay at all, and upgrading it again may take
[SSH](#servers-on-02-or-030-beta6-and-earlier). In a script, under
`--json` or without a terminal, Ployz refuses the downgrade unless you pass `--allow-downgrade`.

Ployz releases are signed, starting with the first release after 0.3.0-beta.2. A server checks
the signature before it installs a release, and refuses one that isn't signed by Ployz or that
claims a different version. Releases up to 0.3.0-beta.2 are unsigned, so a server can't install
them, even by exact version.

To test a release before it's published, point the downloads at another copy of its assets, as
`install.sh` does. Set `PLOYZ_GITHUB_URL` to a repository's base URL, such as
`https://github.com/<owner>/<repo>`; Ployz downloads `<base>/releases/download/v<version>/...`.
`PLOYZ_FALLBACK_URL` replaces `https://ployz.sh`, which Ployz uses only when the first can't be
reached. `PLOYZ_CHANNEL_URL` replaces `https://ployz.sh` where `stable` and `beta` resolve, so
`ployz server upgrade stable` can follow a test channel's `v0/stable` pointer. All three must be
`https://` URLs, and must hold no credentials: the server passes them to its upgrade process on
the command line. Set them in `/etc/default/ployz` on the server and run
`sudo systemctl restart ployz` for `ployz server upgrade`, and in your shell for
`ployz server add --ssh` or `ployz server checkin --install`. The server still checks the
release's signature.

`ployz server upgrade` works the same way whether automatic upgrades are on or off: your apps keep
running and private DNS keeps answering (with the exceptions in
[Private networking](../services/private-networking.md)), Ployz stops at the first server that fails, and a server whose upgrade fails goes back to
the release it ran before.
The command prints one line per server it tried, the failed one included. The error after it, on
stderr, says why that server failed.
It exits 1 when a server's upgrade failed before any server moved to another release, and 3
when some did first, or when a server took the upgrade and then stopped answering, since it may
still finish. A version that was never released fails with `No Ployz release 9.9.9.`
and `not_found` in JSON. The CLI reports the Server's release lookup failure, so the CLI computer
does not need its own connection to GitHub.
A server that already runs the newest release on the channel you name, or a newer one, says
`already runs 0.3.0-beta.7`, and a server on this release or later is left alone: nothing is
reinstalled or restarted. An exact version it already runs says `Server machine-1 already runs 0.3.1.`
A downgrade says `Downgraded Server machine-1 from 0.3.1 to 0.3.0.` The Ingress Proxy follow-up
reports whether any Service changes are needed without claiming that every Server is current.
Ctrl-C stops the command, not the upgrade: the server finishes it on its own, and
`ployz server show` says how it ended.
With `--json`, each entry in `attempts` names its server's id under `server`, and `unattempted`
lists the ids of the servers the command never reached. Direct upgrades also include `from_version`,
the daemon version observed before the request. When that version is known, successful attempts
include `change` as `upgraded`, `downgraded`, or `unchanged`. Otherwise the command says the Server
`now runs` the returned version and leaves out `change`.
A server that doesn't answer within a minute counts as failed, so an offline server stops the
command quickly instead of waiting out the upgrade. Once a server has taken the upgrade, it gets
up to 5 minutes to answer again, the time its daemon has to restart, within the command's overall
16 minutes. Without Ployz Cloud, a server that powers off or loses its network while it upgrades
stops the command after those 5 minutes with
`Server machine-1 stopped answering during upgrade …`, and exits 3. Once it's back,
`ployz server show` says how the upgrade ended.

An upgrade doesn't always replace the web server Ployz runs on each server. One created by an
earlier release can hang a request for about 15 minutes when a server running one of your
replicas powers off. See [This app didn't respond](../troubleshooting/app-not-reachable.md#this-app-didnt-respond)
to replace it.

With Ployz Cloud, a server that's offline, building, or already taking another upgrade is skipped
instead: Ployz names it and says why, upgrades the rest, exits 3, and prints the command that
upgrades the skipped servers once they're free. A server already upgrading isn't upgraded twice;
the command follows the upgrade that's running.

## Servers on different versions

Ployz Cloud and the CLI keep working with servers that run an older Ployz release, back to the
first release after 0.3.0-beta.6. The **Servers** page shows each one and offers its upgrade, as in
[Upgrade Ployz on a server](#upgrade-ployz-on-a-server). A server on 0.2 or 0.3.0-beta.6 and earlier is
the exception: see [Servers on 0.2 or 0.3.0-beta.6 and earlier](#servers-on-02-or-030-beta6-and-earlier).

When a server's release can't serve a feature, the feature names that server:

- **Logs** show the lines from your other servers, with a note such as **web-2 needs an upgrade to
  show its logs** and a link to web-2. When no server can send its logs, the log reads **Upgrade
  web-2 to see its logs** with **Open web-2**, or **Upgrade 2 servers to see their logs** with
  **Open Servers**. Upgrade the server from its page, then refresh the logs page. See
  [Logs](../observe/logs.md).
- **Copies** on a volume names the servers to upgrade before any volume moves or mirrors. See
  [Move a volume](../services/move-a-volume.md).
- **`ployz volume move`** onto such a server fails. If the server already runs the newest release
  on your channel, a later release brings the feature, and the message reads:

  ```text
  Volume move is not in Ployz 0.2.4 on web-2 yet.
  ```
  Every volume move, mirror, sync, release, restore and mirror removal, and a Deploy that restores
  a volume onto a new server, also needs every server that holds or receives the volume to run a
  Ployz new enough to refuse a request from an older run. The message names what you ran and the
  server: "Volume move needs a newer Ployz on web-1, which runs 0.3.0. Upgrade it to 0.3.1." when
  that server's release is behind your channel's. When Ployz can't tell, it says only what is
  missing: "Mirror removal is not in Ployz 0.3.0 on web-2."
  A volume move onto a server on 0.3.0-beta.6 is refused before any data moves: that release can
  take the volume, but can't put the moved service back in web traffic. A move that already reached
  such a server waits at "Starting" until you upgrade it, then finishes.
- **Drain** in Ployz Cloud names the servers to upgrade before it changes anything:
  "Drains need a newer Ployz on web-1".
- **Upgrade** off Ployz 0.2.4 that stops partway while the old Ployz keeps running looks like it's
  still going: Cloud waits until the upgrade times out, then shows its result as unknown rather
  than interrupted. Start the upgrade again.

From the CLI, `ployz logs` prints the logs from your other servers, names each server that can't
send its own, and exits with code 3:

```text
$ ployz logs api
! Server web-2 needs a newer Ployz. It cannot serve ployz.logs.history.v2.
```

With `--follow`, the command stops at the first server that can't stream its logs and exits with
code 1. With `--json --follow`, it ends the output as a partial result and exits with code 3
instead. `ployz exec` into a container on such a server exits with code 1 and names the server:

```text
Server web-2 needs a newer Ployz. It cannot serve ployz.container.exec.v1.
```

`ployz server drain` of a server on Ployz 0.2.4 turns its services off, then stops and names the server:

```text
Server web-2 needs a newer Ployz. It cannot serve ployz.container.ensure.v1.
```

Upgrade the server and run `ployz server drain` again. The rerun finds services already off and carries
on.

### Servers on 0.2 or 0.3.0-beta.6 and earlier

Starting with the first release after 0.3.0-beta.6, Ployz Cloud and the CLI prove their access in a
new way on every connection, and servers on 0.2 or 0.3.0-beta.6 and earlier don't understand it. Through
Ployz's relay, Cloud and a newer CLI can't connect to such a server directly. The server keeps
running your apps. When Cloud's last try to reach your servers lands on such a server, the
**Servers** page reads **Upgrade your server over SSH** and shows the commands below. When the CLI can't reach a server on a newer
release first, it stops with:

```text
This Server runs Ployz 0.2 or a 0.3.0 beta up to beta.6, which this release can't reach through the relay.
```

Every 0.2 release, 0.2.4 included, is on this side of the boundary. A server on 0.2 upgrades to
0.3.0 in place the same way: it keeps its apps, volumes and place among your servers, and doesn't need to be
enrolled again. Upgrade it with the newest CLI, not with a 0.2 CLI's `ployz server upgrade`: that
installs 0.3.0 but can't confirm the result once the server restarts.

Upgrade the first server over SSH from your computer. Install the newest CLI, then run it with
`--connect` and the server's SSH address:

```sh
curl -fsSL https://ployz.sh | sh
ployz server upgrade stable web-1 --connect ssh://root@203.0.113.10
```

The server installs the newest release on the channel you name. Then Cloud and the CLI reach it
through the relay again. If the upgrade fails, the server goes back to the release it ran before
and keeps running; run the command again.

Once one server runs the newer release, Cloud and the CLI can reach the others through it, so you
can upgrade the rest from the **Servers** page or with `ployz server upgrade`. Do it right away:
see [Upgrade Ployz on a server](#upgrade-ployz-on-a-server) for why. If a server still stops with
the message above, upgrade it over SSH too.

Without SSH from your computer, say with only your provider's console, upgrade a server on the
server itself:

```sh
sudo ployzd install --version stable --software-only
```

`--software-only` replaces Ployz and leaves Docker, packages and storage as they are.

The same boundary holds the other way: a CLI on 0.2 or 0.3.0-beta.6 and earlier can't reach a server
on a later release. Install the newer CLI with `curl -fsSL https://ployz.sh | sh`.

## Remove a server

> [!WARNING]
> Volumes stay on the removed server's disk as copies, and your services lose them until you
> make a copy the writer again with `ployz volume restore`. The server's mirrors of volumes
> written on other servers are deleted, and so is a restore still copying there. A restore that
> finished copying stays as a copy. Copy off any data you need first.

1. Open the server and click **Remove server**.
2. Ployz lists the volumes on the server: each of your volumes by its name and its project and
   environment, such as `pg-data` in `shop/production`. A volume Docker made with no name shows as
   **Anonymous volume**. Type the server's name and click **Remove**.

The server you remove and every server Ployz reaches it through must run a Ployz that can remove
servers. If one doesn't, the removal stops and says "Server removal
needs a newer Ployz on web-1". Upgrade that server, then remove again.

<!-- screenshot: the Remove web-2? dialog listing one volume, with the name typed -->

[Drain the server](#change-what-a-server-does) first to move its services off. While services still
run there, **Remove server** says how many, next to a **Drain** button. If only services whose
volume is on the server are left, it names them: draining won't move them. It also names services
no project owns, which draining leaves. `ployz server rm`
warns about services a drain could still move off it and, while it still asks you to confirm,
prints the drain command. With `--confirm` it doesn't ask, so it keeps the warning but leaves out
the drain command. The warning leaves out what a drain won't move: services with a volume or bind
mount on the server, global services, and Ployz's own web server. On your last server it skips that
warning. After the removal it warns which replicated services now run no replicas, because all of
them were on that server, and prints `ployz deploy` as the next step. It warns separately which
other replicated services may now run short of replicas. A deploy that still has work on that
server when the removal finishes fails on its turn with `Server machine-2 was removed while the
deploy ran`, and the deploy's web traffic check doesn't count the removed server as unchecked.
That needs Ployz 0.3.4 or later on the server the CLI connects through; an older one names the
server's ID instead. While the removal runs, a deploy doesn't add a missing web server to the
leaving server, and its web traffic check doesn't count the leaving server as unchecked when it
stops answering partway through the removal. That needs Ployz 0.3.4 or later on the server being
removed and on the server the CLI connects through. Once the removal is done, `ployz ps` and the other listings that were still asking it
warn `machine-2 was removed while this ran; its rows are missing.` and exit 3; that needs 0.3.4 or
later on the server the CLI connects through. With `--json` the removal lists every
service left short as `under_replicated`. It also lists a service as `no_copies` when every other
server answered and none showed a replica of it. If a server didn't answer, the service is listed
only as `under_replicated`. `ployz status` keeps naming a service with no replicas until a deploy
starts one; see
[Status says a service runs no copies](../troubleshooting/deployments.md#status-says-a-service-runs-no-copies).
Without a terminal, the first `ployz server rm` refusal names the Volumes requiring loss
acceptance before it gives the command to confirm. If the Server has none (builder-cache Volumes
don't count), drop `--accept-volume-loss`; supplying it still exits 2. An empty Server name also exits 2 and asks you
to name a Server. On a Server Ployz Cloud does not manage, `--approval` has no effect and prints a
warning, also included in JSON `warnings`. The warning does not change the exit code.

Ployz's build cache isn't one of the volumes you confirm losing.
Otherwise services that ran only on that server stop. Your next deploy replaces its replicas on your other
servers, except for services whose volume was on it (see
[When a server goes down](../services/scaling.md#when-a-server-goes-down)). Removing your last
server stops everything; your projects and settings stay, and the bottom bar shows **Add servers**
until you [add one](add-a-server.md).

If a standalone context has no Servers left, runtime commands say which context is empty
and print `ployz server add --standalone --context NAME` as the next step. This starts a
new standalone Cluster in that empty context without signing in to Cloud. Run it on the
Linux server, or append `root@HOST` to install over SSH from your computer. See
[Add a standalone server](add-a-server.md#add-a-standalone-server).

On a server that joined from a Fleet Token, `sudo ployz status` then says
`this server = Not in a Cluster`.

The reset first stops every copy in progress on the server. If one doesn't stop within 30 seconds,
the removal fails and names its volume; remove the server again.

Your other servers also forget which replicas the removed server was sending requests to, so
`ployz status` never names it as a server that hasn't picked up a service's new version.

If the server resets but your other servers can't forget it, `ployz server rm` says "Server web-2
reset but is still listed." and prints `ployz server forget web-2` to finish. Ployz Cloud
finishes such a removal on its own.

A Ployz Cloud removal that retries can find the server already gone. Ployz then
can't confirm the server reset. Cloud drops its record of the server, and `ployz server rm` says
"Cloud let go of Server web-2, but its reset couldn't be confirmed: it was already gone from the
Cluster. Reset does not erase volume data." and exits with code 3. Your last server is the exception: Cloud keeps its hold on your
servers and says why. With `--json`, `cloud_kept` says which of the two happened.

`sudo systemctl stop ployz.socket ployz` stops Ployz's control of the server. Stop the socket too:
any `ployz` command on the server starts Ployz again through it. Your services, their private DNS,
the private network and the server's place among your servers keep running until the server reboots or
you remove it.

When Ployz Cloud manages the server, `ployz server rm` and `ployz server forget` always wait for
an approval (see
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first))
before they take the server out, and so does an agent that removes or forgets one. The
**Remove server** and **Forget server** dialogs don't ask: confirming one is the approval. An
[agent sandbox](../account/try-without-an-account.md) that no person has claimed yet doesn't ask
either, since no one could approve. One
approval removes the server once, and approving a forget doesn't approve a removal that resets
the server.

When you remove a server, the approval names each volume Ployz loses access to. The removal
doesn't erase those volumes' data: it stays on that server's disk until you wipe or reinstall
the disk. `sudo ployz-uninstall` keeps volume data. Forgetting a server says the volume's data
is deleted, since a server you forget may already be gone with its disk.

Removing or forgetting a server Ployz Cloud manages always goes through Ployz Cloud, even with
`--context` or `--connect`: Cloud keeps a record of each server it manages, and removing one behind
its back would leave that record behind.
While other servers are listed, Ployz Cloud carries the removal through one of them, since a server
can't carry its own; your last server carries its own. If none of the others answers Ployz Cloud,
the removal fails on its first try, names them, and changes nothing: bring one back online, or forget
the ones that are gone, then remove the server again.
Ctrl-C stops following the removal but doesn't stop it: it keeps running in Ployz Cloud, and the
server stays listed, since the removal can still fail.

If Ployz Cloud runs out of retries after it sent the server its reset, the removal didn't finish,
and Ployz can't say how far it got: the server may have reset, and Ployz Cloud may already have
dropped it. `ployz server rm` says the removal didn't finish and prints `ployz server ls` as its
next step, so you check before you forget anything. The **Remove** dialog says the removal didn't
finish and points at **Forget server**. If the
server is gone but `ployz server ls` (or the **Servers** page) still lists it,
[forget it](#forget-a-server-that-is-gone); if it still answers, remove it again; if it's no longer
listed, there is nothing left to forget. A removal that runs out of retries before it reaches the
server only shows its failure. While Ployz Cloud works, `ployz server rm` and `ployz server forget`
say why after 30 seconds, then print a line each minute. They stop following a removal after 20
minutes. They then say the removal keeps running in Ployz Cloud, and to forget the server if the
removal fails and the server is gone but still listed.

Ployz stays installed on a removed server, so you can add it again later. To remove Ployz itself,
run `sudo ployz-uninstall` on the server after you remove it. Docker, your images and your volume
data stay on the disk. It also removes the server's Fleet Token and its check-in identity, so
installing it again with a Fleet Token checks it in as a new host, as on any other server. If it says `Ployz mutation or installation is active; retry uninstall`,
wait for the upgrade or install to finish and run it again. Nothing was removed.

## Forget a server that is gone

If a server is gone or can't be reset, forget it instead. Forgetting takes it off your list of
servers without reaching it. The volumes it held come back only where they have a
[mirror](../services/volumes.md#when-its-server-goes-down); the rest are lost with it.

When the server is offline, its page shows **Forget web-2** under **Danger**. Click **Forget
server**, type its name, then click **Forget**.

The dashboard offers **Forget server** only while the server shows as offline. When it still
shows as up but doesn't answer within 10 seconds, forget it from the CLI and type `dead` to
confirm:

```sh
ployz server forget web-2 --confirm dead
```

The approval Ployz Cloud asks for names the server and each volume whose only copy is on it, as
those are lost with it. A volume with a mirror or a writer on another server isn't listed.

Once you approve, forgetting an offline server takes seconds. Forgetting one that is still up but
doesn't answer takes Ployz Cloud longer: a minute or two when the server holds only mirrors, and
about 11 minutes when it holds a volume's writer, since Ployz Cloud retries the server first in
case it still writes the volume. `ployz server forget` prints a line each minute meanwhile, and
says `Forgot Server web-2.` once it's done.

`ployz server rm` on a server that doesn't answer stops before it changes anything and prints that
command as its next step. When Ployz Cloud itself doesn't answer, it says to run `ployz server rm`
again instead. A server that answers but refuses isn't gone: `ployz server rm` prints its reason and
doesn't offer to forget it.

When it is your only server, Ployz Cloud forgets it as it [forgets servers you
deleted](#forget-servers-you-deleted). When none of several servers answers, forget them all that
way.

If your only server still answers, `ployz server forget` refuses. Stop its Ployz daemon first, or
use `ployz server rm` to reset and remove it. On the server, run
`sudo systemctl stop ployz.socket ployz` to stop both the daemon and its socket.

Ployz refuses to forget a server that answers and holds a copy of any volume. Remove it with
`ployz server rm` instead, and the reset makes its copies read-only and deletes its mirrors.

A mirror that Ployz v0.3.0-beta.2 or earlier refreshed last stays on the server after the reset.
To have the reset delete it, the mirror must finish a pull from the writer before you run
`ployz server rm`. `ployz volume sync <volume> --full --wait` copies the whole volume to the
mirror again and returns only once the mirror holds the writer's newest data. Plain `ployz volume
sync <volume>` hands the mirror its pull and returns before the download ends, and removing the
server during the download stops the pull and keeps the mirror. It also pulls only what the
volume wrote since the mirror's last refresh, so the mirror of a volume that wrote nothing since
pulls nothing and stays.

A forgotten server isn't told it was removed. If it runs again as it is, it starts its services on
its own copy of each volume and they keep writing there, and a later move, mirror, sync or release
of the volume doesn't reach it. Ployz never brings back what they write there, and if you've
restored the volume on another server, the two copies drift apart.

A forgotten server stops its services itself as a best effort. It needs the server and at least
one other server to have run Ployz 0.3.1 or later while it was still connected, and the server
that forgot it to run 0.3.1 or later. Once it has heard nothing from a server for a minute, it asks
that server whether it was forgotten, and when one says so it resets itself as `ployz server rm`
would, except that it keeps every volume copy on its disk, mirrors included. `journalctl -u ployz`
on it then says `this Machine was forgotten by its Cluster`.

- A server that was unreachable when it was forgotten stops its services up to about 15 minutes
  after its network returns.
- A server that was still running, but that Ployz Cloud couldn't reach, stops them about a minute
  after the forget, once it runs Ployz 0.3.10 or later. Before 0.3.10 it put itself back on your
  list of servers as soon as it was forgotten, and kept running.

Nothing stops its services when it can't reach any of those servers, or when the versions don't
line up. Those cases, and the minutes before it asks, are yours to cover.

So decide before a forgotten server runs again. As soon as it boots, run `sudo ployz-uninstall`
on it. That stops the services Ployz runs there and keeps every copy of a volume on the disk, along
with Docker and your images. Adding it back later, with `ployz server add` and `--reset`, makes its
copies read-only and keeps their data but can delete its mirrors, even after an uninstall. So don't
add it back while a mirror there may be the last copy of a volume. Wipe or reinstall its disk only
if you need nothing on it. Ployz no longer lists a forgotten server, so `ployz server rm` can't
reach it. `ployz server forget` prints this warning each time it forgets servers.

`ployz server forget` warns about replicated services as `ployz server rm` does, with one
difference. A forgotten server may still run its replicas, so for a service with every replica on
it, forget says "No copies of Replicated Services were seen on the remaining Servers" rather than
that the service runs none. With `--json` it lists them as `no_copies` all the same.

To add a forgotten server back, run `ployz server add` for it with `--reset`. It resets the server
first, as `ployz server rm` would have, and asks before it does; `--yes` skips the question.

## Clean up after a deleted environment

If removing an environment stopped partway, its services and volumes can stay on your servers.
Open the server: under **Running here**, **Namespaces no Project owns** lists what's left of
deleted environments, each marked **Not in any Project**. Click **Remove** on one,
type its name and confirm. That deletes its containers and its volumes, data included, from every
server.

From the CLI, `ployz server clean` lists the same, and
`ployz server clean --namespace my-app-staging --confirm my-app-staging` removes one. It asks for
an approval before it stops services or deletes volume data (see
[Destructive actions ask first](../account/organizations.md#destructive-actions-ask-first)), and
works only when you're signed in, without `--context` or `--connect`, because only Ployz Cloud
knows which environments still exist.

## Forget servers you deleted

If you deleted all your servers at your provider, forget them:

1. Go to **Organization** and, under **Danger**, click **Forget servers**. When the **Servers**
   page can't reach your servers, **Forget them and start over.** opens the same dialog. Only an
   owner or admin, or [Ployz support](../account/organizations.md#let-ployz-support-in), can forget
   servers. For a member the button is disabled.
2. Ployz lists the servers and their volumes. Type your organization's slug and click **Forget
   servers**.

Ployz refuses while any server answers. Your projects, settings and deployment history stay, but
volume data on the forgotten servers can't be recovered. When you add a server again, Ployz deploys
your environments to it. If your organization has a [recovery code](recovery-code.md), that
server shows **Needs recovery** until you give it the code with `ployz recover`.
