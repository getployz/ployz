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
- **All external dependencies are injected interfaces.** `Controller` holds no concrete adapter types. Docker, Corrosion, WireGuard, state persistence, and clock are all injected via interfaces defined in `mesh/ports.go`. Platform-specific `New()` functions wire the concrete implementations.
- **Typed Corrosion subscriptions.** Every hot-path table driving convergence gets a typed `Subscribe<Table>` API in the registry layer. No raw SQL or Corrosion protocol details leak to consumers.
- **SDK always goes through daemon.** No direct Corrosion access from SDK. Daemon is the single writer to local state.
- **Health is reporting, not auto-fix.** Daemon reports per-component health via `GetStatus`. `machine doctor` surfaces problems. Operator decides what to fix.


## Package Layout

```
cmd/ployz/            CLI (thin over pkg/sdk)
cmd/ployzd/           daemon entrypoint
pkg/sdk/              client SDK (workflows, daemon client, types)
internal/mesh/        core types, interfaces (ports.go), pure logic, Controller
internal/reconcile/   reconciliation loop, health tracking, interfaces (ports.go)
internal/engine/      worker pool, lifecycle orchestration
internal/adapter/     all external system integrations:
  adapter/corrosion/    Corrosion HTTP client + subscriptions (implements reconcile.Registry)
  adapter/docker/       Docker Runtime (implements mesh.ContainerRuntime)
  adapter/wireguard/    WireGuard device management (implements PeerApplier)
  adapter/sqlite/       local state persistence (load/save)
  adapter/platform/     platform runtime ops (darwin/linux/stub)
  adapter/fake/         shared fake adapters for chaos/integration testing
    fake/leaf/            leaf fakes (stores, runtimes, platform/status)
    fake/cluster/         cluster-backed fakes and chaos topology controls
    fake/fault/           shared fault injector (FailOnce/FailAlways/SetHook)
internal/check/       build-tagged assertions (debug panics, release no-ops)
internal/daemon/      daemon internals (server, supervisor, proxy, protobuf)
internal/testkit/     shared high-level test composition helpers
  testkit/scenario/     multi-node manager + fake cluster wiring for integration tests
internal/remote/      SSH + remote install scripts
internal/logging/     slog configuration
internal/buildinfo/   version info
```

### Key files in `internal/mesh/`

| File | Purpose |
|------|---------|
| `ports.go` | All consumer-defined interfaces: `Clock`, `ContainerRuntime`, `CorrosionRuntime`, `StatusProber`, `StateStore`, `Registry`, `RegistryFactory` |
| `controller.go` | `Controller` struct (holds all injected dependencies), `Option` funcs, `Status` struct |
| `status.go` | Shared `Status()` method (platform-independent, delegates to `StatusProber`) |
| `management.go` | Pure functions: `ManagementIPFromPublicKey`, `ManagementIPFromWGKey`, `MigrateLegacyManagementAddr` |
| `docker_runtime.go` | Bridge wrappers: `adapter/docker.Runtime` → `ContainerRuntime`, `adapter/corrosion.Adapter` → `CorrosionRuntime` (build-tagged linux/darwin) |
| `service_linux.go` | Linux `New()`, `linuxStatusProber`, `linuxRuntimeOps` |
| `service_darwin.go` | Darwin `New()`, `darwinStatusProber`, `darwinRuntimeOps` |
| `service_stub.go` | Stub `New()` + `stubStatusProber` for unsupported platforms |
| `runtime_common.go` | Shared start/stop logic using `runtimeOps` + injected interfaces |

### Dependency injection flow

Platform-specific `New()` functions wire everything:

1. Create `adapter/docker.Runtime` (concrete Docker client)
2. Wrap it in `dockerContainerRuntime` (adapts to `ContainerRuntime` interface)
3. Wrap it in `corrosionRuntimeAdapter` (adapts to `CorrosionRuntime` interface)
4. Create platform-specific `StatusProber` (e.g. `linuxStatusProber`)
5. Set all fields on `Controller`

Tests inject fakes for any of these interfaces via `With*` options.

### Bridge layer pattern

Core packages define interfaces with their own types (`mesh.ContainerInfo`, `mesh.Mount`, etc.). Adapter packages define their own matching types (`docker.ContainerInfo`, `docker.Mount`). Build-tagged bridge files in `mesh/` (e.g. `docker_runtime.go`) contain thin wrappers that convert between the two, avoiding import cycles.

## Go! Tiger Style

This codebase follows [Go! Tiger Style](https://predixus.com) — a Go-specific adaptation of TigerBeetle's engineering discipline. The full audit checklist lives in `.claude/skills/tiger-audit/SKILL.md`. The principles below are integrated into the relevant sections of this document.

The three priorities, in order: **Safety, Performance, Developer Experience**.

Zero technical debt: solve problems right the first time. Don't allow potential issues to slip into production. Simplicity requires hard work and discipline — it's achieved through iteration, not first attempts.

## Core Rules

Non-negotiable architectural rules for all changes. These exist to keep the codebase testable, predictable, and safe to change.

### 1. Interfaces in consumer packages

Define interfaces where they're used, not where they're implemented. Place them in a `ports.go` file next to the consumer.

```go
// internal/reconcile/ports.go — the consumer defines what it needs
type Registry interface {
    ListMachineRows(ctx context.Context) ([]mesh.MachineRow, error)
    SubscribeMachines(ctx context.Context) ([]mesh.MachineRow, <-chan mesh.MachineChange, error)
}
```

The adapter (`adapter/corrosion/`) implements it without importing the consumer.

### 2. No side effects in core logic

Decision logic in `mesh/`, `reconcile/`, and `engine/` must be pure: data in, data out. No Docker calls, no HTTP, no WireGuard, no disk I/O, no `time.Now()`. Orchestration code in these packages may call injected interfaces (ports), but never imports adapter packages directly.

All external dependencies are abstracted behind interfaces in `mesh/ports.go`:
- `Clock` — time source (inject `RealClock{}` in production, fake in tests)
- `ContainerRuntime` — Docker/Podman container and network operations
- `CorrosionRuntime` — Corrosion container lifecycle (WriteConfig, Start, Stop)
- `StatusProber` — platform-specific infrastructure health checks
- `StateStore` — state persistence (SQLite in production)
- `Registry` / `RegistryFactory` — Corrosion data access

If a function needs the current time, use the injected `Clock`. If it needs to apply a change, call an injected interface or return a plan and let the caller apply it.

### 3. All infra calls in adapters

Every call to an external system (Corrosion HTTP API, Docker API, WireGuard kernel/userspace, SQLite, filesystem, SSH) lives in `internal/adapter/`. Core logic never imports adapter packages. Dependency direction is always inward.

**Bridge layer exception**: build-tagged files in `mesh/` (e.g. `docker_runtime.go`) may import adapter packages to wire concrete implementations into interface wrappers. These files contain only type conversion — no business logic. Platform-specific `New()` functions (`service_linux.go`, `service_darwin.go`) are the only code that creates adapter instances.

### 4. No hidden constructors in loops

Never call `New()` or create concrete dependencies inside `Run()` or hot loops. Inject dependencies at construction time so tests can substitute fakes.

```go
// Bad: hardcoded inside Run()
func (w *Worker) Run(ctx context.Context) error {
    reg := registry.New(addr, token)  // untestable
    ctrl, _ := mesh.New()              // untestable
    // ...
}

// Good: injected at construction
type Worker struct {
    Registry   Registry       // interface
    Reconciler PeerReconciler // interface
}
```

### 5. Persistence in adapters only

Struct definitions, validation, and serialization helpers live in core packages (e.g. `mesh/state.go`). All database/file I/O lives in adapter packages (e.g. `adapter/sqlite/state.go`).

### 6. Every core change has success + failure test

Any change to `mesh/`, `reconcile/`, or `engine/` must include at least one success-path and one failure-path test. Use table-driven tests for pure logic, fake-driven tests for orchestration.

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

### 11. Build-tagged assertions for programmer errors

Use `internal/check.Assert()` for preconditions, postconditions, and invariants that indicate **programmer errors** (wiring bugs, impossible states). These use build tags: panics under `//go:build debug`, no-ops in release.

Assertions are not a replacement for error handling. Errors handle runtime failures (bad input, network down). Assertions catch bugs that should never reach production.

```go
// Precondition: constructor wiring bug
func NewWorker(reg Registry, rec PeerReconciler) *Worker {
    check.Assert(reg != nil, "NewWorker: registry must not be nil")
    check.Assert(rec != nil, "NewWorker: reconciler must not be nil")
    return &Worker{Registry: reg, PeerReconciler: rec}
}

// Postcondition: derived value must be valid
ip, err := ManagementIPFromPublicKey(key)
if err != nil { return err }
check.Assert(ip.IsValid() && ip.Is6(), "management IP must be valid IPv6")

// Invariant: exhaustive switch
switch change.Kind {
case mesh.ChangeAdded: ...
case mesh.ChangeUpdated: ...
case mesh.ChangeDeleted: ...
default:
    check.Assert(false, "unknown change kind: "+string(change.Kind))
}
```

Where to assert:
- Constructor functions after options applied — all required deps non-nil
- Public method entry — preconditions that would cause nil derefs downstream
- After derivation — postconditions on computed values (valid IP, non-empty key)
- Switch on typed constants — default case asserts exhaustiveness
- Port casts — `0 < port && port <= 65535` before narrowing to uint16

### 12. Explicit capacity in `make()`

When allocating slices, maps, or channels, be explicit about capacity when the size is known or bounded.

```go
// Good: final size known from input
peers := make([]Peer, 0, len(rows))

// Good: approximate size known
users := make(map[string]User, expectedCount)

// Good: channel buffer as named constant
const subscriptionBufCap = 128
ch := make(chan MachineChange, subscriptionBufCap)

// Fine: size genuinely unknown, document why
var results []Result // unbounded: depends on external query
```

Don't pick arbitrary numbers when the size is genuinely unknown — that's worse than letting Go's growth strategy handle it.

### 13. Bounds on everything

All loops and queues must have fixed upper bounds. Reality has limits.

- Retry loops need max attempts. If a sibling function has `attempts < 3`, the retry loop next to it shouldn't be unbounded.
- `io.ReadAll` on untrusted input needs `io.LimitReader`.
- Maps and slices used as caches or trackers need eviction or a cap.
- `for {}` event loops are acceptable — they terminate via context cancellation. Document this explicitly if it isn't obvious.

## Style and Conventions

### Formatting

- Standard `gofmt` formatting. Keep `go vet` clean.
- Avoid style-only churn in unrelated lines.

### Function Length & Control Flow

Function length is a correlated smell, not the disease. The disease is deep nesting, multiple responsibilities, intertwined state, and difficulty reasoning about what happens when.

A 120-line function that reads like a script — sequential steps, shallow nesting, state flowing top to bottom — is fine. Don't split it into 6 helpers called exactly once just to hit a line count. That scatters a linear process and increases cognitive load.

A 60-line function with 4 levels of nesting, a switch inside a for inside a select, and 8 intermediate variables mutating through branches — that needs breaking up regardless of length.

The reasons to extract a helper: it isolates state, it names a domain concept (better than a comment), or it's independently testable. "It's too long" by itself is not a reason.

### Imports

Three groups in order: stdlib, third-party, local (`ployz/...`).

### Naming

- Exported: `PascalCase`. Unexported: `camelCase`. Packages: short, lowercase, no underscores.
- Keep initialisms consistent: `API`, `CIDR`, `DNS`, `IP`, `WG`.
- Name every magic number. Durations, buffer sizes, port numbers, retry counts — all get named constants. Same literal repeated in multiple locations must be defined once as `const`.
- Use consistent names across packages for the same concept. Don't call it `Management` in one package and `ManagementIP` in another.

### Error Handling

- Return errors, don't panic. Panics are for programmer bugs caught by assertions (see rule 11), not runtime failures.
- Wrap with context at package boundaries: `fmt.Errorf("parse endpoint: %w", err)`. Don't wrap mechanically at every level — that creates stutter.
- Use `errors.Is` / `errors.As` for sentinel/typed checks. Never classify errors by string matching (`strings.Contains(err.Error(), ...)`).
- Never silently discard errors with `_ = expr`. Either handle it, log it, or add a comment explaining why it's safe to ignore.
- All errors must be handled explicitly. Silent fallbacks (defaulting when an operation fails without diagnostic) are bugs.

### Context and Concurrency

- Pass `context.Context` as first parameter for blocking/IO operations.
- Respect cancellation in remote/network operations.
- Don't introduce goroutines unless lifecycle/cancellation is clearly handled.

### Platform-Specific Code

- OS-specific behavior behind build-tagged files: `*_linux.go`, `*_darwin.go`, `*_stub.go`.
- Explicit errors where a platform isn't supported.
- Each platform's `New()` wires concrete adapters into the `Controller` via interfaces.
- Platform-specific `StatusProber` and `runtimeOps` implementations live in platform files; shared `Status()` and `startRuntime()`/`stopRuntime()` live in common files.

### Types

- Prefer `net/netip` types (`netip.Addr`, `netip.Prefix`, `netip.AddrPort`).
- Keep struct field names explicit in literals.

## Testing

- Table-driven tests for parsing, normalization, reconciliation logic.
- Cover success + at least one failure path per public function change.
- Isolate pure logic from mesh/SSH/Docker dependencies for unit testability.
- **Inline stubs** for simple, single-test fakes live in `_test.go` files in the consumer package.
- **Shared fake adapters** (`adapter/fake/`) for multi-test or cross-package use: `fake.Clock`, `fake.StateStore`, `fake.ContainerRuntime`, `fake.Cluster`, `fake.Registry`, etc. All fakes embed `CallRecorder` for call assertion, support per-method error injection, and are thread-safe.
- Prefer the shared fault injector (`internal/adapter/fake/fault`) for new tests: use `FailOnce`, `FailAlways`, and `SetHook` on fake adapters before adding new per-method `...Err` fields.
- **Shared scenario testkit** (`internal/testkit/scenario`) for multi-node workflow tests. Use this when tests need real `supervisor` + `engine` + `reconcile` orchestration across nodes, but with fake adapters.
- `fake.Cluster` simulates a Corrosion gossip cluster with per-node state, configurable topology (latency, partitions, drop rates), and deterministic replication via `Tick()`/`Drain()`.
- Run single tests with: `go test ./path/to/pkg -run '^TestName$' -count=1 -v`
- Run tests with assertions enabled: `go test -tags debug ./...`

### Multi-node Scenario Testkit

Use `internal/testkit/scenario` as the default for SDK/daemon behavior tests that involve more than one machine.

- Build scenarios with `scenario.MustNew(t, t.Context(), scenario.Config{...})`.
- Access node-level handles via `s.Node("id")` (`Manager`, `PlatformOps`, stores, runtimes).
- Dynamically add/remove managed nodes with `s.AddNode("id")` and `s.RemoveNode("id")`.
- Use `s.Cluster` for low-level registry fault injection and topology controls.
- Use `s.SetLink`, `s.BlockLink`, `s.Partition`, `s.Heal`, `s.KillNode`, `s.RestartNode`, `s.Tick`, and `s.Drain()` for manual chaos control.
- Use `s.Snapshot("id")` for deterministic invariant checks after fault/topology transitions.
- When using `testing/synctest`, pair cluster drains with `synctest.Wait()` (prefer `t.Cleanup(synctest.Wait)` in setup).
- Keep `DataRootBase` unique per test to avoid accidental state collisions in `/tmp`.

### Constructor split (production vs tests)

- `supervisor.NewProduction(ctx, dataRoot)` is the production entrypoint; it wires sqlite/platform/corrosion dependencies.
- `supervisor.New(ctx, dataRoot, opts...)` is the pure constructor for injected dependencies and test composition.
- Avoid mixing production wiring into tests; prefer `scenario` or explicit dependency injection.

### Property-Based Testing (Fuzz)

Pure functions that parse, normalize, or derive values are candidates for Go's native fuzzer (`testing.F`). Property-based testing complements table-driven tests — tables verify specific points, fuzz tests verify properties across the input space.

Key properties to test:
- **Idempotency**: `Normalize(Normalize(x)) == Normalize(x)`
- **Inverse operations**: encode/decode, serialize/deserialize round-trip to identity
- **Invariants**: result always within expected bounds (valid IP, prefix within CIDR, interface name <= 15 chars)
- **Determinism**: same input always produces same output

Assertions (rule 11) are a force multiplier for fuzzing — they catch invariant violations the fuzzer wouldn't know to check for.
