---
title: p2panda Substitution Gains
status: active
created: 2026-05-19
origin:
  - MVP/slice-045-p2panda-substitution-gains-plan.md
  - MVP/slice-045-p2panda-substitution-gains.md
  - MVP/primitive-decisions.md
---

# p2panda Substitution Gains

## Decision

Bias toward p2panda, but substitute where it deletes Ployz-local plumbing
without erasing Ployz product semantics.

The active MVP workspace already uses p2panda `0.6.0` for facts, sync,
transport, and authz. Slice 046 added the missing reliability gate for the
p2panda-net fact-node path. The next highest-leverage product substitution is
now volume transfer membership-backed p2panda facts.

## What p2panda Should Own

- p2panda-core: signed operation envelope, operation validation, payload/body
  hash mechanics, and canonical operation identity.
- p2panda-store: operation and log persistence, including SQLite storage for
  p2panda operations.
- p2panda-sync: deterministic log sync protocol mechanics where full
  p2panda-net is not needed.
- p2panda-net: iroh-backed transport, topic log sync, address book,
  discovery/bootstrap transport information, gossip, and optional internal
  supervision where it reduces Ployz-local actor plumbing.
- p2panda-auth: group membership reduction, active writer/replica role
  evidence, and strong-removal-style membership replay.

## What Ployz Keeps

- PloyzBus subject, request/reply, queue group, bridge, and permission
  semantics.
- Fact-key grants and command-entry conflict checks.
- Projection reducers, current-state selection, superseded/conflict status, and
  SQLite/snapshot output.
- Gateway/DNS last-good serving behavior and process-role health surfaces.
- Visible nodes at decision time, no-quorum/local-decision semantics, and
  operator-facing reachability evidence.
- Product command state machines: deploy, machine remove, ACME, environment,
  volume transfer, and future storage/membership commands.
- Machine tombstone/reinvite policy and WireGuard overlay policy.

## Highest-Gain Candidates

### 1. p2panda-net Fact-node Reliability

Status:

- Completed in Slice 046.
- `p2panda-net-fact-node-reliability-contract` runs 12 canonical fact-node
  roundtrips and fails if any iteration reaches its deadline with zero
  attempted imports after publish.
- `PandaNetFactNodeStats` and process-role `P2pandaNetRoleStatus` now surface
  idle timeouts, stream refreshes, stream errors, replay skips, attempted
  import batches, and sync lifecycle counters.
- The current fix keeps p2panda-net as the product-shaped fact transport. It
  adds explicit address-book peer insertion for direct two-node tests and
  better no-progress diagnostics instead of replacing the transport.

Remaining follow-up:

- Keep p2panda-net address book/discovery as transport observation, not command
  membership truth.
- Revisit p2panda-net supervisor integration only if process-role status shows
  recurring stream failures that Ployz should not handle manually.

### 2. Volume Transfer Membership-backed Facts

Why:

- `MVP/e2e/src/volume_transfer_contract.rs` still creates an E2E-local
  `PandaVolumeFactStore`, manually trusts the writer author key, and imports
  replayed operations through direct `import_operation`.
- The deploy, machine-remove, ACME, sync, and process-serving proofs now use
  membership-backed authority. Volume is the last product-shaped command canary
  with a separate trust idiom.
- Migrating it tests whether the membership/fact/projection substrate is now
  good enough for storage movement without another bespoke adapter layer.

Expected follow-up slice:

- Create a small membership-backed opener for the volume E2E or extract a
  `mvp-volume-p2panda` adapter only if the migration reveals reusable
  ownership/lease writer logic.
- Replace manual trust/direct import with `IslandAuthoritySnapshot` and
  replica-import membership.
- Add negative probes mirroring the deploy and machine-remove authority
  boundaries: replica importer cannot write, writer-only member cannot import,
  original author fact-key denial names the author, same-island untrusted author
  is rejected, and foreign-island ownership facts are rejected.

### 3. Manual Trust API Quarantine

Why:

- `mvp-p2panda-facts` still exposes manual trust/import APIs because unit tests
  and fallback fixtures exercise them.
- Product-shaped paths should stop calling these directly.

Decision:

- Do not delete these APIs before volume migration and p2panda-net fallback
  audit complete.
- After volume moves, make the remaining manual trust API status explicit:
  harness-only, low-level fallback, or delete.

### 4. `MVP/p2panda-06-spike`

Why:

- It proved p2panda `0.6.0` fit before the active workspace moved.
- The active workspace now uses p2panda `0.6.0`, so the spike is historical.

Decision:

- Do not keep expanding it.
- Delete or archive it after Slice 045's recommendation is implemented and its
  compile evidence is either represented by active crates/tests or no longer
  useful.

## Explicit Deferrals

- p2panda-blobs: still not a current adoption target; payload/blob strategy
  should be revisited only when the crate API is mature enough to beat the
  current fact payload path.
- p2panda discovery/address book as membership: rejected. It can improve
  transport reachability, not durable command authority.
- p2panda supervision as Ployz supervision: deferred. It can reduce internal
  p2panda-net module failure handling, but Ployz process roles still need their
  own health/status/last-good surfaces.
- Removing historical E2Es from `all`: defer until a specific replacement map
  proves no unique gateway/DNS, projection, or fact-source behavior would be
  lost.
