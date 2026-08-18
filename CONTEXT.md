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
The durable opaque identity of one Machine. It is distinct from its mutable Machine Name. Uniqueness is within one Cluster and its Relay Tenant, not across organizations.
_Avoid_: Machine Name, hostname, globally unique Machine ID

**Machine Name**:
A human-facing selector for a Machine that may be ambiguous. It is not an identity or a globally unique value.
_Avoid_: Machine ID, unique hostname

**Machine Target**:
The unresolved name-or-ID text used to target one Machine. It is not a wildcard and is not a unique identity.
_Avoid_: Fan-out Selector, unique hostname

**Fan-out Selector**:
A selection of every visible Machine or one Machine Target. `*` is the only wildcard spelling.
_Avoid_: Machine Target, all

**Local Machine Phase**:
A Machine's own lifecycle phase: uninitialized, joining, participating, or resetting. Joining includes catching up unless a concrete behavior requires that stage to be distinguished.
_Avoid_: Machine state, membership state, readiness

**Membership Observation**:
One Machine's potentially incomplete or stale judgment that another Machine is unknown, up, suspect, or down. It is not the observed Machine's lifecycle or an authoritative liveness fact.
_Avoid_: Machine status, cluster membership truth

**Service**:
An observer-derived grouping of Service Containers. It is not an independently persisted entity and has no canonical current specification or state.
_Avoid_: Workload, application, desired service

**Qualified Service**:
Logical Service identity: a Project Name plus a Service Name, written `project/name`. It is not a Service ID.
_Avoid_: Service Name as identity, global service name

**Service ID**:
The opaque deployment identity that survives updates. It is not the grouping key for observer-derived Services.
_Avoid_: Service Name, grouping key

**Service Name**:
The short selector that may match several Qualified Services in one Cluster view.
_Avoid_: Service ID, unique service name, Qualified Service

**Service Selector**:
The unresolved Service ID, Qualified Service (`project/name`), or Service Name used to select a Service.
_Avoid_: Service Name as identity

**Service Attempt**:
One Service Name this Deploy will apply from the target. Attempts are implicitly required until a requirement distinction exists. An empty selected list on Plan Options is full reconciliation; a non-empty list is partial. There is no independent prune flag.
_Avoid_: selected-service list as a prune flag

**Service Container**:
A managed Docker container carrying the Resolved Service Spec from its creation and the Project that owns it. It is one observed instance of a Service, not a replica identity or the canonical Service definition.
_Avoid_: Replica, service record

**Healthcheck**:
A present probe declaration on a Service Container. It is Disabled or Configured. Absence means the image's probe is inherited or that no probe is available, not a third kind of Healthcheck.
_Avoid_: health check flag, disabled boolean plus command

**Disabled Healthcheck**:
An explicit Healthcheck that turns probing off. It is not an absent Healthcheck.
_Avoid_: inherited healthcheck, missing healthcheck

**Configured Healthcheck**:
A Healthcheck with a non-empty command that probes the container.
_Avoid_: enabled healthcheck

**Hook Container**:
A managed Docker container that executes a pre-deploy hook rather than serving as an instance of the Service. It records the same owning Project as the Service's regular containers. Its identity and runtime observation remain distinct from those of Service Containers.
_Avoid_: Service Container, sidecar

**Container ID**:
The durable runtime identity of one managed Docker container. Generated container names are display values, not identities.
_Avoid_: Container name, replica identity

**Container Selector**:
The unresolved Container ID, display name, or ID prefix used to select one Container.
_Avoid_: Container name as identity, replica identity

**Container Runtime Observation**:
A point-in-time Docker lifecycle observation such as created, running with health, paused, restarting, exited, removing, dead, or an unknown external state. Container observations do not combine into an authoritative Service state.
_Avoid_: Service state, desired state

**Requested Service Spec**:
The normalized service configuration supplied to a Deploy before placement and container-specific resolution.
_Avoid_: Current service spec, desired state

**Resolved Service Spec**:
The exact service configuration attached to a Service Container when it is created. Different observed containers in one Service may legitimately carry different Resolved Service Specs.
_Avoid_: Current service spec, canonical service spec

**Project**:
An observer-derived ownership namespace. It is not a persisted resource, a workflow, or the loaded Compose input. `ployz-system` is reserved for Ployz infrastructure.
_Avoid_: ComposeProject, Compose project, deployment resource

**ComposeProject**:
The loaded Compose input for one command. It is not a Cluster-side Project.
_Avoid_: Project

**Deploy**:
A bounded command attempt that calculates and executes work against an observer-relative snapshot. It is not a persistent resource or durable workflow.
_Avoid_: Deployment resource, reconciliation loop

**Deploy Intent**:
The complete desired Services for one Deploy together with which of those Services this command applies. Empty `selected` is full reconciliation of the target, including removal of observer-visible Services the target no longer declares. Services in the target that are not applied are unchanged.
_Avoid_: leftover filtered Compose project, Cloud Attempt Target, Full/Partial/Adhoc as kinds of Deploy

**Deploy Snapshot**:
The observer-relative Machine, Service Container, and Docker Volume observations gathered for one Deploy, including target-specific Container and Docker Volume failures and omissions. Completeness is relative to the entry Machine's current visible required fan-out, not Cluster truth.
_Avoid_: current cluster state, desired state, cluster snapshot, authoritative Cluster completeness

**Prune Refusal**:
Why a full reconciliation must not remove visible drift. Observer-relative; never a claim of Cluster completeness. Absence means this Deploy may remove obsolete Services owned by the resolved user Project.
_Avoid_: prune flag, Cluster-complete snapshot

**Deploy Plan**:
The ephemeral sequence of operations calculated for one Deploy. It may complete only a prefix and is neither persisted nor generally rolled back.
_Avoid_: Desired state, workflow

**Deploy Preview**:
The observer-relative plan-plus-warnings offered for confirmation before one Deploy executes. It is Live Observation shaped for a decision, not persisted state.
_Avoid_: persisted plan, cluster decision record

**Deploy Progress**:
Live evidence of one in-flight Deploy: the current operation, the completed prefix, and health/hook waits. It is not Cluster Watch, not a workflow status, and not persisted.
_Avoid_: Watch frame, durable Deploy status, workflow state

**Deploy Outcome**:
The evidence produced by executing a Deploy Plan: its completed prefix, any failed operation, its unexecuted suffix, and any narrow replacement compensation attempted. It does not imply atomicity or general rollback.
_Avoid_: Bare deployment error, transaction result

**Docker Volume**:
A machine-local Docker storage resource and possible placement anchor. Its name is meaningful only together with its Machine.
_Avoid_: Cluster volume, replicated volume, CSI volume

**Data Loss**:
One named thing an operation will destroy, carrying the identity that makes it unique. A Data Loss list is Live Observation from one observer, not a complete Cluster view, and it is not a warning, a plan, or an operation.
_Avoid_: warning, plan, operation

**Machine Pool**:
A ZFS storage budget on one Machine, chosen when that Machine joins and not addable afterwards. Provisioned Volumes live on it. Docker's data-root, image layers, and build cache do not.
_Avoid_: Cluster pool, auto-created pool, dedicated disk, Machine ZFS Pool, ZFS-enabled cluster

**Provisioned Volume**:
A Docker Volume backed by a dataset on a Machine Pool, with a maximum size declared in Compose under `x-volumes`. A name declared only under `volumes:` is not one and is unaffected.
_Avoid_: Managed Volume, Managed ZFS Volume, cluster volume, storage class, CSI volume

**Service Volume Reference**:
A name used within one Service specification to refer to storage. It is not the Docker Volume name or a machine-independent storage identity.
_Avoid_: Docker Volume name, cluster volume ID

**Bind Mount**:
A container mount whose source is a path on its Machine. It is distinct from a Docker Volume, a Provisioned Volume, and tmpfs.
_Avoid_: Docker Volume, Provisioned Volume, cluster storage

**Tmpfs Mount**:
An ephemeral memory-backed container mount. It is distinct from a Bind Mount, Docker Volume, and Provisioned Volume.
_Avoid_: Docker Volume, Provisioned Volume, persistent volume

**Machine Subnet**:
The IPv4 /24 subnet locally selected for one Machine's containers. It is an optimistic allocation candidate and may overlap another Machine Subnet after concurrent changes.
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

**Serving Container**:
A Service Container that is healthy and has a Container Address. It is observer-derived eligibility to receive traffic, not a replica identity.
_Avoid_: replica, endpoint, upstream

**Internal DNS Answer**:
An observer-local, TTL-zero A answer derived from replicated healthy Service Container observations. It is not persisted and is not authoritative Cluster state even though the DNS response is authoritative for the `.internal` zone.
_Avoid_: Service registry record, membership-filtered endpoint set

**Caller Project**:
The Project attributed to an Internal DNS query by matching its source Container Address to exactly one visible Service Container. It is observer-relative attribution, not authenticated identity; zero or several matches mean there is no Caller Project.
_Avoid_: caller identity, authenticated client, source registry

**Ingress Hostname**:
The HTTP hostname a Service publishes through ingress: assignment from the reserved hosted DNS domain, or an explicit validated hostname. An empty string is not an assignment signal.
_Avoid_: empty hostname sentinel

**Hostname Owner**:
The Qualified Service that wins an Ingress Hostname in one observer's Container observations. Derived; not a persisted record, lease, or lock. Different observers may select different owners until their observations match.
_Avoid_: hostname lease, ownership table, global hostname uniqueness

**Certificate Material**:
The certificate and private key held in cluster state for one Ingress Hostname. It is served as given; it is not an issuance request and not a local proxy store.
_Avoid_: Caddy certificate, ACME certificate, cert secret

**Certificate Policy**:
The cluster-state values that steer certificate issuance: authority directory, external account binding, key type, renewal fraction, backoff bounds, and probe timeout. Absence means the daemon's built-in defaults. A challenge kind the daemon cannot perform is a refusal, not a default.
_Avoid_: ACME config, daemon certificate constants, CA settings

**Cluster DNS Verdict**:
Whether an Ingress Hostname's resolved addresses intersect this Cluster's Machine public addresses. It is not Caddy health, certificate readiness, or a Deploy failure.
_Avoid_: DNS health, certificate gate, reachability

**Nearest DNS Selector**:
An Internal DNS selector that orders addresses from the observing Machine's subnet before other addresses. It expresses subnet locality, not measured reachability or latency.
_Avoid_: closest Machine, available endpoint

**Machine-Service DNS Selector**:
An Internal DNS selector that names one Machine ID together with one Qualified Service or Service Name. It is not a Service identity.
_Avoid_: machine-qualified service name, replica address

**Advertised Endpoint**:
An endpoint a target Machine publishes as a way peers might reach it.
_Avoid_: Selected Endpoint, current endpoint

**Selected Endpoint**:
The endpoint one observing Machine currently selects for reaching a target Machine. Different observers may select different endpoints for the same target.
_Avoid_: Advertised Endpoint, globally current endpoint

**Cloud Relay**:
The hosted byte pipe a Machine dials out to and holds open so Cloud can reach it without an inbound route. It carries opaque streams and interprets none of them. It is not a Machine, not a mesh peer, and holds no Cluster observation.
_Avoid_: Machine Proxy, tunnel binary, control plane

**Cloud Pairing**:
The cluster-scoped grant of a Cloud Relay endpoint and Pairing Credential that makes a Cluster's Machines dial out. Absence means no Machine dials. It authenticates a Cluster to the relay and is not a per-Machine credential, not daemon-side authorization, and not proof any Machine is currently connected.
_Avoid_: per-Machine credential, login, daemon authn

**Relay Tenant**:
The Register and Dial partition identified by one Pairing Credential and its Dial Credential. Machine IDs are unique only inside one Relay Tenant. It is not a prefix on Machine ID and not an organization id on the wire.
_Avoid_: global Machine ID namespace, org-prefixed Machine ID, shared pairing

**Pairing Credential**:
The bearer Cloud Pairing grants a Machine to authenticate Register for one Relay Tenant. It is rejected on Dial.
_Avoid_: Dial Credential, per-Machine JWT, global Register secret

**Dial Credential**:
The bearer Cloud presents on Dial for the same Relay Tenant as that Cluster's Pairing Credential. It is not the Pairing Credential and does not authenticate Register.
_Avoid_: Pairing Credential, cluster credential, global Dial secret, machine-id-only Dial

**Tunnel ID**:
The Relay-issued identity of one opaque splice, carried on Open and Attach. It is not a Machine ID.
_Avoid_: session id, connection id, stream id

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
The expected condition in which one Machine Name or Service Name matches multiple durable identities. For Services, the matches are Qualified Services. Ployz preserves every match and does not choose or repair a winner in the domain model.
_Avoid_: Duplicate error, canonical winner
