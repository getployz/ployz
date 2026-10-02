---
title: Build resources and cache
description: Limit the CPU, memory and disk builds use on a server, give builds more time, and free up disk space.
---

Builds on your servers share CPU, memory and disk with your services. A big build can slow
your app down, so you can cap what builds use on each server.

Start in the dashboard: lower a server's **Builds at once**, give builds their own server, or
move them to GitHub Actions. See [Where builds run](where-builds-run.md). For finer limits,
use the settings below. They can't be set from the dashboard yet: you set them on the server,
over SSH as root. Builds on GitHub Actions don't use them.

## Cap CPU, memory and cache

Create `/root/.ployz/build.yaml` on the server. Every line is optional:

```yaml
# Each build may use at most 2 CPU cores
cpu_cores: 2

# ...and at most 4 GiB of memory, in bytes. A build that needs more fails.
memory_bytes: 4294967296

# Keep the build cache near 20 GiB, in bytes
cache_bytes: 21474836480

# Keep at least 10 GiB of disk free, in bytes
min_free_bytes: 10737418240
```

The next build on that server uses it; nothing needs a restart. A mistake in the file fails
every build on the server with `invalid host build.yaml: …` or a message that names the
field, so watch the next build after you change it.

- **The limits are per build.** With **Builds at once** set to 2, builds on the server can use
  twice as much.
- **Cache sizes are targets.** A big build can briefly go over them.

## Give builds more time

A build on your servers fails after 30 minutes. To allow more, up to 86400 seconds (a day),
add a line to `/etc/default/ployz` on the server, then restart Ployz. Above 86400, Ployz won't
start. Restarting drops any builds waiting on the server, so do it when it isn't building:

```sh
# Builds may run for 1 hour (3600 seconds)
echo 'PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS=3600' >> /etc/default/ployz
systemctl restart ployz
```

## Free up disk space

The build cache makes builds faster. To free its space and start from an empty cache, run this
on the server as root when it isn't building:

```sh
ployz server build-cache-clear
```

It keeps your images and everything else in Docker.

Ployz cleans up old images it built, but never images it pulled for
[Docker image services](../deploy/docker-image.md). Remove those with `docker image prune`
when you need the space.
