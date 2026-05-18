---
title: Slice 016 Identity And Routing Boundaries Plan
status: completed
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-014-membership-wireguard-plan.md
  - MVP/slice-015-docs-backed-acme-http01-plan.md
---

# Slice 016 Identity And Routing Boundaries Plan

## Problem Frame

The MVP now proves the main substrate canaries: bus semantics, authority
islands, bridge behavior, docs-backed facts, projections, deploy
commit-before-drain, ACME HTTP-01 serving, coordinator-down serving, wire
HTTP/DNS roles, and membership plus last-applied mesh traffic.

The next product command should not build on top of competing identity shapes.
Today the MVP still has several ways to say "node visible at decision time" and
two ways to represent the same node identity:

- `mvp_identity::NodeId`,
- `mvp_deploy::DeployNodeId`,
- `mvp_lease::VisibleNode`,
- `mvp_deploy::VisibleNode`,
- `mvp_mesh::VisibleNodes`.

That is exactly the kind of semantic accretion this rewrite exists to avoid.
Before adding another node-facing command such as graceful machine remove,
deploy restart recovery, volume transfer, or migrate, make node identity and
routing fields have one obvious representation.

The single proof target for this slice:

```text
MVP node identity has one canonical type across projection, deploy, lease, and
mesh; visible-node evidence has one canonical set type; WireGuard applied
snapshots use typed routing fields; all existing product proofs still pass.
```

This is a simplicity and semantic-leverage slice, not a new product-feature
slice. Its value is that the next feature author cannot accidentally choose the
wrong node type or add another visible-node wrapper.

## Requirements Trace

- `VISION.md`: domain identity must be explicit and operations must expose
  visible preconditions and verification hooks. A command result's visible-node
  evidence is part of that product surface.
- `MVP/overall-plan.md`: the rewrite must prove semantic leverage and simplicity,
  not just E2E behavior. Future slices should keep business logic from learning
  substrate plumbing or duplicate representations.
- `MVP/architecture.md`: transport identity is not authority, and durable facts
  carry typed truth below process wiring.
- `MVP/e2e-proof-plan.md`: membership, deploy, leases, serving, and scale proofs
  already depend on visible-node evidence and node-routed actions.
- `MVP/primitive-decisions.md`: identity cleanup is already scheduled as a known
  design gap before more node-facing commands.
- `MVP/slice-014-membership-wireguard-plan.md`: membership facts and WireGuard
  planning are the first place where node identity, iroh endpoint identity, and
  WireGuard routing identity all meet.
- `MVP/slice-015-docs-backed-acme-http01-plan.md`: ACME kept visible nodes as
  evidence, not as quorum. This slice preserves that semantics while removing
  the lease-local string wrapper.

## Scope

In scope:

- Make `mvp_identity::NodeId` the single canonical MVP node identity.
- Delete `mvp_deploy::DeployNodeId` and update deploy domain, coordinator,
  wire payloads, errors, tests, and E2E fixtures to use `NodeId`.
- Expose one canonical `VisibleNodes` type from `mvp_identity`, backed by
  `BTreeSet<NodeId>`.
- Replace lease, deploy, and mesh visible-node wrappers with the canonical
  `VisibleNodes`.
- Type `WireGuardPeer.allowed_ip` as `WireGuardOverlayCidr`.
- Type `WireGuardPeer.endpoint` as `IrohEndpointId`, matching the membership
  fact source used by full-mesh planning.
- Update process-role mesh harness code to read typed peer fields instead of
  raw strings.
- Update maintainer docs in `MVP/primitive-decisions.md` to record that the
  identity gap was resolved and to document any new crate adopted for routing
  types.
- Keep all changes self-contained under `MVP/`.

Out of scope:

- New product behavior such as graceful machine remove, workload drain, volume
  transfer, deploy crash recovery, or machine reinvite.
- Introducing `mvp-commands` or `PhasedCommand`; this slice removes duplicate
  identity before adding another phase-shaped command.
- A broad string-newtype macro cleanup across the MVP.
- Replacing the MVP-local `IrohEndpointId` string newtype with
  `iroh::EndpointId`; current harness identities are not guaranteed to be real
  iroh public keys.
- Production WireGuard adapter work through `defguard_wireguard_rs` or kernel
  interfaces.
- Changing code outside `MVP/`.

## Crate Scout

Checked before planning:

- `ipnet` 2.12 provides `IpNet`, `Ipv4Net`, and `Ipv6Net` network-prefix types
  aligned with Rust's standard IP address types, and has a `serde` feature:
  <https://docs.rs/ipnet/latest/ipnet/>. This is a good fit for typed
  WireGuard allowed IPs because the field is a network prefix, not a display
  string.
- `iroh::EndpointId` is an alias for the peer public key and identifies an iroh
  endpoint cryptographically:
  <https://docs.rs/iroh/latest/iroh/type.EndpointId.html>. This is the right
  eventual production type once the membership join path carries real iroh
  endpoint keys everywhere. It is too strict for this mechanical MVP cleanup
  because current fixtures still use readable synthetic endpoint ids.
- `nutype` generates validated newtypes and preserves invariants through serde:
  <https://docs.rs/nutype/latest/nutype/>. It may be useful for a later
  boilerplate cleanup, but adopting a proc macro while doing identity
  unification would mix two refactors.
- `derive_more` can derive common newtype traits such as `Display`, `From`, and
  conversions:
  <https://docs.rs/derive_more/latest/derive_more/>. It reduces boilerplate but
  does not by itself solve the identity split, so defer it.

Decision for this slice:

- Add `ipnet` with `serde` to `mvp-mesh` only if the implementation confirms it
  makes `WireGuardOverlayCidr` simpler than a local parser.
- Keep `IrohEndpointId` as an MVP-local newtype for now and make the peer field
  typed with it.
- Do not introduce a new macro or validation crate in this slice.

## Design Decisions

### Canonical Node Identity

`mvp_identity::NodeId` becomes the only node id type in the MVP.

The identity crate owns the canonical type because lease and ACME command
contexts need visible-node evidence while projection depends on lease and ACME
fact payloads. Putting the type in projection creates a dependency cycle.

`mvp_deploy::DeployNodeId` should be deleted, not type-aliased. A type alias
would leave two names in the public surface and preserve the choice a future
contributor should not have to make.

### Canonical Visible Nodes

Visible nodes are command decision evidence, not deploy-specific, lease-specific,
or mesh-specific state. The canonical type should be:

```text
mvp_identity::VisibleNodes(BTreeSet<NodeId>)
```

It should expose the same minimal surface the current wrappers need:

- `new`,
- `iter`,
- `len`,
- `is_empty`.

Deploy state can record `VisibleNodes` directly. Lease command context should
store `VisibleNodes` directly. Mesh join/tombstone command results should
return `VisibleNodes` directly.

### WireGuard Routing Fields

`WireGuardPeer.allowed_ip` and `WireGuardPeer.endpoint` participate in routing
and snapshot equality, so raw strings are the wrong surface.

Use:

```text
allowed_ip: WireGuardOverlayCidr
endpoint: IrohEndpointId
```

`WireGuardOverlayCidr` should be a domain type that can be constructed from
`WireGuardOverlayIp` as a `/128` host route for the current full-mesh MVP.
If `ipnet` is adopted, back it with `ipnet::Ipv6Net`. Otherwise, keep it as a
small local wrapper with no ad hoc parsing beyond what the standard library
already provides.

The process-role harness currently injects loopback socket addresses into the
peer endpoint field to simulate data-plane reachability. Keep that as a
harness-only stand-in and make the conversion explicit. Do not broaden the
production type just to make the fixture look natural.

## Implementation Units

### Unit 1: Shared Identity Types

Files:

- `MVP/projection/src/facts.rs`
- `MVP/projection/src/lib.rs`
- `MVP/lease/src/lib.rs`
- `MVP/mesh/src/domain.rs`
- `MVP/mesh/src/invite.rs`

Work:

- Add and export `VisibleNodes` from `mvp_identity`.
- Replace `mvp_lease::VisibleNode` and `mvp_mesh::VisibleNodes` with the shared
  type.
- Update lease and mesh tests to construct `VisibleNodes` from `NodeId`.

Tests:

- `cargo test -p mvp-projection --lib`
- `cargo test -p mvp-lease --lib`
- `cargo test -p mvp-mesh --lib`

### Unit 2: Deploy Node Identity Refactor

Files:

- `MVP/deploy/src/domain.rs`
- `MVP/deploy/src/error.rs`
- `MVP/deploy/src/coordinator.rs`
- `MVP/deploy/src/wire.rs`
- `MVP/deploy/src/state_machine.rs`
- `MVP/deploy/src/lib.rs`
- `MVP/deploy/src/tests.rs`
- `MVP/e2e/src/deploy_commit_drain_contract.rs`

Work:

- Replace `DeployNodeId` with `mvp_identity::NodeId`.
- Delete the `DeployNodeId` type and remove it from `mvp_deploy` exports.
- Update subject construction to use `NodeId::as_str()`.
- Keep deploy error variants structured; only the node-id type changes.
- Keep deploy wire payloads semantically identical except for the canonical
  serialized node-id type.

Tests:

- `cargo test -p mvp-deploy --lib`
- `cargo run -p mvp-e2e -- deploy-commit-drain-contract`

### Unit 3: Typed Mesh Peer Routing

Files:

- `MVP/mesh/Cargo.toml`
- `MVP/mesh/src/domain.rs`
- `MVP/mesh/src/wireguard.rs`
- `MVP/mesh/src/snapshot.rs`
- `MVP/mesh/src/actor.rs`
- `MVP/e2e/src/membership_wireguard_contract.rs`
- `MVP/e2e/src/process_role_harness.rs`

Work:

- Add `WireGuardOverlayCidr`.
- Change `WireGuardPeer.allowed_ip` to `WireGuardOverlayCidr`.
- Change `WireGuardPeer.endpoint` to `IrohEndpointId`.
- Update snapshot serialization/deserialization and harness parsing.
- Preserve the proof that outbound service traffic resolves targets from the
  applied snapshot instead of caller-supplied addresses.

Tests:

- `cargo test -p mvp-mesh --lib`
- `cargo run -p mvp-e2e -- membership-wireguard-contract`

### Unit 4: Proof And Documentation

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/slice-016-identity-routing-boundaries.md` after implementation

Work:

- Move the identity-cleanup item out of "Documented Design Gaps" after the
  implementation lands.
- Add a "Changed Since Last Slice" note describing the single `NodeId`, single
  `VisibleNodes`, and typed WireGuard peer fields.
- Record whether `ipnet` was adopted or deferred.
- Add a slice result report with the exact before/after semantic-leverage check.

Tests:

- `rg "DeployNodeId|pub struct VisibleNode|allowed_ip: String|endpoint: String" MVP`
  should show no surviving production definitions.
- `cargo clippy -p mvp-projection -p mvp-lease -p mvp-deploy -p mvp-mesh -p mvp-e2e --all-targets -- -D warnings`
- `MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

## Proof Criteria

The slice is complete when:

- There is exactly one MVP node identity type: `mvp_identity::NodeId`.
- There is exactly one visible-node evidence type:
  `mvp_identity::VisibleNodes`.
- Deploy, lease, mesh, and E2E code all use those shared types.
- WireGuard applied snapshots no longer expose peer allowed IPs or endpoints as
  raw `String` fields.
- Existing deploy, lease/ACME, membership/WireGuard, and full E2E proofs pass.
- Maintainer docs explain why no macro/newtype crate was adopted for the broader
  string-newtype boilerplate yet.

## Semantic-Leverage Check

Before implementation, record the grep baseline:

```text
rg "DeployNodeId|pub struct VisibleNode|pub struct VisibleNodes|allowed_ip: String|endpoint: String" MVP
```

After implementation, the only surviving visible-node definition should be the
canonical exported one in `mvp_identity`, and the string routing fields should
be gone.

This is the leverage metric for this slice: future node-facing business logic
gets one node id and one visible-node evidence type. If a future command needs
to report reachability, place work, route traffic, or enforce authorization, it
does not choose between deploy, lease, mesh, or projection-local node shapes.

## Review Risks

- This is a mechanical refactor across several crates. The main risk is missing
  one conversion at a serialization or E2E harness boundary.
- `IrohEndpointId` remains an MVP-local string newtype. Review should not
  mistake this for real iroh key validation; that belongs to the future join
  transport slice.
- The mesh process-role harness still uses loopback sockets as a stand-in for
  real data-plane reachability. Keep that explicitly fixture-local.
- Snapshot JSON shape changes are acceptable inside the isolated MVP harness;
  there is no migration promise for pre-slice snapshots.
- Avoid turning this into the larger C1 boilerplate cleanup. That belongs in a
  separate refactor slice if the repeated newtype shells keep hurting review.

## Suggested Commit Shape

For implementation, keep commits small:

1. Plan document.
2. Shared identity type and deploy/lease/mesh refactor with focused tests.
3. Typed WireGuard peer fields and membership proof updates.
4. Simplification/docs follow-up after tests pass.

Run the simplify workflow after the first green focused proof and before the
final review pass if the refactor leaves adapter functions or unnecessary
clones behind.
