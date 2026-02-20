# AGENTS.md

Practical guidance for coding agents working in this repository.

## Scope

- Language: Go
- Module: `ployz` (see `go.mod` for version)
- Build/run/test commands: see `justfile`

## Ground Rules

- Prefer small, focused changes.
- Match existing patterns before introducing new abstractions.
- Preserve cross-platform behavior (`linux`, `darwin`, and stubs).
- Do not silently change CLI behavior without updating command output/help text.
- Before finishing, run at least `just test` and `just build`.
- If you add dependencies, run `just tidy`.

## Architecture

Ployz is a machine network control plane with three layers:

- **Daemon (`cmd/ployzd`)**: always-on per-machine process. Owns local runtime convergence (WireGuard, Docker networking, Corrosion). Exposes a typed API over unix socket.
- **SDK (`pkg/sdk`)**: client library for multi-machine choreography (bootstrap, join, remove). All cluster workflows live here.
- **CLI (`cmd/ployz`)**: thin UX shell over the SDK. No direct runtime mutations.

### Key architectural rules

- **Imperative setup, event-driven convergence.** Standing up infrastructure (WG interface, Docker network, firewall, Corrosion) is imperative — runs once, succeeds or fails. Only peer tracking is a continuous event-driven loop.
- **Data plane does not depend on control plane.** Peer convergence must keep running even if setup/teardown is broken.
- **Typed Corrosion subscriptions.** Every hot-path table driving convergence gets a typed `Subscribe<Table>` API in the registry layer. No raw SQL or Corrosion protocol details leak to consumers.
- **SDK always goes through daemon.** No direct Corrosion access from SDK. Daemon is the single writer to local state.
- **Health is reporting, not auto-fix.** Daemon reports per-component health via `GetStatus`. `machine doctor` surfaces problems. Operator decides what to fix.


## Package Layout

```
cmd/ployz/       CLI (thin over pkg/sdk)
cmd/ployzd/      daemon entrypoint
pkg/sdk/         client SDK (workflows, daemon client, types)
internal/daemon/ daemon internals (server, supervisor, reconcile)
internal/machine/ machine runtime, registry, platform ops, IPAM, remote
```

Explore the tree for current state — this is actively being restructured.

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
- Run single tests with: `go test ./path/to/pkg -run '^TestName$' -count=1 -v`
