---
title: Slice 004 Authority Bridge Import/Export Plan
status: active
created: 2026-05-17
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/slice-003-authority-islands.md
---

# Slice 004 Authority Bridge Import/Export Plan

## Problem Frame

Slice 003 proved that authority islands are isolated: subjects, queues, request
responders, response permits, grants, and facts are scoped to one island. The
next missing proof is the explicit exception to that isolation.

The MVP architecture says laptop/dev and prod are separate authority islands,
and they share only selected services or message streams through import/export
rules. Without that bridge primitive, later service registry, iroh-docs facts,
machine join, and deploy work would either stay trapped inside one island or
invent ad hoc cross-island shortcuts.

The single proof target for this slice is:

> A laptop island can import a prod deploy service and an exported prod status
> stream without gaining direct authority over prod facts or hidden access to
> unrelated prod subjects.

## Why This Is Next

This is the smallest slice that exercises the central "NATS-shaped, but not a
NATS topology" claim after basic island isolation.

Service registry is not next because, until imports/exports exist, a registry
can only describe local interest. iroh-docs is not next because replicated fact
storage needs to know which principals and islands are allowed to read or write
truth. iroh transport is also not next because the in-memory authority semantics
should be stable before a network substrate makes bugs look like connectivity
problems.

The bridge slice should stay self-contained and in memory. It proves the
product contract first, then later slices can replace the local bridge harness
with iroh streams, docs-backed bridge rules, and service-registry facts.

## Requirements Traceability

- `MVP/overall-plan.md`: bridges import/export explicit subjects; maintainers
  need documentation for why each primitive exists; all work stays under
  `MVP/`.
- `MVP/architecture.md`: authority islands use imports/exports and do not merge
  databases; local subject names should remain natural; service imports and
  stream exports are distinct concepts.
- `MVP/e2e-proof-plan.md`: E2E-3 requires laptop-to-prod deploy service import,
  prod status stream export, direct prod fact-write denial, and foreground
  bridge outage failure.
- `MVP/slice-003-authority-islands.md`: Slice 004 should use the existing
  authority boundary instead of inventing a second authority model.
- `VISION.md`: mutating work remains foreground and operator-visible; no hidden
  background component should queue remote mutation intent.

## Scope

Implement MVP-local authority bridge semantics:

- service imports for request/reply from one island subject to a remote island
  subject,
- stream exports/imports for one-way publish visibility from a remote island
  into a local subject shape,
- explicit subject remapping for bridge rules,
- bridge availability state with typed foreground failure,
- bridge-principal grant enforcement on both sides of a crossing,
- local-side grants for callers and subscribers,
- loop prevention for bridged stream messages,
- E2E and scale proof that unrelated subjects do not leak between islands.

Out of scope for this slice:

- iroh transport, endpoint identities, ALPN protocols, or QUIC stream framing,
- iroh-docs-backed bridge rule replication,
- service registry facts or `$SYS.service.*` discoverability,
- bridge activation tokens, delegated offline authorization, or private export
  JWT-like tokens,
- durable stream replay or JetStream-like persistence,
- gateway/DNS snapshot work,
- machine join/remove,
- deploy state machine beyond a mocked `deploy.submit` service request.

## Current Patterns To Preserve

- Business-facing calls go through `BusActorHandle`.
- The synchronous in-memory bus remains available only through
  `mvp_bus::harness::InMemoryBus` for contract and scale proof.
- `BusAuthority` may configure grants and bridge rules for tests/bootstrap, but
  future business logic should not inspect grants or bridge internals.
- Authorization failures stay structured. Tests should branch on variants, not
  display strings.
- Facts remain island-local. A bridge can request a prod service; it cannot turn
  a laptop principal into a prod fact writer.
- Large-load tests remain part of the local MVP gate.
- All files created or changed by this slice remain under `MVP/`.

## Crate Scout

The slice would otherwise need subject remapping, read-heavy bridge rule lookup,
bridge lifecycle cancellation, and semantic references for account imports and
exports.

Checked options:

- `arc-swap`: useful for atomically publishing read-mostly routing or bridge
  rule snapshots. Defer for this slice because the current in-memory bus already
  owns state behind one mutex, and adding a second concurrent rule snapshot
  would create a new synchronization model before implementation proves the
  mutex is the bottleneck. Copy the idea: bridge rules should become immutable
  snapshots so a later actor or transport layer can swap them cheaply.
- `tokio-util::sync::CancellationToken`: appropriate for future bridge tasks,
  outage propagation, and graceful shutdown. Defer until the bridge has async
  transport tasks to cancel.
- `async-nats`: useful as a semantic reference for services and subjects, but
  do not add it as a runtime dependency. This MVP is explicitly avoiding a NATS
  server/client topology.
- `matchit` and `globset`: defer. They solve URL/path routing or filesystem
  glob sets, not NATS token semantics. Reuse `Subject` and `SubjectPattern`,
  then add a small typed `SubjectTransform` if remapping needs captures.
- `iroh`: keep as the planned transport substrate, but do not pull it into this
  slice. The bridge contract should exist before streams and ALPN are wired.

Decision for this slice: add no new crate unless implementation proves
`SubjectTransform` or rule snapshots cannot stay simple. Record the bridge
primitive in `MVP/primitive-decisions.md` after implementation proof lands.

Sources:

- NATS accounts document service and stream exports/imports, and clarify that
  account import/export "streams" are Core NATS message streams, not JetStream:
  <https://docs.nats.io/running-a-nats-service/configuration/securing_nats/accounts>
- NATS subject mapping documents wildcard capture and remapping syntax; copy the
  idea, not the full mapping language:
  <https://docs.nats.io/nats-concepts/subject_mapping>
- NATS request/reply documents inbox replies and immediate no-responder errors:
  <https://docs.nats.io/nats-concepts/core-nats/reqreply>
- NATS authorization documents subject allow/deny, queue permissions, and
  temporary response permissions:
  <https://docs.nats.io/running-a-nats-service/configuration/securing_nats/authorization>
- NATS services document service names, endpoints, groups, and discovery
  operations; full service registry remains a later slice:
  <https://docs.nats.io/using-nats/developer/services>
- `arc-swap` documents atomic `Arc` replacement for read-mostly routing/config
  snapshots:
  <https://docs.rs/arc-swap/latest/arc_swap/>
- `tokio-util` documents `CancellationToken` for signalling cancellation across
  tasks:
  <https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html>
- iroh docs show ALPN-selected QUIC connections and cheap interleaved streams;
  that remains the later distributed transport target:
  <https://docs.rs/iroh/latest/iroh/>

## Key Technical Decisions

### Bridge Rules Are Authority Data

Bridge rules belong to the authority surface, not to transport. A future iroh
bridge will carry bytes, but this slice decides whether bytes are allowed to
cross islands at all.

For this in-memory slice, `BusAuthority` is the MVP-local authority surface
because `AuthorityIslandActor` is not split out yet. Bridge rule types live in
`mvp_bus` as authority-domain data, not transport data.

The MVP-local API should make this visible:

- a service import names local island, local request subject, remote island,
  remote service subject, and bridge principal,
- a stream import/export pair names remote island, remote exported pattern,
  local island, local subject mapping, and bridge principal,
- disabling the bridge changes foreground service-request behavior immediately,
- bridge-origin metadata identifies source island, original subject, and bridge
  rule id for imported messages so loop prevention and future audit do not
  depend on hidden call-path state.

### Service Imports Are Request/Reply Only

`gpu.deploy.submit` in the laptop island maps to `deploy.submit` in prod for a
request. It should not imply that ordinary laptop `publish(gpu.deploy.submit)`
becomes a prod publish.

This keeps mutating remote work foreground. If prod is unavailable or the import
rule is disabled, the request returns a typed bridge failure or `NoResponders`;
the bus must not enqueue remote mutation intent for later.

The bridged reply permit is remote-scoped for responder authorization, one-use,
deadline-bound, and wired only to the original local request receiver. Prod
responders do not receive a local inbox subscription, and the bridge principal
is not allowed to answer merely because it forwarded the request.

### Stream Exports Are One-Way Notifications

Prod `deploy.<id>.status` can be exported into laptop as
`prod.deploy.<id>.status`. That is a visibility rule, not shared truth.

Stream delivery must not recursively trigger bridge rules. A bridged stream
message is delivered to local subscribers and stops there unless a future slice
explicitly designs multi-hop behavior.

### Remote Grants Still Apply

A bridge rule is not a bypass around prod grants. The bridge forwards through a
prod-side bridge principal, and the existing prod grants decide whether that
principal may publish the imported service request subject. Prod responders must
still have normal subscribe/queue and response grants.

Laptop principals still need local publish permission on the imported local
subject. Laptop subscribers still need local subscribe permission on imported
stream subjects. Imported stream delivery also requires the bridge principal to
have remote export authority on the prod subject and local publish authority on
the mapped laptop subject before local subscribers receive it.

### Subject Remapping Starts Small

Use a small `SubjectTransform` only for the mappings this slice needs:

- exact service subject mapping, such as `gpu.deploy.submit` to `deploy.submit`,
- wildcard stream prefix mapping, such as `deploy.*.status` to
  `prod.deploy.*.status`.

Do not copy the full NATS mapping expression language yet. If a transform would
drop wildcard captures or become ambiguous, reject it during rule registration.

## Implementation Units

### U1: Bridge Domain Model

Goal: introduce explicit service-import and stream-export rule types without
transport coupling.

Files:

- Create: `MVP/bus/src/bridge.rs`
- Modify: `MVP/bus/src/lib.rs`
- Modify: `MVP/bus/src/error.rs`
- Test: `MVP/bus/src/bridge.rs`

Approach:

- Add typed bridge rule identifiers if useful for error messages and metrics.
- Add `ServiceImport`, `StreamExport`, `StreamImport`, `BridgeRuleSet`,
  `BridgeState`, `BridgePrincipal`, `BridgeOrigin`, and `SubjectTransform`
  types.
- Validate no self-imports.
- Validate that service imports use exact local and remote subjects for this
  slice.
- Validate that stream mappings preserve wildcard captures and are
  bidirectionally unambiguous enough for E2E proof.
- Reject duplicate or overlapping local service imports unless a later plan
  defines deterministic precedence.
- Reject ambiguous stream bridge rules that can map the same remote publish into
  the same local subject more than once.
- Add structured errors for invalid rules, disabled bridge paths, explicit
  remote-unavailable bridge state, and forbidden bridge crossings.
- Treat availability as bridge-rule state managed by `BusAuthority`, not as an
  inferred island liveness registry. For this slice, "no prod responder" remains
  `NoResponders`; `RemoteUnavailable` only occurs when the bridge state is set
  that way by test/bootstrap authority.

Test scenarios:

- Exact service import validates.
- Self-import is rejected.
- Duplicate local service import is rejected.
- Stream import with a preserved wildcard capture validates.
- Stream import that drops a wildcard capture is rejected.
- Overlapping stream exports/imports that would duplicate local delivery are
  rejected.
- Disabled bridge state returns a typed bridge availability failure.
- Explicit remote-unavailable bridge state returns a typed foreground failure.
- Error variants carry local island, remote island, and subject context.

### U2: In-Memory Service Import Request Path

Goal: route a local request through an explicit service import into the remote
island while preserving local and remote authorization.

Files:

- Modify: `MVP/bus/src/memory.rs`
- Modify: `MVP/bus/src/actor.rs`
- Modify: `MVP/bus/src/grants.rs` only if a narrow bridge administration grant
  is needed
- Test: `MVP/bus/src/memory.rs`
- Test: `MVP/bus/src/actor.rs`

Approach:

- Add bridge rule setup through bootstrap/test authority methods, not through
  feature business code.
- When `request` targets a local service-import subject, forward through the
  configured remote island and remote subject.
- Require the requester to be authorized to publish the local imported subject.
- Require the bridge principal to be authorized to publish the remote subject.
- Build the forwarded request as a remote `BusMessage` with the remote island,
  remote subject, and bridge principal as publisher, while preserving the local
  requester's response channel and deadline.
- A bridged `ResponseMessage` reports the remote responder's island and
  principal. Its `request_id` remains the original logical request id so the
  local requester can correlate the response without learning remote inbox
  internals.
- The reply permit for the forwarded request is scoped to the remote island and
  remote responder authorization, remains one-use and deadline-bound, and sends
  replies only to the original local request receiver.
- Let the existing remote responder selection, queue-group behavior, response
  permit, timeout, and no-responder logic run in the remote island.
- Make ambiguous local subject ownership explicit. If a local service import is
  installed, registering a local responder on the same exact subject should be
  rejected or tested as a deliberate precedence rule; do not let local fallback
  hide bridge behavior.

Test scenarios:

- Laptop request to `gpu.deploy.submit` reaches prod `deploy.submit`.
- Prod queue group still delivers to exactly one scheduler.
- Prod responder receives prod island context.
- Laptop requester receives one response through the existing request API.
- Bridged response fields report the prod responder island/principal and the
  original logical request id.
- Duplicate bridged responses fail with the existing duplicate-response
  behavior.
- Expired bridged responses fail with the existing response deadline behavior.
- A wrong-principal bridged response fails remote response authorization.
- The bridge principal cannot respond to the imported request unless it is the
  selected prod responder.
- Missing prod responder returns `NoResponders` for the imported target.
- Disabled bridge returns a typed bridge failure before remote handler dispatch.
- Missing local publish grant fails before bridge forwarding.
- Missing prod bridge-principal grant fails before prod handler dispatch.

### U3: In-Memory Stream Export/Import Path

Goal: deliver exported prod status messages into laptop through an explicit
stream import without leaking unrelated subjects or facts.

Files:

- Modify: `MVP/bus/src/memory.rs`
- Modify: `MVP/bus/src/message.rs` for bridge-origin metadata
- Modify: `MVP/bus/src/actor.rs`
- Test: `MVP/bus/src/memory.rs`
- Test: `MVP/bus/src/actor.rs`

Approach:

- On an authorized remote publish, evaluate matching stream exports/imports.
- Remap the subject into the local island and deliver to authorized local
  subscribers.
- Treat the local delivered message as authored by the local bridge principal
  and attach required bridge-origin metadata with source island, original
  subject, and bridge rule id. Do not let local subscribers mistake the message
  for durable prod truth.
- Require the remote bridge principal to have explicit export authority for the
  matched remote subject before any data crosses islands. This may be modeled as
  a narrow bridge-export grant or as a dedicated stream-export authority check,
  but it must not be inferred from the original publisher's grant alone.
- Require the local bridge principal to have publish authority for the mapped
  local subject before delivering an imported stream message.
- A disabled stream bridge does not fail the original prod publish. The prod
  publish still delivers to prod-local subscribers; imported delivery is skipped
  and counted in bridge metrics. Request/reply imports are the foreground
  failure path.
- Build stream bridge deliveries while `Inner` is constructing the publish
  delivery plan, producing explicit remapped `Delivery` entries. Do not
  implement stream bridging by recursively calling `publish` or
  `publish_until`.
- Prevent bridge recursion for already-bridged stream deliveries.
- Do not let stream imports create request/reply capability.

Test scenarios:

- Prod `deploy.d1.status` publishes into laptop
  `prod.deploy.d1.status`.
- Laptop subscribers with matching grants receive the imported status.
- Imported status message carries bridge-origin metadata with prod as source
  island, `deploy.d1.status` as original subject, and the applied bridge rule
  id.
- Non-exported prod subjects do not reach laptop.
- Exported prod subject does not reach laptop if the remote bridge principal
  lacks export authority on the prod subject.
- Exported prod subject does not reach laptop if the local bridge principal
  lacks publish authority on the mapped laptop subject.
- Disabled stream bridge skips imported delivery while prod-local publish still
  succeeds.
- A laptop publish on `prod.deploy.d1.status` does not become a prod publish.
- A bridged stream message does not trigger a second bridge delivery.
- A laptop principal still cannot write prod facts directly.

### U4: Bridge E2E Contract

Goal: add the product-shaped authority bridge proof from E2E-3.

Files:

- Create: `MVP/e2e/src/bridge_contract.rs`
- Modify: `MVP/e2e/src/main.rs`
- Modify: `MVP/e2e/src/bus_syntax.rs` only for clear helper names
- Modify: `MVP/e2e/src/assertions.rs` only for shared error assertions
- Update after implementation: `MVP/README.md`
- Create after implementation: `MVP/slice-004-authority-bridge.md`
- Update after implementation: `MVP/primitive-decisions.md`

Approach:

- Add `cargo run -p mvp-e2e -- bridge-contract`.
- Include `bridge-contract` in `cargo run -p mvp-e2e -- all`.
- Write `MVP/target/mvp-e2e/bridge-contract-metrics.json`.
- Keep the test language product-shaped: laptop imports prod deploy service,
  prod exports deploy status, laptop cannot mutate prod facts.

Test scenarios:

- Laptop imports `gpu.deploy.submit` from prod `deploy.submit`.
- Laptop request reaches exactly one prod scheduler in queue group `schedulers`.
- Prod scheduler observes prod island context.
- Laptop receives the response through the normal request API.
- Prod status stream `deploy.<id>.status` is delivered to laptop as
  `prod.deploy.<id>.status`.
- Imported status carries bridge-origin metadata.
- Missing remote export authority or local bridge publish authority prevents
  imported stream delivery.
- Non-exported prod subject has zero laptop deliveries.
- Laptop direct write to prod fact key remains rejected.
- Disabled bridge returns a typed foreground failure and does not queue work.
- Cross-island leakage count is zero.

Metrics:

- bridged service requests,
- bridged service responses,
- bridged stream deliveries,
- denied bridge attempts,
- bridge outage failures,
- direct prod fact-write denials,
- cross-island leakage count, which must be zero.

### U5: Bridge Scale And Simplicity Proof

Goal: keep the bridge primitive honest under large logical-node load and record
whether it improves business-code semantics.

Files:

- Modify: `MVP/e2e/src/scale.rs`
- Update after implementation: `MVP/slice-004-authority-bridge.md`
- Update after implementation: `MVP/primitive-decisions.md`

Approach:

- Preserve existing 200, 1,000, and 10,000 logical-node bus scale cases.
- Add bridge stream fanout at 200, 1,000, and 10,000 laptop subscribers from
  one prod status publish.
- Add a service-import load case with many laptop requests to a prod queue
  group, proving each request reaches one prod responder and no unrelated island
  receives it.
- Record whether the laptop-to-prod deploy-submit business behavior lives in
  the bridge primitive plus one E2E scenario, rather than scattered transport
  or fact-store special cases.

Test scenarios:

- 200, 1,000, and 10,000 imported stream subscribers all receive the mapped
  status publish.
- Cross-island leakage remains zero at each size.
- Imported service requests distribute across prod queue responders without
  local fallback.
- Bridge-disabled requests fail quickly and do not leave queued deliveries.

Metrics:

- bridge stream delivery p50/p95/p99,
- service-import request p50/p95/p99,
- queue responder skew for imported service requests,
- memory after bridge scale run,
- cross-island leakage count.

## Expected Review Risks

- Security/authorization: bridge forwarding could accidentally bypass remote
  grants or turn a laptop principal into a prod actor.
- Correctness: request import fallback could hide local/no-responder behavior or
  make local and remote responders race.
- Reliability: bridge outage must be a foreground failure for request/reply,
  not a background retry loop that mutates prod later.
- Maintainability: bridge rules could become a second authority model instead
  of using the existing island/grant/session model.
- Performance: stream export lookup could add unnecessary work to every publish
  if implemented as a broad global scan without tests watching 10,000-node
  behavior.
- Simplicity: copying the full NATS subject mapping language would be premature.
  This slice needs exact service mapping and simple wildcard-preserving stream
  mapping only.

## Maintainer Documentation

After implementation, update `MVP/primitive-decisions.md` with a new bridge
entry that explains:

- why bridge import/export exists,
- why it replaces broad cross-island subject prefixes and hidden shared state,
- why it is not the service registry,
- why no new crate was added for mapping/rule snapshots in this slice,
- what would make `arc-swap`, `tokio-util::CancellationToken`, or delegated
  bridge tokens worth revisiting.

Create `MVP/slice-004-authority-bridge.md` as the implementation report with:

- proof results and metrics,
- review and simplification outcomes,
- semantic-leverage notes for laptop-to-prod deploy submission,
- future documentation problems that are still speculative, such as a fuller
  ADR archive or public bridge operator guide.

## Verification

Targeted local checks for the implementation slice:

```text
cd MVP && cargo fmt --all -- --check
cd MVP && cargo test -p mvp-bus bridge
cd MVP && cargo test -p mvp-bus
cd MVP && cargo run -p mvp-e2e -- bridge-contract
cd MVP && cargo run -p mvp-e2e -- scale
cd MVP && just test
```

The slice is not complete until:

- `bridge-contract` passes,
- existing `bus-contract`, `actor-contract`, `authority-contract`, and `scale`
  still pass,
- bridge scale metrics are written under `MVP/target/mvp-e2e/`,
- simplification review has checked that bridge semantics are easy for future
  business logic to use,
- code review has checked security, correctness, performance, reliability,
  maintainability, and project standards,
- all changes remain under `MVP/`.

## Follow-Up Candidates

Do not start these inside Slice 004 unless implementation proves the bridge
contract cannot stand without them:

- docs-backed bridge rule replication,
- service registry facts and `$SYS.service.*` discoverability,
- iroh stream transport for remote bridge forwarding,
- bridge activation tokens or delegated invite credentials,
- full subject mapping language,
- gateway/DNS snapshot proof,
- deploy commit-before-drain proof.
