# T14 — Own and bound pending Relay tunnel lifetime

Status: in_progress
Blocking dependencies: T13
Audit scope: F10

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-relay/src/lib.rs; ployz-relay/src/serve.rs; ployzd/src/relay.rs

## Work

Give Dial an ID-specific pending cleanup guard through upgrade/WebSocket lifetime; Attach consumes the pending map entry, not Dial guard ownership. Add a bounded Attach deadline for a connected Dial with missing Attach. Preserve existing register turnover/shutdown cleanup and generation checks; after Attach, Dial cleanup becomes a no-op for pending state.

## Acceptance

Failed/missing Attach with surviving Register cannot retain pending entries indefinitely. Abandoned Dial cleans pending state including cancelled upgrade. Successful Attach then Dial drop does not delete an active/replacement tunnel. Bound timers/tasks themselves.

## Verification

Rung 1 Relay public start_dial/start_attach/hold seam with deterministic paused time/cancellation; no sleep-based flakiness or mere private map assertions.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T14.md`; coordinator owns index/status updates and final four-axis review.
