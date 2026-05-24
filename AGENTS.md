# Project Direction

- Read `VISION.md` before architectural or product decisions.
- `VISION.md` is source of truth for scope, product direction, and design intent.
- This repo is the open core: orchestrator, daemon, runtime model, SDK/API.
- Cloud products consume the core; they do not define it.
- Treat new features as greenfield. No compatibility shims unless explicitly
  requested for a concrete rollout.

# Documented Solutions

`docs/solutions/` contains documented solutions to past problems and reusable
patterns, organized by category with YAML frontmatter such as `module`, `tags`,
and `problem_type`. Relevant when implementing, debugging, or making decisions
in documented areas.

# Architecture

- Thin edge apps over a small orchestration kernel.
- Domain state, pure models, and protocols live below process wiring.
- Orchestration logic must not depend on runtime/store/transport backends.
- Backends implement explicit seams; they do not point upward.
- SDK is for external consumers, not internal convenience imports.
- Dependencies flow inward toward contracts and domain logic.
- Prefer testable seams, narrow public surfaces, and policy out of binaries.
- `polis` owns distributed substrate primitives, not Ployz product-shaped
  services. Keep Corrosion access, transactions, subscriptions, change
  cursors, iroh identity, tickets, peer RPC, deadlines, probes, membership
  records, leases, and distributed failure typing in `polis`.
- `ployz` owns product behavior: machine join semantics, deploy semantics,
  namespace meaning, routing decisions, capacity policy, volume movement,
  readiness, and operation outcomes. Put translation from Ployz ports to
  Polis primitives in `crates/ployz/src/adapters/polis/`.
- Do not add product-shaped Polis APIs such as `machines.join`,
  `capacity.reserve`, `deploy.record_ready`, or routing policy unless the
  concept has proven to be substrate infrastructure rather than product
  behavior. Ployz adapters may be purposeful and thicker when sequencing
  store, subscription, lease, probe, and RPC primitives.
- New distributed-state work targets Corrosion rows/subscriptions plus iroh
  peer RPC. Treat NATS and p2panda control-plane guidance as historical unless
  a task explicitly asks to maintain old code.

# AI Architecture Guardrails

- AI is allowed to implement features only inside established boundaries. It
  must not invent architecture by accreting fields, enum variants, or branches
  onto existing global paths.
- Adding a capability must not add variants to one global control-plane enum
  unless the capability is truly public API. Internal node RPC must have its own
  typed protocol, separate from external CLI/API requests.
- `DaemonState` must stay a router and lifecycle owner. Do not add
  feature-specific state to it. Feature state belongs in the subsystem that owns
  the feature.
- No handler file may own transport, authorization, orchestration, storage, and
  presentation at once. Split by responsibility before adding behavior.
- The daemon command router must not become a feature registry. If adding a
  command requires touching unrelated handlers or existing feature state, stop
  and create a smaller protocol/dispatch boundary first.
- Backends must not depend upward on daemon, API presentation, or orchestration
  convenience types. Move shared contracts down instead of importing up.
- Store capabilities must be requested by the narrow trait a subsystem needs,
  not by a whole-store facade when the operation only needs one domain.
- Adding a feature must include the ownership rule for its state: who owns it,
  who may mutate it, which messages/events cross the boundary, and who observes
  failures.

# State And Data Representation

- Never use sparse option bags for variant-specific data. Use enums with data
  carried by the relevant variant.
- Never flatten structured domain data into `Vec<String>`, positional arrays,
  or index-based rows except at the final rendering boundary.
- Domain identity must use newtypes or typed fields, not raw strings, when the
  value participates in storage keys, routing, placement, authorization, or
  state transitions.
- Sort, placement, routing, cleanup, and authorization logic must operate on
  typed fields, not display strings or positional columns.
- Stringly fallback errors are allowed only at backend wrappers, transport
  wrappers, serialization/hash fallbacks, test fakes, or presentation edges.

# Control Plane Boundaries

- All privileged operations must pass through an authorization boundary before
  reaching handlers. Local IPC, iroh peer RPC, HTTP endpoints, and background
  tasks are separate trust boundaries.
- Iroh peer RPC must not deserialize directly into public daemon API requests.
  Internal peer commands need a smaller typed protocol with explicit allowed
  operations.
- Background tasks must not silently rewrite durable control-plane truth. They
  may publish observations or send typed commands to an owner that applies a
  checked state transition.
- Background tasks must have a supervisor, shutdown path, health surface, and
  bounded retry policy with backoff/jitter where many nodes may retry together.
- No external control-plane I/O may await indefinitely. Docker, iroh, SSH, HTTP,
  filesystem locks, process waits, and publish/flush paths need explicit
  operation timeouts.
- If lock renewal, lease refresh, or coordination publish can stall, the caller
  must treat that as lock loss within a bounded deadline.

# Operations

- Durable state records operator intent and explicit lifecycle events.
- Do not infer liveness into stored truth.
- Prefer commands and concrete runtime events over self-healing loops.
- Mutating control-plane operations fail fast when peers/preconditions are
  missing.
- Probe reachability at decision time, not via freshness timestamps.
- Centralize placement, participation, coordination, and diagnostics in core.
- Keep steady state boring: startup/deploy reconciliation plus explicit events.
- Reconciliation is not observation. Observing external reality is fine; using
  periodic loops to rewrite cluster policy is not.
- Operator surfaces must separate intent, status, and live observation.
- Background tasks may publish events/observations; they must not silently
  rewrite cluster truth.

# Failure

- Every failure needs an audience.
- Model failure by audience: who learns it failed, when, and what can they do?
- Foreground work returns `Result` to the caller.
- Expected failures must be structured error variants with context, not ad hoc
  strings.
- Stringly fallback errors are only for unclassified backend/transport wrappers,
  serialization/hash fallbacks, test fakes, or presentation-only messages.
- Callers must branch on error types, not parse display text.
- Background-with-consumer work preserves last good value and annotates
  freshness/health for the next reader.
- Background-autonomous work preserves prior state, emits observations, retries
  with backoff, and escalates to operator-visible status.
- Logs are evidence, not an audience.
- Retrying does not erase failure.
- Defaults must not hide uncertainty.
- Background work needs a supervisor or visible state surface.
- Stale-state-served-silently is the worst failure class here.

# Rust

- Use slice patterns over indexing.
- Use explicit enum values; avoid `Default::default()`.
- Destructure in trait impls to catch new fields.
- Match project enums exhaustively; no wildcard arms for convenience.
- Never `.unwrap()` optional state; use `let Some(x) = opt else { ... }`.
- Add `#[must_use]` to builder methods returning `Self`.
- Prefer enums over booleans for modes, phases, policies, outcomes, freshness,
  and failure classes.
- Booleans are only for obvious yes/no facts with no plausible third state.
- Keep variant-specific data in the variant.
- Model state machines as enums plus transition methods, not loose fields.
- Persisted or public enum variants are API surface.
- Treat Clippy suppressions as a last resort; fix the shape first.

# Tests

- Default inner-loop: `just test`.
- Use `cargo test -p <crate>` for crate-local edits.
- Use `just test-all` before pushing or when touching `ployzd`,
  `ployz-runtime-backends`, or the full build graph.
- For `ployz-runtime-backends` tests that do not need Docker/userspace WG:
  `cargo test -p ployz-runtime-backends --no-default-features`.
- Before parallelizing changed E2E paths, rerun affected scenarios repeatedly and
  fix ordering/idempotency bugs. Do not add sleeps to hide them.
