---
title: Preflight Authority Promotions Before Mutation
date: 2026-05-08
category: docs/solutions/architecture-patterns/
module: storage authority promotion
problem_type: architecture_pattern
component: tooling
severity: high
applies_when:
  - Promoting machines into control-plane authority roles
  - Reconfiguring every peer in a replicated authority set
  - Persisting operator intent before restarting local runtime components
  - Routing mutating daemon RPCs to mixed or partially upgraded peers
tags:
  - authority
  - nats
  - storage-promotion
  - preflight
  - rollback
  - bootstrap-peers
---

# Preflight Authority Promotions Before Mutation

## Context

The NATS storage promotion slice added `ployzd machine storage promote` to move
active storage candidates into the replicated authority set. Review found that
the first implementation handled the happy path but left the protocol too
optimistic for real authority changes: duplicate or invalid targets could be
partially processed, old authorities were not always reconfigured, local
bootstrap peers were not written before restart, and remote peers were mutated
without first proving that every final authority daemon understood the new
command.

The durable pattern is to treat control-plane promotions as a small transaction:
validate the final authority set, prove all remote participants are compatible,
mutate peers only after preflight passes, then persist intent and bootstrap
inputs with rollback around local restart.

## Guidance

Build and validate the final authority set before any mutation. The storage
promotion handler now rejects duplicate targets, requires active storage
candidates, counts unique final authority IDs, and ensures the local record is
active storage for the default authority.

```rust
let final_authority_ids = authorities
    .iter()
    .chain(targets.iter())
    .map(|machine| machine.id.clone())
    .collect::<BTreeSet<_>>();
if final_authority_ids.len() != request.replicas.replicas() {
    return Err(StoragePromotionError::ReplicaCount { message });
}
```

Preflight every remote final authority before sending any mutating command. This
includes existing authorities during R3 to R5 expansion, not only newly promoted
candidates. A conservative same-version `Status` RPC is enough for this slice:
older daemons, misrouted subjects, bad payloads, and mismatched machine IDs fail
before the cluster is changed.

```rust
let remote_authorities = authority_peers
    .iter()
    .filter(|machine| machine.id != local_record.id)
    .collect::<Vec<_>>();
preflight_remote_storage_promotion(&client, &remote_authorities).await?;
```

Persist all bootstrap peer records before restarting local authority storage.
The founder needs the same final authority peer set as remote promoted machines,
otherwise restart can come up with stale single-node bootstrap inputs.

```rust
let peer_records = authority_peers
    .iter()
    .map(BootstrapPeerRecord::from_machine_record)
    .collect::<Vec<_>>();
write_bootstrap_peer_records(&network_dir, &peer_records)?;
```

Make failures structured and audience-aware. `StoragePromotionError` carries the
stage, promoted targets, per-machine failures, and rollback errors, so callers
can return a useful payload without parsing display text.

## Why This Matters

Authority promotion changes who owns durable control-plane state. A partial
mutation can create peers that believe they are replicated authorities while the
founder still restarts as a single-node store, or existing authorities that keep
the old replica policy while new peers use the new one.

Compatibility checks are part of the protocol, not a convenience. A missing
handler on an older daemon is a preflight failure; discovering it after one peer
has already changed roles leaves the operator with an avoidable recovery
problem.

Rollback also has to cover the local files that feed restart. Restoring only the
replica intent is incomplete if bootstrap peer records were already rewritten.

## When to Apply

- A command changes authority, coordination, placement, or storage participation.
- The final set of participants is more important than the requested delta.
- Existing peers need to receive the new configuration as well as new members.
- A daemon-to-daemon RPC is newly introduced and mixed-version peers may exist.
- Restart depends on local files generated during the operation.

## Examples

`crates/ployzd/src/daemon/handlers/machine/storage.rs` now sends
`MachineStoragePromoteSelf` to every non-local final authority after a status
capability preflight, so replica policy changes reach existing authorities and
new targets together.

`handle_machine_storage_promote_self` persists `storage_participation`,
`storage_replicas`, and the final bootstrap peer records before
`RuntimeRestartMode::NetworkAndStore`, then updates the authoritative self-record
after restart succeeds.

`machine_storage_promote_self_persists_config_bootstrap_peers_and_self_record`
covers the remote self-promotion handler directly, and
`machine_storage_promote_marks_active_candidates_as_authority_storage` verifies
that the founder writes the final authority bootstrap peers in the memory path.

## Related

- `docs/authority-roadmap.md` tracks storage authority promotion as a stepping
  stone toward multi-authority NATS state.
- `docs/plans/2026-05-08-003-feat-nats-storage-promotion-slice-plan.md`
  describes the slice that introduced this command.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  covers the adjacent status-surface rule: separate durable truth from live
  observation.
