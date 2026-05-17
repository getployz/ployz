---
title: MVP Architecture
status: active
created: 2026-05-17
---

# MVP Architecture

## Frame

Ployz is still a primitive orchestration core for small clusters. The MVP keeps
the product direction from [VISION.md](../VISION.md): commands have visible
preconditions, bounded effects, clear results, and verification hooks.

This document is a strategic amendment to the current substrate direction in
`VISION.md` and `docs/architecture.md`. Those documents still describe NATS as
the deployed control-plane substrate. This MVP proposes a change: NATS remains
the semantic model, but not the server topology Ployz has to operate.

The current NATS-backed model has good semantics but imports an operated NATS
server topology. The MVP keeps the useful Core NATS ontology and implements it
as Ployz-native semantics over iroh:

- subjects
- wildcard subscriptions
- request/reply inboxes
- no-responder failures
- queue groups
- services and endpoints
- drain
- accounts/imports/exports, renamed as authority islands
- subject permissions and temporary response permission

The candidate substrate becomes:

| Concern | MVP choice |
| --- | --- |
| Connectivity | iroh endpoint, QUIC streams, ALPN router |
| Local structure | Kameo actors, supervision, bounded mailboxes |
| Notifications | PloyzBus over iroh streams and iroh-gossip |
| Request/reply | PloyzBus inbox protocol over iroh QUIC streams |
| Durable facts | iroh-docs replicated fact set |
| Large payloads | iroh-blobs content-addressed payloads |
| Local queries | SQLite projection/cache |
| Data plane | WireGuard |
| HTTP serving | Product primitive; Pingora is a candidate runtime |
| DNS serving | Product primitive; process/role shape is open |

External grounding:

- iroh protocol routing supports ALPN-based protocol handlers and multiplexed
  protocols on one endpoint:
  <https://docs.rs/iroh/latest/iroh/protocol/index.html>
- iroh's router documentation describes composing multiple protocols on the
  same endpoint:
  <https://www.iroh.computer/docs/concepts/router>
- Kameo actors provide async actors, lifecycle hooks, bounded mailboxes, and
  supervision:
  <https://docs.rs/kameo/latest/kameo/actor/index.html>
- NATS queue groups and no-responder semantics are the model for load-balanced
  request handling and foreground failure:
  <https://docs.nats.io/nats-concepts/core-nats/queue>
- NATS authorization documents subject-level permissions and temporary response
  publishing via response permissions:
  <https://docs.nats.io/running-a-nats-service/configuration/securing_nats/authorization>

## System Shape

During the MVP rewrite, all new implementation code lives under `MVP/`. The
existing `crates/` tree is reference material, not an implementation target.
Do not wire MVP code into the root workspace, existing daemon, existing
gateway/DNS binaries, or existing E2E runner until the MVP has produced enough
proof to justify a deliberate migration.

One shipped Rust artifact may expose multiple roles:

```text
ployz daemon
ployz gateway
ployz dns
```

The existing binaries may remain during migration:

```text
ployzd
ployz-gateway
ployz-dns
ployzctl
```

HTTP and DNS serving must preserve data-plane continuity when the daemon is
down. The current gateway and DNS binaries are reference material, not
non-negotiable process boundaries. Pingora may remain the right HTTP serving
primitive, but the state shape that feeds it is explicitly up for redesign.
Whatever role/process boundary the MVP chooses must prove that serving does not
depend on daemon liveness.

The command/coordinator daemon is a composition root, router, and lifecycle
owner for mutations. It must not become a registry of feature state, and it must
not be the fate-sharing boundary for steady-state data-plane behavior. Feature
state belongs to actors/subsystems, and steady-state roles need their own
lifecycle story.

When the coordinator role is down:

- existing workloads keep running,
- WireGuard keeps carrying service-to-service traffic with last applied config,
- HTTP/DNS serving keeps answering from last good local state,
- local state appliers can keep applying already-replicated serving-state facts
  if they are not part of the crashed coordinator role,
- new mutations and operator commands for that node fail visibly until the
  coordinator returns.

## Actor Ownership Boundaries

The local runtime should be actor-owned and Kameo-first, but the exact actor
tree is not fixed by this strategy document. Each implementation slice should
justify the actors it introduces.

Candidate ownership boundaries:

```text
MeshSupervisor
├─ IrohEndpointActor
├─ BusActor
├─ AuthorityIslandActor
├─ JoinActor
├─ MembershipActor
├─ ServiceRegistryActor
├─ DocsActor
├─ BlobActor
├─ ProjectionActor
├─ WireGuardActor
├─ DeployCoordinatorActor
├─ RuntimeActor
├─ GatewaySnapshotWriterActor
└─ DnsSnapshotWriterActor
```

Ownership rules to preserve as these boundaries become real:

- `IrohEndpointActor` owns endpoint lifecycle, ALPN router startup, connection
  handles, and graceful shutdown.
- `BusActor` owns local subscriptions, request correlation, inbox expiry,
  queue-member selection, and no-responder detection.
- `AuthorityIslandActor` owns grants, import/export rules, membership view, and
  fact-write authorization.
- `ServiceRegistryActor` owns local service registrations and projects remote
  service facts into bus interest.
- `DocsActor` owns iroh-docs replicas and fact ingestion.
- `ProjectionActor` owns deterministic reduction from docs facts into SQLite.
- Serving-state writers publish atomic local state consumed by HTTP/DNS serving
  roles. The exact gateway/DNS state shape is a slice-level design decision.
- `WireGuardActor` owns full-mesh peer reconciliation for the MVP.
- `DeployCoordinatorActor` owns deploy state machines and durable commit
  boundaries.
- Steady-state appliers are not the same thing as deploy coordinators. Applying
  already-replicated serving state should be able to survive coordinator
  failure if the process-role design keeps those responsibilities separate.

Actors communicate with typed messages. No subsystem should reach into another
actor's internal state.

## Authority Islands

Authority islands are Ployz's product-level version of NATS accounts. They are
authority and visibility boundaries, not geography.

```rust
struct AuthorityIsland {
    id: IslandId,
    name: IslandName,
    root_keys: Vec<PublicKey>,
    member_nodes: Vec<NodeId>,
    principals: Vec<PrincipalId>,
    grants: Vec<Grant>,
    exports: Vec<ExportRule>,
    imports: Vec<ImportRule>,
    docs_namespace: DocsNamespace,
    subject_namespace: SubjectNamespace,
}
```

Rules:

- Subjects are island-local.
- A laptop dev island is as real as production; it just owns different truth.
- Cross-island behavior uses imports/exports. It does not merge databases.
- Transport identity does not imply authority.
- A node key proves who connected. A grant decides what the principal may do.
- A signed fact proves who wrote durable truth.

## PloyzBus

PloyzBus is the internal control-plane contract. The public product primitives
remain operator commands such as machine add/remove, deploy, migrate, branch,
promote, rollback, fork-volume, and dev. E2E proof must terminate in
operator-visible command results, not bus semantics alone.

iroh-gossip and custom iroh streams are transport mechanisms under PloyzBus.

```rust
trait PloyzBus {
    async fn publish(&self, msg: PublishMessage) -> Result<(), PublishError>;
    async fn subscribe(&self, pattern: SubjectPattern, handler: HandlerId)
        -> Result<SubscriptionId, SubscribeError>;
    async fn request(&self, req: RequestMessage, timeout: Duration)
        -> Result<ResponseMessage, RequestError>;
    async fn request_many(&self, target: RequestTarget, req: RequestMessage, policy: RequestManyPolicy)
        -> Result<Vec<ResponseMessage>, RequestManyError>;
    async fn queue_subscribe(&self, pattern: SubjectPattern, queue: QueueName, handler: HandlerId)
        -> Result<QueueSubscriptionId, SubscribeError>;
    async fn drain(&self, deadline: Duration) -> Result<(), DrainError>;
}
```

Messages:

```rust
struct BusMessage {
    id: MessageId,
    island: IslandId,
    subject: Subject,
    reply_to: Option<ReplyTo>,
    headers: Headers,
    payload: PayloadRef,
    auth: AuthContext,
    deadline: Timestamp,
}

struct ReplyTo {
    inbox: Subject,
    endpoint: EndpointId,
    request_id: MessageId,
    expires_at: Timestamp,
}

enum RequestTarget {
    Subject(Subject),
    Pattern(SubjectPattern),
}

struct ReplyPermit {
    inbox: Subject,
    request_id: MessageId,
    responder: PrincipalId,
    expires_at: Timestamp,
}
```

Request-many policy:

```rust
struct RequestManyPolicy {
    max: usize,
    deadline: Duration,
}
```

Semantics:

- `publish` fans out to matching ordinary subscribers.
- `queue_subscribe` delivers each message to one eligible group member.
- `request` returns `NoResponders` immediately when local/known interest proves
  no eligible responder exists.
- `request` targets one concrete subject.
- `request_many` is for fanout and aggregation, not queue groups. It may target
  a concrete subject or a subject pattern through `RequestTarget`.
- Reply inboxes are ephemeral and direct. Do not gossip every inbox
  subscription.
- Request handlers receive a one-use reply permit. Responses are accepted only
  through that permit before its deadline, which makes temporary response
  authorization testable.
- Large payloads should move through content-addressed references once a slice
  needs them.
- Gossip is a wake-up and interest-dissemination path. It is not durable truth.
- Quorum-style pinning and all-responder aggregation remain MVP proof
  requirements. They do not have to be represented as core bus enum variants
  until implementation proves that is clearer than composing `request_many` at
  callers.

## Subject Namespace

Initial island-local subjects:

```text
node.<node_id>.status
node.<node_id>.rpc.inspect
node.*.capacity
deploy.submit
deploy.<deploy_id>.status
deploy.<deploy_id>.inspect
gateway.changed
dns.changed
store.fact.inserted
store.pin_fact
service.<namespace>.<service>.changed
_INBOX.<principal>.<random>
$SYS.service.ping
$SYS.service.info
$SYS.node.health
```

Future cross-island mappings should keep local subject names natural:

```text
laptop island import:
  local  gpu.deploy.submit
  remote prod:deploy.submit
```

Do not globally prefix every subject with an island id. Islands are part of
message context and bridge policy.

## Grants

```rust
struct Grant {
    island: IslandId,
    principal: PrincipalId,
    publish_allow: Vec<SubjectPattern>,
    subscribe_allow: Vec<SubjectPattern>,
    respond_allow: Vec<ResponseGrant>,
    fact_write_allow: Vec<FactKeyPattern>,
}
```

Authorization checks happen before handler dispatch and before fact writes.
Temporary response permission is scoped to one request id, one inbox, and one
deadline. Deny lists, richer queue grants, and explicit RPC pattern grants can
be added when a slice reaches those authorization proofs; the MVP still has to
prove subject, response, queue, bridge, and fact-write authorization.

## Facts And Projections

iroh-docs is the replicated durable fact set. SQLite is a local projection.

Fact envelope:

```rust
struct Fact {
    island: IslandId,
    key: FactKey,
    author: PrincipalId,
    signature: Signature,
    content_hash: BlobHash,
    kind: FactKind,
    created_at: Timestamp,
}
```

Fact keys should be mostly immutable:

```text
facts/cluster/genesis
facts/node/<node_id>/joined/<epoch>
facts/node/<node_id>/tombstoned/<epoch>
facts/node/<node_id>/capabilities/<epoch>
facts/service/<service>/<node_id>/<epoch>
facts/deploy/<deploy_id>/plan
facts/deploy/<deploy_id>/computed_plan
facts/deploy/<deploy_id>/phase/<n>/ready
facts/deploy/<deploy_id>/phase/<n>/commit
facts/deploy/<deploy_id>/done
facts/routes/<route_commit_id>
facts/dns/<commit_id>
facts/gateway/<commit_id>
```

Projection pipeline:

```text
iroh-docs facts
  -> deterministic reducer
  -> projections.sqlite
  -> gateway.snapshot / dns.snapshot
  -> gateway and dns reload
```

Correctness must not depend on `applied_fact` bookkeeping. If projection state
is uncertain, delete/rebuild SQLite from facts and atomically publish a fresh
snapshot.

### Fact Store Contract

iroh-docs is a replicated document/key-value substrate, not an append-only log
or consensus system. Ployz imposes the fact-ledger rules above it:

- Fact keys are write-once by Ployz policy.
- Reusing a fact key with a different content hash is a conflict.
- Reducers ignore unauthorized or conflicting entries and emit visible status
  for operators/tests.
- `store.pin_fact` means a peer has verified and stored the fact envelope and
  referenced content. It is durability evidence, not consensus.
- Current heads are projected views, not mutable authority keys.

## Deploy Invariant

Deploy remains a command-shaped foreground operation:

```text
DeployPlanned
  -> PhasePreparing(n)
  -> PhaseReady(n)
  -> PhaseCommitted(n)
  -> PhaseDraining(n)
  -> PhaseDone(n)
  -> DeployDone
```

Core invariant:

```text
route cutover is a durable fact
drain is a consequence of that fact
```

No route cutover before phase ready. No drain before durable phase commit. No
old-instance removal before drain policy is satisfied.

Durability for serious commits:

```text
write fact locally
request_many store.pin_fact to selected peers
require min_replicas = min(3, live_nodes) for MVP
then treat commit as durable
```

This is durability replication, not consensus. It prevents draining old
production after only one coordinator has observed the route commit.

## Target Machine Add Flow

This is a target flow, not a committed first slice. The first implementation
proof should carve this down to the smallest useful join path.

`ployz init` creates:

- first iroh endpoint identity
- first WireGuard key
- first authority island
- docs namespace
- local projection database
- local bus
- first node facts

`ployz machine invite` writes an island-scoped invite fact and emits a token
with bootstrap endpoint, invite id, secret, expiration, and initial grant
templates.

`ployz join <token>`:

1. Generates iroh and WireGuard keys.
2. Dials bootstrap over iroh.
3. Proves the invite secret.
4. Receives island membership, docs access, and initial grants.
5. Writes `NodeJoined`.
6. Registers node-agent service endpoints.
7. Joins bus/gossip/docs sync.
8. Reconciles full-mesh WireGuard.

For the initial product target, clusters up to 32 nodes can use full-mesh
WireGuard and direct request-many fanout.

## Target Machine Remove Flow

This is a target flow, not a committed first slice. The first implementation
proof should establish at least one tombstone/removal invariant before expanding
into graceful workload drain.

Graceful remove:

1. Write `NodeRemovalStarted`.
2. Revoke scheduling or mark `no_new_work`.
3. Request `node.<id>.rpc.drain_workloads`.
4. Commit route facts removing active backends.
5. Wait drain policy.
6. Stop old workloads.
7. Write `NodeTombstone`.
8. Remove WireGuard peer.
9. Expire service registry endpoints.

Force remove:

1. Write `NodeTombstone`.
2. Revoke grants.
3. Exclude from scheduler/gateway projections.
4. Remove WireGuard peer.
5. Ignore future facts signed by that node unless re-invited.

## Migration From Current Code

Study, but do not preserve by default:

- Pingora HTTP serving implementation and snapshot state patterns in
  [crates/ployz-gateway](../crates/ployz-gateway)
- DNS serving behavior in
  [crates/ployz-dns](../crates/ployz-dns)
- WireGuard backend mechanics under
  [crates/ployz-orchestrator/src/mesh](../crates/ployz-orchestrator/src/mesh)
  and
  [crates/ployz-runtime-backends/src/mesh](../crates/ployz-runtime-backends/src/mesh)
- Deploy commit invariants from
  [docs/routing-and-deploys.md](../docs/routing-and-deploys.md)
- Authority/status/observation separation from
  [docs/authority-roadmap.md](../docs/authority-roadmap.md)
- ACME behavior from
  [crates/ployz-cert-backends](../crates/ployz-cert-backends) and
  [crates/ployzd/src/daemon/cert_coordination.rs](../crates/ployzd/src/daemon/cert_coordination.rs)

Replace or wrap:

- NATS store assets as the cluster truth mechanism.
- Direct HTTP/DNS serving dependence on `NatsStore`.
- Daemon handler files that combine transport, orchestration, storage, and
  presentation.
- The old deploy coordinator shape in
  [crates/ployzd/src/daemon/deploy.rs](../crates/ployzd/src/daemon/deploy.rs).

## HTTP/DNS Serving Prerequisite

The concrete requirement is data-plane continuity, not preservation of the old
gateway/DNS input model. Today gateway and DNS connect to `NatsStore` before
serving. The MVP path must introduce a serving-state adapter/load path:

- HTTP serving loads last good local state before trying live control-plane
  connectivity,
- DNS serving loads last good local state before trying live control-plane
  connectivity,
- invalid next state does not replace the in-memory last good state,
- bus/projection notifications trigger reloads but are not serving
  dependencies.
