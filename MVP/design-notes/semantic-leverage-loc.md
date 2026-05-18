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

## Snapshot Counts

```text
MVP core, excluding MVP/e2e and target:          ~35,500 LOC
MVP E2E contracts and harness:                   ~18,650 LOC
Old crates/ Rust total:                         ~111,000 LOC

MVP deploy + deploy-p2panda:                      ~3,350 LOC
Old single deploy handler:                        ~4,300 LOC
Old broad deploy-path files:                     ~25,100 LOC

MVP machine + machine-p2panda:                    ~3,500 LOC
MVP acme + acme-command + lease:                  ~3,700 LOC
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
