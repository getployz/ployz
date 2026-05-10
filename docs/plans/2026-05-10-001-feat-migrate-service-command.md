---
status: active
created: 2026-05-10
module: deploy
tags:
  - migrate
  - deploy
  - zfs
  - e2e
---

# Migrate Service Command

## Problem Frame

Ployz has the deploy-time primitives needed to move a single-scope volume and
restart attached services on the target machine, but the operator-facing
primitive is still hidden behind hand-authored deploy manifests. The next
product slice should expose the north-star command shape from `VISION.md`:
`ployzctl migrate <workload> --to <machine>`.

This plan makes migration a thin command over the existing deploy machinery.
The command does not introduce a separate migration executor. It renders a
deploy manifest with volume move intent from committed cluster state, then
uses the normal deploy preview/apply path so phase planning, validation,
locking, participant RPCs, ZFS transfer, state commits, and deploy evidence
remain centralized.

## Scope

In scope:

- Add `ployzd migrate apply <namespace/service> --to <machine>` with `ployzctl`
  passthrough support already provided by `crates/ployzctl/src/main.rs`.
- Add `ployzd migrate preview <namespace/service> --to <machine>` to return
  the generated deploy preview without applying.
- Add `ployzd migrate render-manifest <namespace/service> --to <machine>` to
  return the generated deploy manifest for
  inspection and automation.
- Infer source machines server-side from committed `VolumeRecord`s.
- Move all managed volumes mounted by the service when they are movable.
- Reject unsupported service migration requests with structured, actionable
  daemon errors before invoking deploy apply.
- Rework the real-ZFS e2e scenario to prove the operator primitive: a service
  with an attached managed volume moves from `founder` to `peer` and preserves
  data.

Out of scope:

- Stateless service-only movement. Existing `ServiceIntent::Move` planning is
  not yet supported, so this slice should reject services with no movable
  managed volumes.
- Cross-namespace migration, branching, portal, or promotion flows.
- Machine remove orchestration that batches all workloads off a node.
- Automatic source inference in the CLI. Source inference belongs in the daemon
  because only the daemon has authoritative committed volume records.

## Requirements

1. `migrate apply <namespace/service> --to <machine>` creates exactly one
   deploy apply request representing the service migration.
2. `migrate preview <namespace/service> --to <machine>` returns the same
   preview a hand-authored deploy manifest with equivalent volume move hints
   would return.
3. `migrate render-manifest <namespace/service> --to <machine>` returns the
   generated `DeployManifest` and does not apply or preview it.
4. The daemon must export the current namespace manifest, find the named
   service, inspect its managed volume mounts, read committed volume records,
   and add one `VolumeIntent::Move` hint per movable mounted volume.
5. The command must reject ambiguous or unsafe requests:
   - unknown service,
   - service has no managed volume mounts,
   - managed mount has no committed `VolumeRecord`,
   - volume is already on the target,
   - service mounts duplicate managed volumes,
   - any mounted volume has unsupported scope for this primitive.
6. Existing deploy validation remains responsible for participant reachability,
   target existence, placement compatibility, global placement rejection, and
   ZFS execution support.
7. The real-ZFS e2e must verify workload-level behavior, not only raw ZFS
   transfer mechanics.

## Key Decisions

### Server-side migrate request

Add a daemon request for migration rather than making the CLI export and patch
manifests. The CLI cannot safely infer `from_machine` because `DeployExport`
deliberately emits declarations, not live volume ownership. The daemon can read
`VolumeRecord`s from the store at decision time and then reuse existing deploy
handlers.

Relevant files:

- `crates/ployz-api/src/deploy.rs`
- `crates/ployz-api/src/request.rs`
- `crates/ployzd/src/cli.rs`
- `crates/ployzd/src/request_builder.rs`
- `crates/ployzd/src/daemon/handlers/mod.rs`
- `crates/ployzd/src/daemon/handlers/deploy.rs`

### Workload means service for this slice

The user-facing argument is `namespace/service`. This maps cleanly to today’s
stored deploy release model and keeps the command explicit. Future workload
aliases can be added at the CLI/API boundary without changing the deploy
primitive.

### Move mounted managed volumes together

The migration command should migrate the service by moving every mounted
managed volume owned by the service, then letting deploy planning pin the
attached service to the target. This matches the existing volume move primitive:
volume movement carries attached services with it.

### Render mode is an inspection surface

`migrate render-manifest` makes the primitive legible and automation-friendly.
It is also a cheap test surface: unit tests can verify the exact generated
intent without invoking participant RPCs or ZFS.

## Implementation Units

### 1. API and CLI request surface

Files:

- `crates/ployz-api/src/deploy.rs`
- `crates/ployz-api/src/request.rs`
- `crates/ployzd/src/cli.rs`
- `crates/ployzd/src/request_builder.rs`
- `crates/ployzd/src/main.rs`

Work:

- Define a migrate request type with namespace, service, target machine, and
  mode: apply, preview, or render manifest.
- Add `DaemonRequest::MigrateService`.
- Add `ployzd migrate apply|preview|render-manifest <namespace/service> --to
  <machine>`.
- Parse `namespace/service` with the same strict shape as existing
  `namespace/volume` parsing.
- Encode request-builder output directly as `DaemonRequest::MigrateService`.

Tests:

- `crates/ployzd/src/main.rs`: parse apply, preview, and render-manifest forms.
- `crates/ployzd/src/request_builder.rs`: request-builder test for service ref
  parsing and request mode selection.
- Missing action and invalid `namespace/service` should fail before daemon
  dispatch.

### 2. Daemon manifest renderer for migration

Files:

- `crates/ployzd/src/daemon/handlers/mod.rs`
- `crates/ployzd/src/daemon/handlers/deploy.rs`

Work:

- Route `DaemonRequest::MigrateService` in the shared handler lane.
- Implement a helper that:
  - requires an active mesh,
  - acquires the namespace deploy lock before rendering for apply-mode
    migration,
  - exports the namespace manifest with `export_manifest`,
  - finds the requested service,
  - collects managed `MountSource::Volume` mounts,
  - reads each committed volume via the store,
  - validates the current machine and target are distinct,
  - injects `DeployIntent { volumes: [...] }` while preserving any future-safe
    existing intent shape from the exported manifest,
  - returns the manifest, deploy preview, or deploy apply depending on mode.
- Prefer structured error codes such as `MIGRATE_RENDER_FAILED` for pre-deploy
  failures. Deploy apply/preview failures should continue to use deploy error
  response handling.

Tests:

- `crates/ployzd/src/daemon/handlers/deploy.rs`: generated manifest contains
  one move hint with committed `from_machine` and requested `to_machine`.
- Missing service returns a migrate failure before deploy.
- Service with no managed volume mounts is rejected.
- Missing committed volume record is rejected.
- Already-on-target volume is rejected.
- Duplicate managed volume mounts are rejected.
- Multi-volume service emits deterministic hints sorted by volume name.

### 3. Real-ZFS e2e migration scenario

Files:

- `crates/ployz-e2e/src/cli.rs`
- `crates/ployz-e2e/src/scenarios/mod.rs`
- `crates/ployz-e2e/src/scenarios/zfs_transfer_real_smoke.rs`
- `crates/ployz-e2e/src/scenarios/zfs_support.rs`

Work:

- Rework `zfs_transfer_real_smoke` into a workload migration scenario, or
  rename it if the file/enum churn is small.
- Keep it real-ZFS only.
- Deploy `default/db` with managed volume `default/data` on `founder`.
- Ensure the service command seeds the volume only when the value file is
  absent, so migration does not overwrite state after restart.
- Mutate the source value to prove the latest data transfers.
- Run `ployzd migrate apply default/db --to peer`.
- Verify the peer dataset exists, the value is preserved on peer, the `db`
  container is running on peer with the peer dataset bind, and no `db`
  container remains running on founder.

Tests:

- Local compile/test coverage for the e2e harness.
- Real scenario command: `cargo run -p ployz-e2e -- --zfs real --scenario <scenario>`
  when the environment supports real ZFS.

## Validation Plan

- `cargo test -p ployzd`
- `cargo test -p ployz-api`
- `cargo test -p ployz-e2e`
- `just test`
- `just test-all` before pushing because the slice touches `ployzd` and e2e
  behavior.
- Run the real-ZFS e2e scenario if the local environment has the required ZFS
  setup; otherwise rely on CI and call out that local real-ZFS execution was not
  available.

## Risks

- The command should not bypass deploy validation. The renderer must only
  synthesize intent and then hand off to existing deploy paths.
- Multi-volume services can be partially movable only if all mounted managed
  volumes are supported. This slice should fail the whole command rather than
  issuing partial intent.
- If the target machine is invalid, deploy preview/apply should surface the
  existing deploy planning error; the migrate renderer should not duplicate
  machine placement logic.
- The raw ZFS transfer e2e coverage will narrow if the existing scenario is
  reworked. That is acceptable because the product-level deploy move path still
  exercises ZFS send/receive through the supported operator primitive.
