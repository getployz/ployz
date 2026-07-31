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
One service's normalized desired definition inside a namespace revision. A namespace revision entry can satisfy its declared Service Mode only through service containers that are equivalent to that entry.
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
A topological layer of services whose dependencies are satisfied at the same point in a deploy. Services in a phase start only after every dependency phase's services pass their required creation gates; serveability advances per service.
_Avoid_: Manual phase, route promotion phase

**Service Dependency**:
A typed deploy-input relationship requiring one service's phase to promote before another service can start. Service dependencies derive phases and carry a Dependency Condition; they are not durable workflow state.
_Avoid_: Workflow dependency, runtime dependency, Compose condition

**Dependency Condition**:
The minimum gate a Service Dependency requires: started or healthy. A dependency's own creation gate still completes before its phase promotes, so started never bypasses a configured healthcheck; healthy requires an executable healthcheck.
_Avoid_: Compose lifecycle condition, completion dependency, readiness policy

**Pre-Start Hook**:
A one-off command attached to a service that runs before starting new service containers for that service when the deploy plan includes run or replace work. It must complete successfully before the deploy phase can continue; on failure, the hook container is retained as failed deploy evidence.
_Avoid_: Post-start hook, Compose completed service, job service

**Hook Container**:
A one-off service-derived container created to run a pre-start hook. Hook containers are operation evidence, not service containers that can satisfy a Service Mode or be served by routes.
_Avoid_: Job service, replica, sidecar

**Route Binding**:
An external hostname bound to one service id and internal endpoint port inside a namespace. Hostnames are unique across Route Bindings; gateways provide fixed HTTPS entry and HTTP redirect/validation behavior, so public listener ports are not binding input. A route binding can exist even when its service is absent from the current serving target; the binding is valid route state, while serving is a projection result. A deploy manifest may include route bindings as a convenience; future route operations will update the same route binding state independently of deploy manifests.
_Avoid_: Active route, the route (as if a service has at most one), deploy-owned route

**Route Binding Identity**:
The stable identity of one attached route binding. It changes when a route is detached and later recreated, even if the external hostname is reused.
_Avoid_: Host identity, route name, route target

**Route Binding Origin**:
Whether a Route Binding hostname was declared by an operator or generated from the Automatic Hostname Namespace. Origin is durable intent and is never inferred from the hostname text.
_Avoid_: Hostname pattern, managed hostname flag, namespace id

**Automatic Hostname Namespace**:
The single cluster-wide canonical hostname suffix from which automatic hostname requests create Route Bindings; custom input names the suffix without a wildcard label. It cannot be replaced or disabled while any generated Route Binding depends on it.
_Avoid_: Automatic domain, public URL mode, per-route namespace

**Automatic Hostname Configuration**:
The durable operator decision selecting no automatic hostnames, the Ployz Automatic Hostname Namespace, or one custom Automatic Hostname Namespace. Selection does not wait for external readiness; lease allocation, DNS observations, certificate progress, and gateway synchronization remain separate.
_Avoid_: Public URL mode, namespace readiness, DNS provider mode

**Ployz DNS Target**:
The allocated `<lease>.up.ployz.app` hostname that publishes the cluster's Ingress Endpoint Projection and provides stable CNAME indirection. It is independently enabled and may remain available when automatic hostnames are disabled; the Ployz namespace requires it, while custom-namespace dependencies are never inferred.
_Avoid_: Application URL, automatic namespace, DNS provider mode

**Automatic Hostname Label**:
The caller-supplied single lowercase DNS label combined with the Automatic Hostname Namespace to form a generated Route Binding hostname directly beneath that namespace. It is 1–63 ASCII letters, digits, or hyphens without an edge hyphen; an identical request reuses its Route Binding, while Ployz never rewrites invalid input or collisions.
_Avoid_: Service label, generated prefix, collision suffix

**Automatic Hostname Verification**:
A point-of-use check that a concrete generated hostname reaches Ployz. DNS preflight produces warning evidence, while successful exact-certificate issuance is the hard reachability proof for a custom automatic hostname.
_Avoid_: DNS ownership, namespace health, stored verification, direct-address requirement

**Automatic Route Activation**:
The shared bounded path that derives a generated hostname, rejects collisions, obtains the namespace's applicable certificate, synchronizes publishable gateways, and attaches the Route Binding. A replacement activates the new binding before atomically retiring the old one; terminal failure preserves the old route and requires an explicit retry.
_Avoid_: Hostname provisioning workflow, DNS provider operation, route reconciliation

**Route Projection**:
A gateway's local application of one route binding against the current serving target and runtime observations. Route projections can succeed or fail independently, and failures are reported as gateway observations. If a route binding points at a service that is not currently serveable, the route remains attached and the gateway returns an unavailable response for that route.
_Avoid_: Route binding, gateway config, active route

**Ingress Endpoint Projection**:
The canonical complete view of public IPv4 and IPv6 endpoints that DNS consumers may publish for cluster ingress. Its gather candidates are current accepted machines serving the gateway role, including draining machines because draining does not stop existing serving; a candidate is publishable only when its gateway process gives fresh serving testimony and fresh machine facts supply an address. Explicit intent with no candidates makes the projection unavailable and authorizes withdrawal, while silence from declared gateways does not imply an empty endpoint set.
_Avoid_: Route DNS projection, gateway membership, DNS provider state, inferred liveness

**Ingress Endpoint Projection State**:
The tagged availability of the Ingress Endpoint Projection: pending before any decisive gather, current after a non-empty fresh result, retained when total gateway silence preserves the prior current set, or unavailable when explicit intent or fresh negative testimony authorizes withdrawal.
_Avoid_: Optional address list, DNS provider status, inferred liveness

**Ingress Endpoint Projection Revision**:
Superseded by ADR 0040; see git history.

**Dataplane Projection**:
Superseded by ADR 0040; see git history.

**Dataplane Prepare**:
Superseded by ADR 0040; see git history.

**Dataplane Provider**:
Superseded by ADR 0040 (the mesh-provider seam); see git history.

**Dataplane Provider Transition**:
Superseded by ADR 0040; see git history.

**Tailnet Integration**:
A family of optional future integrations that use a Tailscale tailnet for selected access, control-plane reachability, subnet routing, or egress without making Tailscale the cluster mesh provider by default. None of them are implemented; the product contains no Tailscale code today.
_Avoid_: Mesh provider, hidden provider migration, MagicDNS route backend, assuming any of these ship

**Tailnet Access Bridge**:
An optional Tailnet Integration that lets a Tailscale tailnet reach selected Ployz gateway, admin, or machine-access surfaces while the cluster keeps its mesh provider. It is access exposure, not mesh provider choice, route binding authority, machine membership, or control-plane connectivity.
_Avoid_: Mesh provider, Tailscale dataplane adapter, all subnets exposed, MagicDNS route backend

**Tailnet Subnet Access**:
An optional Tailnet Integration that, after explicit cluster-level enablement, advertises active Machine Endpoint Subnets to a Tailscale tailnet for operator or debugging access to machine-local endpoint networks and containers. It is raw subnet reachability and must not be treated as app ingress, route protection, control-plane authority, or silent exposure before enablement.
_Avoid_: Route Binding, Tunnel Ingress, gateway route, implicit all-subnet exposure, control-plane authority

**Ployz Native Mesh**:
Superseded by ADR 0040 (builtin WireGuard mesh provider); see git history.

**Tailscale Dataplane Provider**:
Superseded by ADR 0040 (Tailscale as a mesh provider behind the mesh-provider seam); see git history.

**Dataplane Membership**:
Superseded by ADR 0040; see git history.

**Dataplane Route Advertisement**:
Superseded by ADR 0040; see git history.

**Dataplane Traffic Observation**:
A passive view of data-plane traffic movement for diagnostics, analytics, and capacity insight. It does not decide routing, access, placement, deploy success, or cluster truth.
_Avoid_: Dataplane Prepare, route authority, access authority, billing truth

**Dataplane Host Preparation**:
Superseded by ADR 0040; see git history.

**Local Dataplane Material**:
Superseded by ADR 0040; see git history.

**Dataplane-Capable Machine**:
Superseded by ADR 0040; see git history.

**Serving Unpublish**:
Removing a service from a namespace's gateway route eligibility before cleanup. Gateway convergence is observed as warning evidence; Docker cleanup decides whether the service is actually removed.
_Avoid_: Route removal, service deletion

**Route Protection**:
An explicit gateway-ingress access rule attached to a route binding that controls whether a requester may enter through that route, including public access. It changes gateway entry behavior for that route without changing the service, namespace, serving target, or internal service reachability.
_Avoid_: Protected service, private service, cloud route, auth route

**Route Protection Preset**:
A product-facing shortcut for a common route protection outcome before route protection is submitted. It lets callers choose public, password, or product-managed private access without configuring access-provider mechanics directly.
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
Superseded by ADR 0040; see git history.

**Tunnel Ingress**:
An optional ingress transport where an outbound tunnel connector carries external route traffic to Ployz gateway surfaces, without opening public 80/443 on a machine. The gateway still applies route bindings and route protection; the tunnel must not bypass the gateway to target service containers directly.
_Avoid_: Route Binding, gateway replacement, direct service tunnel, public firewall rule, Tailnet Access Bridge

**Embed Access Renewal**:
An iframe-oriented protected-route presentation where the gateway asks the embedding product surface for fresh access-provider evidence. It preserves route protection while allowing dashboard embeds to recover from expired or invalid route access sessions without a manual sign-in click.
_Avoid_: Silent auth, iframe auth, dashboard bypass

**Active Certificate**:
A valid certificate that Ployz has obtained and every currently publishable gateway can use. Domain-backed Route Bindings require one before attachment; expiry makes only the dependent Route Projection unavailable while other routes continue serving.
_Avoid_: Pending certificate

**Ployz Wildcard Certificate**:
The wildcard certificate owned by the Ployz Automatic Hostname Namespace and shared by its generated Route Bindings. Switching away retires it without disabling the Ployz DNS Target; after control-plane loss it is recovered from its issuing service.
_Avoid_: Route certificate, custom wildcard, cluster certificate

**Exact Route Certificate**:
The exact-hostname certificate owned by one custom generated Route Binding. Failed provisional material is cleaned and detachment ends its lifecycle; gateway copies keep serving through control-plane loss but never restore authority, so it is reissued rather than recovered.
_Avoid_: Custom wildcard, namespace certificate, reusable hostname certificate

**Operator-Supplied Certificate**:
Certificate material an operator provides directly rather than material Ployz obtained. It is a distinct certificate owner because renewal ownership sits outside the cluster: Ployz validates, stores, distributes, and serves it, and never renews it or treats its approaching expiry as a fault it can repair. Ployz must report the expiry it read from the material rather than assuming a renewal loop covers it. Certificate authority choice is a configuration value and is not this concept.
_Avoid_: Certificate provider, custom certificate, BYO issuer, ACME account swap, renewable certificate

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
The typed deploy input submitted by Ployz Cloud or another SDK client. It is the first deploy input source for Ployz.
_Avoid_: Compose project

**Deploy Preview**:
Superseded by ADR 0040 (no preview, no receipts); see git history.

**Pending Build Image**:
Superseded by ADR 0040; see git history.

**Build Platform Requirement**:
Superseded by ADR 0040; see git history.

**Deploy Plan**:
The authoritative, phase-ordered plan created by a Deploy from fresh runtime testimony and concrete image identities. It is limited to platforms covered by each service image.
_Avoid_: Deploy Preview, Cloud workflow plan, stored desired state

**Compose Adapter**:
A future adapter that translates Docker Compose input into deploy input for one namespace. The adapter preserves familiar Compose concepts without making Compose the core domain model.
_Avoid_: Core deploy model

**Cloud**:
An external product workflow owner that submits typed commands to Ployz and stores richer product history. Cloud is not runtime truth and does not orchestrate machine-local steps.
_Avoid_: Runtime authority

**Control**:
Superseded by ADR 0040 (v2 is coreless); see git history.

**Access Provider**:
An external authority trusted to identify requesters and decide whether they may access protected routes. Access providers are cluster-scoped gateway infrastructure; Ployz treats their decisions as access evidence, not runtime truth.
_Avoid_: Cloud auth, dashboard auth, BetterAuth provider, identity provider

**Access Requirement**:
A route-protection value passed to an access provider to decide whether a requester may enter a protected route. Ployz records and forwards it without interpreting product concepts such as organizations, projects, teams, or SSO groups.
_Avoid_: Org role, project permission, Cloud permission

**Access Grant**:
Short-lived, single-use access-provider evidence that a gateway can consume to create or refresh a route access session. It is bound to the current route protection and access provider, and is not the requester's long-lived session.
_Avoid_: Magic link, dashboard token, login token, route session

**Control Assurance**:
Superseded by ADR 0040; see git history.

**State Migration**:
An explicit operation that moves persisted control-plane state from one schema to another so current runtime code can read it. It does not rewrite operation history or machine Local Authority unless that is part of a separate machine-local migration.
_Avoid_: Legacy compatibility, runtime fallback, silent upgrade

**Control-Plane Epoch**:
Superseded by ADR 0040; see git history.

**Control Promotion**:
Superseded by ADR 0040 (repair is reseed or fresh join, never promotion); see git history.

**Reachable Machine**:
Superseded by ADR 0040; see git history.

**Local Authority**:
Durable state outside cluster intent, owned by a machine or role process, that can be trusted during recovery for the specific fact that component owns.
_Avoid_: Cache

**Runtime State**:
The observed condition of a namespace at planning time, including service containers, health, machine availability, volumes, gateway observations, and certificate readiness. Runtime state is an input to deploy planning; it is not desired state or operation history.
_Avoid_: Live state, stored truth

**Operation Runtime Snapshot**:
The bounded runtime view collected for one explicit mutating operation. It uses live machine queries for current machine-local facts; cached observations may provide evidence or context, but they must not create placement, reuse, or cleanup candidates for that operation. Operation runtime snapshots are not durable cluster truth.
_Avoid_: Background reconciliation, stored desired state, unbounded live scan, stale-observation cleanup

**Passive Runtime Projection**:
A read-side or data-plane view built from durable control-plane state and fresh observations, without live machine queries. Gateways, internal DNS, Cloud subscriptions, and CLI watch surfaces use passive runtime projections to stay current without owning mutations.
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
Superseded by ADR 0040 (the deploy op row is an optimistic claim); see git history.

**Atomic Resource Claim**:
Superseded by ADR 0040 (LWW rows admit no compare-and-set; claims are optimistic); see git history.

**Machine Substrate Lock**:
Superseded by ADR 0040; see git history.

**Resource Busy**:
An API rejection meaning a command cannot start because a required resource is held by a live optimistic claim. A resource busy rejection does not create an operation record.
_Avoid_: Failed operation

**Failed Deploy Evidence**:
Runtime material retained after a failed deploy attempt so the failure can be inspected. It can include stopped service containers, container IDs, machine IDs, logs, labels, and failure details.
_Avoid_: Garbage, orphan

**Cleanup**:
Explicit removal of runtime material that is outside the desired namespace revision. Cleanup happens after service phases in a deploy or through another named operation, not through a hidden background reconciler.
_Avoid_: Garbage collection

**Service**:
A named workload inside a namespace. A service's desired definition belongs to a namespace revision, and its runtime presence is one or more service containers.
_Avoid_: Cluster-global service

**Service Mode**:
The declared placement shape for a service. `replicated` means Ployz should run a requested number of replicas; `global` means Ployz should run one replica on each eligible machine.
_Avoid_: Scheduling type, replica mode

**Volume**:
Durable or host-backed storage that can be mounted into a service container. Volumes are part of deploy planning because they can constrain placement and update order.
_Avoid_: Disk, mount as storage identity

**Provisioned Volume**:
A Volume whose deploy declaration includes a maximum size and asks Ployz to provision its durable backing before the Volume is mounted. A Volume's plain or provisioned kind is fixed when that Volume is first created; changing the declaration does not convert existing data between kinds.
_Avoid_: Managed Volume, ZFS Volume, declared Volume, bounded Volume

**Volume Snapshot**:
A point-in-time capture of one provisioned volume's contents, taken by the machine that owns the volume. Taking one is bounded, local, and atomic, so it is a validated write that returns its result synchronously rather than an operation. A snapshot is a machine-owned fact and never cluster truth: it is listed by testimony at the point of use, is never recorded in intent, is never pruned by a background loop, and dies with the dataset it describes and with the machine that holds it. Because it is not intent, it cannot constrain placement.
_Avoid_: Backup provider, automatic snapshot schedule, convergence step, replication, silent retention policy, snapshot registry

**Snapshot Destination**:
A configured location a snapshot is copied to so the data outlives its machine. The destination is a durable operator decision and lives in intent; the snapshots sitting at it are not, and are enumerated by asking the destination at the point of use, so there is no second copy of the destination's contents to drift. The typed target is object storage, which R2, B2, MinIO, and S3 all speak, so an enum over vendors would be indirection rather than an adapter. Another machine in the same cluster is deliberately excluded until capacity accounting knows about reserved pool space, because a machine holding copies keeps bidding on room already spoken for.
_Avoid_: Provider abstraction, central backup catalog, peer replica, retention policy

**Volume Restore**:
An explicit operation that creates or replaces a provisioned volume's contents from a snapshot. Restore is the one path that turns material from outside the cluster into serving state, so it is always operator-initiated, always records what it replaced, and is never something a reconciler performs. It refuses while any service references the volume and names the command that stops them, because replacing a dataset under a running workload is corruption rather than recovery. Keeper has no part in it: convergence must not read, write, or select volume contents.
_Avoid_: Workload restore, automatic recovery, rollback, convergence step, Keeper responsibility, stop-and-restart side effect

**Volume Move**:
An explicit operation that relocates one provisioned volume and every service container that mounts it from one machine to another. Volume and containers move together because a container mounting a local dataset cannot be anywhere but the machine holding it. The copy is incremental: a full send while the service is still serving, then deltas, then a cutover proportional to the last delta rather than to the size of the volume. Everything up to and including confirmation of the final delta is free to abandon, because both copies are still identical and the source is still authoritative; the first write on the destination is what makes it irreversible. Reclaiming the source sits below that line, so a source that goes silent after cutover cannot fail the move.
_Avoid_: Live migration, rebalance side effect, drain side effect, snapshot plus restore, two authoritative copies

**Volume Removal**:
An explicit operation that destroys a volume and its data. It is the only step of the machine removal journey that acknowledges data loss rather than preventing it, which is why it carries the typed confirmation naming the volume: Volume Move relocates a volume, Machine Removal destroys nothing, and Volume Removal is what the operator reaches for when the data is genuinely being discarded. Against a Lost Machine it completes without reaching the host, removing the pin and recording typed evidence that names the assertion it acted on, because the operator has already stated the fact the cluster cannot observe.
_Avoid_: Detach, unpin, cleanup, machine-removal side effect, per-volume force flag

**Orphan Dataset**:
A provisioned dataset present on a machine that no volume pin names. Machines report their own datasets as facts, diffed against intent, so an orphan is derived exactly as a stranded volume is and in the opposite direction. It arises when a Lost Machine's pin is removed without reaching the host, when a Volume Move fails to reclaim its source, and when a rehomed host carries datasets from a previous cluster. Naming one is reporting; removing one stays an explicit operator action.
_Avoid_: Automatic reclamation, garbage collection, convergence cleanup, untracked-volume adoption

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

**Cluster**:
The set of machines sharing one cluster identity and one shared cluster config store. A machine belongs to exactly one cluster for as long as it is current; reaching another cluster is operator context, not membership.
_Avoid_: Fleet, mesh, multi-cluster machine membership, Cloud organization

**Machine**:
An operator-visible host that can run Ployz-managed processes and service containers. Machine is the product and control-plane identity for that host; do not introduce a separate domain entity for a machine-local agent. One machine belongs to exactly one cluster.
_Avoid_: host, node, server as a domain term

**Keeper**:
The machine-local agent that converges one machine's substrate toward the rows it does not own and reports into status rows nobody else may write (ADR 0040). Keeper is mandatory machine substrate rather than a selectable cluster role, is the only part of Ployz that converges continuously, and is the sole root role. Keeper owns the host and its network: processes, versions, pools, reserved capacity, and the machine's mesh interfaces, peers, and routes. It may create and may stop; it may never destroy data.
_Avoid_: Host runner, updater, reconciler for anything but its own machine, agent as a second domain entity

**Worker**:
The role process that executes workload effects on one machine: builds, images, service containers, volumes, endpoints, and logs. Worker owns the containers, where Keeper owns the host and its network; the split follows privilege, so anything needing the host network namespace or a host capability is Keeper's. Worker does the work an operation ordered; it never decides what belongs on the machine.
_Avoid_: Machine role, machine daemon, executor as cluster authority

**Machine Capability**:
An operator-selected outcome for one machine. The current capabilities are running applications, building images, and egress. Capabilities are the operator-facing primitive; there are no named capability profiles, because a profile set that needs a custom escape hatch is presentation over these same choices. Removing a capability from a machine that is still using it is rejected until the machine is drained.
_Avoid_: Profile, preset, general purpose, edge, role as a user-facing choice

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
A cluster-assigned IPv4 or IPv6 CIDR reserved for service container endpoints on one current machine. It is assigned when the machine identity is accepted into the cluster, remains stable while that machine is current, is released when the machine is removed, may be reused immediately after release, and must not be independently chosen by the machine — with the row law's one named exception (ADR 0040): after a door-allocation collision survives convergence, the lowest-ULID machine re-picks its own transport subnet.
_Avoid_: Runtime-derived subnet, local subnet choice, Docker network subnet as authority, first-boot local allocation, subnet cooldown

**Pending Machine Endpoint Claim**:
An operation-owned claim for a machine endpoint subnet that has been reserved but not yet attached to an accepted machine identity. A pending claim is durable transition evidence; it is resumed by the same operation or resolved by explicit cleanup or repair, not by automatic TTL.
_Avoid_: Expiring subnet lease, hidden cleanup, unowned reservation

**Accepted Machine Identity**:
A machine identity committed by the control plane with credentials and an assigned endpoint subnet. Once accepted, later machine role startup or role failures are machine health evidence; the machine's resources are changed through explicit lifecycle operations rather than failed bootstrap cleanup.
_Avoid_: Reservation, pending join, bootstrap attempt

**Accepted Machine Evidence**:
Durable machine-local material proving this host has held an Accepted Machine Identity or Machine Control-Plane Authority, such as accepted machine id state, machine credentials, or role authority material. It does not include the keeper binary, generic install residue, failed bootstrap attempt state, or unaccepted bootstrap delivery files.
_Avoid_: Installed file presence, failed attempt evidence, abandoned session evidence, generic substrate residue

**Machine Join Redemption**:
The control-plane action that validates one machine reservation's join token and accepts the machine identity. Join redemption is the boundary where machine credentials and endpoint subnet assignment become usable; later bootstrap reporting records outcome evidence for an already accepted machine. Retrying redemption for the same accepted identity is idempotent, but redemption must not resurrect a removed machine identity.
_Avoid_: Bootstrap completion, credential pre-issue, local activation

**Machine Join Report**:
Bootstrap outcome evidence reported after machine join redemption. A join report can complete or fail the machine-add operation, but it does not accept the machine identity or assign cluster resources.
_Avoid_: Activation boundary, identity creation, subnet assignment

**Machine Add Readiness Warning**:
Warning evidence on a machine-add operation when bounded post-bootstrap observation finds degraded readiness such as mesh, gateway, DNS, public IP, or first observations not yet healthy. A readiness warning does not undo the accepted machine identity; later workload eligibility is decided by the machine usability view.
_Avoid_: Bootstrap failure, identity rollback, deploy eligibility

**Machine Lifecycle**:
The durable operator-intent state of a current accepted machine identity. The minimal current lifecycle set is active, draining, and lost. There is no removed or retired state: Machine Removal deletes the machine's row, and its absence is the signal, since peers and routes derive from the roster. Lost is not an exception to that rule, because it is an operator assertion made before removal rather than a record kept after it. Machine lifecycle controls authority and placement policy, while runtime readiness, bootstrap failures, and unresolved cleanup come from observations and operation evidence.
_Avoid_: Runtime health, bootstrap result, removed or retired state, every failure mode as lifecycle

**Lost Machine**:
A machine an operator has explicitly declared will not return. It is an assertion about reality the cluster cannot make for itself, because a rebooting host and a destroyed one produce the same silence. It is made once against the machine rather than restated against each thing pinned to it, and it is recorded, so a machine that answers after being declared lost is a visible contradiction rather than a quiet one. It is a precondition for removal, never a result of it, and it authorizes no destruction by itself.
_Avoid_: Removed or retired state, tombstone, inferred from silence, heartbeat timeout, per-volume force flag

**Machine Endpoint Subnet Mismatch**:
A condition where a machine's local endpoint network uses a CIDR different from the cluster-assigned Machine Endpoint Subnet, or has a local endpoint network before the cluster has assigned one. Ployz must report this as a failure that needs explicit repair; it must not adopt the local subnet automatically. A mismatched machine may report diagnostic observations, but it is not eligible for placement, passive serving, or normal deploy cleanup.
_Avoid_: Auto-adopt local Docker subnet, silent subnet repair, automatic network recreation

**Machine Endpoint Repair**:
An explicit operation that resolves a machine endpoint subnet mismatch. Endpoint repair is separate from machine startup so Ployz does not silently delete or recreate local networks that may contain runtime evidence.
_Avoid_: Startup repair, implicit network cleanup, automatic adoption

**Machine Endpoint Allocation Corruption**:
A diagnostic condition where durable machine records assign the same machine endpoint subnet to more than one machine identity, or otherwise violate endpoint subnet ownership rules. Ployz should report this through diagnostics rather than making normal startup scan the whole cluster for impossible allocation states. The v2 door-allocation collision on the transport subnet is the one excepted case (ADR 0040); other ownership violations remain diagnostic findings for explicit repair.
_Avoid_: Startup-wide subnet audit, automatic repair

**Machine Removal**:
The one explicit operation that ends a machine's cluster membership. It deletes the current machine state, revokes control-plane authority, removes the machine from the mesh, and releases the endpoint subnet. Membership is a durable operator decision held by the cluster, so removal never requires the machine to participate: a reachable machine is told to release its local material as best-effort cleanup recorded in evidence, and an unreachable one is removed just as completely. There is no forced variant, because a forced and an unforced path would commit identical cluster truth.
_Avoid_: Force remove, graceful remove, removal kind, separate lifecycle state, machine tombstone, automatic reschedule, implicit recovery

**Machine Removal Precondition**:
A condition Machine Removal refuses on, each naming the command that resolves it. Running services are resolved by Rebalance; pinned volumes are resolved by Volume Move, or by Volume Removal when the data is being discarded; an undrained machine is resolved by drain. Removal itself destroys nothing, so it carries no confirmation prompt — every destructive decision has already been made by the command a precondition named. A machine that answers nothing cannot be refused on testimony grounds; its unknown workloads are recorded as typed evidence instead.
_Avoid_: Force flag, override, generic retry, confirmation on removal

**Machine Removal Evidence**:
Operation evidence recorded when a machine is removed, including the machine identity, machine name, endpoint subnet, affected workloads, cleanup result, credential revocation, and subnet release. Removed machine inspection comes from operation history, not current machine state.
_Avoid_: Removed machine KV tombstone, hidden audit state, removal kind

**Draining Machine**:
A machine that has been explicitly excluded from new workload placement while existing workloads are moved or removed through separate operations. Draining does not by itself remove existing containers from serving, and neither does a later deploy: only Rebalance moves a container that is already running. Draining is durable machine lifecycle state and remains in effect until it is cancelled or the machine is removed.
_Avoid_: Temporary scheduler hint, daemon-local maintenance flag, serving disable, evacuation

**Placement Stickiness**:
The rule that a deploy keeps every existing replica on the machine already running it and places only the shortfall. Placement eligibility is consulted for containers being created, never re-checked against containers already running, so a deploy against a draining machine leaves its containers exactly where they are. Stickiness is why moving a running container needs an operation that owns the move, and why no deploy ever relocates one as a side effect.
_Avoid_: Rescheduling on deploy, eligibility as a running-container invariant, implicit eviction

**Rebalance**:
An explicit operation that recomputes placement across every namespace, or across selected ones, and moves as few containers as reach the recomputed distribution. Every container is available for moving, which is what makes the recompute legal rather than a reason to move one already where it belongs. Balance is counted cluster-wide as placed containers per machine, never per namespace, because a per-namespace count stacks each namespace onto the same first machine and reports success. It is counted from placement alone and never from load, utilization, or capacity readings, so the same cluster state always yields the same plan and a plan stays reviewable. Rebalance is the evacuation half of drain: a draining machine is already excluded from placement candidates, so recomputing moves its services elsewhere. It does not move volumes. A service whose volume is pinned to the draining machine is refused and named to Volume Move, because relocating a local dataset costs a downtime window that is the operator's to spend. It runs behind a Confirmed Plan, the shape deploy shares, and drain does not trigger it — drain means place nothing new here, Rebalance means recompute placement.
_Avoid_: Autoscaling, scheduler policy, machine evacuation command, drain side effect, background reconciliation, volume movement, load-driven placement, capacity bid, per-namespace quota

**Confirmed Plan**:
The set of moves an operator sees and accepts before a mutating placement operation is submitted, shared by Deploy and Rebalance. Planning is bounded, local, and atomic, so it is a synchronous read that returns the plan and creates no operation: a plan costs nothing and commits nothing. The operation is submitted carrying the plan it was confirmed against and is refused if cluster state moved since, which is what makes the acceptance an agreement about specific moves rather than a decoration. An operation never pauses awaiting an answer, because control-plane work that waits on a human waits forever. Where no operator is present the plan is still computed and emitted as evidence and there is no prompt to answer, since confirmation is a courtesy to someone standing there and never an authority boundary — authority is the credential and its permissions.
_Avoid_: Blocking operation awaiting input, plan stored as intent, confirmation as authorization, skip-confirmation flag, plan replay after drift

**Rebalance Scope**:
The namespaces a Rebalance examines, defaulting to every namespace because a draining machine holds containers from all of them. Narrowing to selected namespaces narrows what may move, never what is counted: the recomputed distribution still reads placement cluster-wide, since where one namespace's containers belong depends on where every other namespace's already sit. A narrowed Rebalance cannot evacuate a draining machine, so Machine Removal names the unnarrowed command, and a narrowed run that leaves containers on a draining machine records the namespaces it did not examine as typed evidence rather than reporting the machine clear.
_Avoid_: Machine-scoped rebalance, per-namespace balance, unexamined namespaces read as empty

**Partial Rebalance**:
A Rebalance that completed some of its moves and not others. It is a failure carrying typed per-move outcomes, because the operator asked for a distribution that does not exist yet, and never a success qualified by detail. Failure here does not mean nothing happened or that anything needs unwinding: every landed move is correct and worth keeping, and moving as few containers as reach the target is what makes the intermediate state a waypoint rather than a mess. A move that half-lands leaves a duplicate rather than a gap, since the destination container is started and gated before the source is removed. The repair is another Rebalance, which converges from observed reality; a deploy is not the repair, because stickiness keeps unmoved containers where they are and prunes the duplicate by an ordering unrelated to what the move intended.
_Avoid_: Rollback, compensating move, partial success, deploy as repair, resume from held state

**Drain Cancellation**:
An explicit operation that returns a draining machine to normal placement eligibility; the canonical operation and command name is resume (`machine resume`). Drain cancellation does not reverse deploys, recreate removed containers, or move workloads back to the machine.
_Avoid_: Rollback, undelete, workload restore, activate (reserved for first-machine activation)

**Machine Cleanup Reachability**:
The ability to send machine-local cleanup commands to a machine for runtime material already on that machine. A draining machine may remain cleanup-reachable while it is fresh and authorized, even though it is not eligible for new workload placement.
_Avoid_: Placement eligibility, serving eligibility, liveness as authority

**Unresolved Machine Cleanup**:
Runtime material that may still exist on a machine because cleanup has not completed or the machine stopped participating before cleanup could be verified.
_Avoid_: Successful removal, assumed cleanup, silent orphan adoption

**Affected Workload Acknowledgement**:
Superseded by ADR 0040 (Machine Removal has no forced variant); see git history.

**Machine Control-Plane Authority**:
The ability of a machine identity to authenticate to the control plane, publish its own observations, answer its own machine-scoped requests, and receive assigned work. Machine control-plane authority should become usable only with an accepted machine identity, and revocation belongs to the membership and auth authority rather than removed-machine state. Stale observations may remain as evidence but cannot be refreshed by a removed machine.
_Avoid_: Machine liveness, observation freshness, substrate presence, tombstone-based auth

**Operator Credential**:
A client credential minted for a human operator or automation client to call cluster control-plane services. Operator credentials are not machine identities and should be minted per operator client, preferably by the client that will hold the private seed, then authorized by the cluster using the credential's public key. Bootstrap should not make a shared server-minted operator seed the normal client setup path.
_Avoid_: Shared operator seed, machine join, remote operator join

**Operator Context**:
A client-local record that lets `ployz` connect to one cluster using a cluster endpoint, trust material, and an operator credential. Operator context is client access material, not cluster truth, machine state, or proof that a machine joined the cluster.
_Avoid_: Cluster membership, machine join, shared context

**Machine Bootstrap**:
The first installation and join of Ployz substrate on a machine. It makes a machine capable of running its Ployz role processes and reporting bootstrap progress.
_Avoid_: Install, provisioning, runtime bootstrap

**Bootstrapped Machine**:
A machine that already contains durable Ployz machine-local material showing it has been initialized for a cluster. It is not a fresh bootstrap target; recovery and re-adoption need explicit operation vocabulary.
_Avoid_: Fresh machine, rerunnable bootstrap target, duplicate machine

**Founder Bootstrap**:
Superseded by ADR 0040 (the founder/joiner/cloud bootstrap split is dead); see git history.

**Joiner Bootstrap**:
Superseded by ADR 0040; see git history.

**Bootstrap Delivery**:
The act of running a bootstrap command on a target machine, either locally on that machine or by carrying it there through SSH, copy/paste, cloud-init, or another envelope. Bootstrap delivery does not decide machine identity, acceptance, placement, or cluster truth.
_Avoid_: SSH control plane, daemon transport, provisioning authority

**Cloud Bootstrap Invite**:
Superseded by ADR 0040 (Cloud mints join tokens as an ordinary mesh peer); see git history.

**Cloud Bootstrap Session**:
Superseded by ADR 0040; see git history.

**Cloud Bootstrap Token**:
Superseded by ADR 0040; see git history.

**Cloud Bootstrap Redemption**:
Superseded by ADR 0040; see git history.

**Cloud Founder Claim**:
Superseded by ADR 0040; see git history.

**Waiting Cloud Bootstrap Redemption**:
Superseded by ADR 0040; see git history.

**Abandon Founder Attempt**:
Superseded by ADR 0040; see git history.

**Cloud Connection**:
Cloud's durable product-side relationship to an Organization Cluster after Cloud has accepted bootstrap evidence and can authenticate as an authorized client. A Cloud Connection exists only after reachability succeeds; a connection is not cluster truth, machine membership, or recovery authority.
_Avoid_: Runtime authority, machine membership, Cloud control plane, recovery authority

**Cloud Link**:
An explicit operation that authorizes Cloud as an operator client for an existing local cluster and gives Cloud the endpoint and trust material it needs to connect. Cloud link does not bootstrap a machine and does not make Cloud runtime truth.
_Avoid_: Machine bootstrap, Cloud migration, tunnel setup

**Substrate Update**:
The change of one machine's substrate component versions, driven by a caller-paced upgrade command with keeper-first swap (ADR 0040). It covers Ployz-managed role processes, supervisor units, local role configuration, and substrate artifacts, including keeper itself, and never workload service containers. There is no cluster-level version record: "the cluster is on this version" is a report computed from machines rather than a stored decision anyone made.
_Avoid_: Update, upgrade, rollout, in-place update, update operation, cluster version setting, update channel

**Substrate Uninstall**:
An explicit local action that removes Ployz substrate and machine-local Ployz material from one machine. It may be forced despite Accepted Machine Evidence, but it does not remove cluster truth, delete user workloads, Docker images, Docker volumes, service containers, arbitrary networks, or runtime data by default. If no Accepted Machine Evidence and no removable Ployz substrate or material remain, it is an idempotent no-op success. Its force flag is local only and is not a removal variant: a Machine Removal that reached the machine clears the Accepted Machine Evidence that would otherwise require it, so forcing means the cluster moved on without the machine hearing.
_Avoid_: Runtime wipe, machine removal, Cloud cleanup, destructive reset

**Substrate Component**:
A Ployz-managed machine component that keeper can install or update. Recognized substrate components are the `ployzd` binary, which supplies every role including keeper itself, plus foreign components such as the pinned Corrosion sidecar and the eBPF program. Gateway and DNS are `ployzd` roles, not separately versioned components. Keeper's own version is part of what an upgrade names, so there is no separate keeper update operation.
_Avoid_: Package, role, binary, per-role versioning, keeper update as its own operation

**Activation Strategy**:
The component-specific way keeper moves a staged substrate component version into use, such as bounded restart, graceful gateway or DNS upgrade, keeper self-update handoff, or eBPF link replacement. Activation is a switch mechanic, not a health gate: success is the strategy's own completion, and a machine that is mid-activation is already excluded from placement by ordinary testimony, so no probe, threshold, or readiness contract belongs here. Failed activation stops that component, records typed per-component status, and leaves the staged version staged; only Keeper Handoff reverts, because losing remote management of a host is the one failure no primitive can recover from.
_Avoid_: Restart policy, rollout mode, deploy strategy, readiness probe, health threshold, activation rollback

**Substrate Step**:
An idempotent machine-local check and apply action for machine bootstrap or substrate update. A substrate step reports whether local substrate is already in sync before it mutates the machine.
_Avoid_: Script step, task, command

**Substrate Preflight**:
The non-activating checks keeper runs for all relevant substrate components before a substrate update changes the machine. A failed substrate preflight stops the update before any staged version is activated.
_Avoid_: Dry run, validation step, readiness check

**Keeper Handoff**:
The convergence stage where an old keeper stages the Ployz version the upgrade names, restarts keeper first, and the new keeper resumes from durable local state before applying the remaining components. A staged keeper that fails to come up reverts to the previous binary.
_Avoid_: Self-restart, keeper rollout, bootstrap restart, keeper update as its own operation

**Release Source**:
Superseded by ADR 0040 (the upgrade command carries `{version, sha256, url}`); see git history.

**Unsupported Endpoint Answer**:
A control-plane responder's typed reply that it does not implement the endpoint it was asked for. Control-plane contracts are additive-only within a major version, so a cluster running mixed versions is legal for as long as an operator leaves it that way and a responder declines instead of staying quiet. It exists to protect what silence already means: the machines an intent-driven gather expected and did not hear from.
_Avoid_: Version negotiation, capability handshake, protocol version check, version skew window

**Machine Assignment**:
Superseded by ADR 0040 (no assignments, components, or profiles compiled by Control); see git history.

**Pooled Storage**:
A machine-local ZFS pool that Ployz carves persistent volumes from as datasets. Pooled storage is a typed host feature: Keeper converges it as host substrate while Worker keeps ownership of the datasets themselves. Keeper imports an existing Ployz-owned pool and creates one only on a disk it can prove is empty; anything else is a typed failure for the operator to resolve. Wiping a reused disk is a separate explicit destructive action and is never part of declared intent, because intent is re-applied on every convergence.
_Avoid_: Wipe flag, automatic disk adoption, storage prepare as its own operation, pool as workload data

**Assigned Substrate State**:
Superseded by ADR 0040; see git history.

**Control-Plane Connection**:
Superseded by ADR 0040 (HTTP/JSON/SSE over the mesh); see git history.

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
The one rule set for whether a machine may take new workload placement. It combines durable operator intent such as Machine Lifecycle with fresh, point-of-use testimony required by that placement attempt; staleness is bounded by the mesh provider's reported last-verified age (ADR 0040). The set is validated once: excluded candidates do not trigger recursive shrinking or revalidation. Silent and locally unusable declared machines remain in intent but are not expected peers for that attempt. The view is never stored as cluster truth and never changes existing workloads or serving state.
_Avoid_: Scattered eligibility checks, lifecycle as readiness, stored liveness, observation-age eligibility, eligibility booleans

**Machine Usability Reason**:
A typed explanation for why a machine is excluded from one new-placement attempt. Reasons include durable policy such as draining and fresh evidence such as no answer, unusable local mesh substrate, or a missing or stale mesh handshake. These reasons are attempt evidence, not durable machine state; exclusion does not evict workloads or rewrite serving truth.
_Avoid_: Generic unhealthy, free-text eligibility, hidden scheduler decision, stored health flag

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
Runtime state reported by a DNS process about its internal resolver. DNS observations are diagnostic feedback and do not decide deploy success.
_Avoid_: Machine observation
