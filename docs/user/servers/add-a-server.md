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

## Add a server

1. Go to **Servers** and click **Add server**. While you have no server, the bottom bar of your
   environment offers **Add a server** too.
2. Not on Ubuntu, Debian or Amazon Linux, or can't run ZFS? Click **Start without it** in the dialog first; see
   [Start without ZFS](#start-without-zfs).
3. Click **Copy** next to the command under **Run this as root**.
4. Open a shell on the server, as root or as a user with `sudo`, and paste the command. The
   server takes its hostname as its name; to choose another, add `--name web-1` to the end.
5. When it asks for the enrollment token, click **Copy** under **Paste the token when asked**
   and paste it.

![The Add a server dialog with its command](../images/add-server-dialog.png)

The command looks like this:

```sh
curl -fsSL https://ployz.sh/ | sh && sudo ployz server add
```

The command carries no token. `ployz server add` asks for it, and the prompt doesn't show what you
paste. The token never enters your shell history or the server's process list, whatever your
shell's history settings. If the server's root user is signed in to Cloud with `ployz login`, it
doesn't ask: it enrolls into the organization it's signed in to.

To enroll a server from a script, give `ployz server add` the token one of three ways:

- `--token-stdin` reads it from standard input.
- `--token-file <path>` reads it from a file, such as one your provisioning tool wrote.
- The `PLOYZ_ENROLL_TOKEN` environment variable, set in the root shell or the service that runs
  `ployz server add`. Don't write `sudo PLOYZ_ENROLL_TOKEN=... ployz`: that puts the token in the
  process list.

Use one of them, not two.

When it finishes, the server shows on the **Servers** page, ready for builds, services and web
traffic. The command works for 24 hours; open the dialog again for a new one. If the server
doesn't show up, see [Troubleshooting servers](../troubleshooting/servers.md).

Add more servers the same way, even at different providers. Nothing moves onto a new server until
you deploy; see [Scaling](../services/scaling.md#add-a-server).

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

If your server can't run ZFS, click **Start without it** under "Other Linux, or can't run ZFS?"
in the dialog. The command then ends in `--storage none`, and **Use ZFS instead** switches
back.

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
