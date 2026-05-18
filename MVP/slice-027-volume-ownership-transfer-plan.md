---
title: Slice 027 Volume Ownership Transfer Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/phased-command.md
  - crates/ployzd/src/daemon/handlers/deploy/volume_transfer.rs
  - crates/ployzd/src/daemon/handlers/volume/mod.rs
  - crates/ployzd/src/daemon/handlers/volume/transfer_listener.rs
  - crates/ployz-api/src/volume.rs
  - crates/ployz-e2e/src/scenarios/volume_clone_branch_real_smoke.rs
external:
  - https://docs.rs/libzetta
  - https://docs.rs/zfs/latest/zfs/
  - https://docs.rs/zfs-core/latest/zfs_core/
  - https://docs.rs/tokio-util/latest/tokio_util/sync/index.html
reviewed_by:
  - ce-feasibility-reviewer
  - ce-scope-guardian-reviewer
  - ce-product-lens-reviewer
---

# Slice 027 Volume Ownership Transfer Plan

## Problem Frame

The MVP now has reusable ACME and deploy command surfaces on top of the bus,
advisory leases, p2panda facts, projection, and serving-state primitives. The
next proof should be a product primitive that reuses those pieces without
growing the substrate again.

Volume movement is the right next canary because it is a real Ployz north-star
primitive: `ployzctl migrate <workload> --to <machine>` and `ployzctl
fork-volume` both depend on explicit, safe state movement. It also exercises
the E2E-6a gap: applying advisory lease fencing to a singleton resource that is
not ACME.

This slice does not implement real ZFS send/receive, a generic workflow engine,
or a reusable p2panda adapter crate. It builds the smallest honest MVP command
semantic for transferring volume ownership from one node to another:

- write a durable advisory lease claim for the volume before participant RPC,
- snapshot the current owner,
- receive the exact snapshot evidence on the target,
- write a durable ownership commit fact only after validated receive evidence,
- reject active conflicting holders and stale lease holders before unsafe
  mutation,
- report visible nodes at decision time.

The old volume transfer code is reference material for product invariants and
failure surfaces. It is not a porting target.

## Single Proof Target

`volume-transfer-contract` proves that one volume can move from `node-a` to
`node-b` as a foreground, lease-fenced command over the MVP primitives:

1. the command reads current ownership and lease facts before mutation,
2. an active conflicting holder fails before participant RPC,
3. the command writes a durable lease claim fact before participant RPC,
4. the current holder snapshots and the target receives through typed node RPC,
5. ownership commit validates transfer id, source owner, target node, snapshot
   id, snapshot guid, and received evidence,
6. ownership commit is the only durable authority change for the volume owner,
7. stale lease holders cannot commit after a newer epoch exists,
8. a dropped coordinator after ownership commit can recover the committed owner
   from p2panda facts without rerunning snapshot/receive,
9. a dropped coordinator before ownership commit does not report success and
   leaves no new owner fact.

## Requirements Trace

- `VISION.md`: volume movement is part of the north-star primitive surface:
  migrate workloads, fork volumes, branch environments, promote, and rollback.
  It must be one command with visible preconditions, bounded effects, clear
  result, and verification hooks.
- `MVP/e2e-proof-plan.md`: E2E-6a explicitly names volume ownership as a next
  real singleton-resource proof for advisory leases. E2E-9 asks for another
  representative business rule to measure semantic leverage.
- `MVP/overall-plan.md`: after Slice 026, the next slice should pay down
  product semantic leverage and reuse bus, p2panda facts, projection, leases,
  serving actors, or deploy adapters without growing foundational substrate.
- `MVP/primitive-decisions.md`: leases are advisory, conflict candidates feed
  deterministic reducers, the connected node is the command consistency
  boundary, and command results include visible nodes at decision time.
- `MVP/design-notes/phased-command.md`: volume transfer is the example
  multi-phase operation. This slice keeps the command explicit and records
  whether the `PhasedCommand` trigger has fired after implementation.

## Dependency Scout

Checked before planning on 2026-05-18:

- `libzetta` wraps `zpool(8)` and covers much of `libzfs_core`, but its docs
  describe a low-level operator interface. It is a candidate for a later real
  ZFS backend slice, not for this command-semantics proof.
- `zfs` and `zfs-core` exist on docs.rs, but the MVP currently needs a typed
  participant ABI and command fact model, not kernel/filesystem integration.
- `tokio-util::sync::CancellationToken` remains useful for real transfer
  listener shutdown/cancellation. This slice does not spawn long-lived transfer
  tasks, so adding it now would be plumbing without proof value.

Decision:

- Do not add a ZFS dependency in Slice 027.
- Do not add `mvp-volume-p2panda` yet. One E2E canary is not enough evidence
  for another adapter crate.
- Keep p2panda write/read harnessing E2E-local behind narrow `mvp-volume`
  traits.
- Copy the old code's useful invariants: source node must match the owner,
  transfer status is explicit, failed/interrupted transfer is a structured
  foreground error, and receive/ownership are not assumed from a send request.

## Scope

In scope:

- Add MVP-local volume domain and command code under `MVP/volume`.
- Add typed volume identities and participant RPC payloads.
- Add ownership commit facts with embedded transfer evidence.
- Add command-side lease fact read/write helpers using existing lease facts.
- Use advisory lease facts to fence ownership mutation.
- Add `volume-transfer-contract` to `mvp-e2e -- all`.
- Add semantic-leverage closeout metrics as a required acceptance artifact.
- Record whether this slice trips the `PhasedCommand` trigger.

Out of scope:

- Real ZFS snapshot/send/receive.
- Durable pre-commit transfer phase facts.
- Resume-before-ownership-commit behavior.
- Source cleanup/removal after ownership moves.
- Volume branch/fork-volume implementation.
- Workload migration/routing integration.
- Production storage backend selection.
- A generic `mvp-commands`/`PhasedCommand` runner.
- A reusable `mvp-volume-p2panda` adapter crate.
- Shared projection/cache support for volume facts.
- Quorum, witness acks, consensus, or strict cluster locks.
- Root workspace or existing crate changes outside `MVP/`.

## Design Decisions

### Volume Ownership Is The Durable Authority Boundary

Ownership commit is the only durable authority change:

```text
/facts/volume/<namespace>/<volume>/ownership/<epoch>
```

The payload records:

- namespace,
- volume id,
- owner node,
- source node,
- transfer id,
- snapshot id,
- snapshot guid,
- bytes transferred,
- lease holder,
- lease epoch,
- lease claim hash,
- visible nodes at decision time.

Reducers choose the current owner by `(epoch desc, content_hash asc)` and
surface superseded candidates. Slice 027 reads ownership directly from volume
facts; it does not add shared projection tables or SQLite cache.

### Advisory Lease Fences The Mutation

The volume lease resource must use the existing lease key shape:

```rust
LeaseResource::from_segments(["volume", namespace, volume])
```

Lease facts remain under the existing single encoded resource segment:

```text
/facts/lease/<encoded-resource>/claimed/<epoch>
```

Command entry:

1. reads current volume ownership candidates,
2. reads current lease candidates for the encoded volume resource,
3. fails with structured conflict if another active holder wins,
4. writes a durable `LeaseClaimed` fact through p2panda before participant RPC,
5. carries `(holder, epoch, claim_hash)` into the ownership mutation.

Before writing ownership commit, the command re-reads local lease state and
fails with `StaleLease` if a newer epoch exists or the claim hash no longer
matches.

### Participant RPC Is Explicit

Do not hide transfer work in background reconciliation.

Subjects:

```text
node.<node_id>.rpc.volume.snapshot
node.<node_id>.rpc.volume.receive
```

Payloads include source/target node ids, transfer id, namespace, volume id,
snapshot id, snapshot guid, and byte count where relevant. Participants reject
misaddressed requests and mismatched evidence, matching the old code's
source-node guard.

### No Pre-Commit Resume Claim

This slice does not persist planned/snapshot/received phase facts and does not
claim resume-before-commit. If the coordinator drops before ownership commit,
the command has not succeeded. The proof should show:

- no target ownership fact exists,
- a recovery read reports no committed owner change,
- no false success is presented to the operator.

Durable phase facts and idempotent resume belong to a later `PhasedCommand` or
real backend slice.

### PhasedCommand Stays Deferred For This Slice

The implementation should stay explicit so the pattern is visible. Closeout
must count repeated phase/resume shapes across deploy, machine remove, ACME,
and volume. If the trigger is now met, Slice 028 should plan `mvp-commands`;
do not smuggle it into Slice 027.

## Implementation Units

### Unit 1: Volume Domain And Fact Model

Files:

- `MVP/Cargo.toml`
- `MVP/volume/Cargo.toml`
- `MVP/volume/src/lib.rs`
- `MVP/volume/src/domain.rs`
- `MVP/volume/src/facts.rs`
- `MVP/volume/src/error.rs`
- `MVP/volume/src/tests.rs`

Work:

- Introduce typed identities: `VolumeNamespace`, `VolumeId`,
  `VolumeTransferId`, `VolumeSnapshotId`, and `VolumeSnapshotGuid`.
- Define `VolumeOwnershipFact` with validated transfer evidence and lease
  fencing fields.
- Define fact key and payload encode/decode helpers.
- Define reducer/read APIs:
  - read current ownership from `FactSource`,
  - select winner by `(epoch desc, content_hash asc)`,
  - return superseded candidates with author/hash evidence.
- Treat volume facts as volume-owned payloads. Do not add variants to
  `ProjectionFactPayload` or shared `FactKind` in this slice.

Test scenarios:

- Ownership fact key is namespace/volume/epoch scoped.
- Malformed payload returns structured error.
- Wrong fact shape returns structured error.
- Conflicting same-volume ownership candidates reduce deterministically.
- Superseded candidates retain author and content hash.
- Namespace and volume id validation rejects slash, control characters,
  whitespace-only, and empty values.
- Ownership fact must include lease holder, lease epoch, and lease claim hash.

Verification:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-volume
```

### Unit 2: Lease-Fenced Volume Transfer Command

Files:

- `MVP/volume/src/command.rs`
- `MVP/volume/src/wire.rs`
- `MVP/volume/src/tests.rs`

Work:

- Add a single concrete `VolumeTransferCommand` surface. Do not add a separate
  generic coordinator abstraction unless the implementation proves one is
  necessary.
- Add participant RPC payloads and subjects for snapshot and receive.
- Add narrow traits for volume fact writing and participant RPC so the command
  can be tested without p2panda or real ZFS.
- Add lease state read helper modeled after `mvp-acme-command`:
  read `/facts/lease/<encoded-resource>/`, decode `ProjectionFactPayload`
  lease facts, reduce through `LeaseBook`, and require verified/conflict
  candidates to be readable.
- Add durable lease claim write helper:
  prepare `LeaseClaimed`, compute claim hash, preflight/write the lease fact,
  and return the fencing handle.
- Command entry reads ownership and lease candidates before mutation.
- Snapshot source, receive on target, validate receive evidence, re-check
  current lease, then write ownership commit.
- Return `VolumeTransferResult` with visible nodes, old owner, new owner,
  transfer id, snapshot id/guid, bytes transferred, lease holder, lease epoch,
  lease claim hash, and a `source_cleanup_deferred` flag.

Test scenarios:

- Missing current owner fails before lease claim and participant RPC.
- Active lease held by another principal fails before participant RPC.
- Lease claim fact is written before snapshot RPC.
- Snapshot request goes only to the current owner.
- Misaddressed snapshot/receive replies return structured mismatch errors.
- Receive failure writes no ownership commit.
- Forged receive evidence with wrong transfer id, source node, target node,
  snapshot id, or snapshot guid rejects before ownership commit.
- Stale lease after receive rejects ownership commit.
- Successful transfer writes exactly one ownership commit after receive.
- Source cleanup is explicitly deferred and does not affect ownership success.

Verification:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-volume
```

### Unit 3: E2E p2panda Harness And Contract

Files:

- `MVP/e2e/Cargo.toml`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/volume_transfer_contract.rs`
- `MVP/e2e-proof-plan.md`

Work:

- Add `volume-transfer-contract`.
- Keep p2panda store/write/read glue E2E-local behind `mvp-volume` traits.
- Set up two visible nodes, an initial ownership fact on `node-a`, and p2panda
  fact storage.
- Register bus handlers for snapshot and receive.
- Execute transfer from `node-a` to `node-b`.
- Drop the command object after ownership commit, import exported p2panda
  operations into a fresh store, and recover/read the committed owner.
- Simulate a drop before ownership commit and verify no false success/no new
  ownership fact.
- Include stale-holder, conflicting active-holder, and forged receive evidence
  attempts.

Test scenarios:

- Visible nodes at decision time are included in every result.
- Conflicting active lease fails before snapshot/receive RPC counts move.
- Successful transfer commits ownership only after receive.
- Restart after ownership commit reads the target owner without rerunning
  snapshot or receive.
- Drop before ownership commit leaves no target ownership fact and does not
  report success.
- Stale lease holder cannot commit a later ownership fact.
- Forged receive evidence cannot commit ownership.
- Source cleanup remains deferred and non-authoritative.

Metrics:

- lease acquisition/write duration,
- snapshot RPC count,
- receive RPC count,
- ownership commit write duration,
- recovery read duration,
- stale mutation rejection count,
- forged receive rejection count,
- superseded ownership candidate count,
- visible nodes at decision time.

Verification:

```bash
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

### Unit 4: Documentation, Leverage Ledger, And Trigger Decision

Files:

- `MVP/slice-027-volume-ownership-transfer.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- `MVP/design-notes/phased-command.md`

Work:

- Record the volume transfer proof and exact semantics.
- Add an E2E-9 leverage ledger with:
  - feature/domain LOC,
  - adapter/backend/harness LOC,
  - shared foundation LOC,
  - tests and E2E LOC,
  - files touched for the business rule,
  - public types/enum variants added,
  - product-behavior tests added,
  - green/yellow/red assessment on whether this reused primitives or grew
    substrate.
- Compare against the old volume transfer/branch smoke reference shape.
- Record whether `PhasedCommand` should be planned next based on repeated
  phase/resume patterns now present.
- Record ZFS dependency scout outcome: no real ZFS crate yet; later backend
  slice should re-evaluate `libzetta` and `zfs-core`.

Verification:

```bash
git diff --check
```

## Review Risks

- Accidentally building a generic workflow engine inside `mvp-volume`.
- Treating advisory lease as real exclusivity instead of carrying a fencing
  token to the ownership commit.
- Writing ownership before receive evidence is validated.
- Presenting pre-commit transfer progress as success.
- Hiding participant failures behind logs or pending background state.
- Letting E2E p2panda glue grow into a reusable adapter before a second caller.
- Adding shared projection/cache or ZFS dependencies before the command ABI is
  proven.

Review should include correctness, maintainability, testing, project standards,
reliability/failure behavior, security/authorization, and simplification.

## Verification Gate

Targeted:

```bash
cargo test --manifest-path MVP/Cargo.toml -p mvp-volume
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-volume -p mvp-e2e --all-targets -- -D warnings
```

Closeout:

```bash
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

## Done Criteria

- Volume transfer is represented as a foreground command with structured
  success, conflict, stale-lease, forged-receive, and pre-commit-no-success
  outcomes.
- A durable lease claim fact is written before participant RPC.
- Ownership moves only through a p2panda-backed ownership fact written after
  exact receive evidence is validated.
- Active holder conflict and stale holder mutation fail before unsafe durable
  mutation.
- Coordinator restart after ownership commit reads the target owner without
  rerunning snapshot/receive.
- Coordinator drop before ownership commit leaves no new owner and no false
  success.
- Command results include visible nodes at decision time.
- E2E metrics capture lease, transfer, recovery, stale-rejection, forged
  evidence rejection, and semantic-leverage evidence.
- `PhasedCommand` trigger is explicitly accepted or deferred with evidence.
