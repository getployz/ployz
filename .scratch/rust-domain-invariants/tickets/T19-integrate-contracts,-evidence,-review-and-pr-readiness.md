# T19 — Integrate contracts, evidence, review and PR readiness

Status: complete
Blocking dependencies: T01, T02, T03, T04, T05, T06, T07, T08, T09, T10, T11, T12, T13, T14, T15, T16, T17, T18
Audit scope: All F01-F14 + A06

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

Workspace, sdk generated contracts, evidence/product-paths.tsv, .github/workflows/ci.yml, final review reports

## Work

Regenerate checked-in SDK types and resolve whole-workspace fallout. Fill required honest evidence gaps from ticket handoffs. Run Fast CI-equivalent local checks: fmt, layer3 runner/product-path contracts, all-target/all-feature clippy locked, SDK build, full nonignored workspace tests once with hostile ambient context, SDK package and shell contract checks. Run four isolated axes on full PR diff; fix every actionable issue with implementer, triage stale axes, repeat until all four current passes. Push complete PR, verify required CI, mark ready, clean only task-owned implementer worktrees.

## Acceptance

Every ticket integrated with concrete commit/test evidence; no acceptance criterion left unresolved; source compiles for all test targets; generated contracts match; Standards/Spec/Maintainability/Simplicity independently pass at final state; PR ready, no base-branch merge or deployment. Any missing required external check is explicit and remains pending until resolved.

## Verification

Rungs 1-3 as justified by changes; no gratuitous informing/authority escalation. Follow actual Fast CI configuration and repository scripts.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T19.md`; coordinator owns index/status updates and final four-axis review.

## Completion evidence

Integrated code head: `40b36a4210e1cde9b3a02c82b14135e12496a4a0`. All T00–T18 changes and review corrections are integrated. Final workspace run: 1,360 passed, 0 failed, 47 existing ignored. Workspace Clippy, SDK packaging/TypeScript, generated contracts, formatting, evidence/runner and shell contracts pass. Fast CI passed at this code head: https://github.com/getployz/ployz2/actions/runs/33938879730.

Standards: PASS. Spec: PASS. Maintainability: PASS. Simplicity: PASS. Separate final reports and dispositions are retained in `/home/codex/.cache/ployz-domain-invariants-notes/review/`. Rungs 1–3 and exact regression evidence are recorded in ticket handoffs and `evidence/product-paths.tsv`; informing/authority tests were not run.

PR https://github.com/getployz/ployz2/pull/739 is ready for review. All 20 owned implementer worktrees and the task-created target symlink have been removed. The integration worktree and original audit artifacts are preserved. No base-branch merge or deployment was performed.
