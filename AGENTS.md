# AGENTS.md

Practical guidance for coding agents working in this repository.

## Scope

- Language: Go
- Module: `ployz` (see `go.mod`)
- Build/run/test commands: see `justfile`

## Active Architecture (Current)

The active package graph is intentionally small:

- `cmd/ployz`: operator CLI
- `cmd/ployzd`: daemon entrypoint
- `daemon`: runtime shell and unix-socket gRPC server (`daemon/pb`)
- `machine`: local machine identity/state lifecycle
- `machine/mesh`: network stack lifecycle (WireGuard, store runtime, convergence)
- `machine/convergence`: event-driven peer reconciliation loop
- `infra/*`: adapters for external systems (WireGuard, Corrosion, SQLite, etc.)
- `platform/*`: OS-specific defaults and backend selection
- `internal/*`: support packages (`check`, `logging`, `remote`, `support/buildinfo`)

Historical code under `_internal_legacy_do_not_read/` is reference-only and not part of the active package graph.

Dependency direction is one-way:

`cmd/* -> daemon -> machine -> machine/* -> infra/*`

`platform/*` selects concrete implementations at the wiring edge.

Startup ownership is strict: `platform/*` builds mesh builders, `daemon` decides when to invoke them, `machine` only runs attached mesh.

## Go Tiger Style

This repository follows Go Tiger Style priorities, in order:

1. Safety
2. Performance
3. Developer Experience

Use `.claude/skills/tiger-audit/SKILL.md` for the full audit checklist.

## Ground Rules

- Prefer small, focused changes.
- Match existing patterns before introducing new abstractions.
- Preserve cross-platform behavior (`linux`, `darwin`, and stubs/fallbacks).
- Do not silently change CLI behavior without updating command help/output.
- Before finishing, run `just test` and `just build`.
- If dependencies changed, run `just tidy`.

## Core Design Rules

1. Interfaces live in the consumer package (`ports.go` near the caller).
2. Core logic packages (`machine`, `machine/mesh`, `machine/convergence`) do not import infra packages directly.
3. All external side effects (network/filesystem/process/DB/SSH) belong in `infra/*`.
4. Inject dependencies before lifecycle calls; no hidden constructors/build callbacks in `Run()`/`InitNetwork()`.
5. Keep persistence I/O in adapters; keep domain types/validation in core packages.
6. Pass `context.Context` first for blocking or I/O operations, and respect cancellation.
7. Wrap errors with operation context; use `errors.Is`/`errors.As` for policy checks.
8. Do not silently discard errors unless the best-effort cleanup is intentional and documented.
9. Use `time` directly in code; test time/concurrency with `testing/synctest`.
10. Bound retries/queues/read sizes and name non-trivial constants.

## Assertions and Invariants

Use `internal/check.Assert()` for programmer errors (wiring bugs, impossible states):

- constructor preconditions
- derived-value postconditions
- exhaustive `switch` defaults

Assertions are not runtime error handling. Runtime failures should return errors.

## Testing Expectations

- Behavior changes in `machine`, `machine/mesh`, or `machine/convergence` should include:
  - at least one success-path test
  - at least one failure-path test
- Core unit tests (`machine*`) use fakes only; adapter tests (`platform/infra`) are integration-tagged.
- Prefer table-driven tests for pure logic.
- Keep test doubles local and small.
- Useful commands:
  - `go test ./path/to/pkg -run '^TestName$' -count=1 -v`
  - `go test -tags debug ./...`

## Style and Conventions

- Keep `gofmt` and `go vet` clean.
- Imports in three groups: stdlib, third-party, local (`ployz/...`).
- Use clear names and consistent initialisms (`API`, `CIDR`, `DNS`, `IP`, `WG`).
- Prefer `net/netip` types (`netip.Addr`, `netip.Prefix`, `netip.AddrPort`).
- Avoid style-only churn unrelated to the task.

## Practical Note

Function length alone is not the problem. Keep long functions that are linear and readable.
Extract helpers when they isolate state, name a domain concept, or improve testability.
