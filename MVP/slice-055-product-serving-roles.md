# Slice 055: Product Serving Roles

## Summary

`mvp-node gateway` and `mvp-node dns` now run as snapshot-backed serving roles. They load the node's persisted `gateway.snapshot` and `dns.snapshot`, expose a Unix control socket for readiness/status/reload/shutdown, and keep serving last-good state when reload fails.

## Why This Matters

The three-server vertical needs the daemon to be a modifier/coordinator, not the process that must stay alive for steady state. This slice moves gateway/DNS steady-state serving into product roles so a deployed service can keep being reached after orchestration has stopped.

## Boundary

- `mvp-node` owns process role wiring and the Unix control protocol.
- `mvp-serving` owns snapshot validation, last-good state, and placeholder wire servers.
- Placeholder HTTP/DNS internals are still migration targets; the durable interface is snapshot loading, reload validation, status, and last-good behavior.

## Verification Intent

The product-role test deploys a service, starts `mvp-node gateway` and `mvp-node dns` subprocesses from persisted snapshots, verifies HTTP and DNS reachability, corrupts the gateway snapshot, reloads, and verifies the gateway still serves the previous good route.
