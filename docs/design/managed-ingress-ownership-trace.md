# Current Managed Ingress Ownership Trace

This note records the current implementation across `ployz-rust` and
`ployz-dns`. It is current-state evidence for [the ingress-separation
Wayfinder](https://github.com/getployz/ployz/issues/462), not the target model.
The map's destination and standing constraints supersede conflicting current
behavior.

## Ownership summary

| Concern | Current owner | Durable state | Trigger and contract |
| --- | --- | --- | --- |
| Managed lease allocation | Core `ployzd` chooses `auto` and creates acquisition credentials; `ployz-dns` allocates the slug and owns the lease | Core SQLite stores the bearer token and projected lease record; `ployz-dns` D1 stores the lease, token hash, acquisition claim, expiry, DNS ids, and certificate fields | Core polls an HTTP lease worker. `POST /v1/leases` is idempotent by acquisition id plus token. |
| Managed A/AAAA | Core chooses the address set; `ployz-dns` reconciles authoritative records | Core stores the last successfully applied address set; `ployz-dns` stores desired addresses and exact provider record ids | The core background lease task gathers gateway facts and calls `POST /v1/leases/:lease/renew`. |
| Managed wildcard CNAME | `ployz-dns` | `lease_records` in D1 plus the Cloudflare record | Lease activation always creates `*.<lease>.up.ployz.app CNAME <lease>.up.ployz.app`. Core never projects or mutates this record directly. |
| Automatic hostname derivation | Core deploy preparation | The resulting ordinary Route Binding in core intent | A deploy declaration with `AutoHostname` becomes `<service-label>[-N].<lease>.up.ployz.app`; an existing binding is reused. |
| Managed wildcard certificate | `ployz-dns` issues and stores it; core stores and distributes a downloaded copy | D1 holds issuance claims, retry state, chain, and private key; core lease intent embeds the downloaded bundle; gateways receive it through `intent.get` | Cloudflare Queue/cron drives DNS-01 issuance. Core polls `GET /v1/leases/:lease/cert-bundle`. |
| Exact custom-hostname certificate | Core `CertificateManager` | Core SQLite stores active metadata, ACME account credentials, and HTTP-01 challenges; core filesystem stores material; each gateway stores pushed material | Deploy calls `ensure`; a separate cert operation performs DNS preflight, HTTP-01 publication/readiness through intent and machine RPC, issuance, artifact push, then activation. A core timer renews due certs. |
| Runtime route/TLS projection | Core intent service plus each gateway | Core intent is authoritative; gateway keeps last-known-good projection and local custom-certificate material | `intent.get` returns the complete snapshot; `intent.changed` is invalidation. Gateways fold intent with cached machine facts and local cert material. |
| Deploy readiness | Core deploy worker | Deploy operation evidence and terminal failure; no separate readiness truth | Auto hostnames wait for a valid lease, valid managed bundle, and any successfully applied address set. Custom HTTPS blocks route/Serving Target commit until the exact cert is active and pushed. |

## Current flow

### 1. Lease acquisition and public DNS are one remote product

`PublicUrlMode::Auto` creates an `Unacquired` state containing a random
acquisition id and bearer token. The same enum later contains the remote lease
record and the wildcard bundle, so mode, allocation, DNS publication, and
certificate readiness are represented as one state machine
([`cert.rs`](../../crates/ployz-core/src/cert.rs#L35-L120),
[`lease_intent.rs`](../../crates/ployzd/src/intent/lease_intent.rs#L193-L203)).
Core persists that enum in the singleton `managed_lease_intent` row and persists
the last successfully applied address set in a second singleton row
([`core_store.rs`](../../crates/ployzd/src/core_store.rs#L118-L121),
[`core_store.rs`](../../crates/ployzd/src/core_store.rs#L169-L172),
[`lease_intent.rs`](../../crates/ployzd/src/intent/lease_intent.rs#L98-L127)).

The lease task is an always-running core-local timer. Acquisition deliberately
sends no addresses, allowing name allocation and certificate issuance to begin
before gateway discovery. After acquisition, it reads the active roster, selects
gateway-role machines, asks each for fresh machine facts, and derives A/AAAA
from their reported control endpoints
([`task.rs`](../../crates/ployzd/src/lease/task.rs#L31-L149),
[`task.rs`](../../crates/ployzd/src/lease/task.rs#L170-L243),
[`task.rs`](../../crates/ployzd/src/lease/task.rs#L276-L299),
[`task.rs`](../../crates/ployzd/src/lease/task.rs#L401-L447)). Current behavior
requires every known gateway to answer and requires at least one address before
it updates DNS. Silence creates a failed `GatewayTestimony` managed-lease
operation and preserves the previously applied remote records
([`task.rs`](../../crates/ployzd/src/lease/task.rs#L213-L233),
[`managed_lease.rs`](../../crates/ployz-core/src/ops/managed_lease.rs#L15-L75)).
This conflicts with the map's target gather rule: publish responders when at
least one active gateway answers, remove silent gateways, and retain the
complete last-known-good set only when every gateway is silent.

The remote interface is plain bounded HTTP, not NATS: acquire, renew, and bundle
download use 5-second connect and 15-second global timeouts
([`client.rs`](../../crates/ployzd/src/lease/client.rs#L71-L124)). The remote
worker activates a lease by creating apex A/AAAA records followed by the
wildcard CNAME, compensates created records on failure, and reconciles A/AAAA
sets on renewal before extending the lease
([`ployz-dns/src/leases.ts`](../../../ployz-dns/src/leases.ts#L54-L99)).
The D1 schema is the authoritative remote ledger for the lease, addresses,
status, expiry, and provider record ids
([`ployz-dns/migrations/0001_initial.sql`](../../../ployz-dns/migrations/0001_initial.sql#L1-L25),
[`ployz-dns/migrations/0007_add_lease_acquisitions.sql`](../../../ployz-dns/migrations/0007_add_lease_acquisitions.sql#L1-L3)).

### 2. The managed wildcard certificate shares that lease lifecycle

The managed bundle is constrained to exactly `*.<lease>.up.ployz.app` and
`<lease>.up.ployz.app`; the lease name itself is also the automatic-hostname
suffix
([`cert.rs`](../../crates/ployz-core/src/cert.rs#L281-L324),
[`cert.rs`](../../crates/ployz-core/src/cert.rs#L446-L510)). In `ployz-dns`, lease
activation queues issuance, a Queue consumer claims work in D1, and cron repairs
missed or abandoned work. Issuance uses DNS-01, tries configured providers in
order for provider-class failures, persists exponential backoff, and stores the
chain and private key on the lease
([`ployz-dns/src/index.ts`](../../../ployz-dns/src/index.ts#L29-L84),
[`ployz-dns/src/certificates.ts`](../../../ployz-dns/src/certificates.ts#L111-L145)).
The worker's certificate tables are columns on `leases`, not an independent
certificate aggregate
([`ployz-dns/migrations/0003_add_managed_certificates.sql`](../../../ployz-dns/migrations/0003_add_managed_certificates.sql#L1-L7),
[`ployz-dns/migrations/0004_add_issuance_state.sql`](../../../ployz-dns/migrations/0004_add_issuance_state.sql#L1-L5),
[`ployz-dns/migrations/0006_add_issuance_backoff.sql`](../../../ployz-dns/migrations/0006_add_issuance_backoff.sql#L1-L4)).

Core polls pending bundle reads every five seconds and independently refreshes
the lease and bundle at two-thirds of their validity windows. Successful worker
calls create separate managed-lease operation records with only accepted and
terminal states
([`task.rs`](../../crates/ployzd/src/lease/task.rs#L97-L148),
[`task.rs`](../../crates/ployzd/src/lease/task.rs#L245-L337),
[`managed_lease.rs`](../../crates/ployz-core/src/ops/managed_lease.rs#L33-L75)).
No user-facing NATS command owns these mutations; the core timer creates the
operations.

### 3. Automatic hostname allocation is already a small pure seam

Deploy input preserves `AutoHostname` as a distinct route-target variant until
preparation. `auto_hostname_route_binding_commits` is the one allocation seam:
it normalizes the service id, searches all cluster Route Bindings for an existing
binding for the same service/endpoint, and otherwise selects the first free
`<service>[-N].<lease>.up.ployz.app`
([`deploy.rs`](../../crates/ployz-core/src/deploy.rs#L1621-L1694),
[`preparation.rs`](../../crates/ployzd/src/operations/deploy/preparation.rs#L91-L115)).
The committed result is an ordinary Route Binding; there is no durable marker
that distinguishes a generated hostname from a caller-supplied hostname.

The current function hardcodes the managed lease as the only automatic
namespace. This is the smallest existing seam for substituting an explicit
automatic-hostname namespace while retaining the collision and stable-redeploy
rules. The focused test proves collision-safe reuse
([`deploy_command_preparation.rs`](../../crates/ployzd/tests/deploy_command_preparation.rs#L245-L295)).

### 4. Exact custom certificates are a separate core-owned system

Deploy preparation classifies every HTTPS hostname not covered by the current
managed wildcard as custom. Coverage is an apex or one-label-under-apex string
test; there is no concept of a custom automatic namespace
([`preparation.rs`](../../crates/ployzd/src/operations/deploy/preparation.rs#L182-L235)).
Before phase commit, deploy calls `CertificateManager::ensure`. The manager
requires the hostname's current A/AAAA answers to be a non-empty subset of known
gateway IPs, publishes HTTP-01 challenges through intent, waits for addressed
gateways to report challenge application over machine RPC, issues the cert,
writes core-local material, pushes it to every gateway target, and only then
activates its metadata
([`manager.rs`](../../crates/ployzd/src/certificate/manager.rs#L150-L166),
[`manager.rs`](../../crates/ployzd/src/certificate/manager.rs#L279-L384),
[`manager.rs`](../../crates/ployzd/src/certificate/manager.rs#L420-L478),
[`gateway.rs`](../../crates/ployzd/src/certificate/gateway.rs#L29-L176)).

Active exact-certificate metadata, ACME challenges, and ACME account credentials
live in core SQLite. Certificate material lives beside the core database and in
each gateway's local certificate store
([`certificate_intent.rs`](../../crates/ployzd/src/intent/certificate_intent.rs#L19-L180),
[`core_store.rs`](../../crates/ployzd/src/core_store.rs#L137-L150),
[`manager.rs`](../../crates/ployzd/src/certificate/manager.rs#L30-L49)). A
core-local hourly task creates explicit cert operations for renewals
([`task.rs`](../../crates/ployzd/src/certificate/task.rs#L16-L49),
[`task.rs`](../../crates/ployzd/src/certificate/task.rs#L84-L108)). The target
model's exact per-hostname certificates under custom automatic namespaces can
reuse this system; the classification input must stop treating “not under the
managed lease” as the domain distinction.

### 5. Intent and gateway runtime projection carry all concerns together

`IntentSnapshot` currently carries Route Bindings, Serving Target entries,
managed public-url mode plus lease and wildcard private material, exact custom
certificate metadata, and live HTTP-01 challenges in one full response
([`state.rs`](../../crates/ployz-core/src/state.rs#L141-L187)). The core assembles
that snapshot behind `plz.v1.rpc.core.query.intent.get`, while
`plz.v1.signal.intent.changed` is both immediate invalidation and periodic full
payload broadcast
([`service.rs`](../../crates/ployzd/src/intent/service.rs#L53-L84),
[`service.rs`](../../crates/ployzd/src/intent/service.rs#L100-L185),
[`service.rs`](../../crates/ployzd/src/intent/service.rs#L226-L304),
[`subjects.rs`](../../crates/ployz-core/src/subjects.rs#L19-L20)). There is no
NATS contract for a canonical ingress endpoint projection and `ployz-dns` is not
a NATS consumer today.

Each gateway re-lists intent, folds it with cached machine-container facts and
gateway-local exact-certificate material, and keeps last-known-good projection
on invalid or unavailable sources
([`source.rs`](../../crates/ployzd/src/roles/gateway/source.rs#L36-L71),
[`projection.rs`](../../crates/ployzd/src/roles/gateway/projection.rs#L28-L56),
[`projection.rs`](../../crates/ployzd/src/roles/gateway/projection.rs#L104-L197)).
Pingora expands the managed wildcard bundle across matching routes and exact
bundles across exact HTTPS routes when preparing its TLS snapshot
([`pingora.rs`](../../crates/ployzd/src/roles/gateway/pingora.rs#L404-L412),
[`pingora.rs`](../../crates/ployzd/src/roles/gateway/pingora.rs#L462-L503)).

The built-in DNS role is a separate local Route DNS Projection, not the
authoritative `up.ployz.app` publisher. It folds every Route Binding to current
serving gateway endpoint observations and watches `intent.changed`
([`dns/source.rs`](../../crates/ployzd/src/roles/dns/source.rs#L19-L101),
[`dns/source.rs`](../../crates/ployzd/src/roles/dns/source.rs#L120-L142),
[`dns/process.rs`](../../crates/ployzd/src/roles/dns/process.rs#L249-L297)). Its
source fold is the closest current read model to the target canonical ingress
endpoint projection, but it is route-shaped and filters through gateway status;
it should not become the publisher interface by renaming it.

The SDK runtime snapshot also exposes only `RuntimePublicUrl::Auto { domain }`
and per-route exact-certificate lifecycle. It derives these from the combined
intent snapshot and does not expose gateway endpoint answers as a standalone
projection
([`runtime_snapshot.rs`](../../crates/ployzd/src/runtime_snapshot.rs#L20-L93),
[`runtime_snapshot.rs`](../../crates/ployzd/src/runtime_snapshot.rs#L95-L117)).

### 6. Deploy readiness is coupled to the combined model

Only deploys containing `AutoHostname` wait for managed public-url readiness.
They poll core-local lease storage for at most 90 seconds and require all three:
a valid lease, a valid wildcard bundle, and any stored successful address
application. The operation records stage changes as `lease`, `certificate`, or
`gateway_addresses`; timeout becomes a typed deploy failure
([`facts.rs`](../../crates/ployzd/src/operations/deploy/facts.rs#L21-L93),
[`facts.rs`](../../crates/ployzd/src/operations/deploy/facts.rs#L175-L253),
[`deploy.rs`](../../crates/ployz-core/src/ops/deploy.rs#L54-L88)). This gate does
not verify that the currently desired gateway set equals the stored applied set,
and its types cannot express “automatic hostname available but optional managed
DNS target disabled.”

Custom HTTPS readiness is stricter and operation-owned: exact certificate
issuance must succeed before Route Binding and Serving Target commit; every
certificate failure leaves both uncommitted
([`deploy_operation.rs`](../../crates/ployzd/tests/deploy_operation.rs#L1253-L1322),
[`deploy_operation.rs`](../../crates/ployzd/tests/deploy_operation.rs#L1357-L1412)).
Managed HTTPS skips that exact-certificate path solely because suffix coverage
classifies it as covered by the wildcard
([`deploy_operation.rs`](../../crates/ployzd/tests/deploy_operation.rs#L1415-L1438)).

## Existing verification surface

- Core lease model and wire invariants: managed name, wildcard/apex bundle,
  validity windows, pending responses, failure values, and digest validation
  ([`managed_lease.rs`](../../crates/ployz-core/tests/managed_lease.rs#L7-L177)).
- Core lease task: explicit acquisition/renew/download operations, pending
  preservation, and mirrored-record recovery
  ([`managed_lease_task.rs`](../../crates/ployzd/tests/managed_lease_task.rs#L26-L219)).
- Deploy gate: transition from pending certificate to ready, typed lease/cert/
  address timeouts, and bypass for deploys without automatic hostnames
  ([`managed_certificate.rs`](../../crates/ployzd/tests/deploy_runtime_nats/managed_certificate.rs#L13-L225)).
- Exact custom certificate issuance and phase ordering:
  [`custom_certificate_task.rs`](../../crates/ployzd/tests/custom_certificate_task.rs)
  and [`deploy_operation.rs`](../../crates/ployzd/tests/deploy_operation.rs#L1253-L1438).
- Gateway projection/material RPC and TLS selection:
  [`gateway_projection.rs`](../../crates/ployzd/tests/gateway_projection.rs),
  [`gateway_certificate_rpc.rs`](../../crates/ployzd/tests/gateway_certificate_rpc.rs),
  and [`gateway_pingora.rs`](../../crates/ployzd/tests/gateway_pingora.rs).
- Cross-process survival: an automatic HTTPS route remains served after core
  stop, proving the gateway's last-known-good data-plane behavior
  ([`dind_cluster.rs`](../../crates/ployz-e2e/tests/dind_cluster.rs#L293-L370)).
- `ployz-dns`: idempotent lease allocation, DNS compensation, A/AAAA renewal,
  expiry cleanup, asynchronous issuance, provider failover, backoff, and cron
  repair
  ([`leases.test.ts`](../../../ployz-dns/test/leases.test.ts#L6-L140),
  [`index.test.ts`](../../../ployz-dns/test/index.test.ts#L14-L167),
  [`certificates.test.ts`](../../../ployz-dns/test/certificates.test.ts#L6-L78)).

## Smallest existing seams for the later separation

These are extraction points already present in the implementation. They do not
require a new in-process DNS-provider interface.

1. **Ingress endpoint calculation:** extract the known-gateway plus live-facts
   gather at `run_once_with_roster` / `gateway_addresses_from_facts` into the
   owner of the canonical ingress endpoint projection. Replace its current
   all-responders gate with the map's bounded gather and explicit
   responder/silent/all-silent transition. The lease task then becomes only one
   consumer. The DNS role's `dns_projection_input_from_state` supplies useful
   projection tests, but its per-route output is not the interface.
2. **Managed DNS target publication:** retain `LeaseClient` as the external
   adapter and split its remote request shape so lease allocation and stable
   target publication no longer return or require a wildcard certificate.
   `ployz-dns` already concentrates provider mutation in `createLease`,
   `reconcileAddresses`, and `CloudflareDnsClient`; no provider abstraction is
   needed inside core.
3. **Automatic hostname namespace:** generalize the existing pure
   `auto_hostname_route_binding_commits` seam from `ManagedLeaseName` to a
   namespace value with explicit dependency on a publication target. The
   persisted Route Binding remains the allocation evidence. Add provenance only
   if disabling the managed target must distinguish generated managed routes
   from coincidentally matching custom hostnames; current state cannot make that
   distinction.
4. **Certificate policy classification:** replace
   `custom_certificate_hostnames(..., managed_lease)` with classification by
   automatic namespace: Ployz's own namespace is covered by one managed
   wildcard; every generated hostname under a user-owned automatic namespace
   enters the existing exact-certificate manager. The manager, cert operation,
   storage, machine RPC, and gateway artifact path already form a deep module.
5. **Observation contract:** add a complete ingress-endpoint `get` plus
   `changed` invalidation alongside `intent.get`; do not put provider mode or
   mutation deltas in it. `NatsIntentReader`, the gateway/DNS invalidation loops,
   and last-known-good projection states are existing patterns. `ployz-dns` can
   consume this contract independently while external controllers consume it at
   the same time.
6. **Deploy gates:** split `ensure_managed_public_url_for_deploy` into the exact
   prerequisites implied by the requested route: automatic namespace available,
   required publication target enabled/reachable, and applicable certificate
   ready. Reuse the existing operation evidence points, but remove the current
   invariant that every automatic hostname requires one object containing lease,
   addresses, and wildcard certificate.

The narrowest first cut is the ingress endpoint projection. It has multiple
real consumers already hidden inside the lease task, Route DNS Projection, SDK
runtime assembly, and future external controllers. Once that interface exists,
managed DNS publication can move behind an independent consumer without
changing gateway route projection or custom certificate issuance.
