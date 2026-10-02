---
title: Troubleshooting servers
description: Fix install failures, servers that don't join or go offline, and full disks.
---

Fixes for adding a server, keeping it connected and keeping its disk free. Commands with `sudo` run
on the server; `sudo journalctl -u ployz -n 200` shows Ployz's logs there.

## The command can't use sudo

You'll see `sudo: command not found` or `run this command with sudo`.

- **`sudo: command not found`**: you're root on a server without `sudo`. The CLI is already
  installed, so run the second half of the command again without `sudo`:

  ```sh
  ployz server add --token '...'
  ```

- **`run this command with sudo`**: keep the `sudo` the dashboard command includes, or run it as
  root.

## My server isn't supported

You'll see `Unsupported platform`, `Ployz requires systemd`, `unsupported Machine architecture` or
`Ployz Machine must be Linux`.

Use an amd64 (x86_64) or arm64 (aarch64) server running a Linux distribution with systemd.

## The install stops while preparing ZFS

You'll see one of these:

- `ZFS storage preparation is not supported on debian yet; use a supported Ubuntu release`
- `Ubuntu has no packaged ZFS module for the running kernel ...`
- `OpenVZ does not allow this Machine to load the host ZFS kernel module`, or the same for
  unprivileged LXC
- `Host root has ... bytes available; ZFS validation needs ...`

The server can't run managed volumes. Either:

- use Ubuntu LTS with its stock kernel, on a VM or bare metal, with free disk space (Ployz keeps a
  quarter of the disk, at least 10 GB, free for the system), or
- add `--storage none` to the end of the command and run it again. The server shows **Docker
  only**; see [Start without ZFS](../servers/add-a-server.md#start-without-zfs) for what that costs.

## The command was rejected

You'll see `enroll HTTP 401`.

The command expired or was mistyped. It works for 24 hours. Go to **Servers**, click **Add server**
and run the new command.

## The server's name is invalid

You'll see `invalid Machine Name "web-1.example.com"`.

The server takes its hostname as its name, and a name must be one lowercase word: letters, digits
and hyphens, up to 63 characters. Add `--name web-1` to the end of the command and run it again.

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
3. Read the server's logs with `sudo journalctl -u ployz -n 200`.

## My first server never finished joining

You'll see `This Organization's Servers can't be reached` when you add another server, and an
alert on the **Organization** page. The alert also shows while your first server installs, so give
it a few minutes first.

No other server can join until the first one finishes.

1. If the first server still exists, run its command on it again. If the command expired, take a
   new one from **Add server**; it picks up where the server left off.
2. If that server is gone, or you want to start over, make sure it's stopped or erased at your
   provider. Then go to **Organization**, click **Reset founding attempt**, tick **I confirm the
   old Machine has been stopped or erased.** and click **Reset enrollment**.
3. Add a server again.

## Can't reach your servers right now

The **Servers** page shows **Can't reach your servers right now**, with what your servers last
reported or nothing at all. Adding a server can fail with `This Organization's Servers can't be
reached`.

1. Check at your provider that the servers are running.
2. On a server, run `sudo systemctl status ployz`. If Ployz is stopped, start it with
   `sudo systemctl start ployz`, and read its logs with `sudo journalctl -u ployz -n 200`.
3. If your servers' outgoing traffic is filtered, allow https to `relay.ployz.dev`.
4. If you deleted the servers, see
   [Forget servers you deleted](../servers/manage-servers.md#forget-servers-you-deleted).

## A server shows Offline

The server's page says **Can't reach web-2**. Services that run only there are down.

1. Check at your provider that the server is running, then run `sudo systemctl status ployz` on
   it.
2. Make sure UDP 51820 is open between your servers.
3. If the server is gone for good, remove it without resetting it, then deploy again. This is
   CLI-only for now:

   ```sh
   ployz server rm web-2 --no-reset --confirm web-2
   ```

   See [When a server goes down](../services/scaling.md#when-a-server-goes-down) for which
   services come back.

## The bottom bar shows Add a server instead of Deploy

Your organization has no server, so nothing can deploy.

[Add a server](../servers/add-a-server.md). Your projects and settings are kept. Once it joins,
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
- **Container logs.** If Docker was installed before Ployz, it keeps its own log settings, and
  Docker's default never rotates logs. Set `log-opts` in `/etc/docker/daemon.json` and restart
  Docker.
- **Plain Docker volumes** have no size limit, and `docker system df` above counts them.

## A deploy says there isn't enough disk space

You'll see `Not enough disk space: about ... more free space is needed`.

A managed volume needs its whole storage limit free on the server, on top of the quarter of the
disk Ployz keeps for the system. Free disk space, give the server a bigger disk, or lower the
volume's storage limit before you deploy it. See [Volumes](../services/volumes.md).

## Related

- [Add a server](../servers/add-a-server.md)
- [Manage servers](../servers/manage-servers.md)
- [My app isn't reachable](app-not-reachable.md): 502s, ports, domains and certificates
