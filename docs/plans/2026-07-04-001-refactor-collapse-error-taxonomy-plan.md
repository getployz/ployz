# Collapse the write-only operation-API error taxonomy

## Context

A cold architecture review found that the operation API's unavailability
taxonomy is write-only: ~17 `*UnavailableSource` / `*Failure` enums are
exported through `ployz-sdk-types` and `generated.ts`, and no client —
ployzctl, the TS SDK, or Cloud — matches a single variant. The enums are
`Copy` and carry no evidence, so they are also useless for debugging:
`open_bucket` does not say which bucket, `timeout` does not say against
what. Every internal storage change currently ripples through four layers
plus generated TypeScript.

Grilled decisions:

- Clean wire break; no compat window. Cloud adopts the new shape at its
  next SDK pin.
- `Unavailable { message: String }` with no subsystem enum. The message is
  evidence for a human or AI reading it, not state anything dispatches on.
- A variant stays typed only if it carries data the client renders (ids,
  domain failure payloads) or changes what the user does next.
  `OperationCorrupt` folds into the message (`"operation record corrupt:
  ..."`); it comes back as a type only when corruption handling becomes a
  real workflow.
- Internal forwarded-only mirror enums die too; errors are typed once
  where born, rendered once where they leave the cluster.
- Rendering bar (AI-debuggability): the boundary render preserves the full
  causal chain — bucket, key, subject, timeout duration, underlying NATS
  error text. No truncation at the top of the chain.

## Changes

### 1. `crates/ployz-sdk-types/src/lib.rs` — delete the plumbing enums

Delete: `RuntimeSnapshotUnavailableSource`, `LogsTailUnavailableSource`,
`ServiceQueryUnavailableSource`, `MachineQueryUnavailableSource`,
`MachineJoinReportUnavailableSource`, `MachineJoinRedeemUnavailableSource`,
`MachineUpdateUnavailableSource`, `MachineAddUnavailableSource`,
`OperationSubmitUnavailableSource`, `OperationSubmitStatusFailure`,
`OperationSubmitEventFailure`, `OperationSubmitClockFailure`,
`OpsStatusUnavailableSource`, `OpsWatchUnavailableSource`,
`StatusReadFailure`, `EventReplayFailure`.

Every endpoint envelope's `Unavailable` variant becomes
`Unavailable { message: String }`, keeping any ids it already carries
(e.g. `OpsStatusError::Unavailable { operation_id, message }`).

Stays typed, untouched: `NoSuch*`, `ResourceBusy`/`NamespaceBusy`,
`InvalidTarget`, `DuplicateSequenceMismatch`, `MaterialNotReady`, the
domain payloads (`MachineJoinReportFailure`, `CloudBootstrap*`, deploy
operation failures), and the envelope enums themselves (serde-tagged
decode needs them). `BootstrapMaterialFailure` also dies: nothing
matched on it, it only rode inside the deleted
`MachineAddUnavailableSource`.

### 2. Evidence-preserving `Display` in the owning crates

The internal operation error enums mostly lack `Display`. Implement it
where the error is defined (`ployz-nats`: `OperationStatusStoreError`,
`OperationEventLogError`, `OperationEventReplayReadError`,
`CoreStateStoreError`; `ployzd::controllers` errors), rendering every
payload field the enum already captures and chaining inner errors
(`"status store: put status op_123: nats: timeout after 5s"`). These
impls serve logs as well as the API boundary.

### 3. `crates/ployzd/src/operation_api/error_map.rs` — mapping becomes rendering

The categorization functions (`operation_submit_status_failure`,
`operation_submit_event_failure`, `event_replay_failure`,
`record_*_unavailable_source`, ...) collapse into `to_string()` calls on
the source errors at each envelope construction site. Actionable mappings
(state conflicts, busy, no-such, material-not-ready) stay as they are.
Expected size: from 647 lines to well under half.

### 4. Internal mirror collapse (survival test applied)

- Survives (matched on): `OperationStatusStoreError` —
  `repository/submission.rs` branches on `RecordExists` and `Timeout`.
- Survives (real behavior): `SubmitCommandError` — adds `NamespaceBusy`
  around the submit path.
- Dies if forwarded-only: `MachineAddSubmitCommandError` and any
  intermediate whose arms all reduce to re-tagging — carry the source
  error directly instead. Apply the same test to each enum touched while
  shrinking `error_map.rs`; delete only what nothing matches on.

### 5. TypeScript surface

Regenerate `generated.ts` (deleted enums disappear, `Unavailable` shapes
gain `message`). Update `packages/ployz-sdk/src/index.ts` /
`primitives.ts` helpers only if they name a deleted type. SDK typecheck
is the gate.

### 6. ployzctl

No variant matching exists; update any `Debug`-formatted error output to
print the `message` field so the evidence chain reaches the terminal.

## Not in scope

- Keeper/bootstrap domain failure enums and typed operation failure
  details on durable records — they are the Operation Rules' audience.
- Retry semantics or a retryable flag: nothing branches today; adding one
  is a deliberate future decision.
- The other three review candidates (repository seam, runtime projection
  extraction, lock-guard Option).

## Verification

1. Grep gate: `UnavailableSource` and the deleted enum names appear
   nowhere in the workspace or `generated.ts`.
2. `cargo test --workspace --exclude ployz-e2e --exclude ployz-keeper` —
   endpoint envelope tests updated to the new `Unavailable { message }`
   wire shape, with at least one pin asserting a rendered message
   preserves the inner chain (bucket/key + source text).
3. `npm run typecheck` in `packages/ployz-sdk` after regeneration.
4. `wc -l error_map.rs` shows the shrink; zero warnings workspace-wide.
