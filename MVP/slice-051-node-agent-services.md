---
title: Slice 051 Node Agent Services Report
status: complete
created: 2026-05-19
plan: MVP/slice-051-node-agent-services-plan.md
---

# Slice 051 Node Agent Services Report

Slice 051 adds the first product node-agent service surface.

## What Changed

- Added `MVP/node/src/node_agent.rs`.
- Added daemon startup registration for six local participant handlers:
  capacity, prepare instance, start instance, drain instance, stop instance, and
  cleanup deploy candidates.
- Reused the existing `mvp-deploy` wire request/reply types and exposed the
  wire encode/decode helpers for product composition.
- Added a small in-memory node-agent runtime state below the daemon boundary.
- Added tests that exercise requests through the bus instead of registering
  E2E-local deploy handler closures.

## Boundary

This is not the real runtime backend. The in-memory runtime proves the product
service contract and keeps deploy subject semantics stable. The next slice can
replace the runtime behavior with a process backend while keeping the handler
subjects and wire payloads unchanged.

## Next Blocker

U4: add a minimal runtime backend that can launch, rediscover, drain, and stop a
trivial local service process.

