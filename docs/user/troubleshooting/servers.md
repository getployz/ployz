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

## The server's name is invalid

You'll see `invalid Machine Name "Web_1"`, with the closest valid name, `web-1`, below it.

A name must be one lowercase word: letters, digits and hyphens, up to 63 characters. Only a name
you pass with `--name` is refused; pass the suggested name and run the command again.

## The server is already initialised

You'll see `This Server is already initialised; rerun with --reset to reset it before enrollment`.

The server belongs to an organization, or was never fully removed from one. Add `--reset` to the
end of the command and run it again. This removes every container Ployz runs on that server, so
its apps go down; volume data stays on the disk.

## The server doesn't finish joining

You'll see `the joined Server did not become ready`, or `the first Server did not become ready` on
your first server.

1. Run the same command again. A first start can take several minutes.
2. Make sure your servers reach each other on UDP 51820, in any firewall your provider runs in
   front of them.
3. Read the server's logs with `sudo journalctl -u ployz -n 200`. Only if they say `refusing to
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
new server's own `ployz` when it joined with a Fleet Token, or the computer where you ran
`ployz server add`. Usually web-1 runs a service with a
[sealed variable](../services/variables.md#seal-a-secret), and that `ployz` is 0.3.3 or earlier.
The message ends with `restore peer connectivity and redeploy`, but your network is fine. Releases
after 0.3.5 say `this ployz can't read web-1's answer, so update ployz on this host` instead.

The server has joined and stays joined. To finish:

1. Update `ployz` where the join ran to the release your other servers run, 0.3.4 or later. For
   the new server, [upgrade Ployz on it](../servers/manage-servers.md#upgrade-ployz-on-a-server).
2. Redeploy, so its global services start on it.

## A server waits for approval and never shows up

A server you started with a Fleet Token runs `ployz server checkin --install`. That installs a
service, `ployz-checkin`, which checks in with Ployz Cloud until you approve the server, and
prepares Docker and ZFS meanwhile.

1. Run `ployz status` on the server. The **this server** line says where it stands, and `blocked`
   lines say what stops it.
2. Read the loop's log with `sudo journalctl -u ployz-checkin -n 100`, and preparation's with
   `sudo journalctl -u ployz-prepare -n 200`. A failed preparation is retried 10 minutes later,
   up to three times. Once you've fixed the cause, `sudo ployz server checkin --install` starts it
   again right away. Preparation that stopped mid-step on every try reads as failed.
3. `Check-in stopped` means the loop isn't running, so nothing checks in and, a few minutes
   later, Ployz Cloud shows the server Offline. `last answer` is what it saw before it stopped,
   and `checked in` says when. `sudo systemctl status ployz-checkin` says why it stopped, and
   `sudo ployz server checkin --install` starts it again. Run `ployz status` with `sudo`: only
   root can tell whether the loop runs.
4. `Can't reach Cloud` means the server can't reach Ployz Cloud over HTTPS, or something in
   between, such as a proxy, answered instead. It keeps trying every minute.
5. `Clock is off` means Ployz Cloud refused the check-in and the server's clock is more than 4
   minutes away from Cloud's, close to the 5 minutes Cloud allows, so the token isn't to blame.
   The `reason` line says by how much, and says too when Cloud refused the token itself: a token
   refusal from a server whose clock is that far off doesn't count against the token until the
   clock is right. Set the clock right, for example with
   `timedatectl set-ntp true`, and the running loop checks in again within a minute. If the
   install itself said so, it started nothing: fix the clock and run the install again.
6. `Not Ployz Cloud` means the check-in URL reaches something that answers but isn't Ployz
   Cloud, such as a 404 or a web page. That can be a wrong `PLOYZ_CLOUD_URL`, or a proxy or
   firewall in between. The `reason` line names the URL. The loop tries again every minute. Point
   the URL at your Ployz Cloud, or let the server through, and run the install again.
7. `Held by Cloud` means Ployz Cloud answered but held the check-in back, and the `reason` line
   gives Cloud's message. `This Fleet Token already has N waiting servers.` means the token has
   as many waiting servers as it allows. Rejected servers count too, so rejecting them frees
   nothing, and a server you add frees its place once Cloud's cleanup runs, at least a week after
   it joins. Run the install again with another token, or revoke the full one: that removes its
   waiting servers that haven't started joining and stops them checking in. Servers already
   joining finish, and joined servers stay. Otherwise Cloud is busy, and the loop asks again when
   Cloud says to.
8. `Needs a newer ployz` means Ployz Cloud couldn't read this server's check-in. The loop tries
   again every hour. Install a newer `ployz` and run `sudo ployz server checkin --install`.
9. `Token refused` means Ployz Cloud refused the Fleet Token once. The loop asks again once a
   minute has passed on Cloud's clock since that refusal, even across a restart, and goes on as
   before if Cloud accepts it. A new token in the file starts over with its own two chances. A
   refusal during the install itself ends it with an error instead, since the token was only just
   given, and starts nothing.
10. `Rejected`, `Token expired` or `Token revoked` are final. The running check-in loop stops,
   deletes `/etc/ployz/fleet-token`, or the file you named with `--token-file`, if it still holds
   the token Cloud answered, and won't start again at boot. If the answer comes during
   `ployz server checkin --install`, the install ends with an error and keeps the file. A token Cloud refuses twice in a row, at least a minute apart by Cloud's clock, because you revoked or
   deleted it, reads `Token revoked`. If you put a new token in the file as the loop stops, it
   keeps it and restarts with it, unless Cloud rejected the server. Put a new Fleet Token in that file and run `sudo ployz server checkin --install`
   again to start a new check-in attempt. A non-rejection response clears the local rejection
   status, but Cloud can still reject the server. If the token file goes missing, the loop stops
   the next time it starts. Preparation already under way finishes.
11. A `blocked` line saying `ployzd ... is older than this ployz` means you upgraded `ployz` but
   not `ployzd`. The loop keeps checking in with the older one, and Cloud shows the server as
   blocked until you run `sudo ployz server checkin --install`. It uses the first older one that
   answers, trying the one install chose, then PATH, then its own copy, and doesn't prepare Docker
   or ZFS with it. `No ployzd` means there is none at all: the loop exits until the same command
   installs one.
12. Once you approve the server, the loop enrolls it and the **this server** line walks through
    `Installing`, `Enrolling`, `Queued to join`, `Setting up ZFS`, `Joining` and `Publishing`,
    the same stages Ployz Cloud shows, where `Queued to join` reads **In line** and `Publishing`
    reads **Going live**. `Queued to join` means other servers are joining the fleet first. When
    it reads `Joined`, the running check-in loop turns itself off and deletes
    `/etc/ployz/fleet-token`, or the file you named with `--token-file`.
13. `Enroll failed` means a stage failed, and the line after it names the stage and why, for
    example `Enroll failed at enrolling: Docker is not running`. The server goes back to waiting
    and Ployz Cloud shows the same failure. Fix the cause, then approve the server again in Ployz
    Cloud. If Cloud enrollment publication or completion failed, run `ployz server enroll`
    again, or use **Enroll** in **Servers > Waiting to join**. If the loop restarts mid-enroll,
    it picks up the same attempt rather than starting a second one and keeps the last reported
    stage. If the join committed but a follow-up failed, the server stays Joined, the loop
    cleans up, and its log and status name that follow-up.
14. Rejecting a server stops enrollment at the next safe point. A package install finishes
    before stopping at the enroll or queue step. Storage setup finishes once begun in the
    current run. After a restart from storage setup, rejection stops enrollment at the enroll
    or queue step. Once the daemon accepts the join and Cloud records `Joining` or
    `Publishing`, rejection lets enrollment finish, including after a restart. Cloud keeps
    its enrollment token valid and continues answering the attempt until it joins or fails.
    Expiring or revoking a Fleet Token lets an open enrollment finish.
15. If Cloud selects a spare disk, the server builds its ZFS pool on that disk.
16. When you check ports on waiting servers, each one listens on TCP 80 and 443 and UDP 51820 for
    at most 10 seconds, then closes them. Nothing stays listening after that. A check that arrives
    while `ployz server checkin --install` runs makes it pause for those seconds, and it says so.
    The server tells Cloud what it heard only after it closes the ports. One that restarts while it
    listens tells Cloud nothing for that check, so its ports read **Unknown · server didn't
    report**: check again. **Open** means Cloud reached the port and the server answered.
    **Closed** means the server listened but Cloud's connection was refused, so a firewall in front
    of it turns the port away. **Couldn't listen · port in use** means Ployz couldn't listen on that
    port in time. Usually another program, such as a web server, already holds it. Find it with
    `sudo ss -lntup 'sport = :80'`, stop it, and check again. If nothing holds it, a delayed
    check-in reply or a busy server may have left too little time to listen, so check again. A
    server running a `ployz` older than this feature leaves every port Unknown.

## Another program holds UDP port 51820

A waiting server reads **Blocked** with `UDP port 51820 is in use by avahi-daemon (pid 812).`, or
`ployz status` lists it under `blocked`. Ployz needs that port for WireGuard between servers, and
while another program holds it, Ployz can't start on the server: an enroll would fail waiting for
it. See what holds it with `sudo ss -ulpn 'sport = :51820'`, then stop or restart that program.
avahi-daemon, for one, can take the port at random and lets it go when restarted. The block
clears on the server's next check-in.

A server that's already in a Cluster, or one you added with `ployz server add`, has no check-in.
There, Ployz keeps restarting, and `sudo journalctl -u ployz -n 50` says `UDP port 51820 is in use
by avahi-daemon (pid 812), so WireGuard can't listen`, or `by another program` when it can't tell
which. Free the port the same way, and Ployz starts on its next restart.

When `ss` lists the port with no program, another WireGuard interface, such as a tunnel set up with
`wg-quick`, probably holds it. `sudo wg show all listen-port` names the interface. Move that
tunnel to another port, or take it down.

## Ployz stops over a leftover network

`sudo systemctl status ployz` shows the service failed with `status=78`, and its logs say
`refusing to change the retained WireGuard interface` or `refusing to replace the existing Docker
network`. Something else, or an earlier install, left a network Ployz can't prove is its own, so
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

## My first server never finished joining

You'll see `This Organization's Servers can't be reached` when you add another server, and an
alert on the **Organization** page. The alert also shows while your first server installs, so give
it a few minutes first.

No other server can join until the first one finishes.

1. If the first server still exists, run its command on it again. If its token expired, run
   `ployz server add --command` for a new one; it picks up where the server left off.
2. If that server is gone, or you want to start over, make sure it's stopped or erased at your
   provider. Then go to **Organization**, click **Reset founding attempt**, tick **I confirm the
   old Machine has been stopped or erased.** and click **Reset enrollment**.
3. Add a server again.

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
`docker stop ployz-corrosion`, Ployz starts it again within a few seconds; try again then.

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

The server's page says **Can't reach web-2**. Services that run only there are down. Stopping Ployz
with `sudo systemctl stop ployz.socket ployz` doesn't cause this: the server keeps its place among
your servers until it reboots or you remove it.

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
- `Docker's ip6tables is off or uses nftables`: Ployz needs Docker's default iptables firewall
  with `ip6tables` on. Remove `"ip6tables": false` and `"firewall-backend": "nftables"` from
  `/etc/docker/daemon.json`, restart Docker, then run `sudo systemctl restart ployz`.
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

## The bottom bar shows Add a server instead of Deploy

Your organization has no server, so nothing can deploy.

[Add a server with `ployz server add`](../servers/add-a-server.md#add-one-server-that-joins-right-away). Your projects and settings are kept. Once it joins,
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
- **Kept container logs and metrics.** Ployz keeps the logs of containers it ran, even after
  they're replaced, and the server's metrics in `ployz-observe` under Docker's directory.
  `docker system df` doesn't count them; `sudo du -sh /var/lib/docker/ployz-observe` does. Together
  they're capped at 5% of the disk, kept between 512 MB and 5 GB. Logs go after 30 days. When free
  space drops below 10% of the disk (at least 2 GB), the oldest logs and metrics go first until it's
  back.

## A deploy says there isn't enough disk space

You'll see `Not enough disk space: about ... more free space is needed`.

A managed volume needs its whole storage limit free on the server, on top of the quarter of the
disk Ployz keeps for the system. Free disk space, give the server a bigger disk, or lower the
volume's storage limit before you deploy it. See [Volumes](../services/volumes.md).

## Related

- [Add a server](../servers/add-a-server.md)
- [Manage servers](../servers/manage-servers.md)
- [My app isn't reachable](app-not-reachable.md): error pages, ports, domains and certificates
