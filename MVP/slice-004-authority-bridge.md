---
title: Slice 004 Authority Bridge
status: completed
plan: MVP/slice-004-authority-bridge-plan.md
completed: 2026-05-17
---

# Slice 004 Authority Bridge

## Result

This slice adds explicit cross-island import/export semantics to the MVP bus.

The new bridge primitive supports:

- service imports from a local subject to a remote island subject,
- stream imports from a remote subject pattern into a local subject shape,
- subject transforms that preserve wildcard captures,
- bridge-origin metadata on imported service requests and stream messages,
- explicit bridge state for disabled or unavailable service imports,
- bridge-principal grant checks on both sides of a crossing,
- endpoint-shaped service-import construction to make local/remote authority
  boundaries explicit,
- rule validation that rejects self-imports, duplicate rule IDs, duplicate
  service imports, local responder/import conflicts, and ambiguous stream
  mappings.

The important product proof is now direct: a laptop island can request a prod
deploy service and receive exported prod deploy status without gaining direct
write authority over prod facts or hidden access to unrelated prod subjects.

## Crate Decision

No new crate was added.

`arc-swap` remains a good candidate for future read-mostly bridge rule
snapshots, but this slice keeps rules inside the in-memory bus state so there is
only one synchronization model to reason about. Subscriber indexes by island now
keep many matching bridge rules from scanning every local subscriber.
`tokio-util::CancellationToken`
remains a good fit for future async iroh bridge tasks, but there are no bridge
tasks to cancel yet. `async-nats` remains a semantic reference only; adding it
would reintroduce the topology this MVP is trying to avoid.

The decision is recorded in
[MVP/primitive-decisions.md](primitive-decisions.md).

## Proof

Checks run for this slice:

```text
cd MVP && cargo test -p mvp-bus
cd MVP && cargo run -p mvp-e2e -- bridge-contract
cd MVP && cargo run -p mvp-e2e -- scale
cd MVP && just test
```

Results:

- `mvp-bus`: 96 unit tests passed.
- `bridge-contract`: passed and wrote
  `MVP/target/mvp-e2e/bridge-contract-metrics.json`.
- `scale`: passed and wrote `MVP/target/mvp-e2e/scale-metrics.json`.
- `just test`: passed after simplification and multi-agent review fixes.
- Existing scale proof still covers 200, 1,000, and 10,000 logical nodes for
  publish and request-many.
- New bridge stream scale proof covers 200, 1,000, and 10,000 imported stream
  subscribers with zero cross-island leakage.
- New bridge service scale proof covers 100 laptop service-import requests to a
  prod scheduler queue group with 100 observed deliveries and 100 unique
  responders.
- Rule-volume proof covers 10,000 service imports, 20,000 stream imports, one
  indexed single-rule stream publish, and one 10,000-rule matching stream fanout
  with zero leakage.

Observed local scale metrics from the current run:

```text
bridge stream 10k subscribers:
  deliveries: 200000/200000
  p50: 94719us
  p95: 95679us
  p99: 102463us
  cross-island leakage: 0

bridge service import:
  requests: 100
  deliveries: 100
  unique responders: 100
  p50: 122us
  p95: 138us
  p99: 146us

bridge rule volume:
  service imports: 10000
  stream imports: 20000
  matching stream deliveries: 10000/10000
  matching stream publish: 156484us
  leakage: 0
```

Review fixes landed before completion:

- service imports now dispatch to exactly one remote responder,
- exact-subject `request_many(..., max=1)` can use an imported service,
- `request_many(..., max>1)` on an imported service fails with a bridge-specific
  structured error,
- duplicate bridge rule IDs are rejected globally,
- stream-source overlap is rejected for the same source/local island pair,
- service-import requests carry bridge origin with source principal,
- local subscriber and queue-subscriber conflicts are covered in both
  registration orders,
- service-import `RemoteUnavailable` is covered separately from `Disabled`,
- queue groups are explicitly tested as island-scoped.

## Semantic-Leverage Check

Business rule: "laptop can ask prod to deploy, but cannot write prod truth."

After the bridge primitive exists, that rule is expressed as one service import,
one stream import, normal grants, and one E2E scenario:

- laptop publishes a foreground request to `gpu.deploy.submit`,
- the bridge maps it to prod `deploy.submit`,
- prod scheduler queue group chooses one scheduler,
- prod emits `deploy.d1.status`,
- laptop receives `prod.deploy.d1.status` with bridge-origin metadata,
- direct prod fact write by an ungranted laptop principal is rejected.

No transport branch, service registry shortcut, mutable fact head, or special
case in deploy business logic is needed.

## Follow-Up

- Replace in-memory bridge rules with docs-backed facts after fact replication
  exists.
- Move bridge forwarding onto iroh streams after the semantic contract is stable.
- Add service registry facts and `$SYS.service.*` discovery on top of the bridge
  model rather than beside it.
- Keep recording maintainer-facing primitive decisions in
  `MVP/primitive-decisions.md`; a larger ADR archive is still not necessary.
