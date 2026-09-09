# ployz2

Ployz Cloud, CLI, and daemon for a cluster of Docker machines

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Release process: [docs/RELEASE.md](docs/RELEASE.md).

## Workspace

- `crates/ployz-core`: domain and wire contracts shared by both binaries
- `crates/ployz`: CLI for Linux, macOS, and Windows through WSL
- `crates/ployz-relay`: Cloud Relay HTTP/1.1 WebSocket splice (Linux binary + `ghcr.io/getployz/ployz-relay`)
- `crates/ployz-sdk`: napi package `@ployz/sdk` (linux and macOS x64/arm64 gnu bindings; published on GitHub Release Publish). Its TypeScript declarations are derived from the Rust wire types by `cargo test -p ployz --test sdk_payloads`
- `crates/ployzd`: Linux-only daemon
- `crates/ployz-testkit`: unpublished support crate used only by tests

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
Multi-platform builds currently require local output and reject requested
`build.provenance` or `build.sbom`; Dockerfiles remain limited to one platform. Deploy platform
inference is separate work.

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

`ployz build` and `ployz deploy` run here by default. `--remote=<Machine>` pins
one Machine, plain `--remote` selects one automatically, and `--local` states the
local default explicitly. Build location is per invocation: Machine selectors are
Cluster-local and may be ambiguous, so no Compose field pins one. Automatic
selection uses visible Machines that advertise remote Builds, prefers the one
whose reported architecture runs the most requested `build.platforms` natively,
and breaks ties on Machine ID, so it does not depend on observation order. The
selected Machine is reported before any upload, along with any platform this
client could not confirm it runs natively and any Machine that did not answer
for itself; it still admits or refuses the Build itself, and Ployz never falls
back to a local Build silently.

Remote Builds (`ployz build --remote=<machine>`) use one active
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

`ployz build --remote=<Machine>` runs Dockerfile or native-platform Railpack
Builds on the selected Machine. That Machine supplies the build resource policy;
source uploads and build requests cannot change it.

Ordinary builds use local Docker: remote `DOCKER_HOST` endpoints and non-default
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

Cloud lives in `cloud/` with its own package and lockfile. Run `pnpm install --frozen-lockfile` and `pnpm pr:check` there. Engine Cargo commands run from the repository root. `site/` serves the ployz.sh installer CDN.

Cloud installs the published `@ployz/sdk`, including platform bindings, pinned to the workspace version. Run `python3 scripts/check-cloud-sdk-version.py` to check those pins. See [the repository layout decision](docs/adr/0004-product-repo-layout.md).

Production cutover uses the existing Railway **Ployz Dashboard / production / web** service: change its source to `getployz/ployz2`, branch `main`, and root directory `/cloud`. Keep its existing variables, domains, and `pnpm db:migrate` pre-deploy command. This repository change does not apply those hosted settings.

Cloud migrations now start from one fresh baseline for the planned Railway reset. Apply it only to an empty Cloud database with a fresh Drizzle migration journal. Reset Electric's persisted sync state together with Postgres before cutover, then run `pnpm db:migrate` from `cloud/`. Historical migrations remain in the original dashboard repository history.
