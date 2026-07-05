# Re-slice `ployz-core::ops` by operation kind

## Why

The ops module is sliced by mechanism (states / events / classification /
projection files), so one operation kind's model spans six files and
`machine.rs`. Adding the simplest kind (machine lifecycle) touched 61 files;
the bounce inside `ployz-core::ops` is the accidental part of that cost. The
two newest kinds already grew per-kind projection submodules — this finishes
that drift deliberately.

## Shape

One module per kind, each owning its states, failures, transitions, event
grouping, and projection:

- `ops/deploy.rs`, `ops/cert.rs`, `ops/machine_add.rs`,
  `ops/machine_update.rs`, `ops/machine_lifecycle.rs`
- `ops.rs` stays the spine: `OperationKind`, `OperationStatus` (+ctors),
  `OperationStatusSnapshot`, `OperationIdempotencyKey`, `EventSequence`,
  and `pub use` of every per-kind item at its existing public path.
- `ops/events.rs` keeps the flat `OperationEvent` (persisted wire contract:
  serde shape, subjects, message ids are all pinned) and gains the
  flat-event → per-kind-event classification match — the file's job is
  already "every match that must enumerate all event variants".
  `classification.rs` is deleted as a file.
- `ops/projection.rs` shrinks to the spine: projection error types,
  `ProjectionOperationState`, and the `project_operation_event` dispatcher.
  `projection/machine_update.rs` and `projection/machine_lifecycle.rs`
  fold into their kind files.

## Decisions (grilled)

- Classification lives beside `OperationEvent` in `events.rs` — routing is
  part of an event's identity; the match must exist exactly once.
- `MachineAddOperationState`/`MachineAddOperationStateName` move to
  `ops/machine_add.rs` with **no re-export shim** in `machine.rs`; all
  importers rewrite to the canonical `ops::` path. `MachineAddFailure`
  stays in `machine.rs` (machine-domain evidence used by the join flow).
- `tests/operation_projection.rs` splits per kind in the same pass.

## Invariants

- Every `ployz_core::ops::X` public path is unchanged (re-exports).
- Wire pins (`tests/subjects.rs`, `tests/wire_contract.rs`) pass untouched.
- No serde shape changes anywhere.
