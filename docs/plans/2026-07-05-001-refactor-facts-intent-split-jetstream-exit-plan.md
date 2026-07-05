# Facts/Intent Split and JetStream Exit — Rewrite Plan

Implements ADR 0028 (machines broadcast facts; the core owns intent) and
ADR 0029 (JetStream exits). There are no live clusters and no persisted
data to honor: this is a rewrite, not a migration. No dual-writes, no
compatibility windows, no wire-format preservation — each stroke deletes
the old subsystem and lands its replacement in the same PR. The only gate
is that the workspace compiles and its tests pass at every merge.

The stress-test arena produced five hardening requirements (H#). They are
requirements of the design, not of the transition:

- H1: intent is rebroadcast on the same periodic drumbeat as facts —
  change-only broadcast over at-most-once transport is forbidden.
- H2: a facts request subject exists so fresh readers pull instead of
  waiting a tick.
- H3: serving eligibility stays an operation-written commit; the gateway
  fold never serves from facts alone.
- H4: `ops` gather queries name unanswering machines; partial never
  renders as complete.
- H5: readers re-list intent on every reconnect.

## Preflight

Before Stroke 1 starts, `cargo test --workspace` must be green on the branch
without JetStream-exit code changes. If it is red, fix or explicitly quarantine
the baseline failure first; otherwise the per-merge gate cannot distinguish
rewrite regressions from inherited failures.

## Stroke 1 — the facts plane

One `MachineFactsSnapshot` per machine: containers with identity labels,
public IP, roles, applied lifecycle, applied substrate versions, cert
refs, `observed_at`. Cert refs are defined in their post-Stroke-5 form
(artifact digest plus machine-local path, never an Object Store
reference) so the pinned schema survives Stroke 5. Machine ployzd
publishes it on change (Docker events, debounced) and every 30s, and
answers `facts.get` (H2). Machine credentials permit publishing only
the machine's own facts subject. The core keeps a live fact cache. Passive
read and data-plane surfaces may use cached or disk-backed last-known-good
facts marked with `observed_at`; mutating operation runtime snapshots must
use fresh gather/live RPC and treat non-answering machines as unknown
operation evidence, never as reuse, cleanup, or placement input.

Delete in the same stroke: `KV_OBS`, `observations.rs` as a store, and
every observation-scatter key. Flip every reader — `gateway_source`,
`dns_source`, operation-API queries — to the cache/broadcast/gather path
with disk-backed last-known-good (each role process owns and prunes its
own LKG file under its own state directory). The SDK projection-source
types lose their observation-store vocabulary here (`ployz-sdk-types`
rework + TS regeneration ride this stroke). Permission changes in this
stroke are limited to the machine facts-subject narrowing; the full
profile rework belongs to Stroke 2.

## Stroke 2 — the intent plane and the serving commit

Core-owned evidence files: roster (identity, name, subnet), lifecycle,
route bindings, serving promotions, authorized users. Written only by
operations through the core sequencer (tmp+rename, single process, so the
serving commit stays atomic — H3). Core serves `intent.get`, broadcasts
`intent.changed`, rebroadcasts full intent on the drumbeat (H1); readers
re-list on reconnect (H5). Gateways/DNS fold `intent × facts`.

The sequencer also owns NATS authority updates: mint/revoke credentials,
update subject permissions, persist evidence, and fail the operation if the
authority update cannot be applied. Mint claims and the identity/subnet
never-reuse record live in sequencer-written evidence files beside the
roster. Facts/intent permissions are deny by default: machines publish and
answer only their own facts, role processes read the facts/intent their
role needs (the gateway and DNS role grants legitimately include the full
facts broadcast — their fold spans every machine), operators read through
authorized core services, and any grant a role does not need is denied.
Authorized users are core-local intent consumed only by the core's own
authorization render: they are never part of the intent broadcast or
`intent.get` payload.

Deploy planning re-points in this stroke: the eligibility scan and
serving/route inputs (`deploy_worker/facts.rs`, `deploy_worker/ports.rs`)
read `intent.get` plus facts gather as the interim source, replaced by
bid RPCs in Stroke 4.

Delete in the same stroke: all of `core_state/` except
`namespace_lock.rs` (the deploy fence survives until Stroke 3 replaces it
with the sequencer's in-process mutex), the state-key machinery in
`ployz-core`, `NamespaceStateCommitter` and its adapters, and the KV
commit halves of the deploy, lifecycle, and machine-update runtimes
(reported substrate versions move to the facts snapshot). The SDK
projection-source types lose their core-state vocabulary here (TS
regeneration rides this stroke too).

## Stroke 3 — operations on evidence logs

The sequencer writes one append-only evidence file per operation; live
progress on plain subjects publish-fenced to the sequencer credential
(machines and users are subscribe-only, so watched progress cannot be
spoofed by another principal); `ops watch` replays the file then tails;
`ops list/status` read the log and mark unanswering parties (H4). The
namespace fence becomes the sequencer's in-process mutex; terminal dedup
becomes writing the terminal record once. Machines record their own steps
in their fact ledgers for deep-inspect corroboration. Evidence files live in
ployzd-owned `0700` directories as `0600` files, are written atomically with
fsync, exclude secret values, redact credentials/tokens/cert material, and
are removed by retention deletion for both live and archived evidence.

Delete in the same stroke: `operations/repository*`, `status_store`, the
event-stream append path, `namespace_lock.rs`, and the operation-event
message-id dedup contract (its wire pins go with it — there is no
persisted stream left to protect).

## Stroke 4 — placement bids and machine-side drain

Placement flips from the interim intent-scan (Stroke 2) to live bid
RPCs; draining machines decline (the planner consults intent as well).
The ADR 0027 end-state lands here.

Delete in the same stroke: the interim eligibility scan and its
intent-derived machine scope.

## Stroke 5 — JetStream off

Before `jetstream: disabled`, replace each former durable trigger as one of:
recreated from intent, failed into a visible operation, or deliberately
operator-owned. Cert renewal is a core-owned local timer that creates explicit
operations and records missed or failed renewal work. Object Store usage moves
to RPC artifact push; every pushed artifact carries expected digest, size,
type, and operation id, and machines verify before writing or using it.

Then set `jetstream: disabled`; delete `kv.rs`, `streams.rs`, `schedules.rs`,
bootstrap assurance, and the JetStream test-support bootstrap (retiring the
contention flake class). Replace the "Reindex" glossary with "Core Assurance";
recovery is documented as: adopt preserved or restored intent evidence files,
wait one broadcast tick. If core evidence is lost, operator intent is lost
unless an external or operator-held restore source exists.

## Verification

- H1: test full intent rebroadcast on the periodic drumbeat.
- H2: test `facts.get` serves a fresh reader without waiting for a tick.
- H3: test gateways refuse to serve retained containers from facts alone.
- H4: test `ops` names every unanswering machine instead of rendering partial
  results as complete.
- H5: test readers re-list intent on reconnect.
- Pin fresh schemas for fact snapshots and intent files, alongside surviving
  machine RPC and SDK wire contracts.

## Notes

- Strokes are dependency order, not compatibility order. 1 and 2 can be
  built in parallel branches; 3 depends on 2 (the sequencer's home); 4
  depends on 2 (it replaces the interim scan); 5 is mechanical once 1–3
  land.
- Tests are rewritten with their subsystems, not preserved: the JetStream
  fixtures die with the stores they exercised. Wire pins survive only for
  contracts that still exist (machine RPC protocol, SDK API shapes, fact
  snapshot and intent file schemas — pin those fresh).
- ADR 0019 (core promotion) is unchanged; after Stroke 5 promotion
  recovers intent files only, and the generation fence prevents a healed
  old core from writing intent.
