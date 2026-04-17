# RPC Coordination: Next Steps

## Model

Each node owns its own state. When another node needs to know something, it
asks.

Liveness, reachability, and "ready to accept work" are not replicated, not
written to the store, and not inferred from timestamps. They are a single RPC
call to the node itself, answered from in-process memory.

The store holds committed intent — membership, subnet allocations, routing and
deploy records. It does not hold liveness pulses.

Mutations continue to use the existing coordination primitives: prepare, renew,
commit, abort, with quorum fanout and lease/nonce idempotency. Leases belong to
transactional intent, not to "is this node alive?"

## Why this is better

- **Fresh by construction.** Every answer comes from the authority at the
  moment of the question. No clock skew, no replication lag, no stale
  timestamps, no arbitrary `STALE_*_SECS` constants.
- **Partition-correct by construction.** If you cannot reach a node, that is
  its status from your vantage point for this decision. No hysteresis required.
- **One question, one answer.** Reviewers do not chase liveness through the
  store or across background tasks.
- **Less code.** Three background tasks, a classifier enum, a three-state
  participation machine, and a handful of constants are deleted outright.

## Trade-off to own

Decision-time RPC cost. Deploy planning, `doctor`, and `machine list` fan out a
`NodeStatus` call with a short deadline. Bounded, parallel, infrequent. Hot
paths (routing, connection handling) do not pre-check liveness — they try, and
a failure is the answer.

---

## Phase 1: `NodeStatus` RPC

Introduce a single RPC that returns the node's view of itself, served from
in-process state. Zero store reads.

**`crates/ployz-api/src/lib.rs`:**

```rust
pub struct NodeStatusPayload {
    pub machine_id: MachineId,
    pub phase: Phase,
    pub ready: bool,
    pub subnet_claim: Option<Subnet>,
    pub workloads: WorkloadSummary,
    pub version: String,
}
```

No timestamp. The fact that the node responded is the freshness signal.

**`crates/ployzd/src/daemon/handlers/node_status.rs` (new):** serves the
payload from the orchestrator's in-memory state.

**`crates/ployz-sdk`:** add `DaemonClient::node_status()`.

**`crates/ployzd/src/rpc/node_status_fanout.rs` (new):** mirrors
`fanout_prepare` in shape — takes `&[FanOutTarget]` and a deadline, returns
`Vec<(MachineId, Result<NodeStatusPayload, Offline>)>` in parallel.

## Phase 2: Rewrite consumers to pull

**`crates/ployz-orchestrator/src/deploy/planning.rs`:**

- `deployable_machines` becomes async. Fans out `NodeStatus` to peers with a
  bounded deadline.
- Eligible = responded with `ready == true` within the deadline.
- Quorum: if fewer than `cluster_size / 2 + 1` replies including self, return
  `QuorumLost` with the list of unreachable peers.
- Delete the `machine_is_fresh` filter and the `Participation::Enabled`
  filter. Readiness is expressed once, by the node itself.

**`crates/ployzd/src/daemon/handlers/doctor.rs`:**

- `build_participation_rows` becomes `build_node_rows`. Fans out `NodeStatus`
  at command time. Renders `reachable: yes/no` and each node's self-reported
  phase.
- Overlay probe stays — that is data-plane diagnostics, a different question.

**`crates/ployzd/src/daemon/handlers/machine/render.rs`:**

- `format_liveness` deleted. A single column sourced from the live fanout:
  `present` / `absent`.
- `format_heartbeat` deleted.

## Phase 3: Delete

Whole files go:

- `crates/ployz-orchestrator/src/machine_liveness.rs`
- `crates/ployz-orchestrator/src/mesh/tasks/self_liveness.rs`
- `crates/ployz-orchestrator/src/mesh/tasks/heartbeat.rs`
- `crates/ployz-orchestrator/src/mesh/tasks/participation.rs`

Fields and types go:

- `MachineRecord::last_heartbeat`
- `MachineLiveness` enum, `STALE_HEARTBEAT_SECS`, `machine_liveness()`,
  `machine_is_fresh()`
- `Participation` enum — the three-state hysteresis was machinery in service
  of a question the node now answers itself. Its remaining callers in
  `deploy/mod.rs`, `self_record.rs`, and `lifecycle.rs` either drop the filter
  or fold into `NodeStatus.ready`.
- `SelfRecordMutation::RefreshLiveness`
- `SelfLivenessCommand`, `HeartbeatCommand`, `ParticipationCommand`
- `PARTICIPATION_HYSTERESIS_SAMPLES`
- `MeshReadyStatus.heartbeat_started`, `MeshReadyPayload.heartbeat_started`

The mesh task directory shrinks from eight tasks to five: `self_record`,
`peer_sync`, `endpoint_refresh`, `subnet_claim_monitor`, `ebpf_sync`. All five
are either event-driven or reconcile node-owned kernel state (WireGuard, eBPF,
endpoints). None pulse liveness.

## Phase 4: Dead coordination code

- Remove `CoordinationLockKey::MembershipMachine` and `MachineOperation`.
- Remove `CoordinationOperation::MembershipPrepare`, `MembershipCommit`,
  `MembershipAbort`.
- Keep `CoordinationRenewRequest` — used by subnet claims and deploy locks.

## Phase 5: Update the philosophy doc

Add a **Current status** section to `RPC_COORDINATION_PHILOSOPHY.md`:

- **Liveness:** pulled at decision time via `NodeStatus`. No replication, no
  leases for liveness.
- **Mutations:** `SubnetClaim*` and `LockAcquire(DeployNamespace)` use quorum
  prepare/commit with lease/nonce idempotency.
- **Background tasks still running:** reconcile node-local kernel state or
  subscribe to store events. None exist to push liveness outward.

---

## Readiness

A node's readiness is a single Boolean it computes from its own state:

```
self_ready = phase == Running
          && store_healthy
          && sync_connected
          && self_record_published
          && overlay_interface_up
```

It appears in `NodeStatus.ready`. `MeshReadyPayload` becomes
`{ ready, phase, store_healthy, sync_connected, self_record_published }`.

## Bootstrap

A newly joining node answers `NodeStatus` as soon as its RPC listener is up,
with `ready: false` until its self-record is published and the overlay is
live. It needs no peer ledger state to become queryable, which removes the
chicken-and-egg problem from the prior plan.

## Failure modes

- **Peer unreachable at decision time.** Counts as not-eligible for that
  decision. No global state mutation. A subsequent deploy re-fans-out and
  re-evaluates.
- **Self-fence.** A node that cannot reach its dependencies reports
  `ready: false` in its own `NodeStatus`. No external supervisor required.
- **Quorum loss during deploy planning.** Returns `QuorumLost` with the list
  of unreachable peers. Operator retries or removes peers explicitly.

---

## Verification

**Phase 1 — `NodeStatus` RPC:**

```bash
cargo build --workspace
cargo test -p ployzd -- node_status
cargo test -p ployz-sdk -- node_status
```

**Phase 2 — rewrite consumers:**

```bash
cargo test -p ployz-orchestrator -- deploy::planning
cargo test -p ployzd -- doctor
cargo test -p ployzd -- machine::render
```

**Phase 3 — deletions:**

```bash
cargo build --workspace
cargo test --workspace
```

**Phase 4 — dead coordination code:**

```bash
cargo test -p ployzd -- coordination
```

**E2E coverage to add (beyond happy path):**

- Deploy while one of three peers is partitioned: expect `QuorumLost` or
  correct exclusion depending on cluster size.
- Peer returns mid-session: a re-run produces a consistent plan with no stale
  liveness state anywhere.
- Churn: peer joins, deploys, leaves, rejoins; no background reconciler
  required to converge.

Existing e2e suites that must continue to pass:

```bash
cargo test -p ployz-e2e -- single_node_init
cargo test -p ployz-e2e -- machine_add_basic
cargo test -p ployz-e2e -- quorum_subnet_coordination
cargo test -p ployz-e2e -- deploy_smoke
```
