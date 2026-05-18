---
title: Slice 025 p2panda-net Substitution Consolidation Plan
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
  - MVP/slice-022-p2panda-net-current-api-substitution.md
  - MVP/slice-023-owned-p2panda-net-transport.md
  - MVP/slice-024-acme-command-surface.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://github.com/p2panda/p2panda
  - https://www.iroh.computer/docs/concepts/router
---

# Slice 025 p2panda-net Substitution Consolidation Plan

## Problem Frame

Slice 023 proved that owned p2panda-net nodes can carry stable Ployz fact
envelopes over maintained network/log-sync machinery. Slice 024 then moved the
ACME command canary onto reusable command code. The next risk is maintenance
shape: the MVP currently has a stable p2panda fact-store line, a git
p2panda-net transport line, and E2E scenarios that still import some git
p2panda types directly.

That is tolerable for proving the first network path, but it is not a good
foundation. If every product canary has to understand p2panda-net topics,
signing keys, node info, test utilities, and import outcome minutiae, then we
are only moving custom transport plumbing around.

This slice should consolidate the p2panda-net substitution boundary. The goal
is not to use direct RC iroh. It is explicitly acceptable to avoid direct RC
iroh and let p2panda-net own the iroh/gossip/log-sync carrier while Ployz owns
fact authorization, command semantics, projections, serving, deploy, machine
operations, and WireGuard.

This is an enabling maintenance slice between product proofs, not a new product
feature. It should proceed only if it removes leakage or retires proof-only
surface. If implementation cannot delete direct E2E p2panda-net imports, retire
`mvp-p2panda-spike`, or make ACME/deploy product canaries simpler to read, stop
and return to the deploy command-surface slice instead.

## Requirements Trace

- User direction: bias toward using maintained p2panda crates because the MVP's
  custom substrate is more likely to be weaker than theirs.
- User direction: it is fine not to use RC iroh. Prefer a workaround through
  p2panda-net if the maintained stack covers the path.
- `MVP/overall-plan.md`: p2panda is the preferred durable fact substrate and
  future slices should reduce custom glue rather than add parallel scaffolds.
- `MVP/architecture.md`: iroh remains connectivity direction, but p2panda-net
  can be the network carrier under the Ployz fact/bus boundaries.
- `MVP/design-notes/p2panda-substitution-audit.md`: keep `FactSource`, keep
  PloyzBus, keep product reducers; replace generic substrate where p2panda
  already owns the better primitive.
- `MVP/slice-023-owned-p2panda-net-transport.md`: owned p2panda-net nodes are
  proven enough to stop deferring network substitution.
- `MVP/slice-024-acme-command-surface.md`: ACME now has reusable command
  semantics; the next network work should simplify substrate exposure around
  that command path.

## Dependency Scout

Checked before planning:

- `p2panda-net` latest docs describe it as a local-first networking crate with
  iroh endpoint integration, address book, confidential discovery, gossip, log
  sync, and supervisor modules. It is designed as a broadcast/sync abstraction
  over transports rather than a PloyzBus replacement.
- `p2panda-sync` latest docs position the crate as lower-level sync protocols
  for applications that bring their own convergent data type. That matches
  Ployz's current fact operation model.
- The p2panda repository describes `p2panda-net`, `p2panda-store`,
  `p2panda-sync`, `p2panda-core`, `p2panda-auth`, and `p2panda-blobs` as
  separable crates. That supports the current adapter strategy: adopt the
  carrier/store pieces without outsourcing Ployz business semantics.
- iroh router docs remain useful architecture grounding for future direct
  protocol composition, but this slice should not force direct iroh if
  p2panda-net already owns the needed iroh integration.

Decision:

- Use p2panda-net as the network substitution path for this slice.
- Do not introduce direct RC iroh.
- Do not migrate the stable production-shaped `mvp-p2panda-facts` store to git
  p2panda APIs in this slice unless consolidation proves it is required.
- Quarantine git p2panda-net/core/store/sync types inside
  `mvp-p2panda-transport`. Product E2Es should use Ployz-owned wrapper types.

## Scope

In scope:

- Consolidate p2panda-net git API exposure behind `mvp-p2panda-transport`.
- Remove direct p2panda-net/core/store/sync imports from E2E canary files where
  wrapper types can express the same thing.
- Add a higher-level transport session or topic API that moves stable Ployz
  fact envelopes between `PandaFactStore` replicas without each E2E hand-rolling
  spawn/open/replay/import loops.
- Keep import validation through the canonical trusted-replica path.
- Preserve existing `p2panda-net-sync-contract`,
  `p2panda-net-owned-node-contract`, and
  `p2panda-net-acme-http01-contract` behavior.
- Delete or retire `MVP/p2panda-spike` if current production-shaped tests cover
  all of its remaining proof value.
- Record a concrete substitution ledger: LOC/API surface removed from E2E,
  direct dependency leakage removed, and remaining custom substrate that still
  needs a future slice.

Deletion gates:

- `MVP/e2e/Cargo.toml` should no longer need git `p2panda-net`,
  `p2panda-store`, or `p2panda-sync` dependencies unless the closeout names a
  specific surviving exception.
- `MVP/e2e/src/p2panda_acme_http01_contract.rs` should lose its local
  p2panda-net node harness and use a Ployz-owned transport helper.
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs` should keep scenario
  assertions but shed repeated node setup/import loops where the helper applies.
- `MVP/p2panda-spike` should be deleted if its remaining proof value is covered
  by production-shaped p2panda fact and transport contracts.

Out of scope:

- Replacing PloyzBus with p2panda gossip or log sync.
- Replacing authority-island subject grants with p2panda-auth.
- Real ACME issuance.
- Direct iroh router/ALPN implementation.
- Migrating existing non-MVP crates.
- Replacing Pingora/hickory production serving.

## Design Decisions

### One Git p2panda Boundary

The MVP can tolerate git p2panda-net in one adapter crate. It should not leak
into every proof scenario. `mvp-p2panda-transport` should own:

- node identity seed/signing key construction,
- topic representation,
- bind config and free-port helpers where possible,
- node info/bootstrap details,
- stream opening and replay loops,
- transport import outcome classification.

E2Es should express product intent:

```text
transport all exported facts from left to right as trusted replica
expect imported/duplicate/conflict/deferred/rejected/failed counts
project and serve the result
```

They should not have to script p2panda-net internals for each product canary.

### Stable Ployz Envelopes Stay

Slice 023 deliberately transported `PandaFactWireEnvelope` bytes over
p2panda-net. Keep that. It lets stable `mvp-p2panda-facts` remain the canonical
fact writer/importer while p2panda-net is treated as carrier.

This avoids a full production fact-store migration to the current git p2panda
API before there is a proven deletion payoff.

### Outcome Semantics Stay Loud

Do not collapse outcomes into a boolean "synced." The adapter must preserve:

- imported,
- duplicate,
- conflict,
- deferred out-of-order,
- rejected malformed/unauthorized/untrusted/cross-island,
- failed local ingest/store/missing-payload.

Those distinctions are the failure audience for operators and future agents.

### No New Generic Framework

Do not build a transport framework for hypothetical future protocols. Add the
smallest wrapper that makes the existing p2panda-net fact transport paths easy
to use and hard to misuse. If the wrapper cannot delete E2E boilerplate, it is
not ready.

### Deploy Stays Next If This Does Not Delete Enough

The next product proof after this consolidation should be the deploy
command-surface equivalent of Slice 024 unless this slice uncovers a blocking
transport issue. Deploy is still the primary semantic-leverage measurement
against the old `deploy.rs` shape. This slice earns priority first only because
the current E2E dependency leakage would otherwise be copied into that deploy
work.

## Implementation Units

### Unit 1: Transport Surface Audit

Files:

- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/fact_driver.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`

Work:

- Inventory every direct git p2panda type used outside
  `mvp-p2panda-transport`.
- Record before-counts for direct git p2panda imports and E2E local transport
  helper LOC so closeout can show whether this slice reduced maintenance
  burden.
- Decide which wrapper types belong in transport and which are legitimate test
  fixtures.
- Write a short implementation note, in the commit message or closeout report,
  for the exact first deletion target. The expected target is the ACME-local
  `replay_all_exported_facts_via_net` / `AcmeNetHarness` path.
- Do not land a new wrapper in this unit unless it replaces the ACME-local
  helper in the same commit. A helper that cannot delete E2E boilerplate is not
  ready.
- Sketch the small transport-facing API needed for Unit 2:
  - deterministic local node config,
  - topic creation,
  - opening sender/receiver streams,
  - replaying exported fact operations into a target store through a replica
    session.
- Keep this unit read/plan-shaped or deletion-coupled; avoid additive
  abstraction before the E2E contracts are pinned.

Test scenarios:

- Existing p2panda-net E2Es still pass before and after the wrapper is
  introduced.
- Wrapper API can express trusted replica gating without depending on network
  delivery order.

Verification:

- `cargo check --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e`

### Unit 2: Product-Canary Transport Helper And ACME Deletion

Files:

- `MVP/p2panda-transport/src/fact_driver.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/tests.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`

Work:

- Add a helper that replays exported stable `PandaFactWireEnvelope` operations
  from a source store over owned p2panda-net nodes and imports into a target
  store with a trusted replica session.
- Return a structured report with replayed/imported/duplicate/conflict/deferred
  /rejected/failed counts.
- Replace the ACME E2E-local net replay helper with the transport helper.
- Delete the ACME-local `AcmeNetHarness` and replay/import loop in the same
  implementation commit that introduces the replacement helper.
- Preserve ACME's current behavior: conflict is unexpected in this canary and
  should still fail loudly.

Test scenarios:

- ACME net canary imports before-clear and after-clear operations with the same
  counts as Slice 024.
- Unauthorized replica import is still proven with an explicit known envelope.
- Command adapter outage still serves last-good HTTP-01 state.
- Clear still projects to 404 after transported facts arrive.

Verification:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-acme-http01-contract`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport`

### Unit 3: General Network Contract Cleanup

Files:

- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/node.rs`

Work:

- Replace remaining direct p2panda-net setup boilerplate in the generic network
  contracts where the new wrapper cleanly applies.
- Keep scenario-specific assertions in E2E. Do not hide product proof behind a
  black-box helper.
- Remove direct git p2panda dependencies from `MVP/e2e/Cargo.toml` if no E2E
  file still needs them.

Test scenarios:

- `p2panda-net-sync-contract` still proves imported, duplicate, conflict,
  untrusted, cross-island, trusted-replica, and projection behavior.
- `p2panda-net-owned-node-contract` still proves owned-node transport without
  p2panda test utilities.
- E2E no longer imports p2panda-net/core/store/sync git crates directly unless
  a documented exception remains.

Verification:

- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-owned-node-contract`

### Unit 4: Delete Obsolete Spike Surface If Covered

Files:

- `MVP/Cargo.toml`
- `MVP/p2panda-spike/Cargo.toml`
- `MVP/p2panda-spike/src/lib.rs`
- `MVP/design-notes/p2panda-substitution.md`
- `MVP/e2e-proof-plan.md`

Work:

- Compare `mvp-p2panda-spike` tests against current
  `mvp-p2panda-facts`, `mvp-p2panda-transport`, and E2E coverage.
- Build an explicit proof map for the five spike behaviors recorded in
  `MVP/design-notes/p2panda-substitution.md`:
  - signed operation decodes into a Ployz `FactCandidate`;
  - conflicting operations for one fact key both reach projection;
  - payloads can be read by candidate content hash;
  - p2panda body hashes map to Ployz `b3:<hex>` content hashes;
  - operation storage can group by island while preserving author identity.
- If every spike behavior has production-shaped coverage, remove
  `mvp-p2panda-spike` from the MVP workspace and delete the crate.
- If any behavior remains unique, keep the crate and document exactly what it
  still proves.

Test scenarios:

- Workspace still builds after deletion or documented retention.
- The substitution design note no longer points maintainers at a stale proof.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-spike`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts`
- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport`
- Relevant E2E scenarios named in the proof map.
- `cargo check --manifest-path MVP/Cargo.toml --workspace`

### Unit 5: Closeout, Review, And Stress Gate

Files:

- `MVP/slice-025-p2panda-net-substitution-consolidation.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/primitive-decisions.md`

Work:

- Record direct dependency/API leakage removed.
- Record LOC removed from E2E and any spike deletion.
- Record whether the deletion gates were met. If any gate remains open, name
  the blocker and do not present the slice as a maintenance win.
- Document why p2panda-net remains the carrier and direct RC iroh is not needed
  for this MVP path.
- Run simplify and code review with subagents because this slice touches shared
  transport test boundaries.

Verification:

- `cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-sync-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-owned-node-contract`
- `cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-acme-http01-contract`
- `cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e --all-targets -- -D warnings`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all`

## Risks

- The wrapper could hide too much E2E proof. Keep scenario assertions in E2E and
  only abstract repeated transport plumbing.
- The git p2panda API may shift. That is exactly why this slice should
  quarantine it behind `mvp-p2panda-transport`.
- Removing `mvp-p2panda-spike` too early could erase useful regression
  coverage. Only delete it if production-shaped tests cover its behaviors.
- A broad migration to git p2panda store APIs could balloon the slice. Avoid
  that unless direct dependency cleanup proves impossible without it.

## Non-Goals

- No direct iroh RC adoption.
- No p2panda-auth membership adoption.
- No p2panda-blobs adoption.
- No new command/product behavior.
- No non-MVP crate edits.
