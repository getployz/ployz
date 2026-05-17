---
title: Slice 009 Advisory Lease Facts And ACME Canary
status: active
plan: MVP/slice-009-advisory-lease-acme-plan.md
created: 2026-05-17
---

# Slice 009 Advisory Lease Facts And ACME Canary

## Result

This slice implements the first advisory lease canary under the corrected
local-consistency contract.

Implemented shape:

- `mvp-lease` defines advisory lease facts, local command context with visible
  nodes, non-zero epochs, deterministic content hashes, supersession status,
  private guard minting, explicit release, and best-effort RAII drop release.
- Lease guards are tied to their originating `LeaseBook`, and renew/release
  facts carry the winning claim hash so they cannot accidentally mutate a
  superseded same-resource/same-holder/same-epoch claim.
- `mvp-acme` maps ACME HTTP-01 hostname/token ownership onto encoded lease
  resources and requires the current lease epoch before challenge publish/delete.
- ACME challenge records are fenced by the exact winning lease claim hash, and
  challenge tokens/key authorizations are validated in the shape the HTTP-01
  serving path will need.
- `mvp-e2e` includes `lease-acme-contract` in the scenario table and proves the
  ACME canary end to end, including local-only command success without witness
  acks.

The important correction: this is not a distributed lock. There are no witness
acks, no `min_replicas`, no pin-fact commit path, no quorum mode, and no strict
lease mode. Resource-level enforcement remains responsible for real exclusivity.

## Crate Decisions

Checked before implementation:

- `instant-acme` remains the likely future ACME client layer:
  <https://docs.rs/instant-acme/latest/instant_acme/>
- `rustls-acme` is useful for rustls-serving applications but too coupled for
  this ownership primitive:
  <https://docs.rs/rustls-acme/latest/rustls_acme/>
- `scopeguard` is a proven RAII helper, but the MVP guard needs typed release
  facts and domain-visible behavior, so direct `Drop` stayed simpler:
  <https://docs.rs/scopeguard/latest/scopeguard/>

## Proof

Targeted checks run:

```text
cd MVP && cargo check -p mvp-lease -p mvp-acme -p mvp-e2e
cd MVP && cargo test -p mvp-lease -p mvp-acme
cd MVP && cargo run -p mvp-e2e -- lease-acme-contract
cd MVP && cargo clippy --all-targets -- -D warnings
cd MVP && cargo test --all
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
just test
```

Observed `lease-acme-contract` metrics:

```text
first_publish_succeeded: true
visible_nodes_at_decision: 2
conflict_detected: true
expired_takeover_succeeded: true
stale_publish_rejected: true
stale_delete_rejected: true
local_only_publish_succeeded: true
superseded_candidate_count: 1
superseded_local_mutation_rejected: true
release_on_drop_recorded: true
lease_facts_recorded: 2
challenge_records: 1
elapsed_ms: 1
```

## Semantic-Leverage Check

Old ACME reference baseline:

```text
crates/ployzd/src/daemon/cert_coordination.rs: 520 LOC
crates/ployz-cert-backends/src/instant_acme_issuer.rs: 525 LOC
```

New MVP canary:

```text
MVP/lease/src/lib.rs: 1714 LOC
MVP/acme/src/lib.rs: 783 LOC
MVP/e2e/src/lease_acme_contract.rs: 412 LOC
```

This is not a LOC win yet. The useful win is semantic: ACME challenge ownership
now says "acquire advisory lease" and "publish with current epoch" instead of
owning lock/topology behavior itself. The cost is that `mvp-lease` is carrying a
lot of primitive surface and test code. The simplify pass already removed the
wrong quorum/witness shape, bound guards to their book, and fenced renew/release
facts with the claim hash; future slices should keep pushing this primitive
toward a smaller fact-source-backed API.

The future active-member/partition-view idea stays outside this slice. It may
improve the visible-node evidence a command reports later, but it must not
become a hidden quorum or witness-ack gate for lease or fact commits.

Review note: RAII drop remains intentional because this slice is explicitly
testing that shape. It is best-effort local cleanup, not a hard durability or
cluster-exclusion guarantee.

## Remaining Work

- Persist lease facts through iroh-docs instead of the in-memory lease book.
- Materialize or compact per-resource lease reductions if long-lived resources
  with many renewals become a real hot path; Slice 009 only removes the previous
  global fact-log scan.
- Serve HTTP-01 challenge responses through the gateway serving-state path.
- Connect the canary to `instant-acme` order/account flows after the serving
  proof exists.
- Reuse the same advisory lease primitive for deploy ownership only if the next
  deploy slice proves it needs that shape.
