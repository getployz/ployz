# Node Runtime

`crates/ployz-node-runtime` is the library-level owner for local component
lifetimes. It is inspired by `tailscale-rs/ts_runtime`: one runtime object owns
component handles, shutdown, health, and typed message surfaces.

It does not own deploy policy, placement, certificate policy, volume movement
policy, or durable cluster state transitions.

## Responsibilities

- Own long-lived local component lifetimes.
- Provide a shared shutdown token and bounded shutdown deadline.
- Publish component health through `crates/ployz-supervision`.
- Host typed internal node-client surfaces as they move away from raw daemon
  request envelopes.

## Non-Responsibilities

- No hidden reconciliation loops.
- No background mutation of durable cluster truth.
- No deploy, branch, promote, storage placement, or certificate issuance policy.
- No daemon/API response shaping.

## Supervision

`crates/ployz-supervision` provides the reusable health and shutdown primitive:

- `Supervisor` spawns named tasks.
- `HealthRegistry` exposes task health snapshots.
- Shutdown cancels tasks and waits with a deadline.

Every autonomous background task should eventually be owned by this path or an
equivalent supervised component. Fire-and-forget `tokio::spawn` remains a bug
unless the task has an explicit audience and shutdown path elsewhere.

## Handler Direction

`ployzd` remains the process adapter:

- parse config and environment,
- compose runtime/backend instances,
- expose IPC/API surfaces,
- map typed workflow outcomes into daemon responses.

Feature workflow files should own product operations. Runtime components should
own component lifetimes. Handlers should not own both.
