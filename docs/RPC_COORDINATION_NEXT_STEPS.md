# RPC Coordination: Next Steps

## Model

Each node owns its own state. When another node needs to know something, it
asks.

Liveness, reachability, and "ready to accept work" are not replicated, not
written to the store, and not inferred from timestamps. They are a single RPC
call to the node itself, answered from in-process memory.

The store holds committed intent — membership, subnet allocations, routing and
deploy records, and operator-set drain state. It does not hold liveness
pulses.

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
- **Less code.** Three background tasks, a classifier enum, and a handful of
  constants are deleted outright.

## Trade-off to own

Decision-time RPC cost. Deploy planning, `doctor`, and `machine list` fan out a
`NodeStatus` call with a short deadline. Bounded, parallel, infrequent. Hot
paths (routing, connection handling) do not pre-check liveness — they try, and
a failure is the answer.

---

## Phase 1: `NodeStatus` RPC

Introduce a single RPC that returns the node's view of itself, served from
in-process state. Zero store reads on the hot path (drain is mirrored into
memory from a store subscription).

**`crates/ployz-api/src/lib.rs`:**

```rust
pub struct NodeStatusPayload {
    pub machine_id: MachineId,
    pub boot_id: BootId,
    pub phase: Phase,
    pub ready: bool,
    pub draining: bool,
    pub subnet_claim: Option<Subnet>,
    pub workloads: WorkloadSummary,
    pub version: String,
}
```

- No timestamp. The fact that the node responded is the freshness signal.
- `boot_id` is a monotonic generation marker assigned once at daemon start
  (UUID or nanos-since-epoch at boot). It does not encode time; it only needs
  to differ between process lifetimes. Its consumer is deploy apply — see
  [Failure modes](#failure-modes).
- `ready` means "healthy and able to accept work." `draining` is operator
  intent, orthogonal. A node can be `ready: true, draining: true` — working
  fine, but the operator has asked for it to be taken out of rotation.

**`crates/ployzd/src/daemon/handlers/node_status.rs` (new):** serves the
payload from the orchestrator's in-memory state.

**`crates/ployz-sdk`:** add `DaemonClient::node_status()`.

**`crates/ployzd/src/rpc/node_status_fanout.rs` (new):** mirrors
`fanout_prepare` in shape. Parallel, deadline-bounded.

```rust
pub enum NodeStatusResult {
    Ok(NodeStatusPayload),
    Offline,                                   // timeout or connection refused
    InvalidIdentity { reported: MachineId },   // target said it was someone else
}

pub async fn fanout_node_status(
    targets: &[FanOutTarget],
    deadline: Duration,
) -> Vec<(MachineId, NodeStatusResult)>;
```

The fanout helper compares each target's expected `machine_id` against the
payload's `machine_id` and surfaces mismatch as a distinct `InvalidIdentity`
result. This catches overlay-IP reuse, stale fanout target lists, and
cross-wired peers before they can feed a planning decision.

## Phase 2: Rewrite consumers to pull

**`crates/ployz-orchestrator/src/deploy/planning.rs`:**

Split the quorum rule by operation class. Preview is advisory and tolerates
partial availability; apply mutates and enforces strict quorum.

- **Eligibility predicate (both):** responded within the deadline with
  `ready == true && draining == false` and matching `machine_id`.
- **`preview`:** best-effort. Compute the plan against eligible peers.
  Unreachable peers, `InvalidIdentity` results, and draining peers land in
  `DeployPreview.warnings`. Never returns `QuorumLost`. Preserves "can I see
  the plan?" during a transient partition.
- **`apply`:** strict. If eligible peers + self do not meet
  `cluster_size / 2 + 1`, return
  `QuorumLost { unreachable, drained, invalid_identity }`. No changes made.

Delete the `machine_is_fresh` filter. Delete the `Participation::Enabled`
filter — `ready && !draining` replaces it.

**`crates/ployzd/src/daemon/handlers/doctor.rs`:**

- `build_participation_rows` becomes `build_node_rows`. Fans out `NodeStatus`
  at command time. Renders `reachable`, `ready`, `draining`, and each node's
  self-reported phase.
- Overlay probe stays — that is data-plane diagnostics, a different question.

**`crates/ployzd/src/daemon/handlers/machine/render.rs`:**

- `format_liveness` deleted. Columns sourced from the live fanout: `present /
  absent`, plus an explicit `draining` column when applicable.
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
- `SelfRecordMutation::RefreshLiveness`
- `SelfLivenessCommand`, `HeartbeatCommand`, `ParticipationCommand`
- `PARTICIPATION_HYSTERESIS_SAMPLES`
- `MeshReadyStatus.heartbeat_started`, `MeshReadyPayload.heartbeat_started`

**`Participation` is reshaped, not deleted.** The three-state
`Enabled/Draining/Disabled` enum existed to drive hysteresis-based automatic
disablement — that behavior goes. Operator drain is a real, distinct piece of
committed intent and stays:

- Replace the enum with `MachineRecord::drain: bool`.
- Set via an explicit `machine drain <id>` / `machine undrain <id>` command
  that writes to the store.
- Each node subscribes to changes on its own record and mirrors `drain` into
  in-memory state that `NodeStatus` reads.
- `deployable_machines` filters on `ready && !draining` where both come from
  the live `NodeStatus` fanout.

This keeps operator intent as durable committed state (aligned with the
philosophy doc) and separates it cleanly from liveness.

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
- **Operator intent (drain):** durable in the store, mirrored into each
  node's in-memory state, exposed on `NodeStatus.draining`.
- **Mutations:** `SubnetClaim*` and `LockAcquire(DeployNamespace)` use quorum
  prepare/commit with lease/nonce idempotency.
- **Background tasks still running:** reconcile node-local kernel state or
  subscribe to store events. None push liveness outward.

---

## Readiness

A node's `ready` is a single Boolean it computes from its own state:

```
self_ready = phase == Running
          && store_healthy
          && sync_connected
          && self_record_published
          && overlay_interface_up
```

It appears in `NodeStatus.ready`. `draining` is separate — it reflects
operator intent, not health. `MeshReadyPayload` becomes
`{ ready, phase, store_healthy, sync_connected, self_record_published }`.

## Bootstrap

A newly joining node answers `NodeStatus` as soon as its RPC listener is up,
with `ready: false` until its self-record is published and the overlay is
live. It needs no peer ledger state to become queryable, which removes the
chicken-and-egg problem from the prior plan.

## Failure modes

- **Peer unreachable at decision time.** Counts as not-eligible for that
  decision. No global state mutation. A subsequent operation re-fans-out.
- **`InvalidIdentity`.** Target answered but reported a different
  `machine_id`. Treated as not-eligible and surfaced in warnings/logs.
  Usually means overlay-IP reuse, a stale target list, or a mis-routed RPC.
- **Self-fence.** A node that cannot satisfy its readiness predicate reports
  `ready: false` in its own `NodeStatus`. No external supervisor required.
- **Quorum loss during deploy apply.** Returns `QuorumLost` with unreachable,
  drained, and invalid-identity peers. Operator retries or removes peers
  explicitly. Preview never returns `QuorumLost`; it degrades into a plan
  plus warnings.
- **Peer restart mid-operation (ABA).** Deploy apply is two-phase: prepare
  across the quorum, then commit. A peer that restarts between phases has
  lost its in-memory prepare tokens. Record each peer's `boot_id` in the
  prepare result; verify at commit that `boot_id` is unchanged. Mismatch
  aborts the apply with `BootIdMismatch`.

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

- Preview with one peer partitioned: returns a plan with a warning naming
  the unreachable peer. Does not fail.
- Apply with one of three peers partitioned: `QuorumLost` with the peer
  listed. No state changes.
- Apply where a peer restarts between prepare and commit: aborts with
  `BootIdMismatch`. No partial deploy.
- Operator drain: drained peer is excluded from planning; comes back after
  undrain with no stale flag anywhere.
- Cross-wired identity: inject a peer whose `NodeStatus` reports the wrong
  `machine_id` — treated as `InvalidIdentity`, not eligible.
- Churn: peer joins, deploys, leaves, rejoins; no background reconciler
  required to converge.

Existing e2e suites that must continue to pass:

```bash
cargo test -p ployz-e2e -- single_node_init
cargo test -p ployz-e2e -- machine_add_basic
cargo test -p ployz-e2e -- quorum_subnet_coordination
cargo test -p ployz-e2e -- deploy_smoke
```
