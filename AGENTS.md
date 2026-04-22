# Project Direction

- Read `VISION.md` before making architectural or product-level decisions.
- Treat `VISION.md` as the source for repo scope, product direction, and design
  intent.
- This repo's focus is the orchestrator core, daemon, runtime model, and SDK
  and API surface. Future cloud products are downstream consumers of that core,
  not the source of truth for it.

# Architecture Intent

- Keep the system shaped as thin edge apps over a small orchestration kernel.
- Put durable domain state, pure models, and protocol contracts below process
  wiring and backend implementations.
- Keep orchestration and reconciliation logic independent from concrete
  runtime, store, transport, or sidecar implementation details.
- Express runtime and store integration through explicit API seams; concrete
  backends implement those seams and do not point back upward.
- Treat SDK as an external-consumer umbrella only, not as an internal import
  hub.
- Prefer dependency direction that flows inward toward contracts and domain
  logic, not sideways through convenience crates.
- When in doubt, optimize for testable seams, narrow public surfaces, and
  moving policy out of binaries and adapters.

# Operational Design Rules

- Durable cluster state should represent operator intent and explicit lifecycle
  events, not inferred liveness.
- Prefer imperative transitions triggered by commands or concrete runtime events
  over background self-healing or correction loops.
- Mutating control-plane operations should fail fast and fail loudly when
  required peers or preconditions are missing.
- Reachability checks belong at decision time through direct probes, RPC, or
  session establishment, not freshness timestamps.
- Keep placement, participation, coordination, and diagnostic classification
  policy centralized in orchestrator core helpers, not duplicated in daemon
  handlers or UI shaping code.
- Operator-facing surfaces should distinguish stored intent, explicit status,
  and live observations rather than collapsing them into one derived field.
- Background tasks may publish explicit events or observations, but they should
  not silently rewrite cluster truth.

# Defensive Rust Rules

- Use slice patterns over indexing: `let [a, b] = slice else { ... }` not `slice[0]`
- Use explicit enum values, never `Default::default()`
- Destructure in trait impls to catch new fields: `let Self(x) = self;`
- Never wildcard on project-defined enums — spell out all variants
- Never `.unwrap()` on Option state — use `let Some(x) = opt else { return err }`
- Add `#[must_use]` on all builder methods returning `Self`
- Prefer enums over boolean parameters

# Test Discipline

- Before enabling parallel CI for changed E2E paths, rerun the affected scenarios repeatedly and fix ordering/idempotency bugs instead of adding sleeps or longer timeouts.
