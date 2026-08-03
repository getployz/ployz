# Ephemeral hot-potato orchestration: Swarm, K3s/Kubernetes, Nomad, and Ployz

Date: 2026-07-20

## Conclusion

Yes, this can be assembled with Swarm, K3s/Kubernetes, or Nomad. It is not a
capability that those orchestrators provide end-to-end, however. Each readily
handles only some of the problem:

- replacing a failed or drained container is commodity orchestration;
- rotating control-plane nodes is supported when a stable quorum or an external
  durable datastore remains underneath them;
- moving a local ZFS volume is a storage workflow, not a scheduler feature;
- retrying a build after an ambiguous failure requires durable submission,
  idempotent execution, and externally durable output regardless of scheduler.

The proposed timing is the important constraint. With one VM arriving every
three minutes and each VM living five minutes, the cluster normally has only one
or two machines. That is below the recommended three-voter control plane for
Swarm, embedded-etcd K3s, and Nomad. A conventional implementation therefore
needs a stable external control plane/storage service, more overlapping VMs, or
a deliberately serialized single-authority handoff.

Ployz's plausible advantage is the third option: make the known-death,
two-machine handoff a first-class product operation that coordinates authority,
build admission, ZFS pre-copy, single-writer cutover, workload readiness, and
serving promotion. That would be meaningfully different from composing a
scheduler, consensus cluster, CSI system, build service, registry, and custom
operator. It is an advantage only once the complete failure contract is
implemented and proven; ordinary rescheduling, rolling replacement, and job
retry are not differentiators.

## Capability matrix

| Capability | Docker Swarm | K3s / Kubernetes | Nomad | What remains custom |
| --- | --- | --- | --- | --- |
| Drain and replace a stateless workload | Native: drain launches replacement service tasks | Native controllers replace Pods; graceful node shutdown can reject new Pods and terminate existing ones | Native drain migrates service allocations | Deadline-specific readiness and traffic cutover policy |
| Survive an abrupt worker death | Native desired-state reconciliation | Native for controller-owned Pods, subject to node detection/eviction behavior | Native rescheduling when configured; service jobs default to unlimited reschedules | Application recovery and uncertain side effects |
| Rotate the control plane | Join/promote/demote managers, but Raft quorum must remain | Join/remove servers, but embedded etcd needs three or more; external DB is another stable dependency | Join/remove servers; Autopilot helps stabilize introductions and remove dead servers, but quorum must remain | A safe one-old/one-new authority-transfer protocol |
| Preserve a local ZFS volume | Not native; local volumes stay on one host; use an external volume driver or custom copy | ZFS LocalPV is node-local; CSI snapshots and other replicated CSI engines are separate systems | Not native; CSI delegates mobility to the storage provider | Snapshot/pre-copy, final incremental transfer, fencing, pin change, rollback |
| Migrate a running process | No; a replacement task is a new container | No; a replacement Pod is a new Pod | No; a replacement allocation is new execution | CRIU-style process migration, if literal live migration is required |
| Retry a build killed mid-run | Swarm is not a build workflow; BuildKit can externalize cache | A Job retries failed Pods and persists status in the Kubernetes datastore | Batch execution and reschedule policy exist, but planned drain waits for batch jobs and does not replace them | Idempotency key, duplicate-result convergence, durable source/cache/image/receipt |

## Docker Swarm

Swarm already provides the stateless half. Services are desired state; when a
task fails, Swarm creates a new task. Draining a node prevents new assignments,
stops its service tasks, and launches replacement tasks on active nodes
([service/task model](https://docs.docker.com/engine/swarm/how-swarm-mode-works/services/),
[node drain](https://docs.docker.com/engine/swarm/swarm-tutorial/drain-node/)).
Rolling-update controls and health monitoring can govern replacement rollout
([deploy services](https://docs.docker.com/engine/swarm/services/)). This is
container recreation, not process migration.

The control-plane shape conflicts with the proposed population. Swarm managers
replicate state with Raft; a majority must be available even for membership
changes. Docker recommends an odd manager count greater than two, and explicitly
notes that a two-manager swarm requires both managers for quorum. Existing tasks
continue without quorum, but no task can be started, stopped, moved, or updated
([Swarm administration](https://docs.docker.com/engine/swarm/admin_guide/)). A
one-manager cluster could promote the successor and demote the predecessor while
both are healthy, but the intervening two-manager configuration has zero failure
tolerance. It is planned replacement, not continuous availability through an
arbitrary death.

Swarm's default volume driver is local. If the same named volume is requested on
another node, Docker creates a distinct local volume there; local service
volumes do not share data across machines. Cross-host persistence requires a
volume driver or external storage such as NFS
([Docker volumes](https://docs.docker.com/engine/storage/volumes/),
[volume-plugin API](https://docs.docker.com/engine/extend/plugins_volume/)).
Swarm therefore does not move ZFS data as part of task migration.

Swarm also does not own a durable build-attempt workflow. BuildKit can export
and import cache through a registry, making a restarted build cheaper, but that
does not make an interrupted build exactly-once or preserve an image that was
only present on the dead VM
([BuildKit](https://docs.docker.com/build/buildkit/),
[registry cache](https://docs.docker.com/build/cache/backends/registry/)).

## K3s / Kubernetes

Kubernetes has the richest set of composable primitives. Controllers replace
failed Pods, and a StatefulSet replacement can reconnect to the same
PersistentVolume ([workload controllers](https://kubernetes.io/docs/concepts/workloads/controllers/)).
Kubelet graceful-node-shutdown handling rejects new Pods and terminates existing
ones. Abrupt death is less tidy: a StatefulSet Pod and its volume attachment can
remain stuck until the node is explicitly marked out of service; Kubernetes
warns that incorrect force-detach handling can corrupt data
([node shutdowns](https://kubernetes.io/docs/concepts/cluster-administration/node-shutdown/)).

The control plane still needs durable consensus state. K3s's embedded SQLite
cannot be used by multiple servers. Its embedded-etcd HA mode requires three or
more server nodes and an odd count for quorum
([K3s datastores](https://docs.k3s.io/datastore),
[embedded-etcd HA](https://docs.k3s.io/datastore/ha-embedded)). K3s can instead
run multiple replaceable server nodes over external etcd, PostgreSQL, MySQL, or
MariaDB, and can put a fixed registration address in front of them
([K3s architecture](https://docs.k3s.io/architecture)). That makes the server VMs
ephemeral, but it moves durability into the external database and load-balancer;
it does not make the whole system hot-potato.

Kubernetes PersistentVolumes are an API and scheduling abstraction, not a data
replication mechanism. Local volumes remain topology-bound and do not support
dynamic provisioning in core Kubernetes
([StorageClasses](https://kubernetes.io/docs/concepts/storage/storage-classes/)).
VolumeSnapshot standardizes requests to CSI drivers, but support and the actual
copy behavior belong to the selected driver
([VolumeSnapshots](https://kubernetes.io/docs/concepts/storage/volume-snapshots/)).

Several storage compositions can supply the missing behavior:

- OpenEBS ZFS LocalPV exposes ZFS snapshots, clones, and quotas, but its own docs
  say a local volume is available only on one node and becomes inaccessible when
  that node is unhealthy
  ([OpenEBS LocalPV](https://openebs.io/docs/3.2.x/concepts/localpv)). Backup and
  restore can recreate it on another node, but that is not a native live move
  ([ZFS LocalPV backup/restore](https://openebs.io/docs/user-guides/local-storage-user-guide/local-pv-zfs/advanced-operations/zfs-backup-restore)).
- A replicated engine such as OpenEBS Mayastor synchronously maintains multiple
  copies and can switch an I/O target to a healthy node, but HA requires more
  than one replica and enough distinct storage nodes
  ([replica operations](https://openebs.io/docs/user-guides/replicated-storage-user-guide/replicated-pv-mayastor/additional-information/replica-operations),
  [HA behavior](https://openebs.io/docs/user-guides/replicated-storage-user-guide/replicated-pv-mayastor/advanced-operations/ha)).
- Longhorn similarly rebuilds failed replicas through full, delta, or fast
  rebuilds. Rebuild completion must fit inside the churn budget
  ([Longhorn replica rebuilding](https://longhorn.io/docs/1.12.0/advanced-resources/rebuilding/)).

These systems can make the experiment work, but their storage controllers and
replicas also have to survive the same five-minute machines. With only one old
and one new data holder, copying to the successor is possible, but there is no
spare replica if either side fails during transfer.

Kubernetes Jobs are a good model for a 20-second build: a Job creates a new Pod
after Pod or node failure. The documented guarantee is deliberately not
exactly-once: even with one completion and parallelism one, the same program can
sometimes start twice; the program must handle incomplete output, locks, and
duplicate execution
([Kubernetes Jobs](https://kubernetes.io/docs/concepts/workloads/controllers/job/)).
The build input, output image, and completion identity therefore still need
durable, content-addressed storage outside the dying Pod.

## Nomad

Nomad's workload side also maps well. Drain mode prevents new allocations and
migrates service allocations; service jobs default to unlimited reschedule
attempts on failure
([node drain](https://developer.hashicorp.com/nomad/commands/node/drain),
[reschedule](https://developer.hashicorp.com/nomad/docs/job-declare/failure/reschedule)).
Rolling, canary, blue/green, health-gated, and auto-revert deployments are native
([update strategy](https://developer.hashicorp.com/nomad/docs/job-specification/update)).
Again, these create replacement allocations rather than move a running process.

Nomad servers replicate global state with Raft. HashiCorp recommends three or
five servers; two require both for quorum, and loss of quorum prevents new log
entries. A single server is explicitly discouraged because its failure entails
data loss ([Nomad consensus](https://developer.hashicorp.com/nomad/docs/architecture/cluster/consensus)).
Autopilot can stabilize newly introduced servers and remove dead ones after a
replacement arrives, but it does not remove the quorum requirement
([Autopilot](https://developer.hashicorp.com/nomad/docs/manage/autopilot)). Thus
Nomad can rotate server members continuously if a three-voter floor is
maintained, not with the normal one-or-two-node population described here.

Nomad delegates persistent storage to CSI. The scheduler understands which
nodes can access a volume and waits for the provider to claim and mount it; the
provider is responsible for making that volume available on the destination
([Nomad CSI](https://developer.hashicorp.com/nomad/docs/architecture/storage/csi)).
A local ZFS send/receive and ownership cutover would still be custom storage
logic.

For builds, one planned-drain detail matters: Nomad waits for batch allocations
until completion or the drain deadline, then does not replace them. An
unexpected failure can be handled by an explicit reschedule policy, but durable
input/output and duplicate-safe publication remain build-system concerns, not
Nomad guarantees
([node drain semantics](https://developer.hashicorp.com/nomad/commands/node/drain)).

## What is genuinely differentiated

The commodity part is substantial: node admission and drain, desired-state
replacement, health-gated rollout, routing to ready replicas, leader election,
job retry, CSI attachment, and replicated block storage all exist elsewhere.
Ployz should not claim those mechanisms alone as an advantage.

The credible differentiator is a smaller and sharper contract for this exact
failure domain:

1. The successor proves it has the latest accepted intent and all required
   authority before admission moves.
2. New build and deploy attempts receive stable idempotency keys; an ambiguous
   response is safe to retry on the successor.
3. ZFS performs an initial snapshot transfer and incremental pre-copy while the
   source serves. OpenZFS directly supports full and incremental remote streams
   ([`zfs send`](https://openzfs.github.io/openzfs-docs/man/v2.4/8/zfs-send.8.html)).
4. The final single-writer cutover briefly quiesces the application, sends the
   last incremental state, atomically changes volume ownership, starts and
   health-checks the destination, then promotes serving.
5. Every interruption leaves typed evidence that determines whether to resume,
   roll back, or retry, without treating silence as success.

That is near-live application relocation, not literal live process migration.
It could be built as a custom Kubernetes operator plus CSI/storage system, or as
automation around Nomad or Swarm. Ployz's advantage would be making it the
native unit of operation without requiring a permanently available three-node
consensus cluster or a separate durable storage control plane.

The tradeoff should be stated plainly: quorum systems preserve every committed
control-plane write across a leader death when a majority survives. A
single-authority succession design must instead prove that admission fencing
and successor acknowledgement close the lost-write window. Until that and ZFS
relocation exist, this scenario is a compelling design target rather than an
implemented moat.
