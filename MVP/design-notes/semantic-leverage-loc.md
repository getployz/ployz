---
title: Semantic Leverage LOC Snapshot
status: active
created: 2026-05-18
origin:
  - MVP/overall-plan.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
---

# Semantic Leverage LOC Snapshot

This note records a rough local LOC snapshot for the MVP rewrite. It is not a
scoreboard and should not be treated as exact accounting. The purpose is to
keep future slices honest about whether new product behavior is becoming
simpler to write, or whether the MVP is only moving complexity into new
substrate.

Method: rough Rust LOC counts use nonblank, non-`//` lines. Comparison sets are
approximate because the old code and the MVP do not have identical module
boundaries.

## Current Read

The rewrite is delivering a real maintenance-burden reduction where product
commands compose existing primitives. Deploy is the clearest win: the old
single deploy handler is about 4,300 lines and broader old deploy-shaped paths
are much larger, while `MVP/deploy` plus `MVP/deploy-p2panda` is about 3,350
lines with tests included and with more explicit commit/drain/recovery
semantics.

Machine remove is directionally better, but less dramatic: `MVP/machine` plus
`MVP/machine-p2panda` is about 3,500 lines and now includes restart recovery,
p2panda facts, serving cutover, tombstone, and cleanup-done semantics.

The rewrite is not yet a total LOC reduction. ACME, serving, projection, and
p2panda/fact substrate are carrying proof and foundation cost. That is
acceptable only if future product slices add less bespoke glue because the
shared primitives are doing real work.

Slice 033 is a positive composition signal: environment branch/promote/rollback
adds a new product primitive mostly by reusing p2panda facts, routing serving
writers, projection catch-up, process serving, and typed visible-node evidence.
The new `mvp-environment-p2panda` adapter is small and repeats the same backend
shape as routing/deploy/machine rather than inventing another store path.

Slice 040 is a direct substrate deletion win: roughly 2,193 active Rust lines
were deleted and 486 added while removing the opaque `PFO1`/quarantine-log
p2panda-net path. The canonical path now delegates network log mechanics to
p2panda-net and keeps Ployz code focused on fact authority, projection, and
business semantics.

Slice 041 is a substitution win rather than a raw deletion win. It adds the
durable membership store and E2E proof, but removes trusted-author CLI/input
shape from product-serving roles and gives future product slices one authority
source to reuse: root membership operation -> active writer/replica snapshot ->
fact-store authorization. The semantic leverage target for Slice 042 is that
ACME and sync stop carrying their own trusted-author/replica setup.

Slice 042 hits that substitution target without claiming a raw size win. The
slice changes ACME and the main p2panda sync proof from manual trusted-author
maps, trusted-replica setup, and hand-built sync scopes to shared
membership-backed authority. The slice diff is only a small net E2E increase
(`218` added, `152` deleted from the plan commit), but it removes a second
authority idiom from product-shaped proofs. ACME remains larger than the old
cert path: old cert coordination/backend files are roughly 1,180 physical Rust
lines, while `MVP/acme`, `MVP/acme-command`, and `MVP/lease` are roughly 4,151
physical Rust lines. That is acceptable only if future ACME work now composes
the existing lease/fact/sync/projection primitives instead of adding another
authority or coordination substrate.

Slice 043 continues the same substitution pattern for machine remove. It changes
the machine-remove E2E from manual trusted-author and trusted-replica setup to
the shared p2panda-auth membership snapshot. The slice diff is again a small
E2E increase (`216` added, `68` deleted from the plan commit), but it removes
feature-local authority setup from a multi-stage product command that exercises
serving cutover, recovery replay, tombstone, projection rebuild, and WireGuard
peer removal. That is the leverage target for the next product canaries:
business contracts should reuse the membership/fact/projection substrate
instead of creating their own trust model.

Slice 044 applies the same substitution to deploy restart recovery. The E2E
diff grows mostly from targeted negative probes rather than new substrate. That
is not a raw LOC win, but it removes manual author-key trust from the central
deploy crash-recovery proof and keeps deploy on the same membership-backed
authority model as ACME, sync, machine remove, and process serving.

## Snapshot Counts

```text
MVP core, excluding MVP/e2e and target:          ~35,500 LOC
MVP E2E contracts and harness:                   ~19,000 LOC
Old crates/ Rust total:                         ~111,000 LOC

MVP deploy + deploy-p2panda:                      ~3,350 LOC
Old single deploy handler:                        ~4,300 LOC
Old broad deploy-path files:                     ~25,100 LOC

MVP machine + machine-p2panda:                    ~3,500 LOC
MVP environment + environment-p2panda:             ~2,000 LOC
MVP acme + acme-command + lease:                  ~4,151 LOC
MVP serving + routing + routing-p2panda
  + projection:                                   ~8,600 LOC
MVP bus + mesh + iroh + p2panda facts
  + p2panda transport:                           ~14,400 LOC
```

## What Counts As A Win

Future slices should report:

- feature/business LOC added or deleted,
- shared primitive LOC added or deleted,
- adapter/backend LOC added or deleted,
- E2E/harness LOC added or deleted,
- old equivalent LOC retired or made irrelevant,
- duplicated glue deleted, such as local writer wrappers, projection parsers,
  transport setup, or command-specific storage code.

The target trend is that product features increasingly add business rules and
tests, not new substrate. Raw MVP LOC can grow during the proof phase, but
shared foundation LOC per product primitive should fall.

## Current Risks

- `mvp-p2panda-facts` and `mvp-p2panda-transport` are valuable only if more
  commands use them without adding feature-local sync/storage wrappers.
- The E2E harness is large enough that process-role and p2panda-net helpers
  should be reused aggressively instead of copied into each scenario.
- Serving/projection is not a raw LOC win yet. Its value depends on becoming
  the one reusable path for deploy, machine remove, ACME, routing, DNS, and
  future volume/service updates.
- ACME is not yet a LOC reduction because the MVP includes lease and command
  scaffolding but not full production issuance behavior. Treat it as a canary
  for singleton/lease semantics, not as a completed simplification result.
