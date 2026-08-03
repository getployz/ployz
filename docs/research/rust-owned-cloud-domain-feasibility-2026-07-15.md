# Rust-owned Cloud domain feasibility

Research performed 2026-07-15 against Ployz Rust `0ddd70d512ac6ee7815ca86e1b53128624d8ba9e`
and the sibling Ployz Cloud checkout `f25714ac60e6a834ef7fa3ed0c56adca2db7463f`.
The Cloud checkout is 40 commits behind its local `origin/main`
(`5062f2891978d12fc78705b39966a2c5cb8ed88d`), so canonical-main changes were
also inspected with `git show`/`git diff` without changing the checkout.

## Verdict

A Rust-authoritative module for Cloud configuration semantics, validation,
staged-change evaluation, discard planning, and Cloud-to-runtime compilation is
practical. A compatibility-first migration is roughly four to eight
engineer-weeks.

Moving Cloud persistence, auth, TanStack live collections, Inngest workflows,
GitHub/provider integrations, and React presentation into Rust is a different
project: a multi-quarter backend rewrite with little leverage for the runtime.

## What staged changes are today

Cloud does not keep a separate draft or changeset store. Canvas edits mutate
desired product rows, while deploy actions freeze those rows into
`environment_node_config_snapshot` records. Staged changes are pure comparisons
between projected current rows and a baseline selected from the latest deployment
snapshot (`../ployz-cloud/src/models/services/service-deployments.server.ts`,
`../ployz-cloud/src/models/services/deployment-editing-baseline.ts`, and
`../ployz-cloud/src/models/services/service-deployment-diff/`).

The comparison covers Cloud product concepts that the runtime deliberately does
not own: Git source settings, build configuration, secret fingerprints, logical
resource lineage, Variable Group provenance, and UI discard ownership. Canonical
Cloud main additionally folds live runtime drift into the staged-change surface
(`getServiceRuntimeDriftDiffGroups` and managed-hostname drift in
`src/models/canvas-node-diff.ts` at the commit above).

The runtime has a distinct plan. `ployz-core` derives a `DeployPlan` only after it
has a normalized `DeployRequest`, fresh machine testimony, existing container
observations, route intent, eligibility, and volume pins
(`crates/ployz-core/src/deploy/request.rs` and
`crates/ployz-core/src/deploy/planning.rs`). Cloud staged changes therefore must
not be relabelled as the core deploy plan.

## Recommended seam

Add a Cloud-owned pure Rust module, separate from `ployz-core`, with a small
versioned interface such as:

```text
evaluate(ChangeInput) -> ChangeSet
validate(DesiredEnvironment) -> Diagnostics
plan_discard(ChangeSet, selection) -> DesiredPatch
compile(FrozenEnvironment, ResolvedInputs) -> DeployRequest
```

The implementation can reuse runtime domain types while keeping Cloud product
types out of `ployz-core`. It should return semantic tagged changes and structured
values; TypeScript should retain labels, formatting, localization, database
effects, optimistic mutations, and UI composition.

For browser and server reuse, compile the pure module to WebAssembly behind one
thin TypeScript adapter. Use versioned JSON/Serde-shaped DTOs initially, preload
the module before canvas evaluation, and keep resolved secret inputs on the
server-only compilation path. Run the Rust evaluator in shadow mode against the
existing TypeScript evaluator before switching reads.

## Migration estimate

1. Contract and WASM packaging spike: 3-5 days.
2. Cloud desired/snapshot/change types and fixtures: 1-2 weeks.
3. Port staged-change semantics, runtime-drift joins, and discard planning:
   2-3 weeks.
4. Port frozen-snapshot-to-`DeployRequest` compilation and cross-repo fixtures:
   1-2 weeks.
5. Shadow comparison, mismatch telemetry, and consumer cutover: 1-2 weeks.

The work is closer to the upper bound if every field-level form validator must
also become Rust-authoritative in the first pass. Keeping structural Zod/Drizzle
validation at TypeScript persistence seams while moving semantic validation first
keeps the module deep and the migration bounded.

## Sources

- Ployz product ownership: `VISION.md`, `CONTEXT.md`, and
  `../ployz-cloud/docs/future.md`.
- Runtime state and legal read paths: `docs/architecture/code-map.md` and
  `docs/architecture/backbone.md`.
- Runtime request, revision, and plan: `crates/ployz-core/src/deploy/request.rs`,
  `revision.rs`, and `planning.rs`.
- Generated Cloud transport surface: `crates/ployz-sdk-types/src/`,
  `packages/ployz-sdk/src/generated.ts`, and `../ployz-cloud/package.json`.
- Cloud snapshots and workflow: `../ployz-cloud/src/db/schema.ts`,
  `src/models/services/service-deployments.server.ts`, and
  `src/inggest/functions/environment-deployments/deploy.ts`.
- Cloud staged-change semantics: `../ployz-cloud/src/models/canvas-node-diff.ts`,
  `src/models/services/service-deployment-diff/`, and
  `src/models/environment-resources/{variable-group-config,volume-config}.ts`.
- Canonical Cloud compiler at the inspected commit:
  <https://github.com/getployz/ployz-dashboard/blob/5062f2891978d12fc78705b39966a2c5cb8ed88d/src/models/services/environment-deployments.ts>.
- Rust/WebAssembly deployment targets:
  <https://wasm-bindgen.github.io/wasm-bindgen/reference/deployment.html>.
