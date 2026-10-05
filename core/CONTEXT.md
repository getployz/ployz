# Ployz

Ployz is a cluster of Docker machines and a Config Store that holds what you deploy to it. The CLI is `ployz`. The daemon is `ployzd`. Operational semantics stay deliberately weak: a Cluster is what one entry Machine observes, not a globally authoritative entity.

Architectural bets and their red flags live in [DESIGN.md](DESIGN.md).

## Language

**Ployz**:
The product, CLI, and daemon in this repository.
_Avoid_: Ployz2

**Cluster**:
The product-level mesh as observed from one entry machine. A Cluster is not a globally authoritative entity or complete view.
_Avoid_: Cluster truth, authoritative cluster state

**Cluster Observation**:
One Entry Machine's incomplete, potentially stale view of a Cluster.
_Avoid_: Cluster truth, complete view

**Entry Machine**:
The Machine through which a client observes a Cluster and routes commands. It represents only what it can currently observe.
_Avoid_: Controller, leader, source of Cluster truth

**Machine**:
A durable participant identity in a Cluster. Its local lifecycle and its membership as observed by another Machine are separate facts.
_Avoid_: Node, host, member

**Server**:
The user-facing name for a Machine, used alike by the CLI and Cloud. Code and this glossary say Machine.
_Avoid_: Machine in user-facing copy, node, host

**Machine Label**:
An operator-assigned key/value classification of a Machine used to select placement candidates. It is metadata, not evidence of runtime capability or permission to accept work.
_Avoid_: Machine tag, Machine Role

**Machine Role**:
One of a Machine's independently enabled permissions to accept Builds, application Services, or the Ingress Proxy. These roles may be combined; an explicit placement target does not override their permissions.
_Avoid_: Swarm manager/worker role, runtime capability, Machine Label

**Machine ID**:
The durable opaque identity of one Machine. It is distinct from its mutable Machine Name. Uniqueness is within one Cluster, not across organizations.
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
Logical Service identity: a Namespace plus a Service Name, written `namespace/name`. It is not a Service ID.
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

**Service Template**:
The preset, by ID and version, an authored Service was created from, such as PostgreSQL version 1. It is authoring metadata: it changes nothing that runs, and the Service's own Settings stay authoritative. A template's Volume is found through the Service's mounts.
_Avoid_: preset as a stored entity, template group

**Service Container**:
A managed Docker container carrying the Resolved Service Spec from its creation and the Namespace that owns it. It is one observed instance of a Service, not a replica identity or the canonical Service definition.
_Avoid_: Replica, service record

**Healthcheck**:
A present probe declaration on a Service Container. It is Disabled, Configured or HTTP. Absence means the image's probe is inherited or that no probe is available, not another kind of Healthcheck. A Cloud Service authors one of three kinds: `none`, an `http` path or a shell `command`. `none` lowers to Disabled, so Cloud never runs an image's own probe.
_Avoid_: health check flag, disabled boolean plus command

**Disabled Healthcheck**:
An explicit Healthcheck that turns probing off. It is not an absent Healthcheck.
_Avoid_: inherited healthcheck, missing healthcheck

**Configured Healthcheck**:
A Healthcheck with a non-empty command that probes the container.
_Avoid_: enabled healthcheck

**Hook Container**:
A managed Docker container that executes a pre-deploy hook rather than serving as an instance of the Service. It records the same owning Namespace as the Service's regular containers. Its identity and runtime observation remain distinct from those of Service Containers.
_Avoid_: Service Container, sidecar

**Container ID**:
The durable runtime identity of one managed Docker container. Generated container names are display values, not identities.
_Avoid_: Container name, replica identity

**Container Selector**:
The unresolved Container ID, display name, or ID prefix used to select one Container.
_Avoid_: Container name as identity, replica identity

**Container Runtime Observation**:
A point-in-time Docker lifecycle observation such as created, running with health, paused, restarting, exited, removing, dead, or an unrecognized external state carried verbatim as its observed value. Container observations do not combine into an authoritative Service state.
Running health `stopping` is the daemon's own overlay, not Docker's: a client marked the Container ahead of a stop so the Ingress Proxies drop it first. The mark lives in daemon memory and ends when the stop runs.
_Avoid_: Service state, desired state

**Requested Service Spec**:
The normalized service configuration supplied to a Deploy before placement and container-specific resolution.
_Avoid_: Current service spec, desired state

**Resolved Service Spec**:
The exact service configuration attached to a Service Container when it is created. Different observed containers in one Service may legitimately carry different Resolved Service Specs.
_Avoid_: Current service spec, canonical service spec

**Namespace**:
The observer-derived ownership group of the Containers one Environment deploys, named in their labels and Internal DNS. It is not a persisted resource or a workflow. `ployz-system` is reserved for Ployz infrastructure.
_Avoid_: Project (the product grouping), Compose project, deployment resource

**Direct Image Transfer**:
A bounded operation that makes an image held by one Machine, such as a Build result, available on selected Machines without requiring an external registry. It preserves layer-aware transfer.
_Avoid_: Unregistry, image ingest as a product term

**Image Cleanup**:
Removing superseded images that a Deploy delivered to Machines, or that a Build Grant pushed into one. It runs after the Deploy Outcome, keeps recent unused images for rollback, never touches an image a Container uses, and never changes the Outcome.
_Avoid_: Image prune, garbage collection

**Build**:
The work to produce one container image from source and a build recipe. A Deploy with three Git-sourced Services has three Builds.
_Avoid_: Deploy, whole-project build as one Build

**Build Method**:
How a Service's image is described for building: a Dockerfile or Railpack.
_Avoid_: Builder, builder type

**Build Concurrency**:
How many Build Attempts one Machine runs at once, recorded on its Machine record. Unset means automatic: 1 when the Machine accepts Services, otherwise one per 4 GB of RAM, clamped to 1–4. Each running Build Attempt holds one build slot and never the Machine's mutation lock.
_Avoid_: build parallelism, worker count

**Build Receipt**:
Evidence associating captured build inputs with a completed image and its verified platforms. It does not establish that the image remains available or that a Deploy succeeded.
_Avoid_: Applied State, cached deployment

**Uploaded Source**:
A Service's build input uploaded from a local directory for one Deploy, identified by a hash of its files and never by a commit.
_Avoid_: dirty commit, local commit, HEAD as its identity

**Build Attempt**:
One execution of a Build, which may succeed, fail, or stop before producing an image.
_Avoid_: Build as an execution identity, Deploy Attempt

**Build Grant**:
A Machine-minted permission to push one Build's image into that Machine and nothing else. Its one push is recorded when that image's digest-named tag lands, or when the Machine confirms it already holds exactly that tag at that digest. It ends when that Build finishes or is cancelled, and it grants no Machine RPC access.
_Avoid_: CI credential, push token, Management Capability

**Deploy**:
A bounded command attempt that calculates and executes work against an observer-relative snapshot. It is not a persistent resource or durable workflow; its record is a Deployment.
_Avoid_: Deployment (the record), reconciliation loop

**Global catch-up**:
A bounded membership-command operation that establishes this Machine's running Service Container for every observed eligible Global before the command completes. Unknown eligibility or incomplete placement is a reported outcome; it is not a Cluster-wide Deploy and has no background retry.
_Avoid_: scheduler, Cluster-wide Deploy

**Global slot convergence**:
A bounded client attempt for a dispatched Global slot: establish it when eligible, retire it when definitely ineligible, or hold it unchanged when eligibility is unknown. Its observations and lifecycle actions are separate operations, not one atomic decision.
_Avoid_: Background maintenance, Cluster-wide reconciler, scheduler

**Drain**:
A bounded command that turns a Machine's services role off and moves its Service Containers onto other eligible Machines. Services it cannot move, such as those mounting a Volume or Bind Mount on that Machine, keep running and are reported; it neither withdraws ingress nor removes the Machine.
_Avoid_: Eviction, replica relocation, rebalance

**Placement convergence**:
A bounded client attempt for one replicated Service: replace each of its Service Containers on a definitely ineligible Machine with one started from the shared observed Resolved Service Spec on an eligible Machine, healthy before the old one is removed, holding unchanged when eligibility is unknown. It keeps the Container count, runs no hooks, and is not a Deploy.
_Avoid_: Rebalance, reschedule, replica relocation

**Observed Global Slot Spec**:
The Resolved Service Spec carried by the newest observer-visible Service Container and used for Global catch-up or slot convergence. It retains that Container's provenance and is not canonical Service intent.
_Avoid_: Current service spec, canonical service spec, desired Global state

**Deploy Intent**:
The complete desired Services for one Deploy together with which of those Services this command applies. Empty `selected` is full reconciliation of the target, including removal of observer-visible Services the target no longer declares. Services in the target that are not applied are unchanged.
It is derived from an Environment's Saved State when a Deploy is admitted, never authored directly. Compose files are not an input.
_Avoid_: leftover filtered Compose project, Compose as an authoring format, Cloud Attempt Target, Full/Partial/Adhoc as kinds of Deploy

**Config Store**:
The home of authored configuration and its history: Projects, their Environments, Working and Saved State, and deploy history. Every client changes authored configuration only through it. Cloud hosts one per Organization; the Cluster never holds it.
_Avoid_: Cloud database, config file, Org Store (the dashboard's browser cache), control plane

**Project**:
A named group of Environments in one Config Store.
_Avoid_: Namespace, app, stack

**Environment**:
One Project's authored configuration of Services and Volumes, with its own Working and Saved State, deployed to its own Namespace.
_Avoid_: Namespace, stage, workspace

**Default Environment**:
The Environment a project opens, chosen in the project's settings. There is no per-user remembered Environment.
_Avoid_: Remembered environment, primary environment, main environment

**Directory Link**:
A directory's recorded Project and Environment, kept on this device beside the CLI config and inherited by its subdirectories. `--project`/`--env` and `PLOYZ_PROJECT`/`PLOYZ_ENV` take precedence over it, in that order; a link made in another Organization is refused.
_Avoid_: Project file, workspace, current project

**Setting**:
One named, user-facing value of an Environment Node, Environment, Project or Organization, addressed by a path such as `web.replicas`. It is staged until a Deploy or applies immediately; the settings catalog lists every Setting with its type and default, and the stored configuration behind it is never addressed directly.
_Avoid_: Field, option, config key, storage path

**Environment Node**:
A deployable element of an Environment whose desired configuration participates in reviewed snapshots. Services and Environment Resources are Environment Nodes, while retaining distinct storage and lifecycle behavior.
_Avoid_: Canvas node when referring to deployment identity

**Environment Resource**:
A non-Service Environment Node with stable identity and type-owned configuration and lifecycle behavior. Volumes are the current Environment Resource type; effects on a Service's container template remain Service-owned.
_Avoid_: Generic canvas item, Service subtype

**Service Metadata**:
A Service's name. Renaming it is a staged change to its slug in Working State, shipped by the next Deploy like any other; Private DNS stays as it was, so other Services keep reaching it. Private DNS changes only when set itself, also staged, and no two Services share a name or Private DNS name. References target IDs; managed `PLOYZ_SERVICE_NAME` exports the stable Private DNS name.
_Avoid_: Deployable name, DNS alias

**Typed Address**:
Another Service's private address written into a variable's text instead of referenced: `NAME.internal` anywhere, or a bare `NAME` where only a host can stand, a URL's host or the whole value of a host key such as `DB_HOST`. It reaches the same place in this Environment, but nothing links the two Services, so a Branch that doesn't copy the other can't reach it and a Deploy doesn't order them. An edit that writes one keeps it as typed and answers with the reference to set instead; nothing refuses or rewrites it.
_Avoid_: Hardcoded host, literal link

**Volume Kind**:
The explicit, user-chosen kind of an Environment's Volume: a Provisioned Volume (sized, quota-enforced, hosted only on a Server with a managed pool) or a plain Docker Volume (unsized, any Server). Both are machine-local. Creation defaults to a Provisioned Volume; plain Docker storage requires an explicit opt-out. The kind and maximum can change in Working State until the first Deployment targeting that Volume is admitted. Admission fixes both, including failed, cancelled and pending attempts, because frozen attempts can prepare storage or be retried. Saved documents always carry the explicit kind and never infer it from a missing size.
_Avoid_: Storage class, volume type dropdown

**Shared Writes**:
A Volume's opt-in to more than one writer: several replicas of one Service, or several Services mounting it. Off by default; while off, the Config Store refuses a change that adds a second writer or turns it off over several. Working State that already had several writers keeps them and still deploys. It changes nothing that runs, so it applies at once and is no diff row; discarding the whole Volume or Environment returns it to the deployed value.
_Avoid_: Multi-attach, ReadWriteMany, shared volume

**Registry Credential**:
Current encrypted authentication material owned by a Service identity, with a new revision on rotation. Deployable configuration contains only its stable credential reference. Connecting or disconnecting that reference is staged; rotating its contents is immediate. Admission freezes the credential revision and encrypted material with the deployment snapshots. Discard cannot undo a rotation.
_Avoid_: Saved credential contents, credential revision as configuration

**Working State**:
The mutable Environment configuration currently being edited, with a revision that advances as edits are persisted. Persisting edits preserves Working State without publishing it as Saved State or making it eligible for deployment. Removing a Volume from Working State also deletes its draft identity and Node Introduction when no Saved revision, deployment snapshot, removal attempt, or other Node Introduction retains it. Retained identity alone does not make a Volume visible on the canvas; runtime connectivity does not determine draft retention. Every write keeps the Environment's rules: one writer per Volume without Shared Writes, references only to Services and variables that exist, no reference cycle, and Setup Commands only in Services it has. A write is refused only for a rule it breaks that neither the Working State it replaces nor what it copies or restores already broke, so older or inherited breaks keep editing and deploying.
_Avoid_: Saved State, deployable revision, client diff ledger

**Saved State**:
An Environment's published configuration revisions. The latest is the only configuration a Deploy is admitted from; publishing Working State adds a revision without deploying.
_Avoid_: Working State, deployed configuration, draft

**Publish**:
Adding a revision to an Environment's Saved State from its Working State without deploying. A manual Deploy publishes first.
_Avoid_: Save, commit, promote

**Deployment**:
The Config Store's record of one Deploy: its Attempt Target, who started it and from what source, its Node Outcomes and its events. It stays after the Deploy ends; its runner is recorded, so a Deployment whose runner is gone reads as outcome unknown, never as running.
_Avoid_: Deploy (the command), Cloud Deployment Attempt, Deploy Record

**Attempt Target**:
The immutable complete runtime target frozen when a Deployment starts. One compiler materializes Derived Service Configuration from Saved State, then combines it with trigger-specific source revisions and required or opportunistic deployment requirements.
_Avoid_: Saved state, deploy preview, mutable queued request

**Node Outcome**:
One Environment Node's result within a Deployment: Deployed, Removed, Failed, Not attempted (an earlier failure stopped work before reaching it), or Unchanged (in the Attempt Target without a difference). A Service is Deployed the moment its container is replaced, not when the attempt ends. User-facing copy uses these labels verbatim.
_Avoid_: Applied, Skipped, Succeeded, Live (the Environment as it is now is not an outcome)

**Applied State**:
The per-Environment-Node projection of the latest confirmed runtime outcomes. Successful or removed nodes advance independently; failed or skipped nodes retain their previous Applied State.
_Avoid_: Latest deployment, active attempt, all-or-nothing baseline, "Applied" in user-facing copy

**Environment Change Set**:
One pure, serializable comparison from the latest queued or running Deployment's Saved revision to Working State, falling back to per-node Applied State when none is active. Accepted deployment hides the submitted changes; later edits compare against that submission. Failed or cancelled work reappears against confirmed Applied State. A node never deployed compares against its Node Introduction, published or not, so every edit of it is a change; once it is in Applied State, never again. Lifecycle changes and setting changes are counted once; deployment progress is separate.
_Avoid_: Persisted diff, mutation log, deployment snapshot

**Environment Publication Review**:
Authority to publish the current Working State revision against one exact Saved State basis; later Working State edits invalidate that review. It always names the reviewed Working fingerprint, the Saved revision observed by the reviewer (or that no Saved State existed), and the complete destructive Service and Volume set, including Volume evidence; the set is explicit even when empty. Publish and manual Deploy supply this authority. Automated deployment triggers consume existing Saved State. Publication conflicts when its Saved basis is no longer latest; commands never silently rebase onto another user's revision.
_Avoid_: Optional destructive callback, deploy-only review, implicit safe publisher

**Saved State Command**:
One atomic mutation of Saved State that names the exact Saved revision it was constructed from. The Saved State aggregate serializes commands per Environment and refuses a stale basis. Discard publishes at most one replacement revision in the same transaction as its Working State reset.
_Avoid_: Latest-state mutation, automatic rebase, loop of Saved writes

**Discard**:
One command restoring a field, node, or the whole Environment to the Environment Change Set's comparison baseline in Working and Saved State. It guards the Working revision, Saved basis, and comparison baseline and writes both states atomically. A new-node field reset uses its Node Introduction without publishing that node. Discard never changes an accepted deployment's target.
_Avoid_: Layered reset plans, loop of Saved writes, implicit deployment cancellation

**Node Introduction**:
The strictly versioned configuration an environment node had immediately after its creation transaction finalized. It is the comparison and reset source for edits made before the node has Applied State; it is not a second editable draft.
_Avoid_: Initial diff, creation event log, default config

**Derived Service Configuration**:
The disposable compiler output produced from a complete Saved State authoring graph. It resolves attached Volumes into each Service's environment, mounts, and variable producer index. It belongs to an Attempt Target and is never independently edited or read as Saved authority.
At runtime lowering, Core supplies `PORT=8080` only when resolved authored variables omit `PORT`. The built-in `PORT` reference default uses the same value; authored variables override reference defaults. Built-in reference values do not by themselves add environment entries to containers. Generated and custom domains with a null target port follow this container `PORT`; explicit targets override routing only. HTTP healthchecks use the container `PORT`. Invalid authored values are not replaced by the default and fail lowering when a port is required.
Authoring and runtime both require 1–50 replicas; zero is refused by the Setting validator.
_Avoid_: Saved Service config, copied consumer snapshot, second source of truth

**Branch**:
An Environment made from another, its Parent, and deployed to its own Namespace. Its nodes keep their Parent's lineage. It runs Own Copies of the nodes picked, and uses what those need from its Parent's Namespace as Live Nodes. Unless it is a Kept Branch, it may close once it syncs into its Parent, and closes on its own a week after its latest Deployment. Branching rules are pure authored-configuration rules, the same for every client.
_Avoid_: Fork, clone, preview; "branch" alone for a Git branch

**Conditional Sync**:
A PR Environment's Sync into one of its Destinations that goes live with its pull request's merge; a Sync from it into any other Environment stages now. It keeps what landing needs, sealed secrets and registry credentials included, so it lands even once the PR Environment is gone. Where the Destination changed a row too, the pull request's value lands only as a hint, which a take stages. The author's own edits to the PR Environment, or a new target Git branch, withdraw it; what the PR Environment follows from its Parent doesn't.
_Avoid_: Approval, deferred sync, Conditional Save

**Parent**:
The Environment a Branch was made from, whose Namespace lends the Branch its Live Nodes. An Environment without one, such as production, is a root.
_Avoid_: Base, upstream

**Own Copy**:
A node a Branch deploys in its own Namespace, made from its Parent's configuration with the same lineage. An Own Copy of a Volume starts empty.
_Avoid_: Clone (a copy of data)

**Live Node**:
A node a Branch uses without deploying it. It is reached in the Namespace that runs it as `{service}.{namespace}.internal`, with the values it has there.
_Avoid_: Portal, shared node, borrowed node

**Setup Command**:
A command a Branch adds after one Own Copy's own pre-deploy command, in the same Hook Container, until that Service first deploys successfully. It prepares the Own Copy's data, for example by seeding it.
_Avoid_: Seed script, data hook, post-deploy hook

**Kept Branch**:
A Branch that stays after syncing into its Parent and never closes on its own, such as staging.
_Avoid_: Long-lived environment, permanent branch

**Starting point**:
A Branch that was never deployed, kept as the recipe other Branches copy from. Its nodes stay staged until it deploys once.
_Avoid_: Template, draft branch

**Destination**:
Where a Branch syncs its changes by default: its Parent, or for a PR Environment, each Environment that deploys the pull request's target Git branch with nothing in its Parent chain deploying that Git branch too. Environments below a Destination that deploy the same Git branch get the merged code but not the settings; they catch up by Follow. A pull request whose target Git branch nothing deploys has no Destination.
_Avoid_: Target, sync target

**Sync**:
Putting one Environment's changes, chosen change by change, into another Environment of the same Project as the receiver's changes to deploy. It takes the sender's Working State, deployed or not, and never deletes or deploys anything. It compares over what the two last shared, so a change left out, or discarded by the receiver before it deploys, is offered again; a change the receiver also made since then is flagged and, if synced, overwritten. Sizing, custom domains, generated addresses, the Git branch and Volume data never sync. A secret's value never syncs either: a secret the receiver has stays as it is, and one it lacks arrives without a value, which the receiver's Deploy refuses until it is set. Any two Environments of a Project sync: a Branch into its Parent, sideways or skipping a level, a root into a Branch or into another root. A pair that never synced compares over where the sending Branch was made, else where the receiving Branch was made, else the receiver itself. Unless a Branch syncs into its own Parent, what it only got from its Parent is left out unless picked.
_Avoid_: Save, push, promote, merge (a GitHub merge only), Publish

**Follow**:
A Branch receiving what its Parent deploys, as staged changes in its Working State. When a Deployment applies changes in an Environment, each of its Branches gets them, without waiting for its own undeployed changes, so they flow down one level per deploy. Where the Branch changed a setting too, its own value stays and the Parent's is a Use hint. Each of the Parent's changes is delivered once: one the Branch discards stays a Use hint until the Parent changes that setting again. Never-synced settings don't follow. A Parent's secret value follows into a Branch that never set its own.
_Avoid_: Update, pull, rebase, inherit

**Never sync**:
A mark an Environment puts on one of its settings: Sync never carries it from that Environment and never changes it there. It joins sizing, custom domains, generated addresses, the Git branch and Volume data, none of which ever sync. A Branch of the Environment still gets the Environment's value; the mark doesn't carry into Branches.
_Avoid_: Pin, lock, local override

**Deploy Snapshot**:
The observer-relative Machine, Service Container, and Docker Volume observations gathered for one Deploy, including target-specific Container and Docker Volume failures and omissions. Completeness is relative to the entry Machine's current visible required fan-out, not Cluster truth.
_Avoid_: current cluster state, desired state, cluster snapshot, authoritative Cluster completeness

**Prune Refusal**:
Why a full reconciliation must not remove visible drift. Observer-relative; never a claim of Cluster completeness. Absence means this Deploy may remove obsolete Services owned by the resolved user Namespace.
_Avoid_: prune flag, Cluster-complete snapshot

**Deploy Plan**:
The ephemeral sequence of operations calculated for one Deploy. It may complete only a prefix and is neither persisted nor generally rolled back.
_Avoid_: Desired state, workflow

**Deploy Preview**:
The observer-relative plan-plus-warnings offered for confirmation before one Deploy executes. It is Live Observation shaped for a decision, not persisted state.
_Avoid_: persisted plan, cluster decision record

**Destructive Change**:
Removal of an existing Service or explicitly requested destruction of a Docker Volume, requiring operator approval. Ordinary Service updates, container replacements, and scaling changes do not require additional destructive confirmation.
_Avoid_: Every container replacement, implicit Volume deletion

**Deploy Progress**:
Live evidence of one in-flight Deploy: the current operation, the completed prefix, and health/hook waits. It is not Cluster Watch, not a workflow status, and not persisted.
_Avoid_: Watch frame, durable Deploy status, workflow state

**Deploy Outcome**:
The evidence from a Deploy Plan: completed operations, any failed operation, every unattempted operation, and narrow replacement compensation. Sequential failures retain prefix/suffix ordering, while a preflight rejection may name a later operation before any operation runs; neither implies atomicity or general rollback.
_Avoid_: Bare deployment error, transaction result

**Replacement Compensation**:
A bounded attempt to clean up a failed replacement and restore what it replaced: the prior Service Container when it was stopped, or the prior daemon release after a failed Upgrade. Its outcome records recovery success or failure; it does not reverse application data writes or other completed changes.
_Avoid_: General rollback, atomic deployment

**Docker Volume**:
A machine-local Docker storage resource and possible placement anchor. Its name is meaningful only together with its Machine.
_Avoid_: Cluster volume, replicated volume, CSI volume

**Data Loss**:
One named thing an operation will destroy, carrying the identity that makes it unique. A Data Loss list is Live Observation from one observer, not a complete Cluster view, and it is not a warning, a plan, or an operation.
_Avoid_: warning, plan, operation

**Data Loss Confirmation**:
The exact Data Loss identities an operator accepted from one Live Observation. One name may confirm several observed identities, but Data Loss that appears later remains unconfirmed.
_Avoid_: confirmation flag, confirmation names, Observed Data Loss

**Machine Pool**:
A ZFS storage budget on one storage-ready Machine. Provisioned Volumes live on it. Docker's data-root, image layers, and build cache do not.
_Avoid_: Cluster pool, auto-created pool, dedicated disk, Machine ZFS Pool, ZFS-enabled cluster

**Provisioned Volume**:
A Docker Volume backed by a dataset on a Machine Pool, with a declared maximum size. An ordinary named Docker Volume is not one and is unaffected. User-facing copy (CLI output, errors, dashboard) calls it a Managed volume and an ordinary one a Docker volume; Provisioned stays the wire and code name. A Machine without a Machine Pool reads Docker only.
_Avoid_: Managed ZFS Volume, cluster volume, storage class, CSI volume; Managed volume in wire or code names

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
The IPv4 /24 subnet selected for one Machine's containers. It is an optimistic allocation candidate and may overlap another Machine Subnet when operators use independent allocation histories.
_Avoid_: Reserved subnet, globally allocated subnet

**Operator Allocation History**:
An operator’s saved Machine assignments, including enrollments not yet visible in a Cluster Observation. It coordinates that operator’s enrollment attempts, not independent operators, and is not Cluster truth.
_Avoid_: allocator role, global reservation, Cluster IPAM

**Cloud Enroll Token**:
Cloud's Organization-scoped bearer that authorizes copy-paste founding and joining. It does not own enrollment lifecycle and is not a Pairing Credential or Management Capability.
_Avoid_: pairing token, API key, join URL as identity

**Founding Claim**:
Cloud's Organization-scoped exclusive pending enrollment attempt. It has no automatic timeout or transfer: only the matching founder resumes it, and recovery requires an explicit verified reset.
_Avoid_: token claim, timed lease, init mutex, leader election, cluster lock

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
A Service Container that is healthy, has a Container Address, and carries this observer's selected Serving Shape for its Qualified Service. It is observer-derived eligibility to receive traffic, not a replica identity.
The selected shape is the newest traffic-eligible shape observed for that Qualified Service. A starting, unhealthy, or stopped replacement does not exclude healthy older Containers; once a newer shape can take traffic, only that shape serves.
_Avoid_: replica, endpoint, upstream

**Serving Shape**:
The content identity of the Resolved Service Spec fields whose change requires a new Container. Equal shapes are interchangeable Containers. It is derived from observed spec content, not Cluster intent. Service ID is not a Serving Shape.
_Avoid_: current version, desired generation, Service ID as generation

**Public Ingress**:
The public HTTP request path from DNS resolution through a Machine's Ingress Proxy to a Serving Container. It is a diagnostic boundary, not a single process or globally authoritative edge.
_Avoid_: Edge

**Ingress Proxy**:
The Machine-local process that receives published HTTP traffic and routes it toward Serving Containers. It is one component of Public Ingress, not the whole public request path.
_Avoid_: Edge, public ingress as a process

**Published Ingress Configuration**:
A validated routing configuration made available to a Machine's Ingress Proxy. Publication records the daemon's completed handoff; it does not claim that the Ingress Proxy has adopted the configuration or successfully served traffic from it.
_Avoid_: Activated configuration, accepted configuration

**Internal DNS Answer**:
An observer-local, TTL-zero A answer derived from Serving Containers and optionally filtered by this Machine's Membership Observations. It is not persisted or Cluster truth even though the DNS response is authoritative for the `.internal` zone.
_Avoid_: Service registry record, Cluster-wide endpoint set

**Caller Namespace**:
The Namespace attributed to an Internal DNS query by matching its source Container Address to exactly one visible Service Container or Hook Container. It is observer-relative attribution, not authenticated identity; zero or several matches mean there is no Caller Namespace. A `{service}.internal` query uses that Namespace as the missing label; without a Caller Namespace the name is NXDOMAIN.
_Avoid_: caller identity, authenticated client, source registry

**Ingress Hostname**:
The HTTP hostname a Service publishes through ingress. It is always an explicit validated name: the runtime never generates one, and Cloud expands its managed hostnames against the Organization's Cluster Domain before they arrive. An empty string is not a hostname.
_Avoid_: empty hostname sentinel, Cluster Domain label

**Hostname Owner**:
The Qualified Service that wins an Ingress Hostname from one observer's Service Container observations; Serving is not required. Derived; not a persisted record, lease, or lock. Different observers may select different owners until their observations match.
_Avoid_: hostname lease, ownership table, global hostname uniqueness, Serving as the ownership gate

**Certificate Material**:
The certificate and private key held in cluster state for one Ingress Hostname or single-level wildcard `*.x`. It is served as given; it is not an issuance request and not a local proxy store. Published material comes from an operator or Cloud; ACME never orders, renews, or overwrites it, and a published wildcard serves every hostname one label under `x`.
_Avoid_: Caddy certificate, ACME certificate, cert secret

**Certificate Policy**:
The cluster-state values that steer certificate issuance: authority directory, external account binding, key type, renewal fraction, backoff bounds, and probe timeout. Absence means the daemon's built-in defaults. A challenge kind the daemon cannot perform is a refusal, not a default.
_Avoid_: ACME config, daemon certificate constants, CA settings

**Hostname Verdict**:
What one Machine saw when it reached for this Cluster through an Ingress Hostname: the hostname does not resolve, is unreachable, redirects to HTTPS, reaches elsewhere, or reaches this Cluster — directly, or via a proxy. A proxy here is a third-party front the user runs, such as a CDN; it is never the Ingress Proxy. It gates certificate issuance and can be older than the latest DNS change. It is not Caddy health, certificate readiness, or a Deploy failure.
_Avoid_: Cluster DNS Verdict, DNS health, certificate gate

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

**Cloud Pairing**:
Cloud's Organization-scoped association with one Cluster generation, held on each Machine as the `cloud` Management Client. It scopes enrollment and connection candidates; the daemon never sees it, and it is neither Cluster membership nor evidence of reachability.
_Avoid_: live connection, Cluster authority, per-Machine identity, Management Client

**Standalone Cluster**:
A Cluster with no Cloud Pairing, operated through the CLI over SSH contexts. Its runtime is fully operable, but it has no Config Store, so it receives no authored configuration or other Cloud-driven features.
_Avoid_: self-hosted cluster, offline mode, unpaired as a fault

**Release Channel**:
One of exactly two names a daemon or installer may follow: `stable` (the highest published `vX.Y.Z`) or `beta` (the highest published release, `vX.Y.Z-beta.N` or stable). A channel is scoped to a release line (a major version): a daemon resolves `ployz.sh/v<its major>/<channel>`, so it never crosses a breaking release through a channel. Only the live installer reads the unscoped `ployz.sh/<channel>`, which points at the newest line. A channel only moves forward, and upgrading through one never downgrades a Machine; only an exact version crosses a line or moves backwards. A build from `main` is addressable by tag or commit, never by a channel.
_Avoid_: latest, nightly, dev channel

**Upgrade**:
Moving a Machine's daemon to another published release in one durable attempt the Machine runs itself, ending as succeeded, failed, or interrupted. When a failed Upgrade had already replaced the daemon, Replacement Compensation restores the prior release on the same line.
_Avoid_: Update (that is Branch staging), self-update

**Hosted DNS**:
The Ployz-run service that grants Cluster Domains and serves their public records. Only Cloud calls it (see `dashboard/CONTEXT.md`); the daemon, SDK and CLI make no Hosted DNS calls, and a Cluster stores no reservation.
_Avoid_: Cloud DNS, cluster-held reservation, generated domain as runtime state

**Pairing Credential**:
The secret identifying the current Cloud Pairing and authenticating the CLI's enrollment callbacks to Cloud for that attempt. It never reaches the daemon and is distinct from a Machine's Management Capability.
_Avoid_: Management Capability, Machine identity, presence proof

**Management Capability**:
A protected bearer granting shared administrative Machine RPC access to one Machine over the management transport. Possession does not prove the intended Machine identity or Cloud Organization authorization.
It is shared administrative authority, not per-user access; rotating a Management Client revokes every previous holder of that client's capability, and Cloud logout does not revoke a separately held capability.
_Avoid_: per-user permission, read-only grant, Pairing Credential, management endpoint

**Management Client**:
One named holder slot on a Machine, such as `cloud` or a signed-in device's `cli-<device>`, whose client key may use the management transport. Setting it mints a Management Capability for that holder; clearing it revokes only that holder's connections and leaves a Cleared tombstone of its public keys. A redial with a tombstoned key is refused with `CLIENT_CLEARED`, confirming the removal; any other refused key gets `CLIENT_REFUSED`. The Machine knows holders, never the people or Organizations behind them.
_Avoid_: Cloud Pairing, user, session, per-user permission

**Management Identity**:
The Machine's iroh public key, identifying its management plane to Cloud and the remote CLI. It is not a mesh peer, a Machine ID, or the WireGuard key.
_Avoid_: Machine ID, WireGuard key, Advertised Endpoint, management endpoint

**Ployz Relay**:
The Ployz-hosted iroh relay through which clients reach a Management Identity behind NAT. It is shared infrastructure, not part of a Self-hosted Cloud. It carries ciphertext only and is never a mesh peer; public relays are not configured.
_Avoid_: DERP, public relay, self-hosted relay, hosted relay protocol, mesh peer

**Connection Candidate**:
An Organization's protected access descriptor for one Machine in its current Cloud Pairing. It is a way to attempt a connection, not membership or live presence.
_Avoid_: Machine catalog, registered member, reachable Machine

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

**Deployment Log ID**:
An adapter-supplied identity for the deployment attempt that created a Container. It is creation metadata used to select output, not part of the authored Service configuration or a globally coordinated deployment record.
