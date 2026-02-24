# Architecture Migration: Why and What

## The Problem

Ployz is a machine network control plane — it manages WireGuard tunnels, Docker networks, and Corrosion (a distributed SQLite) across a fleet of machines. The daemon runs convergence loops that reconcile desired state against reality.

The codebase works, but it can't be tested at scale. Specifically:

- **Hidden constructors in hot loops.** `network.New()` (creates a Docker client) is called inside `engine.go`'s worker loop and `worker.go`'s `Run()` method. `registry.New()` (creates a Corrosion HTTP client) is created inline in every controlplane method. These can't be swapped for fakes.

- **No interfaces at adapter boundaries.** The reconciliation loop directly imports the Corrosion registry package. The controller directly imports the WireGuard and Docker packages. There's no seam to inject test doubles.

- **SQLite called directly from core logic.** `loadState()`/`saveState()` in `state.go` open a SQLite database directly. No way to substitute an in-memory store.

- **Flat package layout doesn't match dependency direction.** `machine/network/` contains both pure logic (config normalization, peer diffing) and infrastructure calls (Docker, WireGuard, Corrosion HTTP, SQLite). `coordination/registry/` mixes shared types (`MachineRow`) with Corrosion-specific HTTP transport. `runtime/` is a grab-bag.

The result: to test anything, you need a real Docker daemon, a real WireGuard interface, and a running Corrosion instance. There are almost no tests.

## The Goal

**Chaos testing at scale.** Simulate 200 nodes in a single test process — inject failures at every layer (Docker ops, WireGuard device, Corrosion replication, network partitions) and verify convergence. All in-memory, no real infrastructure.

This requires every external dependency to be behind an interface that can be replaced with a fake.

## The Migration

One commit does two things:

### 1. Move files to a clean layout

Separate core logic from adapters, following the dependency rule: core packages never import adapter packages.

```
internal/
  network/        Pure types, config, peer logic, controller, state struct
  reconcile/      Convergence loop (watches Corrosion, reconciles peers)
  engine/         Worker pool (starts/stops network controllers)
  adapter/
    corrosion/    Corrosion HTTP client, subscriptions, container management
    docker/       Docker API (networks, iptables)
    wireguard/    WireGuard device management
    sqlite/       Local state persistence (daemon spec store)
    platform/     CIDR utilities
  controlplane/   gRPC API, manager, proxy, protobuf
  remote/         SSH + install scripts (unchanged)
  logging/        slog setup (unchanged)
```

### 2. Extract interfaces and inject dependencies

Three key injection points:

- **`reconcile.Worker`** gets `Registry` and `PeerReconciler` interfaces instead of creating `registry.New()` and `network.New()` in its `Run()` method.

- **`engine.Engine`** gets a `NetworkControllerFactory` instead of calling `netctrl.New()` in its worker loop.

- **`network.Controller`** gets a `RegistryFactory` and `StateStore` instead of calling `registry.New()` inline in every method and opening SQLite directly.

Shared types (`MachineRow`, `HeartbeatRow`, `ChangeKind`, etc.) move from the `registry` package into `network/` so they're available to both core logic and adapters without circular imports.

### What doesn't change

No logic changes. Same behavior. The controlplane manager wires the real implementations at startup. The only difference is indirection through interfaces — which makes every layer independently testable.
