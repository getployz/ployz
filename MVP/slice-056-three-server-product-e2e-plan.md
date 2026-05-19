# Slice 056: Three-Server Product E2E

## Goal

Add a black-box MVP product smoke that behaves more like the main product
`ployz-e2e` harness: scenario-oriented, product-binary driven, artifact
producing, and responsible for cleanup.

## Scope

- Add a `three-server-product` scenario to `mvp-e2e`.
- Add a reusable product harness for invoking `mvp-node`, spawning serving
  roles, probing HTTP/DNS, and collecting command output.
- Drive three fresh product nodes through init, invite, join, admit, concurrent
  daemon runs, deploy, gateway/DNS serving, and daemon-kill steady-state proof.
- Emit a JSON proof artifact under `MVP/target/mvp-e2e/three-server-product/`.
- Add `MVP/scripts/three-server-smoke.sh` as the documented local command.

## Non-Goals

- No SSH/remote-host mode in this slice.
- No kernel WireGuard proof. This smoke uses the host-network backend from the
  current product vertical.
- No separate-machine packaging proof. The smoke now runs remote deploy
  participant RPC between separate product node processes on one host.

## Product-E2E Pattern Borrowed

- Scenario registry names the behavior under test.
- Runner owns setup, process spawning, probes, cleanup, and artifacts.
- The scenario calls product commands instead of library APIs.
- Failures include command stdout/stderr and write durable artifacts when the
  run succeeds.
