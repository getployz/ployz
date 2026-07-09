# Agent Instructions

## Read First

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

## Product Direction

Ployz is a small-cluster orchestration core built around explicit operations.

Every mutating action should:

- validate preconditions,
- create an operation,
- emit durable progress,
- perform bounded work,
- finish with one terminal result,
- leave useful evidence on failure.

The product is primitives, not hidden policy. Do not add background behavior
that changes cluster truth without an operation owner.

## Architecture

Use NATS as the control-plane backplane:

- NATS Service API for commands, machine RPC, and live testimony
  (facts, bids, status).
- Plain NATS subjects for fanout only: intent-changed and operation progress.
- Core-local intent and evidence files for durable control-plane storage.
- Machine-local fact ledgers for machine-owned truth.
- RPC artifact push for larger control-plane artifacts.
- Subject permissions for authority.

The `Where Control-Plane State And Behavior Live` section below is the rule
that decides which of these any new piece of state or behavior uses.

Use direct TLS-authenticated NATS for machine control-plane connectivity:

```text
async-nats
  -> TLS NATS
  -> nats-server
```

Private overlay transport may be revisited later. Product commands go through
NATS.

## Control Plane And Data Plane

- `ployzd` is control plane: bootstrap, health, services, controllers, machine RPC.
- `ployzd` is not the data plane.
- `nats-server`, gateway, DNS, and workloads are independently supervised.
- Core `ployzd` down must not mean NATS/gateway/DNS down.
- Edge `ployzd` down stops that machine's RPC/observations, not its running
  workloads.
- Gateway and DNS watch NATS directly and keep last-known-good state.
- If `ployzd` starts data-plane/substrate processes, it is a supervisor and
  needs explicit readiness, restart, shutdown, health, and recovery tests.

## Module Ownership

Expected crate shape:

- `ployz-core`: ids, subjects, state models, operation models, deploy planning,
  security role models.
- `ployz-nats`: NATS connection, bootstrap, services, subject construction,
  permissions, and plain-subject transport helpers.
- `ployzd`: process wiring, service handlers, controllers, machine agent, Docker,
  gateway, DNS, certs.
- `ployzctl`: CLI client.
- `ployz-sdk-types`: public schema/type export surface.

Keep dependencies flowing inward. Business logic must not import process wiring.
Transport adapters must not import product orchestration convenience types.

## Control Plane Rules

- User-facing commands are NATS services.
- Machine-local commands are machine-scoped NATS services.
- Mutating services return operation ids quickly.
- The core sequencer owns mutating operation admission and operation evidence.
- Resource fences live in the core sequencer unless a named atomic authority
  file is explicitly introduced.
- NATS credentials and subject permissions are the authority boundary.
- No external control-plane I/O may wait forever.
- Every long-running task needs shutdown, timeout, retry/backoff, and visible
  health.

## Where Control-Plane State And Behavior Live

Every new piece of state or behavior is exactly one of four kinds. Classify it
before adding it; the kind fixes where it lives and how it is read. Lean on
NATS Services for the request/reply kinds — the product already runs on the
NATS Service API, so this is discipline, not new dependency.

- **Durable operator decision** — roster, lifecycle, route bindings, serving
  promotions, authorized users. Lives in core-local intent evidence files,
  served by one `intent.get` service endpoint, invalidated by an
  `intent.changed` broadcast. Readers call the endpoint and never import the
  storage behind it: one projection owner, many storage-blind callers, so
  moving a store is invisible to every reader.
- **Live machine or role testimony** — facts, placement bids, gateway/DNS
  status, logs. Answered by a NATS service request at the point of use, never
  cached into shared truth. A stale answer is one fresh gather away, and a
  dead responder surfaces as silence, not as inferred state.
- **Fanout invalidation** — `intent.changed`, operation progress. A plain
  subject carrying "something changed / here is the latest," never authority.
  A missed message is repaired by the periodic rebroadcast or a re-list on
  reconnect, never by trusting the delta.
- **Durable operation evidence** — operation records and events. The
  sequencer's local append-only log, mortal with the core; an external
  subscriber such as Cloud keeps durable history if it wants it.

Two disciplines keep the split honest:

- **A gather is driven by a known set, never by who answers.** Request-reply
  reports only responders, so take the candidate set from intent; silence is
  `intent − responders`, recorded as typed evidence, never read as "none." A
  service's own PING/INFO/STATS discovery is observability, never membership
  or truth.
- **No NATS Service is authoritative by existing.** A responder answering does
  not make it a member, and a service call never stores state or orders work
  durably — those are intent files, the sequencer, and the evidence log.

## State Rules

Where each kind of state lives is the `Where Control-Plane State And Behavior
Live` section above. These rules are about truth semantics, not storage:

- Docker is execution reality.
- Docker labels are recovery evidence.
- Local machine storage is cache/evidence, not cluster truth.
- Active service state is committed only after successful deploy completion.
- Pending and failed targets live in operation state/events.
- Do not infer liveness into stored truth.

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
- Centralize subject construction without building a complex type-level subject
  language.

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

## Code Reviews

- When asked for a thermo-nuclear (or "thermodynamic") review, dispatch it as
  a subagent with fresh context and a deliberately minimal prompt: point it at
  `.agents/skills/thermo-nuclear-code-quality-review/SKILL.md`, give it the
  diff range, and say "report back your findings" - nothing else. Do not
  paste the skill's rules into the prompt, add steering, pre-seed suspected
  findings, or explain the design decisions behind the diff. The reviewer's
  value is its cold read.
- Implementation work is reviewed across four lanes, always: standards and
  spec are `/code-review`'s two axes; thermo-nuclear is the skill above,
  dispatched as above; ponytail is the `ponytail-review` skill. Each lane
  runs twice — once as a Claude subagent, once as a Codex run — so every
  lane gets a second opinion. All eight are cold reads: the diff range, the
  lane's skill, "report back your findings", nothing else. A finding both
  engines raise is real; judge unseconded findings on merit.

## Verification Gates

A change is green when all of these pass on the branch as it will merge:

- `cargo fmt --all` leaves no diff.
- `cargo clippy --workspace --all-targets` reports zero warnings.
- `cargo test --workspace` passes. Grep the output for `test result: FAILED`
  and `error[`; an exit code that passed through a pipe lies (zsh:
  `${pipestatus[1]}`).
- When `ployz-sdk-types` or the TS surface changes: regenerate from
  `packages/ployz-sdk` (`pnpm generate:types && pnpm generate:fixture`) and
  commit the drift.
- When product behavior changes: the full gated DinD suite
  (`scripts/dind-e2e.sh`), every scenario. One cluster fits the host —
  serialize runs with `until mkdir /tmp/ployz-dind-e2e.lock 2>/dev/null; do
  sleep 30; done` and always `rmdir` the lock afterwards, including on
  failure. A run after a code change rebuilds product binaries (no
  `PLOYZ_DIND_SKIP_BUILD`). On failure, read the evidence directory the
  harness prints before retrying.
- When merging main into a branch, compose semantic conflicts: union the
  imports, keep both sides' additions, and give each side's exhaustive
  matches the arms the other side's new enum variants need.
