---
title: Slice 039 p2panda Substitution Deletion Audit Plan
status: active
created: 2026-05-19
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/p2panda-substitution-audit.md
  - MVP/slice-038-p2panda-06-canonical-transport.md
external:
  - https://docs.rs/p2panda-net/0.6.0/p2panda_net/
  - https://docs.rs/p2panda-auth/0.6.0/p2panda_auth/
  - https://docs.rs/p2panda-sync/0.6.0/p2panda_sync/
  - https://docs.rs/p2panda-store/0.6.0/p2panda_store/
  - https://docs.rs/p2panda-blobs/0.5.2/p2panda_blobs/
---

# Slice 039 p2panda Substitution Deletion Audit Plan

## Problem Frame

Slice 038 changed the facts on the ground: `p2panda-net 0.6.0` is usable in the
active MVP workspace on non-RC `iroh 0.98`, and live fact-node transport can
move canonical signed p2panda fact operations.

The next risk is continuing to maintain both the older AI-written plumbing and
the p2panda-backed path. The MVP already has enough custom substrate to become
the thing it was meant to replace. This slice is a deep deletion audit before
the next product feature: identify the biggest simplification wins, choose the
next implementation slice, and make the retained-code rationale explicit.

Bias: prefer p2panda-maintained crates for generic substrate, even when they are
pre-`1.0`, because the alternative is more bespoke MVP-local plumbing. Keep
Ployz business semantics above those crates.

## Dependency Scout

Checked on 2026-05-19:

- `cargo search p2panda-net` reports `p2panda-net = "0.6.0"`.
- `cargo info p2panda-net@0.6.0` reports default features for address book,
  iroh endpoint, discovery, gossip, sync, and optional supervision.
- `cargo tree -p mvp-p2panda-transport -i iroh` shows `p2panda-net 0.6.0`
  using non-RC `iroh 0.98.2`.
- `cargo tree -p mvp-iroh -i iroh` also resolves to non-RC `iroh 0.98.2`; the
  old iroh `0.96` conflict is gone in the active workspace.
- `cargo info p2panda-auth@0.6.0` reports the group processor API remains the
  maintained candidate for island membership/revocation.
- `cargo search p2panda-blobs` still reports `p2panda-blobs = "0.5.2"`, and
  the published crate root still only contains a refactor TODO. Do not adopt it
  as payload/blob substrate in this slice.

## Scope

This is an investigation and planning slice. It may include small compile-only
probes if the audit cannot answer a substitution question from existing code
and local crate sources, but it should not migrate product behavior.

In scope:

- Audit every remaining `PFO1`, `PandaFactWireEnvelope`,
  `PandaNetQuarantineLog`, `PandaNetNode`, `ProcessFactSource`,
  `BusFactSource`, and historical `mvp-iroh` fact proof usage.
- Decide which remaining paths can be deleted outright, which should become
  clearly named legacy fixtures, and which still prove a product invariant not
  covered by canonical p2panda paths.
- Evaluate whether `p2panda-net` supervision/address-book/discovery should
  replace MVP-local restart/refresh/replay plumbing before more Kameo actors are
  written around it.
- Evaluate whether `p2panda-auth` should be the next substitution slice for
  durable island membership before machine-add or more authorization work.
- Evaluate whether `p2panda-discovery` should replace custom bootstrap/topic
  discovery ideas or stay behind explicit invite/bootstrap flow for now.
- Produce the next implementation-slice recommendation with proof gates and
  semantic-leverage accounting.
- Update `MVP/design-notes/p2panda-substitution-audit.md`,
  `MVP/primitive-decisions.md`, and `MVP/overall-plan.md`.

Out of scope:

- No product-feature implementation.
- No migration outside `MVP/`.
- No p2panda-blobs adoption unless a newer usable crate release appears during
  the scout.
- No replacement of PloyzBus request/reply, queue groups, no-responder
  semantics, bridge imports/exports, or subject grants.
- No replacement of deploy, ACME, machine, volume, environment, routing, or
  projection business semantics with p2panda data types.

## Audit Units

### Unit 1: Remaining Custom Transport Plumbing

Files:

- `MVP/p2panda-transport/src/lib.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/quarantine_log.rs`
- `MVP/p2panda-transport/src/harness.rs`
- `MVP/p2panda-transport/src/fact_driver.rs`
- `MVP/e2e/src/p2panda_net_sync_contract.rs`
- `MVP/e2e/src/p2panda_net_owned_node_contract.rs`
- `MVP/e2e/src/p2panda_net_fact_node_contract.rs`
- `MVP/e2e/src/p2panda_net_process_serving_contract.rs`

Questions:

- Can `PandaNetNode` and `PandaNetQuarantineLog` be deleted now that
  `PandaNetFactNode` is canonical?
- If not, which exact E2E invariant do they prove that the fact-node contract
  does not?
- Can the remaining direct import probes use canonical
  `Operation<PandaFactExtensions>` helpers instead of `PFO1` bytes?
- Does `p2panda-net` supervision make our stream-refresh workaround obsolete,
  or is refresh still a Ployz process-role concern?

Expected output:

- A deletion table with `delete now`, `legacy fixture`, or `retain` for each
  transport type/API.
- The next implementation plan if deletion is safe.

### Unit 2: Remaining Custom Fact Sources

Files:

- `MVP/bus/src/facts.rs`
- `MVP/projection/src/bus_source.rs`
- `MVP/e2e/src/process_fact_source.rs`
- `MVP/iroh/src/facts.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e-proof-plan.md`

Questions:

- Which scenarios still use custom bus/process/iroh fact sources?
- Are those scenarios still product proof, or historical proof superseded by
  p2panda persistent stores and p2panda-net fact-node process serving?
- Can `mvp-e2e -- all` stop running historical iroh-docs/fake-source proofs
  once the equivalent p2panda proof is named?
- Should `mvp-iroh` remain for blobs/router experiments only, with facts
  explicitly parked?

Expected output:

- A caller map and replacement/deletion recommendation.
- A concrete test-list change proposal if any scenario should leave `all`.

### Unit 3: Membership And Authorization Substitution

Files:

- `MVP/p2panda-authz/src/lib.rs`
- `MVP/p2panda-facts/src/lib.rs`
- `MVP/mesh/src/*.rs`
- `MVP/machine/src/*.rs`
- `MVP/bus/src/grants.rs`

Questions:

- Should the next implementation slice be durable `p2panda-auth` membership
  operations, replacing manual trusted author/replica maps on product paths?
- What Ployz-owned signed membership envelope is still needed around
  p2panda-auth?
- What cannot be moved into p2panda-auth: subject grants, temporary response
  permissions, bridge imports/exports, command-entry conflicts?

Expected output:

- A go/no-go recommendation for making `p2panda-auth` the next implementation
  slice after transport deletion.
- Exact proof gates for root add, writer demotion/removal, active writer sync
  scopes, replica importer authority, and post-removal fact rejection.

### Unit 4: Discovery, Address Book, And Process Roles

Files:

- `MVP/mesh/src/invite.rs`
- `MVP/p2panda-transport/src/node.rs`
- `MVP/p2panda-transport/src/fact_node.rs`
- `MVP/e2e/src/process_role_harness.rs`
- local `p2panda-net 0.6.0` and `p2panda-discovery 0.6.0` crate sources

Questions:

- Is the p2panda address book the right substrate for Ployz visible-node
  evidence, or should visible nodes remain explicit command-time probes?
- Can p2panda discovery help after invite/bootstrap without weakening the
  product rule that command results name visible nodes at decision time?
- Should process-role liveness and stream health be modeled through
  p2panda-net supervisor events, Kameo actors, or a thin bridge between them?

Expected output:

- A decision note separating transport node discovery from command consistency.
- A recommendation on whether to adopt p2panda-net supervision now or defer
  until the Kameo process-role slice.

### Unit 5: Semantic-Leverage Accounting

Files:

- `MVP/design-notes/semantic-leverage-loc.md`
- `MVP/design-notes/p2panda-substitution-audit.md`

Questions:

- Which retained plumbing has the highest LOC and cognitive cost?
- Which next deletion gives the biggest maintenance reduction without erasing
  product proof?
- Are p2panda adoption boundaries still small enough that pre-`1.0` API churn is
  cheaper than maintaining custom code?

Expected output:

- A before/after estimate for the recommended deletion slice.
- A list of Ployz-owned semantics that must not shrink during deletion.

## Success Criteria

- The plan answers "can we use p2panda-net without RC iroh?" with the current
  branch evidence: yes, through `p2panda-net 0.6.0` and `iroh 0.98.2`.
- Every remaining custom substrate path has an owner decision: delete, legacy
  fixture, retain as product primitive, or investigate with a compile probe.
- The next implementation slice is chosen by expected maintenance reduction and
  proof risk, not by old backlog order.
- The audit names the exact E2E scenarios that must change before any legacy
  path can be deleted.
- The audit keeps Ployz business semantics explicit and does not outsource them
  to p2panda.

## Verification

Docs-only audit plan gate:

```text
git diff --check -- MVP/slice-039-p2panda-substitution-deletion-audit-plan.md MVP/overall-plan.md MVP/architecture.md MVP/primitive-decisions.md
```

No cargo test is required for this plan commit. The resulting implementation
slice must run the targeted p2panda and E2E gates it names.
