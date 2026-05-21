---
title: Slice 053 Host Network Backend Report
status: complete
created: 2026-05-19
plan: MVP/slice-053-host-network-backend-plan.md
---

# Slice 053 Host Network Backend Report

Slice 053 adds the first product-shaped networking backend for the three-server
vertical.

## What Changed

- Added `HostNetworkSnapshot`, `HostNetworkRoute`, `HostNetworkEndpoint`, and
  `HostServiceAddress` in `mvp-mesh`.
- Added `HostNetworkBackend`, which validates active backend reachability with a
  bounded TCP connect and atomically persists the last applied snapshot.
- Added node-level `apply_host_networking_snapshot` and
  `load_host_networking_snapshot` helpers using the node state directory.
- Added `host-network.snapshot` to `NodePaths`.
- Added focused mesh and node tests for typed route conversion, invalid address
  rejection, reachability probing, and daemon-independent reload.

## Boundary

This is not kernel WireGuard. It is the explicit first networking mode for the
three-server product proof: if the hosts can route to each other's backend
addresses, Ployz can validate and remember those endpoints. The later Linux
WireGuard adapter should plug in behind the existing `WireGuardBackend` boundary
without changing deploy, projection, or serving semantics.

## Next Blocker

U6: wire the product `deploy` command through manifest parsing, node-agent
participant RPCs, projected route snapshots, host-network apply, and serving
reload.
