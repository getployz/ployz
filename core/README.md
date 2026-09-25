# Ployz Core

Deployment engine, `ployz` CLI, `ployzd` daemon, and internal SDK for a Cluster
of Docker Machines. Ployz Cloud lives in [dashboard/](../dashboard/README.md).

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Releases and channels: [docs/RELEASE.md](docs/RELEASE.md). What a 0.x daemon
keeps working across: [DESIGN.md](DESIGN.md#stable-promise).

## CLI

The CLI operates a Cluster over SSH contexts. Cloud authors and deploys
Services; the CLI has no deploy, build, or image command.

```text
ployz
├── cloud      enroll
├── machine    init · add · ls · inspect · logs · rename · rm · rtt · update
│              upgrade [inspect] · build-cache-clear
├── service    ls · inspect · logs · exec · scale · start · stop · rm
├── volume     create · ls · inspect · rm
├── ingress    config · deploy · logs
├── project    ls · rm
├── ctx        ls · show · use · rm · connection
├── proxy
├── ps
├── version
└── completion
```

`crates/ployz/tests/cli_shape.rs` pins this tree; there are no aliases.

`ployz machine add` saves subnet assignments beside its configuration file in
`<config-stem>.enrollment/` before publishing or joining. Commands using that
store serialize allocation, including pending work; retry with the same identity
and inputs to resume after a network failure. Context aliases and renames share
history through observed durable Machine identities. Disjoint observations
cannot establish a shared scope. Different computers, configuration stores, and
Cloud remain independent operators and can still choose overlapping subnets;
there is no automatic reclamation, cross-store synchronization, or subnet repair.

`ployzd` issues https Ingress certificates from the ACME directory named by
`PLOYZ_ACME_DIRECTORY` in the daemon environment:

| `PLOYZ_ACME_DIRECTORY` | Certificate issuance |
| --- | --- |
| unset | Let's Encrypt production |
| a URL | that ACME directory |
| empty | off |

## Workspace

Run Cargo and engine script commands from `core/`.

- `crates/ployz-core`: domain and wire contracts shared by both binaries
- `crates/ployz`: CLI for Linux, macOS, and Windows through WSL
- `crates/ployz-build`: BuildKit execution for one captured Build
- `crates/ployz-config-wasm`: the SDK's config ABI, compiled to WASM
- `crates/ployz-sdk`: internal workspace package `@ployz/sdk`, never published. napi serves Machine RPC; config runs on the `ployz-config-wasm` build in Node and the browser. Its TypeScript declarations are derived from the Rust wire types by `cargo test -p ployz --test sdk_payloads`
- `crates/ployzd`: Linux-only daemon
- `crates/ployz-testkit`: unpublished support crate used only by tests

Each release archive ships one binary. The remote management transport (iroh, via the Ployz-hosted Ployz Relay) is in-process in `ployz`, `ployzd`, and the SDK; there is no helper process.

Cloud builds each Git Service on one Build Machine. The Service's build settings
select a Dockerfile or Railpack; a failed recipe never falls back to another.

Railpack uses matching pinned 0.39.0 preparation/frontend tooling with BuildKit
0.26.2, provisioned through Docker. Preparation derives each Railpack Service's
platforms from the Machines it may be placed on, read from the current Cluster
Observation, before building; the Build Machine must provide native or emulated
support for each. Separate solves are assembled with pinned regctl 0.11.6 into one
immutable image in Docker’s containerd store, with every platform’s content
verified. Dockerfile Builds produce the Build Machine's native platform.

After every Build succeeds, the completed platforms are checked against
the fresh Deploy plan's destinations; a Machine no variant runs stops the Deploy
before any Service, hook, or volume change, and the fix is a rerun, never an
automatic rebuild or a moved placement. Images travel by exact content digest
from the complete Build host, and every peer-to-peer transfer names the
destination's platform. A Machine serves an image only when Docker reports that
variant's manifest, configuration and layers present in its containerd store: a
tag, an image index, or a listed-but-absent platform is not content, so a peer
that pulled one platform is never the source for another.

Service variables become build variables without changing runtime values; a
Railpack build command is passed as `RAILPACK_BUILD_CMD`. Values travel as
private secret mounts. Docker ignore patterns and Railpack's configured
exclusions apply before source transfer. Recipes can still print or embed values.

Builds use one active slot per Machine and a FIFO of eight waiting attempts.
Source and secrets stay on the client until admission. Configure the daemon
environment and restart it:

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

Preparation chooses one responsive, Build-accepting Machine randomly and runs all
of its Builds there. A rejected or failed Build is never moved to another host.

The selected Machine supplies the build resource policy; source uploads cannot
change it. Application placement selects image destinations, independently of
the builder.

Configure the daemon user on each Build Machine in `~/.ployz/build.yaml`. If `HOME` is unset or empty, the user's account
home is used. The file is read once at admission. All fields
are optional; omitted CPU/memory limits are disabled. Without `cache_bytes` or
`min_free_bytes`, GC keeps a fifth of the Docker root filesystem free, with no
reserved cache floor:

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
runtime limits. Unsupported Docker resource
controls and observed launch failures stop the attempt.

`cache_bytes` and `min_free_bytes` are retention/GC targets, **not hard peak disk
quotas**. BuildKit owns eviction; Ployz also requests upstream GC after successful
output, before removing the ephemeral worker. Active work can exceed the cache
targets, and GC may not meet an impossible free-space target. Reusable build
layers persist across builder recreation; independent cache-mount reuse is not
promised.

Image Cleanup removes superseded build images from the Machines a Deploy delivered
to. Direct Image Transfer tags each delivered image `repository:ployz-sha256-<digest>`;
only those tags, in repositories the Deploy built, are candidates. Per repository,
Ployz keeps the three newest unused images and removes older ones idle for seven
days. Below 20% free on a Machine's Docker root it keeps one and removes the rest.
A Machine never removes an image any Container uses, running or not. The SDK
cleans up after each Deploy by default and reports it as the last `images_pruned`
event; pass `imageCleanup: "manual"` to run `pruneImages(pruneTargets)` yourself.
Cleanup never changes the Deploy Outcome.

Run `ployz machine build-cache-clear` **on the execution host as its build user**
to clear Ployz's retained builder cache. It preserves completed Docker images and
unrelated Docker data, requires no running daemon, and refuses active or
quarantined ownership. It does not accept a remote connection/context; use host
administration to run it on the selected Machine. Docker must use its default
context and a local Unix socket; remote `DOCKER_HOST` and non-default Docker
contexts are refused before builder mutation. Daemon Builds and cache clearing
share a stable per-user lock under `/var/tmp/ployz-build-<uid>`, even with different
home or Docker configuration directories. There is still one active Build per
builder; no configurable concurrency or cache replication is introduced.

Controls follow [Docker's container builder resource and cache support](https://docs.docker.com/build/builders/drivers/docker-container/)
and [BuildKit 0.26.2 GC policy](https://github.com/moby/buildkit/blob/v0.26.2/cmd/buildkitd/config/gcpolicy.go).

Run the fast local gate with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.

`site/` serves the ployz.sh installer CDN. Dashboard links the workspace SDK.
