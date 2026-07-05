# JetStream Data Audit

Superseded for target architecture by ADR 0028 and ADR 0029. This document
describes the pre-exit codebase and remains useful only as a deletion map for
the JetStream exit plan.

This audits the JetStream data stored by the current codebase. It excludes
plain NATS request/reply subjects unless a JetStream stream or KV bucket stores
them.

Reindexability here means recoverability after JetStream loss by an explicit
reindex operation using non-JetStream evidence: Docker reality, machine-local
facts, role-process facts, local NATS authorization files, and fresh
observations. The reindex operation is still a required future capability, so
the answers below describe the intended recovery contract, not an implemented
command.

## Current Resources

Bootstrap currently creates these resources:

| Resource | Kind | Storage | Replicas | Current role |
| --- | --- | --- | --- | --- |
| `KV_CORE` | JetStream KV | file | 1 | Current control-plane indexes and explicitly named NATS authority. |
| `KV_OPS` | JetStream KV | file | 1 | Operation status projection, machine-add pending-join handoff, mint claims, and temporary secret delivery records. |
| `KV_OBS` | JetStream KV | file | 1 | Latest machine and role observations. |
| `PLZ_OPS` | stream over `plz.v1.op.>` | file | 1 | Operation event timeline and replay source. |

No Object Store bucket is currently bootstrapped. `ObjectBucketSpec` exists for
future resources, and cert models can validate `obj://...` references, but
there is no current production Object Store writer. `PLZ_BACKUPS` is explicitly
not created in the current control-runtime tests.

Older plan documents mention `KV_LOCKS`, `PLZ_JOBS`, `PLZ_AUDIT`,
`PLZ_OBS_TRANSITIONS`, `PLZ_SCHEDULES`, message schedules, durable consumers,
and Object Store buckets. Those are not current stored resources in the active
manifest.

Direct machine observation subjects such as `plz.v1.obs.machine.<machine_id>.*` are not
captured by a stream today. The persisted latest observations live in
`KV_OBS`.

## `KV_CORE`

`KV_CORE` stores current cluster indexes and the NATS authorized-principal set.
Most records are intended to be rebuildable indexes. The authorized-principal
set is different: it is durable authority with an on-disk recovery file.

| Key pattern | Stored value | Writer | Classification | Reindexable? |
| --- | --- | --- | --- | --- |
| `services.<service_id>` | `ActiveServiceState { service_id, active_revision }` | Deploy worker after successful active-service commit. | Rebuildable current-state index. | Yes, with ambiguity checks. Rebuild from managed Docker containers and machine facts that prove the current serving revision. Do not adopt if multiple revisions or missing evidence make the serving target ambiguous. The original operation id and timeline are not recovered here. |
| `routes.<hex_hostname>.<port>` | `ActiveRouteState { target, endpoint_port, service_id, revision_id }` | Deploy worker during route cutover; gateway and DNS watch this prefix. | Rebuildable route index. | Yes, with ambiguity checks. Rebuild from machine-local route facts and gateway/DNS last-known-good facts. Conflicting route claims must stay ambiguous until a repair operation resolves them. |
| `machines.<machine_id>` | `ActiveMachineState { machine_id, name, activated_by }` | Machine join report / first-machine activation after machine-add completion. | Current machine index. | Partially. Current machine identity can be adopted from reconnecting machines, keeper/local authority, and NATS authorization recovery evidence. The exact historical `activated_by` operation id is operation history; it is only exact if operation history or backup survives. |
| `nats_authorized_user.<authority_key>` | `NatsAuthorizedUser { principal, nkey_public }` | Machine credential minting; control start also adopts entries from `authorized-users.conf`. | Explicitly named durable authority. | Yes from local authority, not from inference. Rebuild by reading `authorized-users.conf` and adopting missing principals into KV before rendering. If both KV and that file are lost, the authority set is not reindexable. |

Current code does not store active certificates under `KV_CORE`; active certs
appear as operation-event payloads.

## `KV_OBS`

`KV_OBS` stores latest observations. These are not cluster truth; they are
freshness-sensitive read-side inputs.

| Key pattern | Stored value | Writer | Classification | Reindexable? |
| --- | --- | --- | --- | --- |
| `containers.<machine_id>` | `MachineContainerObservationSnapshot` | Machine process scans local Docker and replaces the machine snapshot. | Live observation. | Yes. A machine reconnect or observation tick rebuilds this from Docker labels and runtime state. Until then, passive projections should treat the machine as missing or stale. |
| `machines.<machine_id>.public_ip` | `MachinePublicIpObservation` | Machine process replaces or deletes the public IP observation. | Live observation. | Yes. Rebuilt by machine observation. Loss only removes cached public-IP evidence until the machine republishes. |
| `gateways.<machine_id>.status` | `GatewayStatusObservation` | Gateway process refresh loop after applying route state. | Live role observation. | Yes. Rebuilt by the gateway process. Missing or stale gateway observations are warning/diagnostic evidence, not durable membership. |

KV watch history on these keys is an invalidation mechanism. It is not a
durable event log that reindex should preserve.

KV revisions and delete tombstones are NATS-managed bucket metadata, not
product records; reindex should rebuild current values, not revision numbers.

## `KV_OPS`

`KV_OPS` multiplexes several operation-memory families. These records are
useful across control-process restarts, but after JetStream loss they are not
cluster truth.

| Key pattern | Stored value | Purpose | Classification | Reindexable? |
| --- | --- | --- | --- | --- |
| `ops.<operation_id>` | `OperationStatus` | Latest user-visible operation status projected from accepted events and transitions. | Disposable operation memory / status projection. | No after JetStream loss. If `PLZ_OPS` survives, status could be projected again in principle, but current reindex recovery should not infer exact operation state from machine facts. In-flight operations should fail visibly and be retried as new operations. |
| `machine_add_claims.<idempotency_key>` | `StoredMachineAddClaim` with operation id, machine id, machine name, roles, join bundle, issued join token, and raw join token. | First-writer claim for machine-add idempotency and join material before the accepted submission has a stream sequence. Same-operation retries adopt this record. | Disposable operation claim with pending-join material. | No. If the claim survives, retry the same operation idempotency key to finish acceptance. If KV is lost, submit a new machine-add operation. |
| `machine_add_submissions.<idempotency_key>` | `StoredMachineAddSubmission` with operation id, start sequence, machine id, machine name, roles, join bundle, issued join token, and raw join token. | Accepted machine-add join material and startup resume of unfinished credential mints. Same-operation retries adopt this record to finish status projection. | Disposable operation memory with pending-join material. | No. If the machine has already become current, reindex current machine/auth state separately. If the machine add is still pending, the join cannot be recovered from non-JetStream evidence. |
| `machine_add_join_tokens.<fingerprint>` | `StoredMachineAddJoinToken { operation_id, idempotency_key }` | Lookup from presented raw join token to the accepted machine-add submission. | Disposable pending-join index. | No. It only supports a pending join. |
| `machine_add_mint_claims.<idempotency_key>` | `StoredMachineAddMintClaim { operation_id, nkey_public, nkey_seed }` | Write-once atomic claim for minted machine credentials so resumed or concurrent mint workers converge. | Atomic operation claim / secret-bearing operation memory. | No. The NKey seed cannot be derived from the public key. If lost before delivery, mint through a new machine-add operation. If already delivered, current authority can be rebuilt from `authorized-users.conf` and the machine's local credential. |
| `machine_add_secret_deliveries.<idempotency_key>` | `StoredMachineAddSecretDelivery { operation_id, secret_delivery }` containing the machine NATS seed. | Temporary handoff record used by join redemption after material is ready. Deleted after join report records completed or failed. | Temporary secret-bearing operation memory. | No. Loss before redeem means the pending join cannot receive credentials from reindex. Loss after successful join report does not matter. |

Audit note: `machine_add_submissions.*` stores a raw join token and
`machine_add_mint_claims.*` stores an NKey seed. `machine_add_secret_deliveries.*`
is deleted after join report, but mint claims are not deleted by current code.
That can be intentional operation evidence, but it should be treated as
secret-bearing JetStream state in backup, retention, and reindex policy.

## `PLZ_OPS`

`PLZ_OPS` stores operation events on `plz.v1.op.>`. It uses file storage,
single-core replication, limits retention, and old-message discard. The code
sets stable `Nats-Msg-Id` values for duplicate detection.

| Subject family | Stored events | Classification | Reindexable? |
| --- | --- | --- | --- |
| `plz.v1.op.<operation_id>.deploy.*` | Deploy submitted payload, planning/running transitions, deploy plan evidence, dataplane prepare evidence, started containers, health-check start, cleanup evidence, terminal completed/failed/cancelled events. | Optional evidence/history plus event source for `KV_OPS` status while JetStream survives. | No after JetStream loss. Runtime service/container facts may rebuild current state, but not the exact event sequence, submitted payload, or operator-facing timeline. |
| `plz.v1.op.<operation_id>.cert.*` | Cert submit, challenge published, validation started, completed with `ActiveCertState`, or failed. | Optional evidence/history. | No after JetStream loss. Current code has no separate active-cert KV index or Object Store writer to reindex from. |
| `plz.v1.op.<operation_id>.machine.add.*` | Machine-add submitted, joined, credential provisioning steps, completed, failed. | Optional evidence/history plus machine-add operation progression. | No after JetStream loss. Current machine/auth state is recovered separately from current machine facts and NATS authority evidence. |
| `plz.v1.op.<operation_id>.backup.*` | Backup submit, running stages, completed manifest, failed, cancelled. | Optional evidence/history. | No after JetStream loss. Backup artifacts and manifests live through the configured backup adapter, currently S3-shaped, not JetStream Object Store. |
| `plz.v1.op.<operation_id>.cancelled` | Generic cancellation event used by operation kinds. | Optional evidence/history. | No after JetStream loss. |

`KV_OPS.ops.*` is a projection of these events for normal operation status
reads. That projection relationship is useful when the stream is intact; it
does not make operation history reindexable after JetStream loss.

## Object Store

No JetStream Object Store bucket is active today.

Known non-stored or future references:

| Name / shape | Current status | Reindexability note |
| --- | --- | --- |
| `PLZ_BACKUPS` | Not created. Backup artifacts are written through backup adapters; current tests assert this Object Store is absent. | Not applicable. |
| `obj://PLZ_CERTS/...` / certificate bundle refs | The type layer validates Object Store refs, but no bucket or writer is bootstrapped. | Future cert material storage must be classified before it is introduced. If cert material is only in Object Store, it needs either backup or machine-local cert evidence for reindex. |
| Deploy bundles, diagnostics, rendered specs | Mentioned as architectural Object Store use cases, but not implemented as current JetStream storage. | Future buckets need an ADR-0001 classification and explicit reindex/backup story. |

## Backup And Restore Interaction

The canonical product backup scope names `KV_CORE`, `KV_OPS`, `KV_OBS`, lock
state, backup manifests, NATS credentials/config, Ployz domain config, and
operation event streams as included control-plane concerns. The current backup
bundle is narrower: runtime code snapshots only `KV_CORE`, restore validates
only `KV_CORE`, and restore reports observations as rebuildable after machine
reconnect.

That means today's backup/restore path preserves current `KV_CORE` records but
does not preserve operation memory, observation cache, or operation event
history. That matches the disposable/rebuildable direction for many records,
but it leaves two audit gaps:

- `KV_OPS` contains secret-bearing machine-add records; if it is not backed up,
  pending joins and exact operation evidence are intentionally lost after
  JetStream loss.
- Current bundle scope still lists `LockStateKv` as included even though no
  `KV_LOCKS` bucket exists in the active bootstrap manifest.

## Reindex Outline

An explicit reindex after JetStream loss should:

1. Recreate the bootstrap resources: `KV_CORE`, `KV_OPS`, `KV_OBS`, and
   `PLZ_OPS`.
2. Adopt NATS authorized users from `authorized-users.conf` into
   `KV_CORE.nats_authorized_user.*` before rendering authorization.
3. Wait for machines and role processes to reconnect and publish fresh
   observations.
4. Rebuild `KV_OBS` naturally from machine and gateway observation loops.
5. Rebuild `KV_CORE.machines.*`, `KV_CORE.services.*`, and `KV_CORE.routes.*`
   only from unambiguous machine-local facts and Docker reality.
6. Leave ambiguous or missing service, route, cert, and machine facts as
   diagnostic observations until a named repair operation resolves them.
7. Do not reconstruct `KV_OPS` machine-add handoff secrets, mint claims, status
   records, or `PLZ_OPS` event timelines. Record the reindex operation's
   own evidence about what was adopted, skipped, or ambiguous.

## Source Pointers

- Resource manifest: `crates/ployz-nats/src/bootstrap.rs`.
- Resource creation shape: `crates/ployz-nats/src/bootstrap/assurance.rs`.
- Key names and state models: `crates/ployz-core/src/state.rs`.
- Operation stream subject names: `crates/ployz-core/src/subjects.rs`.
- `KV_CORE` adapters: `crates/ployz-nats/src/core_state/`.
- `KV_OBS` adapter: `crates/ployz-nats/src/observations.rs`.
- `KV_OPS` keys and records: `crates/ployz-nats/src/operations/keys.rs` and
  `crates/ployz-nats/src/operations/status_store.rs`.
- `PLZ_OPS` event writes/replay: `crates/ployz-nats/src/operations/events.rs`
  and `crates/ployz-nats/src/operations/repository.rs`.
- NATS authorization recovery evidence:
  `crates/ployzd/src/nats_authorization.rs` and
  `crates/ployzd/src/nats_authorization/writer.rs`.
- Backup/restore scope:
  `crates/ployzd/src/backup_runtime.rs`,
  `crates/ployzd/src/backup_restore.rs`, and
  `crates/ployz-core/src/backup.rs`.
