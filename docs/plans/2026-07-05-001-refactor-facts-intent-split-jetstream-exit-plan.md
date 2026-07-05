# Facts/Intent Split and JetStream Exit — Migration Plan

Implements ADR 0028 (machines broadcast facts; the core owns intent) and
ADR 0029 (JetStream exits). Every phase ships green on its own; JetStream
degrades from truth → mirror → deleted, which ADR 0001's rebuildable-index
framing already licenses. No data migration ever runs: the destination
state is, by construction, whatever machines say next plus what the core's
evidence files already hold.

Design decisions were stress-tested by adversarial review; the hardening
items it produced are marked (H#) and are requirements, not suggestions:

- H1: intent is rebroadcast on the same periodic drumbeat as facts —
  change-only broadcast over at-most-once transport is forbidden.
- H2: a facts request subject exists so fresh readers pull instead of
  waiting a tick.
- H3: serving eligibility stays an operation-written commit; the gateway
  fold never serves from facts alone.
- H4: `ops` gather queries name unanswering machines; partial never
  renders as complete.
- H5: readers re-list intent on every reconnect (ADR 0005).

## Phase 1 — the fact snapshot and broadcast (pure addition)

- One `MachineFactsSnapshot` per machine: containers with identity labels,
  public IP, roles, applied lifecycle, cert refs, `observed_at`. Machine
  ployzd publishes it on change (Docker events, debounced) and every 30s
  (`OBSERVATION_PUBLISH_INTERVAL`), and answers `facts.get` (H2).
- Machine credentials permit publishing only the machine's own facts
  subject.
- Existing KV observation writes continue in parallel; nothing reads the
  broadcasts yet.

## Phase 2 — readers flip to facts

- `gateway_source.rs`, `dns_source.rs`, and the operation-API query
  runtimes read the core's live fact cache / broadcasts with disk-backed
  last-known-good, instead of `KV_OBS`.
- The deploy worker's runtime snapshots come from `facts.get` gathers.
- `KV_OBS` writes go dark; `observations.rs` shrinks to the broadcast
  client.

## Phase 3 — intent files and the serving commit

- Core-owned evidence files gain `route-bindings.json` and
  `serving-promotions.json` beside the existing authorized-users and
  machine-lifecycles files; machine roster (identity, name, subnet) moves
  from KV machine records into the roster file written by join/add/remove
  operations.
- The deploy worker commits route bindings and serving promotions to
  intent files (single-process atomic write, tmp+rename) instead of KV
  CAS; `NamespaceStateCommitter` and its lock-checked adapter are deleted.
- The core serves `intent.get`, broadcasts `intent.changed`, and
  rebroadcasts full intent on the drumbeat (H1). Gateways/DNS fold
  `intent × facts`, re-listing on reconnect (H5); serving requires a
  promotion record (H3).
- Delete `core_state/serving_target_entry.rs`, `core_state/route_binding.rs`,
  `core_state/active_machine.rs` and their state-key machinery.

## Phase 4 — operations move to the evidence log

- The sequencer writes one append-only evidence file per operation; live
  progress publishes on plain subjects; `ops watch` replays the file then
  tails; `ops list/status` read the log and mark unanswering parties (H4).
- The namespace lock becomes the sequencer's in-process mutex; terminal
  dedup becomes the sequencer writing its terminal record once.
- Delete `operations/repository*`, `operations/status_store`,
  `core_state/namespace_lock.rs`, and the event-stream append path.

## Phase 5 — placement bids and machine-side drain decline

- Placement flips from the eligibility scan to live bid RPCs; draining
  machines decline (intent consulted by the planner as well); the ADR 0027
  end-state lands.

## Phase 6 — JetStream off

- `jetstream: disabled` in the NATS process config; delete `kv.rs`,
  `streams.rs`, `schedules.rs`, bootstrap assurance, and the JetStream
  test-support bootstrap (retiring the contention flake class).
- Retire "Reindex" from CONTEXT.md; recovery is documented as: adopt
  evidence files, wait one broadcast tick.

## Sequencing notes

- Phases 1–2 and 3 are independent workstreams after Phase 1 lands.
- The wire contract for the SDK/CLI is unchanged in shape through Phase 4;
  gather-backed queries add `observed_at` / unanswered-machine fields only.
- The point of maximum dual-run is Phases 2–3 (KV written but unread);
  both phases delete their dual-write in the same PR that flips readers,
  so no long-lived mirror state exists.
- ADR 0019 (core promotion) is unchanged; after Phase 6 promotion recovers
  intent files only, and the generation fence prevents a healed old core
  from writing intent.
