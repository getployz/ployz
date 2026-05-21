---
title: Slice 045 p2panda Substitution Gains
status: completed
created: 2026-05-19
origin:
  - MVP/slice-045-p2panda-substitution-gains-plan.md
  - MVP/design-notes/p2panda-substitution-gains.md
  - MVP/primitive-decisions.md
external:
  - https://docs.rs/p2panda-net/latest/p2panda_net/
  - https://docs.rs/p2panda-auth/latest/p2panda_auth/
  - https://docs.rs/p2panda-store/latest/p2panda_store/
  - https://docs.rs/p2panda-sync/latest/p2panda_sync/
  - https://p2panda.org/2025/07/09/streams-transactions-crash-resilience.html
  - https://p2panda.org/2025/07/28/access-control.html
  - https://p2panda.org/2025/08/27/notes-convergent-access-control-crdt.html
---

# Slice 045 p2panda Substitution Gains

## Result

The active workspace is already on the useful p2panda line:

- `mvp-p2panda-facts` uses `p2panda-core`, `p2panda-store`,
  `p2panda-stream`, `p2panda-sync`, and `mvp-p2panda-authz`.
- `mvp-p2panda-transport` uses `p2panda-net 0.6.0` plus
  `p2panda-sync 0.6.0`.
- `p2panda-auth` is now the right authority source for product-shaped fact
  stores through `IslandAuthoritySnapshot`.

The largest remaining product simplification is still volume transfer
membership-backed facts, but the next implementation slice should be
p2panda-net fact-node reliability first. The reason is concrete: the planned
Slice 045 verification produced one focused `p2panda-net-fact-node-contract`
failure where the receiver reported zero attempted imports, and the same proof
passed immediately on rerun. That is exactly the reliability class the MVP is
supposed to flush out before more product code depends on the transport path.

## Inventory

Remaining manual trust or direct import hits fall into five groups:

| Surface | Current hits | Classification | Decision |
| --- | ---: | --- | --- |
| `mvp-p2panda-facts` manual trust/import APIs | public fallback APIs and unit tests | fallback plus regression tests | Keep for now, quarantine after volume migration. |
| `mvp-machine-p2panda` trust helpers | helper methods plus tests | mostly fixture/adaptor compatibility | Keep until a cleanup slice can remove unused helper surface safely. |
| `mvp-commands-p2panda` raw trusted-author setup | unit tests only | fixture | Keep; not a product path. |
| `volume_transfer_contract` `PandaVolumeFactStore` | E2E-local manual trust and direct import | product-shaped manual-trust canary | Migrate after p2panda-net reliability. |
| p2panda-net fallback probes | `manual_fallback_store` and direct probe helpers | low-level negative regression fixture | Keep until replaced by an explicit reliability/fallback proof. |

The most important product hit is
`MVP/e2e/src/volume_transfer_contract.rs`. It still constructs an E2E-local
`PandaVolumeFactStore`, trusts the writer author key manually, and replays
exported operations through direct import. That is now inconsistent with ACME,
sync, machine remove, deploy recovery, and process serving.

The most important reliability hit is
`MVP/p2panda-transport/src/fact_node.rs`. `PandaNetFactNode` already delegates
endpoint, discovery, gossip, and log sync to p2panda-net, but Ployz wraps it
with startup timeouts, stream refresh, replay suppression, pending deferred
imports, and process-role retry loops. That local wrapper is small enough to
audit, but the zero-import focused failure says it needs a reliability slice
before it becomes invisible substrate.

## Upstream Fit Matrix

| Crate | Replace now | Cannot replace |
| --- | --- | --- |
| `p2panda-auth` | Membership graph reduction, strong-removal replay, active writer and replica-import role evidence. | Ployz fact-key grants, command preflight checks, tombstone policy, visible-node reporting. |
| `p2panda-store` | SQLite operation/log/topic/group storage and transaction primitives. | Ployz operation validation, product authorization, projection reducers. |
| `p2panda-sync` | Deterministic two-party log sync where full networking is not needed. | Ployz command semantics, request/reply bus, process-role serving. |
| `p2panda-net` | Iroh transport, address book, discovery, gossip, topic log sync, optional internal module supervision. | Durable membership truth, command consistency, import authorization, serving health surfaces. |
| `p2panda-stream` | Future stream/rebuild helpers if exposed cleanly for command/state processing. | Workflow replay, phase semantics, or command compensation. |

The p2panda docs line up with the current architecture: p2panda-net owns event
delivery and topic log sync; p2panda-auth owns eventually consistent group
state and strict manager-only group changes; p2panda-store owns storage traits
and SQLite implementations; p2panda-sync owns lower-level protocol/manager
interfaces. Ployz still has to own application validation, fact-key grants,
operator command semantics, and last-good serving.

## Candidate Ranking

### 1. p2panda-net Fact-node Reliability

This is the next slice because the proof gate actually failed once:

```text
FAIL p2panda-net fact-node imports did not arrive:
PandaNetFactImportReport { attempted: 0, imported: 0, ... }
```

The rerun passed, and `mvp-e2e -- all` passed earlier in the same session, so
this is not a persistent regression. It is still a foundation issue: a
zero-import false failure means the E2E cannot yet tell whether the transport
has not delivered, the stream started too early, the idle refresh was needed,
or the harness observed before the receiver was actually ready.

Next proof shape:

- Run `p2panda-net-fact-node-contract` and
  `p2panda-net-process-serving-contract` repeatedly in one bounded E2E.
- Record startup, stream idle refresh, stream-ended refresh, replay-skip,
  attempted import, and zero-import retry metrics.
- Use p2panda-net address book/discovery/supervisor state only as observation,
  not as command truth.
- Preserve Ployz-owned import outcomes and last-good serving state.

### 2. Volume Transfer Membership-backed Facts

This remains the largest product simplification after transport reliability.
The next volume slice should move the E2E-local `PandaVolumeFactStore` onto
membership-backed authority and replica import, matching the ACME, deploy, and
machine-remove proofs.

The slice should not extract a generic `mvp-volume-p2panda` adapter unless the
migration reveals reusable ownership/lease writer logic that clearly belongs
outside the E2E. The first goal is to delete the separate trust idiom from the
last product-shaped canary.

### 3. Manual Trust API Quarantine

Do not delete manual trust APIs from `mvp-p2panda-facts` yet. They still carry
unit-test and low-level fixture value. After volume moves, quarantine the
remaining calls with clearer naming or feature gates so product paths cannot
quietly reintroduce them.

### 4. Historical Spike Cleanup

`MVP/p2panda-06-spike/src/lib.rs` is 418 lines of historical compile evidence.
It should not grow. Delete it after the reliability and volume substitutions
make its current p2panda `0.6.0` evidence redundant in active crates.

## Rejected Substitutions

- Do not treat p2panda-net address book/discovery as membership. It is
  reachability evidence, not durable authority.
- Do not let p2panda supervision replace Ployz process-role health surfaces.
  It may reduce internal p2panda-net module handling, but serving roles still
  need explicit last-good state and operator-visible failures.
- Do not remove historical E2Es from `all` until a replacement map proves no
  unique projection, gateway/DNS, process-role, or fact-source behavior is
  being dropped.

## Proofs

Commands run:

```text
rg -n "trust_author_key|trust_replica_peer|with_trusted_author_key|from_trusted_authors|import_operation\\(" MVP -g '*.rs'
rg -n "PandaNetFactNode|AddressBook|Discovery|Gossip|LogSync|Supervisor|PandaNetReplayCache|refresh_stream|StreamEnded" MVP/p2panda-transport MVP/e2e/src -g '*.rs'
cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts --depth 1
cargo tree --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport --depth 1
wc -l MVP/p2panda-facts/src/lib.rs MVP/p2panda-transport/src/fact_node.rs MVP/p2panda-transport/src/node.rs MVP/p2panda-authz/src/lib.rs MVP/machine-p2panda/src/lib.rs MVP/commands-p2panda/src/lib.rs MVP/e2e/src/volume_transfer_contract.rs MVP/e2e/src/process_role_harness.rs MVP/e2e/src/p2panda_net_fact_node_contract.rs MVP/e2e/src/p2panda_sync_fact_source_contract.rs
cargo check --manifest-path MVP/Cargo.toml -p mvp-e2e
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-facts
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-transport
cargo test --manifest-path MVP/Cargo.toml -p mvp-p2panda-authz
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- p2panda-net-fact-node-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- volume-transfer-contract
cargo run --manifest-path MVP/Cargo.toml -p mvp-e2e -- all
git diff --check
```

Outcomes:

- `mvp-p2panda-facts`: 35 tests passed.
- `mvp-p2panda-transport`: 14 tests passed.
- `mvp-p2panda-authz`: 35 tests passed.
- `p2panda-net-fact-node-contract`: failed once with zero attempted imports,
  then passed on immediate rerun with 11 attempted imports, 8 inserted imports,
  1 conflict, and 2 structured rejections.
- `volume-transfer-contract`: passed.
- `mvp-e2e -- all`: passed before this report, including p2panda sync, ACME,
  deploy restart recovery, process serving, machine remove, volume transfer,
  and scale.

## Review

Subagent review was unavailable during this slice because the account hit the
subagent usage limit. I ran the planned review roles locally against the
evidence:

- Correctness: do not implement volume before the zero-import transport failure
  has a stronger diagnosis; otherwise new product proof may inherit an
  intermittent substrate failure.
- Security: keep p2panda-auth as authority evidence only. Ployz fact-key grants
  and command preconditions remain mandatory.
- Testing: the next p2panda-net slice needs a repeated-run proof, not another
  single happy-path pass.
- Maintainability: volume is the last product-shaped manual-trust canary and
  should be next after reliability, not deferred behind broad API cleanup.

## Next Slice

Plan Slice 046 as p2panda-net fact-node reliability hardening. The target is
not a rewrite; it is a bounded proof and small fix surface that explains and
eliminates zero-import false failures. Volume transfer membership-backed facts
should follow immediately after that if the transport proof stabilizes.
