---
title: Add a server
description: What a server needs, the command to paste on it, and the ports to open.
---

Your apps run on your own servers. To add one, you paste a single command into it, and it joins
your other servers on a private network.

## What you need

- **A Linux server with systemd**, amd64 or arm64, from any provider: a Hetzner Cloud server, a
  DigitalOcean Droplet, a bare-metal box.
- **cgroup v2** to run builds. Every current distribution uses it. A server on cgroup v1 joins and
  runs apps, but its builds fail.
- **Ubuntu LTS, Debian 12–13 or Amazon Linux 2023 on a VM or bare metal** for managed volumes.
  Anything else works [without them](#start-without-zfs).
- **A main disk of 30 GiB or more.** Ployz keeps a quarter of the disk, at least 10 GiB, free for
  the system. On EC2, raise the root volume from its 8 GB default when you launch the instance.
- **Root**, or a user that can run `sudo`.
- **Outbound internet access over IPv4**, and a **public IP address** on at least one server so
  visitors can reach your apps. A server with only IPv6 needs NAT64 and DNS64; see
  [IPv6-only servers](#ipv6-only-servers).

You don't need Docker: Ployz installs it if it's missing.

## Add one server that joins right away

From your own computer, signed in with `ployz login`:

```sh
ployz server add --command
```

It prints a line to paste on the server and an enrollment token beside it, then waits until the
server joins. Paste the line in a shell on the server, as root or as a user with `sudo`, and paste
the token when it asks. The prompt doesn't show what you paste, so the token never enters the
server's shell history or process list. It takes one line: a paste of more than one line is
refused and nothing is kept, so paste the token alone. The token works for 24 hours.

A script, such as cloud-init or `ssh host '...'`, has no terminal to answer the prompt. There,
`export PLOYZ_ENROLL_TOKEN=pmet_…` before the line: it keeps that variable through `sudo`. You can
also pipe the token to `sudo ployz server add --token-stdin` after the install part of the line.

The server takes its hostname as its name, lowercased, with anything else a name can't hold, such
as `_` or `.`, turned into `-`; `MY-SERVER` becomes `my-server`. The line prints the name it chose.
To choose another, run
`ployz server add --command --name web-1` and the printed line keeps the name. It keeps every other
flag that shapes the server too, such as `--storage`, `--label-add` or `--accepts-ingress`. When it
finishes, the server shows on the **Servers** page, ready for builds, services and web traffic. If
it doesn't show up, see [Troubleshooting servers](../troubleshooting/servers.md).

While the server joins, the line prints `Waiting for the Server's daemon to start…`. The Ployz
daemon restarts once after it enrolls, and a joining server catches up with your other servers,
which usually takes under a minute. If it is still catching up after a minute, the line says so
and names UDP 51820, which your servers use to reach each other. The wait gives up after 5
minutes.

### Put the server in a region

A server joins the `default` region unless you name another one with `--region`:

```sh
ployz server add --command --region fsn
```

The printed line keeps `--region`. A region is made of letters, digits, hyphens, underscores and
dots, such as `fsn` or `eu_west.1`. Capitals are lowered, so `FSN` joins `fsn`. `--label-add
region=fsn` does the same. Give both with different regions and the command stops before anything
starts, saying to keep `--region`. The dashboard's line joins the `default` region; set the
region afterwards, as in [Set a server's region](manage-servers.md#set-a-servers-region).

Run the command again for a server that already joined and it keeps its region, or stays without
one if it joined before regions. Name a different region and the command stops and says which
region the server is in. To move a server, see
[Set a server's region](manage-servers.md#set-a-servers-region).

Ask the [Ployz agent](../account/agent-sidebar.md) to add a server in a region, such as Sydney, and
it gives you the `ployz server add` line with `--region` for you to run. It adds nothing itself.

Add more servers the same way, even at different providers. Each server after your cluster's
first waits for a device you approved to sign its join, and you're told the code to approve; see
[Approve a join](#approve-a-join). Nothing moves onto a new server until
you deploy; see [Scaling](../services/scaling.md#add-a-server).

### Approve a join

An existing server admits a new one only under an approval from one of your approved devices.
Until then it publishes nothing about the new server and doesn't share the cluster's key for
sealed values. When you add a server from an approved computer with `ployz server add`, the CLI
signs that approval with your device key.

A server that joins with the line from `ployz server add --command` or the dashboard has no
approved device key, so its first join stops and waits for a person to approve it. The line
carries a key, `--key pk1_…`: the device or passkey the new server trusts to approve its join.
The line from `ployz server add --command` carries your computer's device. The dashboard's line
carries your passkey. The new server refuses an approval signed by any other key.

With the dashboard's line, approve the join from your browser. The **Servers** page lists the
server under **Waiting for approval**, with its join code. Choose **Approve with passkey** and
answer your passkey's prompt. Approve a server only if you added it. Your passkey signs the join
for that one server, and Ployz Cloud only passes the approval on. The prompt offers only the
passkey the line carried. When that passkey belongs to someone else, the dashboard says so and
offers no button: ask them to approve it. Once it's approved, run the same line again on the
server.

With the line from `ployz server add --command`, approve from the computer whose device the line
carries. The join command on the server prints a 20-character code. On that computer, run:

```sh
ployz device approve --join CODE
```

The code carries 100 bits of the server's identity and key fingerprint. Check the name, subnet
and endpoints the CLI shows before it signs. It also lists the devices the server will trust, and
the passkeys, if your cluster has any, with what a passkey can do. Then run the same join command
on the server again, with its token and options. A new admission needs an approval less than 15 minutes old. If the reply
is lost, run the same command again to recover the stored capsule, even after the approval expires.

On a computer whose device isn't the one the line carries, the command stops before it signs and
names the key the server waits for:

```text
Server web-2 trusts only passkey tbpigkmhxf, not this computer's device vgu6xrfgwt. Approve it with that passkey: choose Approve with passkey on the Servers page in Ployz Cloud. Nothing was approved.
```

If a join was approved with another key anyway, the server refuses it and the join stays open
until the server joins. Approve it again with the key the server waits for, then run the join
command again.

A new server refuses to join without an approval. If the approval goes missing on the way, the
join stops and says the join approval carries no signature. Nothing changes on the server, so
you can run the join again.

The new server trusts the same devices, passkeys and CI or agent keys as the server that admits
it, and none that server removed. It takes that list from the admitting server, sealed under the
cluster's key, and not from the approval. A key someone removed while the join ran is removed on
the new server too.

When you run `ployz server add` over SSH, the CLI tells the new server which devices may sign its
approval: your own, and with `--standalone`, those of the server it joins through. A join from a
pasted line trusts the key that line carries. The new server refuses an approval signed by any
other key, and a join with neither stops:

```text
copy the install command again, or run sudo ployz key add LINE on this Server
```

When `ployz server add` or `ployz device approve --join` reaches your cluster through Ployz
Cloud, the CLI signs only for a cluster whose key you pinned with `ployz device pin`, and
otherwise stops before it signs anything:

```text
Server web-2 would join a Cluster this computer hasn't pinned, and the join was relayed through Ployz Cloud, which can't vouch for it. Pin your Cluster's key from one of its Servers, then add the Server again. Nothing was approved.
```

Run `ployz device pin --connect ssh://USER@SERVER` against one of your servers, then add the
server again.

Each server checks new servers against its own devices before it accepts them as members or
WireGuard peers.

The admitting server doesn't publish the new server as a member until the new server proves it
opened its capsule. Opening the capsule gives the new server the cluster's key for sealed values,
and it answers with a confirmation only that key can produce. The join command sends the
confirmation back the same way as the join, and the admitting server checks it before it
publishes the new server. Until then, no server peers with it.

### Join through Ployz Cloud without an approved device

`ployz server add root@HOST` tells the new server which devices may approve its join: this
computer's device. From a computer with no device key it has nothing to tell it, so it stops
before the server joins:

```text
web-2 can't join from here: this computer has no device key, so nothing tells the new Server which devices may approve its join. Run the same command from an approved device. Nothing was enrolled.
```

`ployz server add --standalone` can also take the devices of the server it joins through, when it
reaches that server over SSH. With neither, it stops the same way:

```text
web-2 can't join from here: this computer has no approved device key and reaches your Servers only through Ployz Cloud. Run it from an approved device, or add --connect ssh://<USER>@SERVER for a Server of the Cluster. Nothing was enrolled.
```

The line the dashboard or `ployz server add --command` gives you runs on the server itself and
carries the key that may approve its join.

### The key on an install line

The key a line carries is public: it names a device or passkey, and holds no secret. The server
keeps it as its first key, in `/var/lib/ployz-first-key.json`. It trusts that key to found the
cluster, or to approve its join, once. After the server enrolls, the first key is used up, and
the server trusts only what the cluster does.

Check that the line you paste is the one you copied. Someone who changes the key on a line before
you run it can approve that server's join.

A server that already has a first key keeps it. Running a line with a different key asks you at
the server's terminal to type the new key's code. It never takes the new key from `--yes` or the
environment. With no terminal, as under cloud-init, it changes nothing and prints the command to
run at one:

```sh
sudo ployz key add pk1_…
```

Removing a server with `ployz server rm`, which resets it, and the uninstaller use up the first
key too. To add a server you removed or reinstalled, run `sudo ployz key add` with the key from a
new install command at the server's terminal, then join it again.

`ployz server add USER@HOST --reset` can't make such a server found a cluster through Ployz Cloud,
since no line names its key. Run `ployz server add --command` instead, and run the line it prints
at the server's terminal. With `--json`, `ployz key add` never asks, so it changes nothing there.

## Add a standalone server

To start a Cluster outside Ployz Cloud, run this on the Linux server:

```sh
sudo ployz server add --standalone --context NAME
```

From your computer, give the server's SSH destination instead:

```sh
ployz server add --standalone root@HOST --context NAME
```

No Cloud sign-in or enrollment token is needed. A new or empty context gets its first
Server and becomes the current context. To add another Server to a context that already
has connections, use the SSH form. It joins the existing Cluster.

Run on the server, it approves no device: a device made there would be root's on that server, ride
into every server you add later, and stay trusted after the server is gone. It prints the command
that gives the server its first key from your computer:

```sh
ployz device approve --this-device --connect ssh://root@HOST
```

## Your first server's first key

A new cluster's first server trusts the key its install line carried, and nothing else. A line
from the dashboard carries your passkey, so the first server trusts your passkey to approve
joins, automatic deploy rules and Private Access. Ployz Cloud can't give it a device. Until you
approve one on the server itself, it refuses deploys and the other changes that
[need an approved device](manage-servers.md#what-needs-an-approved-device). Approve one over SSH on that server:

- **From your computer, over SSH.** Run
  `ployz device approve --this-device --connect ssh://root@HOST`, as in
  [Approve a device](manage-servers.md#approve-a-device).
- **With a device request.** Run `ployz device request` on your computer, then on the server run
  `sudo ployz login` and `sudo ployz device approve CODE`.

`ployz server add --standalone root@HOST` approves this computer's device on the new server for
you. Servers you add later trust the same devices and passkeys as the server that admits them.

## Add a server from the dashboard

Each install line adds one server. Before your first server, the **Servers** page shows the card
**Add your first server**. While you have no server, the bottom bar of an environment offers the
same card under **Add server**; you can skip it and build your project first. Once you have a
server, click **Add server** on the **Servers** page, which opens **Add a server**.

1. Click **Get install command**. It makes a token for one server and shows the install line on
   the **Shell** tab, with the token under it and **Shown once. Works for one server, for 24
   hours.** Copy the token now: Ployz can't show it again.
2. Open a shell on the server, as root or as a user with `sudo`, and paste the line. Not on Ubuntu,
   Debian or Amazon Linux, or can't run ZFS? See [Start without ZFS](#start-without-zfs).
3. Paste the token when it asks. The prompt doesn't show what you paste.

The line is the same one `ployz server add --command` prints:

```sh
curl -fsSL https://ployz.sh | sh -s -- VERSION && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add --key pk1_…
```

To start the server from user data, take the **cloud-init** tab instead. It sets the token before
the same line, so nothing asks for it:

```yaml
#cloud-config
runcmd:
  - export PLOYZ_ENROLL_TOKEN=pmet_…; curl -fsSL https://ployz.sh | sh -s -- VERSION && sudo --preserve-env=PLOYZ_ENROLL_TOKEN ployz server add --key pk1_…
```

Any process on the server, containers included, can read user data from the provider's metadata
service for as long as the server runs. The token in it works for one server, for 24 hours.

The server joins the `default` region. To put it in another, see
[Set a server's region](manage-servers.md#set-a-servers-region), or add the server with
`ployz server add --command --region NAME` instead. For the next server, click **Get install
command** again: each token adds one server.

### Your first server

Your first server starts your cluster as soon as its line runs. The card says **Waiting for your
server…** until the server shows in the list.

### Every server after the first

A later server waits for your approval to join, and the dialog says so: **The server waits for your
approval to join: approve it under Waiting for approval on the Servers page, then run the line
again.**

1. Run the line on the server. It stops and prints a join code.
2. On the **Servers** page, find the server under **Waiting for approval** and choose **Approve
   with passkey**. See [Approve a join](#approve-a-join).
3. Run the same line again on the server. The token still works, for 24 hours from when you made
   it.

The server then joins and trusts the same devices and passkeys as your other servers.

### The key on the dashboard's line

Add a passkey in **Account** before you make the first install command, and the line carries it:
your first server trusts that passkey. With no passkey, the line carries no key, the first server
trusts none, and you approve a device on it as in
[Your first server's first key](#your-first-servers-first-key).

For a later server, the line carries your passkey once every server trusts it. The server takes
only that passkey's approval, so `ployz device approve --join` doesn't work for it.

When your servers trust no passkey of yours, the dashboard shows no install line. It names the
people whose passkey your servers trust, who can add the server instead, and the command that
makes a server trust yours:

```sh
sudo ployz key add pk1_…
```

Run it as root on each server. It changes only the server you run it on.

Only an owner or admin, or [Ployz support](../account/organizations.md#let-ployz-support-in), can
get an install command. For a member, **Add server** and **Get install command** say so instead.

### Choose a name and Cloudflare Tunnel first

On the **Add your first server** card, under **Get install command**, **Use Cloudflare Tunnel
instead of public ports** opens two choices for the Cluster your first server starts. Leave it
closed and the first server starts with ports 80 and 443 open, and your apps get addresses under
your organization's free domain.

- **Cluster name** is the name your services' private addresses end in, like
  `.acme.ployz.internal`. It starts as your organization's slug. Change it here, or later in
  **Organization → Network**.
- **Cloudflare Tunnel** serves your apps through your own Cloudflare zone, so your servers never
  open ports 80 and 443. Turn it on, paste a Cloudflare API token with **Zone Read**, **DNS Edit**
  and **Cloudflare Tunnel Edit**, and click **Find zones**. Pick the
  zone, like `example.com`, and click **Check**. Ployz checks the token can set up the tunnel and
  that `*.example.com` is free, and creates nothing yet. The card says **Ready for example.com**.

**Get install command** waits until Cloudflare is ready. Turn the switch off, or close the
section before it's ready, and the card says **Without Cloudflare Tunnel, your servers open ports
80 and 443.**

With Cloudflare ready, the install line starts your first server with ports 80 and 443 closed.
Keep the tab open while it joins: the tab holds your token, and Ployz never stores it. Once the
server joins, Ployz creates a tunnel named `ployz-<your organization>` and one `*.example.com`
record, opens the tunnel on the server, drops the token, and the Servers page says **Cloudflare
Tunnel is set up on example.com**. With no open ports to keep your apps answering meanwhile,
Ployz then switches your web traffic to the tunnel by itself, and a deployment that is already
running when it looks moves with it. Your apps get addresses like `web-production.example.com`. If that
switch stops, **Organization → Network** says why, and **Review and switch** finishes it.

If setup stops, the Servers page says why. **Try again** runs it with the same token. **Use
public ports instead** opens ports 80 and 443. A setup that fails once it has reached your server
opens the ports instead, so your apps answer, and says so. If you closed the tab before the server joined, the
Servers page says your apps aren't reachable yet: paste the token again to finish, or open the
ports. A server whose `ployz` predates this can't start closed, and adding it says to update
`ployz` or get an install command without Cloudflare Tunnel.

A Cluster remembers the doors you chose: `ployz server add --ingress-closed` and `ployz ingress use`
record them on the Cluster. A server joins with them whichever way you add it, and a server that
restarts doesn't start a tunnel connector the Cluster left closed. A server whose `ployz` is too old to follow
them can't join a Cluster that is closed or uses the tunnel, with or without the ports. Adding it says
to update `ployz` on it and run the join command again. A Cluster that never recorded its doors works
as it did: new servers copy the doors the servers already run. So does one whose entry server runs an
older `ployz`, when no server of the Cluster can record doors. When one can, `ployz ingress use` run
through the older server records the doors on that one and goes on.

Once a server has joined, the card is gone. Rename the Cluster and switch between public ports
and Cloudflare Tunnel in **Organization → Network**; see [Cloudflare Tunnel](../services/cloudflare-tunnel.md).

## Use an empty spare disk for volumes

To put managed volumes on a spare disk, name it when you add the server:

```sh
sudo ployz server add --storage zfs:/dev/disk/by-id/virtio-data
```

A by-id path stays stable when the kernel renames disks after a reboot. You can also
name a kernel path such as `zfs:/dev/vdb`. The disk must
be an empty whole disk. Ployz refuses a disk with a filesystem, a partition table,
partitions, or mounts. It creates the `ployz-device` pool on the selected disk.
Retrying installation on the same disk also imports an exported pool before checking
whether the disk is empty. After a reboot, a volume lookup, mount, or storage request
imports that pool again. Ployz records device ownership before serving volumes,
including when the operating system already imported the pool. If the device is
unavailable, storage operations fail instead of creating a replacement pool on the
main disk.

With `--storage zfs` and no device, Ployz prepares ZFS and creates a file-backed pool
on the main disk when you deploy the first managed volume. A failed named-device
attempt that creates no pool leaves this option available. Existing file-backed
storage takes priority over pools found on attached disks. If a previously owned
device pool is missing, preparation refuses instead of forgetting it. For installer-only use,
`ployzd install --storage zfs:/dev/vdb` accepts the same device form.

## Start without ZFS

The command sets up managed volumes with ZFS, so every [volume](../services/volumes.md) gets a
storage limit. ZFS needs:

- Ubuntu LTS with its stock kernel, Debian 12 or 13, or Amazon Linux 2023.
- Secure Boot off, on Debian and Amazon Linux.
- A VM or bare metal, not an OpenVZ or unprivileged LXC container.
- Free space on the main disk for your volumes.

On Ubuntu, ZFS comes ready-made. On Debian and Amazon Linux, the server builds it, so the command
takes longer: 15–20 minutes on a 2-vCPU Amazon Linux Server, a few minutes on Debian. The build
tools stay installed, so ZFS can be rebuilt when the kernel updates.

If your server can't run ZFS, add `--storage none` to the end of the install line, or run
`ployz server add --command --storage none` and the line it prints keeps it.

The server then shows **Docker only**. If none of your servers has managed volumes, every new volume,
a database's included, fails to deploy until you open it and tick **Use a plain Docker volume
(not recommended)** under **Advanced**; see
[When your servers show Docker only](../services/volumes.md#when-your-servers-show-docker-only).

## Open the firewall

Ployz opens the ports it needs on the server itself. If your provider runs a firewall in front of
it, like a Hetzner Cloud Firewall, a DigitalOcean Cloud Firewall or an AWS security group, allow
these there:

| Port | From | Needed for |
| --- | --- | --- |
| TCP 80, TCP and UDP 443 | Anywhere | Your apps over https, and their certificates, on servers that take web traffic. Not needed behind a [Cloudflare Tunnel](../services/cloudflare-tunnel.md) |
| UDP 51820 | Your other servers | Services on different servers reaching each other |

On a server that runs firewalld (`systemctl is-active firewalld` says `active`), also open UDP
51820 and trust `ployz-wg` in firewalld, as
[A server that runs firewalld doesn't answer your other servers](../troubleshooting/servers.md#a-server-that-runs-firewalld-doesnt-answer-your-other-servers)
shows.

## IPv6-only servers

Ployz downloads its releases from github.com, which has no IPv6 address. When a server can't reach
github.com, the download goes through ployz.sh instead, which has one. So on a server whose only
route out is IPv6, the install line still installs `ployz`, and setup and upgrades still download
the Ployz daemon. Setting the server up needs more: it pulls some images from ghcr.io, which has
no IPv6 address either, so enrolling the server or running `ployz server add` can't finish. Do one
of these first:

- **Give the server an IPv4 address.** Most providers sell one as an add-on, such as a Hetzner
  Primary IPv4. The server is then dual-stack and installs like any other.
- **Use NAT64 and DNS64.** DNS64 answers an IPv6 address for names that only have IPv4 ones, and
  NAT64 carries that traffic to IPv4. You need both, and they must agree on the prefix: a DNS64
  resolver alone, such as a public one, gives addresses nothing answers unless a NAT64 gateway
  serves its prefix. Use your provider's NAT64 and DNS64, or a public service that runs both.
  With both in place, the install line works and a single server sets up and runs. The server
  advertises only its own IPv6 addresses, never the gateway's IPv4 one, and your services
  resolve names through Ployz as on any other server. A container you start yourself with
  `docker run` on Docker's default network has only IPv4, so it reaches neither its resolvers
  nor anything else. Run it with `--network host`, or on a Docker network with IPv6 turned on.

With NAT64 and DNS64, your services reach the internet over IPv6. A container's IPv6 address is
private, so it lists a name's IPv4 addresses first, and the server can't reach those. Your app's
client must try each address, as curl and Python do. One that tries only the first, such as
BusyBox `wget`, fails.
A service that publishes a port on the server has only IPv4, so it can't reach the internet at
all. Give it a domain instead.

Keep the IPv4 address or NAT64 for as long as the server runs Ployz, because upgrades can pull new
images from ghcr.io.

## Good to know


- **Operating system updates stay with you.** Ployz doesn't install them.
- **Already running Docker?** Ployz keeps it and its settings, if it's Docker 29.2 or newer
  with buildx 0.18 or newer, the containerd image store turned on and live-restore on. Otherwise
  setup stops before changing anything; see
  [The install stops while checking Docker](../troubleshooting/servers.md#the-install-stops-while-checking-docker).
  Keep Docker's default iptables firewall. Ployz
  [keeps unsigned containers away from databases](../services/private-networking.md#who-can-reach-a-service-that-holds-a-volume)
  with rules in Docker's `DOCKER-USER` chain, so don't add your own iptables rules that accept
  traffic for the `ployz` bridge in `FORWARD` ahead of Docker's jump to it.
  Docker's default never rotates container logs, so they can fill the disk; see
  [The server's disk is filling up](../troubleshooting/servers.md#the-servers-disk-is-filling-up).
- **Which Caddy runs?** The one the server's Ployz release ships with. Upgrading Ployz on the
  server moves its web server to the new release's Caddy.
- **A server that never joined doesn't hold up the next one.** If a token was used but its server
  never joined, `ployz server ls` lists it as down with no version and warns that it hasn't
  joined and may still be joining. `ployz status` shows the same. Both name
  `ployz server forget <its ID> --confirm dead`. A join keeps going on the server after
  `ployz server add` stops waiting, so run it only once that server is deleted or won't join,
  from wherever you're signed in to the Organization. A Cloud add that fails before the server
  takes the join, for example over a policy that differs from the server's, prints that command
  after `undo:`. The next `ployz server add` doesn't wait on it: it warns `Server web-3 may still
  be joining` and points you to `ployz server ls`. A server whose add is still running looks the
  same, so check first: if it's still down with no version after a few minutes, take it out with
  the `ployz server forget <its ID> --confirm dead` the warning names. With `--json`, the result
  lists each such server under `unarrived`, with its `id`, `name` and that command as `next`; run
  it only on the same condition. You get the warning, the command and the list even when the add
  then fails for another reason, such as a down server.
- **Cloud refuses an enrollment with HTTP 409.** When Cloud marks its user-facing reply
  with a conflict code, the CLI shows its reason and next step, such as contacting support
  to move a Cluster connected to another Organization. It does not add advice to repeat
  the same command. For these coded conflicts, `--json` reports `conflict`.
  Generic conflicts and replies from older Cloud versions keep their existing messages,
  retry advice, and JSON codes.
- **A failed add can be retried with other options.** If `ployz server add` fails after Cloud took
  the server, for example while downloading or installing the daemon, run it again on the same
  host, with the same token while it lasts or a new one. It may change options such as
  `--storage`. The retry finishes the same server and keeps its address. After
  `ployz server forget`, the retry is refused and says how to add it with `--reset` and a new
  token; a server forgotten before it finished joining needs a fresh install first. Once the server has joined, a retry must match it; to add the host again
  with other options, run `ployz-uninstall` on it, remove the old server, and add it again.
- **Client IPs stay out of Ployz's web server logs, not your app's.** Caddy passes the client IP to
  your app in `X-Forwarded-For`, over IPv4 and IPv6. If your app logs requests, its logs, and Cloud's copy of
  them, can hold client IPs and user agents. Those logs are yours to manage.

Next: [manage your servers](manage-servers.md).
