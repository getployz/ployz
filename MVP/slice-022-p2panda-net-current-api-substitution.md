# Slice 022 p2panda-net Current API Substitution

Status: completed.

Plan:
[MVP/slice-022-p2panda-net-current-api-substitution-plan.md](slice-022-p2panda-net-current-api-substitution-plan.md)

## Outcome

Slice 022 proved p2panda-net as the maintained transport carrier for MVP fact
sync, but did not replace the canonical stable `PandaFactStore` implementation
with the current git p2panda store/operation API line.

The safe shape is:

```text
p2panda-net local node
  -> carries opaque stable PandaFactOperation envelope in operation body
  -> receiver decodes envelope
  -> PandaFactStore::import_replica_operation
  -> island/trust/grant/conflict checks
  -> projection-visible candidate facts
```

The rejected shape is:

```text
p2panda-net store
  -> remote operation already stored
  -> projection treats it as canonical before Ployz grants are checked
```

That keeps p2panda-net in the useful place: discovery, iroh-backed node
connectivity, gossip, and log-sync transport. Ployz still owns the authority
boundary.

## What Changed

- Added `PandaFactWireEnvelope`, a harness-gated stable envelope for carrying
  existing canonical operations through p2panda-net bodies without exposing
  p2panda wire framing to E2E code.
- Added `p2panda-net-sync-contract`, an E2E scenario that transports six
  operations through local p2panda-net nodes and imports the received envelopes
  through `PandaFactStore::import_replica_operation`.
- Added focused comments demoting `export_operations` to deterministic
  harness/debug use and documenting `sync_panda_fact_stores` as the canonical
  same-process proof path.
- Added `import_replica_operation` so network-carried fact operations require a
  trusted same-island replica principal before normal operation validation runs.
- Updated the architecture, decision ledger, overall plan, and E2E proof plan
  to record that p2panda-net is the current transport direction while the
  stable `PandaFactStore` path remains canonical.

## Latest Metrics

From the passing all-run:

```text
p2panda-net-sync-contract:
  transported_operations: 6
  imported_operations: 3
  duplicate_operations: 1
  conflict_candidates: 2
  untrusted_rejected: true
  cross_island_rejected: true
  no_cross_island_leakage: true
  projected_nodes: 1
  trusted_replica_required: true
  network_sync_ms: 80
  projection_rebuild_ms: 9
  elapsed_ms: 124
```

The full `mvp-e2e -- all` run completed inside the 120s budget. The scale
scenario still moved 1,000,000 publish deliveries and 1,000,000 request-many
replies at 10,000 logical nodes, while the new p2panda-net proof added only a
small bounded network scenario to the default suite.

## Maintenance Impact

Code added:

- `MVP/e2e/src/p2panda_net_sync_contract.rs`: 430 LOC E2E proof.
- `MVP/p2panda-facts/src/lib.rs`: 84 LOC for stable operation envelope,
  characterization, and documentation.

Code deleted:

- None yet.

Code demoted:

- Manual `export_operations`/`import_operation` remains for deterministic
  harness/debug flows. Network replication should enter through
  `import_replica_operation`, which wraps the canonical lower-level import with
  trusted-replica gating.
- `sync_panda_fact_stores` remains the same-process product proof because it
  gives deterministic failure injection that p2panda-net `test_utils` does not.

This is a mixed semantic-leverage slice. It does not shrink the fact-store code
today. It prevents a larger future mistake: hand-rolling raw iroh transport or
letting p2panda-net's pre-authorized store become durable cluster truth. The
next deletion opportunity is a production p2panda-net integration that replaces
the in-process message carrier while preserving
`PandaFactStore::import_replica_operation` as the network authority gate.

## Known Issues

- One focused rerun observed a transient
  `p2panda-net::test_utils::TestNode` startup panic before scenario logic ran;
  an immediate isolated rerun passed. The all-run passed. A production transport
  slice must own node startup/shutdown/error reporting instead of relying on
  `test_utils`.
- The scenario uses live p2panda-net streams because that is the current API
  path that works from the E2E binary. Setup and event waits have deadlines,
  but a production integration should provide explicit bounded catch-up and
  lifecycle control.
- Current git p2panda store/operation types remain a migration target, not
  canonical production types. Moving `PandaFactStore` itself to that API line
  should wait until the authority/import seam can become narrower, not larger.

## Verification

Passed:

```bash
cargo fmt --all
cargo test -p mvp-p2panda-facts --lib
cargo run -p mvp-e2e -- p2panda-net-sync-contract
cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cargo run -p mvp-e2e -- deploy-restart-recovery-contract
cargo run -p mvp-e2e -- p2panda-sync-fact-source-contract
cargo clippy -p mvp-p2panda-facts -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```
