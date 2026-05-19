# Slice 055: Product Serving Roles

## Goal

Make `mvp-node gateway` and `mvp-node dns` real product roles that load the node's persisted serving snapshots, serve last-good state, and reload independently from the daemon.

## Scope

- Add a `mvp-node` serving role boundary over the existing `mvp-serving` actor.
- Wire CLI commands for gateway and DNS with explicit listen/control addresses.
- Keep role control small: readiness/status/reload/shutdown over a Unix socket.
- Prove daemonless steady state by deploying once, stopping orchestration, then serving through the product roles.
- Extend the placeholder DNS wire server to answer `A` records because product deploy writes `A` records today.

## Non-Goals

- No Pingora migration yet.
- No full hickory-server migration yet.
- No polish of placeholder HTTP/DNS connection behavior beyond what blocks the product proof.
- No changes outside `MVP/`.

## Tests

- Product gateway role starts from persisted snapshots and proxies a deployed service.
- Product DNS role starts from persisted snapshots and answers the deployed service record.
- Bad snapshot reload returns a structured failure while the gateway keeps serving its last-good snapshot.
- Roles run as `mvp-node` subprocesses, not only in-process library calls.
