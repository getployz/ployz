---
title: Slice 054 Product Deploy Command Report
status: complete
created: 2026-05-19
plan: MVP/slice-054-product-deploy-command-plan.md
---

# Slice 054 Product Deploy Command Report

Slice 054 adds the first product deploy path.

## What Changed

- Extended deploy participant start replies with an optional concrete backend
  endpoint.
- Taught the deploy coordinator to materialize serving commit active backends
  from runtime-reported endpoints while preserving existing static-manifest
  behavior when no endpoint is reported.
- Added `ProductDeployOptions`, `ProductDeployReport`, and
  `deploy_product_service`.
- Added `mvp-node deploy --state <dir> --target-node <id>` with optional
  deploy id, service, revision, and hostname flags.
- Wired the product deploy wrapper through real node-agent handlers,
  `ProcessRuntime`, `PandaDeployFactWriter`, `PandaServingFactWriter`,
  `ProjectionActorHandle`, and `HostNetworkBackend`.
- Added integration tests proving first deploy and update deploy through the
  shipped `mvp-node runtime-http` child role.

## Boundary

This is still a single-process product deploy proof for participant RPC. It uses
the product node-agent handlers and persistent p2panda fact writers, but it does
not yet carry node-agent request/reply over the live p2panda transport between
separate node processes. That remote command path is the next blocker for the
black-box three-server smoke.

## Next Blocker

U7/U8 need to connect product process roles: gateway/DNS from node state and a
three-node smoke harness that runs node daemons plus deploy from an operator
node.
