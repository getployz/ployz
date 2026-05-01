# E2E Strategy

Ployz E2E tests live in `crates/ployz-e2e`. They are the only long-running
system harness. There is no separate lab harness.

The NATS-native control-plane work should reshape E2E around product
guarantees, not old implementation mechanics. In particular, machine add is not
a quorum-changing operation. The old quorum-add scenarios were deleted; explicit
storage-promotion scenarios should replace that coverage.

## Current Shape

The current harness starts Docker-backed nodes, installs the current payload,
creates a private outer network, drives `ployzd`/`ployzctl` over SSH, and stores
artifacts under `.e2e-artifacts`.

Useful existing scenarios:

| Current scenario | Keep / replace | Reason |
|------------------|----------------|--------|
| `single_node_init` | keep and deepen | Base R=1 semantics, daemon restart/adoption, NATS health. |
| `machine_add_basic` | keep and change assertions | Machine add should prove membership/connectivity without storage promotion. |
| `machine_add_does_not_promote_storage` | keep | Adds a second and third machine, proving membership changes do not promote storage authority by checking roles and NATS asset replicas. |
| `machine_drain_standby_activate_cycle` | keep | Operator intent and lifecycle state remain important. |
| `mesh_restart_from_seed_cache` | keep and deepen | Data plane/control plane adoption remains core. |
| `wireguard_reconnect` | keep | Substrate continuity matters. |
| `deploy_smoke` | keep and deepen | Deploy should prove NATS lock/commit/projection behavior. |
| `bridge_forward_smoke` | keep | Docker bridge path remains a supported runtime path. |
| `volume_smoke` | keep | Storage primitives remain product-critical. |
| `zfs_transfer_smoke` | keep | Direct TCP is still valid for byte streams. |
| `destroy_with_dead_peer` | revise | Keep loud failure, but express it through NATS membership/status semantics. |

## Target Scenario Set

### Single Node

`single_node_r1_bootstrap`

- Founder starts with R=1 authoritative NATS assets.
- `ployzd` restart adopts NATS, gateway, DNS, WireGuard, and workloads.
- Status clearly reports single-copy storage and no HA claim.

`single_node_control_plane_down`

- Stop the node or NATS.
- Mutations fail loudly.
- No UI/CLI output implies HA or invisible recovery.

### Machine Add

`machine_add_does_not_promote_storage`

- Add second and third machines.
- Assert membership and NATS connectivity.
- Assert authoritative streams/KV remain R=1 until explicit promotion.
- Assert status surfaces storage eligibility separately from storage authority.

`machine_add_offline_joiner_fails_loudly`

- Interrupt joiner startup or NATS connectivity.
- Assert invite/membership state is not silently promoted to active authority.
- Assert the failure lands on the foreground command or operator-visible status.

### Storage Promotion

`storage_promote_r3_plan_reports_data_and_latency`

- Start three eligible machines.
- Run promotion plan.
- Assert plan reports candidate set, current replica state, route reachability,
  RTT/loss, failure domains, capacity, data size, and catch-up estimate.

`storage_promote_r3_rejects_unhealthy_candidate`

- Make one candidate NATS-unhealthy or daemon-unreachable.
- Promotion fails before mutating replica state.

`storage_promote_r3_rejects_same_failure_domain`

- Give candidates identical declared failure-domain metadata.
- Promotion fails or requires an explicit override, depending on final CLI
  policy.

`storage_promote_r3_rejects_high_latency_without_ack`

- Inject latency above the selected class.
- Promotion fails before mutation unless the operator explicitly accepts that
  latency class.

`storage_promote_r3_success_reports_catchup`

- Promote three eligible candidates.
- Assert stream/KV replica count reaches R=3.
- Assert command does not return success until new replicas are current.

### Quorum And Offline Nodes

`r3_one_storage_node_offline_remains_writable`

- Promote to R=3.
- Stop or partition one storage candidate.
- Assert a small KV/stream mutation succeeds after leader election.
- Assert status reports degraded-but-writable.

`r3_below_quorum_blocks_mutations`

- Promote to R=3.
- Stop or partition two storage candidates.
- Assert mutating control-plane operations fail loudly.
- Assert data-plane readers keep using last-good runtime state where applicable.

`offline_leaf_node_command_fails_loudly`

- Add a non-authoritative node.
- Stop its daemon or NATS subscription.
- Assert `node.<machine>.cmd.>` request/reply returns no responders or timeout.
- Assert unrelated quorum-healthy writes still work.

`storage_node_rejoin_catches_up_without_intent_rewrite`

- Promote to R=3.
- Take one storage node offline, perform writes, then rejoin it.
- Assert catch-up completes and membership/operator intent is not rewritten by a
  background policy.

### Removal

`planned_storage_removal_requires_demote`

- Promote to R=3.
- Attempt to remove an authoritative storage node directly.
- Assert removal is rejected with a demotion requirement.

`storage_demote_then_remove_preserves_quorum`

- Promote to R=3 or R=5.
- Run a demotion plan that preserves the requested availability target.
- Remove the node after demotion.
- Assert replica state and membership match the operator plan.

`unplanned_storage_loss_requires_operator_choice`

- Lose a storage node without a demotion.
- Assert status reports the loss and available options: wait, replace, demote,
  or explicit degradation.
- Assert no background task rewrites membership or replica intent.

### Upgrade

`rolling_upgrade_checks_quorum_between_nodes`

- Promote to R=3.
- Upgrade one node at a time.
- After each node, assert NATS health, stream replica state, daemon version, and
  data-plane continuity before moving on.

`upgrade_rejects_when_quorum_is_already_degraded`

- Put R=3 into a degraded state.
- Attempt rolling upgrade.
- Assert the command fails before restarting another storage candidate.

### Regions And Latency

`regional_quorum_latency_profile`

- Run R=3 in a regional latency class.
- Record KV write latency, stream publish latency, node request/reply latency,
  and leader-election recovery time.

`cross_region_quorum_requires_ack`

- Inject cross-region latency between candidates.
- Assert R=3 promotion requires explicit acceptance of the latency class.

`regional_mirror_read_local_write_owner_explicit`

- Configure a remote mirror/read locality shape.
- Assert reads can be local/mirrored, but writes remain owned by the selected
  region.

`region_failover_requires_operator_promotion`

- Simulate primary-region loss with mirrors present.
- Assert writes do not automatically move to the mirror region.
- Assert mirror promotion/failover is an explicit operator operation.

### Deploy And Runtime

`deploy_lock_is_single_nats_lease`

- Start two concurrent deploys to the same namespace.
- Assert exactly one acquires `locks.deploy.<namespace>`.
- Assert stale release cannot drop a newer lease.

`deploy_commit_visibility_is_atomic`

- During deploy, assert routes do not point at candidates before readiness and
  commit.
- After commit, assert gateway/DNS projections observe one coherent release.

`deploy_participant_offline_fails_foreground`

- Make a required participant unreachable.
- Assert deploy apply fails to the foreground caller with the target named.
- Assert no partial commit is presented as success.

### ZFS Transfer

`zfs_transfer_control_over_nats_payload_over_tcp`

- Assert setup/metadata commands use NATS request/reply.
- Assert the payload transfer uses direct TCP.
- Measure setup latency separately from byte throughput.

## Required E2E Instrumentation

The NATS-native suite needs helpers that can inspect NATS-visible state:

- stream replica count is available through `ployzd --json status` and asserted
  by `machine_add_does_not_promote_storage`,
- stream leader,
- stream/consumer lag,
- KV bucket metadata and revision,
- object store metadata,
- request/reply no-responder vs timeout,
- per-command elapsed time,
- NATS route/leaf health,
- mirror lag,
- node-local daemon/NATS status.

Latency observations should be written into scenario artifacts as structured
JSON. They should not be pass/fail SLOs at first; they are semantic evidence for
where time is spent.

## Migration Order

1. Add remaining NATS inspection helpers for leader, lag, KV revisions, and
   no-responder/timeout classification.
2. Add storage-promotion plan guardrail scenarios.
3. Add R=3 success, one-loss, below-quorum, and rejoin scenarios.
4. Add deploy-lock and participant-offline scenarios.
5. Add latency/region/mirror scenarios once topology support exists.
