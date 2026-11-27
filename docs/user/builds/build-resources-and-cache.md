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

Ployz names its build containers `buildx_buildkit_ployz-…` and `ployz-…-railpack`, `-prepare` or
`-assemble`. Ployz counts a container with one of those names as its own only when its image is
exactly the one Ployz uses under that name, listed below, and it has the environment variable
`PLOYZ_BUILD_OWNER` exactly once, set to the ID of the server user Ployz runs as. Ployz sets
that variable on every build container it creates. Ployz never builds in or removes any other
container with one of those names. The cache clear names such a container so you can remove or
rename it. A Build that would need the name moves to another build slot, except when Ployz kept
that slot's worker running after an earlier Build. Then the Build stops, with an error naming
the container.

Ployz acts on a build container by its Docker ID, read when it checked the container is its own,
never by its name. If another container takes the name meanwhile, Ployz leaves it alone. A
worker that Ployz kept running after a Build is the exact container that Build ran on.

Ployz versions before `PLOYZ_BUILD_OWNER` didn't set it. Ployz counts a container that has no
`PLOYZ_BUILD_OWNER` at all as its own when one of these is true:

- It's named `buildx_buildkit_ployz-…`, its image is exactly `moby/buildkit:v0.26.2` or
  `moby/buildkit:v0.26.2@sha256:de10faf919fc71ba4eb1dd7bd6449566d012b0c9436b1c61bfee21d621b009aa`,
  and it has the environment variable `BUILDKIT_SETUP_CGROUPV2_ROOT`, which Docker Buildx sets
  on the builders it starts.
- It's named `ployz-…-railpack` and its image is exactly
  `ghcr.io/railwayapp/railpack-frontend@sha256:db24dc37640b6887c3d455b40876ea30f75182964479670cba6e4cde7ffef103`.
- It's named `ployz-…-prepare` and its image is exactly
  `buildpack-deps:trixie-curl@sha256:3dd78c060b4d7c97b426d0160f7d8876c84fbf2fc36e651e9c75620ac9a7da24`.
- It's named `ployz-…-assemble` and its image is exactly
  `regclient/regctl@sha256:5fe7c6a6206e1d9d71af719674c9db03e74a4ae221a32089b691f5b55533d6ea`.

Those are also the images Ployz runs today under each name, the worker from the
`@sha256:` reference.

When Ployz starts on the server, it always visits build slot 0, plus the slots it finds in two
places: the slot records it keeps on disk, and the workers Docker lists that count as Ployz's. It
removes the workers that count as Ployz's in each slot it visits. If a slot's record shows a Build
was interrupted, Ployz also removes that slot's helpers that count as Ployz's, then or when a
later Build takes the slot. Any later Build that uses a helper's name also removes a helper that
counts as Ployz's under that name before it creates its own. Otherwise a helper in a slot Ployz
has no record of stays until you remove it.

Ployz can't tell such a container from one you made the same way, so don't run those images
under those names yourself.

After each deploy, Ployz cleans up old images it built, but never images it pulled for
[Docker image services](../deploy/docker-image.md). Remove those with `docker image prune`
when you need the space.

An image a server built for a service that reaches no data has no name. After each such build,
the server removes the ones nothing has used for a week, or for an hour when its Docker disk is
nearly full.
