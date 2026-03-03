# Ployz Architecture

This repo now uses a single crate with one main `src/` tree and two binaries.

## Runtime split

- `ployzd` is the rootless control plane.
- Data plane concerns (WireGuard, service lifecycle, routing hooks) are adapter-driven and treated as durable state.
- Normal daemon restart should stop control flow, not destroy data plane resources.

## Binaries

- `src/bin/ployzd.rs`: daemon/control-plane entrypoint.
- `src/bin/ployz.rs`: operator CLI entrypoint.

## Source layout

```text
src/
  lib.rs
  config.rs
  error.rs

  domain/        # pure domain model and rules
  control/       # reconcile/convergence/hot-path lifecycle
  dataplane/     # ports/traits for side-effect adapters
  adapters/      # concrete IO implementations (memory/systemd/launchd/docker/...)
  transport/     # local daemon socket message types
```

## Control plane / data plane contract

- Control plane runs membership/convergence reconciliation.
- Adapters apply idempotent operations (`up`, `set_peers`, `start`, `stop`) and report health.
- `detach` means stop control activity and keep infra alive.
- `destroy` is explicit decommission.

## Current implementation notes

- Memory adapters are production test doubles and remain the default wired backend.
- Platform/profile resolution lives in `src/config.rs`.
- Lifecycle integration tests are in `tests/lifecycle.rs`.
