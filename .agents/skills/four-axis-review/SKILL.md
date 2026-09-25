---
name: four-axis-review
description: Gate changes on four independent review axes—repository standards, spec fidelity, maintainability, and simplicity. Use instead of $code-review during $implement, or when AGENTS.md requires the four-axis review.
---

# Four-axis review

This skill supersedes `$code-review` when used by `$implement`.

Use the supplied fixed point and spec; otherwise use the merge-base with `origin/main` and the change request. Confirm the ref resolves and the diff is non-empty.

## Initial pass

Run four read-only, isolated reviewers in parallel—one per axis:

1. Run the **Standards** brief from `$code-review`: return `PASS`, or `FAIL` with findings.
2. Run the **Spec** brief from `$code-review` with the spec: return `PASS`, or `FAIL` with findings.
3. Cold-read `$thermo-nuclear-code-quality-review`: return **Maintainability** `PASS`, or `FAIL` with findings.
4. Cold-read `$ponytail:ponytail-review`: return **Simplicity** `PASS`, or `FAIL` with cuts and net lines removable.

The initial thermo and ponytail reads are cold: give each reviewer only its skill invocation, the full diff, and the verdict format—no spec, conversation, or other review output. Only the initial read is cold; every later round reuses the same reviewer (see Incremental loop).

Keep all four reports separate. Give every finding a disposition: fix it or reject it with concrete evidence.

## Incremental loop

After fixes, spawn one read-only triage subagent with the delta since the last reviewed state plus the prior findings and dispositions. It returns the smallest set of axes the delta may have invalidated; include an axis when uncertain, and always include the source axis of a fixed finding.

Rerun only those axes. Reuse each axis's existing reviewer: send it the delta, the new head, and the dispositions of its findings (via SendMessage), and never spawn a new reviewer for an axis that already has one. Keep axes in separate agents; never combine Standards and Spec. A rejected finding stays rejected unless the reviewer brings new evidence. Preserve passing results for unaffected axes.

Apply all fixes for a round with one implementer, and reuse that same implementer for later rounds.

Repeat triage and selective review until every axis is current and has no actionable finding. Report the four final results separately.
