---
title: Slice 020 p2panda Substrate Replacement Investigation Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-019b-persistent-p2panda-fact-store-plan.md
external:
  - https://docs.rs/p2panda-core/latest/p2panda_core/
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/p2panda-stream/latest/p2panda_stream/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://docs.rs/p2panda-auth/latest/p2panda_auth/
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-blobs/latest/p2panda_blobs/
---

# Slice 020 p2panda Substrate Replacement Investigation Plan

## Problem Frame

The MVP has already adopted `p2panda-core`, `p2panda-store`, and
`p2panda-stream` for signed local fact operations. That is useful, but it is
not yet the full substitution question.

The operator direction is stronger: if p2panda owns a primitive, prefer using
it over maintaining our own AI-written substrate, even if p2panda is not yet
production-perfect. Our custom substrate is also not production-perfect, and it
has a much higher chance of becoming accidental maintenance load.

This slice is therefore a deep investigation slice, not the next product
feature slice. Its job is to answer:

```text
Which MVP primitives can p2panda own now, which should it own soon, and which
must stay Ployz-owned because they are product semantics?
```

The investigation must be compile-backed where possible. A docs-only opinion is
not enough when a crate can be probed cheaply inside `MVP/`.

## Requirements Trace

- `VISION.md`: Ployz should own product primitives, not unnecessary plumbing.
  The cluster should stay small, explicit, and command-shaped.
- `MVP/overall-plan.md`: p2panda is now the preferred durable fact substrate,
  and the daemon/data-plane proof requires fewer custom fact/sync paths.
- `MVP/primitive-decisions.md`: `p2panda-auth` and `p2panda-sync` are deferred
  only until persistent p2panda stores exist; Slice 019b created that boundary.
- `MVP/design-notes/p2panda-substitution-audit.md`: the remaining custom
  substrate pressure is concentrated in `MVP/iroh/src/facts.rs`,
  `MVP/e2e/src/process_fact_source.rs`, `MVP/bus/src/facts.rs`, the spike
  crate, and future membership/revocation code.
- User direction: bias toward p2panda substitution because bespoke MVP
  substrate is likely less production-ready than their crates.

## Current External Signals

As of 2026-05-18, the current p2panda crates checked for this plan are 0.5.2.
GitHub lists v0.5.2 as the latest release from 2026-03-09, with fixes for
SQLite `previous` hash handling and a gossip/sync session race. The project is
active and pre-1.0, so API churn is expected; adoption should sit behind
Ployz-owned seams rather than leaking p2panda types through business crates.

- `p2panda-core` provides signed append-only operations, body hashes, custom
  extensions, fork tolerance, pruning hooks, partial sync support, and an
  operation shape that separates header metadata from body bytes.
- `p2panda-store` provides Memory and SQLite stores, read/write traits, and
  transaction patterns. It explicitly does not validate log integrity by
  itself; applications must validate before or while persisting operations.
- `p2panda-stream` remains the ingestion/validation layer above the store.
- `p2panda-sync` provides standalone two-party sync protocols and managers over
  p2panda append-only logs. Its docs point most high-level users toward
  `p2panda-net`, but this crate is the narrower target for replacing manual
  export/import first.
- `p2panda-auth` provides eventually consistent group membership, `Pull`,
  `Read`, `Write`, and `Manage` access levels, strict group modification, and
  strong-removal conflict resolution.
- `p2panda-net` is broader than previously treated: it includes an iroh
  endpoint wrapper, address book, discovery, gossip, log sync, and supervisor.
  It depends on `iroh` 0.96.x while the MVP iroh crate currently uses a newer
  iroh line, so adopting it may require either aligning iroh versions or
  isolating the p2panda network stack behind an adapter.
- `p2panda-blobs` 0.5.2 still has 0% docs and no obvious public API in docs.rs.
  It should be rechecked directly from source before relying on it.
- The older `p2panda-rs` client/schema/document API is not a target for this
  rewrite. The modern modular crates deliberately lean toward raw bytes and
  bring-your-own data types, which matches Ployz typed fact payloads better
  than adopting an external schema/document model.

## Scope

In scope:

- Deep crate/API investigation for `p2panda-auth`, `p2panda-sync`,
  `p2panda-net`, `p2panda-discovery`, and `p2panda-blobs`.
- Compile-backed probes under `MVP/` where the public API fit is ambiguous.
- A deletion/substitution map for current MVP plumbing.
- A hard recommendation for the next implementation slice after this
  investigation.
- Maintainer-facing docs explaining what we will let p2panda own.
- Stress-test implications for 200, 1,000, and 10,000 logical-node proofs.

Out of scope:

- Shipping ACME changes.
- Replacing PloyzBus product semantics with p2panda broadcast semantics.
- Deleting working fixtures before a replacement proof passes.
- Root workspace or existing `crates/` integration.
- Treating p2panda docs as sufficient when compile probes are cheap.
- Building a general workflow engine or the future `PhasedCommand` primitive.

## Investigation Units

### Unit 1: Substrate Inventory And Deletion Ledger

Files:

- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/design-notes/p2panda-substitution.md`
- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`

Work:

- Refresh the current LOC and ownership inventory after Slice 019b.
- Classify each custom path as `replace now`, `replace after proof`, `fixture
  only`, or `Ployz-owned`.
- Include the exact replacement crate/API or the reason no p2panda replacement
  fits.
- Add a "do not write more code here" warning for paths that are waiting for
  deletion, especially custom fact sync/local-view wrappers.

Acceptance:

- The ledger names the next deletion target and the proof required before
  deletion.
- The ledger distinguishes product semantics from substrate plumbing.
- Future slice planners can choose the next implementation slice without
  rereading all previous p2panda notes.

### Unit 2: p2panda-sync Replacement Probe

Files:

- `MVP/p2panda-facts/Cargo.toml`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_fact_source_contract.rs`
- Optional new E2E scenario under `MVP/e2e/src/`

Work:

- Investigate whether `p2panda-sync` can replace manual
  `export_operations`/`import_operation` for two persistent stores.
- Prefer a minimal compile-backed proof over an abstract adapter. The proof
  should sync p2panda operations between two stores, then project through the
  existing `FactSource` seam.
- Measure offline catch-up, duplicate idempotency, conflict candidate
  preservation, and missing/untrusted-author behavior.

Acceptance:

- Decision: adopt `p2panda-sync` now, spike once more, or defer with a precise
  blocker.
- If adopted, the plan for deleting manual export/import is explicit.
- If deferred, the blocker is API/semantic mismatch, not "we did not look."

Test scenarios:

- Two persistent p2panda stores converge after one side writes while the other
  is offline.
- Re-running sync is idempotent and does not duplicate candidates.
- Same-key/different-content operations remain reducer-visible conflicts after
  sync.
- Untrusted-author operations sync as bytes only if the API forces that, but
  remain unverified/unauthorized at the Ployz candidate boundary.

### Unit 3: p2panda-auth Membership Probe

Files:

- Optional new crate or module under `MVP/`
- `MVP/mesh/src/*.rs`
- `MVP/machine/src/*.rs`
- `MVP/e2e/src/membership_wireguard_contract.rs`
- `MVP/e2e/src/machine_remove_contract.rs`

Work:

- Compile a minimal p2panda-auth group that maps:
  - island root/admin principal -> `Manage`;
  - node principal -> `Write` or a condition-restricted equivalent;
  - projection/serving/runtime principals -> `Read` or `Pull` where useful.
- Test add, remove, demote, concurrent remove/re-add, and strong-removal
  behavior against Ployz tombstone semantics.
- Decide whether p2panda-auth owns island membership/revocation, or only
  informs the model.

Acceptance:

- Decision: adopt p2panda-auth for island membership now, adopt later after a
  specific bridge, or reject for a named semantic mismatch.
- The decision explicitly states whether p2panda-auth replaces any of:
  `MachineInvite`, tombstone dominance, bus grants, subject permissions, or
  trusted p2panda author-key binding.
- The machine-remove and membership proof impact is clear.

Test scenarios:

- A manager adds a node and the resulting group grants enough authority to
  write node join facts.
- A non-manager cannot mutate group state.
- A removed node's concurrent writes are invalidated or surfaced in a way that
  preserves Ployz tombstone dominance.
- Reinvite requires an explicit new epoch/key path; no accidental resurrection.

### Unit 4: p2panda-net Transport And Supervision Probe

Files:

- `MVP/iroh/Cargo.toml`
- `MVP/iroh/src/facts.rs`
- `MVP/bus/src/actor.rs`
- `MVP/mesh/src/actor.rs`
- Optional new spike crate under `MVP/`

Work:

- Investigate whether `p2panda-net` can replace or shrink any of:
  - custom iroh endpoint setup;
  - address/ticket/bootstrap handling;
  - gossip notification plumbing;
  - log sync orchestration;
  - actor supervision loops.
- Check the iroh version conflict directly. If `p2panda-net` pulls `iroh`
  0.96.x and MVP uses a newer line, decide whether to align, isolate, or defer.
- Do not assume p2panda-net replaces PloyzBus. Its docs describe broadcast and
  local-first event delivery; Ployz still needs request/reply, request-many,
  no-responders, queue groups, services, subject permissions, and bridges.

Acceptance:

- Decision: adopt `p2panda-net` now for fact sync/event delivery, isolate it as
  a parallel p2panda substrate, or defer because of iroh/runtime/API mismatch.
- The decision names the exact code it would delete or simplify.
- If deferred, the next recheck condition is concrete, such as p2panda moving
  to the same iroh line or a successful isolated network proof.

Test scenarios:

- Two local nodes discover or are manually introduced through p2panda-net and
  exchange one durable fact topic.
- A gossip notification wakes a projection without being treated as truth.
- Killing a p2panda-net module surfaces supervisor status without killing the
  serving/data-plane role.

### Unit 5: p2panda-blobs And Payload Boundary Check

Files:

- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/architecture.md`
- Optional spike under `MVP/`

Work:

- Inspect the actual published `p2panda-blobs` source, not only docs.rs, because
  docs.rs exposes no useful API surface for 0.5.2.
- Decide whether p2panda-blobs can replace direct `iroh-blobs` in the MVP, or
  whether direct `iroh-blobs` remains the simpler and better-supported path.
- Confirm that no older `p2panda-rs` schema/document API is being mistaken for
  the modern p2panda substrate. Schema/document modeling should stay as typed
  Ployz facts unless the current modular crates expose a clear replacement.

Acceptance:

- Decision: adopt, defer, or reject p2panda-blobs.
- If rejected/deferred, the architecture continues to state that p2panda owns
  signed operations and local fact sync, while iroh-blobs owns large payloads.

### Unit 6: Next-Slice Recommendation

Files:

- `MVP/design-notes/p2panda-substitution-audit.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`
- New slice implementation plan if the recommendation is clear

Work:

- Choose the next implementation slice based on evidence, not the old backlog.
- The expected default is one of:
  - p2panda-sync-backed fact replication,
  - p2panda-auth-backed membership/revocation,
  - p2panda-net isolated transport proof,
  - ACME on p2panda facts if no deeper substrate replacement is ready.
- Include the expected LOC deletion/avoidance and E2E proof value.

Acceptance:

- There is one clear next slice recommendation.
- The recommendation includes exact tests and code paths.
- The recommendation explains why it beats returning immediately to ACME.

## Decision Criteria

Bias toward adoption when:

- p2panda owns a generic distributed-systems primitive better than the MVP code;
- the crate compiles cleanly in `MVP/`;
- the API can sit behind an existing Ployz seam;
- the replacement deletes or prevents custom substrate code;
- Ployz business semantics remain visible in reducers/commands/tests.

Defer when:

- adoption forces two incompatible iroh/runtime stacks into product-shaped code;
- the p2panda API is not currently exported or is undocumented enough that a
  thin adapter would become more code than the bespoke path;
- the primitive is product-specific rather than substrate-specific;
- E2E proof would become weaker or less legible.

Reject only when:

- p2panda semantics conflict with Ployz product invariants;
- the replacement would move command authority or conflict behavior into an
  opaque substrate where operators cannot see it;
- the crate cannot express the required behavior without a large wrapper that
  recreates the old complexity.

## Expected Outputs

- Updated substitution audit with current crate findings and deletion ledger.
- Compile-backed probe code where needed, kept under `MVP/`.
- Metrics or at least exact proof results for any E2E scenarios added.
- Updated primitive decision entries for adopted/deferred p2panda primitives.
- A next implementation slice plan.

## Verification

The investigation should run the checks relevant to any probe code it adds.
Baseline expected commands:

```text
cd MVP && cargo test -p mvp-p2panda-facts --lib
cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-process-role-serving-contract
cd MVP && cargo clippy -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
git diff --check
```

If the slice adds a new spike crate, include its `cargo test -p ...` command.
If it adds a new E2E scenario, add it to the exact-command list and decide
whether it belongs in `mvp-e2e -- all`.

## Risks

- `p2panda-net` may be attractive enough to pull in but currently sits on a
  different iroh line. Treat version alignment as a first-class decision, not a
  hidden dependency accident.
- `p2panda-auth` may fit membership while still not fitting bus subject grants.
  Keep those decisions separate.
- `p2panda-sync` may require a data-type manager shape that is heavier than
  manual import/export for the current MVP. If so, document the exact threshold
  that would make it worth adopting.
- A successful p2panda substitution can still produce too much adapter code.
  The simplification pass should count adapter complexity as a real cost.
- p2panda `main` may contain unreleased breaking changes. Pin crates for slice
  work, cite the version in decision docs, and keep the adapter small enough to
  rewrite if 0.6 changes names or store/sync APIs.
