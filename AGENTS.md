# Project Direction

- Read `VISION.md` before making architectural or product-level decisions.
- Treat `VISION.md` as the source for repo scope, product direction, and design
  intent.
- This repo's focus is the orchestrator core, daemon, runtime model, and SDK
  and API surface. Future cloud products are downstream consumers of that core,
  not the source of truth for it.
- Treat new feature work as greenfield: do not add backwards-compatibility
  shims, legacy decode paths, or compatibility aliases unless explicitly
  requested for a concrete rollout need.

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
- Keep steady-state runtimes boring: prefer one-shot startup/deploy
  reconciliation and explicit event handling over interval-driven
  control-plane loops that continuously converge internal policy.
- The real distinction is reconciliation vs observation:
  periodic loops that keep re-deriving cluster policy from internal state are
  bad; periodic checks that observe external reality because it cannot emit a
  native event are acceptable.
- Polling external reality is fine when it turns an external fact into an
  explicit event or maintains narrow runtime truth such as transport endpoint
  selection. It must not silently rewrite cluster policy or operator intent.
- Operator-facing surfaces should distinguish stored intent, explicit status,
  and live observations rather than collapsing them into one derived field.
- Background tasks may publish explicit events or observations, but they should
  not silently rewrite cluster truth.

# Failure Audience Rule

Every operation has an audience. A failure is only "loud" if it lands on
someone who can act on it. It is never acceptable for a failure to have no
audience — that is the definition of silent degradation.

Three categories, each with a different audience and a different shape:

- **Foreground** (RPC handler, CLI command, deploy commit): a synchronous
  caller is waiting. Audience is the caller. Propagate `Result` outward; do
  not log-and-continue, do not `let _ = ...` on a meaningful Result.
- **Background-with-consumer** (snapshot reload, subscription, projection):
  another component reads the output. Audience is the next read.
  Failure must annotate the output with explicit health state (e.g.
  `SnapshotHealth::Stale { since, last_error, consecutive_failures }`),
  preserve the last good value, and let the consumer pick policy
  (serve stale with a header, refuse past a threshold, etc).
  Never silently keep serving stale data as if it were fresh.
- **Background-autonomous** (cert renewal, cleanup, finalization): no direct
  consumer. Audience is the operator. Failure must preserve prior state,
  emit an explicit observation, retry with backoff, and escalate to an
  operator-visible status surface (`ployz status`, metrics, telemetry)
  after N failures. Fire-and-forget `tokio::spawn` without a supervisor
  is a bug.

Corollary: stale-state-served-silently is the worst class of failure in this
codebase, because the daemon thinks it is fine, the operator sees no errors,
and the data plane is wrong. Always model freshness explicitly when a
background loop is the source of truth.

# Defensive Rust Rules

- Use slice patterns over indexing: `let [a, b] = slice else { ... }` not `slice[0]`
- Use explicit enum values, never `Default::default()`
- Destructure in trait impls to catch new fields: `let Self(x) = self;`
- Never wildcard on project-defined enums — spell out all variants
- Never `.unwrap()` on Option state — use `let Some(x) = opt else { return err }`
- Add `#[must_use]` on all builder methods returning `Self`
- Prefer enums over boolean parameters
- Treat Clippy suppressions as a last resort. First ask why the lint is firing,
  whether the code should be split, simplified, or reshaped, and fix the code
  directly when that makes sense.

# Test Discipline

- Before enabling parallel CI for changed E2E paths, rerun the affected scenarios repeatedly and fix ordering/idempotency bugs instead of adding sleeps or longer timeouts.

# Build Discipline

- Default inner-loop test command is `just test`, which excludes `ployzd` and
  `ployz-runtime-backends`. Those two crates drag in the Docker client, userspace
  WireGuard, pingora, and hickory; compiling them dominates cold-build time.
- Use `just test-all` before pushing or when changes touch `ployzd`,
  `ployz-runtime-backends`, or anything that affects the full build graph.
- When editing a specific crate, prefer `cargo test -p <crate>` over the
  full-workspace form.
- `ployz-runtime-backends` exposes `docker` and `userspace-wg` cargo features,
  both on by default. For tests that don't need them, compile with
  `cargo test -p ployz-runtime-backends --no-default-features`.
