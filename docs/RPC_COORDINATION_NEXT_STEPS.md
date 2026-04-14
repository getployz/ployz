# RPC Coordination: Next Steps

## Context

The coordination ledger (prepare/commit/renew/abort with quorum fanout) is
fully implemented and used by machine join and deploy namespace locking. But
the rest of the system still runs on legacy patterns: heartbeat timestamps for
liveness, a `MachineLiveness::Fresh/Stale/Down` classifier, a participation
task with hysteresis-based TCP probing, and periodic background loops that poll
rather than react.

The RPC philosophy doc says: "no hidden eventual-heal magic in background
tasks" and "operation-time truth over periodic reconciliation." The code does
not yet live up to that. This plan replaces the legacy liveness and
participation infrastructure with coordination-lease-based presence, and
removes the background loops that exist only because the old model required
them.

---

## Phase 1: Presence Leases — Replace Heartbeat Timestamps

**Problem:** Liveness is currently determined by each machine publishing a
`last_heartbeat` timestamp to the replicated store every 5 seconds. Other
machines classify peers as `Fresh` (heartbeat within 30s), `Stale` (older), or
`Down` (explicit). This is fragile:

- Depends on clock accuracy across machines
- Store replication lag can make a live machine appear stale
- The 30s `STALE_HEARTBEAT_SECS` constant is arbitrary
- Different nodes can disagree on whether a peer is "fresh"

**Replace with:** Each machine holds a **presence lease** in the coordination
ledger of every peer. The coordination `prepare` with same owner/nonce already
acts as lease renewal (see `coordination.rs` lines 92-109). Lease expiry is
deterministic — no timestamp age guessing.

### Changes

**`crates/ployz-api/src/lib.rs`:**
- Add `CoordinationOperation::PresenceLease { machine_id: String }` variant
- Add `CoordinationLockKey::Presence { machine_id: String }` variant

**`crates/ployzd/src/daemon/handlers/coordination.rs`:**
- Add `key_tag` and `operation_key` arms for the new variants

**`crates/ployzd/src/coordination/fanout.rs`:**
- Add `fanout_presence_renew()` — fan out a prepare with the same nonce to all
  known peers. Uses the existing re-prepare-extends-lease behavior. Best-effort
  (offline peers just miss the update — their local ledger expires the lease).

**`crates/ployz-orchestrator/src/mesh/tasks/self_liveness.rs`:**
- Replace `publish_liveness()` (which writes `last_heartbeat` to the store)
  with `renew_presence_lease()` — calls `fanout_presence_renew()` to all peers.
  Same 5s interval initially, but the signal is now a coordination lease, not a
  store timestamp.

**`crates/ployzd/src/daemon/handlers/coordination.rs`:**
- Add `CoordinationLedger::is_presence_active(machine_id) -> bool` — checks
  whether a given machine has a non-expired presence lease in the local ledger.
  This replaces `machine_is_fresh()`.

### What this kills

- `last_heartbeat` field becomes vestigial (kept for backward compat but no
  longer the source of truth for liveness)
- `STALE_HEARTBEAT_SECS` constant — replaced by the lease TTL
- Clock-skew sensitivity — lease expiry is relative to local time of receipt,
  not a timestamp written by a remote machine

---

## Phase 2: Kill MachineLiveness and Freshness

**Problem:** `machine_liveness.rs` exports `MachineLiveness::Fresh/Stale/Down`
and `machine_is_fresh()`. These are consumed by:

- `deploy/planning.rs:167` — filters deployable machines
- `participation.rs:214` — filters required peers
- `doctor.rs:233` — renders peer health
- `machine/render.rs:109` — renders machine list

All of these ask the same question: "is this machine present in the cluster
right now?" The answer should come from the coordination ledger, not from
heartbeat timestamp arithmetic.

### Changes

**`crates/ployz-orchestrator/src/machine_liveness.rs`:**
- Replace `machine_is_fresh()` with a new query: `machine_is_present()` that
  checks the coordination ledger for an active presence lease.
- Remove `MachineLiveness` enum, `STALE_HEARTBEAT_SECS`, and the timestamp
  comparison logic.

**Consumers:**
- `deploy/planning.rs` — `deployable_machines()` filters on
  `machine_is_present()` instead of `machine_is_fresh()`
- `doctor.rs` — renders presence status from ledger
- `machine/render.rs` — renders "present" / "absent" instead of
  "fresh" / "stale" / "down"

### What this kills

- `MachineLiveness` enum
- `STALE_HEARTBEAT_SECS` constant
- `machine_liveness()` function
- `machine_is_fresh()` function
- All heartbeat-age arithmetic

---

## Phase 3: Kill the Participation Task

**Problem:** The participation task (`participation.rs`) runs a 5s loop that:
1. Lists all machines from the store
2. Filters "required peers" by freshness
3. TCP-probes every required peer's overlay IP
4. Applies 3-sample hysteresis to toggle `Participation::Enabled/Disabled`

This exists because the old model had no way to know if a peer was actually
reachable without probing it. With presence leases, reachability is already
answered — if a peer's presence lease is active, it is reachable (it renewed
its lease via coordination RPC through the overlay network).

### Changes

**Remove the participation task entirely:**
- `crates/ployz-orchestrator/src/mesh/tasks/participation.rs` — delete
- `crates/ployz-orchestrator/src/mesh/tasks/mod.rs` — remove exports
- `crates/ployz-orchestrator/src/mesh/orchestrator/task_runtime.rs` — stop
  spawning `run_participation_task`

**Remove the heartbeat coordinator:**
- `crates/ployz-orchestrator/src/mesh/tasks/heartbeat.rs` — delete (it only
  existed to fan out ticks to self_liveness + participation)

**Simplify `Participation` enum:**
- The three-state `Enabled/Disabled/Draining` model was driven by the
  participation task. With presence leases:
  - A machine is eligible for deploys if it holds active presence leases for a
    quorum of peers (already answered by the coordination ledger).
  - `Draining` is replaced by aborting the presence lease and letting it expire.
- `deploy/planning.rs:deployable_machines()` filters on presence instead of
  `Participation::Enabled && machine_is_fresh()`.

**What this kills:**
- `participation.rs` (entire file)
- `heartbeat.rs` (entire file)
- `ParticipationCommand` / `HeartbeatCommand` types
- `ParticipationState` / hysteresis logic
- `PARTICIPATION_HYSTERESIS_SAMPLES` constant
- TCP overlay probing for participation (probing remains for `peer_sync`
  endpoint ranking and `doctor` diagnostics)
- `heartbeat_started` flag in readiness checks

**Readiness impact:** `MeshReadyStatus.heartbeat_started` (readiness.rs:43)
needs to change. Readiness becomes: `phase == Running && store_healthy &&
sync_connected && presence_lease_held`.

---

## Phase 4: Remove Dead Coordination Code

**Why:** While adding presence leases, clean up the unused coordination types
that were never wired in.

### Changes

**`crates/ployz-api/src/lib.rs`:**
- Remove `CoordinationLockKey::MembershipMachine` and `MachineOperation`
- Remove `CoordinationOperation::MembershipPrepare`, `MembershipCommit`,
  `MembershipAbort`

**`crates/ployzd/src/daemon/handlers/coordination.rs`:**
- Remove corresponding `key_tag`, `operation_key`, and
  `commit_matches_prepared_operation` arms
- Remove tests that exercise removed operations

**Keep `CoordinationRenewRequest`** — presence lease renewal could use it as
an alternative to re-prepare (both work, renew is semantically cleaner).

---

## Phase 5: E2E Test Speed

With participation killed and liveness driven by coordination leases, e2e
tests no longer depend on background task intervals for convergence. But the
test infrastructure itself has unnecessary latency.

### Changes

**`crates/ployz-e2e/src/support.rs`:**
- Reduce `POLL_INTERVAL` from `Duration::from_secs(2)` to
  `Duration::from_millis(500)`

**`crates/ployz-e2e/src/daemon_probes.rs`:**
- Reduce settled-state consecutive match threshold from 3 to 2

**`crates/ployz-orchestrator/src/mesh/tasks/mod.rs`:**
- Add `TaskTimingConfig::fast()` (1s intervals) for test environments

**`crates/ployzd/src/daemon/setup.rs`:**
- Read `PLOYZ_TASK_TIMING=fast` env var to apply fast timing in e2e containers

---

## Phase 6: Update RPC Coordination Philosophy Doc

### Changes to `docs/RPC_COORDINATION_PHILOSOPHY.md`

Add a **Current status** section:

- **Coordination primitives in use:** `SubnetClaimPrepare/Commit/Abort` with
  quorum fanout for machine join. `LockAcquire` with `DeployNamespace` for
  deploy locking. `PresenceLease` with periodic renewal for cluster presence.
- **Replaced by RPC coordination:** Heartbeat timestamps (`last_heartbeat`),
  `MachineLiveness::Fresh/Stale/Down`, `STALE_HEARTBEAT_SECS`, participation
  task with TCP probe hysteresis.
- **Kept as periodic tasks:** Presence lease renewal (5s, but through
  coordination RPC, not store writes). Peer sync WireGuard kernel reads (no
  kernel event API). Endpoint refresh (30m external discovery). eBPF sync and
  subnet claim monitor (already event-driven via store subscriptions).

---

## Verification

### Phase 1 (presence leases):
```bash
cargo build --workspace
cargo test -p ployzd -- coordination
cargo test -p ployzd -- fanout
```

### Phase 2 (kill freshness):
```bash
cargo build --workspace
cargo test -p ployz-orchestrator -- machine_liveness
cargo test -p ployzd -- deploy
cargo test -p ployzd -- doctor
```

### Phase 3 (kill participation):
```bash
cargo build --workspace
cargo test -p ployz-orchestrator
cargo test -p ployzd -- handlers
# E2E: verify machines still converge and deploys work
cargo test -p ployz-e2e -- single_node_init
cargo test -p ployz-e2e -- machine_add_basic
cargo test -p ployz-e2e -- quorum_subnet_coordination
cargo test -p ployz-e2e -- deploy_smoke
```

### Phase 4 (dead code):
```bash
cargo build --workspace
cargo test -p ployzd -- coordination
```

### Phase 5 (e2e speed):
Compare wall-clock times before/after across all e2e scenarios.
