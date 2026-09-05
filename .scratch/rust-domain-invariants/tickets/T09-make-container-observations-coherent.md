# T09 — Make Container observations coherent

Status: in_progress
Blocking dependencies: none
Audit scope: F05 Container

Read [the spec](../spec.md) first. Source anchors are repository-relative in your assigned worktree:

ployz-core/src/domain/observation.rs; ployz-core/src/service.rs; ployz-core/src/service/serving.rs; ployzd/src/docker/mod.rs; ployzd/src/docker/create.rs; ployzd/src/global_reconcile.rs

## Work

Remove duplicated Service name/ID from ContainerObservation and derive them from resolved spec. Use private coherent aggregate/accessors for correlated fields. Compare external Docker labels with retained spec before admitting the observation; preserve Project as separate Container ownership, Hook/Service roles, unknown runtime and same-entity historical facts. Update serialization, all consumers and tests.

## Acceptance

An observation cannot group under api while its retained spec identifies web. Normal creation/inspection, serving and observed Global extraction keep the same identity. Mismatched labels/spec are rejected or explicitly unavailable without inventing canonical identity.

## Verification

Rung 1 observation constructor/serde and Rung 2 existing observe/grouping seam. Unknown Docker states and legitimate differing specs across Containers remain accepted.

## Handoff

Implement in your assigned isolated worktree/branch. Read applicable AGENTS.md, DESIGN.md, CONTEXT.md and implementation instructions. Finish the complete vertical change, including impacted test fixtures and SDK type generation. Commit only this ticket's changes. Return commit ID, changed representation, exact checks and rung, any product-path evidence update needed, and remaining limits. Write handoff to the shared control directory `handoffs/T09.md`; coordinator owns index/status updates and final four-axis review.
