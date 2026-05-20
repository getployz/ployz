---
title: Iroh Bus MVP Foundation
status: active
created: 2026-05-17
---

# Iroh Bus MVP Foundation

This directory is the working specification for rebuilding the Ployz foundation
around a NATS-shaped bus over iroh, Kameo-owned local subsystems, iroh-docs
fact replication, SQLite projections, WireGuard, and redesigned HTTP/DNS
data-plane serving.

The goal is not to port the current architecture feature-for-feature. The goal
is to prove a cleaner version-1 foundation with end-to-end tests that show it
works, stays fast, and survives the failure cases that matter.

## Source Of Truth

- [MVP/overall-plan.md](overall-plan.md) is the overall strategy map that
  future slice plans should run against.
- [MVP/architecture.md](architecture.md) defines the target architecture.
- [MVP/e2e-proof-plan.md](e2e-proof-plan.md) defines the proof harness and
  acceptance tests.
- [MVP/primitive-decisions.md](primitive-decisions.md) is the maintainer-facing
  rationale for the main crates and architecture primitives.

## Local Proof Command

From `MVP/`, run `just test` to execute the local MVP gate:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- time-budgeted `cargo run -p mvp-e2e -- all`

This is intentionally kept inside `MVP/` while the rewrite remains isolated
from the existing codebase. Wire it into the repo-level CI only when the user
allows touching non-`MVP/` files.

Individual E2E scenarios are also runnable while iterating:

- `cargo run -p mvp-e2e -- bus-contract`
- `cargo run -p mvp-e2e -- actor-contract`
- `cargo run -p mvp-e2e -- authority-contract`
- `cargo run -p mvp-e2e -- bridge-contract`
- `cargo run -p mvp-e2e -- projection-contract`
- `MVP/scripts/three-server-smoke.sh`
- `cargo run -p mvp-e2e -- scale`

Each scenario writes a JSON proof artifact under `MVP/target/mvp-e2e/`.
The `all` scenario is capped by `MVP_E2E_ALL_TIMEOUT`, defaulting to `120s`,
when run through `just test`.

## Product Binary Track

The three-server product vertical starts with `mvp-node`, a product-facing
binary that is separate from the E2E harness:

```text
cargo run -p mvp-node -- init --state /var/lib/ployz-mvp --island prod --node-id node-a
cargo run -p mvp-node -- daemon --state /var/lib/ployz-mvp --run-for-ms 1000
cargo run -p mvp-node -- invite --state /var/lib/ployz-mvp
cargo run -p mvp-node -- join --state /var/lib/ployz-mvp-b --token '<invite-json>' --node-id node-b
cargo run -p mvp-node -- admission --state /var/lib/ployz-mvp-b
cargo run -p mvp-node -- admit --state /var/lib/ployz-mvp --request '<admission-json>'
cargo run -p mvp-node -- daemon-status --control /tmp/ployz-daemon.sock
cargo run -p mvp-node -- deploy --state /var/lib/ployz-mvp --target-node node-b
cargo run -p mvp-node -- gateway --state /var/lib/ployz-mvp --listen 127.0.0.1:0 --control /tmp/ployz-gateway.sock --tls-listen 127.0.0.1:0
cargo run -p mvp-node -- acme-issue --state /var/lib/ployz-mvp --hostname web.example.test --gateway http://127.0.0.1:8080 --gateway-control /tmp/ployz-gateway.sock
cargo run -p mvp-node -- dns --state /var/lib/ployz-mvp --listen 127.0.0.1:0 --control /tmp/ployz-dns.sock
cargo run -p mvp-node -- status --state /var/lib/ployz-mvp
```

The currently wired surface is still intentionally small: `init` and `status`
establish persistent node identity and state paths; `invite` emits a bootstrap
token with a stable p2panda ticket; `join` initializes a node from that token;
`admission` emits the joiner's stable ticket and author identity; `admit`
records that joiner on the bootstrap node; `daemon` starts the product
p2panda fact node for a bounded run and handles addressed node-agent command
facts; `daemon-status` reads structured readiness/status from an owner-only
local control socket; `deploy` starts one trivial managed HTTP service through
the product deploy state machine, including peer targets; `deploy-status` reads
durable deploy lifecycle facts; `acme-issue` performs ACME HTTP-01 issuance,
reloads the gateway through `--gateway-control` before validation when supplied,
and writes the activated certificate into the serving projection; and
`gateway`/`dns` run snapshot-backed serving roles with a local Unix control
socket. `gateway` can also bind a product TLS listener with `--tls-listen
<addr>`.

`deploy` currently has two explicit modes. With `--control <socket>`, deploy is
submitted through the local daemon that owns transport, membership, and remote
node-agent command handling. Without `--control`, standalone deploy may run when
the local coordinator daemon is down, and it owns the local p2panda transport
for the duration of the operation. If the daemon already owns that transport
port, standalone deploy fails fast instead of racing the daemon.

The current membership path proves durable product state, stable restart-safe
tickets, invite/admission handoff, and three-node membership convergence over
p2panda-net. The bootstrap node publishes durable admitted-peer facts, and
already-joined nodes consume those facts to learn later admitted node authors
without manual local state updates.

`MVP/scripts/three-server-smoke.sh` is the current product vertical proof. It
drives three fresh nodes through init/join/admit, a founder daemon that starts
before peer admission, peer daemon convergence, daemon status readiness,
daemon-owned deploy from founder to `peer-a`, founder-daemon-down gateway/DNS
serving, durable deploy status phases, target-daemon-kill steady-state checks,
and target daemon restart readiness using the `mvp-node` binary as the system
boundary.

## Maintainer Notes

Use [MVP/primitive-decisions.md](primitive-decisions.md) as the living
maintainer map for the architecture Lego pieces. When a slice picks or rejects
an important primitive, record the short why/what-it-replaces/costs/revisit
entry there, and keep the slice report focused on proof and implementation
evidence.

## Non-Negotiables

- Keep the product model from [VISION.md](../VISION.md): explicit operations,
  no hidden control-plane reconcilers, and operator-visible failure.
- Keep HTTP/DNS data-plane serving alive across daemon restarts. Do not preserve
  the old gateway/DNS process or input-model shape just because it exists.
- Treat the daemon as a command/coordinator role. Killing it must not stop
  existing workloads, WireGuard service-to-service traffic, or HTTP/DNS serving
  from last applied state; it should only block new local mutations and expose
  visible coordinator staleness.
- Treat Pingora as a strong HTTP serving candidate, not a non-negotiable.
- Treat SQLite as a rebuildable projection/cache, not cluster truth.
- Treat iroh endpoint identity as transport identity only. Authority comes from
  island grants and signed facts.
- Make the E2E harness a first-class deliverable. A slice is not done until it
  has proof.
- Treat code shape as a success metric. The new foundation must let us express
  real business behavior with far less substrate glue than the previous
  codebase.
- Keep new implementation work isolated under `MVP/` until the foundation has
  earned migration into the existing codebase.
