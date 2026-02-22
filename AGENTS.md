# AGENTS.md

Practical guidance for coding agents working in this repository.

## Scope

- Language: Go
- Module: `ployz` (see `go.mod` for version)
- Build/run/test commands: see `justfile`

## Ground Rules

- Prefer small, focused changes.
- Be aggressive about future TODOs: when scope grows, ship a minimal step and leave a clear follow-up TODO for additional functionality instead of trying to do everything in one pass.
- Match existing patterns before introducing new abstractions.
- Preserve cross-platform behavior (`linux`, `darwin`, and stubs).
- Do not silently change CLI behavior without updating command output/help text.
- Before finishing, run at least `just test` and `just build`.
- If you add dependencies, run `just tidy`.

## Architecture

Ployz is a machine network control plane with three layers:

- **Daemon (`cmd/ployzd`)**: single process. Owns desired state, exposes typed API over unix socket, and runs convergence loops in-process via the runtime engine.
- **SDK (`pkg/sdk`)**: client library for multi-machine choreography (bootstrap, join, remove). All cluster workflows live here.
- **CLI (`cmd/ployz`)**: thin UX shell over the SDK. No direct runtime mutations.

### Key architectural rules

- **Imperative setup, event-driven convergence.** Standing up infrastructure (WG interface, Docker network, firewall, Corrosion) is imperative — runs once, succeeds or fails. Peer tracking stays continuous in runtime loops within the daemon process.
- **Typed Corrosion subscriptions.** Every hot-path table driving convergence gets a typed `Subscribe<Table>` API in the registry layer. No raw SQL or Corrosion protocol details leak to consumers.
- **SDK always goes through daemon.** No direct Corrosion access from SDK. Daemon is the single writer to local state.
- **Health is reporting, not auto-fix.** Daemon reports per-component health via `GetStatus`. `machine doctor` surfaces problems. Operator decides what to fix.


## Package Layout

```
cmd/ployz/            CLI (thin over pkg/sdk)
cmd/ployzd/           daemon entrypoint
pkg/sdk/              client SDK (workflows, daemon client, types)
internal/network/     core types (MachineRow, Peer, Config) + pure logic (diff, peers, config)
internal/reconcile/   reconciliation loop, health tracking, interfaces (ports.go)
internal/engine/      worker pool, lifecycle orchestration
internal/adapter/     all external system integrations:
  adapter/corrosion/    Corrosion HTTP client + subscriptions (implements reconcile.Registry)
  adapter/docker/       Docker API (networks, containers, iptables)
  adapter/wireguard/    WireGuard device management (implements PeerApplier)
  adapter/sqlite/       local state persistence (load/save)
  adapter/platform/     platform runtime ops (darwin/linux/stub)
internal/daemon/      daemon internals (server, supervisor, proxy, protobuf)
internal/remote/      SSH + remote install scripts
internal/logging/     slog configuration
internal/buildinfo/   version info
```

This layout is being migrated to. Code still in old paths (`internal/machine/network/`, `internal/coordination/`, `internal/runtime/`, `internal/platform/`) must follow the same Core Rules — apply the principles in place until the code is moved.

## Core Rules

Non-negotiable architectural rules for all changes. These exist to keep the codebase testable, predictable, and safe to change.

### 1. Interfaces in consumer packages

Define interfaces where they're used, not where they're implemented. Place them in a `ports.go` file next to the consumer.

```go
// internal/reconcile/ports.go — the consumer defines what it needs
type Registry interface {
    ListMachineRows(ctx context.Context) ([]network.MachineRow, error)
    SubscribeMachines(ctx context.Context) ([]network.MachineRow, <-chan network.MachineChange, error)
}
```

The adapter (`adapter/corrosion/`) implements it without importing the consumer.

### 2. No side effects in core logic

Decision logic in `network/`, `reconcile/`, and `engine/` must be pure: data in, data out. No Docker calls, no HTTP, no WireGuard, no disk I/O, no `time.Now()`. Orchestration code in these packages may call injected interfaces (ports), but never imports adapter packages directly.

If a function needs the current time, accept it as a parameter or inject a clock. If it needs to apply a change, return a plan and let the caller apply it.

### 3. All infra calls in adapters

Every call to an external system (Corrosion HTTP API, Docker API, WireGuard kernel/userspace, SQLite, filesystem, SSH) lives in `internal/adapter/`. Core logic never imports adapter packages. Dependency direction is always inward.

### 4. No hidden constructors in loops

Never call `New()` or create concrete dependencies inside `Run()` or hot loops. Inject dependencies at construction time so tests can substitute fakes.

```go
// Bad: hardcoded inside Run()
func (w *Worker) Run(ctx context.Context) error {
    reg := registry.New(addr, token)  // untestable
    ctrl, _ := network.New()          // untestable
    // ...
}

// Good: injected at construction
type Worker struct {
    Registry   Registry       // interface
    Reconciler PeerReconciler // interface
}
```

### 5. Persistence in adapters only

Struct definitions, validation, and serialization helpers live in core packages (e.g. `network/state.go`). All database/file I/O lives in adapter packages (e.g. `adapter/sqlite/state.go`).

### 6. Every core change has success + failure test

Any change to `network/`, `reconcile/`, or `engine/` must include at least one success-path and one failure-path test. Use table-driven tests for pure logic, fake-driven tests for orchestration.

### 7. Error wrapping with context

Always wrap errors with what was being attempted. Use sentinel errors and `errors.Is`/`errors.As` for policy decisions.

```go
return fmt.Errorf("reconcile peers for %s: %w", network, err)
```

### 8. Context first, cancellation respected

Pass `context.Context` as first parameter for any blocking or I/O operation. Check `ctx.Done()` in loops. No goroutines without clear lifecycle ownership.

### 9. No `time.Now()` or `time.Sleep()` in core logic

Core packages must not call `time.Now()` or `time.Sleep()` directly. Accept timestamps as parameters or inject a clock so tests are deterministic and fast.

### 10. PR gate: `just test` + `just build` always green

Every change must pass `just test` and `just build` before merge. No exceptions. If a test is flaky, fix or delete it — never skip it.

## Style and Conventions

### Formatting

- Standard `gofmt` formatting. Keep `go vet` clean.
- Avoid style-only churn in unrelated lines.

### Imports

Three groups in order: stdlib, third-party, local (`ployz/...`).

### Naming

- Exported: `PascalCase`. Unexported: `camelCase`. Packages: short, lowercase, no underscores.
- Keep initialisms consistent: `API`, `CIDR`, `DNS`, `IP`, `WG`.

### Error Handling

- Return errors, don't panic.
- Wrap with context: `fmt.Errorf("parse endpoint: %w", err)`.
- Use `errors.Is` / `errors.As` for sentinel/typed checks.

### Context and Concurrency

- Pass `context.Context` as first parameter for blocking/IO operations.
- Respect cancellation in remote/network operations.
- Don't introduce goroutines unless lifecycle/cancellation is clearly handled.

### Platform-Specific Code

- OS-specific behavior behind build-tagged files: `*_linux.go`, `*_darwin.go`, `*_stub.go`.
- Explicit errors where a platform isn't supported.

### Types

- Prefer `net/netip` types (`netip.Addr`, `netip.Prefix`, `netip.AddrPort`).
- Keep struct field names explicit in literals.

## Testing

- Table-driven tests for parsing, normalization, reconciliation logic.
- Cover success + at least one failure path per public function change.
- Isolate pure logic from network/SSH/Docker dependencies for unit testability.
- Fakes live in `_test.go` files in the consumer package (e.g. `reconcile/reconcile_test.go` contains `fakeRegistry`). Never in a shared `testutil` or `mocks` package.
- Run single tests with: `go test ./path/to/pkg -run '^TestName$' -count=1 -v`
