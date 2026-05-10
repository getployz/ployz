---
title: "feat: Service branch source preview primitives"
type: feat
status: active
date: 2026-05-10
origin:
  - VISION.md
  - docs/architecture/deploy-primitives-roadmap.md
  - docs/plans/2026-05-08-004-feat-service-branching-deploy-plan.md
  - docs/plans/2026-05-10-003-feat-deploy-volume-snapshot-clone-branching.md
---

# feat: Service branch source preview primitives

## Summary

Add the first service-side deploy evidence for branch environments: deploy
preview can distinguish services that are fresh in the target namespace from
services branched from a committed source service. This is the service
counterpart to volume clone evidence. It does not implement full `ployzctl
branch` yet; it gives deploy a typed preview vocabulary that branch commands
and cloud workflows can compile to later.

## Problem Frame

Volume cloning now has source lineage and clone replacement preflight evidence.
Service branching has partial lineage through branch source records, but the
deploy preview surface does not yet express the core branch vocabulary:

- fresh service,
- branched service.

Without explicit service source modes, cloud and CLI workflows will be tempted
to encode branching behavior outside core. That would violate the core/cloud
boundary in `VISION.md` and make branch promotion, rollback, and evidence harder
to reason about.

Portal is intentionally not part of this slice. The current manifest already has
a `ServiceIntent::Portal` shape, but planning support rejects it. Keeping that
rejection is correct until portal authorization, source opt-in, preview
redaction, and runtime semantics are designed.

## Scope

In scope:

- Add deploy preview vocabulary for resolved service source mode.
- Expose service source mode in `DeployPreview`.
- Preserve existing service branch lineage behavior for branch mode.
- Add validation and preview tests for fresh-derived and branch-derived
  services.
- Keep portal rejected before source lookup, participant planning, or mutation.
- Update generated deploy manifest schema/types if the service intent schema is
  touched.

Out of scope:

- Full `ployzctl branch`.
- Portal source lookup, preview linkage, participant planning, authorization, or
  execution.
- Portal volume semantics.
- Production promotion.
- Service move planning, preview, execution, or placement migration. Existing
  `ServiceIntent::Move` validation remains rejected until a dedicated service
  move primitive exists.
- Cloud UI implementation.
- Cloud consumption of the new preview field before generated preview schemas
  exist. If this repo still lacks preview schema generation, document that gap
  and treat the field as core-internal evidence until the generated API package
  lands.

## Key Decisions

1. Source mode is planned work, not a warning.
   Clients should read typed preview fields instead of parsing warning strings.

2. Portal remains rejected/reserved.
   This slice must not make portal previewable. Preview cannot expose source
   namespace/service topology unless the caller is authorized to view it, and no
   such policy exists yet.

3. Branch lineage remains durable evidence.
   Branch mode should keep using committed source revision lineage, and preview
   should make the source namespace/service/revision visible.

4. Fresh is preview-derived, not a manifest intent.
   The existing manifest signal for a fresh service is absence of any
   source-preserving service intent, not just absence of branch lineage.
   `ServiceIntent::Branch` resolves to branch source evidence, and
   `ServiceIntent::Move` is a source-preserving intent that must not be
   classified as fresh. Because deploy planning currently rejects service moves,
   this slice should keep move unsupported rather than introducing partial move
   preview semantics. Do not add a public `ServiceIntent::Fresh` variant in this
   slice. The resolved preview can still show `fresh` so clients have a complete
   service-source table. The preview should make the derivation auditable, for
   example `fresh` with `origin = no_source_intent`, rather than implying the
   user explicitly supplied a fresh mode.

5. Branch source preview should not replace commit lineage yet.
   Existing `service_branch_sources` commit/preview behavior remains valid.
   The new preview field can overlap temporarily if it gives clients a clearer
   per-service source-mode view.

6. Source stability is revalidated, not serialized by target locks.
   The first slice should not claim that a target namespace lock protects source
   namespace truth. Branch preview/apply stability should use source release
   revision revalidation or compare-and-swap; broader source leases can be a
   later phase primitive.

7. Preview-to-apply drift checks require an explicit preview baseline.
   Apply cannot infer what the user previously previewed from
   `manifest_json` alone. The apply request must carry an expected preview
   fingerprint or expected branch-source revision baseline emitted by preview;
   otherwise apply can only compare plans resolved inside the same apply call
   and will silently accept source drift that happened after the user's preview.
   This slice should either add that baseline to `DeployOptions`/apply request
   and enforce it before participant RPCs, or explicitly defer
   preview-to-apply drift rejection.

## Implementation Units

### U1. Model resolved service source preview

Files:

- `crates/ployz-types/src/spec.rs`
- `crates/ployz-types/src/model.rs`
- `crates/ployz-types/src/error.rs`
- `crates/ployz-types/src/spec.rs` tests
- `crates/ployz-types/src/model.rs` tests
- `packages/deploy/deploy-manifest.schema.json` if manifest schema changes
- `packages/deploy/index.d.ts` if manifest schema changes

Approach:

- Keep the existing manifest shape: `ServiceIntent::Branch` remains the explicit
  branch intent, and absence of a service intent remains fresh.
- Do not add `ServiceIntent::Fresh`.
- Model fresh as a preview result with an explicit derived origin, not as a
  second input syntax.
- Do not relax `ServiceIntent::Portal` planning support. Portal should continue
  to fail structural/planning validation before source lookup.
- Do not relax `ServiceIntent::Move` planning support. Move should continue to
  fail planning validation before source-mode preview until service migration is
  designed as its own primitive.
- Add preview model fields that identify the resolved source mode:
  - `fresh` for services with no source lineage, including an explicit derived
    origin such as `no_source_intent`,
  - `branch` with source namespace, source service, and source revision hash.
- Add serde tests pinning snake_case wire values.

Test scenarios:

- Fresh service preview serializes as derived `fresh` mode without manifest
  source intent.
- Branch service intent validates with non-empty source namespace/service.
- Move service intent remains rejected before preview source-mode derivation.
- Portal service intent remains rejected before preview source lookup.
- Invalid empty source names are rejected structurally.
- Generated manifest schema/types remain in sync if `ServiceIntent` changes.

### U2. Resolve fresh and branch source modes into preview

Files:

- `crates/ployz-api/src/deploy.rs`
- `crates/ployz-api/src/request.rs`
- `crates/ployzd/src/request_builder.rs`
- `crates/ployzd/src/daemon/handlers/deploy.rs`
- `crates/ployz-orchestrator/src/deploy/plan.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Resolve branch mode against committed source service release truth.
- Resolve fresh mode only for services without branch source lineage and without
  a source-preserving service intent such as `ServiceIntent::Move`.
- Keep service move rejection in the existing validation path; do not resolve
  move as fresh/no-source evidence and do not expose partial move preview.
- Keep portal rejection in the existing validation path; do not resolve portal
  source namespace/service and do not expose portal preview evidence.
- Add source mode evidence to `DeployPreview` alongside existing
  `service_branch_sources`.
- Emit a stable preview baseline for branch source evidence, either as a
  preview fingerprint or as expected source revisions for each branch-derived
  service.
- Extend apply input, preferably through `DeployOptions`, so callers can send
  the expected preview baseline back with `DeployApply`.
- On apply with a baseline, re-resolve branch source revisions before
  participant inspection/RPCs and reject drift from the expected preview
  baseline with a structured error.
- On apply without a baseline, do not claim preview-to-apply source drift
  protection. At most, keep the existing same-apply plan stability check that
  compares plans resolved during that apply call.

Test scenarios:

- A fresh service preview shows fresh mode and no lineage.
- A branch service preview shows source namespace/service/revision.
- A service move intent remains rejected before preview and does not appear in
  the fresh source set.
- Preview emits a baseline/fingerprint that includes branch source revisions.
- Apply with a matching expected preview baseline succeeds.
- Apply with a stale expected preview baseline rejects branch source drift
  before participant inspect/start RPCs.
- Apply without an expected preview baseline does not run the
  preview-to-apply-drift test path and does not document that guarantee.
- Portal intent does not preview source namespace/service details.

### U3. Preserve commit evidence boundaries

Files:

- `crates/ployz-orchestrator/src/deploy/execute.rs`
- `crates/ployz-orchestrator/src/deploy/lifecycle.rs`
- `crates/ployz-orchestrator/src/deploy/tests.rs`

Approach:

- Ensure branch mode still commits durable service branch lineage with the
  target release.
- Ensure fresh mode does not create branch lineage.
- Touch shared store commit facts only if the existing branch lineage record
  shape must change.
- Ensure portal remains rejected before deploy status, participant inspection,
  release, route, or lineage mutation.

Test scenarios:

- Branch mode commit writes service branch lineage.
- Fresh mode commit writes no branch lineage.
- Portal mode rejection writes no deploy status, release, route, or lineage
  facts.

## Risks

- Portal can become a footgun if modeled as "just reuse prod" without safety
  constraints. This slice avoids that by keeping portal rejected and
  non-enumerable.
- The preview model may overlap with existing `service_branch_sources`. That is
  acceptable temporarily if the new source-mode field becomes the clearer
  contract and later cleanup removes duplication.
- Generated downstream types may lag. If preview schema generation does not
  exist yet, the PR should explicitly record that cloud consumers cannot treat
  the new preview field as generated API until that package exists.

## Verification

- `cargo fmt --check`
- `cargo test -p ployz-types service_source --quiet`
- `cargo test -p ployz-orchestrator service_branch --quiet`
- `cargo test -p ployz-orchestrator portal --quiet`
- `cargo test -p ployzd deploy --quiet`
- schema/type generation or a documented no-op if no preview generator exists
- `just test-all`
