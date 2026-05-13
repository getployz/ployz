---
title: "refactor: Finish ployzd Boundary Extraction"
type: refactor
status: completed
date: 2026-05-13
origin: docs/plans/2026-05-13-001-refactor-idiomatic-crate-boundaries-plan.md
---

# refactor: Finish ployzd Boundary Extraction

## Summary

Finish the remaining handler-shaped and contract-leak work left after the
crate-boundary extraction pass. `ployzd` should remain the process edge:
request dispatch, daemon lifecycle, active mesh lookup, concrete NATS/RPC
transport, and backend construction. Feature crates should own feature
workflow state machines, pure policy, operation records, and reusable tests.

## Problem Frame

The previous slices removed `ployz-types`, extracted build and image workflow
logic, and introduced focused contract/backend crates. The largest remaining
debt is now concentrated in `ployzd` and one oversized orchestrator test file:

- `crates/ployzd/src/daemon/handlers/deploy.rs` still combines daemon adapter
  code, deploy/branch/migrate rendering, lock handling, volume-move RPC, and
  many tests.
- `crates/ployzd/src/daemon/handlers/volume/zfs.rs` still owns transfer record
  storage, transfer state transitions, ZFS command orchestration, peer RPC
  adapter code, and tests.
- `crates/ployzd/src/daemon/handlers/machine/storage.rs` still combines
  storage-promotion policy, operation recording, peer RPC, bootstrap file
  mutation, and runtime restarts.
- `ployz-cert-api` still provides
  `wait_for_http01_challenge_visible`, which is runtime polling code in a
  contract crate.
- `crates/ployz-orchestrator/src/deploy/tests.rs` is a single 9,659-line test
  module.
- `crates/ployzd/src/daemon/handlers/mod.rs` still centralizes lane
  classification and all control/node dispatch arms.

## Requirements

- R1. `ployz-cert-api` must expose certificate contracts only; runtime polling
  implementations belong in `ployz-cert-acme`, `ployz-orchestrator`, or the
  daemon edge.
- R2. Deploy/branch/migrate behavior must keep current request/response
  semantics while moving non-daemon helpers and workflow policy out of the
  monolithic handler file.
- R3. ZFS volume transfer state, record storage, and local ZFS workflow helpers
  must live with `ployz-storage-zfs`; `ployzd` keeps active mesh and peer RPC
  adapters.
- R4. Machine storage promotion preflight and transition policy must be
  isolated from daemon transport/restart wiring.
- R5. Orchestrator deploy tests must be split into a module tree by behavior
  area without changing production behavior.
- R6. The central daemon dispatcher must shrink by moving lane classification
  and grouped dispatch functions into feature modules.
- R7. Existing wire shapes, public request variants, response codes, payloads,
  and operation record semantics must remain stable.
- R8. Verification must include targeted crate tests plus boundary checks for
  the moved contracts.

## Scope Boundaries

- Do not redesign deploy semantics, ZFS transfer protocol, machine promotion
  protocol, or ACME issuance behavior.
- Do not introduce compatibility shims for old internal module paths.
- Do not split `ployzd` into a reusable daemon library in this pass.
- Do not move concrete NATS subjects, RPC timeout policy, active mesh access,
  or runtime restart wiring below `ployzd`.
- Do not change the external `DaemonRequest`, `NodeRequest`, or payload schema
  except for internal import locations.

## Context & Patterns

- `crates/ployz-image/src/push.rs` is the current extraction pattern: a feature
  crate owns workflow logic through an explicit service context and peer-client
  port, while `ployzd` implements the port with NATS node RPC.
- `docs/solutions/architecture-patterns/extract-feature-workflows-behind-daemon-adapters-2026-05-13.md`
  captures the same pattern and should guide deploy/ZFS/machine slices.
- `crates/ployz-orchestrator/src/deploy/participant.rs` already defines a
  deploy participant port. `ployzd` should adapt to it; orchestration policy
  should not depend on daemon state.
- `crates/ployz-storage-zfs/src` already owns ZFS command execution and dataset
  semantics. Transfer state and record storage are ZFS feature behavior, not
  daemon request routing.
- `crates/ployzd/src/daemon/cert_coordination.rs` already has daemon-local HTTP
  challenge readiness logic; contract-level readiness implementations should be
  moved away from `ployz-cert-api`.

## Key Technical Decisions

| Decision | Rationale |
|---|---|
| Start with the cert contract leak | It is small, mechanically verifiable, and restores the rule that `*-api` crates define contracts instead of runtime loops. |
| Split deploy in-place before extracting a new crate | Deploy already has most reusable behavior in `ployz-orchestrator`; the first value is separating daemon adapters, render helpers, volume move RPC, and tests into modules with clear ownership. |
| Move ZFS transfer state into `ployz-storage-zfs` | Transfer records and transitions are ZFS feature state, while the daemon only supplies data directory roots, active mesh membership, and peer transport. |
| Isolate machine storage promotion as policy plus edge adapters | Promotion decisions can be tested against store/membership inputs without NATS peer RPC or runtime restart concerns. |
| Split orchestrator deploy tests by behavior, not arbitrary line count | The test file should mirror deploy domains: preview/plan, apply lifecycle, prepared deploys, managed domains, volume moves, image availability, and helpers. |
| Move dispatcher classification before handler extraction is complete | Lane rules and dispatch grouping are daemon concerns, but they do not need to live in the same central file as every request arm. |

## Implementation Units

### U1. Move HTTP-01 Readiness Runtime Out of cert-api

**Goal:** Make `ployz-cert-api` contract-only by moving local HTTP-01 challenge
visibility polling into an implementation crate.

**Requirements:** R1, R8

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-cert-api/src/lib.rs`
- Modify: `crates/ployz-cert-api/Cargo.toml`
- Modify: `crates/ployz-cert-acme/src/lib.rs`
- Modify: `crates/ployz-cert-acme/src/instant_acme_issuer.rs`
- Modify: `crates/ployz-orchestrator/src/certificates.rs`
- Test: `crates/ployz-orchestrator/src/certificates.rs`

**Approach:** Keep `Http01ChallengeReadiness` as the contract in
`ployz-cert-api`, move `LocalHttp01ChallengeReadiness` and polling constants
into `ployz-cert-acme`, and update orchestrator imports/tests to use the
implementation crate where they need concrete readiness.

**Test scenarios:**
- Already-visible challenge returns immediately.
- Late-written challenge is observed before timeout.
- Missing challenge returns the same timeout class.
- `ployz-cert-api` compiles without `tokio` time/runtime dependencies.

**Verification:** `cargo check -p ployz-cert-api`,
`cargo test -p ployz-cert-acme`, and
`cargo test -p ployz-orchestrator http01_challenge_visibility`.

### U2. Split Deploy Handler Adapters and Helpers

**Goal:** Reduce `daemon/handlers/deploy.rs` by moving deploy edge modules into
focused files while keeping `ployzd` as the composition root.

**Requirements:** R2, R7, R8

**Dependencies:** U1

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/deploy.rs`
- Create: `crates/ployzd/src/daemon/handlers/deploy/apply.rs`
- Create: `crates/ployzd/src/daemon/handlers/deploy/branch.rs`
- Create: `crates/ployzd/src/daemon/handlers/deploy/migrate.rs`
- Create: `crates/ployzd/src/daemon/handlers/deploy/locks.rs`
- Move or keep: `crates/ployzd/src/daemon/handlers/deploy/manifest_render.rs`
- Move or keep: `crates/ployzd/src/daemon/handlers/deploy/node.rs`
- Move or keep: `crates/ployzd/src/daemon/handlers/deploy/responses.rs`
- Move or keep: `crates/ployzd/src/daemon/handlers/deploy/volume_transfer.rs`
- Test: affected deploy handler tests under the same module tree.

**Approach:** Convert `deploy.rs` into `deploy/mod.rs`, then move cohesive
sections without changing public handler method names. Keep daemon methods on
`DaemonState` where they directly gather active mesh, NATS store, locks, or
certificate coordination. Move pure helpers and grouped tests with their
modules.

**Test scenarios:**
- Preview/prepare/apply/prepared apply keep existing no-mesh and invalid
  manifest responses.
- Branch prepare/apply replay behavior stays stable.
- Migrate manifest rendering still rejects invalid namespace/service/mount
  shapes and emits sorted move hints.
- Lock-loss failure marking still distinguishes pre-commit and post-commit
  phases.

**Verification:** `cargo test -p ployzd deploy --bin ployzd`.

### U3. Move ZFS Transfer State into ployz-storage-zfs

**Goal:** Move ZFS transfer records, transitions, store, validation, and local
transfer helpers out of the daemon handler.

**Requirements:** R3, R7, R8

**Dependencies:** None

**Files:**
- Modify: `crates/ployz-storage-zfs/Cargo.toml`
- Modify: `crates/ployz-storage-zfs/src/lib.rs`
- Create: `crates/ployz-storage-zfs/src/transfer.rs`
- Modify: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`
- Test: `crates/ployz-storage-zfs/src/transfer.rs`
- Test: `crates/ployzd/src/daemon/handlers/volume/zfs.rs`

**Approach:** Move `TransferStatus`, `TransferState`, `TransferRecord`,
`TransferStore`, claim handling, startup recovery, ID validation, and
finalization helpers into `ployz-storage-zfs::transfer`. Keep daemon handlers
responsible for translating API requests/responses, locating local volumes,
constructing `ZfsDriver<TokioShellRunner>`, and invoking peer RPC.

**Test scenarios:**
- Startup recovery marks running transfers interrupted.
- Move claims are idempotent and reclaimable after stale/invalid records.
- Transfer ID validation rejects path segments.
- Transfer listing stays ordered by `started_at` descending then ID ascending.
- Finalization records success/failure evidence identically.

**Verification:** `cargo test -p ployz-storage-zfs transfer` and
`cargo test -p ployzd volume_zfs --bin ployzd`.

### U4. Isolate Machine Storage Promotion Policy

**Goal:** Move storage-promotion preflight and record transition planning out
of `DaemonState` methods.

**Requirements:** R4, R7, R8

**Dependencies:** None

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/machine/storage.rs`
- Create: `crates/ployzd/src/daemon/handlers/machine/storage/promotion.rs`
- Test: `crates/ployzd/src/daemon/handlers/machine/storage/promotion.rs`

**Approach:** Extract pure validation and promotion planning into a policy
module that consumes machine membership records, requested replica policy, and
local authority state. Keep peer RPC (`MachineStoragePromoteSelf`,
`MachineStorageRestoreSelf`), bootstrap file writes, operation store writes,
and runtime restarts in the daemon adapter.

**Test scenarios:**
- Duplicate targets fail before side effects.
- Non-active/non-storage/non-candidate targets produce stable failure causes.
- Replica count mismatch includes current authority and target counts.
- Local non-authority execution fails loudly.

**Verification:** `cargo test -p ployzd machine_storage --bin ployzd`.

### U5. Split Orchestrator Deploy Tests into Module Tree

**Goal:** Replace the single large deploy test file with behavior-focused test
modules.

**Requirements:** R5, R8

**Dependencies:** U1

**Files:**
- Delete or shrink: `crates/ployz-orchestrator/src/deploy/tests.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/mod.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/helpers.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/preview.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/apply.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/prepared.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/managed_domains.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/volume_moves.rs`
- Create: `crates/ployz-orchestrator/src/deploy/tests/image_availability.rs`

**Approach:** Move tests mechanically by topic, putting common fakes/builders
in `helpers.rs`. Keep test names and assertions stable unless compilation
requires imports to be localized.

**Test scenarios:** Existing deploy test scenarios must remain present after
the split; no behavior additions are required for this unit.

**Verification:** `cargo test -p ployz-orchestrator deploy::tests`.

### U6. Shrink Central Daemon Dispatcher

**Goal:** Move lane classification and grouped control/node dispatch into
dedicated dispatcher modules.

**Requirements:** R6, R7, R8

**Dependencies:** U2, U3, U4

**Files:**
- Modify: `crates/ployzd/src/daemon/handlers/mod.rs`
- Create: `crates/ployzd/src/daemon/handlers/lane.rs`
- Create: `crates/ployzd/src/daemon/handlers/control_dispatch.rs`
- Create: `crates/ployzd/src/daemon/handlers/node_dispatch.rs`
- Test: moved dispatcher lane tests.

**Approach:** Preserve the public `DaemonState` entrypoints used by the IPC
listener, but move classification and request-group matches out of the central
module. Dispatch remains daemon-owned because it references concrete handler
methods and lane policy.

**Test scenarios:**
- Exclusive control requests still route exclusively.
- Shared control requests still route shared.
- Self-targeting drain/standby still routes exclusively.
- Node peer mutation requests still route exclusively while deploy/ZFS/image
  peer work stays shared.

**Verification:** `cargo test -p ployzd request_lane --bin ployzd`.

## Verification Plan

- `cargo fmt --all`
- `cargo check -p ployz-cert-api`
- `cargo test -p ployz-cert-acme`
- `cargo test -p ployz-orchestrator deploy::tests`
- `cargo test -p ployzd deploy --bin ployzd`
- `cargo test -p ployzd volume_zfs --bin ployzd`
- `cargo test -p ployzd machine_storage --bin ployzd`
- `just test-boundaries`

## Deferred to Later

- A separate `ployz-deploy` crate can be considered after the daemon deploy
  adapter has been split and the remaining reusable logic is visible.
- A separate machine feature crate can be considered after storage-promotion
  policy is isolated from daemon transport and runtime restart effects.
- Full `ployzd` library/binary separation remains out of scope until handler
  ownership is materially smaller.
