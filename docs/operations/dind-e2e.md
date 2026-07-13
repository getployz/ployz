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
- Roughly 4 GB of memory per two-machine cluster pair. The suite serializes
  (`--test-threads=1`) so only one cluster exists at a time — do not run
  scenarios in parallel.
- Host-arch Linux artifacts. Everything is built for the Docker server's
  architecture (`docker info --format '{{.Architecture}}'`); nothing
  hardcodes amd64, so Apple Silicon hosts build and run arm64 throughout.
- No registry access at test time: the machine image bakes `nats-server` and
  the workload image tarball, and the ployz binaries are volume-mounted.

## One Command

```sh
scripts/dind-e2e.sh
```

This rebuilds the linux release artifacts and the machine image, then runs
the gated suite with `PLOYZ_DIND_E2E=1` and `--test-threads=1`.

## Manual Pieces

Build the machine image and artifacts explicitly:

```sh
scripts/build-dind-machine-image.sh
```

This produces the `ployz-dind-machine:local` image (systemd PID 1, inner
dockerd, `nats-server`, baked workload tarball) and host-arch release
binaries under `/tmp/ployz-dind-machine-target/release/` that the harness
volume-mounts read-only at `/opt/ployz/artifacts` inside every machine.

Run the suite directly:

```sh
PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster -- --test-threads=1
```

## Running A Single Scenario

Pass the test name (or any prefix) as a filter:

```sh
scripts/dind-e2e.sh scenario_machine_add_via_join_bundle
```

Scenarios in `crates/ployz-e2e/tests/dind_cluster.rs`:

- `boots_machine_image` — smoke: one machine reaches systemd + inner-docker
  readiness; teardown leaves nothing labeled behind.
- `scenario_init_and_activate_first_machine` — Host Runner first-machine install +
  `init activate-first-machine` through the real product path; mint event
  sequence, unit states, authority file, and bootstrap evidence.
- `scenario_machine_add_via_join_bundle` — `machine add` + the
  `scripts/ployz.sh` join flow on an edge machine; per-machine credential
  minting, never-shrinking authority file, single-use join token.
- `scenario_deploy_restart_invisibility_and_auth_rejection` — cross-machine
  deploy serving through both gateways, daemon restarts invisible to the
  data plane, and auth rejection (bad NKey, no TLS, scope violations, inbox
  isolation) against the live cluster.
- `scenario_v1_acceptance_five_steps_on_fresh_machines` — the five-step v1
  acceptance journey on three fresh machines: form the cluster, deploy Umami
  with PostgreSQL, diagnose failed deploys, prove Core downtime is invisible
  to traffic, then retry and roll back through explicit operations.

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
inspectable behind.

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
