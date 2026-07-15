# Docker-in-Docker E2E Harness

The DinD harness is the local acceptance path for multi-machine ployz. It
boots privileged systemd "machine" containers, installs the real Host Runner
artifacts, forms a real TLS-authenticated NATS cluster through product
commands only, and asserts operations, running workloads, daemon-restart
invisibility, and auth rejection. It supersedes the two-machine bash recipe
that used to live in `scripts/local-dataplane-proof.sh`; that script still
owns the Ployz Native Mesh dataplane proof.

## Requirements

- Docker with `--privileged` container support: OrbStack or Docker Desktop on
  macOS, plain Docker on Linux. The harness connects from the environment
  (`DOCKER_HOST` honored), so any of these contexts work.
- Roughly 4 GB of memory per two-machine cluster pair. The smoke path runs
  alone; the remaining groups use two workers by default.
- Host-arch Linux artifacts. Everything is built for the Docker server's
  architecture (`docker info --format '{{.Architecture}}'`); nothing
  hardcodes amd64, so Apple Silicon hosts build and run arm64 throughout.
- No registry access at test time: the machine image bakes `nats-server` and
  the workload image tarball, and the ployz binaries are volume-mounted.

## One Command

```sh
scripts/dind-e2e.sh
```

The default gated suite refreshes every mutable workload image tag, rebuilds
the machine image, disables incremental compilation, and runs every scenario.
This keeps the final verification command clean and reproducible.

## Manual Pieces

Build the machine image and artifacts explicitly:

```sh
scripts/build-dind-machine-image.sh
```

This produces the `ployz-dind-machine:local` image (systemd PID 1, inner
dockerd, `nats-server`, baked workload tarball) and host-arch release
binaries under `/tmp/ployz-dind-machine-target/release/` that the harness
volume-mounts read-only at `/opt/ployz/artifacts` inside every machine.

Run one group directly:

```sh
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster group_core_deploy_semantics -- --nocapture
```

## Running A Single Scenario

Pass the test name (or any prefix) as a filter:

```sh
scripts/dind-e2e.sh group_core_deploy_semantics --exact
```

Filtered runs reuse the existing machine image when its platform
and substrate fingerprint match the current Dockerfile, Docker daemon
configuration, image loader unit, NATS version, and workload image references.
They rebuild only the mounted Rust and eBPF artifacts with Cargo incremental
compilation enabled.
A missing, stale, or wrong-platform machine image falls back to the full build.

Unfiltered and `--full` runs always pull all four workload references so mutable tags are
refreshed. Unchanged image IDs reuse their existing tar archives; changed or
missing archives are saved atomically before the machine image is rebuilt.
The shared builder image contains the native and eBPF toolchains, so editing
`ployzd` does not reinstall system packages or Rust tooling.

The suite owns six cluster lifecycles:

- `serial_smoke` — init, detailed edge join, cross-machine deploy, runtime
  fields, volumes, daemon restart, auth rejection, and teardown.
- `group_core_deploy_semantics` — single-machine deploy convergence, hooks,
  dependency ordering, registry behavior, repush, and rollback.
- `group_network_repair` — two-machine network testimony, repair, resolver,
  cross-machine DNS traffic, and last-known-good behavior.
- `group_placement_peer_health` — direct image push and three-machine
  placement under silent and unhealthy peers.
- `group_unreachable_join` — isolated typed admission failure for an
  unreachable overlay peer.
- `group_v1_acceptance` — the five-step v1 journey on fresh machines.

Harness provisioning and observation are test plumbing. Every product action
in the five steps uses the shipped `ployz` command surface and the real install
script. Compose compatibility (#314) and custom-domain HTTP-01 (#318) remain
prerequisite evidence; they do not add steps to this scenario.

## Environment Variables

| Variable | Effect |
| --- | --- |
| `PLOYZ_DIND_E2E=1` | Gate: without it every DinD test early-returns (so `cargo test --workspace` stays fast). |
| `PLOYZ_DIND_KEEP=1` | Skip teardown after the test and print the run id / container names for debugging. |
| `PLOYZ_DIND_MACHINE_IMAGE` | Machine image tag (default `ployz-dind-machine:local`). |
| `PLOYZ_DIND_ARTIFACT_DIR` | Host directory with the linux binaries (default `/tmp/ployz-dind-machine-target/release`). |
| `PLOYZ_DIND_TARGET_DIR` | Build target dir used by the build script and the wrapper's marker file (default `/tmp/ployz-dind-machine-target`). |
| `PLOYZ_DIND_BUILDER_IMAGE` | Shared native/eBPF builder image (default `ployz-dind-builder:rust-1.91-bookworm-v2`). |
| `PLOYZ_DIND_WORKERS` | Concurrent compatible groups: `1`, `2` (default), or `3`. The heavyweight acceptance group stays serial. |

## Evidence

On any assertion failure the harness dumps per-machine evidence to:

```text
target/dind-evidence/<run_id>/<machine>/
  journal.txt              # journalctl -u nats-server -u 'ployzd-*'
  systemctl-failed.txt
  docker-ps.txt            # inner docker ps -a
  authorized-users.conf    # the NATS authority file (recovery evidence)
```

Logs are evidence, not the audience: tests assert on operation status and
events; these dumps exist so a failed gated run leaves something
inspectable behind. Every group and nested scenario also emits a stable
`DIND_TIMING name=... status=... elapsed_ms=...` line.

## Cleanup

Every Docker resource the harness creates (containers, networks) carries the
label `dev.ployz.dind.managed=true` plus a per-run
`dev.ployz.dind.run=<run_id>`. Each run sweeps stale labeled resources
before provisioning, and:

```sh
scripts/dind-clean.sh
```

removes everything labeled, any time — after a crashed run, a `Ctrl-C`, or a
`PLOYZ_DIND_KEEP=1` debugging session. Verify with:

```sh
docker ps -a --filter label=dev.ployz.dind.managed=true
```
