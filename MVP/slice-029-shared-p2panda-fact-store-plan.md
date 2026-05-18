---
title: Slice 029 Shared p2panda Fact Store Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-028-p2panda-machine-remove-facts.md
external:
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html
---

# Slice 029 Shared p2panda Fact Store Plan

## Problem Frame

Slice 028 made p2panda the durable fact source for machine remove, matching the
deploy p2panda adapter and routing serving writer. That also crossed the line
where shared-store plumbing is no longer harmless local glue:

- `mvp-deploy-p2panda` owns a cloneable `Arc<Mutex<PandaFactStore>>` wrapper,
  exports operations, implements `FactSource`, and implements routing's serving
  sink.
- `mvp-machine-p2panda` owns the same wrapper shape plus trust/import helpers,
  implements `FactSource`, and implements routing's serving sink.
- `mvp-routing-p2panda` owns a generic sink trait even though the sink is not
  routing-specific anymore.
- `mvp-e2e` carries another local cloneable p2panda wrapper for volume transfer
  facts.

The previous decision-ledger note said not to promote `PandaServingFactSink`
until a second command repeated the boundary. That condition is now met. The
next slice should pay down the duplicate p2panda adapter shell before adding
more product proofs on top of it.

This is a maintenance-surface slice, not a product behavior slice. Its purpose
is to make the existing product proofs easier to extend without growing a new
store wrapper in each command crate.

## Requirements Trace

- `VISION.md`: keep operation primitives legible and avoid hidden in-cluster
  complexity.
- `MVP/overall-plan.md`: prefer p2panda signed operations behind `FactSource`,
  keep new implementation isolated under `MVP/`, and make business behavior
  require less orchestration glue.
- `MVP/primitive-decisions.md`: update the stale `PandaServingFactSink` decision
  now that multiple commands repeat the same storage boundary.
- Slice 028 review feedback: remove duplicated cloneable p2panda wrapper code
  while preserving domain-specific command writers and structured error mapping.

## Dependency Scout

No new crate should be added for this slice.

The current p2panda crate surface remains the right low-level substrate. The
`p2panda-store` docs describe operation/log store traits and explicitly leave
log design and validation to the application. Ployz already owns that validation
through `PandaFactStore`, trusted author keys, trusted replica import, and
Ployz fact envelopes, so the reuse point is our wrapper around `PandaFactStore`,
not a new direct dependency on p2panda store traits.

The repeated wrapper uses `tokio::sync::Mutex` because write/import paths hold
the store while awaiting p2panda ingestion. Tokio's mutex is the standard tool
for an async lock that may be held across `.await`. The plan keeps that lock
placement, but centralizes the unavailable/try-lock behavior so every adapter
does not invent its own wording and shape.

## Scope

In scope:

- Add one neutral shared p2panda fact-store handle under `mvp-p2panda-facts`.
- Remove the routing-owned generic p2panda sink contract by making the routing
  writer depend on the shared handle directly.
- Convert deploy, machine, and routing tests to use the shared handle where it
  removes duplicated store mechanics.
- Keep domain-specific writers in domain adapter crates.
- Update the primitive decision ledger and slice notes.

Out of scope:

- No command behavior changes.
- No new fact schema.
- No iroh or p2panda-net transport changes.
- No production migration outside `MVP/`.
- No new product proof scenario unless implementation reveals an uncovered
  regression that cannot be represented by current tests.

## Design Decisions

### Shared Handle Belongs In `mvp-p2panda-facts`

The shared wrapper is substrate-level. It should live next to `PandaFactStore`,
not in routing, deploy, machine, or E2E. `mvp-p2panda-facts` already owns the
fact-store contract and implements `FactSource` for the raw store, so it is the
least surprising place for a cloneable async handle over that store.

Proposed names:

- `SharedPandaFactStore`

Exact names can change during implementation if a clearer local naming pattern
appears, but the ownership boundary should not.

### Domain Writers Stay Domain-Specific

The shared handle writes opaque key/payload facts and exposes import/export,
trust, preflight, and `FactSource` delegation. It should not know what a deploy,
machine removal, serving commit, or volume ownership fact means.

Keep these in their existing crates:

- `PandaDeployFactWriter`
- `PandaMachineFactWriter`
- `PandaServingFactWriter`
- volume-transfer E2E's command-specific write metrics and ownership/lease
  mapping

This preserves the useful part of the current architecture: business errors
remain branchable in command domains, while storage mechanics are shared.

`SharedPandaFactStore` returns only p2panda/storage-level outcomes and
`PandaFactError`s for opaque key/payload writes. Domain writers are the only
layer that converts those outcomes/errors into `RoutingError`, `DeployError`,
`MachineRemoveError`, volume metrics, or command-specific write outcomes.

### Imports Stay Explicit

Do not hide trust or replay behind an automatic `sync_from` convenience method.
The trust boundary matters:

- authored writes require the author's key to bind to the session principal,
- replica imports require a trusted replica principal,
- read-only projection principals must not become import authorities.

The shared handle can provide small methods like `trust_author_key`,
`trust_replica_peer`, `import_operation`, `import_replica_operation`, and
`export_operations`, but call sites should still show which session is importing
and why.

There are two replay modes and this slice must preserve the distinction:

- author-validated local import after trusting the original author key,
- trusted replica import through a replica principal.

Deploy restart recovery currently uses author-key import. Machine remove uses
trusted replica import. Do not collapse those into one helper.

### Delete The Generic Sink Unless It Remains Earned

`mvp-routing-p2panda` should remain the adapter that turns a
`ServingCommitPlan` into a p2panda fact. It should not own the generic trait
that machine and deploy import merely to write any fact payload.

After `SharedPandaFactStore` exists, `PandaServingFactWriter` should depend on
that shared handle directly. Do not move `PandaServingFactSink` into
`mvp-p2panda-facts` by default. Only introduce a shared trait if implementation
still leaves at least two non-test implementations after deploy and machine are
converted.

## Implementation Units

### Unit 1: Add Shared Store Handle

Files:

- `MVP/p2panda-facts/src/lib.rs`
- optionally `MVP/p2panda-facts/src/shared.rs`

Add a cloneable shared store handle around `PandaFactStore`.

Expected capabilities:

- construct from `PandaFactStore`,
- write fact payloads,
- export operations,
- trust author keys,
- trust replica peers,
- import author-validated operations,
- import replica operations,
- check write preflight through a synchronous non-blocking method such as
  `try_can_write_fact(&self, session, key) -> Result<bool, FactSourceError>`,
- implement `FactSource`,
- provide the direct write surface needed by routing/deploy/machine adapters.

Tests:

- shared handle writes inserted and already-present outcomes,
- shared handle rejects unauthorized writes with the original
  `PandaFactError`,
- shared handle exports operations and imports them through a trusted replica
  session,
- shared handle preserves direct author-key import for callers that need it,
- shared preflight uses `try_lock()` and does not require `.await`,
- read-side `FactSource` delegation returns candidates and payloads,
- read-side `FactSource` delegation returns `Unavailable` when the write lock
  is held.

### Unit 2: Delete The Routing-Owned Sink

Files:

- `MVP/routing-p2panda/src/lib.rs`
- `MVP/routing-p2panda/Cargo.toml` if dependency cleanup is needed

Replace `PandaServingFactSink` by making `PandaServingFactWriter` depend on
`SharedPandaFactStore` directly, unless implementation proves there are at least
two non-test sink implementations after the conversion.

Tests:

- serving writer records inserted and already-present serving outcomes using
  the shared store handle,
- serving writer maps p2panda conflicts to `RoutingError::ServingFactConflict`,
- serving writer still rejects unauthorized serving writes through the p2panda
  fact-store error path.

### Unit 3: Convert Deploy And Machine Adapters

Files:

- `MVP/deploy-p2panda/src/lib.rs`
- `MVP/deploy-p2panda/Cargo.toml`
- `MVP/machine-p2panda/src/lib.rs`
- `MVP/machine-p2panda/Cargo.toml`
- `MVP/e2e/src/deploy_restart_recovery_contract.rs`
- `MVP/e2e/src/machine_remove_contract.rs`

Replace duplicated wrapper internals with the shared handle.

Preferred shape:

- If type aliases keep existing call sites clear, use aliases such as
  `PandaDeployFactStore = SharedPandaFactStore`.
- If domain names make tests and E2E output much clearer, keep thin newtypes,
  but their bodies should delegate to the shared handle rather than repeat
  locking, `FactSource`, sink, import, and export logic.

Tests:

- deploy p2panda writer tests remain behavior-identical,
- machine p2panda writer tests remain behavior-identical,
- deploy restart recovery preserves direct author-key import,
- machine remove preserves trusted replica import,
- authorization failures remain structured command errors rather than generic
  store strings.

Dependency cleanup:

- Remove `mvp-routing-p2panda` from production dependencies of deploy and
  machine adapters.
- Keep `mvp-routing-p2panda` only as a dev-dependency where adapter tests
  instantiate `PandaServingFactWriter`.

### Unit 4: Optional Volume E2E Store Shell Check

Files:

- `MVP/e2e/src/volume_transfer_contract.rs`

The volume transfer proof still has E2E-local p2panda glue by design. This unit
does not promote a `mvp-volume-p2panda` crate. It is deferred unless
deploy/machine/routing finish with no shared API expansion and the volume
fixture can delegate generic store mechanics without changing command behavior.

Keep E2E-local:

- volume ownership write metrics,
- lease write metrics,
- volume-specific outcome conversion,
- race fixture wrappers.
- `VolumeFactWriter::preflight` mapping to `VolumeError`, while delegating the
  underlying sync check to `SharedPandaFactStore::try_can_write_fact` if this
  unit lands.

Move to shared handle:

- `Arc<Mutex<PandaFactStore>>`,
- export operations,
- `FactSource` delegation,
- trusted import/replay helpers if the call site already needs them.

Tests:

- If this file is touched, `volume-transfer-contract` still proves lease claim,
  ownership commit, conflict behavior, and metrics.
- If this file is not touched, the slice report records why volume remains the
  next extraction candidate.

### Unit 5: Documentation And Decision Ledger

Files:

- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md` only if the slice changes the next-proof map
- `MVP/slice-029-shared-p2panda-fact-store.md`

Update the decision ledger:

- replace the stale "do not promote `PandaServingFactSink`" note,
- record that Slice 029 centralizes cloneable p2panda store mechanics,
- state that domain fact writers remain outside the shared store handle,
- state what should not be centralized yet.

The slice report should include a semantic-leverage accounting section:

- shared substrate LOC added once,
- duplicate wrapper LOC removed from domain adapters, and from E2E if Unit 4
  lands,
- domain writer LOC retained,
- tests and docs touched.

## Verification Plan

Targeted tests:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-routing-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-deploy-p2panda`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-machine-p2panda`

Product E2Es:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- deploy-restart-recovery-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- machine-remove-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract`
  if `MVP/e2e/src/volume_transfer_contract.rs` is touched

Closeout:

- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts -p mvp-routing-p2panda -p mvp-deploy-p2panda -p mvp-machine-p2panda -p mvp-e2e --all-targets -- -D warnings`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`

## Risks

- Over-extracting a store facade that starts to know deploy/machine/volume
  semantics. Keep the shared handle payload-shaped.
- Hiding import authority in convenience helpers. Keep trust/import sessions
  visible at call sites.
- Losing structured domain errors by mapping every p2panda error into strings.
  Domain writers must keep their current branchable error mapping.
- Touching volume E2E too deeply. If converting the fixture forces command
  behavior changes, stop at deploy/machine/routing and document volume as the
  next extraction candidate.
- Accidentally changing lock behavior. Preserve async write locking and
  non-blocking `FactSource` reads unless a test demonstrates a better shape.

## Done Criteria

- No production adapter crate owns its own cloneable
  `Arc<Mutex<PandaFactStore>>` wrapper.
- `mvp-routing-p2panda` no longer owns a generic p2panda sink trait imported by
  deploy or machine.
- Deploy, machine, routing, and volume proofs still use the same fact schemas
  and command semantics.
- Trusted replica replay remains explicit and tested.
- Direct author-key import remains distinct from trusted replica import.
- Existing E2E proofs pass with the same scenario names.
- `MVP/primitive-decisions.md` explains why the shared handle now exists and
  what remains deliberately domain-specific.
- `MVP/slice-029-shared-p2panda-fact-store.md` exists and records substrate LOC
  added once, duplicate wrapper LOC removed or deferred, domain writer LOC
  retained, and tests/docs touched.
- Either `MVP/e2e/src/volume_transfer_contract.rs` delegates generic store
  mechanics to `SharedPandaFactStore`, or the slice report records why volume
  remains the next extraction candidate.
