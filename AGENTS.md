# Project Direction

- Read `VISION.md` before architectural or product decisions.
- `VISION.md` is source of truth for scope, product direction, and design intent.
- This repo is the open core: orchestrator, daemon, runtime model, SDK/API.
- Cloud products consume the core; they do not define it.
- Treat new features as greenfield. No compatibility shims unless explicitly
  requested for a concrete rollout.

# Architecture

- Thin edge apps over a small orchestration kernel.
- Domain state, pure models, and protocols live below process wiring.
- Orchestration logic must not depend on runtime/store/transport backends.
- Backends implement explicit seams; they do not point upward.
- SDK is for external consumers, not internal convenience imports.
- Dependencies flow inward toward contracts and domain logic.
- Prefer testable seams, narrow public surfaces, and policy out of binaries.

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
