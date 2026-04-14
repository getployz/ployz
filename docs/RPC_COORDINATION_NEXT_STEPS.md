# RPC Coordination: Next Steps

## Context

The RPC coordination model (prepare/commit/renew/abort with quorum fanout) is
fully implemented and used by machine join (subnet claims) and deploy namespace
locking. But old polling loops for liveness, participation, and peer sync still
run alongside it. The RPC philosophy doc doesn't reflect what was built vs.
what was aspirational. E2E tests are slow due to conservative poll intervals
and stability thresholds. Dead coordination code (unused Membership operations)
clutters the API surface.

This plan addresses three goals: faster e2e tests, cleaner coordination code,
and an accurate doc that captures what actually needs periodic polling vs. what
the RPC model replaced.

---

## Phase 1: E2E Poll Interval and Settled-State Threshold

**Why:** Every `wait_until()` call sleeps 2s between checks. Settled-state
waits need 3 consecutive matches (minimum 6s pure sleep per call). The quorum
scenario calls this twice = 12s+ of mandatory idle time.

### Changes

**`crates/ployz-e2e/src/support.rs`** line 9:
- Change `POLL_INTERVAL` from `Duration::from_secs(2)` to `Duration::from_millis(500)`

**`crates/ployz-e2e/src/daemon_probes.rs`** lines 106 and 164:
- Change `consecutive_matches >= 3` to `consecutive_matches >= 2` in both
  `wait_for_settled_machine_states` and `wait_for_settled_machine_states_with_ticks`

**Impact:** Settled-state minimum drops from 6s to 1s per call. Across the full
e2e suite this saves ~15-25s of pure sleep.

**Safety:** Test-only constants. SSH commands are stateless/idempotent. 500ms
is still conservative enough to avoid hammering containers.

---

## Phase 2: Fast Task Timing for E2E Containers

**Why:** The daemon uses `TaskTimingConfig::production()` (5s intervals) even
in e2e containers. Participation hysteresis needs 3 healthy samples at interval
rate: 15s at 5s intervals, 3s at 1s intervals. Non-tick-based waits
(`wait_mesh_ready`, `wait_all_machine_states`) depend entirely on these
background intervals.

### Changes

**`crates/ployz-orchestrator/src/mesh/tasks/mod.rs`** after line 48:
- Add `TaskTimingConfig::fast()` with 1s intervals for all three tasks

**`crates/ployzd/src/daemon/setup.rs`** (mesh construction):
- Read env var `PLOYZ_TASK_TIMING`. When `"fast"`, apply
  `.with_task_timing(TaskTimingConfig::fast())` to the mesh builder

**`crates/ployz-e2e/src/nodes.rs`** (container start args):
- Add `-e PLOYZ_TASK_TIMING=fast` to Docker run arguments for e2e containers

**Impact:** Participation convergence drops from 15s to 3s minimum.
`machine_add_basic` and `single_node_init` see the biggest improvement since
they don't use manual tick injection.

**Safety:** Only affects e2e containers via env var. `STALE_HEARTBEAT_SECS`
(30s) is well above 1s interval. Production path unchanged.

---

## Phase 3: Remove Dead Coordination Code

**Why:** `MembershipPrepare/Commit/Abort` operations and
`MembershipMachine`/`MachineOperation` lock keys are defined, handled by the
ledger, and tested — but never invoked by any caller. Machine join uses
`SubnetClaimPrepare/Commit` directly. This dead code inflates the coordination
surface and makes the API harder to reason about.

### Changes (cascade order)

**`crates/ployz-api/src/lib.rs`:**
- Remove `CoordinationLockKey::MembershipMachine` and `MachineOperation` variants
- Remove `CoordinationOperation::MembershipPrepare`, `MembershipCommit`,
  `MembershipAbort` variants
- Spell out all remaining variants at match sites (no wildcards on project enums)

**`crates/ployzd/src/daemon/handlers/coordination.rs`:**
- Remove `key_tag` arms for removed lock key variants
- Remove `operation_key` arms for removed operation variants
- Remove `commit_matches_prepared_operation` arms for Membership operations
- Remove tests exercising Membership operations

**`crates/ployzd/src/daemon/handlers/mod.rs`:**
- Update request lane test data that references removed Membership types

**Keep `CoordinationRenewRequest`:** It is a legitimate primitive for
long-running operations, even though no current flow uses it.

---

## Phase 4: Update RPC Coordination Philosophy Doc

**Why:** The doc describes aspirational design intent without reflecting what
was built, what was kept as polling, and why. After Phase 3 prunes the dead
code, the doc should capture these decisions.

### Changes to `docs/RPC_COORDINATION_PHILOSOPHY.md`

Add a **Current status** section covering:

- **Implemented and in use:** `SubnetClaimPrepare/Commit/Abort` with quorum
  fanout for machine join. `LockAcquire` with `DeployNamespace` key for deploy
  locking. Full two-phase coordination with quorum intersection.
- **Implemented, available for future use:** `CoordinationRenewRequest` for
  long-running operations that need lease extension.
- **Kept as periodic polling (by design):** Self-liveness heartbeats (must
  self-report, cannot be event-driven). Peer sync WireGuard kernel reads (no
  kernel notification API). Participation TCP probes (lightweight, tests actual
  overlay connectivity). Endpoint refresh (30-minute external discovery).
- **Already event-driven (no change needed):** eBPF sync (store subscription).
  Subnet claim monitor (store subscription, observation only).
- **Removed as dead code:** `MembershipPrepare/Commit/Abort` operations
  (SubnetClaim covers the same coordination surface). `MembershipMachine` and
  `MachineOperation` lock keys (never used by any caller).

---

## Verification

### Phase 1+2 (speed):
```bash
# Run e2e test suite and compare wall-clock time before/after
cargo test -p ployz-e2e -- single_node_init
cargo test -p ployz-e2e -- machine_add_basic
cargo test -p ployz-e2e -- quorum_subnet_coordination
```
Expect: each scenario completes measurably faster (5-15s savings per scenario).

### Phase 3 (dead code removal):
```bash
# Full build ensures all match sites updated
cargo build --workspace
# Run coordination unit tests
cargo test -p ployzd -- coordination
# Run fanout tests
cargo test -p ployzd -- fanout
# Run request lane tests
cargo test -p ployzd -- handlers
```
Expect: clean compile, all remaining tests pass, no Membership types referenced.

### Phase 4 (doc):
Manual review -- doc accurately reflects implementation state.
