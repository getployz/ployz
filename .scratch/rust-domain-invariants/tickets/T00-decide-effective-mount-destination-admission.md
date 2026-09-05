# T00 — Decide effective mount destination admission

Status: complete
Blocking dependencies: none
Audit scope: F08

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/service_graph.rs; ployz-core/src/domain/spec.rs; ployzd/src/docker/lifecycle.rs; ployzd/src/docker/create.rs

## Work

Trace effective config defaults and Docker mount assembly. Decide a conservative, explicit rule for conflicting sources at the same effective target; preserve repeated sources at distinct targets. Resolve whether identical duplicates normalize or reject, using production code and primary Docker documentation/source if needed. Save decision with examples and source evidence at /home/codex/.cache/ployz-domain-invariants-notes/mount-policy.md. No production edits.

## Acceptance

Concrete decision examples: volume/config both target /data; config target omitted and name data; same source at /one and /two; identical duplicate and dot/trailing-slash spellings. Distinguish syntactic guarantee from external path semantics.

## Verification

Exploration only; acceptance is a source-backed rule implementable at the existing spec admission seam.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T00.md`; coordinator owns index/status updates and final four-axis review.

## Resolved decision

Reject every duplicate effective mount destination, including identical declarations, after lexical absolute Unix path cleaning (collapse slashes/dot, resolve dot-dot at root, remove trailing slash). Reject effective root only for mount targets. Config defaults resolve to /{name} through the same effective-target path used by materialization. Repeated sources at distinct destinations and parent/child destinations remain legal. This proves lexical destination uniqueness, not filesystem/source/symlink equivalence.

Evidence: Moby v28.3.3 volume/mounts/linux_parser.go (destination cleaning and root validation) and daemon/volumes.go (duplicate destination rejection irrespective of source). Full decision matrix and primary links: /home/codex/.cache/ployz-domain-invariants-notes/mount-policy.md.
