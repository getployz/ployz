---
title: Slice 031 p2panda-net Process Serving Plan
status: implemented
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-012-process-role-serving.md
  - MVP/slice-013-wire-http-dns-serving.md
  - MVP/slice-019b-persistent-p2panda-fact-store-plan.md
  - MVP/slice-030-p2panda-net-fact-node.md
  - MVP/e2e/src/process_role_harness.rs
  - MVP/e2e/src/p2panda_process_role_serving_contract.rs
  - MVP/e2e/src/p2panda_net_fact_node_contract.rs
  - MVP/p2panda-transport/src/fact_node.rs
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html
  - https://docs.rs/interprocess/latest/interprocess/
  - https://docs.rs/hickory-server/latest/hickory_server/
---

# Slice 031 p2panda-net Process Serving Plan

## Problem Frame

Slice 030 proved a running `PandaNetFactNode` can ingest p2panda-net traffic
into a local `SharedPandaFactStore`, and projection can rebuild from that
receiver store. That is the right substrate shape, but it is still an
in-process proof.

The important daemon-failure invariant is stronger:

```text
coordinator dies
  -> fact sync role keeps receiving already-authorized serving facts
  -> projection role writes gateway/DNS snapshots
  -> serving role keeps last-good state and can reload the new state
  -> local mutation attempts remain unavailable
```

Today `p2panda-process-role-serving-contract` proves a process role can project
from persistent p2panda SQLite, but the update is written directly into the
same local SQLite file. `p2panda-net-fact-node-contract` proves network ingest,
but not in a separate process role and not with the coordinator killed. Slice
031 should join those proofs without changing the existing root codebase.

## Single Proof Target

Add `p2panda-net-process-serving-contract`:

1. start a serving/projection OS process with a p2panda-net fact node and a
   persistent local p2panda store,
2. start a separate coordinator/publisher process only long enough to publish
   the baseline serving commit over p2panda-net,
3. prove the serving/projection process imports, projects, reloads, and answers
   gateway/DNS state from its own local store,
4. kill the local coordinator/publisher role,
5. publish a later serving commit from a remote publisher process over
   p2panda-net while the local coordinator remains dead,
6. prove the still-running serving/projection process imports that network
   operation, rebuilds projection/snapshots, preserves last good during the
   update, and reloads to the new gateway/DNS state,
7. delete `projections.sqlite` while serving is live and rebuild from the
   synced local p2panda store,
8. restart the serving/projection process while no coordinator is running and
   load last-good snapshots before answering,
9. reject or surface malformed, wrong-island, untrusted-author, and
   unauthorized-replica p2panda-net operations as structured import status,
10. keep local mutation attempts through the dead coordinator path visibly
    unavailable.

The product proof is that serving state can continue to update from networked
facts after the command/coordinator role is gone. It is not a proof of kernel
WireGuard, production Pingora, or production DNS server integration.

Implementation note: the shipped contract proves process-separated p2panda-net
import into serving projections, local mutation unavailability, delayed remote
serving update after baseline, malformed message rejection, deleted SQLite
rebuild, and restart from last-good snapshots/local p2panda store. The receiver
refreshes its p2panda stream after idle timeouts so later appends from a stable
remote peer are picked up without the local coordinator.

## Requirements Trace

- `VISION.md`: the daemon is disposable; data plane and serving state must
  outlive it.
- `MVP/overall-plan.md`: already-replicated serving-state updates should keep
  applying when the coordinator is down.
- `MVP/architecture.md`: the coordinator proposes changes; separate
  steady-state roles apply already-committed facts to snapshots and serving
  state.
- `MVP/e2e-proof-plan.md`: remaining E2E-7 work explicitly names process-role
  p2panda-net serving replication.
- `MVP/primitive-decisions.md`: p2panda-net owns transport, `SharedPandaFactStore`
  owns Ployz authorization/import/projection, and placeholder wire roles should
  be judged by the interfaces that survive migration.

## Dependency Scout

Checked before planning on 2026-05-18:

- `p2panda-net` remains the right network carrier. Its docs describe a set of
  local-first peer-to-peer modules, and Slice 030 already wraps the current git
  API behind `mvp-p2panda-transport`. This slice should reuse that wrapper
  rather than hand-writing iroh streams or another sync loop.
- `tokio-util::sync::CancellationToken` would be a reasonable helper for
  shutting down multiple background tasks in one process. The first
  implementation should start with the existing process-role control socket,
  `JoinHandle`, and oneshot/watch patterns. Add `tokio-util` only if the import
  loop plus projection/reload loop otherwise grows custom cancellation
  machinery.
- `interprocess` provides cross-platform local sockets, but the MVP process
  harness already uses Unix sockets and is explicitly Unix-only for these
  process-role proofs. Do not add it until production role IPC needs a portable
  abstraction.
- `hickory-server` is the production-grade DNS server candidate. This slice is
  not the DNS migration slice; keep using the current `mvp-serving` placeholder
  interfaces and focus review on snapshot loading, last-good state, and reload
  semantics.

Decision:

- Reuse `mvp-p2panda-transport::PandaNetFactNode`.
- Add only narrow transport/config helpers that make process-role wiring
  possible, such as serializable node-info tickets or hex parsing for network
  ids/topics/seeds.
- Do not add a new production dependency unless implementation shows existing
  task shutdown code becomes unclear.

## Scope

In scope:

- Process-role harness support for a p2panda-net-backed serving/projection
  role.
- A publisher role that writes serving facts into its local
  `SharedPandaFactStore` and publishes the resulting p2panda operation over
  p2panda-net.
- A receiver role that owns:
  - a persistent p2panda SQLite store,
  - a `PandaNetFactNode`,
  - an import loop,
  - a projection actor,
  - a serving actor with last-good snapshot state,
  - structured status for imports, projection, reloads, and mutation
    unavailability.
- E2E proof that the receiver process updates after the coordinator/publisher
  is killed and a remote publisher sends a later serving commit.
- Metrics for startup, first import, import-to-projection, projection-to-reload,
  deleted-SQLite rebuild, and restart-from-snapshot.
- Decision/proof docs recording what this adds to daemon-down semantics.

Out of scope:

- Root workspace changes outside `MVP/`.
- Production daemon wiring.
- Pingora migration.
- `hickory-server` migration.
- Kernel WireGuard mutation.
- Generic process supervisor.
- Generic `mvp-commands` / `PhasedCommand`.
- p2panda-auth adoption.
- Consensus, quorum, witness acknowledgements, or active-partition checks.

## Design Decisions

### The Serving/Projection Process Owns The Local Fact Receiver

The receiver process should not wait for the coordinator to tell it to import.
It starts the p2panda-net fact node, consumes stream events, imports into its
local store, projects accepted facts, and reloads serving state. The
coordinator is absent from that path.

### Import Status Is A Serving-Role Health Surface

The import loop is background-with-consumer work. It must preserve last good
serving state and make failures visible to the next status reader. Status
should include enough structured detail for tests and maintainers:

- attempted/imported/duplicate/conflict/deferred/rejected/failed counts,
- last successful import timestamp or monotonic counter,
- last rejection/failure class,
- last projection report,
- last reload status,
- whether mutation is unavailable in this role.

Logs are not the audience.

### Auto-Apply Accepted Serving Facts

Manual `ProjectOnce` plus `Reload` requests are still useful for targeted
tests, but this proof needs an automatic applier path: after an imported fact
changes the local store, the receiver should project and reload without a local
coordinator command. If projection or reload fails, it preserves last good state
and records the failure.

### Keep Config Typed At The Transport Boundary

Process arguments should not expose p2panda git internals directly. If the
process role needs bootstrap information, add a Ployz-owned serializable wrapper
under `mvp-p2panda-transport`, for example `PandaNetNodeTicket`, rather than
passing debug strings or raw git `NodeInfo` structures through E2E code.

### Wire Roles Remain Placeholder Interfaces

The current Hyper and `hickory-proto` wire roles are placeholders. This slice
should not polish their connection framing. It should prove the durable
interfaces that survive: snapshot writes, validated reload, last-good shared
state, and explicit health.

## Implementation Units

### Unit 1: Transport Process Config Helpers

Files:

- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/p2panda-transport/src/errors.rs`
- `MVP/p2panda-transport/src/tests.rs`

Requirements:

- Add parse/format helpers for `PandaNetNetworkId`, `PandaNetNodeSeed`, and
  `PandaNetTopic` if needed by process flags.
- Add a stable serializable `PandaNetNodeTicket` or equivalent wrapper so a
  process can report bootstrap info and another process can consume it without
  importing p2panda git types.
- Expose enough non-mutating fact-node status for a process role to report
  node address and import counters.
- Do not broaden domain crates to depend on p2panda-net types.

Tests:

- ticket round-trips through JSON,
- invalid hex/length is rejected with structured transport/config error,
- a node spawned from a ticket can be used as bootstrap for a second node,
- existing fact-node unit tests still pass.

### Unit 2: Process Role Fact Receiver

Files:

- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/process_fact_source.rs` only if the existing process-file source
  helpers need no-op sharing,
- `MVP/e2e/src/main.rs`

Requirements:

- Add a new process role or fact-source mode for p2panda-net serving
  projection. Prefer extending the serving/projection role only if the state
  stays readable; split to a named role if the match arms become ambiguous.
- The role opens a persistent `SharedPandaFactStore`, starts a
  `PandaNetFactNode`, and runs an import/apply task.
- The import/apply task imports one operation, updates structured import
  status, projects local facts, and reloads serving snapshots.
- Control socket requests can:
  - report status,
  - wait until at least N imports have been attempted,
  - wait until a serving commit id/revision is visible,
  - trigger explicit rebuild from the local p2panda store,
  - shut down cleanly.
- Local mutation requests still return `mutation_unavailable_in_this_role`.

Tests:

- role readiness does not report healthy projection until initial startup is
  complete,
- shutdown stops import/apply tasks without orphaned children,
- malformed network operation records rejection and does not kill the role,
- projection failure preserves last-good serving state and status names the
  failure audience.

### Unit 3: Process Role Publisher

Files:

- `MVP/e2e/src/process_role_harness.rs`
- `MVP/e2e/src/main.rs`

Requirements:

- Add a short-lived p2panda-net publisher role for serving commits.
- It opens a local `SharedPandaFactStore` with the requested author/grants,
  writes a serving commit payload, publishes the resulting operation through
  `PandaNetFactNode`, prints a JSON ack, and exits.
- It can also publish malformed or wrong-island/untrusted test bodies for the
  rejection path.
- It must not share the receiver's SQLite file. Replication must happen through
  p2panda-net.

Tests:

- publisher returns inserted/already-present/conflict as structured ack,
- publisher fails before publish when its author lacks the fact-write grant,
- wrong-island operation is published but rejected by the receiver.

### Unit 4: E2E Contract

Files:

- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e-proof-plan.md`

Scenario:

1. Reset scenario directory.
2. Spawn receiver serving/projection process with p2panda-net fact node.
3. Read its bootstrap ticket from status.
4. Spawn baseline publisher process and publish `serving-1`.
5. Wait for receiver import/apply and assert gateway/DNS answer `serving-1`.
6. Kill the local coordinator/publisher path, or keep the coordinator socket
   absent and assert mutation unavailable through that path.
7. Spawn remote publisher process using the receiver bootstrap ticket and
   publish `serving-2`.
8. While import/apply is happening, assert serving still answers last good.
9. Wait for receiver to expose `serving-2`; assert gateway/DNS answer updated.
10. Publish malformed, wrong-island, and untrusted-author bodies; assert status
    reports structured rejections and no cross-island leakage.
11. Delete `projections.sqlite`, request rebuild, and assert serving answers
    throughout.
12. Restart receiver with no coordinator and assert it loads snapshots/local
    p2panda store before answering.
13. Write JSON metrics.

Metrics:

- receiver startup ms,
- baseline publish-to-import ms,
- remote publish-to-import ms,
- import-to-projection/reload ms,
- serving outage probes during import,
- projection rebuild ms,
- restart-from-snapshot ms,
- import outcome counts,
- rejection counts by class.

### Unit 5: Documentation And Semantic-Leverage Accounting

Files:

- `MVP/slice-031-p2panda-net-process-serving.md`
- `MVP/overall-plan.md`
- `MVP/e2e-proof-plan.md`
- `MVP/primitive-decisions.md`

Requirements:

- Record that Slice 031 upgrades p2panda-net from in-process fact-node proof to
  process-role serving replication proof.
- Record any helper added to transport config and why it does not leak git
  p2panda types into domain crates.
- Add semantic-leverage numbers:
  - process-role glue added,
  - reusable transport helper LOC added,
  - E2E scenario LOC added,
  - product behavior proven without adding new deploy/ACME/machine code.

## Review Risks

- The import loop may become a hidden reconciler. It must only apply already
  imported facts into local projection/snapshot state; it must not make product
  decisions or mutate durable truth.
- Task shutdown may leak child processes or background tasks. Process cleanup
  tests and orphan registry coverage matter.
- Passing bootstrap info through CLI flags could become stringly. Keep it in
  typed transport wrappers with validation.
- Auto-apply could serialize too much work behind one mutex. Keep the proof
  simple, but status and import path should make slow projection visible.
- Reusing one SQLite store across publisher and receiver would invalidate the
  proof. Stores must be separate; p2panda-net must move the operation.

## Verification

Targeted checks:

```text
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --all-targets
cargo test --manifest-path MVP/Cargo.toml -p mvp-e2e --all-targets
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-process-serving-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-process-role-serving-contract
cargo clippy --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
```

The full E2E gate should stay time-budgeted. If adding process p2panda-net
replication makes the all-scenario run exceed the current 120s budget, the
slice must either reduce unnecessary sleeps/work or document and justify a new
budget with metrics.

## Acceptance Criteria

- `p2panda-net-process-serving-contract` passes and is part of `mvp-e2e -- all`.
- The receiver process updates gateway/DNS serving state from p2panda-net facts
  while no local coordinator is alive.
- Local mutation attempts fail visibly while serving/projection continues.
- Malformed, wrong-island, untrusted-author, and unauthorized-replica paths are
  visible in structured status and do not corrupt last-good serving state.
- Deleted SQLite projection rebuilds from the receiver's local p2panda store
  while serving continues.
- Restart with no coordinator loads last-good snapshots and local p2panda state.
- No code outside `MVP/` changes.
- The implementation commit and later simplify/review commits stay separate if
  the slice grows beyond a narrow change.
