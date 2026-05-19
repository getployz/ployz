# Slice 056: Three-Server Product E2E

## Summary

`mvp-e2e` now has a `three-server-product` scenario that launches the shipped
`mvp-node` binary as the system boundary. The scenario initializes three nodes,
invites and admits two peers, runs all three daemons concurrently over the real
p2panda-net fact transport, deploys a trivial HTTP service to `peer-a` through
the product node-agent request/reply path after daemon status readiness, starts
product gateway/DNS roles, probes HTTP and DNS, kills the target daemon, and
probes HTTP/DNS again.

## Proof Artifact

The scenario writes:

```text
MVP/target/mvp-e2e/three-server-product/three-server-product-report.json
```

The report includes command stdout/stderr, daemon import counts, node-agent
handler counts, remote deploy target, daemon status output, deploy output,
gateway/DNS listen addresses, before/after daemon-kill HTTP/DNS probes, and
runtime cleanup count.

## Command

```text
MVP/scripts/three-server-smoke.sh
```

The script builds `mvp-node` and `mvp-e2e`, sets `MVP_NODE_BIN`, and runs:

```text
cargo run -p mvp-e2e -- three-server-product
```

## Remaining Gap

The local smoke is still multi-process on one host, not SSH or separate
machines. It now proves the product binary boundary, real p2panda-net
membership convergence, remote node-agent deploy participation, and
gateway/DNS/service survival after the target daemon exits.
