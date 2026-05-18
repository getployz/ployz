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
| Durable facts | p2panda signed operation log plus local store behind `FactSource` |
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
- fact-sync, projection, and snapshot applier roles keep consuming
  already-authorized replicated serving-state facts and can publish new local
  gateway/DNS snapshots without the coordinator,
- new mutations and operator commands for that node fail visibly until the
  coordinator returns.

That means "daemon" cannot be shorthand for every local control-plane
responsibility. The coordinator proposes and coordinates changes. Separate
steady-state actors or process roles apply already-committed local state to
snapshots and data-plane configuration. Killing the coordinator should remove
the node's ability to accept fresh mutations; it should not remove its ability
to serve, route, resolve DNS, keep workloads alive, or observe replicated
commits that other live coordinators have already made.

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
- `FactStoreActor` owns signed fact operations, local persistence, and fact
  ingestion.
- `ProjectionActor` owns deterministic reduction from fact candidates into
  SQLite.
- Serving-state writers publish atomic local state consumed by HTTP/DNS serving
  roles. The exact gateway/DNS state shape is a slice-level design decision.
- `WireGuardActor` owns full-mesh peer reconciliation for the MVP.
- `DeployCoordinatorActor` owns deploy state machines and durable commit
  boundaries.
- Steady-state appliers are not deploy coordinators. Applying
  already-replicated serving state must survive coordinator failure in the MVP
  process-role design.

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
- Request-many aggregation remains an MVP proof requirement for observation and
  capacity checks. It is not a quorum mechanism for writes.

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

p2panda signed operations are the preferred durable fact substrate. SQLite is a
local projection. iroh-docs remains historical proof/reference material unless a
future slice explicitly reintroduces it for a narrower bridge.

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
facts/machine-remove/<node_id>/<removal_epoch>/decision
facts/machine-remove/<node_id>/<removal_epoch>/cleanup/done
facts/routes/<route_commit_id>
facts/lease/<resource>/<epoch>/claimed
facts/lease/<resource>/<epoch>/released/<content_hash>
facts/acme/http01/<hostname>/<token>/<epoch>/presented
facts/acme/http01/<hostname>/<token>/<epoch>/cleared/<content_hash>
facts/dns/<commit_id>
facts/gateway/<commit_id>
```

Projection pipeline:

```text
p2panda fact operations
  -> deterministic reducer
  -> projections.sqlite
  -> gateway.snapshot / dns.snapshot
  -> gateway and dns reload
```

p2panda-net is the preferred maintained transport for moving fact operations
between nodes. In the current MVP shape it is a carrier, not the authority
boundary: live fact-node transport carries canonical
`Operation<PandaFactExtensions>` values, and received operations must still be
imported through the `PandaFactStore` trusted same-island replica gate before
projection can observe them. A p2panda-net store populated before Ployz
authorization is transport state, not durable cluster truth.

Correctness must not depend on `applied_fact` bookkeeping. If projection state
is uncertain, delete/rebuild SQLite from facts and atomically publish a fresh
snapshot.

### Fact Store Contract

The fact substrate is an append-only signed operation log, not a consensus
system. Ployz imposes the fact-ledger rules above it:

- Fact keys are write-once by Ployz policy.
- Reusing a fact key with a different content hash is a conflict.
- The operator's connected node is the consistency boundary for a command.
  Foreground commands write durably to that node's local fact store and return.
  Other nodes learn the fact through eventual replication.
- There is no commit quorum, `min_replicas`, or witness-ack collection for fact
  writes.
- Commands read relevant durable facts before their first mutation. If the
  precondition set already contains incompatible intent, the command fails with
  a structured `Conflict` naming the conflicting fact, principal, and time.
- Surviving write races remain in the fact set as conflict candidates. Reducers
  order candidates deterministically by `(epoch desc, content_hash asc)` and
  annotate losers as `Superseded { by_epoch, by_principal, at }` in projection
  status.
- Operator status surfaces `Superseded` events for commands authored by that
  operator. Conflict-as-candidate is substrate machinery; the command surface
  stays loud.
- Current heads are projected views, not mutable authority keys.

Command results include the visible nodes at decision time. Reachability is
evidence for the operator, not a hidden quorum gate.

A future membership or active-partition view may make that reachability evidence
better by checking known-alive members before mutation. That should be modeled
as explicit decision-time evidence and structured precondition failure, not as a
peer-ack commit protocol.

## Advisory Lease Facts

Leases are advisory coordination facts, not cluster-enforced locks. They help
commands avoid obvious races and carry fencing tokens into resource-specific
mutation code, but they do not claim exclusive truth across partitions.

Lease facts should model:

- resource id,
- holder principal,
- epoch,
- TTL and expiry,
- renewal,
- release,
- fencing token,
- RAII release-on-drop for local holders where a process owns a lease guard.

There is no quorum mode, no opt-in strict mode, and no witness-ack collection.
The lease reducer uses the same conflict-as-candidate fact contract as every
other reducer. Surviving races are ordered deterministically by
`(epoch desc, content_hash asc)` and surfaced as superseded projection status.

Real exclusivity belongs to the resource being mutated. For ACME that means the
ACME directory and challenge validation path. For storage it means the storage
backend or filesystem primitive. A Ployz lease only decides whether a command
should proceed and which epoch/fencing token it must carry when it talks to the
resource.

Slice 021 proves the ACME HTTP-01 canary on this contract: lease and challenge
facts are signed p2panda operations, sync is performed through the generic
p2panda-sync adapter, projection rebuilds on another local store, stale synced
facts are superseded instead of rolling serving state back, and HTTP-01 keeps
serving last-good state while the issuer/coordinator adapter is absent.

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

Durability for commits:

```text
write fact locally
then treat commit as durable
replicate to other nodes eventually
```

The route commit is durable when the coordinator's connected node has persisted
the fact to its local fact store. The command result reports visible nodes at
decision time, but it does not wait for peer acknowledgements before moving to
the next command-state transition. If another race survives into replication,
the reducer's deterministic supersession rules make the projected winner and
loser visible.

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

The normal join path does not clear tombstones. Once a node id is tombstoned,
later `NodeJoined` or service-registration facts for that node are ignored until
a future slice defines an explicit reinvite/clear primitive with its own grants
and failure semantics.

## Target Machine Remove Flow

Machine remove is a command-shaped flow. Membership facts describe cluster
truth; command facts describe recoverable operator intent and completion.
Projection state may prove catch-up, but it is not request context.

Graceful remove:

1. Probe the target participant.
2. Write `MachineRemoveDecision` with visible nodes, epochs, reason, and exact
   serving plan.
3. Write `NodeRemovalStarted`.
4. Revoke scheduling or mark `no_new_work`.
5. Request `node.<id>.rpc.drain_workloads`.
6. Commit route facts removing active backends.
7. Wait for local projection catch-up to the serving commit.
8. Stop old workloads.
9. Write `NodeTombstone`.
10. Write `MachineRemoveCleanupDone`.
11. Remove WireGuard peer.
12. Expire service registry endpoints.

If the coordinator dies after serving commit and before stop/tombstone, a fresh
coordinator reads the decision plus exact serving commit from `FactSource` and
resumes cleanup. It must not replay probe, drain, or serving writes. A raw
tombstone excludes the node from projections, but cleanup is complete only when
cleanup-done validates against the decision and the expected tombstone fact.

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
