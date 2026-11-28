---
title: Troubleshooting servers
description: Fix install failures, servers that don't join or go offline, and full disks.
---

Fixes for adding a server, keeping it connected and keeping its disk free. Commands with `sudo` run
on the server; `sudo journalctl -u ployz -n 200` shows Ployz's logs there.

## The command can't use sudo

You'll see `sudo: command not found` or `run this command with sudo`.

- **`sudo: command not found`**: you're root on a server without `sudo`. The CLI is already
  installed, so run the part after `&&` again without `sudo`, and paste the token when it asks:

  ```sh
  ployz server add
  ```

- **`run this command with sudo`**: keep the `sudo` the dashboard command includes, or run it as
  root.

## My server isn't supported

You'll see `Unsupported platform`, `Ployz requires systemd`, `unsupported Machine architecture` or
`Ployz Machine must be Linux`.

Use an amd64 (x86_64) or arm64 (aarch64) server running a Linux distribution with systemd.

## The install stops while preparing ZFS

The server can't set up managed volumes as it is. Find your message below for the fix. Either way,
you can add `--storage none` to the end of the command and run it again: the server shows **Docker
only**; see [Start without ZFS](../servers/add-a-server.md#start-without-zfs) for what that costs.

- ``Managed volumes need Ubuntu LTS, Debian 12–13 or Amazon Linux 2023; this Server runs ... Use one of those, or add `--storage none`.``

  The server runs another distribution, or another version of one of these. Reinstall it with a
  supported image.

- ``Secure Boot is on, so this Server can't load the ZFS module Ployz builds for ... Turn Secure Boot off, use Ubuntu, or add `--storage none`.``

  Turn Secure Boot off in your provider's settings for the server, or reinstall it with Ubuntu.
  Then run the command again.

- ``Amazon Linux 2023 has no kernel-devel package for the running kernel ..., so ZFS can't be built for it. Update the kernel, reboot, and retry, or add `--storage none`.``

  The server runs an older kernel than its package repositories offer. Update it, reboot, then
  run the command again:

  ```sh
  sudo dnf upgrade -y
  sudo reboot
  ```

- ``Couldn't build ZFS for kernel ... on ...: the OpenZFS checksum check failed. Add `--storage none` to start without managed volumes.``

  The ZFS source the server downloaded wasn't the release Ployz expects, so nothing was installed.
  Run the command again. If it fails the same way, something between the server and GitHub, like a
  proxy, changes downloads.

- ``Couldn't build ZFS for kernel ... on ...: ... failed. Add `--storage none` to start without managed volumes.``

  The build stopped at the step the message names. The output above the message shows why. Fix
  that and run the command again.

- `Ubuntu has no packaged ZFS module for the running kernel ...; install a supported Ubuntu kernel and retry`

  Switch the server to Ubuntu's stock kernel, reboot, and run the command again.

- `... apt-get is required for ZFS storage preparation`, or the same for `dnf`

  Reinstall the server with its provider's stock image.

- `OpenVZ does not allow this Machine to load the host ZFS kernel module`, or the same for
  unprivileged LXC

  Use a VM or bare metal.

- `Host root has ... bytes available; ZFS validation needs ...`

  Free disk space, or give the server a bigger disk. Ployz keeps a quarter of the disk, at least
  10 GiB, free for the system, so use a main disk of 30 GiB or more.

## The install stops while checking Docker

The server already has a Docker that Ployz can't use. Setup stops before it changes anything, so
fix Docker and run the command again. Or uninstall Docker, and Ployz installs a current one.

- `Docker is installed but not running. Start it (systemctl start docker) and run this again.`

  Run `sudo systemctl start docker`, then run the command again.

- `Docker ... is too old: Ployz needs Docker 29.2 or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one.`

  Upgrade Docker from the source you installed it from. A distribution's own Docker package, like
  Amazon Linux's `docker`, is often older: remove it with `sudo dnf remove docker` or
  `sudo apt-get remove docker.io`, then run the command again.

  Ployz 0.3.2 and earlier then fail to start the Docker they install: `restart Docker: Job for
  docker.service failed`. Run `sudo systemctl reset-failed docker.socket docker.service` and
  `sudo systemctl restart docker.socket docker.service`, then run the command again.

- `Docker Buildx ... is too old: Ployz needs Buildx 0.18 or newer. Upgrade Docker, or uninstall it and run this again so Ployz installs a current one.`

  Upgrade the buildx plugin along with Docker, or uninstall Docker.

- `Docker isn't using the containerd image store, which Ployz needs to build and move images between Servers. Enable it in /etc/docker/daemon.json ("features": {"containerd-snapshotter": true}) and restart Docker, or uninstall Docker and run this again.`

  Add `"features": {"containerd-snapshotter": true}` to `/etc/docker/daemon.json` and run
  `sudo systemctl restart docker`. Images you pulled before switching don't carry over, so pull
  them again.

- `Docker's live-restore is off, so your Services stay stopped if Docker crashes. Add "live-restore": true to /etc/docker/daemon.json and run sudo systemctl reload docker (containers keep running), then run this again. If this Server runs Docker swarm mode, leave swarm first: Docker won't start with both on. Or uninstall Docker and run this again.`

  Add `"live-restore": true` to `/etc/docker/daemon.json` and run `sudo systemctl reload docker`.
  A reload keeps running containers up. Check it took with
  `docker info -f '{{.LiveRestoreEnabled}}'`, which prints `true`. Older Docker refuses
  live-restore while swarm mode is on, so leave swarm first.

  A server that joined before this check can have live-restore off too. Run the same
  `docker info` command on it, and turn live-restore on the same way if it prints `false`.

## See what Ployz reads from a server

Once Ployz is installed, this prints what it reads from the server and what would stop it, without
changing anything:

```sh
sudo ployzd install --probe --json
```

`facts` holds the distribution, kernel, virtualization, cgroup version, CPUs, memory, disks and
Docker. `assessment.blockers` lists what stops Ployz, each with a `code` and a `message`, and
`assessment.warnings` lists what may misbehave. A root disk under 30 GiB shows as the
`disk_too_small` blocker, but the install doesn't refuse it yet. `assessment.zfs` is `Eligible`
with the disks managed volumes can use, or `Ineligible` with the reason the ZFS install stops on.
A value Ployz couldn't read shows as `null`, and `facts.unread` says why. When Ployz can't read
whether an LXC container is privileged, `assessment.zfs` is `Ineligible` with
`lxc_privilege_unknown`, and when it can't read the firmware's Secure Boot setting on Debian or
Amazon Linux, `Ineligible` with `secure_boot_unknown`. When a Docker command fails partway, the
`docker_unreadable` blocker carries the error the install would stop on.

The probe never starts Docker. When `docker.socket` listens but `docker.service` isn't running,
`facts.docker` is `SocketActivated` with no version, and `facts.unread` says why. That isn't a
blocker: the install's first Docker command starts it. If `docker.service` failed the last time
it ran, `facts.unread` says so, because only the install's start shows whether it fails again. A
Docker that doesn't answer shows as `docker_stopped`.

## Managed volumes are missing after a kernel update

After a Debian or Amazon Linux server reboots into a new kernel, its managed volumes are
unavailable and the services that use them don't start. Your volume data is still on the disk.

The server rebuilds ZFS for each kernel it installs, but that build failed, for example because
the kernel is newer than ZFS supports. Either:

- rebuild ZFS for the running kernel, then reboot:

  ```sh
  sudo dkms autoinstall
  sudo reboot
  ```

  If it fails, install the running kernel's headers first, then try again:

  ```sh
  # Debian
  sudo apt-get install linux-headers-$(uname -r)
  # Amazon Linux
  sudo dnf install "kernel-devel-uname-r = $(uname -r)"
  ```

- or boot the previous kernel, which stays installed. Choose it in the boot menu from your
  provider's console.

## The command was rejected

You'll see `Cloud no longer takes this enrollment token`.

The enrollment token expired or was mistyped, or the organization's servers were forgotten,
which voids every token made before. A token works for 24 hours. Run
`ployz server add --command` on your computer for a new one and run its line again.

## A server refused the command

You'll see `web-1 refused:` followed by the server's reason, such as that the command needs Full
access. The server answered, but this computer's access doesn't cover the command, so nothing
changed on it. When the server suggests a next step, the CLI prints it under the reason. When the
reason says the command needs Full access, approve this computer as a device. See
[Approve a device](../servers/manage-servers.md#approve-a-device). When it says
only Cloud manages slots, make the change from the dashboard instead. Before Ployz 0.3.4, the CLI
said such a server `did not answer`.

## Removing a server says Ployz Cloud can't read an environment

You'll see `Ployz Cloud can't read shop/production from the Config Store` from `ployz server rm`,
from **Remove server**, or when you move volumes off a server. Ployz Cloud holds settings for that
environment it can't read back, as happens when a self-hosted Cloud is rolled back below the
release that wrote them. Ployz can't
tell which of that environment's volumes are on the server, so it removes nothing rather than risk
losing their only copy. The **Remove server** list still names every other environment's volumes,
and lists that environment's volumes as **Unreadable volume**. If you rolled Ployz Cloud back,
roll it forward (see [Self-hosting](../self-hosting.md)), then try again. Otherwise, contact
support.

## The server's name is invalid

You'll see `invalid Machine Name "Web_1"`, with the closest valid name, `web-1`, below it.

A name must be one lowercase word: letters, digits and hyphens, up to 63 characters. Only a name
you pass with `--name` is refused; pass the suggested name and run the command again.

## The server is already initialised

You'll see `This Server is already initialised`, followed by advice to make a new token.

The server is already in a cluster, or partway through joining or leaving one. The token that add
used is now tied to the server as it is, and `--reset` gives the server a new identity, so that
token won't work after a reset. Add it again with `--reset` and a new token. Signed in, either run
`ployz server add --command --reset` and run the line it prints on the server, or run
`ployz server add root@HOST --reset` to add it over SSH, which makes its own token. The reset
removes every container Ployz runs on that server, so its apps go down, and it can delete its
mirrors, so don't use it while a mirror there may be the last copy of a volume.

## The first server is already in a Cluster

You'll see `This Server is already in a Cluster with other Servers, or something other than Ployz
Cloud manages it.` From Ployz 0.3.15, `ployz server add` may stop earlier on the server with
`this Server is already in a Cluster this enrollment didn't found`, or with `a new founding claim
requires an uninitialized Server`. When it also says `Ployz Cloud let go of this founding attempt`,
the token is used up and nothing else is left to reset.

An organization's first server starts a new Cluster, so it can't be a server that already has other
servers, or that something other than Ployz Cloud manages. The server stays in its Cluster. Don't run
another token on it as it is.

Run `ployz server add --command` for a new token, and add a server that isn't in a Cluster. If that
says `This Organization's Servers can't be reached`, go to **Organization**, click **Reset founding
attempt**, tick the confirmation and click **Reset enrollment**. The confirmation asks you to stop or
erase the old Machine, but this server can keep running in its Cluster: the reset revokes Ployz
Cloud's own access to it.

When the message is `a new founding claim requires an uninitialized Server`, you can add this
server itself instead, once Ployz Cloud let go of the founding attempt or you reset it: run
`ployz server add --command --reset` for a new token, and run the line it prints on the server. The
reset takes the server out of its Cluster and removes every container Ployz runs on it, so its apps
go down, and it can delete its mirrors, so don't use it while a mirror there may be the last copy
of a volume.

## A server belongs to another Cluster

If add says `Server MY-SERVER belongs to another Cluster`, the server is already in another
Cluster, so this add stops before changing it. [Remove it](../servers/manage-servers.md#remove-a-server)
from that Cluster first, then add it here with a new token: signed in, run `ployz server add --command`
and run the line it prints on the server. The token you used is tied to the server as it was, so
rerunning the same line is refused.

If you can't manage that Cluster, signed in here run `ployz server add --command --reset` and run
the line it prints on the server. The reset takes the server out of the other Cluster and removes
every container Ployz runs on it, and it can delete its mirrors, so don't use it while a mirror
there may be the last copy of a volume.

The reset can leave the server listed, as down, in the other Cluster:

- If the server couldn't remove itself from its own copy of the Cluster's records, the add warns
  `Server NAME (ID) reset, but couldn't confirm that its old Cluster removed it`, then gives the
  reason and the `ployz server forget ID --confirm dead` line for the other Cluster.
- If the server was cut off from the other Cluster while it reset, the add shows no warning, and
  the other Cluster keeps listing it.

Either way, whoever manages the other Cluster removes it there with
`ployz server forget <id> --confirm dead`. In that Cluster, `ployz server ls --json` shows each
server's id as `machine.id`.

The server may stay listed as down in the Organization you tried to add it to.
`ployz server ls --json` shows its id as `machine.id`; forget it by that id,
`ployz server forget <id> --confirm dead`, since a server you add again can share its name.

## A forgotten server is refused when you add it again

You'll see `Server web-2 already participates, but Cloud doesn't list it as joined`.

If an add of this server just stopped partway, run the same command again in a minute. If you
[removed](../servers/manage-servers.md#remove-a-server) or
[forgot](../servers/manage-servers.md#forget-a-server-that-is-gone) the server, it kept running, and
the token that add used is now tied to the server as it was before. `--reset` gives
the server a new identity, so that token won't work after a reset. Add it again with a new token.
Signed in, either run `ployz server add --command --reset` and run the line it prints on the
server, or run `ployz server add root@HOST --reset` to add it over SSH. The reset removes every
container Ployz runs on that server and can delete its mirrors, so don't use it while a mirror
there may be the last copy of a volume.

## The server's Cluster is connected to another Organization

Adding your organization's first server, `ployz server add` can stop with:

```text
error: This Server's Cluster is already connected to another Organization in Ployz Cloud. Contact Ployz support to move it.
```

If `ployz server add` was going to show you a recovery code, the first line is
`Recovery code not saved. Run ployz recovery rotate now to make one.` instead, with the same cause.
With an older Ployz Cloud, the CLI still wraps this sentence in
`Cloud enrollment completion failed; rerun the same ployz server add command without --reset (keep all other options).`

An older `ployz` doesn't show Ployz Cloud's sentence. Its cause reads
`enroll HTTP 409: enrollment completion rejected`, which it prints for other refusals too, so rerun
the command once. If it fails the same way again, the server's Cluster may be connected to another
Organization.

Ployz Cloud connects a Cluster to one Organization: the first one that added it. The server you're
adding belongs to a Cluster another Organization already added, so this Organization can't manage
it. Add a server that isn't in that Cluster instead, or manage it from the Organization that added
it. On ployz.dev, to move the Cluster to this Organization, or if you don't recognize the other one,
contact Ployz support. On a self-hosted Ployz Cloud, ask whoever runs it.

## The server doesn't finish joining

You'll see `the joined Server did not become ready`, or `the first Server did not become ready` on
your first server.

If the cause says `ployzd refused to start:`, Ployz stopped waiting as soon as it read the refusal,
over SSH or on the server itself. Go straight to step 3, then run the same command again.

If the wait printed `The Server's daemon is running but still joining`, or the error says the
daemon `stayed joining`, the server started but couldn't catch up with your other servers. Start
at step 2.

If it says `Cloud candidate publication failed`, the server started, but Ployz Cloud didn't see
your other servers list it as joined before the wait ran out. Start at step 1.

1. Run the same command again. A first start can take several minutes.
2. Make sure your servers reach each other on UDP 51820, in both directions, in the server's own
   firewall and in any firewall your provider runs in front of them. On a server that runs
   firewalld, follow
   [A server that runs firewalld doesn't answer your other servers](#a-server-that-runs-firewalld-doesnt-answer-your-other-servers).
3. Read the server's logs with `sudo journalctl -u ployz -n 200`, or the error's `cause:` line
   when the wait reached the server over SSH. Only if they say `refusing to
   change the retained WireGuard interface` or `refusing to replace the existing Docker network`,
   follow [Ployz stops over a leftover network](#ployz-stops-over-a-leftover-network). If they say
   `UDP port 51820 is in use`, follow
   [Another program holds UDP port 51820](#another-program-holds-udp-port-51820).

## A new server joined, but Global catch-up can't read another server

```text
Joined Server MY-SERVER.
error: Server joined, but Global catch-up is incomplete; it remains a Cluster member.
  cause: Global catch-up cannot plan from partial Service observations: web-1: Machine RPC payload failed: Could not decode the JSON payload: ...
```

The `ployz` that ran the join is older than web-1, and it can't read what web-1 runs. That's the
`ployz` where you ran `ployz server add`: the new server's own when you ran its install line
there, or your computer's. Usually web-1 runs a service with a
[sealed variable](../services/variables.md#seal-a-secret), and that `ployz` is 0.3.3 or earlier.
The message ends with `restore peer connectivity and redeploy`, but your network is fine. Releases
after 0.3.5 say `this ployz can't read web-1's answer, so update ployz on this host` instead.

The server has joined and stays joined. To finish:

1. Update `ployz` where the join ran to the release your other servers run, 0.3.4 or later. For
   the new server, [upgrade Ployz on it](../servers/manage-servers.md#upgrade-ployz-on-a-server).
2. Redeploy, so its global services start on it.

## Move a service off `deploy.mode: global`

A service deployed from a Compose file with `deploy.mode: global` before 0.1.2-beta.42 runs on
every server. Draining a server that runs it stops before it moves anything, and a deploy can't
switch it to a replica count (`service mode cannot be changed`). A drain that finds it only after
turning the server's services off says so in its error, and leaves them off: turn **Run services
here** back on, or run `ployz server set SERVER --accepts-services=true`. To run it like your other
services, remove it and add it back:

1. If its image keeps data in a volume, find that volume first. `ployz ps SERVICE` lists each of
   its containers and the server it runs on. On that server, this prints each volume's name and
   the path it holds in the container:

   ```sh
   docker inspect --format '{{range .Mounts}}{{.Name}} {{.Destination}}{{println}}{{end}}' CONTAINER
   ```

2. Remove the service with `ployz service rm SERVICE`, or delete it on the canvas, and deploy the
   whole environment. When your environment doesn't hold the service, that deploy removes it
   anyway, as it does any service running in the environment that the environment doesn't hold.
   Its containers go. Volumes its image created, which no volume in Ployz names, stay on each
   server. The service is down from here until step 3's deploy is healthy.
3. Add the service back with a replica count and deploy. Its new containers start with empty
   volumes.
4. To carry the data over, copy it from the old volume into a new container. On the old server
   the files are in `/var/lib/docker/volumes/NAME/_data`. Copy them to the server `ployz ps`
   now shows for the new container, then copy them in and restart the service:

   ```sh
   docker cp ./data/. CONTAINER:/path/in/the/container
   ```

   Data in a volume Ployz doesn't name goes when a later deploy replaces the container. To keep
   it, [add a volume](../services/volumes.md#add-a-volume) at that path and copy into it instead.
5. Once the data is safe, remove each old volume on its server with `docker volume rm NAME`.

## A server that waited with a Fleet Token never joins

`ployz status` on the server, or `sudo journalctl -u ployz-checkin`, says `Token revoked. Ployz
Cloud no longer adds Servers from a Fleet Token…`.

Ployz Cloud no longer adds servers from a Fleet Token, so a server that was waiting with one
won't join on its own. Its check-in service is already turned off, and `/etc/ployz/fleet-token` is
gone.

1. Get an install command for it: click **Add server** on the **Servers** page and **Get install
   command**, or run `ployz server add --command` where you're signed in.
2. Run the line on the server and paste the token when it asks. See
   [Add a server](../servers/add-a-server.md).

To take Ployz off the server instead, run `sudo ployz-uninstall`. It removes what's left of the
Fleet Token install too.

## Another program holds UDP port 51820

`sudo journalctl -u ployz -n 50` says `UDP port 51820 is in use by avahi-daemon (pid 812), so
WireGuard can't listen`, or `by another program` when it can't tell which, and Ployz keeps
restarting. Ployz needs that port for WireGuard between servers, so it can't start on the server
while another program holds it.

See what holds it with `sudo ss -ulpn 'sport = :51820'`, then stop or restart that program.
avahi-daemon, for one, can take the port at random and lets it go when restarted. Ployz starts on
its next restart.

When `ss` lists the port with no program, another WireGuard interface, such as a tunnel set up with
`wg-quick`, probably holds it. `sudo wg show all listen-port` names the interface. Move that
tunnel to another port, or take it down.

## Enroll refuses over an earlier enrollment's network

You'll see `refusing to enroll over an earlier enrollment's network`. The server's Ployz state was
wiped, but the `ployz-wg` interface or the `ployz` Docker network from its earlier enrollment is
still there. Enroll stops before it changes anything. The message names each leftover, including
the containers still attached to the network, and ends with the commands that clear them all.
Removing those containers stops them, and their volume data stays on the disk. Run the commands,
then run the enroll command again. [After deleting /var/lib/ployz](#after-deleting-varlibployz)
covers the old identity left in your server list.

## Ployz stops over a leftover network

`sudo systemctl status ployz` shows the service failed with `status=78`, and its logs say
`refusing to change the retained WireGuard interface` or `refusing to replace the existing Docker
network`. A `ployz` command that connects to that server over SSH says the same, after
`ployzd refused to start:`. A command that goes through another server only says the server did not
answer. Something else, or an earlier install, left a network Ployz can't prove is its own, so
Ployz stops rather than take it over. It won't start again until you clear it. The log line ends
with the safe recovery for your case.

For a leftover `ployz-wg` interface:

```sh
sudo systemctl stop ployz.socket ployz
sudo ip link delete ployz-wg
sudo systemctl reset-failed ployz.socket ployz
sudo systemctl start ployz
```

For a Docker network named `ployz`, stop Ployz the same way. Run `docker network inspect ployz`
and find its owner from its labels and attached containers. Move or remove those containers through
whatever deployed them. Once the network is empty and nothing needs it, run
`docker network rm ployz`, then the last two commands above.

A server can have both. Clear them together, because Ployz names only the first one it meets on
each start.

### After deleting /var/lib/ployz

`/var/lib/ployz` holds the server's identity, and the `ployz-wg` interface and `ployz` Docker
network belong to that identity. Deleting the folder doesn't remove them, so the server's next
enrollment refuses over them. If you delete `/var/lib/ployz`, clear them too, before you add the
server again:

```sh
sudo systemctl stop ployz.socket ployz
sudo ip link delete ployz-wg
docker network inspect ployz --format '{{range .Containers}}{{.Name}} {{end}}'
docker rm -f MY-CONTAINER ...
docker network rm ployz
sudo systemctl reset-failed ployz.socket ployz
sudo systemctl start ployz.socket
```

The containers that `docker network inspect` lists are the apps and the ingress proxy the server ran
before. Removing them stops those apps; their volume data stays on the disk.

Your server list still shows the old identity as a second row with the same name, so the join can
end with `Global catch-up is incomplete` and `MY-SERVER: no terminal response`. The server has
joined all the same. Forget the old row by its ID, which `ployz --json server ls` shows, as in
[Forget a server that is gone](../servers/manage-servers.md#forget-a-server-that-is-gone). Then
redeploy, so your apps and the ingress proxy start on the server again.

## My first server never finished joining

You'll see `This Organization's Servers can't be reached` when you add another server, and an
alert on the **Organization** page. The alert also shows while your first server installs, so give
it a few minutes first.

No other server can join until the first one finishes.

1. If the first server still exists, run its command on it again. If its token expired, run
   `ployz server add --command` for a new one; it picks up where the server left off.
2. If that server is gone, or you want to start over, make sure it's stopped or erased at your
   provider. Then go to **Organization**, click **Reset founding attempt**, tick **I confirm the
   old Machine has been stopped or erased.** and click **Reset enrollment**. Tokens you made for
   other servers keep working. If the reset says to use Forget Servers, go to **Organization** and,
   under **Danger**, click **Forget servers**, or run
   `ployz server forget --confirm <your organization's slug>` where you're signed in. Forget
   deletes every enrollment token of your organization, so make a new one for each server, with
   `ployz server add --command` or **Add server**.
3. Add a server again.

If your first server did finish joining, there's no founding to reset, and the CLI says so. To
start over, stop or erase your servers, then forget them as in step 2.

## Can't reach your servers right now

The **Servers** page shows **Can't reach your servers right now**, with what your servers last
reported or nothing at all. Adding a server can fail with `This Organization's Servers can't be
reached`. A CLI command, or a deployment that couldn't run, says `Server web-1 isn't answering`,
naming each server it couldn't reach by the name it last reported, or by its ID if Ployz never
learned one. When the CLI asked through Ployz Cloud, it says `Server web-1 isn't answering
through Ployz Cloud` and asks you to check that the server is online, its Ployz daemon is running,
and its outgoing https isn't filtered: any of the three looks the same from the CLI. When a server
answered but refused every connection, the error names the server tried last, as in
`Server web-2, tried last, refused the connection`. When its connection finds no
server answering, `ployz status` lists `Server web-1 isn't answering` under **needs attention**, and
`ployz deploy --plan` warns first that the deploy can't run until it answers. A deploy whose web
traffic check couldn't reach a server warns `Couldn't check ingress: Server web-1 isn't answering`, with
what the connection said in parentheses, and exits 3. A server that stopped answering because
`ployz server rm` was removing it doesn't count: the removal says so in the server's own record
before it stops answering (Ployz 0.3.4 or later on that server and on the server the CLI connects
through). Any deploy step a server leaves
unanswered reads `Server web-1 did not answer during create Container`, with what the connection
said beneath it. A server that just joined or restarted
answers within a minute; try again then. While your organization's first server is still
enrolling, the page shows its enrollment progress instead: there's no server to reach until it
joins.

1. Check at your provider that the servers are running.
2. On a server, run `sudo systemctl status ployz`. If Ployz is stopped, start it with
   `sudo systemctl start ployz`, and read its logs with `sudo journalctl -u ployz -n 200`.
3. If your servers' outgoing traffic is filtered, allow https to `relay.ployz.dev`. When Cloud
   can't reach your first server, adding it fails with `Cloud can't reach this Server through the
   relay or directly`. Allow https to `relay.ployz.dev` from the server, and from Cloud if you host
   it yourself, then try again.
4. If you deleted the servers, see
   [Forget servers you deleted](../servers/manage-servers.md#forget-servers-you-deleted).
5. If the servers run Ployz 0.2 or 0.3.0-beta.6 and earlier, Cloud can't reach them through the relay.
   Upgrade one over SSH, then the rest from Cloud, as in
   [Servers on 0.2 or 0.3.0-beta.6 and earlier](../servers/manage-servers.md#servers-on-02-or-030-beta6-and-earlier).
   A server on 0.3.2 or earlier is used only through a newer one. Upgrade it as in
   [Servers on 0.3.2 and earlier](../servers/manage-servers.md#servers-on-032-and-earlier); it is
   used once a server that holds your cluster's key is back online.
6. If the page reads **Waiting for a server that holds your cluster's key**, a server answers but
   doesn't hold your cluster's key yet. It's used once a key holder is back online. Keep it running,
   and don't forget it. If the page stays this way while a server that holds the key is online,
   check the daemon logs on the server that answers: `sudo journalctl -u ployz -n 200`.

## The CLI says a server runs 0.2 or 0.3.0-beta.6 and earlier

```text
This Server runs Ployz 0.2 or a 0.3.0 beta up to beta.6, which this release can't reach through the relay.
```

Upgrade one server over SSH, then the rest from Cloud, as in
[Servers on 0.2 or 0.3.0-beta.6 and earlier](../servers/manage-servers.md#servers-on-02-or-030-beta6-and-earlier).

A server that answers but refuses says so instead, with its own reason:
`Server web-1 couldn't give this command access: Cluster store is not available`. It is running, so
don't forget it. Read why in its logs with `sudo journalctl -u ployz -n 200`.

`error: Corrosion HTTP request failed`, with the cause `Connection refused`, means the server
answered but its cluster store wasn't running. If it was stopped, as by
`docker stop ployz-corrosion`, Ployz starts it again within a few seconds, including while the
daemon is still starting; try again then.

## A server's cluster store was lost

```text
error: this Server's Cluster store holds no other Server: it was recreated and can't reach them yet
```

A server keeps the cluster's shared state in `/var/lib/ployz/corrosion/store.db`. If that file is
lost, for example after a power loss or a manual wipe, Ployz starts an empty one. The server keeps
its tunnels to the other servers, and the empty store refills from them, usually within a minute.
Until then, commands through that server fail with this error. Commands through another server
still work.

1. Wait a minute and try again.
2. If it persists, run `ployz server ls` through another server. If this one shows `down` there,
   make sure UDP 51820 is open between your servers, then restart Ployz on it with
   `sudo systemctl restart ployz`.

If you still have the old `store.db`, you don't need it back: the refilled store holds the same
state.

## A server shows Offline

The server's page says **Can't reach web-2**. Services that run only there are down.

Ployz Cloud also shows a server Offline within about ten seconds of Ployz stopping on it, for
example after `sudo systemctl stop ployz.socket ployz`, because nothing there answers for what it
runs. Its containers read **Server offline** rather than Crashed, since what they last reported may
no longer be true. Start Ployz again and the server reads online. Such a stop doesn't remove the
server: it keeps its place among your servers until it reboots or you remove it.

1. Check at your provider that the server is running, then run `sudo systemctl status ployz` on
   it.
2. Make sure UDP 51820 is open between your servers.
3. If the server is gone for good, [forget it](../servers/manage-servers.md#forget-a-server-that-is-gone),
   then deploy again. Its page offers **Forget server** while it's offline.

   See [When a server goes down](../services/scaling.md#when-a-server-goes-down) for which
   services come back, and [When its server goes down](../services/volumes.md#when-its-server-goes-down)
   to bring a volume back from its mirror with **Use mirror** before you deploy.

## A server's address changed

Ployz reads a server's public IP and network addresses again each time Ployz starts on it. When a
server comes back from a stop with a new address, the other servers learn it, and it shows in
`ployz server ls`. If the public IP lookup fails, the server keeps its old address and logs a
warning; restart Ployz on it with `sudo systemctl restart ployz` to try again.

An address you chose yourself never changes on its own. That means `--public-ip` or `--wg-endpoint`
given to `ployz server add` or `ployz server set`. To change it, run
`ployz server set` again.

A server on 0.3.0-beta.6 or earlier keeps the public IP and network addresses it has when you
upgrade it to a release after 0.3.0-beta.7. Those older releases didn't record which addresses you
chose, so Ployz treats both as chosen. From then on the server doesn't pick up a new address on its
own: after its IP changes, run `ployz server set`. Only resetting the server goes back to automatic
addresses. A server already upgraded to 0.3.0-beta.7 may have swapped an address you chose for one
it found; set yours again with `ployz server set`.

If every server's address changed at once, the servers can't reach each other to share the new
addresses, and each one shows the others as unreachable. [Tell them where one of them
is](#servers-lost-each-other-after-their-ips-changed).

## Servers lost each other after their IPs changed

Every server got a new public IP address at once, for example after a provider moved or restarted
them. Each server shows itself running and the others as not answering, in `ployz server ls` and in
Ployz Cloud, because each one still dials the others at their old addresses.

Tell the servers where one of them is now. Run this once, with that server's name and its new
public IP address:

```sh
ployz server set web-2 --wg-endpoint 203.0.113.20
```

Every other server the command reaches starts dialing `web-2` at that address right away; WireGuard
uses port 51820 unless you name another, as in `203.0.113.20:51821`. Once one of them reaches
`web-2`, `web-2` answers from its own new address and the two find each other, usually within
seconds. The command says which servers took the address, then saves it on `web-2`:

```text
web-1 now dials web-2 at 203.0.113.20:51820.
Updated Server web-2.
```

If `web-2` still can't be reached, the second line reads `! web-2 didn't answer, so its record may
or may not have changed.` and the command still succeeds. Run it again once both servers show up in `ployz server
ls` to save the address on `web-2` too. With more than two servers, repeat the command for any
server that still shows as not answering.

Run it from your computer with Ployz Cloud, or on one of the other servers itself. Run on `web-2`, it only saves the address on `web-2`.

## Large packets to a server are dropped

You'll see `! Large packets to web-2 are dropped on its mesh path from the Server that answered, so
calls with large replies to web-2 fail.` in `ployz server ls`. Small requests still reach `web-2`, so
it answers pings, but its rows may read `Volume support unknown` or `did not answer`. A deploy says
`! Server web-2: deployment observations are incomplete (containers: target Machine RPC timed out;
large packets to web-2 are dropped on its mesh path from the Server this command entered by)` and
leaves `web-2` as it is. From 0.3.8 it deploys on the other servers and exits 3, as when a server
is down; the replicas on `web-2` keep running until a deploy after the path heals replaces them.
Before 0.3.8, every deploy that started a replica failed on every server, with
`list Containers failed`, `Machine web-2 did not answer`.

While the path stays broken, the confirmations from `web-2`'s proxy that a deploy waits for may stop
arriving. A server that reads `down` no longer counts toward that wait. Otherwise, after 5 minutes,
the deploy fails with `proxy on web-2 did not confirm; large packets to web-2 are dropped on its mesh
path from the Server this command entered by`.

`ployz server show web-2` still shows it. When `web-2` doesn't answer through the server the command
entered by within 3 seconds, the CLI reads it over its own connection and prints `! web-2 did not
answer through the Server this command entered by, so it was read over its own connection.` This
works signed in to Ployz Cloud, or with a context that lists `web-2`. Without a connection of its
own, the command fails with `large packets to web-2 are dropped on the mesh path from the Server
that routed this call`, or `target Machine RPC timed out` if it gives up first; the warning above
still names the cause.

Something between your servers, often a provider's network or a VPN with a smaller packet size,
drops packets above a size. From 0.3.7, each server checks every minute that a full-size packet
reaches each other server. When it doesn't, the server finds the largest size that does and sends
to that server in packets of that size, so nothing fails. `ployz server show` on that server then
lists the peer with its smaller size, as in `peer = web-2 0.4 ms, mtu 1310`; a peer at full size
shows no size. The server tries full
size again every 10 minutes and goes back to it once the path carries it, logging
`the mesh path to web-2 carries 1420-byte packets again`.

The warning above means even 1280-byte packets, the smallest IPv6 allows, are lost, so Ployz can't
work around it. Ask your provider why UDP packets on port 51820 above that size are dropped between
the two servers, or move one of them. Once full-size packets cross again, the warning goes away
within a minute. A server on 0.3.6 or earlier doesn't check, so it can't name the cause; a call
routed through it says `target Machine RPC timed out` instead.

## The server a command entered by can't reach another

You'll see a command through `web-1` fail or come back partial with a line like this, where
`web-1` is the server the command entered by:

```text
web-1, the Server this command entered by, has had no WireGuard handshake with web-3 for 6m.
```

or `has never had a WireGuard handshake with web-3` for a server that just joined. `ployz server
set`, `ployz server show` and `ployz server add` say it in their error. `ployz server ls` says it
as a warning of its own and still exits 0, so check its warnings, not only its exit code.

A server that is powered off gets the same line and the same advice to allow UDP 51820, because
its WireGuard stops answering too. If `web-3` is meant to be off, power it on again, or remove it for
good with `ployz server forget web-3 --confirm dead`. A server whose `ployz` daemon alone has stopped
keeps its WireGuard handshake, so it doesn't get this line.

With a third server, `web-3` can still read `up`: the servers' shared state travels through the
third server, but commands from `web-1` to `web-3` can't. Then `ployz server ls` says it under
`! web-3 did not answer; its rows are missing.`, and `ployz server show web-1` lists the peer as
`peer = web-3 down, no WireGuard handshake for 6m` instead of a round trip. With only two servers,
`web-3` reads `down`, and `ployz server show web-1` has no line for it.

1. Allow UDP 51820 between `web-1` and `web-3`, in both directions, in each server's own firewall
   and in any firewall your provider runs in front of them.
2. Until then, run the command through another server with `--connect`.
3. For a server that just joined, run `ployz deploy` once the two handshake, so its global
   services start on it.

Before 0.4.0 these commands said only that `web-3` didn't answer.

## Services on other servers stop answering after a firewall change

Ployz keeps its own iptables rules first in `INPUT` and Docker's `DOCKER-USER` chain. A tool that
later inserts rules above them or flushes them, such as ufw-docker, `firewall-cmd --reload` or a
security agent, can cut services off from other servers. Ployz applies its rules again every
minute, so traffic returns within a minute with no restart.

When a rule Ployz keeps first was missing or no longer first, `journalctl -u ployz` names it, as in
`restored firewall rules that were missing or no longer first: filter DOCKER-USER: -i ployz-wg -o ployz -j ACCEPT`.
The line also names the TCP clamp on `ployz-wg` when it was missing. It doesn't name rules inside
Ployz's own `PLOYZ-` chains, which Ployz rewrites whole each minute, or its NAT rule, which Docker
moves down every time it creates a network or restarts. When the line repeats every minute,
something keeps rewriting the firewall. Find it and have it add its rules below Ployz's.

If iptables fails, for example because another program holds its lock for more than 10 seconds,
the log says `failed to reapply firewall rules` with iptables' reason, and Ployz tries again a
minute later. Ployz 0.3.16 and earlier put the rules back only when the server restarts.

## A server that runs firewalld doesn't answer your other servers

A command or deploy says `! web-2 did not answer; its rows are missing.` with
`cause: Permission denied (os error 13)`, and a deploy leaves an old replica running on another
server. Check whether firewalld runs on that server:

```sh
systemctl is-active firewalld
```

If it says `active`, firewalld refuses the calls your other servers make to Ployz on that server,
even though Ployz opens its ports in iptables. List its zones, and note the one shown above its
public interface, such as `eth0`:

```sh
sudo firewall-cmd --get-active-zones
```

Then open UDP 51820 in that zone, shown here as `public`, and trust `ployz-wg`, the interface your
servers reach each other on:

```sh
sudo firewall-cmd --zone=public --permanent --add-port=51820/udp
sudo firewall-cmd --zone=trusted --permanent --add-interface=ployz-wg
sudo firewall-cmd --reload
```

`ployz-wg` only carries traffic from your own servers and the apps on them. Trusting it lets them
reach anything listening on this server's private address, as they could without firewalld, while
Ployz keeps its own ports closed to everything else. On some older systems the reload also clears
Ployz's own iptables rules; Ployz puts them back within a minute, as
[Services on other servers stop answering after a firewall change](#services-on-other-servers-stop-answering-after-a-firewall-change)
describes. On Ployz 0.3.16 and earlier, run `sudo systemctl restart ployz` after the reload
instead.

## Ployz stops because Docker uses its nftables firewall

`journalctl -u ployz` says `Docker doesn't pass forwarded IPv4 through iptables' DOCKER-USER chain`
and Ployz doesn't start its network. Ployz keeps unsigned containers away from services that hold
a volume with iptables rules that Docker runs only with its default iptables firewall. The line
names the setting that turns it off when Ployz finds one, such as
`/etc/docker/daemon.json sets "firewall-backend": "nftables"` or
`dockerd runs with --iptables=false`. Remove it, then run:

```sh
sudo systemctl restart docker
sudo systemctl restart ployz
```

## Virtual machines on another bridge lose their network after you install Ployz

Ployz turns on the Linux setting that sends traffic between containers on one bridge through the
firewall, so it can keep unsigned containers away from services that hold a volume. The setting
covers every bridge on the server, so traffic on another bridge, such as KVM's `br0` or LXC's
`lxcbr0`, now meets Docker's firewall, which drops forwarded traffic it doesn't know. Allow that
bridge, as Docker's own documentation does:

```sh
sudo iptables -I DOCKER-USER -i br0 -o br0 -j ACCEPT
```

Docker rebuilds its chains when it restarts, so run the same command from your firewall's startup
to keep it after a reboot.

## Apps see IPv6 visitors as 10.210.x.1, or services can't reach IPv6 addresses

Through a domain, your app's `X-Forwarded-For` should show each visitor's own address, and a
service that publishes no port on the server should reach IPv6 addresses. If `X-Forwarded-For`
shows your server's private gateway, such as `10.210.0.1`, for visitors over IPv6, or a container
Ployz 0.3.6 or later created for a service that publishes no port has no IPv6 address, the
server's container network has no IPv6. `journalctl -u ployz` says why:

- `learn IPv6 from router advertisements`: the server takes its IPv6 route or address from router
  advertisements, which IPv6 forwarding would stop. Run the `sysctl -w` command the log names, add
  the same setting to a file in `/etc/sysctl.d/` so it survives a reboot, then run
  `sudo systemctl restart ployz`.
- `/etc/docker/daemon.json sets "ip6tables": false` or `dockerd runs with --ip6tables=false`:
  Ployz needs Docker's default iptables firewall with `ip6tables` on. The line names the setting
  and where it found it, `daemon.json` or Docker's command line. Remove it there, restart Docker,
  then run `sudo systemctl restart ployz`. `"iptables": false` and `"firewall-backend":
  "nftables"` read the same way. Restarting Docker is enough, with no reboot.
- `/etc/docker/daemon.json changed at …, after dockerd started at …`: Ployz can't tell what the
  running Docker loaded, so the network stays IPv4-only until Docker restarts. With no
  `daemon.json`, it reads `/etc/docker changed at … and holds no daemon.json`, as after deleting
  the file. Run `sudo systemctl restart docker`, then
  `sudo systemctl restart ployz`. `systemctl reload docker` isn't enough. Other lines name what
  Ployz couldn't read, such as `daemon.json` itself, Docker's pidfile, or more than one dockerd,
  and the step that clears it.
- `the IPv6 mesh fence failed`: the log line gives ip6tables' reason. Ployz tries again at its
  next start.
- `could not take IPv6`: the log line gives Docker's reason. Ployz tries again at its next start.

A port a service publishes on the server always shows IPv6 visitors as the gateway, and a service
that publishes one has no IPv6 address. Give the service a domain instead to see visitors'
addresses and reach IPv6 addresses.

When the services have an IPv6 address but can't reach IPv6 addresses, check the server itself:
`curl -6 https://example.com` on it fails when the server has no IPv6 route out. Its container
network can still take IPv6, but nothing carries it further. Ask your provider for IPv6. Until
then your services keep reaching IPv4 addresses.

When only some services can't reach IPv6 addresses, check whether each has an IPv6 address:
`ployz exec SERVICE -- cat /proc/net/if_inet6` lists one starting `fdcd` when it does, if the
image has `cat`. A container created before 0.3.6 has none until Ployz replaces it. A deploy that changes the image or a variable replaces it. A deploy that changes nothing, or
only CPU or memory limits, keeps it.

## Services can't resolve each other's names

A service on the server gets `NXDOMAIN` or a timeout for a name like `postgres.app.internal`,
while `ployz service ls` shows the target running.

1. Run `sudo systemctl status ployz-dns`. Its Status line says `serving`, `loading`, or
   `waiting for the Corrosion token` while it works. `idle` means it has no names to answer.
   Either the server isn't in an organization yet, or it couldn't read its list of names. In
   the second case its logs say why, and it starts answering once the list is readable.
2. If the Status line says `waiting for port 53 on 10.210.0.1` (your server's private address),
   another program holds that port, often `dnsmasq` from libvirt or a local DNS cache. Find it
   with `sudo ss -lntup 'sport = :53'`, then stop it or make it listen only on other addresses.
   Private DNS starts answering on its own once the port is free.
3. Read its logs with `sudo journalctl -u ployz-dns -n 200`.
4. If the unit is missing, run `sudo systemctl restart ployz`. Ployz recreates it.

## The bottom bar shows Add server instead of Deploy

Your organization has no server, so nothing can deploy.

[Add a server](../servers/add-a-server.md): run `ployz server add --command`, or click **Add
server** and **Get install command**. Your projects and settings are kept. Once it joins,
**Deploy** is back and Ployz deploys your environments to it. Staged changes stay in the bottom
bar.

## The server's disk is filling up

See what uses the space:

```sh
df -h /
sudo docker system df
```

- **Build cache.** Run `sudo ployz server build-cache-clear` on the server. It keeps your images
  and refuses while a build runs.
- **Images from a registry**, like an old `postgres` version, stay until you remove them with
  `sudo docker image rm`. Ployz cleans up the images it builds on its own.
- **Container logs.** Ployz sets each container's log settings itself: Docker keeps up to three
  10 MB files per container and rotates them. `log-opts` in `/etc/docker/daemon.json` doesn't
  change them. A container started before your server ran this release keeps its old settings
  until its next deploy.
- **Plain Docker volumes** have no size limit, and `docker system df` above counts them.
- **Volumes kept from removed containers.** When a drain, a lower replica count or deleting a
  service removes a container whose image made its own volume, like `postgres`, the server keeps
  that volume and its data. `ployz server volumes <server>` lists them with their sizes, and
  `ployz server volumes <server> prune` deletes the ones no container uses after asking. See
  [Delete the volumes a server kept](../services/services.md#delete-the-volumes-a-server-kept).
- **Kept metrics.** Ployz keeps the server's metrics in `ployz-observe` under Docker's directory.
  `docker system df` doesn't count them; `sudo du -sh /var/lib/docker/ployz-observe` does. They're
  capped at 5% of the disk, kept between 512 MB and 5 GB. When free space drops below 10% of the
  disk (at least 2 GB), the oldest metrics go first until it's back. The server keeps no logs of
  its own beyond Docker's: a container's output ends with the container.

## A deploy says there isn't enough disk space

You'll see `Not enough disk space: about ... more free space is needed`.

A managed volume needs its whole storage limit free on the server, on top of the quarter of the
disk Ployz keeps for the system. Free disk space, give the server a bigger disk, or lower the
volume's storage limit before you deploy it. See [Volumes](../services/volumes.md).

## Related

- [Add a server](../servers/add-a-server.md)
- [Manage servers](../servers/manage-servers.md)
- [My app isn't reachable](app-not-reachable.md): error pages, ports, domains and certificates
