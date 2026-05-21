---
title: Slice 018a p2panda Substrate Substitution Investigation Plan
status: completed
created: 2026-05-18
completed: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/primitive-decisions.md
  - MVP/slice-018-deploy-restart-recovery-plan.md
---

# Slice 018a p2panda Substrate Substitution Investigation Plan

## Problem Frame

The MVP has deliberately built small substrate proofs for the bus, facts,
projection, advisory leases, authority islands, machine removal, serving
snapshots, and docs-backed ACME. That was useful to prove the semantics. The
next risk is carrying too much custom substrate code forward after the semantics
are understood.

Before hardening the deploy restart recovery slice, run a deep substitution
investigation into `p2panda`. The purpose is not to admire another project or
rewrite the MVP around it. The purpose is to answer one question with evidence:

```text
Can p2panda crates delete or prevent enough Ployz substrate code to justify
adopting them early, while preserving Ployz's bus, command, deploy, serving,
DNS, WireGuard, and operator semantics?
```

This slice should produce a decision that is strong enough to affect the next
implementation slice. If adoption is worthwhile, update the deploy restart
recovery plan before implementing it. If adoption is not worthwhile, record the
reason and continue with the current substrate without reopening this question
every slice.

## Requirements Trace

- The operator does not want to maintain unnecessary custom substrate code.
- `MVP/overall-plan.md` says every implementation slice should scout crates
  that remove plumbing.
- `VISION.md` prefers a small orchestration kernel with explicit seams and
  business logic below process wiring.
- `MVP/architecture.md` keeps Ployz-specific semantics in subject bus,
  command/orchestration, serving projections, DNS projections, WireGuard
  reconciliation, and deploy invariants.
- `MVP/primitive-decisions.md` records `mvp-iroh`, `FactSource`, grants,
  leases, and projection reducers as current MVP primitives. This investigation
  must challenge those primitives where a maintained crate can replace them.
- `MVP/slice-018-deploy-restart-recovery-plan.md` should not be implemented on
  top of a fact substrate we already know we want to replace.

## Current External Signals

Use current upstream documentation as planning input, then verify with compile
spikes before adopting anything:

- `p2panda-core` exposes signed operation data types. Its `Header` includes
  public key, signature, sequence number, backlinks/previous links, payload
  hash, payload size, timestamp, and extensions. That overlaps with Ployz's
  signed immutable fact envelope and author/content-hash metadata.
  <https://docs.rs/p2panda-core/latest/p2panda_core/operation/struct.Header.html>
- `p2panda-sync` describes data-type agnostic sync interfaces plus concrete
  append-only log sync protocols. That overlaps with Ployz's eventual fact
  propagation concerns, but may be lower-level or differently shaped than
  iroh-docs.
  <https://docs.rs/p2panda-sync/latest/p2panda_sync/>
- `p2panda-auth` provides decentralized group management with Pull/Read/Write/
  Manage access levels, conditional access, eventual group convergence, strict
  group modification, and configurable concurrency resolution including strong
  removal. That overlaps with authority-island membership and some grant
  lifecycle concerns, but not with Ployz subject permissions by itself.
  <https://docs.rs/p2panda-auth/latest/p2panda_auth/>
- `p2panda-net` exposes endpoint, discovery, gossip, and log-sync abstractions.
  Its model is a local-first broadcast/sync stack, not NATS-shaped
  request/reply/queue/service semantics. Treat it as a candidate transport/sync
  substrate, not a PloyzBus replacement.
  <https://docs.rs/p2panda-net/latest/p2panda_net/>
- `p2panda-stream` provides stream combinators for decoding, validating,
  ordering, pruning, and storing p2panda operations. That may simplify ingestion
  around `FactCandidate` construction.
  <https://docs.rs/p2panda-stream/latest/p2panda_stream/>
- `p2panda-blobs` wraps `iroh-blobs` and provides memory/filesystem stores plus
  BLAKE3-addressed import/download/export flows. That overlaps with future
  payload/blob plumbing but may be redundant while the MVP already uses the
  iroh family directly.
  <https://docs.rs/p2panda-blobs/latest/p2panda_blobs/>
- The p2panda FAQ says the core features are implemented and stable enough for
  applications, but does not recommend untrusted/open-network deployments yet
  because the project has not had a security audit and still has security/data
  privacy gaps. That matters for production trust boundaries.
  <https://aquadoggo.p2panda.org/faq/>

## Scope

In scope:

- Evaluate whether p2panda can replace, wrap, or simplify the custom MVP code
  behind:
  - `MVP/iroh/src/facts.rs`
  - `MVP/projection/src/source.rs`
  - `MVP/projection/src/bus_source.rs`
  - `MVP/projection/src/reducer.rs`
  - `MVP/e2e/src/process_fact_source.rs`
  - `MVP/bus/src/facts.rs`
  - `MVP/bus/src/grants.rs`
  - `MVP/lease/src/lib.rs`
- Evaluate `p2panda-core`, `p2panda-store`, `p2panda-stream`, `p2panda-auth`,
  `p2panda-sync`, `p2panda-net`, `p2panda-discovery`, and `p2panda-blobs`.
- Create isolated spike code under `MVP/` if docs are insufficient. Spike code
  should live behind obvious investigation names and must not become production
  substrate by accident.
- Produce a deletion/avoidance estimate: which current modules or future
  modules become smaller if p2panda is adopted.
- Produce a risk estimate: API stability, security/audit posture, dependency
  size, compatibility with current Rust/MSRV, iroh version alignment, and
  whether Ployz semantics become harder to read.
- Decide before Slice 018 implementation whether to adopt, defer, or reject each
  candidate p2panda crate.

Out of scope:

- Replacing PloyzBus subject, wildcard, request/reply, request-many, queue group,
  service registry, bridge, drain, or no-responder semantics.
- Replacing deploy, machine, ACME, gateway, DNS, or WireGuard business reducers.
- Replacing foreground command semantics with local-first document editing
  semantics.
- Adding Temporal/Cadence/Restate-style activity replay.
- Touching any existing root workspace code outside `MVP/`.
- Hardening placeholder HTTP/DNS serving internals; those are separate
  production migration concerns.

## Substitution Questions

Answer these in order.

### 1. Operation Envelope

Can `p2panda-core::Operation` replace Ployz's custom fact envelope?

Required proof:

- Encode a Ployz fact as a p2panda operation without losing:
  - `IslandId`
  - `FactKey`
  - `FactKind`
  - author/principal identity
  - content hash
  - epoch or equivalent reducer ordering input
  - immutable payload reference
- Decode that operation into the existing `FactCandidate` shape without
  projecting invalid or unauthorized facts.
- Preserve deterministic reducer behavior for conflict candidates:
  `(epoch desc, content_hash asc)`.
- Model malformed or missing payloads as structured candidate status, not a
  silent drop.

Adoption threshold:

- Adopt only if the operation envelope deletes custom signature/hash/metadata
  code or prevents a larger upcoming version of it.
- Reject if adopting requires contorting Ployz fact keys or reducers around
  p2panda document semantics.

### 2. Store And Stream Ingestion

Can `p2panda-store` plus `p2panda-stream` replace custom local fact ingestion
and local-view plumbing?

Required proof:

- Ingest operations from at least two authors out of order.
- Read all candidates for a fact prefix.
- Read payloads by content hash.
- Surface conflict candidates rather than rejecting them at write time.
- Preserve projection rebuild behavior after deleting the SQLite projection.
- Compare the shape against `MVP/e2e/src/process_fact_source.rs` and
  `MVP/iroh/src/facts.rs`.

Adoption threshold:

- Adopt only if the adapter is smaller and clearer than the current
  `FactSource` implementations.
- Reject if the adapter just wraps p2panda in the same amount of Ployz-specific
  indexing code.

### 3. Authority Islands And Grants

Can `p2panda-auth` replace authority-island membership or grant lifecycle code?

Required proof:

- Model island membership with root/admin/node principals.
- Model removal and re-addition behavior.
- Verify strong-removal behavior matches the current "ignore future facts signed
  by removed node unless re-invited" requirement.
- Decide whether `AccessLevel` plus conditions can express Ployz fact-write
  grants cleanly.
- Prove that subject publish/subscribe/request permissions remain Ployz-owned.

Adoption threshold:

- Adopt for membership/group-state only if it removes custom revocation and
  membership convergence code without weakening subject/RPC permission checks.
- Reject for the bus if it cannot naturally express subject wildcard,
  queue-group, response-inbox, and bridge permissions.

### 4. Sync, Discovery, And Networking

Can `p2panda-sync`, `p2panda-net`, or `p2panda-discovery` replace the current
iroh-docs sync direction?

Required proof:

- Run a two-node sync spike, not only a type-level compile check.
- Confirm whether p2panda networking can coexist with the existing iroh endpoint
  plan and ALPN layout.
- Compare failure surfaces against `iroh-docs`: durable local write, eventual
  replication, missing content, remote unavailable, and restart.
- Confirm whether using p2panda-net would fight the NATS-shaped PloyzBus
  semantics.

Adoption threshold:

- Adopt only if it materially reduces the docs/sync/transport code while
  keeping PloyzBus as the public primitive.
- Defer if it requires replacing `mvp-iroh` wholesale before the MVP's product
  proofs are complete.

### 5. Blob Payloads

Can `p2panda-blobs` replace direct `iroh-blobs` usage?

Required proof:

- Import, address, fetch, and export payloads through memory and filesystem
  stores.
- Confirm hash compatibility with the rest of the fact payload model.
- Check whether the wrapper simplifies future large manifest/cert/env package
  payloads.

Adoption threshold:

- Adopt only if it removes meaningful blob service code or gives a clearer
  high-level API than direct iroh-blobs.
- Defer if it only adds one more wrapper around the same dependency.

## Implementation Units For The Investigation

### Unit 1: Substrate Inventory

Files to inspect:

- `MVP/iroh/src/facts.rs`
- `MVP/projection/src/source.rs`
- `MVP/projection/src/bus_source.rs`
- `MVP/projection/src/reducer.rs`
- `MVP/e2e/src/process_fact_source.rs`
- `MVP/bus/src/facts.rs`
- `MVP/bus/src/grants.rs`
- `MVP/lease/src/lib.rs`

Deliverable:

- A short inventory in `MVP/design-notes/p2panda-substitution.md` listing custom
  code that is substrate rather than business logic, with rough deletion or
  avoidance potential.

### Unit 2: Current p2panda API And Risk Scout

Files to create/update:

- `MVP/design-notes/p2panda-substitution.md`

Required checks:

- Current crate versions and release cadence.
- License compatibility.
- Rust/MSRV and edition requirements.
- iroh version compatibility.
- Security/audit posture.
- Whether the APIs are documented enough for maintainers to debug.

Deliverable:

- A source-linked crate matrix with recommendation candidates:
  `adopt-now`, `spike-only`, `defer`, or `reject`.

### Unit 3: Fact Operation Spike

Candidate files if code is needed:

- `MVP/p2panda-spike/Cargo.toml`
- `MVP/p2panda-spike/src/lib.rs`

Required tests:

- A signed operation decodes into a `FactCandidate`.
- Two conflicting operations for the same Ployz fact key both reach the reducer.
- Winner selection remains deterministic by epoch and content hash.
- Unauthorized author or malformed payload becomes structured status.

Deliverable:

- Either a compile-tested spike or a written reason why the API shape makes the
  spike unnecessary or impossible.

### Unit 4: Auth Group Spike

Candidate files if code is needed:

- `MVP/p2panda-spike/src/auth.rs`

Required tests:

- Root/admin can add a node.
- Removed node cannot continue as an accepted writer after removal converges.
- Re-invited node can be represented without resurrecting stale writes.
- Subject/RPC permissions remain outside p2panda-auth.

Deliverable:

- A clear recommendation on whether p2panda-auth should replace any current
  authority-island machinery.

### Unit 5: Sync/Blob Spike

Candidate files if code is needed:

- `MVP/p2panda-spike/src/sync.rs`
- `MVP/p2panda-spike/src/blobs.rs`

Required tests:

- Two local nodes exchange operation data.
- Blob import/export works against a filesystem-backed store.
- Missing remote/unavailable peer is surfaced as a structured error, not hidden
  behind a retry loop.

Deliverable:

- A recommendation on whether p2panda should replace `mvp-iroh` fact/blob
  internals, wrap them, or remain a design reference only.

### Unit 6: Decision And Plan Rewrite

Files to update:

- `MVP/design-notes/p2panda-substitution.md`
- `MVP/primitive-decisions.md`
- `MVP/slice-018-deploy-restart-recovery-plan.md`

Required outcome:

- One of:
  - Adopt one or more p2panda crates before Slice 018 implementation.
  - Defer adoption until after the MVP proofs, with exact trigger conditions.
  - Reject adoption for this MVP and list the ideas copied instead.
- If adoption changes the fact substrate, rewrite the Slice 018 deploy recovery
  plan around the new substrate before implementation starts.

## Test And Verification Plan

Minimum investigation gates:

- Any spike crate must pass `cargo test -p <spike-crate>`.
- The existing MVP gate must still pass after docs-only or spike-only changes:
  `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`.
- If an adapter replaces a production MVP fact source, rerun:
  - `cargo run -p mvp-e2e -- iroh-docs-contract`
  - `cargo run -p mvp-e2e -- projection-contract`
  - `cargo run -p mvp-e2e -- docs-backed-acme-http01-contract`
  - `cargo run -p mvp-e2e -- machine-remove-contract`
- If auth adoption touches grants or island membership, rerun:
  - `cargo run -p mvp-e2e -- authority-contract`
  - `cargo run -p mvp-e2e -- bridge-contract`
  - `cargo run -p mvp-e2e -- membership-wireguard-contract`

The investigation is not done until the recommendation is tied to tests or a
specific API limitation. "The docs look nice" is not enough.

## Decision Rules

- Prefer deleting custom substrate over wrapping custom substrate in more
  substrate.
- Keep PloyzBus custom unless a crate proves the NATS-core-shaped semantics
  directly.
- Keep Ployz reducers custom. The business rule is the reducer; the substrate
  can only feed it better candidates.
- Keep operator command semantics foreground and explicit. No activity replay.
- Do not adopt a crate that makes steady-state failure audiences less clear.
- Do not adopt a crate unless the replacement boundary is narrow enough to
  reverse without rewriting deploy, machine, ACME, serving, DNS, or WireGuard.
- Treat unaudited security-sensitive primitives as acceptable for MVP proof only
  behind a wrapper seam, not as a final production trust decision.

## Expected Output Shape

At the end of this slice, maintainers should be able to read
`MVP/design-notes/p2panda-substitution.md` and answer:

- What parts of the current MVP are custom substrate?
- Which p2panda crates could replace those parts?
- What did the spike prove or fail to prove?
- How many lines or future modules would adoption plausibly delete or avoid?
- What does adoption do to failure semantics and operator-visible conflicts?
- Does Slice 018 deploy restart recovery proceed on the current fact substrate
  or a p2panda-backed one?

That is the bar for "deep investigation." Anything less is just a dependency
scout.

## Completion

Completed in
`MVP/slice-018a-p2panda-substitution-investigation.md`, with the durable
decision note in `MVP/design-notes/p2panda-substitution.md` and the compile
spike in `MVP/p2panda-spike`.
