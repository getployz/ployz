# NATS Future Topology

Long-term target for the NATS substrate when ployz spans multiple regions,
authorities, and developer machines. This is design, not a porting plan.

For the current single-installation control plane, read
[`nats-native-control-plane.md`](nats-native-control-plane.md). For the NATS
features this builds on, read [`nats.md`](nats.md).

## Question

Do we need an explicit `stable | dev | lab | edge` tier in NATS subjects or
authority metadata, or can we satisfy the requirements with a model based on
authority IDs, route exports, grants, and subscription/ACL policy?

## Answer

No tier in subjects. Encode three things structurally: `<installation>`,
`<authority>`, and `<plane>`. Tier is policy metadata on the authority registry
record; it changes, subjects don't.

Product hierarchy:

- **Installation** is the compute, trust, and substrate boundary. Use a separate
  installation when compute pools, credentials, operators, or blast radius should
  be separated.
- **Namespace** is the deploy/environment boundary inside an installation. Prod,
  staging, preview, and PR environments are normally namespaces.
- **Authority** is an internal durable write/quorum/failure domain. It is not
  the normal way to separate prod from staging or one compute pool from another.

Region is also not a subject dimension. A region is a placement, latency,
routing, and machine-grouping concept. An authority is a write/trust/quorum/
failure domain. A region can exist without owning durable authority state; it
can run workloads and gateways while durable control-plane state remains owned
by the installation's current home/data authority.

Route source truth is authority-private. Sharing, public exposure, and
cross-authority gateway visibility are explicit route exports derived from that
source journal by owner-local grants. Use per-authority JetStream domains +
NATS accounts when an authority exists, so partition tolerance and
cross-authority visibility fall out of NATS-native primitives instead of subject
namespacing. A compute-only region does not need its own JetStream authority
domain. Cross-authority access is mediated by account exports/imports +
owner-local grant records + route export streams, not by subject prefix
wildcards.

Authority means write/trust/quorum/failure domain. It does not mean
environment, compute pool, or org boundary. Most production, staging, PR, and
dev environments are namespaces inside an installation. If a user wants compute
separation, create or join a separate installation. A PR or dev session becomes
its own authority only when it must keep making progress while partitioned from
the parent authority and sharing an installation is still the right trust model;
otherwise it is usually a namespace in the org installation or a namespace in a
personal/local installation.
A region becomes an authority only when it owns durable writes and quorum, not
when a machine merely exists there.

## MVP shape: many regions, one home authority

Start with regions in the model, but only one region as the HA data home.

```
installation: acme

region us-west
  role: home/data
  authority: default
  NATS: clustered
  JetStream: enabled
  control streams: R3

region sin
  role: compute/edge
  authority: none yet
  NATS: connects to the installation mesh
  JetStream: no independent durable authority

region eu
  role: compute/edge
  authority: none yet
  NATS: connects to the installation mesh
  JetStream: no independent durable authority
```

In this shape, `ployz deploy` can still place replicas in `us-west`, `sin`,
and `eu`, but deploy commits, route source truth, volumes, secrets, and durable
coordination stay in `auth-default` until an operator explicitly promotes
another region into a data authority.

The future promotion primitive is explicit:

```
ployzctl region promote sin --data --replicas 3
```

That command verifies suitable `storage=true` machines, creates the regional
NATS account/domain, initializes or transfers the selected authority state,
starts route export into the global serving view, and returns only after the new
serving/storage path is observable.

## Why tier-in-subject loses

1. Tier transitions become subject migrations. A `lab` namespace promoted to
   `stable` would either rewrite history or require a stream copy with subject
   remap. That is exactly the kind of operator-visible churn the rest of the
   design avoids.
2. It does not actually solve the cross-tier cases. Stable gateways exposing
   dev preview URLs, or dev gateways consuming prod routes, both cross the
   prefix anyway. Grants and projections are still needed. The prefix becomes
   a hint, not a boundary.
3. It is redundant with the authority registry. Whatever a `<tier>` token
   would say, `authorities[<id>].tier` already says, and the registry can
   change without rewriting subjects.
4. Hard ACLs are stronger than prefix matching anyway. NATS accounts give a
   cryptographic boundary; subject prefix gives a string boundary. Make the
   structural axis the one the broker can actually enforce on.

What is genuinely structural and forever-true about a piece of data:

- which **installation** it belongs to,
- which **authority** owns the writes,
- which **plane** it lives on (control plane / route / RPC / work / audit),
- and for routes specifically, whether the message is the authority-private
  **journal** or an intentional **export** to a named audience.

## Subject hierarchy

```
ployz.v1.<installation>.<authority>.<plane>.<plane-specific>
```

- `<installation>` — top-level cluster identity (`local` for laptop dev,
  `inst-<id>` otherwise). Lets one NATS deployment serve multiple installations
  cleanly without future migrations.
- `<authority>` — opaque stable id (`auth-default`, `auth-sin`,
  `auth-dev-nick`). Maps 1:1 to a NATS **account** and a JetStream **domain**
  once the authority exists. Do not create authorities for ordinary namespaces
  or for compute-only regions; create one only for an independent
  write/trust/quorum/failure boundary.
- `<plane>` ∈ `{cp, route, rpc, work, audit}`.

### Control plane (durable, not visibility-bearing)

```
ployz.v1.<inst>.<auth>.cp.deploy.commit.<namespace>.<deploy_id>
ployz.v1.<inst>.<auth>.cp.deploy.status.<namespace>.<deploy_id>      (KV)
ployz.v1.<inst>.<auth>.cp.invite.<invite_id>                         (KV)
ployz.v1.<inst>.<auth>.cp.cert.meta.<hostname>                       (KV)
ployz.v1.<inst>.<auth>.cp.acme.account.<hash>                        (KV)
ployz.v1.<inst>.<auth>.cp.acme.challenge.<hostname>.<token>          (KV)
ployz.v1.<inst>.<auth>.cp.lock.<resource_kind>.<resource_id>         (KV)
```

### Installation substrate plane

Machines join the installation-wide substrate once. Authorities then record
which substrate machines participate in that authority and what placement they
currently own.

```
ployz.v1.<inst>.substrate.region.<region_id>                         (KV)
ployz.v1.<inst>.substrate.machine.<machine_id>                       (KV)
ployz.v1.<inst>.substrate.gateway.<gateway_id>                       (KV)
ployz.v1.<inst>.substrate.dns.<dns_id>                               (KV)
ployz.v1.<inst>.substrate.rpc.node.<machine_id>.<command>
ployz.v1.<inst>.substrate.obs.node.<machine_id>.<observation_kind>
```

Machine records carry `region`, `az`, `storage`, labels, trust tier, capacity,
and runtime facts. Region records say whether the region is `home/data`,
`compute`, `disabled`, or `draining`, plus which authority owns durable state
there if any.

Substrate RPC covers install, ping, mesh diagnostics, node facts, and whole-node
drain/removal. Authority RPC covers commands scoped to an authority's
workloads, volumes, and participants.

### Route plane (journal and exports)

```
ployz.v1.<inst>.<auth>.route.journal.event.<event_id>
ployz.v1.<inst>.<auth>.route.export.<audience_kind>.<audience_id>.event.<event_id>
ployz.v1.<inst>.<auth>.route.export.<audience_kind>.<audience_id>.withdraw.<event_id>
ployz.v1.<inst>.<auth>.route.export.<audience_kind>.<audience_id>.snapshot.<namespace>.<rev>
```

`route.journal` is source truth and is readable only by the owning authority.
It does not become public or shared by changing subject names.

`route.export.<audience_kind>.<audience_id>` is a materialized, intentional
feed created by an operator grant. Audience kind and id are fixed tokens so
wildcards stay sane:

```
public.global
authority.<grantee_auth>
group.<group_id>
gateway.<gateway_id>
```

The broker-enforced boundary is on journal vs. export and on the named export
audience. That keeps route ownership boring while still making privacy leaks
hard at the substrate.

Revocation and TTL expiry are explicit lifecycle events. Correctness depends on
durable `withdraw` events such as `RouteWithdrawn`, `GrantRevoked`, and
`GrantExpired`; removing a source line or purging old projection subjects is
cleanup after the withdrawal is durable, not the lifecycle signal.

### RPC plane (request/reply)

```
ployz.v1.<inst>.<auth>.rpc.node.<machine_id>.<command>
ployz.v1.<inst>.<auth>.rpc.gateway.<gateway_id>.<command>
ployz.v1.<inst>.<auth>.rpc.dns.<dns_id>.<command>
```

### Work queues

```
ployz.v1.<inst>.<auth>.work.cert.renew.<hostname>
ployz.v1.<inst>.<auth>.work.cert.schedule.<hostname>
```

### Installation-level federation plane

The only place subjects span authorities:

```
ployz.v1.<inst>.sync.route.public.global.event.<owner_auth>.<event_id>
ployz.v1.<inst>.sync.route.public.global.withdraw.<owner_auth>.<event_id>
ployz.v1.<inst>.sync.route.authority.<grantee_auth>.event.<owner_auth>.<event_id>
ployz.v1.<inst>.sync.route.authority.<grantee_auth>.withdraw.<owner_auth>.<event_id>
```

Populated by JetStream sources owned by an installation-root account, gated by
grant records, and sourced only from `route.export.*` feeds. This is the only
subscription public gateways need for cross-authority public routes.
Installation-root is a directory/projection authority, not a master. Private
dev work and authority-local serving do not depend on root reachability.

## Stream / KV bucket layout

### Per-authority, in that authority's JetStream domain (`dom-<auth>`)

| Asset | Subjects | Notes |
|---|---|---|
| `cp_deploy_commits_<auth>` (stream) | `ployz.v1.<inst>.<auth>.cp.deploy.commit.>` | append-only; replica policy per authority |
| `route_journal_<auth>` (stream) | `ployz.v1.<inst>.<auth>.route.journal.event.>` | authority-private source truth; one event message per routing fact |
| `route_exports_<auth>` (stream) | `ployz.v1.<inst>.<auth>.route.export.>` | owner-grant-derived audience feeds, including withdraw events |
| `work_cert_<auth>` (stream, WorkQueue) | `ployz.v1.<inst>.<auth>.work.cert.renew.>`, `ployz.v1.<inst>.<auth>.work.cert.schedule.>` | renewal jobs plus broker-held schedules |
| `instances_<auth>` … `locks_<auth>` (KV) | per `cp.*` resource | authority-local lifecycle status and locks |

### Installation-root domain (`dom-<inst>-root`)

Held by the installation's substrate/directory authority. It indexes the mesh
and route grants that need cross-authority projection, but it does not own
private route truth.

| Asset | Sources from | Consumers |
|---|---|---|
| `authorities_<inst>` (KV) | direct writes | every daemon: knows the registry |
| `regions_<inst>` (KV) | direct writes | every daemon: knows placement regions and home/data status |
| `machines_<inst>` (KV) | direct writes | installation-wide substrate identity/facts |
| `gateways_<inst>` / `dns_<inst>` (KV) | direct writes | installation-wide serving endpoints |
| `grant_index_<inst>` (KV) | mirrors/indexes owner-local grants selected for federation | projection-stream commands and status |
| `sync_public_routes` (stream, sources) | `ployz.v1.<inst>.<owner_auth>.route.export.public.global.>` for every authority with a current public grant | public gateways |
| `sync_authority_<grantee>` (stream, sources, one per grantee) | `ployz.v1.<inst>.<owner>.route.export.authority.<grantee>.>` filtered by grants where `grantee=<grantee>` | that grantee's gateways |

The shared-routes projection is one stream per grantee, not one global stream.
That is how privacy is enforced on the read side: a dev never has subscribe
permission on another dev's projection. The owner authority remains source of
truth for the grant; root's `grant_index` is a projection/directory aid.

Replica policy is **per authority**, not global or per region:

- `auth-default` (MVP home/data authority): R=3 across storage-enabled nodes
  in the home region.
- `auth-dev-nick` (laptop): R=1, local-only domain.
- `auth-sin` (regional data authority after promotion): R=3 within `sin`;
  exports selected routes to the installation serving view.
- Installation-root/substrate authority: R=3 on the canonical home/data region.

This is the existing "homogeneous substrate, storage eligibility per machine,
replica count is operator intent" model from `nats-native-control-plane.md`,
extended from one implicit home/data authority to N domains, one per authority.
Regions that are only compute/edge regions do not have a per-region replica
policy because they do not own durable authority state. The promotion guardrails
apply when a region is promoted into an authority.

## Global deploys, regional execution

The default deploy intent can be global even while durable state remains
regional:

```
ployzctl deploy
```

The foreground deploy primitive:

1. reads service policy from the home/data authority,
2. probes eligible regions for live capacity,
3. asks candidate machines for placement offers,
4. prepares and starts the chosen replicas,
5. writes deploy placement and route lifecycle events to the owning authority,
6. exports the serving view to public/shared projections if grants require it,
7. returns success only after the selected regional placements and route
   projections are observable.

For stateless services, `global` means "run replicas in every eligible region
with capacity." For stateful services, compute may be global but state remains
in the declared home authority until an explicit volume/stream fork, migration,
or regional authority promotion moves it.

## How gateways decide what to subscribe to

Each gateway carries a static, operator-provided config:

```toml
[gateway.public-sin]
serves = [
  { exports = "public.global" },
]

[gateway.home-us-west]
serves = [
  { authority = "auth-default", journal = true },
  { exports = "public.global" },
]

[gateway.promoted-sin]
serves = [
  { authority = "auth-sin", journal = true },
  { exports = "public.global" },
]

[gateway.dev-nick]
serves = [
  { authority = "auth-dev-nick", journal = true },
  { exports = "authority.auth-dev-nick" },
  { exports = "public.global" },
]
```

That config compiles into a deterministic subscription set:

- For each authority where the gateway is part of that authority's local
  serving surface: subscribe to `route_journal_<auth>` in that authority's
  domain.
- For public routes from other authorities: subscribe to `sync_public_routes`
  in installation-root.
- For routes shared with this authority: subscribe to
  `sync_authority_<self_authority>`.

No tier wildcards. The subscription set is a pure function of the gateway
config, authority registry, and grants, recomputable on `ployzctl status`.
Adding or revoking what a gateway sees is an explicit command. The command
writes the owner-local grant or withdraw event, configures the export feed and
any required root projection source, and returns success only after the serving
path is observable. If projection cannot be configured, the foreground command
fails loudly.

A supervised projection owner may retry failed projection setup and publish
operator-visible health, but it is not the source of route visibility truth and
does not silently decide new audiences.

## Route sharing and public exposure

### Private (default)

The owning authority publishes to
`ployz.v1.<inst>.<auth>.route.journal.event.>`. Only `<auth>`'s own account can
subscribe. No export exists.

### Shared (dev-to-dev)

Two-step:

1. Owner runs
   `ployzctl route grant --to <grantee_auth> --namespace <ns> [--ttl 24h]`.
   Writes the grant in the owner authority's `route_grants_<auth>` bucket.
2. The owner authority materializes an export feed at
   `ployz.v1.<inst>.<owner>.route.export.authority.<grantee_auth>.event.<ns>.>`.
   The export is derived from journal events matching the grant.
3. If the share crosses authorities through installation-root, the foreground
   command updates `sync_authority_<grantee>` to source that export feed. The
   grantee's gateway sees only its projection stream. Direct authority-to-
   authority sharing can skip root when the two authorities have a direct
   account import and the operator explicitly chooses that path.

Revocation writes `GrantRevoked` and `RouteWithdrawn` events in the owner
authority, then removes source lines and optionally purges projection subjects
as cleanup.

### Public exposure

Same shape, more careful guardrails:

1. Owner runs
   `ployzctl route expose --hostname <h> [--ttl 1h] [--auth <policy>]`. Writes
   a public-publish grant in the owner authority's `route_grants_<auth>` bucket.
2. The owner authority materializes an export feed at
   `ployz.v1.<inst>.<owner>.route.export.public.global.event.<ns>.>`, only for
   journal events matching a current public grant. If the grant is missing or
   expired, export fails loudly and journal truth remains private.
3. The foreground command updates `sync_public_routes` to source the public
   export feed and verifies that the public gateway projection sees it. TTL
   expiry writes `GrantExpired` and `RouteWithdrawn`; source removal and purge
   follow after the withdrawal is durable.

Public gateways subscribe **only** to `sync_public_routes`. They never see
private or shared subjects, by account import scope.

## Partition / staleness

Per-authority domains make this nearly free:

- A dev domain is its own JetStream cluster. Partitioned dev keeps reading and
  writing its own `cp.*`, `route.journal.*`, local `route.export.*`, and
  `work.cert.*`. Local R=1 means quorum is the laptop itself.
- The installation-root/home side stops seeing the dev's export events; that lag
  shows up as `sync_authority_<grantee>` source freshness on the root side.
- The dev side stops seeing home-authority `cp.*` updates and
  `sync_public_routes` deliveries; that shows up as mirror lag on the dev side.
- **No subject is written by two authorities.** Mirrors are unidirectional.
  There is nothing to merge on heal — each domain catches up on the other's
  appends.

Operator surface (extending the `control_plane component=...` status-entry pattern
already in `ployzctl status`):

- `domain_link component=leaf-<other_auth>` — leaf-bridge connectivity to each
  other domain, with stale-since.
- `mirror component=<stream_name>` — mirror lag, last-seen sequence vs. the
  authoritative leader's last-seen sequence (when reachable), stale-since.
- `projection component=sync_public_routes` /
  `projection component=sync_authority_<grantee>` — sources, current grants,
  last configure time, last configure error.
- `export component=<audience_kind>.<audience_id>` — current owner-local grant,
  source journal sequence, last exported sequence, stale-since, last error.
- `authority component=<auth>` — domain quorum state, replica
  current/configured/offline, leader, last write.

Any gateway projection that goes stale escalates the same way the existing
routing/cert subscriptions do: explicit failure event, projection marked
stale, sidecar metrics + status entry, last-good kept serving (with stale
flag), recovery only on a fresh successful subscription.

## Tier in subject vs. metadata — pros/cons

| | Tier in subject | Tier as authority metadata |
|---|---|---|
| Tier transition | Subject migration; rewrite ACLs | KV update; nothing moves |
| Wildcard subscriptions | `ployz.*.stable.>` cheap | Must enumerate authorities (or use sync streams) |
| Hard ACL boundary | Prefix match | NATS accounts (stronger) |
| Cross-tier sharing | Still needs grants/projections | Still needs grants/projections |
| Subject self-documenting | Yes | Subject says *what*, registry says *policy* |
| AI-operator reasoning | Tier is on the wire AND in registry — risk of drift | Single source of truth for tier |
| Failure mode if tier is wrong | Subjects misclassified; hard to fix | KV record wrong; one update fixes it |
| Account/import design | Account boundary does not align with prefix | Account = authority; exports per audience |

The wildcard convenience is real but small. Stable gateways need to subscribe
to `sync_public_routes` (one stream) regardless. Dev gateways enumerate the
authorities they serve regardless. The tier prefix does not actually save
subscription wiring once you take grants seriously.

## Why this shape

Authority is the only stable axis worth structuring subjects around — it is
the unit of write ownership, the unit of partition tolerance, the unit of
replica policy, and the unit of trust. Map it to JetStream domain + NATS
account and the rest of the requirements (dev partitions, promoted regional
data authorities, public gateways not seeing private dev routes, dev gateways
seeing public routes, public exposure with TTL) drop out of NATS-native
primitives without inventing a tier abstraction. Installation-root is a
directory and projection authority, not a master for private work. Substrate
machines belong to the installation and regions are recorded on the substrate;
authority participation is recorded separately. Route source truth belongs in
an authority-private journal; sharing and public exposure belong in explicit
export feeds with durable withdraw events because they are operator actions, not
inherent route identity. The AI-operator simplicity bet wins because there is
exactly one place where tier is recorded, exactly one place where route grants
are recorded, and the gateway subscription set is a pure function of those two
— no inference, no hidden state.
