# Agent Instructions

## Read First

- Read the [contributor code map](docs/architecture/code-map.md) before changing
  repository structure, runtime ownership, state ownership, or test placement.
- Read `VISION.md` before product or architecture work.
- Read `CONTEXT.md` before product, architecture, or domain-model work. Use
  its preferred terms in code, docs, tests, CLI copy, and operation/state names.
- Read `docs/adr/` before architecture work; the accepted ADRs are the
  current control-plane direction.

## Issue Tracking And Wayfinder

Use GitHub issues via `gh`.

For `/wayfinder`: one `wayfinder:map` issue; child issues labelled
`wayfinder:{research,prototype,grilling,task}`; native sub-issues and
dependencies when available, otherwise task-list children plus `Part of #...`
and `Blocked by: #...` body lines. Claim by assigning yourself first. Refer by
linked title, not bare number. Resolve one ticket per session: comment, close,
then add a linked one-line gist to the map.

**v2 is coreless —
[ADR 0040](docs/adr/0040-corrosion-replaces-the-core-and-nats.md).**
Corrosion rows over HTTP/JSON/SSE on a WireGuard mesh; no core, no
sequencer, no NATS. Control-plane architecture guidance lives in ADR 0040
and the v2 design specs, not in this file; the incumbent code in-tree is
frozen and converges to that shape.

## Product Direction

Ployz is a small-cluster orchestration core built around explicit operations.

Every mutating action should:

- validate preconditions,
- create an operation,
- emit durable progress,
- perform bounded work,
- finish with one terminal result,
- leave useful evidence on failure.

Work that is bounded, local, and atomic is a write, not an operation. A
validated intent write returns its result synchronously and carries its own
provenance; an operation record would only describe work that already
succeeded or already failed. Work that spans hosts, processes, or time stays
an operation: machine add, deploy, removal, and recovery.

The product is primitives, not hidden policy. Do not add background behavior
that changes cluster truth without an operation owner. Keeper is the scoped
exception: it converges machine substrate toward decisions an operator
already recorded, never new cluster truth.

A multi-step journey is a composition of primitives, never a command that
fuses them. Each primitive keeps one meaning, and each refusal names the
command that resolves it, so the refusal is the seam where one primitive hands
off to the next. Two consequences bind everywhere. A refusal never performs work; it names the
command that does. And no operation gets a `--force` variant whose forced and
unforced paths commit identical truth, because that flag records only that the
operator was willing to type it twice — the generic misleading verb this rule
exists to prevent.

## Module Ownership

The contributor code map is the canonical path-level ownership guide; read it
for crate- and path-level boundaries.

Keep dependencies flowing inward. Business logic must not import process wiring.
Transport adapters must not import product orchestration convenience types.

## Operation Rules

- Model operation state as enums with explicit transitions.
- Terminal states are final.
- Failed operations carry typed failure details.
- Failed started deploy containers should be retained for inspection.
- Retrying must not erase prior failure.
- Logs are evidence, not the audience.
- Operation status and events are the audience.
- Next deploys may converge from observed reality, but background loops must
  not silently mutate cluster truth.

## Code Style

- Prefer plain structs, enums, and async functions.
- Add a trait only when there are two real implementations or a hard test seam.
- Avoid generic operation engines.
- Avoid actor frameworks.
- Avoid stringly states.
- Avoid sparse option bags for variant data.
- Encode system invariants in types. If a state, transition, target, or failure
  shape is invalid, make it unrepresentable instead of documenting the rule.
- Use typed ids for storage keys, subjects, placement, routing, authorization,
  and operation state.
- Keep handlers small. A handler must not own transport, authorization,
  orchestration, storage, and presentation at once.
- Centralize public route and path construction without building a complex
  type-level routing language.
- No external control-plane I/O may wait forever.
- Every long-running task needs shutdown, timeout, retry/backoff, and visible
  health.

## Rust Rules

- Use slice patterns over indexing.
- Use explicit enum values; avoid `Default::default()`.
- Destructure in trait impls to catch new fields.
- Match project enums exhaustively; no wildcard arms for convenience.
- Never `.unwrap()` optional state; use `let Some(x) = opt else { ... }`.
- Add `#[must_use]` to builder methods returning `Self`.
- Prefer enums over booleans for modes, phases, policies, outcomes, and
  failure classes.
- Prefer variant-specific data over optional fields shared across variants.
- Booleans are only for obvious yes/no facts with no plausible third state.
- Treat Clippy suppressions as a last resort; fix the shape first.

## Comments

- Comments are timeless: they describe what the code is and the invariants it
  keeps, for every future reader across revisions. Never write comments about
  the act of changing the code - what was just deleted, moved, renamed, or
  why an edit was made. That narration belongs in commit messages and PR
  descriptions; in the file it is stale the moment it lands.

## Codex Task Supervision

- A Codex ticket task is its own supervisor. It owns orientation, the spec and
  acceptance checklist, packet boundaries, integration, finding dispositions,
  verification, current-main revalidation, the PR, landing, issue closure, and
  cleanup.
- Delegate substantive implementation to native subagents in bounded,
  non-overlapping packets with explicit files or seams, tests, expected output,
  and do-not-touch scope. Inspect every subagent diff before accepting it.
- Keep immediate glue work in the parent only when delegation would cost more
  than the change. Do not turn a one-line integration fix into a work packet.
- Implementation subagents run focused tests for their packet. They do not run
  workspace-wide gates, SDK generation, DinD, GitHub publication, or cleanup;
  the parent task serializes and owns those shared operations.
- Codex-native implementation tasks and implementation subagents do not invoke
  Codex through the CLI or app-server; use native tools and native subagents for
  implementation. Fresh-context cold reads, plan gates, and review gates may
  invoke Codex through the CLI when the dispatcher requests it or when the
  configured external review model is unavailable. Record the CLI model and
  reasoning effort, keep the invocation read-only, and never use it to edit the
  candidate.
- Before implementation, the supervisor drafts the plan and runs an independent
  plan gate. Use `opus-advisor` when available. When the dispatcher requests
  Codex CLI or the external review model is unavailable, a fresh-context,
  read-only Codex CLI `gpt-5.6-sol` High gate is a valid substitute. The advisor
  reviews only; Codex owns the plan and every implementation decision.
  `PLAN_REVISE` returns the plan to Codex. An unavailable or unverified required
  route stops implementation unless the dispatcher explicitly made the gate
  best-effort.

## Code Reviews

- When asked for a thermo-nuclear (or "thermodynamic") review, dispatch it as
  a subagent with fresh context and a deliberately minimal prompt: point it at
  `.agents/skills/thermo-nuclear-code-quality-review/SKILL.md`, give it the
  diff range, and say "report back your findings" - nothing else. Do not
  paste the skill's rules into the prompt, add steering, pre-seed suspected
  findings, or explain the design decisions behind the diff. The reviewer's
  value is its cold read.
- Implementation receives one four-lane Codex cold-read wave: Standards and
  Spec are `/code-review`'s two axes; thermo-nuclear is the skill above; and
  ponytail is the `ponytail-review` skill. Run each lane once through a separate
  fresh-context, read-only Codex CLI invocation. Every reviewer uses
  `gpt-5.6-sol` with high reasoning effort. If the CLI cannot verify model or
  effort, record that limitation; do not claim the routing succeeded. Native
  subagents remain the implementation route and do not duplicate these cold
  reads merely for harness symmetry unless the dispatcher explicitly changes
  the review route.
- Do not mirror the CLI cold wave through `opus-advisor` or wait for Claude
  capacity. Add another independent read only for an exceptionally risky seam
  where it supplies a materially distinct judgment, never to preserve a model
  or harness matrix. Treat security, authority,
  money, privacy, destructive behavior, persistence, migrations, concurrency,
  distributed state, public contracts, architecture boundaries, or a broad
  multi-module diff as large or risky. The supervisor records the classification.
- A second full Codex/Opus wave requires dispatcher approval and is reserved
  for fixes that materially change the public contract, authority boundary,
  state model, or more than roughly 20% of the reviewed diff.
- Correctness, security, data-loss, and unmet-spec findings block. Broader
  maintainability or simplification ideas become follow-up tickets unless the
  diff introduced the regression directly.
- Do not open the PR until every required Opus response is valid and
  model-confirmed, every review finding is dispositioned, and no blocker remains.
- Merging current main does not invalidate the ticket review. Review semantic
  conflict resolutions and run the verification gates on the merged candidate.

## Verification Scheduling

- During implementation, run focused tests for changed seams. After review
  findings are dispositioned, a ticket may run full workspace gates immediately;
  it does not wait for the landing queue.
- Keep `pnpm check:generated` on every final candidate. Run SDK typecheck/tests
  when SDK source or generated output changed; otherwise record them as not
  applicable.

## Verification Gates

A change is green when all of these pass locally on the branch as it will
merge. These mirror `.github/workflows/pr.yml` — the merge decision is made
on the local run, never by waiting for GitHub checks, so a gate added to the
workflow is added here in the same change:

- `cargo fmt --all` leaves no diff.
- `cargo clippy --workspace --all-targets` reports zero warnings.
- `cargo test --workspace` passes. Grep the output for `test result: FAILED`
  and `error[`; an exit code that passed through a pipe lies (zsh:
  `${pipestatus[1]}`). The real Corrosion integration test downloads the exact
  pinned archive when its verified target cache is cold. Offline runs set
  `PLOYZ_CORROSION_ARCHIVE` to a pre-fetched archive; the test verifies that
  file against the release-manifest SHA before execution.
- From `packages/ployz-sdk`: always run `pnpm check:generated` (regenerates and
  diffs `generated.ts` + the operation-contract fixture — commit real drift).
  When SDK source or generated output changed, also run `pnpm typecheck` and
  `pnpm test`; otherwise record both as not applicable.
- When `.github/workflows/` changes: `actionlint`.
- `scripts/dind-e2e.sh` currently compiles and tests the surviving role-neutral
  DinD harness. The incumbent cluster scenarios and real-host scripts were
  deleted with their NATS-era subjects. A v2 slice that introduces behavior
  requiring Docker-in-Docker or real-host proof must add a public-seam scenario
  and restore the corresponding gated invocation in the same change. Until
  then record `DinD: not applicable` with the deterministic tests that cover
  the changed seam.
- When merging main into a branch, compose semantic conflicts: union the
  imports, keep both sides' additions, and give each side's exhaustive
  matches the arms the other side's new enum variants need.
