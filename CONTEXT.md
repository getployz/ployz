# Ployz

Ployz is a small-cluster orchestration core for deploying and operating services through explicit, bounded operations. Shared runtime bootstrap terms are canonical here and mirrored in the [Ployz Cloud glossary](../ployz-cloud/CONTEXT.md) for Cloud product language.

## Language

**Namespace**:
A deploy environment containing the running and desired services that are planned together. A deploy to a namespace observes the current runtime state, compares it with the full desired state for that namespace, and computes the operations needed to remove, update, start, or leave services.
_Avoid_: Environment

**Namespace Revision**:
The internal normalized service graph for a namespace at a point in time. Ployz derives a namespace revision from deploy input so it can plan, label service containers, record evidence, and advance serving targets.
_Avoid_: User-supplied revision, service revision, active state

**Namespace Revision Entry**:
One service's normalized desired definition inside a namespace revision. A namespace revision entry can satisfy replicas only through service containers that are equivalent to that entry.
_Avoid_: Service revision, service-equivalence identity, container spec hash

**Namespace Revision Entry Identity**:
The stable identity of one namespace revision entry, derived per service so that two services never share an identity even if their container shape is otherwise identical. It is a versioned digest so a future change to which fields it covers is a deliberate, detectable version bump rather than silent drift. It covers only fields that require a new service container, currently service id and image reference; it excludes replica count, route bindings, and routed endpoint port, since a routed endpoint port change is satisfied by an endpoint reroute instead of container replacement.
_Avoid_: Container spec hash, service fingerprint, target revision

**Endpoint Reroute**:
A route-level deploy effect that changes where traffic for a service lands without recreating its containers. A routed endpoint port change commits new route state during the deploy; gateway upstream matching dials a container's observed IP on the route's endpoint port rather than requiring the container's own declared port to match, so containers stay usable by namespace revision entry identity.
_Avoid_: Container replacement, in-place update, machine-local override, per-container planning outcome

**Service Container Shape**:
The planned target-specific runtime shape for service containers that can satisfy one namespace revision entry on a compatible machine. It includes the namespace revision entry, target platform, and planned resolved image identity.
_Avoid_: Namespace revision entry, route binding, machine observation

**Serving Target**:
The current serveable service set for a namespace. It tells gateways which services and namespace revision entries are eligible to serve when combined with route bindings and runtime observations.
_Avoid_: Active service, active revision, completed phase pointer

**Phase**:
A deploy unit derived from dependencies between services in a namespace revision. A phase becomes part of the serving target only after the services in that phase pass their required gates.
_Avoid_: Manual phase, route promotion phase

**Service Dependency**:
A deploy-input relationship that requires one service to be planned before another service. Service dependencies help derive phases; they are not durable workflow state and do not carry Compose lifecycle conditions as core semantics.
_Avoid_: Workflow dependency, runtime dependency, Compose condition

**Pre-Start Hook**:
A one-off command attached to a service that runs before starting new service containers for that service when the deploy plan includes run or replace work. It must complete successfully before the deploy phase can continue; on failure, the hook container is retained as failed deploy evidence.
_Avoid_: Post-start hook, Compose completed service, job service

**Hook Container**:
A one-off service-derived container created to run a pre-start hook. Hook containers are operation evidence, not service containers that can satisfy replicas or be served by routes.
_Avoid_: Job service, replica, sidecar

**Route Binding**:
An external route bound to a service id inside a namespace. A service can have any number of route bindings, each with its own hostname and endpoint port, and several route bindings may share one endpoint port. A route binding can exist even when its service is absent from the current serving target; the binding is valid route state, while serving is a projection result. A deploy manifest may include route bindings as a convenience; future route operations will update the same route binding state independently of deploy manifests.
_Avoid_: Active route, the route (as if a service has at most one), deploy-owned route

**Route Binding Identity**:
The stable identity of one attached route binding. It changes when a route is detached and later recreated, even if the external hostname is reused.
_Avoid_: Host identity, route name, route target

**Route Projection**:
A gateway's local application of one route binding against the current serving target and runtime observations. Route projections can succeed or fail independently, and failures are reported as gateway observations. If a route binding points at a service that is not currently serveable, the route remains attached and the gateway returns an unavailable response for that route.
_Avoid_: Route binding, gateway config, active route

**Route DNS Projection**:
A DNS process's local serving of route binding hostnames to the gateway answers that can serve them. It is separate from machine-name DNS provided by a mesh such as Tailscale MagicDNS.
_Avoid_: MagicDNS route backend, machine name DNS, tailnet device DNS

**Dataplane Projection**:
A machine-local application of current machine and dataplane state into WireGuard, eBPF, routes, or related network configuration. Dataplane projection is eventually consistent and may be driven by NATS watches or bounded operation-owned NATS machine queries/commands; it is not authority to mutate cluster truth.
_Avoid_: Hidden workload reconciler, subnet allocator, machine recovery, synchronous cluster truth

**Dataplane Prepare**:
A bounded operation step that asks target machines to make dataplane projection usable for one operation attempt and report evidence. It may use NATS machine calls, but it does not make dataplane state cluster truth.
_Avoid_: Dataplane Projection Readiness, Dataplane Host Preparation, mesh bootstrap

**Dataplane Provider**:
The cluster-level data-plane mesh implementation used for dataplane projection. Deploys declare Dataplane Membership; they do not choose providers, and machines do not bring their own provider.
_Avoid_: Per-deploy mesh, per-machine provider, route backend

**Dataplane Provider Transition**:
An explicit cluster operation that changes the cluster's Dataplane Provider. It is separate from deploy and must leave evidence about provider preparation, cutover, rollback, and cleanup.
_Avoid_: Deploy side effect, silent mesh switch, mixed provider rollout

**Tailnet Integration**:
A family of optional future integrations that use a Tailscale tailnet for selected access, control-plane reachability, subnet routing, or egress without making Tailscale the cluster Dataplane Provider by default.
_Avoid_: Dataplane Provider, hidden provider migration, MagicDNS route backend

**Tailnet Access Bridge**:
An optional Tailnet Integration that lets a Tailscale tailnet reach selected Ployz gateway, admin, or machine-access surfaces while the cluster keeps its Dataplane Provider. It is access exposure, not dataplane provider choice, route binding authority, machine membership, or control-plane connectivity.
_Avoid_: Dataplane Provider, Private Control-Plane Path, Tailscale dataplane adapter, all subnets exposed, MagicDNS route backend

**Tailnet Subnet Access**:
An optional Tailnet Integration that, after explicit cluster-level enablement, advertises active Machine Endpoint Subnets to a Tailscale tailnet for operator or debugging access to machine-local endpoint networks and containers. It is raw subnet reachability and must not be treated as app ingress, route protection, control-plane authority, or silent exposure before enablement.
_Avoid_: Route Binding, Tunnel Ingress, gateway route, implicit all-subnet exposure, control-plane authority

**Ployz Native Mesh**:
The built-in dataplane provider that implements dataplane projection through Ployz-owned WireGuard, eBPF, routes, and local machine dataplane material. It is one implementation behind Dataplane Prepare; WireGuard and eBPF details are provider internals and evidence.
_Avoid_: ManagedWireGuardEbpf, WireGuard data plane, generic mesh

**Dataplane Membership**:
A machine's operation-derived participation in the cluster data-plane mesh for service endpoint reachability. It is distinct from durable machine control-plane authority, machine lifecycle, and workload placement eligibility.
_Avoid_: Machine membership, runtime membership, control-plane authority, placement eligibility, durable membership registry

**Dataplane Route Advertisement**:
An operation-derived claim or provider evidence that one or more machine endpoint subnets should be reachable through a dataplane member. In the minimum declared dataplane prepare shape, route advertisements are derived from Dataplane Membership and provider facts, not submitted as separate generic request data. They do not own endpoint subnet assignment or route bindings.
_Avoid_: Route Binding, subnet ownership, gateway route, WireGuard peer, durable route registry

**Dataplane Traffic Observation**:
A passive view of data-plane traffic movement for diagnostics, analytics, and capacity insight. It does not decide routing, access, placement, deploy success, or cluster truth.
_Avoid_: Dataplane Prepare, route authority, access authority, billing truth

**Dataplane Host Preparation**:
A bounded machine-local preparation step that makes a machine eligible for dataplane projection by preparing required host capabilities and local machine-owned dataplane material. It can run during bootstrap or an explicit substrate update; it does not create live WireGuard interfaces, routes, peers, or eBPF attachments.
_Avoid_: Dataplane projection, host prep, substrate update, runtime preparation

**Local Dataplane Material**:
Machine-owned material required for dataplane projection that remains local to the machine and can be used as reindex evidence. It is not cluster truth, operation evidence, release material, or data that should be copied into JetStream.
_Avoid_: Cluster state, operation evidence, release artifact, NATS state

**Dataplane-Capable Machine**:
A machine whose fresh machine-local facts and recent observations show dataplane projection is healthy enough for normal service workload placement and serving. It is derived capability, not a stored cluster truth flag; cleanup and repair may proceed without it when machine RPC is reachable.
_Avoid_: Local-only service placement, cleanup reachability, generic health, stored capability flag

**Serving Unpublish**:
Removing a service from a namespace's serveable surfaces before cleanup, including gateway route eligibility and DNS publication. Role-process convergence is observed as warning evidence; Docker cleanup decides whether the service is actually removed.
_Avoid_: Route removal, DNS removal, service deletion

**Route Protection**:
An explicit gateway-ingress access rule attached to a route binding that controls whether a requester may enter through that route, including public access. It changes gateway entry behavior for that route without changing the service, namespace, serving target, or internal service reachability.
_Avoid_: Protected service, private service, cloud route, auth route

**Route Protection Preset**:
A product-facing shortcut for a common route protection outcome before route protection is submitted to core. It lets callers choose public, password, or product-managed private access without configuring access-provider mechanics directly.
_Avoid_: Auth config, gateway policy, provider rule

**Route Access Session**:
A requester-specific session accepted by a gateway for the current protection on one protected route and the current access provider that granted it. It is derived from access-provider evidence and stops applying when the route protection, access provider, or route binding identity changes.
_Avoid_: Dashboard grant, Cloud session, auth cookie, user session

**Gateway Session Key**:
Secret material shared by gateway replicas in a cluster for creating and verifying route access sessions. It is local authority for gateway request admission, not route state or operation history.
_Avoid_: Route secret, auth secret, cookie secret

**Closed Public Ingress**:
A machine network posture where no public inbound service ports are required for normal operation. The machine may still require explicit outbound egress to the control plane, tailnet, tunnel provider, package sources, or operator-approved dependencies.
_Avoid_: Air gap, no network, firewall disabled, no egress

**Private Control-Plane Path**:
A future topology where machine control-plane connectivity reaches NATS through an approved private network path such as a tailnet or private mesh, instead of requiring public internet reachability to the NATS endpoint. It changes control-plane reachability, not cluster truth or data-plane provider semantics.
_Avoid_: Dataplane Provider, Tailnet Access Bridge, public NATS, hidden control channel

**Tunnel Ingress**:
An optional ingress transport where an outbound tunnel connector carries external route traffic to Ployz gateway surfaces, without opening public 80/443 on a machine. The gateway still applies route bindings and route protection; the tunnel must not bypass the gateway to target service containers directly.
_Avoid_: Route Binding, gateway replacement, direct service tunnel, public firewall rule, Tailnet Access Bridge

**Embed Access Renewal**:
An iframe-oriented protected-route presentation where the gateway asks the embedding product surface for fresh access-provider evidence. It preserves route protection while allowing dashboard embeds to recover from expired or invalid route access sessions without a manual sign-in click.
_Avoid_: Silent auth, iframe auth, dashboard bypass

**Active Certificate**:
A certificate that Ployz has obtained and can use for gateway serving. Domain-backed route bindings require an active certificate before they can be attached.
_Avoid_: Pending certificate

**Operation**:
A user-visible record of a bounded command attempt. It explains what was attempted and what the attempt reported, but future planning uses live runtime state rather than the operation record.
_Avoid_: Workflow, source of truth

**Deploy**:
An attempt to make one namespace match one namespace revision. A deploy observes live runtime state, compares it with the desired namespace revision, and applies the planned changes phase by phase.
_Avoid_: Service deploy

**Deploy Outcome**:
The terminal result of a namespace deploy attempt. It describes whether the namespace deploy fully completed, completed with warnings, partially completed through one or more phases, failed before useful namespace progress, or was cancelled.
_Avoid_: Service result, operation status

**Service Deploy Result**:
The result for one service within a namespace deploy attempt. It lets a service be completed, failed, skipped, unchanged, or removed inside a deploy whose namespace-level outcome may be different.
_Avoid_: Deploy outcome, service status, active service

**Deploy Input**:
The caller-provided input for a deploy, such as Compose YAML, a cloud-generated payload, or an SDK request. Ployz turns deploy input into an internal namespace revision before planning. Deploy input may include route bindings as a convenience so one manifest can update service containers and route state together, but routes are not owned by deploys and will be updatable independently.
_Avoid_: Desired state

**Resolved Image Identity**:
The exact image identity selected during deploy planning for a service container, such as an immutable digest. Execution uses the planned resolved image identity; machines do not resolve mutable image references after planning. Heterogeneous targets may require platform-specific resolved image identities.
_Avoid_: Latest tag, requested image, background refresh

**Cloud Deploy Payload**:
The typed deploy input submitted by Ployz Cloud or another SDK client. It is the first deploy input source for core Ployz.
_Avoid_: Compose project

**Compose Adapter**:
A future adapter that translates Docker Compose input into deploy input for one namespace. The adapter preserves familiar Compose concepts without making Compose the core domain model.
_Avoid_: Core deploy model

**Cloud**:
An external product workflow owner that submits typed commands to core Ployz and stores richer product history. Cloud is not runtime truth and does not orchestrate machine-local steps.
_Avoid_: Runtime authority

**Control-Plane Core**:
The current machine role that hosts the cluster's NATS and JetStream authority surface. The core is disposable and may be replaced by promoting another existing joined machine.
_Avoid_: Main machine, primary server, Cloud core

**Access Provider**:
An external authority trusted to identify requesters and decide whether they may access protected routes. Access providers are cluster-scoped gateway infrastructure; Ployz core treats their decisions as access evidence, not runtime truth.
_Avoid_: Cloud auth, dashboard auth, BetterAuth provider, identity provider

**Access Requirement**:
A route-protection value passed to an access provider to decide whether a requester may enter a protected route. Ployz core records and forwards it without interpreting product concepts such as organizations, projects, teams, or SSO groups.
_Avoid_: Org role, project permission, Cloud permission

**Access Grant**:
Short-lived, single-use access-provider evidence that a gateway can consume to create or refresh a route access session. It is bound to the current route protection and access provider, and is not the requester's long-lived session.
_Avoid_: Magic link, dashboard token, login token, route session

**Reindex**:
A future recovery operation that rebuilds JetStream-backed indexes after JetStream loss or reset by collecting fresh facts from machines and role processes, then adopting only unambiguous state.
_Avoid_: Automatic recovery

**State Migration**:
An explicit operation that moves persisted control-plane state from one schema to another so current runtime code can read it. It does not rewrite operation history or machine Local Authority unless that is part of a separate machine-local migration.
_Avoid_: Legacy compatibility, runtime fallback, silent upgrade

**Control-Plane Epoch**:
A monotonically increasing cluster-local generation for the current Control-Plane Core endpoint. Machines use it to reject stale endpoint updates after recovery.
_Avoid_: Recovery version, failover counter

**Core Recovery Promotion**:
A local operator action on an existing joined machine that makes that machine the Control-Plane Core after core loss. It preserves the cluster identity, increments the Control-Plane Epoch, and is authorized by local root access plus existing machine-held cluster material rather than Cloud.
_Avoid_: Cloud failover, founder failover, provisioned replacement core

**Local Authority**:
Durable state outside JetStream, owned by a machine or role process, that can be trusted during future reindex for the specific fact that component owns.
_Avoid_: Cache

**Runtime State**:
The observed condition of a namespace at planning time, including service containers, health, machine availability, volumes, gateway observations, and certificate readiness. Runtime state is an input to deploy planning; it is not desired state or operation history.
_Avoid_: Live state, JetStream truth

**Operation Runtime Snapshot**:
The bounded runtime view collected for one explicit mutating operation. It uses live machine RPC for current machine-local facts; NATS observations may provide cached evidence or context, but they must not create placement, reuse, or cleanup candidates for that operation. Operation runtime snapshots are not durable cluster truth.
_Avoid_: Background reconciliation, stored desired state, unbounded live scan, stale-observation cleanup

**Passive Runtime Projection**:
A read-side or data-plane view built from durable control-plane state and fresh NATS observations, without live machine RPC. Gateways, DNS, Cloud subscriptions, and CLI watch surfaces use passive runtime projections to stay current without owning mutations.
_Avoid_: Mutating operation snapshot, live RPC requirement, hidden reconciliation

**Managed Container Identity**:
The single record of what a managed container is and where it came from: its namespace, service, and namespace revision entry identity, plus the operation, step, and container kind that created it. It is rendered into Docker labels as recovery evidence, reported in machine observations, sent in machine run commands, and compared for cleanup fencing - one struct everywhere, so the copies cannot drift.
_Avoid_: Label set, container spec, flattened identity fields

**Container Provenance**:
The half of a managed container identity stamped by the executing operation rather than derived from deploy input: the operation id and step id that created the container. Provenance is never consumed apart from the full identity; it is a named concept, not a separate type.
_Avoid_: Audit trail, creation metadata

**Service Container**:
A Docker container that belongs to a service in a namespace. Service containers are runtime evidence for planning and inspection, but they are not the canonical service definition.
_Avoid_: Replica as container identity

**Replica**:
A desired capacity slot for a service in a namespace revision. A replica can be satisfied by a usable service container, but it is not itself a specific container.
_Avoid_: Container

**Usable Service Container**:
A service container that can satisfy a desired replica. It is running, valid for the intended placement, and equivalent to the desired service definition for that replica by namespace revision entry identity. A container passes its healthcheck once, at first creation, and only when the service defines one; reuse by a later deploy never re-runs that gate. Route endpoint port changes never affect usability: they are route state, satisfied by an endpoint reroute during the deploy, not by container replacement.
_Avoid_: Running container

**Container Replacement**:
A deploy action that creates a new service container for a desired replica and retires an existing service container that no longer satisfies that replica. A container replacement uses an update order to decide whether the new or old container moves first.
_Avoid_: In-place update

**Update Order**:
The replacement mode for a service container update. `start-first` starts and health-checks the new service container before stopping the old one; `stop-first` stops the old service container before starting the new one when overlap is unsafe.
_Avoid_: Replacement order, rollout mode

**Stop Grace Period**:
A bounded time allowed for an old service container to exit after Ployz asks it to stop. In `start-first` updates, new service containers are started and promoted before old service containers receive the stop signal; the stop grace period comes from service configuration and defaults to 10 seconds.
_Avoid_: Drain period, gateway consensus, cutover wait

**Promotion**:
Updating the serving target after a phase succeeds. Promotion makes the phase's services eligible for gateway serving, even if individual gateways report convergence later.
_Avoid_: Route cutover stage

**Gateway Convergence**:
A gateway's observed application of the current serving target and route bindings. Gateway convergence is diagnostic feedback after promotion, not a prerequisite for deploy success.
_Avoid_: Cutover confirmation

**Role Observation Window**:
A bounded warning-only period during coordination steps such as routed service promotion or serving unpublish where Ployz observes whether role processes relevant to that step report the expected applied state. It lasts for at least its configured minimum duration even if role processes converge early, and missing convergence creates warning evidence rather than operation quorum.
_Avoid_: Gateway gate, role quorum, membership wait, reconciliation wait

**Namespace Lock**:
A short-lived exclusive claim required before creating a deploy operation for a namespace. It prevents concurrent deploy mutation for the namespace and expires if the deploy worker dies.
_Avoid_: Operation owner lease, deploy queue

**Atomic Resource Claim**:
A durable claim for one resource identity created through the control-plane store's atomic create or compare-and-set behavior. Use an atomic resource claim when the contested resource has a natural key, so Ployz can avoid a broader lock.
_Avoid_: Global lock, scan-and-hope allocation, advisory claim

**Machine Substrate Lock**:
A short-lived exclusive claim required before creating a machine-local substrate mutation operation. It prevents concurrent keeper update, substrate update, bootstrap finalization, and role assignment changes for one machine.
_Avoid_: Machine lock, updater lock, host lock

**Resource Busy**:
An API rejection meaning a command cannot start because a required exclusive resource is currently locked. A resource busy rejection does not create an operation record.
_Avoid_: Failed operation

**Failed Deploy Evidence**:
Runtime material retained after a failed deploy attempt so the failure can be inspected. It can include stopped service containers, container IDs, machine IDs, logs, labels, and failure details.
_Avoid_: Garbage, orphan

**Cleanup**:
Explicit removal of runtime material that is outside the desired namespace revision. Cleanup happens after service phases in a deploy or through another named operation, not through a hidden background reconciler.
_Avoid_: Garbage collection

**Deploy Plan**:
The phase-ordered changes computed to move a namespace from runtime state to a namespace revision. A deploy plan is produced for an attempt; it is not durable desired state.
_Avoid_: Workflow definition

**Service**:
A named workload inside a namespace. A service's desired definition belongs to a namespace revision, and its runtime presence is one or more service containers.
_Avoid_: Cluster-global service

**Service Mode**:
The replica placement shape for a service. `replicated` means Ployz should run a requested number of replicas; `global` means Ployz should run one replica on each eligible machine.
_Avoid_: Scheduling type, replica mode

**Volume**:
Durable or host-backed storage that can be mounted into a service container. Volumes are part of deploy planning because they can constrain placement and update order.
_Avoid_: Disk, mount as storage identity

**Config**:
Non-secret material injected into a service container as part of a namespace revision. Changing a config changes the desired service definition that deploy planning compares against runtime state.
_Avoid_: Runtime setting, secret

**Secret**:
Sensitive material injected into a service container without becoming shared observation state or public deploy history. Secrets are distinct from configs because their values require stricter storage, redaction, and access rules.
_Avoid_: Config, env var

**Healthcheck**:
A service-defined readiness signal checked once when a service container is first created. It gates only that container's creation, and only when the service defines one; reused containers and phase continuation never wait on healthchecks.
_Avoid_: Liveness as stored truth, gateway check, recurring deploy gate

**Port**:
A declared network entry point for a service container. Ports may describe host-bound exposure or routeable service traffic, but they are not themselves route bindings.
_Avoid_: Route, endpoint

**Machine**:
An operator-visible host that can run Ployz-managed processes and service containers. Machine is the product and control-plane identity for that host; do not introduce a separate domain entity for a machine-local agent.
_Avoid_: host

**Machine Identity**:
The stable, non-reused identity of an accepted machine. Machine identity owns credentials, endpoint subnet assignment, observations, and operation history.
_Avoid_: MachineId as product identity, machine name as identity, name-keyed observations

**Machine Name**:
A current-machine-unique operator-facing label for a machine. Machine names help humans search and recognize current machines, but they are not authority for subjects, credentials, endpoint subnets, observations, or operation identity. Removed machine history does not reserve names.
_Avoid_: Hostname, machine identity, permanent name reservation

**Machine Reservation**:
A pre-activation claim created while a machine add is waiting for join. A reservation may hold a name and join material, but it does not own a machine endpoint subnet.
_Avoid_: Active machine, subnet allocation, accepted machine identity

**Machine Endpoint Subnet**:
A cluster-assigned IPv4 or IPv6 CIDR reserved for service container endpoints on one current machine. It is assigned when the machine identity is accepted into the cluster, remains stable while that machine is current, is released when the machine is removed, may be reused immediately after release, and must not be independently chosen by the machine.
_Avoid_: Runtime-derived subnet, local subnet choice, Docker network subnet as authority, first-boot local allocation, subnet cooldown

**Pending Machine Endpoint Claim**:
An operation-owned claim for a machine endpoint subnet that has been reserved but not yet attached to an accepted machine identity. A pending claim is durable transition evidence; it is resumed by the same operation or resolved by explicit cleanup or repair, not by automatic TTL.
_Avoid_: Expiring subnet lease, hidden cleanup, unowned reservation

**Accepted Machine Identity**:
A machine identity committed by the control plane with credentials and an assigned endpoint subnet. Once accepted, later machine role startup or role failures are machine health evidence; the machine's resources are changed through explicit lifecycle operations rather than failed bootstrap cleanup.
_Avoid_: Reservation, pending join, bootstrap attempt

**Accepted Machine Evidence**:
Durable machine-local material proving this host has held an Accepted Machine Identity or Machine Control-Plane Authority, such as accepted machine id state, NATS machine credentials, role authority material, or assigned substrate state. It does not include the keeper binary, generic install residue, failed bootstrap attempt state, abandoned Cloud Bootstrap Session material, or unaccepted bootstrap delivery files.
_Avoid_: Installed file presence, failed attempt evidence, abandoned session evidence, generic substrate residue

**Machine Join Redemption**:
The control-plane action that validates one machine reservation's join token and accepts the machine identity. Join redemption is the boundary where machine credentials and endpoint subnet assignment become usable; later bootstrap reporting records outcome evidence for an already accepted machine. Retrying redemption for the same accepted identity is idempotent, but redemption must not resurrect a removed machine identity.
_Avoid_: Bootstrap completion, credential pre-issue, local activation

**Machine Join Report**:
Bootstrap outcome evidence reported after machine join redemption. A join report can complete or fail the machine-add operation, but it does not accept the machine identity or assign cluster resources.
_Avoid_: Activation boundary, identity creation, subnet assignment

**Machine Add Readiness Warning**:
Warning evidence on a machine-add operation when bounded post-bootstrap observation finds degraded readiness such as dataplane projection, gateway, DNS, public IP, or first observations not yet healthy. A readiness warning does not undo the accepted machine identity; later workload eligibility is decided by the machine usability view.
_Avoid_: Bootstrap failure, identity rollback, deploy eligibility

**Machine Lifecycle**:
The durable operator-intent state of a current accepted machine identity. The minimal current lifecycle set is active and draining. Machine lifecycle controls authority and placement policy, while runtime readiness, removal kind, bootstrap failures, and unresolved cleanup come from observations and operation evidence.
_Avoid_: Runtime health, bootstrap result, every failure mode as lifecycle

**Machine Endpoint Subnet Mismatch**:
A condition where a machine's local endpoint network uses a CIDR different from the cluster-assigned Machine Endpoint Subnet, or has a local endpoint network before the cluster has assigned one. Ployz must report this as a failure that needs explicit repair; it must not adopt the local subnet automatically. A mismatched machine may report diagnostic observations, but it is not eligible for placement, passive serving, or normal deploy cleanup.
_Avoid_: Auto-adopt local Docker subnet, silent subnet repair, automatic network recreation

**Machine Endpoint Repair**:
An explicit operation that resolves a machine endpoint subnet mismatch. Endpoint repair is separate from machine startup so Ployz does not silently delete or recreate local networks that may contain runtime evidence.
_Avoid_: Startup repair, implicit network cleanup, automatic adoption

**Machine Endpoint Allocation Corruption**:
A diagnostic condition where durable machine records assign the same machine endpoint subnet to more than one machine identity, or otherwise violate endpoint subnet ownership rules. Ployz should report this through diagnostics or reindex rather than making normal startup scan the whole cluster for impossible allocation states.
_Avoid_: Startup-wide subnet audit, automatic repair

**Force Removed Machine**:
A machine identity removed without requiring the machine to participate. Force removal revokes control-plane authority, removes the machine from live dataplane membership, releases current machine state and endpoint subnet, and records any unresolved cleanup as operation evidence; workload recovery remains a separate explicit operation.
_Avoid_: Separate lifecycle state, machine tombstone, automatic reschedule, implicit recovery

**Graceful Machine Removal**:
An explicit workload-aware operation that removes a machine from service through planned unplacement, serving exclusion, and local cleanup while the machine can still participate. It is the normal removal path when the machine is reachable and deletes the current machine state after successful removal evidence is recorded.
_Avoid_: Force remove, silent drain, background reschedule

**Machine Removal Evidence**:
Operation evidence recorded when a machine is removed, including the machine identity, machine name, endpoint subnet, removal kind, affected workloads, cleanup result, credential revocation, and subnet release. Removed machine inspection comes from operation history, not current machine state.
_Avoid_: Removed machine KV tombstone, hidden audit state

**Draining Machine**:
A machine that has been explicitly excluded from new workload placement while existing workloads are moved or removed through separate operations. Draining does not by itself remove existing containers from serving; deploy or recovery operations move serving away deliberately. Draining is durable machine lifecycle state and remains in effect until it is cancelled or the machine is removed.
_Avoid_: Temporary scheduler hint, daemon-local maintenance flag, serving disable

**Drain Cancellation**:
An explicit operation that returns a draining machine to normal placement eligibility. Drain cancellation does not reverse deploys, recreate removed containers, or move workloads back to the machine.
_Avoid_: Rollback, undelete, workload restore

**Machine Cleanup Reachability**:
The ability to send machine-local cleanup commands to a machine for runtime material already on that machine. A draining machine may remain cleanup-reachable while it is fresh and authorized, even though it is not eligible for new workload placement.
_Avoid_: Placement eligibility, serving eligibility, liveness as authority

**Unresolved Machine Cleanup**:
Runtime material that may still exist on a machine because graceful cleanup has not completed or the machine stopped participating before cleanup could be verified. A machine with unresolved cleanup requires force removal rather than graceful removal.
_Avoid_: Successful removal, assumed cleanup, silent orphan adoption

**Affected Workload Acknowledgement**:
An explicit confirmation required before force removing a machine when fresh observations show Ployz-managed service containers on that machine. The acknowledgement records that the operator chose force removal instead of graceful machine removal.
_Avoid_: Implicit drain, automatic recovery confirmation

**Machine Control-Plane Authority**:
The ability of a machine identity to authenticate to the control plane, publish its own observations, respond to its own machine-scoped RPC subjects, and receive assigned work. Machine control-plane authority should become usable only with an accepted machine identity, and revocation belongs to the NATS/auth authority store rather than removed-machine state. Stale observations may remain as evidence but cannot be refreshed by a removed machine.
_Avoid_: Machine liveness, observation freshness, substrate presence, tombstone-based auth

**Operator Credential**:
A client credential minted for a human operator or automation client to call cluster control-plane services. Operator credentials are not machine identities and should be minted per operator client, preferably by the client that will hold the private seed, then authorized by the cluster using the credential's public key. Founder bootstrap should not make a shared server-minted operator seed the normal client setup path.
_Avoid_: Shared operator seed, machine join, remote operator join

**Operator Context**:
A client-local record that lets `ployzctl` connect to one cluster using NATS endpoint, trust material, and an operator credential. Operator context is client access material, not cluster truth, machine state, or proof that a machine joined the cluster.
_Avoid_: Cluster membership, machine join, shared context

**Machine Bootstrap**:
The first installation and join of Ployz substrate on a machine. It makes a machine capable of running its assigned Ployz role processes and reporting bootstrap progress.
_Avoid_: Install, provisioning, runtime bootstrap

**Bootstrapped Machine**:
A machine that already contains durable Ployz machine-local material showing it has been initialized for a cluster. It is not a fresh bootstrap target; recovery and re-adoption need explicit operation vocabulary.
_Avoid_: Fresh machine, rerunnable bootstrap target, duplicate machine

**Founder Bootstrap**:
The first-machine bootstrap that forms a new cluster control plane and then activates that same machine as the first accepted machine. Founder bootstrap exists only before there is an existing control-plane operation surface for the cluster.
_Avoid_: Machine add, joiner bootstrap, remote init

**Joiner Bootstrap**:
A machine bootstrap that uses an existing cluster's machine-add operation and join material to add another machine to that cluster.
_Avoid_: Founder bootstrap, cluster init, provisioning

**Bootstrap Delivery**:
The act of running a founder or joiner bootstrap command on a target machine, either locally on that machine or by carrying it there through SSH, copy/paste, cloud-init, or another envelope. Bootstrap delivery does not decide machine identity, acceptance, placement, or cluster truth.
_Avoid_: SSH control plane, daemon transport, provisioning authority

**Cloud Bootstrap Invite**:
A time-limited Cloud permission that can issue one or more single-redemption Cloud Bootstrap Tokens for pre-rendered Bootstrap Delivery. The invite carries Cloud-side org, cluster, actor, and bootstrap intent; a valid token redeem request is the approval boundary for each tokenized machine use.
_Avoid_: One-time bootstrap token, machine join token, org flag

**Cloud Bootstrap Session**:
A short-lived Cloud session created by `ployz-keeper bootstrap` for interactive Bootstrap Delivery. The target machine polls the session while the user opens a browser link on their workstation to choose a Cloud organization; Cloud derives founder, joiner, or wait behavior from that organization's Organization Cluster state. A session that expires before approval creates no Cloud Bootstrap Redemption. The session is not an org, cluster, machine identity, join token, or operator credential.
_Avoid_: Localhost callback, pasted cloud token, browser-owned machine session

**Cloud Bootstrap Token**:
The single-redemption bearer secret string embedded in noninteractive Bootstrap Delivery material for a Cloud Bootstrap Invite. The token is not the org, cluster, machine identity, join token, or callback credential.
_Avoid_: Org id, cluster id, join token, callback token

**Cloud Bootstrap Redemption**:
One machine's approved use of a Cloud Bootstrap Session or Cloud Bootstrap Token. For interactive bootstrap, browser approval creates the redemption by binding the session to an organization. An unapproved session and an unredeemed invite are not redemptions; the redemption is machine-local evidence that Cloud can turn into founder or joiner bootstrap material without making the session, invite, or token itself cluster truth.
_Avoid_: Bootstrap invite, bootstrap session, token, machine acceptance, operation completion

**Cloud Founder Claim**:
The Cloud-side assignment of one new-cluster redemption to Founder Bootstrap for one Organization Cluster. Competing new-cluster redemptions serialize through this claim: the first approved redemption that wins the claim becomes founder, and later redemptions wait for founder outcome. Once Cloud returns founder bootstrap material for that claim, the claim is sticky; another redemption does not automatically become founder after failure. Callback failure after local founder success keeps the same claim; rerunning keeper on the same machine resumes the same attempt rather than selecting a new founder.
_Avoid_: Leader election, automatic founder failover, first healthy server

**Waiting Cloud Bootstrap Redemption**:
A Cloud Bootstrap Redemption approved while an Organization Cluster has an active Cloud Founder Claim but no Cloud Connection. It has its own post-approval expiry separate from Cloud Bootstrap Session expiry, waits for the founder to establish a Cloud Connection, be abandoned, or for the waiting redemption to expire, and does not preissue runtime join authority, perform local machine mutation, or become founder automatically. Once expired, it is terminal and cannot later receive join material.
_Avoid_: Founder candidate, standby founder, pending machine join

**Abandon Founder Attempt**:
A Cloud-side operator action that marks a formed-but-unreachable Cloud Founder Claim terminal so the organization can start a new Founder Bootstrap through a new Cloud Bootstrap Session. It does not clean up, revoke, or mutate the already-formed local machine.
_Avoid_: Founder failover, automatic promotion, Cloud cleanup, machine removal

**Cloud Connection**:
Cloud's durable product-side relationship to an Organization Cluster after Cloud has accepted bootstrap evidence and can authenticate as an authorized NATS client. A Cloud Connection exists only after reachability succeeds; a Cloud Bootstrap Redemption may establish one, but they are separate concepts and a connection is not cluster truth, machine membership, or recovery authority.
_Avoid_: Runtime authority, machine membership, Cloud control plane, recovery authority

**Cloud Link**:
An explicit operation that authorizes Cloud as an operator client for an existing local cluster and gives Cloud the endpoint and trust material it needs to connect over direct TLS NATS. Cloud link does not bootstrap a machine and does not make Cloud runtime truth.
_Avoid_: Machine bootstrap, Cloud migration, tunnel setup

**Substrate Update**:
An explicit operation that changes already-installed non-keeper Ployz substrate on one machine to one requested Ployz version. It covers Ployz-managed role processes, supervisor units, local role configuration, and substrate artifacts, not workload service containers or keeper.
_Avoid_: Update, upgrade, rollout, in-place update

**Substrate Uninstall**:
An explicit local action that removes Ployz substrate and machine-local Ployz material from one machine. It may be forced despite Accepted Machine Evidence, but it does not remove cluster truth, delete user workloads, Docker images, Docker volumes, service containers, arbitrary networks, or runtime data by default. If no Accepted Machine Evidence and no removable Ployz substrate or material remain, it is an idempotent no-op success.
_Avoid_: Runtime wipe, machine removal, Cloud cleanup, destructive reset, force removed machine

**Keeper Update**:
An explicit operation that changes keeper on one machine to one requested Ployz version. It is separate from substrate update because keeper is the local executor for substrate steps.
_Avoid_: Self-update, keeper rollout, updater update

**Substrate Component**:
A Ployz-managed machine component that keeper can install or update. Recognized substrate components include ployzd, NATS server, gateway, DNS, and eBPF.
_Avoid_: Package, role, binary

**Activation Strategy**:
The component-specific way keeper moves a staged substrate component version into use. Activation strategies include bounded restart, graceful gateway upgrade, graceful DNS upgrade, NATS server lame-duck restart, keeper self-update handoff, and eBPF link replacement.
_Avoid_: Restart policy, rollout mode, deploy strategy

**Substrate Step**:
An idempotent machine-local check and apply action for machine bootstrap or substrate update. A substrate step reports whether local substrate is already in sync before it mutates the machine.
_Avoid_: Script step, task, command

**Substrate Preflight**:
The non-activating checks keeper runs for all relevant substrate components before a substrate update changes the machine. A failed substrate preflight stops the update before any staged version is activated.
_Avoid_: Dry run, validation step, readiness check

**Keeper Handoff**:
The keeper update stage where an old keeper stages a requested keeper version, restarts keeper, and the new keeper resumes the same operation from durable local state.
_Avoid_: Self-restart, keeper rollout, bootstrap restart

**Release Source**:
A machine-local configuration that lets keeper resolve an explicitly requested Ployz substrate version into artifact metadata. It is not authority to choose the latest version.
_Avoid_: Update channel, latest feed, package repository

**Assigned Substrate State**:
Durable machine-local state that tells keeper which substrate components and roles belong on that machine. It guides local substrate steps but is not cluster truth.
_Avoid_: Desired state, role cache, local cluster state

**Control-Plane Connection**:
The machine's NATS connection to the Ployz control plane. In v1 this is a direct TLS-authenticated NATS connection rather than an overlay tunnel.
_Avoid_: Tunnel, peer connection, transport session

**Machine Observation**:
Runtime state reported by a machine about its host and local runtime. It can describe service containers, Docker health, resources, public IP, and local process health, but it does not own gateway or DNS status.
_Avoid_: Host observation

**Fresh Machine Observation**:
A recent machine observation that can contribute to current runtime views. Stale machine observations may remain as evidence, but they are not current cluster state and must not make an offline or force removed machine appear serveable.
_Avoid_: Stored container truth, TTL as correctness

**Machine Observation Hygiene**:
Best-effort cleanup or expiry of stale observations for removed or inactive machines. Observation hygiene is not a correctness boundary; projections and operations must ignore observations that do not belong to current usable machines.
_Avoid_: Observation deletion as removal success, stale observation as authority

**Machine Usability View**:
A computed read model that explains how a machine may currently be used, including workload placement, cleanup reachability, and serving eligibility. It combines machine lifecycle, required capability, and either an operation runtime snapshot or passive runtime projection into typed eligibility outcomes with reasons, so deploy, gateway, and machine APIs do not each reimplement the rules.
_Avoid_: Scattered eligibility checks, lifecycle as readiness, raw observation filtering everywhere, eligibility booleans

**Machine Usability Reason**:
A typed explanation for why a machine is not currently usable for placement, cleanup, or serving. Initial reasons include draining, removed or not current, no operation runtime snapshot, stale observation, dataplane degraded, endpoint subnet mismatch, and placement constraint mismatch.
_Avoid_: Generic unhealthy, free-text eligibility, hidden scheduler decision

**Fresh Role Observation**:
A recent observation from a role process such as a machine agent, gateway, or DNS process. Fresh role observations make a process visible for warning-only coordination and diagnostics, but they are not durable membership or operation quorum.
_Avoid_: Membership, quorum, durable heartbeat

**Gateway Observation**:
Runtime state reported by a gateway process about the routes and serving target it has applied. Gateway observations are diagnostic feedback and do not decide deploy success.
_Avoid_: Machine observation

**Known Gateway**:
A gateway process with a fresh gateway observation. Known gateways are used for warning-only convergence observation; they are not a durable membership list or deploy success quorum.
_Avoid_: Gateway member, route owner, gateway quorum

**DNS Observation**:
Runtime state reported by a DNS process about the records or serving state it has applied. DNS observations are diagnostic feedback and do not decide deploy success.
_Avoid_: Machine observation
