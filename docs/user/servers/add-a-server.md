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

## Add many servers with a Fleet Token

A Fleet Token lets any number of hosts ask to join. Make one from your machine:

```sh
ployz server token create --expires 7d
```

It prints the token once, with the line to run on each host:

```sh
curl -fsSL https://ployz.sh | sudo PLOYZ_ENROLL_TOKEN=pflt_… sh
```

- **Hosts wait for you.** A host that runs the line waits until you accept it. Add
  `--auto --max 5` to let up to five hosts in on their own; anyone holding that token can add
  servers, so keep it secret.
- **It lasts 24 hours by default.** `--expires` takes `1d`, `7d` or `30d`.
- **Expiry and revoke stop new hosts only.** Servers already in stay.

List your tokens, and revoke one by name:

```sh
ployz server token ls
ployz server token revoke amber-falcon
```

Revoking also drops the hosts still waiting on that token. The install line puts the token in the
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

If your server can't run ZFS, click **Start without it** under "Other Linux, or can't run ZFS?"
in the dialog. The command then ends in `--storage none`, and **Use ZFS instead** switches
back.

The server then shows **Docker only**. If none of your servers has managed volumes, every new volume,
a database's included, fails to deploy until you open it and tick **Use a plain Docker volume
(not recommended)** under **Advanced**; see
[When your servers show Docker only](../services/volumes.md#when-your-servers-show-docker-only).

## Install with a Fleet Token

A Fleet Token starts with `pflt_`. Give it to the install script and the host asks to join
instead of joining:

```sh
curl -fsSL https://ployz.sh | sudo sh -s -- --enroll
```

The script prints `Installs Docker and Ployz. Nothing joins until you say so.` and asks for the
token at a `Fleet Token:` prompt that doesn't show what you paste. The token never appears on a
command line, so it stays out of `sudo`'s log, shell history and `ps`. The script checks that the
token looks like one before it changes anything. It installs the CLI, writes the token to
`/etc/ployz/fleet-token`, readable by root only, and runs `ployz server checkin --install`. That
ends with `Waiting for approval`, within seconds: Docker installs and ZFS builds in the background
while the host waits, and the host shows on the **Servers** page at once.

- **Without ZFS.** Put `PLOYZ_SKIP_ZFS=1` after `sudo`, as
  `curl -fsSL https://ployz.sh | sudo PLOYZ_SKIP_ZFS=1 sh -s -- --enroll`. It does what
  `--storage none` does for `ployz server add`.
- **From user data.** cloud-init runs user data as root with no terminal to prompt at, so pass the
  token in `PLOYZ_ENROLL_TOKEN` instead:

  ```yaml
  #cloud-config
  runcmd:
    - curl -fsSL https://ployz.sh | PLOYZ_ENROLL_TOKEN=pflt_… sh
  ```

  Any process on the server, containers included, can read user data from the provider's
  metadata service for as long as the server runs. Give a token you put there a short expiry.
- **Run it again.** The same line on the same host updates its token and keeps it one server. A
  token that isn't one leaves the host as it was.
- **An expired token** prints `Token expired.`, Cloud's reason, and `Run the install again with a
  new token.`, and the script exits with an error. A revoked one says so the same way.
- **A token Cloud doesn't know**, or a missing `PLOYZ_CLOUD_URL` when you
  [run your own Cloud](../self-hosting.md), ends the install with
  `Cloud at … refused this Fleet Token.` and an error.

When the install ends with an error, the host stops checking in and won't start again at boot.
Preparation already under way finishes, so a package install is never cut off halfway, and
nothing starts again.

Without `--enroll`, the script uses `PLOYZ_ENROLL_TOKEN` only when it starts with `pflt_`. For
any other value it says so and only installs the CLI, as it does without a token. If the host
waits longer than you expect, see
[A server waits for approval](../troubleshooting/servers.md#a-server-waits-for-approval-and-never-shows-up).

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
