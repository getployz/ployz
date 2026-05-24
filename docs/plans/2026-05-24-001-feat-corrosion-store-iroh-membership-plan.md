---
title: "feat: Add Corrosion Store With Iroh Membership"
type: feat
status: completed
date: 2026-05-24
---

# feat: Add Corrosion Store With Iroh Membership

## Summary

Build the first Corrosion-backed control-plane slice around one vertical path:
`MachineMembershipPort::observe/join` over Corrosion rows with iroh identity and
peer preflight.

`polis` is the distributed-primitives layer. It owns Corrosion access,
transactions, subscriptions, change cursors, iroh identity, tickets, RPC,
deadlines, peer probes, membership row primitives, and distributed failure
typing.

`ployz` owns product semantics. Machine join behavior, namespace meaning,
deploy semantics, routing decisions, capacity policy, volume movement,
readiness, and operation outcomes stay in Ployz modules and Ployz adapters.

The first slice proves the boundary. It must not build a general substrate
framework or product-shaped Polis backend.

---

## Boundary Decision

Default to Option B: Polis owns distributed primitives, not Ployz product
services.

Option A, rejected by default:

- `polis::machines`
- `polis::namespaces`
- `polis::operations`
- `polis::capacity`
- product-shaped calls such as `machines.join`, `capacity.reserve`, or
  `deploy.record_ready`

Option B, chosen:

- `store.transaction`
- `store.query`
- `store.subscribe`
- `store.updates`
- `identity.endpoint_id`
- `membership.known_peers`
- `membership.rows`
- `membership.wireguard_peers_for_machine`
- `peers.probe`
- `peers.rpc`

Why: Option A keeps Ployz very clean in the short term, but it makes Polis
drift toward "Ployz backend in disguise". Product vocabulary leaks downward,
deploy/capacity/routing semantics split across crates, and future non-Ployz
users inherit Ployz-shaped APIs. Option B keeps Polis conceptually small and
reusable: distributed store, RPC, membership, identity, subscriptions, and
networking.

Accepted cost: Ployz adapters carry more responsibility. They may be
purposeful and a little thicker when product behavior needs to sequence
transactions, subscriptions, probes, and RPC calls. Ordinary Ployz modules
still stay readable because they depend on Ployz ports, not on Polis or
Corrosion.

---

## Requirements

- R1. Use Corrosion natively through `corro-client` inside Polis primitives:
  transactions, queries, table updates, and resumable subscriptions.
- R2. Keep Polis store primitives Corrosion-specific and small. Do not build a
  broad `TableSpec<T>` or generic backend framework up front.
- R3. Keep Ployz product code free of `corro-client`, Corrosion SQL/statement
  types, iroh ticket internals, and `irpc` transport types.
- R4. Keep product behavior in Ployz adapters: machine join/remove/tombstone,
  namespace meaning, deploy behavior, routing, capacity, volume, readiness,
  and operation outcomes.
- R5. Use iroh endpoint identity as durable peer identity. Tickets are
  bootstrap envelopes only.
- R6. Use `irpc` over iroh for bounded daemon-to-daemon commands. Do not
  forward public CLI/API request types as peer RPC payloads.
- R7. Preserve the operator-perspective model: the reached node reads its
  current replicated view, probes peers required by the operation, computes a
  concrete plan, and runs it without requiring perfect global consensus.
- R8. Derive WireGuard peer configuration from active namespace membership,
  not global machine membership.
- R9. Keep the first schema minimal: `machines`, `namespaces`, and
  `namespace_memberships`.
- R10. Model authority islands with `island_id`, namespace membership, and
  prod-owned writes. Do not introduce a large authority schema yet.
- R11. Resolve the async substrate boundary before Corrosion or iroh code
  lands. No adapter may invent local `block_on` behavior.

---

## Scope

### First Slice

Implement only the machinery needed to make this vertical path real:

```text
MachineMembershipPort::observe/join
  -> crates/ployz/src/adapters/polis/machine_membership.rs
      -> polis store transaction/query/subscription primitives
      -> polis membership row primitives
      -> polis peer probe/preflight primitive
```

The first slice may create the minimal Corrosion schema required for:

- `machines`
- `namespaces`
- `namespace_memberships`

But it should only exercise the parts needed by machine membership and
namespace-derived peer sets.

### Explicit Non-Goals

- Do not migrate deploy, routing, image, volume, ACME, or branch stores in this
  slice.
- Do not build product-shaped Polis APIs.
- Do not build a generic backend abstraction.
- Do not make Corrosion the command bus.
- Do not introduce a reconciler or background policy engine.
- Do not attempt perfect split-brain prevention.
- Do not store durable ticket text in machine rows.
- Do not add operations or claims/leases in this slice. Most rows are
  owner-written and mostly static; command evidence and fencing can return
  when a product path proves it needs them.

### Deferred

- Domain status, serving, ACME, volume, branch, routing, and capacity adapters.
- Operator-facing Corrosion diagnostics.
- Hosted relay UX.
- `machine_dial_overrides` or local bootstrap overrides for static/private
  discovery cases, if proven necessary.

---

## Minimal Schema

### `machines`

- `machine_id`
- `island_id`
- `iroh_endpoint_id`
- `wireguard_public_key`
- `overlay_ip`
- `capabilities_json`
- `lifecycle`
- `epoch`
- `updated_at`

No `iroh_ticket` column. Durable identity is `iroh_endpoint_id`.

### `namespaces`

- `namespace_id`
- `owner_island_id`
- `name`
- `lifecycle`
- `epoch`
- `updated_at`

### `namespace_memberships`

- `namespace_id`
- `machine_id`
- `role`
- `lifecycle`
- `epoch`
- `updated_at`
- Primary key: `(namespace_id, machine_id)`

---

## Boot Order

Corrosion starts after iroh, so Corrosion cannot bootstrap iroh connectivity.

1. Load the local iroh secret key from local disk.
2. Start the iroh endpoint with discovery/relay config.
3. Start the internal RPC listener over iroh.
4. Join or rejoin via ticket or known endpoint ID if needed.
5. Start the local Corrosion agent.
6. Seed Corrosion bootstrap/membership from local config or RPC response.
7. Let Corrosion sync tables.
8. Start higher-level Ployz services.

Returning machines load the same iroh key, recover the same endpoint ID, start
iroh, and re-discover/reconnect through configured discovery/relay policy.
Tickets are useful for first join, invite, and manual recovery, but they are
not durable machine truth.

---

## WireGuard Peer Derivation

WireGuard config is derived from namespace membership. A node connects to the
union of peers sharing active namespaces with it. Global machine membership
does not imply network reachability; namespace membership implies an allowed
network edge.

Directional query:

```sql
SELECT DISTINCT peer.*
FROM namespace_memberships self_membership
JOIN namespace_memberships peer_membership
  ON peer_membership.namespace_id = self_membership.namespace_id
JOIN machines peer
  ON peer.machine_id = peer_membership.machine_id
WHERE self_membership.machine_id = ?
  AND self_membership.lifecycle = 'active'
  AND peer_membership.lifecycle = 'active'
  AND peer.machine_id != ?
  AND peer.lifecycle = 'active';
```

This may live in Polis only as a pure membership/network primitive such as
`wireguard_peers_for_machine`. Ployz owns the policy that uses the peer set.

---

## Authority Islands

Authority membership, namespace participation, and network participation are
separate.

A dev/external machine may participate in one prod namespace without joining
prod authority:

```text
dev island / machine
  -> RPC to reachable prod peer
      -> prod peer authorizes
      -> prod peer writes machines + namespace_memberships
      -> affected WireGuard controllers derive peer config
      -> prod probes connectivity
      -> deploy continues
```

External machines do not write prod Corrosion directly. External/dev islands
RPC into prod; prod authorizes and writes prod tables.

---

## Expected Shape

```text
crates/polis/src/store.rs
crates/polis/src/subscriptions.rs
crates/polis/src/identity.rs
crates/polis/src/membership.rs
crates/polis/src/peers.rs

crates/ployz/src/adapters/polis/
  machine_membership.rs
  peer_rpc.rs
```

This is not a hard module map. The hard rule is ownership:

- Polis hides distributed substrate mechanics from Ployz.
- Ployz adapters translate Polis primitives into product ports.
- Ordinary Ployz modules read as product logic.

---

## Implementation Units

### U0. Resolve Substrate Gates

**Goal:** Decide the toolchain and async boundary before adding Corrosion or
iroh implementation code.

**Requirements:** R2, R11

**Files:**

- Modify: `Cargo.toml`

**Approach:**

- Raise `workspace.package.rust-version` to `1.91` if target dependencies
  require it:
  - `iroh = 1.0.0-rc.0`
  - `iroh-tickets = 1.0.0-rc.0`
  - `irpc = 0.15.0`
  - `irpc-iroh = 0.15.0`
- Produce one explicit async boundary outcome:
  - Either Ployz ports that perform substrate I/O become async.
  - Or daemon composition owns the only blocking boundary.
- Forbid ad hoc `block_on` inside individual adapters.

**Verification:**

- Target dependency versions have a compatible workspace Rust version.
- The async boundary is explicit enough to review.

---

### U1. Add Minimal Corrosion Store Primitive

**Goal:** Add the smallest Corrosion-specific primitive required by the machine
membership adapter.

**Requirements:** R1, R2, R3, R9, R11

**Dependencies:** U0

**Files:**

- Modify: `crates/polis/Cargo.toml`
- Modify: `crates/polis/src/lib.rs`
- Create: `crates/polis/src/store.rs`
- Create only if needed: `crates/polis/src/subscriptions.rs`
- Test: `crates/polis/src/store.rs`

**Approach:**

- Add `corro-client` to `polis`, not `ployz`.
- Keep the API boring and narrow:
  - `execute_transaction`
  - `query`
  - `subscribe`
  - `updates`
  - `apply_schema`
- Use `/v1/subscriptions` with `ChangeId` for durable catch-up.
- Use `/v1/updates/{table}` only for non-resumable invalidation or diagnostics.
- Normalize Corrosion/client failures into typed Polis substrate errors.
- Do not add `TableSpec<T>`, generic backend traits, or typed query framework.
- Let the first Ployz adapter own row decoding until duplication is real.

**Verification:**

- A Corrosion transaction can insert rows and a query can read them back.
- A subscription can resume from a saved `ChangeId`.
- Failed transactions do not create partially visible state.
- Ployz does not import Corrosion types.

---

### U2. Add Identity and Membership Row Primitives

**Goal:** Represent endpoint identity, known peer rows, namespace overlap, and
WireGuard peer derivation without owning machine join semantics.

**Requirements:** R3, R5, R8, R9, R10

**Dependencies:** U1

**Files:**

- Modify: `crates/polis/src/lib.rs`
- Create: `crates/polis/src/identity.rs`
- Create: `crates/polis/src/membership.rs`
- Test: `crates/polis/src/identity.rs`
- Test: `crates/polis/src/membership.rs`

**Approach:**

- Represent iroh endpoint IDs as typed values with parsing, serialization, and
  redacted display.
- Represent membership rows as substrate records only.
- Keep lifecycle and epoch as row fields; Ployz interprets product outcomes
  such as `Joined`, `AlreadyPresent`, `Conflict`, and `Removing`.
- Provide primitive reads/writes for:
  - machine rows
  - namespace rows
  - namespace membership rows
  - active peer rows
  - WireGuard peer derivation
- Do not store ticket text in durable rows.

**Verification:**

- Membership rows preserve endpoint ID, WireGuard key, overlay IP,
  capabilities, lifecycle, epoch, and island ID.
- Lower-epoch rows do not overwrite newer owner-written rows.
- Same-epoch owner updates are accepted; this first slice does not build a
  row-conflict subsystem.
- Active namespace overlap derives the expected WireGuard peers.

---

### U3. Add Peer Probe / Bootstrap Primitive

**Goal:** Add just enough iroh/irpc primitive support for machine join
preflight. Full peer command coverage can wait.

**Requirements:** R3, R4, R5, R11

**Dependencies:** U0, U2

**Files:**

- Modify: `crates/polis/Cargo.toml`
- Modify: `crates/polis/src/lib.rs`
- Create: `crates/polis/src/peers.rs`
- Test: `crates/polis/src/peers.rs`

**Approach:**

- Target:
  - `iroh = 1.0.0-rc.0`
  - `iroh-tickets = 1.0.0-rc.0`
  - `irpc = 0.15.0`
  - `irpc-iroh = 0.15.0`
- Represent tickets as bootstrap envelopes with parsing and redacted display.
- Expose primitive operations:
  - `load_or_create_identity`
  - `issue_ticket`
  - `import_ticket`
  - `probe`
  - `rpc` or `preflight_membership`
- Use a narrow internal RPC protocol. Do not reuse public CLI/API request
  types.
- Keep local/fake peer probes available for adapter tests.

**Verification:**

- Tickets parse and round-trip without becoming durable membership truth.
- Local/fake peer probe succeeds and fails with typed errors.
- Iroh-backed smoke test can connect two endpoints and run one bounded probe if
  the test environment supports it.

---

### U4. Build Machine Membership Adapter Over Polis Primitives

**Goal:** Implement the first real Ployz adapter over Polis primitives while
keeping machine join semantics in Ployz.

**Requirements:** R2, R3, R4, R5, R8, R10, R11

**Dependencies:** U1, U2, U3

**Files:**

- Modify: `crates/ployz/src/adapters/polis/mod.rs`
- Create or replace: `crates/ployz/src/adapters/polis/machine_membership.rs`
- Test: `crates/ployz/src/adapters/polis/machine_membership.rs`
- Test: `crates/ployz/src/machine.rs`

**Approach:**

- Keep `crates/ployz/src/machine.rs` focused on product behavior and ports.
- Map `MachineMembershipPort::observe` to Polis membership row primitives.
- Map `MachineMembershipPort::join` to this product sequence:
  1. authority/freshness check,
  2. peer probe/preflight when target is remote,
  3. Corrosion transaction through Polis store primitive,
  4. product outcome mapping.
- Do not allow a real join path to write membership before required peer
  preflight succeeds or is explicitly bypassed by a local/test path.
- Keep namespace-derived WireGuard peer derivation in the adapter or a
  primitive query; product meaning remains in Ployz.

**Verification:**

- Adding an absent preflighted machine commits one membership row and returns
  `Joined`.
- Adding the same machine/epoch/identity returns `AlreadyPresent`.
- Peer preflight failure returns a structured Ployz machine failure and writes
  no membership row.
- Polis transaction/subscription/RPC failures map to Ployz failure enums
  without display-string parsing.

---

### U5. Document Boundary and Retire Stale Fact/NATS Guidance

**Goal:** Ensure docs and future-agent instructions point at the Option B
boundary and no longer direct implementation toward p2panda, NATS, facts, or
product-shaped Polis services.

**Requirements:** R7

**Dependencies:** U0-U4

**Files:**

- Modify: `VISION.md`
- Modify: `AGENTS.md`
- Modify: `docs/architecture.md`
- Modify: `docs/architecture/ployz-rewrite.md`
- Modify: `docs/nats.md`
- Modify:
  `docs/solutions/architecture-patterns/operator-perspective-commands-with-corrosion-rows-2026-05-24.md`

**Approach:**

- Use exact vocabulary:
  - Polis primitive
  - Ployz adapter
  - Ployz port
  - Corrosion row/subscription
- Ban ambiguous "Polis service" wording unless the service is clearly a
  distributed primitive, not product-shaped.
- Mark NATS/p2panda guidance historical or superseded.
- Keep replicated state separate from command execution.

**Verification:**

- A reader no longer sees conflicting guidance that NATS, p2panda, generic
  facts, or product-shaped Polis services are the intended substrate.

---

## Risks

| Risk | Mitigation |
|------|------------|
| Polis primitives become a generic store framework. | U1 is intentionally tiny: transaction, query, subscribe, updates, apply schema. No `TableSpec<T>` until repeated duplication forces it. |
| Polis drifts back into product services. | Product verbs live in Ployz adapters. Ban `machines.join`, `deploy.record_ready`, `capacity.reserve`, and routing policy APIs in Polis by default. |
| The first slice scaffolds too much before proving value. | Build one vertical path first: machine observe/join with peer preflight. Defer domain, serving, volume, ACME, branch, capacity, and routing. |
| Async boundary becomes ad hoc. | U0 must produce one explicit boundary. No adapter-local `block_on`. |
| Tickets become durable truth. | Tickets are bootstrap envelopes only. Durable identity is `iroh_endpoint_id`. |
| Corrosion subscriptions are treated as always-fresh truth. | Preserve `ChangeId`, missed-change errors, and freshness states; surface uncertainty to Ployz adapters. |
| Corrosion agent/client version mismatch. | Verify whether to use crates.io `corro-client 0.2.0-alpha.0`, a git tag, or a vendored wrapper before locking dependencies. |

---

## Sources

- Product direction: `VISION.md`
- Project guardrails: `AGENTS.md`
- Existing Polis boundary: `crates/polis/src/lib.rs`
- Existing Ployz business logic example: `crates/ployz/src/machine.rs`
- Existing Ployz/Polis adapter boundary: `crates/ployz/src/adapters/polis/`
- Corrosion release: `https://github.com/superfly/corrosion/releases/tag/v1.0.0`
- `corro-client`: `https://docs.rs/corro-client/0.2.0-alpha.0`
- `iroh`: `https://docs.rs/iroh/1.0.0-rc.0`
- `iroh-tickets`: `https://docs.rs/iroh-tickets/1.0.0-rc.0`
- `irpc`: `https://docs.rs/irpc/0.15.0`
- `irpc-iroh`: `https://docs.rs/irpc-iroh/0.15.0`
