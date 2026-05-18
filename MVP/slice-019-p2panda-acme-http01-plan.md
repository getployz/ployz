---
title: Slice 020 P2panda-Backed ACME HTTP-01 Plan
status: parked
created: 2026-05-18
origin:
  - VISION.md
  - MVP/overall-plan.md
  - MVP/architecture.md
  - MVP/e2e-proof-plan.md
  - MVP/primitive-decisions.md
  - MVP/design-notes/p2panda-substitution.md
  - MVP/design-notes/phased-command.md
  - MVP/slice-009-advisory-lease-acme-plan.md
  - MVP/slice-015-docs-backed-acme-http01-plan.md
  - MVP/slice-018b-p2panda-fact-substrate-plan.md
---

# Slice 020 P2panda-Backed ACME HTTP-01 Plan

> Parked behind
> [MVP/design-notes/p2panda-substitution-audit.md](design-notes/p2panda-substitution-audit.md).
> ACME remains the next product canary, but the audit moved persistent
> p2panda fact storage, derived-index rebuild, and a restartable fact-store
> role ahead of this slice so ACME does not harden the current in-memory fact
> boundary.

## Problem Frame

Slice 015 proved ACME HTTP-01 ownership and serving through iroh-docs-backed
facts. Slice 018b changed the long-term fact-substrate direction: new durable
fact work should use the p2panda operation boundary instead of hardening more
custom docs wrappers.

This intentionally supersedes older `iroh-docs facts` wording that still
appears in parts of `MVP/architecture.md` for new fact-substrate work. The
architecture update is part of this slice's documentation unit; serving,
projection, and command semantics stay the same, but the preferred durable
operation envelope changes.

The next ACME proof should therefore answer:

```text
Can an ACME issuer acquire an advisory lease, publish/clear an HTTP-01
challenge, replicate those facts as signed p2panda operations, rebuild the
projection on another local node, and keep serving the last-good challenge
while the issuer/coordinator role is gone?
```

This is not a real certificate issuance slice. It is the product canary for
the new fact substrate plus advisory lease semantics.

## Requirements Trace

- `VISION.md`: operations are explicit, command-shaped, and the data plane
  outlives the daemon.
- `MVP/overall-plan.md`: after deploy restart recovery, ACME should move onto
  the p2panda fact boundary and advisory lease semantics.
- `MVP/architecture.md`: leases are advisory; the operator's connected node is
  the command consistency boundary; surviving races reduce deterministically.
- `MVP/e2e-proof-plan.md`: E2E-6a needs ACME challenge ownership as a
  lease-fenced product canary and E2E-7 requires serving continuity while the
  coordinator is down.
- `MVP/primitive-decisions.md`: no quorum, witness ack, strict lease mode, or
  hidden active-partition check.
- `MVP/design-notes/p2panda-substitution.md`: p2panda owns signed operation
  envelopes and append-log ingestion; Ployz owns island grants, reducers, and
  business semantics.
- `MVP/design-notes/phased-command.md`: do not introduce `mvp-commands` for
  ACME unless the phase/resume pattern has repeated enough to justify it.
- `MVP/slice-015-docs-backed-acme-http01-plan.md`: preserve hostname/token
  validation, challenge projection, last-good serving, stale rejection, and
  deterministic supersession.

## Preconditions

- Slice 018c deploy restart recovery is finished and should be treated as the
  current p2panda-backed deploy recovery proof.
- Finish the persistent p2panda storage/restartable fact-role slice first. The
  Slice 019a audit promoted that substrate work ahead of ACME.
- Keep all work under `MVP/`.

## Scope

In scope:

- Add a p2panda-backed ACME command adapter over existing `mvp-acme` and
  `mvp-lease` domain types.
- Write lease claim/renew/release and ACME present/clear facts through
  `mvp-p2panda-facts`, not `IrohFactDoc`.
- Add a small p2panda operation export/import path suitable for deterministic
  two-node E2E replication if Slice 018b does not already expose one.
- Project ACME facts from a second local p2panda-backed `FactSource` after
  imported-operation exchange into SQLite and gateway snapshots.
- Serve HTTP-01 challenge responses from last-good serving state after the
  issuer/coordinator role is dropped.
- Preserve local command results that include visible nodes at decision time.
- Preserve conflict-as-candidate reduction by `(epoch desc, content_hash asc)`.
- Add an E2E scenario named `p2panda-acme-http01-contract`.
- Record metrics and update maintainer docs after implementation.

Out of scope:

- Real Let's Encrypt, Pebble, Boulder, or any external ACME directory.
- ACME account/order/authorization lifecycle.
- TLS certificate install, renewal scheduling, or DNS-01.
- p2panda-net transport, discovery, blobs, or encrypted spaces.
- Quorum, strict leases, witness acknowledgements, or `store.pin_fact`.
- General `mvp-commands` / `PhasedCommand`.
- Pingora migration.
- Existing `crates/` integration.

## Crate Scout

Checked before planning:

- `instant-acme` 0.8.5 is an async pure-Rust ACME RFC 8555 client with typed
  account, order, authorization, challenge, and key-authorization types:
  <https://docs.rs/instant-acme/latest/instant_acme/>. It remains the likely
  candidate when the MVP starts talking to a real ACME directory, but this
  slice only needs challenge ownership and serving state.
- `rustls-acme` 0.15.2 supports TLS-ALPN-01 and HTTP-01 and folds
  certificate acquisition/renewal into polled streams:
  <https://docs.rs/rustls-acme/latest/rustls_acme/>. That is too coupled to
  TLS serving for this substrate canary.
- `p2panda-core`, `p2panda-store`, and `p2panda-stream` 0.5.2 are current on
  docs.rs. `p2panda-stream` is specifically for validating, ordering, pruning,
  and storing operation streams; `p2panda-store` documents read/write store
  interfaces and atomic transaction patterns:
  <https://docs.rs/crate/p2panda-stream/latest>,
  <https://docs.rs/crate/p2panda-store/latest>, and
  <https://docs.rs/crate/p2panda-core/latest>.

Decision for this slice:

- Add no ACME client dependency.
- Keep using the existing Hyper-based serving proof.
- Keep using the p2panda crates already introduced by `mvp-p2panda-facts`.
- If the slice needs two-store operation exchange in the harness, add the
  smallest export/import API to `mvp-p2panda-facts` instead of pulling in
  p2panda-net.

## Design Decisions

### The ACME Command Adapter Is Backend-Neutral Above Fact Writes

`mvp-acme` should continue to own hostname/token/key-authorization validation
and lease-fenced challenge rules. It should not depend on p2panda.

`mvp-p2panda-facts` stays a generic substrate crate. It must not import
`mvp-acme`, know ACME fact keys, or encode challenge ownership semantics. The
ACME-specific adapter should live in `mvp-acme` if it has more than one caller,
or in the E2E harness if it remains canary-only:

```text
read local lease/challenge candidates
  -> reduce current advisory lease state
  -> fail before mutation on visible conflict/stale holder
  -> write one signed p2panda fact operation locally
  -> return visible nodes at decision time
```

If two call sites need the adapter, make it a small public type. If only the
E2E needs it, keep it in the harness.

The adapter also needs a public pure domain boundary. It must not rely on
`mvp-lease` harness-only importers or private `LeaseGuard` constructors to
replay fact state. Add or reuse pure helpers that reduce lease facts from a
`FactSource` into `LeaseState` and validate ACME present/clear facts against a
holder, epoch, and claim hash before mutation.

### Operation Replication Is Explicit Test Plumbing

The E2E should use two local p2panda stores to keep the old "node A writes,
node B projects/serves" proof shape from Slice 015.

The replication path can be deterministic and harness-local:

```text
node-a PandaFactStore exports signed fact operations
node-b PandaFactStore imports them
node-b projection reads through FactSource
```

This proves the fact boundary moves as p2panda operations without committing to
production p2panda-net transport yet. Transport comes later.

The first export shape should be deliberately narrow: opaque signed operation
header bytes plus payload bytes when import validation needs the body. Island,
fact key, author, content hash, candidate status, and payload availability
should be derived again by the importing store and `FactSource`, not trusted as
copied metadata.

### Leases Stay Advisory

The adapter must not add a hidden strict mode. Command entry reads the local
fact view, writes locally when allowed, and reports visible nodes. If two
issuers race, both facts may survive. Projection chooses the deterministic
winner and marks the loser superseded.

### Serving Reads Projection, Not Coordinator State

HTTP-01 serving remains data-plane state:

```text
p2panda facts -> projection -> gateway snapshot -> last-good serving state
```

Dropping the issuer/coordinator object must not remove a challenge from the
gateway. A clear fact or higher-epoch presentation changes serving state after
projection; coordinator liveness does not.

### Do Not Lift PhasedCommand Yet

Do not lift `PhasedCommand` in this slice. Keep ACME explicit and record after
implementation whether this slice added another phase/resume data point. The
actual `mvp-commands` planning remains a separate future slice.

## Implementation Units

### Unit 1: P2panda Operation Exchange Surface

Files:

- `MVP/p2panda-facts/src/lib.rs`
- `MVP/e2e/src/p2panda_fact_source_contract.rs`

Work:

- Add a small exported operation type if needed, carrying the signed p2panda
  operation header bytes plus payload bytes only when the importer needs the
  body for validation.
- Add export/import or merge helpers that keep p2panda validation and author
  binding checks in one place.
- Define the import validation model explicitly:
  - verified when the signature validates, payload hash matches, and public key
    is bound to the claimed principal,
  - unverified when signature/body validation fails or the principal binding
    is missing,
  - unauthorized when the reader lacks grants even though the operation itself
    is structurally valid.
- Keep the API named and documented as local deterministic operation exchange,
  not production sync or replication.
- Preserve the existing `FactSource` behavior: unreadable, cross-island,
  unverified, and unauthorized candidates remain visible as statuses but do
  not provide payloads.
- Extend the existing p2panda fact-source E2E to prove a second store can
  import operations and rebuild projection from imported state.

Tests:

- Importing an exported operation into an empty store yields the same
  candidate metadata and payload.
- Duplicate imported operations are idempotent.
- Same-key different-payload imports surface conflict candidates.
- Imported operations whose public key is not bound to the claimed principal
  remain unverified/unreadable.
- Unauthorized readers cannot read imported payloads.

Verification:

- `cd MVP && cargo test -p mvp-p2panda-facts`
- `cd MVP && cargo run -p mvp-e2e -- p2panda-fact-source-contract`

### Unit 2: Public Lease/ACME Fact Replay Boundary

Files:

- `MVP/lease/src/lib.rs`
- `MVP/acme/src/lib.rs`

Work:

- Add a public pure lease reducer API that accepts typed lease facts for one
  resource and returns `LeaseState` without constructing a `LeaseBook` or
  `LeaseGuard`.
- Add ACME helper APIs that decode/validate presented and cleared facts from
  reconstructed facts and check holder, epoch, claim hash, hostname, token, and
  key authorization.
- Keep local guard minting private. The replay API observes facts and validates
  command preconditions; it does not create lease ownership by itself.
- Keep the existing in-memory `LeaseBook` behavior unchanged.

Tests:

- Pure lease reduction matches `LeaseBook` state for claim, renew, release,
  expiry, and same-epoch conflict cases.
- ACME present/clear validation rejects mismatched hostname, token, holder,
  epoch, claim hash, and key authorization.
- No test uses harness-only fact importers to prove the public replay API.

Verification:

- `cd MVP && cargo test -p mvp-lease -p mvp-acme`

### Unit 3: Lease And ACME P2panda Fact Writer

Files:

- `MVP/acme/src/lib.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`

Work:

- Add a narrow command adapter or harness helper that writes:
  - `LeaseClaimed`
  - `LeaseRenewed`
  - `LeaseReleased`
  - `AcmeHttp01Presented`
  - `AcmeHttp01Cleared`
- Read relevant local candidates before each mutation and branch on structured
  lease/ACME errors instead of parsing display text.
- Re-check holder, epoch, and claim hash immediately before present/clear.
- Return visible nodes at decision time in command results.
- Keep lease and challenge grants separate.
- Preserve renewal and release/RAII behavior through p2panda facts without
  requiring `mvp-p2panda-facts` to know lease domain rules. If RAII drop cannot
  perform async writes directly, use an explicit, test-visible release sink or
  document the synchronous handoff boundary in the adapter.

Tests:

- First issuer claims and presents a challenge through p2panda facts.
- Second issuer sees a structured conflict before mutation when the first
  claim is locally visible.
- Expired lease allows a higher-epoch issuer to present.
- Current holder renews through a p2panda `LeaseRenewed` fact.
- Dropping or explicitly releasing a local holder records a p2panda
  `LeaseReleased` fact through the adapter's release path.
- Stale holder cannot present or clear after a higher epoch wins.
- An issuer with lease write but no ACME write grant cannot present.
- An issuer with ACME write but no lease write grant cannot claim ownership.

Verification:

- `cd MVP && cargo test -p mvp-acme -p mvp-lease -p mvp-p2panda-facts`

### Unit 4: Imported Projection And Last-Good HTTP Serving

Files:

- `MVP/projection/src/reducer.rs`
- `MVP/e2e/src/p2panda_acme_http01_contract.rs`
- `MVP/e2e/src/main.rs`
- `MVP/e2e/src/metrics.rs`

Work:

- Add `p2panda-acme-http01-contract`.
- Use two p2panda stores and two principals:
  - node A writes lease/challenge facts,
  - operation exchange copies signed operations to node B,
  - node B projects from the p2panda `FactSource`,
  - node B reloads serving from gateway snapshot and serves HTTP-01.
- Make the coordinator boundary explicit: node A's ACME command adapter is the
  killed/dropped coordinator role; node A's already-written fact store may
  remain available only as an operation-export fixture; node B's fact import,
  projection, snapshot reload, and serving roles remain alive.
- Drop the node A command adapter after projection and prove node B continues
  serving the last-good challenge response. Then import already-written later
  facts into node B and prove node B can still project/reload them without a
  live node A command adapter.
- Write/import a higher-epoch presentation, project/reload, and prove serving
  switches to the new key authorization.
- Write/import a clear fact from the winning holder and prove serving returns
  `404`.
- Align ACME presentation and clear reduction with the global conflict rule:
  `(epoch desc, content_hash asc)`. Do not keep payload-field tie-breakers such
  as key authorization or holder for same-epoch ACME races.

Required assertions:

- all ACME facts in the scenario are written through `mvp-p2panda-facts`,
  not `IrohFactDoc`,
- visible nodes are included in command results,
- no quorum or witness ack path is invoked,
- one stale-present or stale-clear smoke check fails before mutation, with
  broader stale behavior covered by focused `mvp-acme` tests,
- one imported same-key race produces superseded projection status, with
  broader conflict ordering covered by focused reducer tests,
- HTTP gateway serves the selected key authorization with no trailing newline,
- serving still answers while the command adapter is absent,
- SQLite can be deleted and rebuilt from imported p2panda operations.

Metrics:

Required:

- elapsed scenario time,
- operation export/import duration,
- projection and gateway reload duration,
- HTTP challenge request duration,
- command-adapter outage serving success count.

Optional when cheap to record:

- lease acquire duration,
- challenge present duration,
- conflict/superseded candidate count,
- stale mutation rejection count,
- SQLite rebuild duration.

Verification:

- `cd MVP && cargo run -p mvp-e2e -- p2panda-acme-http01-contract`
- `cd MVP && cargo test -p mvp-projection`
- `cd MVP && cargo run -p mvp-e2e -- docs-backed-acme-http01-contract`
- `cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all`

### Unit 5: Maintainer Docs And Leverage Accounting

Files:

- `MVP/primitive-decisions.md`
- `MVP/e2e-proof-plan.md`
- `MVP/overall-plan.md`
- `MVP/architecture.md`
- `MVP/slice-019-p2panda-acme-http01.md`

Work:

- Add a "Changed Since Last Slice" entry explaining what moved from iroh-docs
  to p2panda operations.
- Update E2E-6a/E2E-7/E2E-9 proof status with the new p2panda ACME scenario.
- Record whether operation export/import remains a harness fixture or became a
  reusable substrate API.
- Record semantic-leverage evidence against the old ACME coordination baseline
  without counting placeholder wire serving polish as product logic.
- Record whether Slice 019 adds another repeated phase/resume data point. Do
  not plan or implement `mvp-commands` in this slice.

## Verification

Before pushing the slice:

```text
cd MVP && cargo fmt --all
cd MVP && cargo test -p mvp-lease -p mvp-acme -p mvp-projection -p mvp-serving -p mvp-p2panda-facts
cd MVP && cargo clippy -p mvp-lease -p mvp-acme -p mvp-projection -p mvp-serving -p mvp-p2panda-facts --all-targets -- -D warnings
cd MVP && cargo run -p mvp-e2e -- p2panda-acme-http01-contract
cd MVP && cargo run -p mvp-e2e -- docs-backed-acme-http01-contract
cd MVP && MVP_E2E_ALL_TIMEOUT=120s cargo run -p mvp-e2e -- all
```

If `mvp-e2e -- all` exceeds the 120-second budget, diagnose the regression
instead of raising the budget by default.

## Review Focus

Use subagents for the product slice review, not for tiny mechanical fixes:

- correctness: lease epoch/claim-hash fencing, stale clear rejection,
  conflict-as-candidate reduction, operation import idempotency,
- security: hostname/token/key-authorization validation, no token echoing from
  request to response, grants separated for lease and challenge writes,
- reliability: serving continuity after command adapter/coordinator drop,
  imported operation projection rebuilds deleted SQLite,
- simplicity: ACME business code should read as acquire/check/present/project/
  serve, with p2panda plumbing isolated at the fact boundary.

Run the simplify workflow after the first green E2E and land that pass as a
separate commit.

## Acceptance Gate

The slice is complete when:

- `p2panda-acme-http01-contract` passes and writes lease/challenge facts only
  through `mvp-p2panda-facts`,
- a second local p2panda-backed projection fed by imported signed operations
  produces a gateway snapshot with HTTP-01 challenge state,
- HTTP serving answers from last-good projected state while the issuer/
  coordinator adapter is absent,
- hostname, token, and key-authorization validation remain enforced by focused
  `mvp-acme` tests,
- key authorization is served exactly as projected, with no trailing newline
  and no token-echo fallback,
- lease and ACME challenge write grants stay separate,
- renewal and release/RAII behavior write p2panda facts or have an explicit
  documented release-sink boundary,
- stale publish/clear attempts fail before mutation with structured errors,
- same-epoch races reduce deterministically and surface superseded losers,
- command results include visible nodes at decision time,
- SQLite rebuild from imported p2panda operations is proven,
- no quorum, witness ack, strict lease, hidden active-partition, or
  `store.pin_fact` behavior is introduced,
- maintainer docs explain what moved from iroh-docs to p2panda and why.
