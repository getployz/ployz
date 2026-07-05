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

## Stroke 1 — the facts plane

One `MachineFactsSnapshot` per machine: containers with identity labels,
public IP, roles, applied lifecycle, cert refs, `observed_at`. Machine
ployzd publishes it on change (Docker events, debounced) and every 30s,
and answers `facts.get` (H2). Machine credentials permit publishing only
the machine's own facts subject. The core keeps a live fact cache.

Delete in the same stroke: `KV_OBS`, `observations.rs` as a store, and
every observation-scatter key. Flip every reader — `gateway_source`,
`dns_source`, operation-API queries, deploy-worker runtime snapshots —
to the cache/broadcast/gather path with disk-backed last-known-good.

## Stroke 2 — the intent plane and the serving commit

Core-owned evidence files: roster (identity, name, subnet), lifecycle,
route bindings, serving promotions, authorized users. Written only by
operations through the core sequencer (tmp+rename, single process, so the
serving commit stays atomic — H3). Core serves `intent.get`, broadcasts
`intent.changed`, rebroadcasts full intent on the drumbeat (H1); readers
re-list on reconnect (H5). Gateways/DNS fold `intent × facts`.

Delete in the same stroke: all of `core_state/` (serving targets, route
bindings, machine records, KV auth projection), the state-key machinery in
`ployz-core`, `NamespaceStateCommitter` and its adapters, and the KV
commit halves of the deploy and lifecycle runtimes.

## Stroke 3 — operations on evidence logs

The sequencer writes one append-only evidence file per operation; live
progress on plain subjects; `ops watch` replays the file then tails;
`ops list/status` read the log and mark unanswering parties (H4). The
namespace fence becomes the sequencer's in-process mutex; terminal dedup
becomes writing the terminal record once. Machines record their own steps
in their fact ledgers for deep-inspect corroboration.

Delete in the same stroke: `operations/repository*`, `status_store`, the
event-stream append path, `namespace_lock.rs`, and the operation-event
message-id dedup contract (its wire pins go with it — there is no
persisted stream left to protect).

## Stroke 4 — placement bids and machine-side drain

Placement flips from eligibility scan to live bid RPCs; draining machines
decline (the planner consults intent as well). The ADR 0027 end-state
lands.

## Stroke 5 — JetStream off

`jetstream: disabled`; delete `kv.rs`, `streams.rs`, `schedules.rs`,
bootstrap assurance, and the JetStream test-support bootstrap (retiring
the contention flake class). Retire "Reindex" from CONTEXT.md; recovery is
documented as: adopt evidence files, wait one broadcast tick.

## Notes

- Strokes are dependency order, not compatibility order. 1 and 2 can be
  built in parallel branches; 3 depends on 2 (the sequencer's home); 5 is
  mechanical once 1–3 land.
- Tests are rewritten with their subsystems, not preserved: the JetStream
  fixtures die with the stores they exercised. Wire pins survive only for
  contracts that still exist (machine RPC protocol, SDK API shapes, fact
  snapshot and intent file schemas — pin those fresh).
- ADR 0019 (core promotion) is unchanged; after Stroke 5 promotion
  recovers intent files only, and the generation fence prevents a healed
  old core from writing intent.
