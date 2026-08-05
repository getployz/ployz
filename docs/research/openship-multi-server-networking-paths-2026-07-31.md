# OpenShip paths to multi-server networking

Date: 2026-07-31

## Scope and conclusion

In this repository, **OpenShip means the external application-platform product
maintained at `oblien/openship`**, not a Ployz subsystem or codename. The existing
source-backed comparison fixes its relevant current shape: a central API and
database drive local Docker, an SSH-reachable Docker server, or a managed cloud
adapter; a self-hosted project's service network is one per-project Docker bridge
on one daemon. OpenShip's advertised multi-server fan-out and private networking
were roadmap items rather than implemented self-hosted cluster semantics at the
comparison's pinned revision
([README](../../README.md), [backbone](../architecture/backbone.md)).

The most plausible paths are therefore:

1. **Most likely near term: Docker Swarm behind OpenShip's central control
   plane.** The live upstream issue and draft PR now go materially beyond the
   earlier pinned comparison: they model stack-native ownership, manager
   discovery, overlay routing, registry-backed source builds, explicit edge
   topology, storage-risk checks, and observe-to-managed adoption. This is the
   only path with active implementation, although the draft is too broad and
   conflicted to merge as-is.
2. **Fallback/incremental path: central SSH fan-out without a scheduler.** Add a
   server group, explicit placement, replicated stateless workloads, and an
   externally supplied overlay while retaining the OpenShip API/database as
   authority. This can ship a smaller multi-server feature but does not supply
   self-healing cluster semantics.
3. **Credible durable alternative: make a Ployz v2 cluster an OpenShip runtime
   target.** OpenShip keeps projects, Git workflows, builds, deployment history,
   and UI; Ployz owns membership, placement, container networking, DNS, ingress,
   and runtime testimony. The boundary is OpenShip calling the cluster's
   HTTP/JSON/SSE API as a mesh peer, rather than OpenShip learning WireGuard,
   Corrosion, or per-machine Docker mechanics.
4. **Least likely: rebuild OpenShip itself as a converged per-machine control
   plane.** This could produce native multi-server semantics, but it replaces the
   product's central ownership and adapter model rather than extending it. It is
   effectively an architectural merger with Ployz and has the largest migration
   and compatibility cost.

The Swarm ranking follows current upstream activity; the other rankings are
inference from the primary repository sources below, not decisions recorded by
either project. A Ployz adapter remains the cleanest long-term division of
ownership, but it is not OpenShip's currently demonstrated implementation path.

## Live upstream status on 2026-07-31

The current OpenShip snapshot inspected was commit
[`1f73d583`](https://github.com/oblien/openship/commit/1f73d583268d4b67bcd55b8e08e4a87984921c2b).
The review covered all 92 checked-in documentation pages, all 148 GitHub issues,
and all 218 pull requests, then inspected the bodies, comments, files, commits,
and patches of the topology-relevant subset.

- The README still calls multi-server a roadmap feature and lists multi-node
  clusters, load-balancing UI, and private networking as "coming next"
  ([README](https://github.com/oblien/openship/blob/1f73d583268d4b67bcd55b8e08e4a87984921c2b/README.md#roadmap)).
- The current runtime remains one central API/database selecting one local,
  SSH-server, or Cloud target per deploy. Multi-service isolation is a local
  per-project Docker bridge, not a cross-host network
  ([runtime model](https://github.com/oblien/openship/blob/1f73d583268d4b67bcd55b8e08e4a87984921c2b/apps/web/content/docs/architecture/runtime-model.mdx),
  [isolation](https://github.com/oblien/openship/blob/1f73d583268d4b67bcd55b8e08e4a87984921c2b/apps/web/content/docs/security/isolation.mdx)).
- Server-to-server migration is real but is a stop-copy-start relocation with a
  DNS cutover, not simultaneous placement or failover. Locally built images
  cannot cross hosts; published images and volumes can
  ([migration guide](https://github.com/oblien/openship/blob/1f73d583268d4b67bcd55b8e08e4a87984921c2b/apps/web/content/docs/guides/server-to-server-migration.mdx),
  [merged implementation PR #235](https://github.com/oblien/openship/pull/235),
  [follow-up PR #277](https://github.com/oblien/openship/pull/277)).
- Native clustering has only a maintainer promise of a technical preview; its
  decisive storage question—pin, replicate, or require shared storage—remains
  unanswered ([issue #163](https://github.com/oblien/openship/issues/163)).
- Docker Swarm is explicitly separate from that native-clustering roadmap. Its
  accepted issue direction is manager-connected, stack-native operation,
  original Compose/stack source as authority, observe-before-manage adoption,
  external-first ingress, and registry-reachable digest deployment
  ([issue #316](https://github.com/oblien/openship/issues/316)).
- The active Swarm draft implements far more than its summary advertises:
  durable stack/service identities, discovery, drift, deployment and rollback,
  registry publishing, managed edge overlay/routing, storage-risk classification,
  manager rebinding, and failure tests. However, it currently has 28,207
  additions across 234 files, 80 commits, merge conflicts, no completed review,
  and no reported checks. The likely upstream path is therefore to rebase and
  extract reviewable slices, beginning with the safety boundary that refuses to
  mutate scheduler-owned task containers
  ([PR #317](https://github.com/oblien/openship/pull/317),
  [triggering failure #311](https://github.com/oblien/openship/issues/311)).
- External ingress and private management transport are compatible supporting
  seams, not cluster schedulers: keep SSH over Tailscale distinct from public
  ingress ([issue #12](https://github.com/oblien/openship/issues/12)); optionally
  split an outbound deployer from the control plane
  ([issue #13](https://github.com/oblien/openship/issues/13)); and add DNS/tunnel
  providers without making them the workload overlay
  ([issue #37](https://github.com/oblien/openship/issues/37),
  [PR #59](https://github.com/oblien/openship/pull/59)).

## What exists today

### OpenShip

The pinned OpenShip study describes one central API/database as product authority,
with remote-machine access through SSH, Docker socket tunnelling, shell/system
effects, OpenResty route files, and a remote mutation journal. Existing workloads
and static routes survive loss of the API, but deploys, changes, observation, and
API-owned schedules stop. The remote path also lacked the local path's
post-activation readiness probe at the studied revision
([OpenShip runtime model](https://github.com/oblien/openship/tree/main/apps/web/content/docs/architecture)).

That model can select a remote **server**, but it does not yet define a cluster:
there is no shared membership authority, cross-machine endpoint network,
machine-to-machine identity, distributed DNS, placement testimony, or
multi-gateway certificate distribution. The comparison explicitly records
multi-node clusters, private networking, and load-balancing UI as future work
([OpenShip runtime model](https://github.com/oblien/openship/tree/main/apps/web/content/docs/architecture)).

### Ployz

Ployz's accepted v2 direction is already a multi-server architecture: one
`ployzd` binary and one pinned Corrosion sidecar per Linux machine; a pluggable
WireGuard mesh; HTTP/JSON with SSE served by every machine; and no core,
sequencer, NATS, quorum, or coordination point
([README](../../README.md),
[ADR 0040](../adr/0040-corrosion-replaces-the-core-and-nats.md)). Every machine
holds all cluster config, while config rows and machine testimony have distinct
writer classes. Any reachable machine accepts commands, and losing one machine
does not block commanding the rest
([backbone](../architecture/backbone.md#thesis),
[availability contract](../architecture/backbone.md#availability-contract)).

The current Rust source tree is the coreless v2 destination described above, but
it is still integration-incomplete. The contributor map and crate-topology
specification are the current implementation references
([code map](../architecture/code-map.md),
[v2 crate topology](../design/binary-crate-topology.md)). Consequently Ployz v2
is a decided and substantially specified destination, not yet an
integration-ready runtime that OpenShip could adopt unchanged.

## The networking shape OpenShip would otherwise have to build

Ployz separates two planes over one pairwise WireGuard mesh:

- A derived ULA IPv6 identity carries Corrosion gossip, HTTP, SSE, CLI traffic,
  and caller identity. It does not move when a container subnet is repaired.
- A randomly allocated per-machine IPv4 `/24` carries container-to-container
  traffic, service DNS, and gateway upstreams. WireGuard `AllowedIPs` contains
  both the machine's IPv6 `/112` and container IPv4 `/24`, so cross-machine
  container routing requires no per-container route rows.

The default `10.210.0.0/16` intentionally tops out near 250 machines; larger
scale uses separate cells. The prefix is selected at init so LAN collisions can
be avoided
([addressing design](../design/mesh-provider-and-principal.md#builtin-addressing-dual-stack-each-family-doing-one-job),
[scale guardrail](../architecture/backbone.md#guardrails)).

Admission is a public join-only HTTPS door with a revocable token and pinned
door-certificate fingerprint. Accepted machines and non-machine peers (operator
laptop or Cloud) join the roster; source address plus cryptokey routing resolves
to a typed `Principal`. That makes membership the security decision and rejects
ambient trusted-network or header identity
([principal](../design/mesh-provider-and-principal.md#identity-one-resolution-rule-the-principal-enum),
[join](../design/mesh-provider-and-principal.md#join-one-public-door-token-in-hand)).

Each machine gets one Docker bridge over its `/24`. Cluster-local DNS is answered
on every machine from locally converged Corrosion rows, producing A records for
the active service revision. Cross-namespace container traffic is denied by a
root-cgroup `cgroup_skb` policy map derived from container testimony
([container plane](../design/mesh-provider-and-principal.md#the-container-plane)).

This is the minimum coherent set behind “private networking”: membership,
address allocation, route convergence, transport identity, join/removal,
container IPAM, DNS, isolation, freshness, and repair. Implementing only an
overlay tunnel in OpenShip would leave the other cluster semantics unresolved.

## Path 1: central OpenShip SSH fan-out

This path preserves OpenShip's current architecture. A project would target a
server pool instead of one frozen server, the API would select machines and
drive each through its existing SSH/Docker adapters, and a centrally managed
WireGuard or third-party overlay would make per-host service networks reachable.
OpenResty configuration could be projected to multiple ingress machines and DNS
could publish their addresses.

It is most likely if the objective is the shortest route to a useful two-to-ten
server product because OpenShip already owns servers, encrypted SSH credentials,
deploy sequencing, routing adapters, and reconciliation after ambiguous SSH
outcomes
([deploy operations](../adr/0003-operations-are-informational-records-not-workflows.md),
[deploy reconciliation](../adr/0004-deploys-are-namespace-reconciliation-attempts.md)).

The architectural cost is cumulative. The central database must now own server
membership, address allocation, placement, service endpoints, and gateway
projection. It must reconcile network state on every host and distinguish stale
observation from negative truth. The existing broad SSH authority becomes
cluster-wide, and API/database loss leaves the running data plane intact but
halts repair and mutation. This is a viable product trade for a small trusted
fleet, but it does not acquire Ployz's “command through any survivor” property.

## Path 2: Ployz as OpenShip's cluster runtime

This is the strongest division of ownership:

| OpenShip retains | Ployz cluster owns |
| --- | --- |
| Organizations, projects, Git integration, workflow, build UX, rich deployment history, notifications | Machine admission/removal, mesh identity, placement, container execution, service DNS, namespace isolation, ingress, live runtime testimony |
| Application intent and user-facing progress | Cluster runtime truth and operation evidence |
| A runtime adapter/client | HTTP/JSON/SSE API served by any machine |

The fit is explicit in Ployz's vision: Cloud is a consumer and ordinary mesh
peer, owns product workflow state, writes the same rows/commands as the CLI, and
does not orchestrate machine-local work
([Cloud relationship](../../VISION.md#cloud-relationship)). Non-machine peers are
first-class roster entries with operator authority, and the design uses the same
join door for machines, laptops, and Cloud
([peers](../design/mesh-provider-and-principal.md#the-peers-table),
[join](../design/mesh-provider-and-principal.md#join-one-public-door-token-in-hand)).

The integration should therefore be an OpenShip `PloyzRuntime` (or cluster target)
that:

1. enrolls as a `Peer` through the supported mesh flow;
2. feature-detects the cluster with `GET /version`;
3. submits typed, bounded cluster commands over HTTP;
4. consumes SSE operation progress and watches relevant rows/status;
5. stores richer product history without treating its database as runtime truth.

Ployz's topology already assigns future-version adaptation to the continuously
deployed Cloud caller and keeps the cluster's API additive and
self-describing—exactly the version skew OpenShip would face across customer
clusters
([cross-version contract](../design/binary-crate-topology.md#cross-version-compatibility-cloud--any-version-clusters)).

The dependencies are substantial but bounded: Ployz must first land the v2
runtime collapse, builtin WireGuard provider, join/peer flow, HTTP/SSE API,
container plane, and stable deploy contracts. OpenShip must add a cluster target
that does not also SSH into the member machines, plus an ownership rule for
project migration between its existing local/server/cloud targets and a Ployz
cluster. Builds can remain OpenShip-owned initially if their result is an
immutable image reference consumable by Ployz; build execution venue should stay
separate from runtime placement, a distinction the prior comparison already
identified
([SDK surface](../design/sdk-cloud-api-surface.md)).

## Path 3: make OpenShip itself converged and mesh-native

OpenShip could adopt the same architecture internally: install a machine agent,
replicate project/runtime rows to every server, resolve identity through the
mesh, and serve its API on every node. This removes the central operational
dependency and makes OpenShip itself the runtime authority.

It is the least likely path because it conflicts with OpenShip's crisp current
single-owner rule—local/server projects are canonical in the self-hosted
database, cloud projects in SaaS—and replaces its adapter-driven central worker
with distributed folds
([row rules](../architecture/backbone.md#row-rules),
[code map](../architecture/code-map.md)).
It would also need to adopt Ployz's row-writer discipline, optimistic uniqueness,
schema rules, tombstone/reseed policy, and no-secrets-in-rows constraint rather
than merely adding Corrosion as a database
([row model](../design/corrosion-row-model.md#the-row-ownership-law),
[cross-cutting conventions](../design/corrosion-row-model.md#cross-cutting-conventions),
[secrecy](../design/corrosion-row-model.md#no-secret-values)).

## Constraints and decision points

- **Trust:** Ployz v2 deliberately equates membership with config write authority
  for a single-operator cluster. Hostile-edge and multi-tenant membership require
  a later operator-signing tier
  ([trust ceiling](../architecture/backbone.md#trust-ceiling)). OpenShip must not
  present one Ployz cluster as a hostile multi-tenant boundary.
- **NAT:** builtin WireGuard supports a NATed member that can dial outward, but
  NAT-to-NAT pairs have no hole punching or relay. The specified answer is a
  future Tailscale provider; v1 ships builtin WireGuard only
  ([provider limits](../design/mesh-provider-and-principal.md#join-one-public-door-token-in-hand),
  [provider matrix](../design/mesh-provider-and-principal.md#provider-matrix)).
- **Linux:** workload machines require Linux facilities: kernel WireGuard,
  cgroups, eBPF, systemd, Docker networking, and sysctls. macOS is a peer unless
  it hosts a Linux VM
  ([machine boundary](../design/mesh-provider-and-principal.md#the-machine-boundary-rule)).
- **Failure semantics:** DNS intentionally serves recorded active endpoints
  without liveness filtering; a dead machine's single replica remains a dead
  answer until explicit repair. Freshness is surfaced through mesh handshake age
  rather than silently folded into truth
  ([service DNS](../design/mesh-provider-and-principal.md#service-dns-a-records-from-rows-as-written)).
- **Ingress and certificates:** multiple direct gateways need holder-to-holder
  certificate distribution over the mesh; key material never enters Corrosion.
  Tunnel providers terminate TLS and bypass that machinery
  ([certificate model](../design/corrosion-row-model.md#certificates)).
- **Initialization order:** WireGuard identity must exist before Corrosion can
  bind its ULA address, and Corrosion must be live before initial rows are
  written. The machine-one spec makes this a resumable, check-then-do chain
  ([mint list](../design/ployz-init-machine-one.md#the-mint-list)).
- **Unverified seam:** the mesh design explicitly requires confirming that the
  pinned Corrosion build gossips over ULA IPv6; the prototype spike used IPv4
  ([implementation check](../design/mesh-provider-and-principal.md#builtin-addressing-dual-stack-each-family-doing-one-job)).

## Recommended sequence

1. Treat OpenShip's immediate multi-server work and the Ployz integration as
   separate milestones. If OpenShip needs a near-term feature, ship explicit
   central server-pool fan-out with its limitations rather than branding it a
   self-healing cluster.
2. Finish and certify Ployz v2's mesh substrate first: machine-one init, machine
   join/removal, peer enrollment, Corrosion over ULA IPv6, `/version`, and
   command/SSE access through more than one machine.
3. Prove the container plane on two real Linux hosts: allocated `/24`s, routed
   cross-machine traffic, active-revision DNS, namespace isolation, gateway
   upstreams, restart, partition, and subnet-collision repair. The repository's
   test map reserves DinD and real-host acceptance for exactly these seams
   ([verification map](../architecture/code-map.md#test-and-verification-map)).
4. Build one narrow OpenShip runtime adapter against a Ployz cluster: enroll one
   peer, deploy one immutable image across two machines, stream operation status,
   and route traffic through two gateways. Keep OpenShip's database as workflow
   history, not cluster truth.
5. Only after that proof decide whether OpenShip's native SSH fan-out remains a
   separate lower-capability target or whether Ployz becomes its sole
   multi-server target.

The decisive product question is not which tunnel to use. It is whether OpenShip
wants to remain the runtime authority. If yes, Path 1 is the honest incremental
route. If no, Path 2 is the most likely durable architecture because it adds
multi-server networking without forcing OpenShip to become a distributed
systems project.
