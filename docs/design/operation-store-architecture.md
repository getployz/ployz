# Operation Store — Long-Term Architecture (handoff)

Target shape for the operation subsystem now that it's on SQLite. Written for
whoever lands the `operation_log.rs` decomposition. The file is 2k lines
because it holds five jobs; more operation kinds ("actions") are coming
(cert renewal is already partial). This is the durable shape that makes each
new action O(one module), keeps projection in Rust and uniqueness in the DB,
and never becomes a generic operation engine.

## The model: one envelope, pluggable actions

Operations are **uniform in the envelope** (submit → durable events → one
terminal result) and **divergent in the middle** (machine-add: join tokens +
secret escrow + mint; deploy: plan + container start + route cutover + serving
commit; cert: ACME challenges). ~90% of the code is the divergent middle, and
it does not generalize. So:

- The **envelope** is shared and thin.
- Each **action** owns its divergent middle in its own module.
- They meet at **one trait**, not an engine (AGENTS.md: *avoid generic
  operation engines*). The trait carries the envelope contract; special cases
  stay private to each action module.

### Three layers

**Layer 1 — Operation envelope (kind-agnostic, shared).**
`OperationStatus` / `OperationEvent` (ployz-core), `record_operation_event`,
replay, the terminal contract. Owns *projection* (Rust). Stays small.

**Layer 2 — Actions (one module per kind).**
`deploy`, `machine_add`, `machine_update`, `machine_lifecycle`, `cert`, … Each
owns: submission/acceptance + idempotency, its kind-specific working records,
its transitions, its status read. Plugs into Layer 1 via the `OperationAction`
trait. New action = new module + one impl; adding `cert` never touches
`deploy`.

**Layer 3 — Store (one row abstraction on `CoreStore`, DB-enforced invariants).**
`CoreStore` owns the keyed-JSON row primitives; every action's storage and the
namespace/roster stores compose them ("one projection owner, storage-blind
callers"). The **database enforces uniqueness**; Rust never hand-checks it.

## The `OperationAction` trait (grow `SubmitKind` into this)

```rust
trait OperationAction: Sized {
    type Payload: Clone + Send + 'static;
    const KIND: OperationKind;

    // envelope
    fn submitted_event(id: OperationId, payload: Self::Payload) -> OperationEvent;
    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)>;
    fn accepted_status(id: OperationId, payload: &Self::Payload, seq: EventSequence)
        -> OperationStatus;
}
```

Deploy / machine-update / machine-lifecycle are pure envelope impls (they already
are — that's `SubmitKind` today). Machine-add's extra machinery (claims, join
tokens, secret escrow, mint) stays **private to `machine_add.rs`**, not hoisted
into the trait. The trait is the thin shared contract; it does not try to model
escrow or transitions for actions that don't have them.

## Storage model

- `operations(operation_id TEXT PK, status_json TEXT)` — projection blob.
  **Do not** decompose `OperationStatus` into columns; it's a rich enum and Rust
  owns projection. Blob is correct here.
- `operation_events(operation_id, sequence, event_json, subject TEXT NULL,
  PRIMARY KEY(operation_id, sequence))` — append-only.
  Add a **partial `UNIQUE` index** on `(operation_id, subject)` where
  `subject IS NOT NULL`, populated for singleton deploy evidence
  (plan/dataplane/health/cleanup; per-container starts leave `subject` NULL).
  "Recorded once per deploy" becomes a schema fact. On the insert's constraint
  violation, map to `AlreadySatisfied`.
- Per-action typed working-record tables with real columns + `UNIQUE`
  constraints (`machine_add_*` with `UNIQUE(operation_id)` — already in flight).
  Keep going; this is *encode-invariants-in-types* pushed to storage.

## Module layout

```
operation_log/
  mod.rs          OperationRepository facade — dispatch + public API (thin)
  operation.rs    envelope: record_operation_event(_txn), replay, status store   (L1)
  action.rs       the OperationAction trait                                       (seam)
  deploy.rs       deploy claim/submit + deploy evidence
  machine_add.rs  the fat one — claims, submissions, join tokens, secrets, mint, redeem
  machine_update.rs
  machine_lifecycle.rs
  cert.rs         (when it lands)
core_store.rs     row store: query_json / query_json_list / get / upsert / create_or_adopt
```

Machine-add stays the honestly-fat file; sub-split (`machine_add/{claims,redeem,
mint}.rs`) when it next grows.

## Execution order (each step earns the next — do NOT reorder)

1. **`RecordOperationEventOutcome::Stored` carries the projected `OperationStatus`.**
   The txn already computes the new status and throws it away, forcing
   `redeem_machine_join_token` to `get()` a second time and re-destructure
   `Joining`. Carry it out. The redeem double-read and re-destructures vanish;
   every `record_*` caller that re-reads can stop; the facade goes thin.
   **This is the keystone — without it the split is just relocation.**

2. **Promote the single-row helpers to `CoreStore`** (`get` / `upsert` /
   `create_or_adopt`) beside the existing `query_json` / `query_json_list`.
   Replace the hand-rolled `prepare + query_map + from_json` loops in
   `select_all_statuses` / `select_machine_add_submissions` with `query_json_list`.
   operation/namespace/roster all compose one row store.

3. **DB-enforce singleton deploy evidence.** Add the `subject` column + partial
   `UNIQUE` index; catch the conflict → `AlreadySatisfied`. Delete the per-write
   event scan (`singleton_deploy_evidence_recorded`) and the
   `SingletonDeployEvidence` mirror enum — the constraint is the source of truth.

4. **Extract `OperationAction` + split into per-action modules.** Now it's
   cohesion, not line-shuffling, because L1 and L3 are already clean. Fold the
   cutlist in as code moves.

### Fold in during step 4 (reviewer cutlist)
- Drop dead error variants matched in `error_map` but never constructed:
  `RedeemMachineJoinTokenError::JoinTokenMismatch`,
  `RecordMachineJoinReportError::JoinTokenMismatch`,
  `SubmitOperationError::InvalidDeployTarget`.
- Collapse the four `Record*Error` aliases into `RecordOperationEventError`.
- Inline the generic `{table,key}` helpers (`select_json`/`insert_json`/generic
  `create_or_adopt`) into a `deploy_claims`-specific helper — `deploy_claims` is
  their only remaining caller after the typed machine-add tables. Keep
  `AdoptResult` (machine-add uses it).
- Make `release_namespace` `pub`; drop the `release_deploy_namespace` pass-through.
- `finish_replay_page` never `Err`s — return `ReplayTxn` directly.

## Guardrails (what NOT to do)

- **No generic operation engine.** Trait + real impls only; divergent logic
  private to each action module.
- **Do not blob-bust `operations` / `operation_events`.** Status/events stay
  JSON; Rust owns projection. Only the *keyed working records* go typed-column.
- **Don't hoist machine-add's escrow/mint into the trait.** The trait is the
  envelope; escrow is machine-add's private business.
- Steps 1–3 change the txn/outcome contract and the schema. Land as **one
  focused branch**, not interleaved with unrelated WIP.

## Net

Less code than today; uniqueness invariants in the schema; projection in Rust;
one row store; a thin facade; and every future action is one module against a
thin envelope contract.
