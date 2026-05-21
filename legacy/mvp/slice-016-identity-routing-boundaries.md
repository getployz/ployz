---
title: Slice 016 Identity And Routing Boundaries Result
status: completed
created: 2026-05-18
plan: MVP/slice-016-identity-routing-boundaries-plan.md
---

# Slice 016 Identity And Routing Boundaries Result

## What Shipped

- Added `mvp-identity` with shared `NodeId` and `VisibleNodes`.
- Replaced lease, ACME, deploy, mesh, and E2E visible-node wrappers with the
  shared type.
- Deleted `mvp_deploy::DeployNodeId`; deploy placement, capacity replies,
  cleanup status, and wire payloads now use the shared `NodeId`.
- Replaced raw WireGuard peer routing fields with `WireGuardOverlayCidr` and
  `IrohEndpointId`.
- Deferred `ipnet`; `WireGuardOverlayCidr` is currently a typed `/128` host
  route derived from `WireGuardOverlayIp`.

## Implementation Note

The plan originally placed the canonical identity type in `mvp_projection`.
During implementation that created a dependency cycle: `mvp_projection` already
depends on lease and ACME fact payloads, while lease/ACME need visible-node
evidence. The resolved shape is a tiny lower-level `mvp-identity` crate used
directly by crates that need node identity.

This preserves the important invariant: there is one real node identity type
and one real visible-node evidence type across the MVP.

## Semantic-Leverage Check

Before the slice, the MVP had:

- `mvp_deploy::DeployNodeId`,
- `mvp_lease::VisibleNode`,
- `mvp_deploy::VisibleNode`,
- `mvp_mesh::VisibleNodes`,
- raw string WireGuard peer `allowed_ip` and `endpoint` fields.

After the slice, the only surviving visible-node definition is
`mvp_identity::VisibleNodes`, and there are no surviving `DeployNodeId`,
`pub struct VisibleNode`, `allowed_ip: String`, or `endpoint: String`
production definitions.

This means the next node-facing command can report reachability, place work,
route traffic, or enforce authorization without choosing between parallel node
identity wrappers.

## Verification

Focused checks run during implementation:

```text
cargo test -p mvp-projection -p mvp-lease -p mvp-acme -p mvp-mesh --lib
cargo test -p mvp-deploy --lib
cargo run -p mvp-e2e -- deploy-commit-drain-contract
cargo test -p mvp-mesh --lib
cargo run -p mvp-e2e -- membership-wireguard-contract
```

Full gates after implementation and simplification:

```text
cargo test
cargo clippy -p mvp-projection -p mvp-lease -p mvp-deploy -p mvp-mesh -p mvp-e2e --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
git diff --check
rg -n "DeployNodeId|pub struct VisibleNode|allowed_ip: String|endpoint: String|mvp_projection::NodeId|mvp_projection::VisibleNodes|mvp_mesh::VisibleNodes|\\.as_set\\(" MVP -S --glob '!slice-016-identity-routing-boundaries-plan.md' --glob '!slice-016-identity-routing-boundaries.md'
```

The final semantic grep only found the intentional new shared
`mvp_identity::VisibleNodes` definition and a documentation note saying
`DeployNodeId` was removed.
