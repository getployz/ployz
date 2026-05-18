---
title: Slice 035 p2panda Authz Fact Authority Plan
status: active
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-034-p2panda-auth-and-substrate-simplification-plan.md
  - MVP/slice-034-p2panda-auth-and-substrate-simplification.md
external:
  - https://docs.rs/p2panda-auth/0.5.2
  - https://docs.rs/p2panda-core/0.5.2
  - https://docs.rs/p2panda-store/0.5.2
---

# Slice 035 p2panda Authz Fact Authority Plan

## Problem Frame

Slice 034 proved the p2panda-auth group model fits island membership, but the
proof is not durable yet. `mvp-p2panda-facts` still carries manual trust maps:

- `trusted_author_keys: BTreeMap<(IslandId, PrincipalId), PublicKey>`
- `trusted_replica_peers: BTreeSet<(IslandId, PrincipalId)>`
- `PandaFactSyncScope::from_trusted_authors(...)`

Those maps are the exact custom substrate this MVP is trying to remove. The
next slice should graduate `mvp-p2panda-authz` from a spike into an authority
source for p2panda fact writes/imports, while keeping product rules out of the
membership layer.

Do not put membership operations into `PandaFactStore`. That would create a
bootstrapping loop where the fact store authorizes the membership data that
authorizes the fact store. Membership gets its own p2panda-authz operation log;
the fact store consumes the resulting authority snapshot.

Root creation is the one exception to "membership authorizes membership." A
`CreateRoot` operation must be anchored by configured Ployz island root/admin
authority before it can create the first p2panda-auth group state. The first
valid root binding for an island/group is pinned; conflicting or unanchored
root-create operations are rejected during import and replay.

## Crate Scout

Checked on 2026-05-18:

- `p2panda-auth 0.5.2` remains the right group CRDT: Manage/Write/Read/Pull,
  conditions, manager-only group mutation, and strong removal.
- `p2panda-core 0.5.2` already provides signed Ed25519 operations, operation
  hashes, custom header extensions, and payload/body hashing. Use it for durable
  membership operation authorship instead of the Slice 034 test-only signature
  sketch.
- `p2panda-store 0.5.2` already provides Memory and SQLite operation stores.
  Reuse it for the authz membership log; do not introduce a new storage crate.
- `p2panda-sync 0.5.2` and `p2panda-net 0.5.2` are already proven for fact
  operation movement, but this slice should not add authz network replication
  yet. Export/import or persistent reopen is enough to prove durable replay.
- The non-RC iroh path is explicitly acceptable. `mvp-iroh` is already aligned
  to the crates.io p2panda-compatible iroh `0.96` family, and future authz
  replication should prefer the maintained `p2panda-net 0.5.2` transport path
  before inventing more direct-iroh protocol glue.
- `p2panda-blobs 0.5.2` still exports no usable library API and remains out of
  scope.

## Scope

In scope:

- Make `mvp-p2panda-authz` persist and replay membership operations with
  p2panda-core/store.
- Replace the test-only membership envelope with production-shaped data:
  p2panda signed operation plus typed membership payload.
- Build an `IslandAuthoritySnapshot` read model from replayed membership
  operations.
- Let `mvp-p2panda-facts` accept an authz-derived authority snapshot for:
  local fact writes, imported fact operations, trusted replica import, and sync
  scope validation.
- Add one E2E contract proving fact authority works without manual trusted
  author/replica seeding.
- Keep PloyzBus grants and fact-key grants as a separate authorization layer.
- Update docs with semantic-leverage accounting: which manual trust code remains
  and which paths no longer need it.

Out of scope:

- No PloyzBus subject permission replacement.
- No command-level behavior changes.
- No network replication of membership operations.
- No p2panda-blobs adoption.
- No generic `mvp-commands` primitive.
- No same-node-id reinvite semantics beyond explicit new member/epoch/key.
- No migration outside `MVP/`.
- No RC iroh dependency bump. If transport work appears while implementing this
  slice, route it through the existing non-RC p2panda/iroh compatibility line or
  defer it.

## Design

### Membership Log

`mvp-p2panda-authz` should own a small p2panda operation log separate from
`mvp-p2panda-facts`.

New durable payloads should be typed and explicit:

```text
IslandMembershipPayload
  CreateRoot { root_binding }
  AddMember { binding, role }
  RemoveMember { member }
  PromoteMember { member, role }
  DemoteMember { member, role }
```

Each p2panda header extension should carry:

- island id,
- group id,
- actor/member id,
- operation kind.

The p2panda operation hash should become the `IslandOperationId`. Do not use a
local counter as a durable id. Local counters are only acceptable as internal
sequence numbers for producing p2panda log positions.

There is one important implementation detail: `p2panda-auth` needs a CRDT
operation id before the p2panda-core operation hash exists. Do not force a
two-phase hash feedback loop. Keep two ids:

- `IslandCrdtOperationId`: the p2panda-auth operation id used inside the group
  CRDT operation.
- `IslandOperationId`: the durable p2panda-core operation hash exposed to the
  rest of Ployz.

Persist the mapping from durable operation hash to CRDT operation id in the
authz log/index. Product code, fact metadata, and reports should reference the
durable operation hash. The CRDT id remains authz-internal plumbing.

The reducer path:

```text
p2panda operation
  -> validate p2panda signature/hash
  -> decode membership payload
  -> reject unanchored root create
  -> reject wrong island/group/nested-group/invalid binding
  -> verify signer public key maps to actor/member id
  -> verify operation author matches signer
  -> verify signer key and epoch match the current durable binding
  -> verify signature covers dependencies/action/introduced binding
  -> feed p2panda-auth group CRDT
  -> update durable key-binding map
  -> expose IslandAuthoritySnapshot
```

Root handling rules:

- local empty-store initialization may create the first root only from an
  explicit `IslandRootAuthority` input owned by the caller;
- opening an existing authz store replays and verifies the pinned root;
- importing a root operation into a non-empty island/group must match the
  pinned root or fail;
- importing a root operation into an empty store is rejected unless the caller
  supplies matching external island root authority;
- a missing or malformed authz store fails closed before any fact store rebuild
  can trust it.

### Authority Snapshot

`IslandAuthoritySnapshot` should be a small immutable value, not a live store
handle. It should answer:

- `author_key(island, principal) -> Option<PublicKey>`
- `can_write_member(island, principal) -> bool`
- `can_import_replica(island, principal) -> bool`
- `sync_scope(island) -> active writer author keys`
- `frontier_for_member(island, principal) -> membership operation id/epoch`
- `historical_authority(island, principal, frontier) -> accepted binding/status`

It must not answer:

- whether a principal may write a specific fact key,
- whether a bus subject may be published/subscribed,
- command preconditions.

Those remain in `FactAuthorizer`, PloyzBus grants, and command code.

Use two different authority semantics:

- current authority for new local writes, new replica imports, and sync scope;
- historical authority for replaying already accepted stored operations after a
  writer was later removed or demoted.

Reopen must preserve durable history without resurrecting removed authority.
That means the snapshot needs enough history to verify an operation against the
membership frontier it recorded, while still excluding removed/demoted writers
from current writes/imports/sync.

### Fact Store Integration

`mvp-p2panda-facts` should consume the snapshot through a narrow authority seam,
not import group CRDT types everywhere.

Authz must open first. The fact store should either accept an authority snapshot
at `open_sqlite` time before it rebuilds indexes, or split open from rebuild so
the caller can install authority before replay. Do not keep the current
"reopen requires manual trusted keys" behavior on the new product path.

The fact store checks become:

```text
write_fact_payload(session, author, key, payload)
  -> session principal matches author principal
  -> authority says active writer and author key matches
  -> fact metadata records the membership operation id/epoch being trusted
  -> FactAuthorizer says principal may write this fact key

import_replica_operation(session, operation)
  -> authority says session principal is active replica importer
  -> operation author key matches active writer member at its recorded authority frontier
  -> FactAuthorizer says original author may write this fact key

sync scope
  -> derived from authority snapshot active writer members
  -> replica session must be active replica importer
```

Legacy manual trust helpers are not a product path after this slice. If
migrating every existing E2E caller would explode the slice, the helpers may
remain only as explicitly named fixture/harness APIs. Any remaining helper must
be documented as fixture-only, excluded from the new product proof, and named in
the follow-up deletion list.

### Process And Daemon-Down Semantics

This slice does not need to change serving process roles. The new proof should
still show that a restarted fact authority can rebuild from durable membership
operations before allowing fact import/write. Later process-serving slices can
replace their manual trusted-author flags with an authority snapshot.

## Implementation Units

### Unit 1: Durable Authz Operation Log

Files:

- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-authz/Cargo.toml`

Work:

- Add p2panda-core/store dependencies if missing.
- Promote the membership payload/envelope out of tests.
- Store membership operations in Memory and SQLite backends.
- Rebuild `IslandAuthz` and key bindings from stored operations on reopen.
- Accept `CreateRoot` only when it is signed by/configured from the island
  root/admin authority, then pin that first valid root for the island/group.
- Reject conflicting or unanchored root-create operations on import and replay.
- Reject wrong island/group, substituted signer key, missing binding, nested
  groups, and malformed payload before p2panda-auth reduction.
- Reject stale epoch keys, operation-author/signer mismatch, dependency/action
  tampering, and introduced-binding tampering before group reduction.

Tests:

- `MVP/p2panda-authz/src/lib.rs`
  - create root, reopen, and recover manager access;
  - unanchored or conflicting `CreateRoot` import/replay fails;
  - add writer, reopen, and recover `(island, principal, epoch, public key)`;
  - remove writer, reopen, and deny write;
  - add replica importer, reopen, and allow import but not write;
  - substituted key, stale epoch key, signer/author mismatch, and tampered
    dependencies/action/introduced binding fail before group reduction;
  - concurrent strong-removal cases still match Slice 034 behavior;
  - operation id equals or is derived from p2panda operation hash.

### Unit 2: Authority Snapshot Seam For Fact Store

Files:

- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/p2panda-facts/Cargo.toml`

Work:

- Add `IslandAuthoritySnapshot` or equivalent read model in authz.
- Add a narrow authority interface to `mvp-p2panda-facts`.
- Add fact-store construction/opening that installs authority before replay, or
  split `open_sqlite` from rebuild so authority can be installed before stored
  operations are validated.
- Replace `trusted_author_keys` and `trusted_replica_peers` checks on the new
  product path with snapshot checks.
- Record or derive the membership operation id/epoch that authorizes a fact
  operation, so imports validate against the authority frontier the writer
  actually used instead of only the latest snapshot.
- Derive sync scope from active writer members.
- Preserve `FactAuthorizer` as the final fact-key authorization check.
- Move any surviving manual trust methods behind names or modules that make
  their fixture-only status obvious to future callers.
- Inventory public wrapper APIs that expose manual trust today, including
  `SharedPandaFactStore` and p2panda-backed product adapters. Each must migrate
  to authority-backed construction, become fixture/harness-only, or be listed
  as named deletion debt in the slice report.

Tests:

- `MVP/p2panda-facts/src/lib.rs`
  - local write succeeds only when authz writer key matches author key;
  - imported operation succeeds only when original author is active writer;
  - pre-remove facts by a once-active writer still import after later removal
    when their recorded authority frontier proves they were written before the
    removal;
  - concurrent-remove, post-remove, demoted, and transitive removed-member fact
    operations fail with structured authority errors;
  - removed writer import fails with structured untrusted/removed error;
  - demoted writer cannot write or import as writer, and demotion to replica
    importer grants import without granting local writes;
  - replica importer can import but cannot write;
  - read-only principal cannot import;
  - sync scope excludes removed/demoted writers and includes active writers;
  - existing manual-trust tests either migrate or are explicitly fixture-only.

### Unit 3: Product Proof E2E

Files:

- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/p2panda_authz_fact_authority_contract.rs`

Work:

- Add a scenario that creates an authz membership log, builds an authority
  snapshot, writes/imports p2panda fact operations through `PandaFactStore`, and
  projects from the resulting store.
- Reopen both membership and fact stores and prove the same snapshot/fact
  projection result without manual trust seeding.
- Remove/demote the writer and prove future imports are rejected while existing
  facts still project as durable history.

Scenario assertions:

- no manual `trust_author_key` call in the success path;
- no manual `trust_replica_peer` call in the success path;
- writer fact projects before removal;
- writer fact still imports after removal when its recorded membership frontier
  predates the removal;
- replica import succeeds for active replica importer;
- replica import fails for read-only/non-replica principal;
- removed writer future import fails;
- demoted writer loses write/import-as-writer authority;
- sync scope after removal/demotion excludes removed writer;
- reopened stores reproduce the same authority decisions.
- authz store opens before fact store replay; no product-path reopen relies on
  manual trusted-author or trusted-replica seeding.

### Unit 4: Documentation And Semantic-Leverage Accounting

Files:

- `MVP/primitive-decisions.md`
- `MVP/overall-plan.md`
- `MVP/e2e-proof-plan.md`
- `MVP/slice-035-p2panda-authz-fact-authority.md`

Work:

- Record whether `trusted_author_keys`, `trusted_replica_peers`, and manual
  sync scope are deleted, fixture-only, or still production paths.
- Report business/domain LOC, adapter/backend LOC, test LOC, and docs LOC.
- Update the p2panda-auth decision with actual adoption status.
- Name the next deletion slice if any manual trust helpers remain.

## Risks And Constraints

- Do not hide fact-key authorization inside p2panda-auth. Membership Write is a
  broad island role; `FactAuthorizer` still decides specific keys.
- Do not let `PandaFactStore` own live mutable membership state. It consumes a
  snapshot.
- Do not introduce quorum or active-partition semantics.
- Do not make membership operation replay depend on projection SQLite.
- Do not overclaim production crypto if any test helper remains hash-only.
- Avoid broad E2E churn. One new product proof is enough for this slice if old
  scenarios still pass.
- Do not treat manual trust maps as a second authority system. They are either
  removed, quarantined as fixtures, or listed as deletion debt in the slice
  report.

## Verification

Required local gates:

```text
cd MVP
cargo fmt --all --check
cargo test -p mvp-p2panda-authz --all-targets
cargo test -p mvp-p2panda-facts --all-targets
cargo run -p mvp-e2e -- p2panda-authz-fact-authority-contract
cargo clippy --workspace --all-targets -- -D warnings
MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

## Follow-Up If This Succeeds

- Replace process-role trusted-author CLI flags with durable authz snapshots.
- Remove or quarantine fixture-only manual trust helpers.
- Consider authz membership replication over p2panda-sync/net after local
  durable replay is proven.
