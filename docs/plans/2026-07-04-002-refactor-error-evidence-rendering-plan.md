# Errors render themselves: finish the evidence-rendering move

## Context

Two cold reviews (thermo-nuclear + ponytail) of the taxonomy collapse
converged on one root cause: rendering is scattered across boundary
helper functions and hand-written `Display` impls instead of being owned
by the error types. Every individual finding — the truncating
`MachineSnapshotError`, the `record_operation_event_message` helper, the
duplicated corrupt-record strings, the parallel kind tables, the Debug
soup in ployzctl and in a durable `FailureMessage` — is an instance of
that one gap.

## The long-term rule

An error renders itself, once, where it is defined:

- An enum that is ever rendered **whole** derives `thiserror::Error`
  with evidence-preserving `#[error]` texts. Hand-written `Display`
  impls and boundary-side renderer functions for such enums are the
  anti-pattern this plan deletes.
- An enum that is **routed** (matched arm-by-arm to different API
  outcomes, like `SubmitCommandError`) gets an exhaustive match, and
  only its rendered *sources* need `Display`. Routed enums do not get
  whole-enum `Display` — that would resurrect the unreachable-stub-text
  smell.
- `{:?}` never reaches a user-facing or durable surface. Debug is for
  developers inside logs, never for `FailureMessage` or CLI output.

## Changes

### 1. Swap the five hand-written Display impls for thiserror derives

`OperationStatusStoreError`, `OperationStatusReadError` (status_store.rs),
`OperationEventLogError`, `OperationEventReplayReadError` (events.rs),
`StatusProjectionError` (projection.rs). thiserror is already a workspace
dep and already the pattern in ployz-nats (`bootstrap.rs`, `connect.rs`,
`schedules.rs`). The `#[error]` texts must be byte-identical to today's
rendered strings — the exact-string tests in `error_map.rs` and
`schedules.rs` are the pin. ~160 lines → ~40.

### 2. `RecordOperationEventError` renders itself in ployz-nats

Derive `thiserror::Error` on it (repository.rs), carrying the corrupt-
record texts that `record_operation_event_message` hand-rolls today.
Then:

- `error_map.rs` deletes the helper; both call sites become
  `source.to_string()`.
- `crates/ployzd/src/machine_update_runtime.rs:206` stops rendering the
  same enum with `{error:?}` into a durable `FailureMessage` — the
  durable operation record (the Operation Rules' audience) gets the same
  evidence quality as the transient API path. This is the one deliberate
  evidence-text change; everything else is byte-identical.

### 3. `MachineAddBootstrapMaterialError` renders itself

Whole-rendered enum in `controllers.rs`: thiserror derive with today's
`bootstrap_material_message` texts; delete that helper. The three inline
`format!("clock: {message}")` sites on *routed* enums stay inline —
three one-liners are below the abstraction bar, and per the rule routed
enums get no whole-enum Display.

### 4. Delete `MachineSnapshotError`

queries.rs still truncates at three `map_err(|_| …::Observations)` sites
and renders the fixed string "machine observations unavailable". The
enum is forwarded-only (nothing matches on it). Carry
`error.to_string()` like every sibling site the collapse already fixed;
`machine_inspect_error` reduces accordingly.

### 5. queries.rs closure cleanup

Inline the four now-identical `*_core_error` helpers
(`|e| X::Unavailable { message: e.to_string() }`) at their call sites;
delete `runtime_machine_error` / `runtime_service_error`, which re-tag
`Unavailable { message }` into an identically shaped variant.

### 6. One kind-name table

`ProjectionOperationState` gets `kind() -> OperationKind` (the existing
accessor pattern, see `OperationStatus::kind`); `kind_name` dies;
`operation_kind_name` becomes the single string table, used by the
`StatusProjectionError` derive texts and anything else that names kinds.

### 7. Corrupt-record sentinel in one place

After change 2, most `"operation record corrupt: …"` strings live in the
`RecordOperationEventError` derive. The remaining hand-typed sites in
`error_map.rs` and `machine_join.rs` route through one
`pub(super) fn corrupt(detail: impl Display) -> String` in `error_map.rs`
so the greppable sentinel prefix has exactly one definition.

### 8. The last mile: ployzctl prints evidence, not Debug

`api_error` (ployzctl runtime.rs) bounds `E: fmt::Debug`, so operators
and driving AIs read `Unavailable { operation_id: OperationId("op_…"),
message: "…" }`. Fix at the type layer, not the CLI: derive
`thiserror::Error` on the endpoint error envelopes in `ployz-sdk-types`
(message-first texts, ids included), and on the small domain payload
enums they embed where needed for the chain. `api_error` re-bounds to
`E: fmt::Display`. Acceptance: no `{:?}`-shaped braces in any API error
the CLI prints. This is the largest chunk; it is also what makes the
whole export surface self-rendering for every future Rust consumer.

### 9. Plan-doc correction

`docs/plans/2026-07-04-001-…` lists `BootstrapMaterialFailure` under
"stays typed"; the implementation (correctly) deleted it. Fix the line
so the plan doesn't misdocument the shipped wire break.

## Not in scope

- A typed clock error shared across the three `Clock { message }`
  variants — three inline one-liners don't justify a type.
- Retryable flags or any wire-shape change: serde output is untouched
  everywhere; `generated.ts` must not change (Display derives don't
  touch serialization).
- The other architecture-review candidates.

## Verification

1. Exact-string tests in `error_map.rs` pass unchanged — proves the
   thiserror texts are byte-identical for changes 1–3.
2. Full suite (`--exclude ployz-e2e --exclude ployz-keeper`) green;
   zero workspace warnings.
3. Grep gates: no `impl fmt::Display` remains on the enums from changes
   1–3; no `:?` inside `operation_api/`, `machine_update_runtime.rs`
   error paths, or `api_error`; `"operation record corrupt: "` literal
   appears in exactly two source files (the derive + the helper).
4. `git diff` on `packages/ployz-sdk/generated.ts` is empty.
5. One new unit test on ployzctl's `api_error` pinning a clean rendered
   `Unavailable` message (the last-mile acceptance).
