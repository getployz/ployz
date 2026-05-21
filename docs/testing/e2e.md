# E2E Strategy

## Active Root Rewrite

The active root `crates/ployz-e2e` crate currently contains in-process product
acceptance tests for the new Polis/Ployz boundary. It is not yet the
long-running daemon/substrate harness. Run it with:

```bash
cargo test -p ployz-e2e
```

`cargo run -p ployz-e2e` exits non-zero until a real scenario runner exists.

Ployz E2E tests live in `crates/ployz-e2e`. They are the long-running system
harness and should be reserved for behavior that cannot be tested meaningfully
below E2E.

Use E2E when the value comes from crossing real boundaries: installed payloads,
multiple node containers, daemon processes, SSH bootstrap, real network
partitions, runtime containers, gateway/DNS/ACME behavior, or real ZFS.

Do not add E2E scenarios for command policy, state transitions, rendering,
store projections, NATS subject construction, or failure classification that can
be covered with memory stores, fake backends, command-handler tests, or
crate-level integration tests.

## Current Scenario Set

Default `just e2e` runs:

| Scenario | Boundary protected |
| --- | --- |
| `mesh_bootstrap_join_smoke` | Real install/startup plus SSH-driven two-node mesh join. |
| `node_restart_adopts_data_plane` | Daemon/container restart with existing mesh substrate. |
| `wireguard_partition_reconnect` | Real network partition and WireGuard recovery. |
| `deploy_http_acme_gateway_smoke` | Deploy, runtime container, gateway, ACME challenge propagation, and HTTPS serving. |
| `docker_bridge_forward_smoke` | Docker runtime bridge forwarding to NATS over the overlay bridge. |

Named final gates that are intentionally opt-in:

| Scenario | Boundary protected |
| --- | --- |
| `mvp_three_node_parity_smoke` | Final MVP data-plane parity: three privileged Docker E2E node containers, installed `mvp-node`, real WireGuard, Docker runtime workloads, gateway HTTP/HTTPS, Pebble ACME, container service DNS, update/drain, and daemon restart survival. |

With `--zfs real`, the suite also runs:

| Scenario | Boundary protected |
| --- | --- |
| `zfs_transfer_real_smoke` | Real ZFS snapshot/send/receive plus transfer tracking. |

`--zfs fake` does not add E2E storage scenarios. Fake-ZFS behavior belongs in
lower-level tests.

## Coverage That Belongs Below E2E

- Machine add must not promote storage authority: test through NATS/store/daemon
  integration surfaces by asserting default replica policy remains R=1 until an
  explicit storage-promotion primitive exists.
- Drain, standby, activate, and membership lifecycle transitions: test in daemon
  command-handler and orchestrator tests.
- Unreachable peer foreground failures: test NATS RPC no-responder and timeout
  classification through daemon/NATS tests.
- Destroy-with-dead-peer semantics: test handler behavior unless a real teardown
  substrate bug requires E2E coverage.
- Managed volume fake-ZFS behavior: test below E2E; reserve E2E storage coverage
  for real ZFS.

## Adding A Scenario

Before adding a new scenario, identify the real boundary it protects and the
lower-level test that would otherwise be insufficient. If the assertion can be
made with a fake backend or memory store, add it there instead.

Scenario names should describe the substrate behavior being protected, not the
implementation detail being exercised.
