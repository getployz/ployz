# Two-Machine Acceptance (retired)

The Hetzner two-machine acceptance harness is retired. Its scripts predated
the direct TLS NATS control plane and invoked daemon commands that no longer
exist.

- Architecture decision: [ADR-0013 — v1 uses direct TLS
  NATS](../adr/0013-v1-uses-direct-tls-nats.md).
- Acceptance path: the Docker-in-Docker e2e harness covers the same
  two-machine product flow (first-machine install, machine add/join, deploy,
  gateway routing) plus restart invisibility and auth rejection — see
  [`dind-e2e.md`](dind-e2e.md) and `scripts/dind-e2e.sh`.

The fresh real-host proof against the current install contract lives in
[`real-host-acceptance.md`](real-host-acceptance.md) (`scripts/real-host-acceptance.sh`
and `scripts/cli-smoke-test.sh`).
