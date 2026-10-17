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
- **A main disk of 30 GB or more.** Ployz keeps a quarter of the disk, at least 10 GB, free for
  the system. On EC2, raise the root volume from its 8 GB default when you launch the instance.
- **Root**, or a user that can run `sudo`.
- **Outbound internet access**, and a **public IP address** on at least one server so visitors
  can reach your apps.

You don't need Docker: Ployz installs it if it's missing.

## Add one server that joins right away

From your own machine, signed in with `ployz login`:

```sh
ployz server add --command
```

It prints a line to paste on the server and an enrollment token beside it, then waits until the
server joins. Paste the line in a shell on the server, as root or as a user with `sudo`, and paste
the token when it asks. The prompt doesn't show what you paste, so the token never enters the
server's shell history or process list. The token works for 24 hours.

The server takes its hostname as its name; to choose another, add `--name web-1`. When it
finishes, the server shows on the **Servers** page, ready for builds, services and web traffic. If
it doesn't show up, see [Troubleshooting servers](../troubleshooting/servers.md).

Add more servers the same way, even at different providers. Nothing moves onto a new server until
you deploy; see [Scaling](../services/scaling.md#add-a-server).

## Add servers from the dashboard

1. Go to **Servers** and click **Add servers**. While you have no server, the bottom bar of your
   environment offers it too.
2. Name the token, or leave the name blank and Ployz picks one. Choose how long it works: 1 day,
   7 days or 30 days.
3. Click **Create**. The dialog shows the install line with the token in it, under
   **Shown once. Expires in 24h.** Copy it now: Ployz can't show the token again.
4. Open a shell on the server, as root or as a user with `sudo`, and paste the line. To put it in
   user data instead, take it from the **cloud-init** or **Terraform** tab; each holds the same
   token. Not on Ubuntu, Debian or Amazon Linux, or can't run ZFS? See
   [Start without ZFS](#start-without-zfs).

![The Add servers dialog with its install line](../images/add-servers-dialog.png)

Run the same line on as many servers as you like while the token works. Each one installs Docker
and Ployz and shows up under **Waiting** on the **Servers** page within seconds, above your other
servers. New ones go to the bottom of the list, and the list never reorders while you look at it.

Select a waiting server and click **Enroll** to choose its name, storage and what it runs.
You can enroll while it prepares. Follow [Add many servers](add-many-servers.md) for the full
review and enrollment flow.

Each row shows the server's name, the IP address Ployz Cloud saw it check in from, its provider,
size, disks and storage, and its status. **Installing Docker** and **Building ZFS** mean it is
still getting ready, and **Ready** means it has everything it needs. **Blocked** rows say what
is wrong with the server, in the server's own words, such as `Disk is 20 GB. Ployz needs 30 GB.`, and
**Offline** means it stopped checking in. Filter the list by provider and by status, or search
by name or IP address.

Click a row to see the server in full, with its CPU, memory, disks, Docker, the storage it would
get, its open ports and anything blocking it. **Reject** there turns the server away: it stops
checking in, and asking again with the same token gets it turned away.

### Your tokens

**Tokens** on the **Servers** page lists your Fleet Tokens with how many servers wait on each and
how many joined, when each expires and when it was last used. **Revoke** stops a token at once
and drops the servers still waiting on it; the message after says how many. Servers that already
joined stay.

## Add many servers with a Fleet Token

A Fleet Token lets hosts register under **Waiting** for you to review and enroll together.
See [Add many servers](add-many-servers.md). You can also make a token from your machine:

```sh
ployz server token create --expires 7d
```

It prints the token once, with the line to run on each host:

```sh
curl -fsSL https://ployz.sh | sudo PLOYZ_ENROLL_TOKEN=pflt_… sh
```

- **Hosts wait.** A host that runs the line checks in and waits; it doesn't join.
- **It lasts 24 hours by default.** `--expires` takes `1d`, `7d` or `30d`.
- **Expiry and revoke stop new hosts only.** Servers already in stay.

List your tokens, and revoke one by name:

```sh
ployz server token ls
ployz server token revoke amber-falcon
```

The [Tokens](#your-tokens) page on **Servers** shows the same list. Revoking also drops the hosts still waiting on that token. The install line puts the token in the
host's process list while it runs; a Fleet Token is short-lived and you can revoke it, so that
exposure ends when you do.

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

If your server can't run ZFS, put `PLOYZ_SKIP_ZFS=1` after `sudo` in the install line, as in
`curl -fsSL https://ployz.sh | sudo PLOYZ_SKIP_ZFS=1 PLOYZ_ENROLL_TOKEN=pflt_… sh`. With
`ployz server add`, add `--storage none`.

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
| TCP 80, TCP and UDP 443 | Anywhere | Your apps over https, and their certificates, on servers that take web traffic |
| UDP 51820 | Your other servers | Services on different servers reaching each other |

## Good to know

- **Operating system updates stay with you.** Ployz doesn't install them.
- **Already running Docker?** Ployz keeps it and its settings, if it's Docker 29.2 or newer
  with buildx 0.18 or newer and the containerd image store turned on. Otherwise setup stops
  before changing anything; see
  [The install stops while checking Docker](../troubleshooting/servers.md#the-install-stops-while-checking-docker).
  Docker's default never rotates container logs, so they can fill the disk; see
  [The server's disk is filling up](../troubleshooting/servers.md#the-servers-disk-is-filling-up).
- **Pinning your own Caddy?** `--ingress-image` needs Caddy 2.8 or newer. Ployz refuses an older
  version tag.
- **Client IPs stay out of Ployz's ingress logs, not your app's.** Caddy passes the client IP to
  your app in `X-Forwarded-For`. If your app logs requests, its logs, and the Log Store's copy of
  them, can hold client IPs and user agents. Those logs are yours to manage.

Next: [manage your servers](manage-servers.md).
