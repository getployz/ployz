# Ployz

Ployz is a Rust container orchestrator with deliberately weak operational semantics and its own product identity.

## Language

**Ployz**:
The product, CLI, and daemon reconstructed in this repository.
_Avoid_: Uncloud, Ployz2

**Cluster**:
The product-level mesh as observed from one entry machine. A Cluster is not a globally authoritative entity or complete view.
_Avoid_: Cluster truth, authoritative cluster state

**Machine**:
A durable participant identity in a Cluster. Its local lifecycle and its membership as observed by another Machine are separate facts.
_Avoid_: Node, host, member

**Machine ID**:
The durable opaque identity of one Machine. It is distinct from its mutable Machine Name.
_Avoid_: Machine Name, hostname

**Machine Name**:
A human-facing selector for a Machine that may be ambiguous. It is not an identity or a globally unique value.
_Avoid_: Machine ID, unique hostname

**Local Machine Phase**:
A Machine's own lifecycle phase: uninitialized, joining, participating, or resetting. Joining includes catching up unless a concrete behavior requires that stage to be distinguished.
_Avoid_: Machine state, membership state, readiness

**Membership Observation**:
One Machine's potentially incomplete or stale judgment that another Machine is unknown, up, suspect, or down. It is not the observed Machine's lifecycle or an authoritative liveness fact.
_Avoid_: Machine status, cluster membership truth

**Service**:
An observer-derived grouping of Service Containers. It is not an independently persisted entity and has no canonical current specification or state.
_Avoid_: Workload, application, desired service

**Service ID**:
The durable opaque identity used to group Service Containers into a Service.
_Avoid_: Service Name

**Service Name**:
A human-facing selector for a Service that may resolve to several Service IDs.
_Avoid_: Service ID, unique service name

**Service Container**:
A managed Docker container carrying the Resolved Service Spec from its creation. It is one observed instance of a Service, not a replica identity or the canonical Service definition.
_Avoid_: Replica, service record

**Hook Container**:
A managed Docker container that executes a pre-deploy hook rather than serving as an instance of the Service. Its identity and runtime observation remain distinct from those of Service Containers.
_Avoid_: Service Container, sidecar

**Container ID**:
The durable runtime identity of one managed Docker container. Generated container names are display values, not identities.
_Avoid_: Container name, replica identity

**Container Runtime Observation**:
A point-in-time Docker lifecycle observation such as created, running with health, paused, restarting, exited, removing, dead, or an unknown external state. Container observations do not combine into an authoritative Service state.
_Avoid_: Service state, desired state

**Requested Service Spec**:
The normalized service configuration supplied to a Deploy before placement and container-specific resolution.
_Avoid_: Current service spec, desired state

**Resolved Service Spec**:
The exact service configuration attached to a Service Container when it is created. Different observed containers in one Service may legitimately carry different Resolved Service Specs.
_Avoid_: Current service spec, canonical service spec

**Deploy**:
A bounded command attempt that calculates and executes work against an observer-relative snapshot. It is not a persistent resource or durable workflow.
_Avoid_: Deployment resource, reconciliation loop

**Deploy Snapshot**:
The observer-relative Machine, Service Container, and Docker Volume observations gathered for one Deploy. It is Live Observation for a bounded command, not Cluster truth.
_Avoid_: current cluster state, desired state, cluster snapshot

**Deploy Plan**:
The ephemeral sequence of operations calculated for one Deploy. It may complete only a prefix and is neither persisted nor generally rolled back.
_Avoid_: Desired state, workflow

**Deploy Outcome**:
The evidence produced by executing a Deploy Plan: its completed prefix, any failed operation, its unexecuted suffix, and any narrow replacement compensation attempted. It does not imply atomicity or general rollback.
_Avoid_: Bare deployment error, transaction result

**Docker Volume**:
A machine-local Docker storage resource and possible placement anchor. Its name is meaningful only together with its Machine and is distinct from a future Managed ZFS Volume.
_Avoid_: Cluster volume, replicated volume, Managed ZFS Volume

**Service Volume Reference**:
A name used within one Service specification to refer to storage. It is not the Docker Volume name or a machine-independent storage identity.
_Avoid_: Docker Volume name, cluster volume ID

**Bind Mount**:
A container mount whose source is a path on its Machine. It is distinct from a Docker Volume and tmpfs.
_Avoid_: Docker Volume, cluster storage

**Tmpfs Mount**:
An ephemeral memory-backed container mount. It is distinct from a Bind Mount and Docker Volume.
_Avoid_: Docker Volume, persistent volume

**Machine Subnet**:
The IPv4 subnet locally selected for one Machine's containers. It is an optimistic allocation candidate and may overlap another Machine Subnet after concurrent changes.
_Avoid_: Reserved subnet, globally allocated subnet

**Management Address**:
The address used to reach a Machine's management plane over the mesh. It is distinct from container, gateway, and endpoint addresses.
_Avoid_: Container address, public endpoint

**Machine Gateway**:
The Machine-local gateway address for its container network.
_Avoid_: Management Address, ingress gateway

**Container Address**:
The address assigned to one container within its Machine Subnet. Its apparent cluster-wide uniqueness depends on optimistic Machine Subnet allocation.
_Avoid_: Management Address, globally unique container address

**Internal DNS Answer**:
An observer-local, TTL-zero A answer derived from replicated healthy Service Container observations. It is not persisted and is not authoritative Cluster state even though the DNS response is authoritative for the `.internal` zone.
_Avoid_: Service registry record, membership-filtered endpoint set

**Nearest DNS Selector**:
An Internal DNS selector that orders addresses from the observing Machine's subnet before other addresses. It expresses subnet locality, not measured reachability or latency.
_Avoid_: closest Machine, available endpoint

**Advertised Endpoint**:
An endpoint a target Machine publishes as a way peers might reach it.
_Avoid_: Selected Endpoint, current endpoint

**Selected Endpoint**:
The endpoint one observing Machine currently selects for reaching a target Machine. Different observers may select different endpoints for the same target.
_Avoid_: Advertised Endpoint, globally current endpoint

**Live Observation**:
Data obtained by directly querying a Machine at a point in time. It may still be incomplete, entry-relative, or obsolete immediately after collection.
_Avoid_: Current truth, complete state, authoritative state

**Replicated Observation**:
Data read from the local eventually convergent store. It may be stale, incomplete, or contradictory even after storage convergence.
_Avoid_: Live state, desired state, cluster truth

**Partial Result**:
A command or fan-out result containing both successful values and target-specific failures or omissions. It is an expected outcome, not an atomic transaction failure.
_Avoid_: Complete result, rollback signal

**Name Ambiguity**:
The expected condition in which one Machine or Service name matches multiple durable identities. Ployz preserves every match and does not choose or repair a winner in the domain model.
_Avoid_: Duplicate error, canonical winner
