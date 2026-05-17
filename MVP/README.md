---
title: Iroh Bus MVP Foundation
status: active
created: 2026-05-17
---

# Iroh Bus MVP Foundation

This directory is the working specification for rebuilding the Ployz foundation
around a NATS-shaped bus over iroh, Kameo-owned local subsystems, iroh-docs
fact replication, SQLite projections, and the existing WireGuard, gateway, and
DNS data plane.

The goal is not to port the current architecture feature-for-feature. The goal
is to prove a cleaner version-1 foundation with end-to-end tests that show it
works, stays fast, and survives the failure cases that matter.

## Source Of Truth

- [MVP/overall-plan.md](overall-plan.md) is the overall strategy map that
  future slice plans should run against.
- [MVP/architecture.md](architecture.md) defines the target architecture.
- [MVP/e2e-proof-plan.md](e2e-proof-plan.md) defines the proof harness and
  acceptance tests.
- [MVP/primitive-decisions.md](primitive-decisions.md) is the maintainer-facing
  rationale for the main crates and architecture primitives.

## Local Proof Command

From `MVP/`, run `just test` to execute the local MVP gate:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo run -p mvp-e2e -- all`

This is intentionally kept inside `MVP/` while the rewrite remains isolated
from the existing codebase. Wire it into the repo-level CI only when the user
allows touching non-`MVP/` files.

Individual E2E scenarios are also runnable while iterating:

- `cargo run -p mvp-e2e -- bus-contract`
- `cargo run -p mvp-e2e -- actor-contract`
- `cargo run -p mvp-e2e -- authority-contract`
- `cargo run -p mvp-e2e -- scale`

Each scenario writes a JSON proof artifact under `MVP/target/mvp-e2e/`.

## Maintainer Notes

Use [MVP/primitive-decisions.md](primitive-decisions.md) as the living
maintainer map for the architecture Lego pieces. When a slice picks or rejects
an important primitive, record the short why/what-it-replaces/costs/revisit
entry there, and keep the slice report focused on proof and implementation
evidence.

## Non-Negotiables

- Keep the product model from [VISION.md](../VISION.md): explicit operations,
  no hidden control-plane reconcilers, and operator-visible failure.
- Keep gateway and DNS as separate process roles that keep serving last good
  snapshots when `ployzd` restarts.
- Keep Pingora as the gateway runtime unless a later plan proves a concrete
  reason to replace it.
- Treat SQLite as a rebuildable projection/cache, not cluster truth.
- Treat iroh endpoint identity as transport identity only. Authority comes from
  island grants and signed facts.
- Make the E2E harness a first-class deliverable. A slice is not done until it
  has proof.
- Treat code shape as a success metric. The new foundation must let us express
  real business behavior with far less substrate glue than the previous
  codebase.
- Keep new implementation work isolated under `MVP/` until the foundation has
  earned migration into the existing codebase.
