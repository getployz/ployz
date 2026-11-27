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
- **Each build slot keeps two caches**, one for services that reach no data and one for the
  rest. Each keeps to half of `cache_bytes`.
- **Without `cache_bytes` or `min_free_bytes`**, Ployz keeps a fifth of the server's Docker disk
  free.

## Give builds more time

A build on your servers fails after 30 minutes. To allow more, up to 86400 seconds (a day),
add a line to `/etc/default/ployz` on the server, then restart Ployz. Above 86400, Ployz won't
start. To fail a build that waits too long for a free slot, also add
`PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS`, with the same limit. Without it, a build waits as long as
it takes. Restarting drops any builds waiting on the server, so do it when it isn't building:

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

Ployz names its build containers `buildx_buildkit_ployz-…` and `ployz-…-railpack`,
`-prepare` or `-assemble`, and marks each one it creates. Ployz never builds in or removes a
container that has one of those names but not the mark. The cache clear names it so you can
remove or rename it. A Build that would need the name moves to another build slot,
except when Ployz kept that slot's worker running after an earlier Build. Then the Build stops,
with an error naming the container.

One exception covers containers from Ployz versions before the mark. Ployz treats an unmarked
container as its own, and removes it after a crash or on restart, when one of these is true:

- It's named `buildx_buildkit_ployz-…`, Docker Buildx started it, and its image is exactly
  `moby/buildkit:v0.26.2` or
  `moby/buildkit:v0.26.2@sha256:de10faf919fc71ba4eb1dd7bd6449566d012b0c9436b1c61bfee21d621b009aa`.
- It's named `ployz-…-railpack` and its image is exactly
  `ghcr.io/railwayapp/railpack-frontend@sha256:db24dc37640b6887c3d455b40876ea30f75182964479670cba6e4cde7ffef103`.
- It's named `ployz-…-prepare` and its image is exactly
  `buildpack-deps:trixie-curl@sha256:3dd78c060b4d7c97b426d0160f7d8876c84fbf2fc36e651e9c75620ac9a7da24`.
- It's named `ployz-…-assemble` and its image is exactly
  `regclient/regctl@sha256:5fe7c6a6206e1d9d71af719674c9db03e74a4ae221a32089b691f5b55533d6ea`.

Ployz can't tell such a container from one you made the same way, so don't run those images
under those names yourself.

After each deploy, Ployz cleans up old images it built, but never images it pulled for
[Docker image services](../deploy/docker-image.md). Remove those with `docker image prune`
when you need the space.

An image a server built for a service that reaches no data has no name. After each such build,
the server removes the ones nothing has used for a week, or for an hour when its Docker disk is
nearly full.
