# AGENTS.md

Practical guidance for coding agents working in this repository.

## Scope

- Repository: `ployz`
- Language: Go
- Module: `ployz`
- Go version: `1.25.5` (from `go.mod`)
- Primary entrypoint: `cmd/ployz`

## Ground Rules

- Prefer small, focused changes.
- Match existing patterns before introducing new abstractions.
- Preserve cross-platform behavior (`linux`, `darwin`, and stubs).
- Do not silently change CLI behavior without updating command output/help text.
- Keep the store-driven/reactive model intact where applicable.

## Build, Lint, and Test Commands

These are the canonical commands from `justfile`.

### Build

- `just build`
- Equivalent: `go build -o bin/ployz ./cmd/ployz`

### Run CLI locally

- `just run <args>`
- Example: `just run machine ls --network default`
- Equivalent: `go run ./cmd/ployz <args>`

### Test

- `just test`
- Equivalent: `go test ./...`

### Lint

- `just lint`
- Equivalent: `go vet ./...`

### Dependency tidy

- `just tidy`
- Equivalent: `go mod tidy`

### Clean artifacts

- `just clean`

### Remote bootstrap/deploy helpers

- `just bootstrap <targets>`
- `just deploy-linux <targets>`

## Running a Single Test (Important)

Use `go test` directly for targeted runs.

- Single test in one package:
  - `go test ./internal/machine -run '^TestName$' -count=1 -v`
- Single subtest:
  - `go test ./internal/machine -run '^TestName$/subcase$' -count=1 -v`
- Same test name across all packages:
  - `go test ./... -run 'TestName' -count=1`
- Package-only quick compile+test:
  - `go test ./cmd/ployz`

Notes:

- `-count=1` avoids cached results while iterating.
- There are currently few/no test files, so `go test` is often used as a compile check.

## Architecture Snapshot

- `cmd/ployz/`: Cobra CLI commands.
- `internal/machine/`: machine runtime, WireGuard, Corrosion, membership, reconcile.
- `internal/agent/`, `pkg/network/`, `pkg/runtime/`: store/reactive orchestration model.
- Source of truth principle: write desired state to store, subscribers reconcile dataplane.

## Style and Conventions

### Formatting

- Always use Go standard formatting (`gofmt`-compatible output).
- Keep code go-vet clean.
- Avoid manual alignment or style-only churn in unrelated lines.

### Imports

- Group imports in this order:
  1) standard library
  2) third-party modules
  3) local module imports (`ployz/...`)
- Use aliases only when needed for name collisions or clarity.

### Naming

- Exported identifiers: `PascalCase`.
- Unexported identifiers: `camelCase`.
- Package names: short, lowercase, no underscores.
- Keep initialisms consistent with existing code: `API`, `CIDR`, `DNS`, `IP`, `WG`.
- Command constructors follow existing pattern: `machineAddCmd`, `machineListCmd`, etc.

### Types and Data Modeling

- Prefer `net/netip` types (`netip.Addr`, `netip.Prefix`, `netip.AddrPort`) for addresses.
- Keep struct field names explicit in literals for readability.
- Keep config normalization centralized (`NormalizeConfig`).
- For persisted machine state, use existing SQLite-backed flow in `internal/machine/state.go`.

### Error Handling

- Return errors; do not panic in normal control flow.
- Wrap lower-level errors with context using `%w`.
  - Example: `fmt.Errorf("parse endpoint: %w", err)`
- Error messages should be concise and actionable.
- Use `errors.Is` / `errors.As` for sentinel/typed error checks.
- Validate user/external input early (`strings.TrimSpace`, parse checks).

### Control Flow

- Prefer early returns to reduce nesting.
- Keep functions focused; extract helpers when logic branches grow.
- Keep side effects explicit (Docker/network/SSH calls should be obvious from function body).

### Context and Concurrency

- Pass `context.Context` as the first parameter where operations can block or call I/O.
- Respect context cancellation/timeouts in remote/network operations.
- Avoid introducing goroutines unless lifecycle/cancellation is clearly handled.

### Platform-Specific Code

- Keep OS-specific behavior behind build-tagged files:
  - `*_linux.go`
  - `*_darwin.go`
  - `*_stub.go`
- Maintain feature parity where feasible, and explicit errors where not supported.

### CLI Output and UX

- Keep output stable and parse-friendly.
- Add fields in a backward-compatible way when possible.
- If changing flags/behavior, update help text and related command docs/comments.

## Testing Guidance

- Prefer table-driven tests for parsing, normalization, and reconciliation logic.
- Cover success path and at least one failure path per public function change.
- For network/SSH/Docker-heavy paths, isolate pure logic into unit-testable helpers.
- Avoid brittle timing in tests; use deterministic polling/timeouts where unavoidable.

## Change Hygiene for Agents

- Keep unrelated refactors out of feature/fix patches.
- If you add dependencies, run `just tidy`.
- Before finishing, run at least:
  - `just test`
  - `just build`

## Cursor and Copilot Rules

Checked locations:

- `.cursor/rules/`
- `.cursorrules`
- `.github/copilot-instructions.md`

Current status:

- No Cursor or Copilot instruction files were found in this repository.

If those files are added later, merge their directives into this document.
