# Ployz

Ployz is a small-cluster orchestration core for deploying and operating services through explicit, bounded operations.

## Language

**Namespace**:
A deploy environment containing the running and desired services that are planned together. A deploy to a namespace observes the current runtime state, compares it with the full desired state for that namespace, and computes the operations needed to remove, update, start, or leave services.
_Avoid_: Environment

**Namespace Revision**:
The internal normalized service graph for a namespace at a point in time. Ployz derives a namespace revision from deploy input so it can plan, label service containers, record evidence, and advance serving targets.
_Avoid_: User-supplied revision, service revision, active state

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
An external route attached to a service inside a namespace. A route binding can exist only after its required certificate is active and any route protection can be enforced; once attached, it becomes serveable when its service is included in the current serving target and has healthy matching containers.
_Avoid_: Active route

**Route Binding Identity**:
The stable identity of one attached route binding. It changes when a route is detached and later recreated, even if the external hostname is reused.
_Avoid_: Host identity, route name, route target

**Route Projection**:
A gateway's local application of one route binding against the current serving target and runtime observations. Route projections can succeed or fail independently, and failures are reported as gateway observations.
_Avoid_: Route binding, gateway config, active route

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
The caller-provided input for a deploy, such as Compose YAML, a cloud-generated payload, or an SDK request. Ployz turns deploy input into an internal namespace revision before planning.
_Avoid_: Desired state

**Cloud Deploy Payload**:
The typed deploy input submitted by Ployz Cloud or another SDK client. It is the first deploy input source for core Ployz.
_Avoid_: Compose project

**Compose Adapter**:
A future adapter that translates Docker Compose input into deploy input for one namespace. The adapter preserves familiar Compose concepts without making Compose the core domain model.
_Avoid_: Core deploy model

**Cloud**:
An external product workflow owner that submits typed commands to core Ployz and stores richer product history. Cloud is not runtime truth and does not orchestrate machine-local steps.
_Avoid_: Runtime authority

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

**Local Authority**:
Durable state outside JetStream, owned by a machine or role process, that can be trusted during future reindex for the specific fact that component owns.
_Avoid_: Cache

**Runtime State**:
The observed condition of a namespace at planning time, including service containers, health, machine availability, volumes, gateway observations, and certificate readiness. Runtime state is an input to deploy planning; it is not desired state or operation history.
_Avoid_: Live state, JetStream truth

**Service Container**:
A Docker container that belongs to a service in a namespace. Service containers are runtime evidence for planning and inspection, but they are not the canonical service definition.
_Avoid_: Replica as container identity

**Replica**:
A desired capacity slot for a service in a namespace revision. A replica can be satisfied by a usable service container, but it is not itself a specific container.
_Avoid_: Container

**Usable Service Container**:
A service container that can satisfy a desired replica. It is running, healthy, valid for the intended placement, and equivalent to the desired service definition for that replica.
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
Non-secret file or inline content mounted into a service container as part of a namespace revision. Changing a config changes the desired service definition that deploy planning compares against runtime state.
_Avoid_: Runtime setting, secret

**Secret**:
Sensitive material provided to a service container without becoming shared observation state or public deploy history. Secrets are distinct from configs because their values require stricter storage, redaction, and access rules.
_Avoid_: Config, env var

**Healthcheck**:
A service-defined readiness signal used to decide whether a service container can satisfy a replica and whether a deploy phase may progress.
_Avoid_: Liveness as stored truth, gateway check

**Port**:
A declared network entry point for a service container. Ports may describe host-bound exposure or routeable service traffic, but they are not themselves route bindings.
_Avoid_: Route, endpoint

**Machine**:
An operator-visible host that can run Ployz-managed processes and service containers. Machine is the product and control-plane identity for that host; do not introduce a separate domain entity for node.
_Avoid_: Node, host

**Machine Observation**:
Runtime state reported by a machine about its host and local runtime. It can describe service containers, Docker health, resources, public IP, and local process health, but it does not own gateway or DNS status.
_Avoid_: Node observation

**Fresh Role Observation**:
A recent observation from a role process such as a machine agent, gateway, DNS process, or tunnel process. Fresh role observations make a process visible for warning-only coordination and diagnostics, but they are not durable membership or operation quorum.
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
