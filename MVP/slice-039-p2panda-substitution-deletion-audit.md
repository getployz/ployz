---
title: Slice 039 p2panda Substitution Deletion Audit
status: completed
created: 2026-05-19
origin:
  - MVP/slice-039-p2panda-substitution-deletion-audit-plan.md
  - MVP/slice-038-p2panda-06-canonical-transport.md
  - MVP/design-notes/p2panda-substitution-audit.md
---

# Slice 039 p2panda Substitution Deletion Audit

## Decision

Use p2panda-net. Do not wait for RC iroh.

The active MVP workspace already resolves `p2panda-net 0.6.0` through non-RC
`iroh 0.98.2`, and `mvp-iroh` also resolves to the same iroh line. The earlier
blocking question is closed: p2panda-net is the maintained transport substrate
for fact operation movement.

The next implementation slice should delete the remaining opaque-body
p2panda-net path:

```text
PandaFactWireEnvelope / PFO1
  -> PandaNetNode
  -> PandaNetQuarantineLog
  -> import_fact_body
```

Replace the last E2E callers with canonical `PandaNetFactNode` or direct
canonical-operation import helpers, then delete the old path. After that, the
next substitution target should be durable p2panda-auth membership operations
so product paths stop installing manual trusted author/replica maps.

## Evidence

Checked during the audit:

- `cargo tree -p mvp-p2panda-transport -i iroh` resolves to `iroh 0.98.2` via
  `p2panda-net 0.6.0`.
- `cargo tree -p mvp-iroh -i iroh` also resolves to `iroh 0.98.2`.
- `cargo info p2panda-net@0.6.0` reports default address-book, iroh endpoint,
  discovery, gossip, sync, and optional supervisor features.
- `cargo info p2panda-auth@0.6.0` reports the group processor remains available
  for membership/revocation.
- `p2panda-blobs 0.5.2` still has no usable crate-root API; its published
  `src/lib.rs` only carries a refactor TODO.

Read-only subagent audits covered:

- transport deletion surface;
- bus/process/iroh fact-source callers;
- p2panda-auth, discovery, address-book, and supervision substitution choices.

## Deletion Ledger

| Surface | Current owner | Decision | Trigger |
| --- | --- | --- | --- |
| `PandaNetFactNode` | `MVP/p2panda-transport/src/fact_node.rs` | Keep | This is the canonical live fact transport. |
| `PandaNetNetworkId`, `PandaNetTopic`, `PandaNetNodeSeed`, `PandaNetNodeInfo`, `PandaNetNodeTicket` | `MVP/p2panda-transport/src/node.rs` | Keep | Canonical fact-node, process-role CLI parsing, tickets, and bootstrap still use these typed wrappers. |
| `PandaFactWireEnvelope` / `PFO1` | `MVP/p2panda-facts/src/lib.rs` | Delete after caller replacement | No product-runtime requirement remains. Replace direct probes with canonical-operation helpers. |
| `import_fact_body` / `import_fact_body_into_shared_store` | `MVP/p2panda-transport/src/fact_driver.rs` | Delete or make private legacy fixture | Only decodes `PFO1`. Canonical imports already use `import_p2panda_operation_into_shared_store`. |
| `PandaNetNode` / `PandaNetStream` | `MVP/p2panda-transport/src/node.rs` | Delete after E2E replacement | Opaque body node now duplicates `PandaNetFactNode` without product semantics. |
| `PandaNetQuarantineLog` / `PandaNetStore` | `MVP/p2panda-transport/src/quarantine_log.rs` | Delete | Reimplements p2panda store/log/topic mechanics for the obsolete opaque-body node. |
| `transport_wire_bodies` harness | `MVP/p2panda-transport/src/harness.rs` | Delete after E2E replacement | Exists only to drive the old opaque-body path. |
| `ProcessFactSource` | `MVP/e2e/src/process_fact_source.rs` | Demote to legacy fixture, then delete when process-serving callers move | p2panda process serving and p2panda-net process serving cover the product direction. |
| `BusFactSource` | `MVP/projection/src/bus_source.rs` | Keep as focused reducer/bus fixture for now | Deploy and cleanup canaries still depend on bus-backed facts. Do not treat it as product durable fact substrate. |
| `IrohDocsFactSource` | `MVP/iroh/src/facts.rs` | Park as historical fact proof | Keep `mvp-iroh` for router/blob experiments only unless a future slice explicitly revives iroh-docs. |

## Exact Callers To Replace

Before deleting `PFO1`, replace these callers:

- `MVP/e2e/src/p2panda_net_sync_contract.rs`
  - Uses `PandaFactWireEnvelope::encode` and `transport_wire_bodies`.
  - Replace with canonical `PandaNetFactNode` or delete if fully superseded by
    `p2panda-net-fact-node-contract`.
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
  - Uses the opaque-body harness and malformed `PFO1` bytes.
  - Replace with a canonical malformed/missing-body p2panda operation test if
    the malformed coverage is still needed.
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
  - `p2panda-net-acme-http01-contract` still replays exported facts through the
    opaque-body harness.
  - Replace with canonical fact-node replay or remove once ACME is covered by
    process-role fact-node E2E.
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
  - Live sync is canonical, but direct unauthorized-replica and author-mismatch
    probes still encode `PFO1`.
  - Replace those probes with direct canonical operation import helpers.
- `MVP/p2panda-transport/src/tests.rs`
  - Replace `owned_nodes_sync_one_opaque_body_with_explicit_bootstrap`.
  - Replace the direct author-mismatch import test with canonical operation
    import.
- `MVP/p2panda-facts/src/lib.rs`
  - Delete `operation_wire_envelope_round_trips_and_rejects_malformed_bytes`
    when no caller needs `PFO1`.

`p2panda-net-process-serving-contract` is the replacement model. It already
runs a separate serving/projection role, receives canonical fact-node traffic,
imports into a persistent p2panda store, applies a delayed remote update,
rejects an unauthorized operation, rebuilds projections, and restarts from the
local p2panda store while the coordinator socket is absent.

## Fact-Source Audit

`mvp-e2e -- all` currently runs every scenario in `SCENARIOS`; there is no
historical-scenario exclusion list.

Recommended test-list direction:

- Keep product canaries in `all`: deploy commit/drain, deploy cleanup,
  machine remove, membership/WireGuard, environment branch/promote/rollback,
  volume transfer, p2panda facts/sync/ACME, p2panda-net fact-node, and
  p2panda-net process serving.
- Move historical fact-substrate canaries out of `all` once their p2panda
  replacements are named: `iroh-docs-contract`,
  `docs-backed-acme-http01-contract`, `process-role-serving-contract`,
  `steady-state-serving-contract`, and the older opaque-body p2panda-net
  contracts.
- Do not remove legacy bus-backed deploy canaries from `all` until they have
  equivalent p2panda-backed coverage. They still prove product behavior even
  though their fact substrate is no longer the final direction.

This avoids pretending old substrates are still product direction while
preserving product behavior coverage until p2panda replacements exist.

## Membership And Auth Direction

Durable p2panda-auth membership should be the next substitution target after
opaque transport deletion.

Why:

- `PandaFactStore` has an `IslandAuthoritySnapshot` seam, but still supports
  manual trusted author and replica maps.
- Process-serving paths still pass `--p2panda-trusted-author` and manually trust
  replica peers.
- `p2panda-auth` provides the maintained group CRDT and strong-removal
  substrate that maps to island membership better than another MVP-local
  membership store.

Ployz still owns the membership envelope:

- island root/admin anchoring;
- principal-to-author-key binding;
- principal epoch;
- membership operation identity exposed to facts;
- fact-log frontier or dependency metadata;
- machine invite, tombstone, and re-invite policy.

Do not move these into p2panda-auth:

- PloyzBus subject permissions;
- queue grants;
- bridge import/export permissions;
- temporary response permissions;
- fact-key read/write grants;
- command-entry conflict checks;
- visible-node evidence;
- deploy/ACME/machine/volume/environment business rules.

## Discovery, Address Book, And Supervision

p2panda-net address book and discovery are useful transport substrate, not
command consistency substrate.

Use them for:

- bootstrap node information;
- node transport info;
- topic interest discovery after an explicit invite/bootstrap path;
- future transport-health observation.

Do not use them for:

- deciding command consistency;
- replacing visible nodes at decision time;
- inferring durable membership;
- silently rewriting cluster truth.

p2panda-net supervision should be considered under a future Kameo/process-role
slice. It restarts p2panda internal actors, but it does not replace Ployz role
health surfaces, Unix control sockets, last-good serving state, projection
rebuild status, or operator-visible failure audiences.

## Semantic-Leverage Accounting

Approximate retained substrate pressure:

```text
MVP/p2panda-transport/src/node.rs              700 LOC
MVP/p2panda-transport/src/quarantine_log.rs    341 LOC
MVP/e2e/src/process_fact_source.rs             682 LOC
MVP/bus/src/facts.rs                           677 LOC
MVP/projection/src/bus_source.rs               180 LOC
MVP/iroh/src/facts.rs                         1689 LOC
MVP/p2panda-authz/src/lib.rs                  2343 LOC
```

Best next deletion win:

1. Delete opaque p2panda-net transport (`node.rs` opaque path,
   `quarantine_log.rs`, `harness.rs`, `PFO1` codec, direct body import helpers)
   after replacing the named callers.
2. Then replace manual trust maps with durable p2panda-auth membership
   operations.
3. Then park historical iroh/process/bus fact-source scenarios from `all` as
   focused legacy fixtures or delete them when p2panda-backed product canaries
   cover the same behavior.

This order gives the largest maintenance reduction without weakening current
product proof.

## Next Implementation Slice

Plan:

```text
Slice 040: Delete opaque p2panda-net transport
```

Target:

- replace every `transport_wire_bodies` E2E with canonical `PandaNetFactNode`
  or direct canonical operation import;
- replace direct `PFO1` probes with canonical operation probes;
- delete `PandaFactWireEnvelope`, `PFO1`, `import_fact_body`,
  `PandaNetNode`, `PandaNetStream`, `PandaNetQuarantineLog`, and
  `MVP/p2panda-transport/src/quarantine_log.rs`;
- keep typed node config/ticket/topic/network wrappers used by
  `PandaNetFactNode`.

Proof gates:

```text
cd MVP && cargo check --workspace
cd MVP && cargo test -p mvp-p2panda-facts
cd MVP && cargo test -p mvp-p2panda-transport
cd MVP && cargo run -p mvp-e2e -- p2panda-net-fact-node-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-process-serving-contract
cd MVP && cargo run -p mvp-e2e -- p2panda-net-acme-http01-contract
rg "PandaFactWireEnvelope|PFO1|PandaNetQuarantineLog|transport_wire_bodies|import_fact_body" MVP/p2panda-facts MVP/p2panda-transport MVP/e2e/src
```

The grep gate should return no product success-path references. If a legacy
fixture remains, it must live under an explicitly named legacy test module with
a deletion trigger.

## Verification

This slice changed decision documents only. The required gate is:

```text
git diff --check -- MVP/slice-039-p2panda-substitution-deletion-audit.md MVP/slice-039-p2panda-substitution-deletion-audit-plan.md MVP/overall-plan.md MVP/primitive-decisions.md MVP/design-notes/p2panda-substitution-audit.md
```
