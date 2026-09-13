# Ployz Core

Deployment engine, CLI, daemon, and SDK for a cluster of Docker machines.
The hosted application lives in [dashboard/](../dashboard/README.md).

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Release process: [docs/RELEASE.md](docs/RELEASE.md).

`ployz machine add` saves subnet assignments beside its configuration file in
`<config-stem>.enrollment/` before publishing or joining. Commands using that
store serialize allocation, including pending work; retry with the same identity
and inputs to resume after a network failure. Context aliases and renames share
history through observed durable Machine identities. Disjoint observations
cannot establish a shared scope. Different computers, configuration stores, and
Cloud remain independent operators and can still choose overlapping subnets;
there is no automatic reclamation, cross-store synchronization, or subnet repair.

## Workspace

Run Cargo and engine script commands from `core/`.

- `crates/ployz-core`: domain and wire contracts shared by both binaries
- `crates/ployz`: CLI for Linux, macOS, and Windows through WSL
- `crates/ployz-sdk`: napi package `@ployz/sdk` (linux and macOS x64/arm64 gnu bindings; published on GitHub Release Publish). Its TypeScript declarations are derived from the Rust wire types by `cargo test -p ployz --test sdk_payloads`
- `crates/ployzd`: Linux-only daemon
- `crates/ployz-testkit`: unpublished support crate used only by tests
- `native/tailcat`: Machine RPC transport helper bundled with CLI, daemon, and native SDK releases

Building `ployz` also requires Go 1.24 or newer. Cargo builds and embeds the Compose helper; installed users need neither Go nor the Docker Compose plugin. Local builds require Docker with Buildx and the containerd image store.

`ployz build` and `ployz deploy` prefer a declared or existing default Dockerfile.
Buildable Services without one use Railpack; image-only Services are unchanged.
Set `build.x-recipe` to `dockerfile`, `railpack`, or `auto` (the default) to control
selection. A failed recipe never falls back to another.

Railpack uses matching pinned 0.39.0 preparation/frontend tooling with BuildKit
0.26.2, provisioned through Docker. Builds default to the native Linux AMD64 or
ARM64 platform. Set `build.platforms: [linux/amd64, linux/arm64]` to build both;
the execution host must provide native or emulated support for each platform.
Separate solves are assembled locally with pinned regctl 0.11.6 into one immutable
image in Docker’s containerd store, with every platform’s content verified.
Multi-platform builds currently require local output. Railpack rejects
`build.provenance` and `build.sbom` because assembly cannot carry attestations;
Dockerfiles pass them to BuildKit and remain limited to one platform.

`ployz deploy` derives each Railpack Service's platforms from the Machines it may
be placed on, read from the current Cluster Observation, before building; explicit
`build.platforms` must cover them, and `DOCKER_DEFAULT_PLATFORM` is replaced
because it describes this client, not the Cluster. Standalone `ployz build`
keeps the native default. After every Build succeeds, the completed platforms are checked against
the fresh Deploy plan's destinations; a Machine no variant runs stops the Deploy
before any Service, hook, or volume change, and the fix is a rerun, never an
automatic rebuild or a moved placement. Images travel by exact content digest
from the complete Build host, and every peer-to-peer transfer names the
destination's platform. A Machine serves an image only when Docker reports that
variant's manifest, configuration and layers present in its containerd store: a
tag, an image index, or a listed-but-absent platform is not content, so a peer
that pulled one platform is never the source for another.

The [prototype findings](https://github.com/getployz/ployz2/blob/c3ca5519a4607256ffb28052d77a1c7d89f1bbe1/prototypes/railpack-transfer/FINDINGS.md)
preserve the evidence for this assembly approach. They used shipped beta binaries
and emulated ARM64; they do not qualify this implementation or native ARM64.

Service variables default build variables; Compose `build.args` and then
`--build-arg` override them without changing runtime values. Values travel as
private secret mounts. Docker ignore patterns and Railpack's configured
exclusions apply before source transfer. Recipes can still print or embed values.

Railpack refuses `--check` and unsupported frontend settings by name. On this
pinned frontend, `--no-cache` and `--pull` force a cold build by clearing the
exclusive Ployz builder cache; unrelated Docker builder caches are untouched.

Remote Builds (`ployz build --remote`) use one active
slot per Machine and a FIFO of eight waiting attempts. Source and secrets stay
on the client until admission. Configure the daemon environment and restart it:

| Setting | Default | Accepted values |
| --- | --- | --- |
| `PLOYZ_BUILD_QUEUE_CAPACITY` | `8` | `0`–`1024` waiting attempts |
| `PLOYZ_BUILD_QUEUE_TIMEOUT_SECONDS` | `600` | `1`–`86400` seconds |
| `PLOYZ_BUILD_ACTIVE_TIMEOUT_SECONDS` | `1800` | `1`–`86400` seconds |

The active budget includes upload, preparation, execution, import and cleanup.
Cancellation and disconnect remove waiters; daemon restart discards the queue.
Unconfirmed termination keeps builder ownership quarantined, including across
restart. Bounded abandoned-builder teardown does not clear that uncertainty:
an operator must confirm the builder and its host processes have stopped before
clearing the lock marker named in the error. Work is never replayed.

`ployz build` stays local by default. `ployz build --remote` and connected
`ployz deploy` choose one responsive, Build-accepting Machine randomly and run all
of the command's Builds there. Use `--remote=<Machine>` to pin a builder or
`deploy --local` to build on the CLI host. `--local` conflicts with `--remote`
and `--no-build`. A rejected or failed Build is never moved to another host.

The selected Machine runs Dockerfile or native-platform Railpack Builds and
supplies the build resource policy; source uploads cannot change it. Application
placement constraints select image destinations, independently of the builder.

Local Builds use local Docker: remote `DOCKER_HOST` endpoints and non-default
Docker contexts are rejected by the shared executor before builder mutation.
Use a selected Machine for remote builds so policy and ownership are enforced
on the execution host.

Configure the execution user on each build host in `~/.ployz/build.yaml` (the daemon
user for selected-Machine Builds). If `HOME` is unset or empty, the user's account
home is used. The file is read once at admission. All fields
are optional; omitted CPU/memory limits are disabled and unconfigured GC keeps
BuildKit 0.26.2 defaults:

```yaml
cpu_cores: 0.5
memory_bytes: 536870912
cache_bytes: 10737418240
min_free_bytes: 2147483648
```

CPU is a finite number from 0.01 to 1000000 cores. Memory is at least 6291456
bytes. Cache targets are positive byte counts; byte counts must fit a signed
64-bit integer. CPU and memory ceilings apply to the BuildKit worker (including
its solve processes) and the separate Railpack preparation container. Memory
limits also disable container swap. These settings are independent of Service
runtime limits and are not build/deploy flags. Unsupported Docker resource
controls and observed launch failures stop the attempt.

`cache_bytes` and `min_free_bytes` are retention/GC targets, **not hard peak disk
quotas**. BuildKit owns eviction; Ployz also requests upstream GC after successful
output, before removing the ephemeral worker. Active work can exceed the cache
targets, and GC may not meet an impossible free-space target. Reusable build
layers persist across builder recreation; independent cache-mount reuse is not
promised.

Run `ployz machine build-cache-clear` **on the execution host as its build user**
to clear Ployz's retained builder cache. It preserves completed Docker images and
unrelated Docker data, requires no running daemon, and refuses active or
quarantined ownership. It does not accept a remote connection/context; use host
administration to run it on the selected Machine. Docker must use its default
context and a local Unix socket; remote `DOCKER_HOST` and non-default Docker
contexts are refused before builder mutation. Local CLI and daemon Builds
share a stable per-user lock under `/var/tmp/ployz-build-<uid>`, even with different
home or Docker configuration directories. There is still one active Build per
builder; no configurable concurrency or cache replication is introduced.

Controls follow [Docker's container builder resource and cache support](https://docs.docker.com/build/builders/drivers/docker-container/)
and [BuildKit 0.26.2 GC policy](https://github.com/moby/buildkit/blob/v0.26.2/cmd/buildkitd/config/gcpolicy.go).

Run the fast local gate with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.

`site/` serves the ployz.sh installer CDN. Dashboard links the local SDK; run
`python3 ../scripts/check-cloud-sdk-version.py` to check its binding version pins.
