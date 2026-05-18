---
title: Slice 040 Delete Opaque p2panda-net Transport
status: completed
created: 2026-05-19
origin:
  - MVP/slice-040-delete-opaque-p2panda-net-transport-plan.md
  - MVP/slice-039-p2panda-substitution-deletion-audit.md
---

# Slice 040 Delete Opaque p2panda-net Transport

## Result

The active p2panda-net fact transport now has one product-shaped path:
`PandaNetFactNode` carrying canonical `Operation<PandaFactExtensions>` values
through p2panda-net `LogSync`.

Deleted from active Rust sources:

- `PandaFactWireEnvelope`, `PandaFactWireEnvelopeError`, and the `PFO1` codec.
- `import_fact_body` and `import_fact_body_into_shared_store`.
- `PandaNetNode`, `PandaNetStream`, `PandaNetQuarantineLog`, and
  `PandaNetStore`.
- `transport_wire_bodies`, `PandaNetWireTransportConfig`, and the legacy
  opaque-body E2E scenarios.

Kept:

- `PandaNetFactNode` and its typed node config/ticket/topic wrappers.
- Branchable import outcomes: imported, duplicate, conflict, deferred, failed,
  and structured rejections.
- Direct harness probes, but only at canonical p2panda operation boundaries.

## Coverage Decision

`p2panda-net-sync-contract`, `p2panda-net-owned-node-contract`, and the
non-process `p2panda-net-acme-http01-contract` only proved the deleted opaque
body path after Slice 038. Their product-relevant checks are now covered by:

- `p2panda-net-fact-node-contract` for canonical p2panda-net fact transport,
  conflict candidates, scoped import rejection, projection rebuild, and serving
  snapshot generation. It also publishes ACME lease/present/clear facts through
  live `PandaNetFactNode` transport and proves HTTP-01 serving returns 200
  before clear and 404 after clear.
- `p2panda-net-process-serving-contract` for process-role canonical net serving,
  delayed remote update, untrusted-author rejection classification, projection
  rebuild, restart, and missing coordinator socket behavior.
- `p2panda-acme-http01-contract` for ACME command, lease, p2panda fact sync,
  projection, and last-good serving semantics.

## Deletion Gate

This grep must stay empty for active Rust sources:

```text
rg "PandaFactWireEnvelope|PFO1|PandaNetQuarantineLog|transport_wire_bodies|import_fact_body" \
  MVP/p2panda-facts MVP/p2panda-transport MVP/e2e/src
```

## LOC Ledger

Rough diff at implementation time:

```text
active Rust deleted:       ~2,193 lines
active Rust added:           ~486 lines
net deletion:             ~1,707 lines
```

This is the first p2panda substitution slice with a direct substrate deletion
win. The maintenance win is not just LOC: p2panda-net now owns the network log
mechanics, while Ployz owns the fact envelope, authority checks, projection,
and product command semantics.

## Next Substitution Target

The next p2panda substitution slice should investigate durable
`p2panda-auth` membership operations replacing manual trusted author and
replica maps on product-shaped paths. Do not reintroduce custom transport
plumbing while doing that work.
