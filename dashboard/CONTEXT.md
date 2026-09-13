# Ployz Cloud

Ployz Cloud is the product and workflow context around creating, connecting, and operating Ployz runtime machines. Shared runtime bootstrap terms follow the [Ployz runtime glossary](../core/CONTEXT.md) and are mirrored here for Cloud product language. Enrollment follows the [protected connection candidate decision](../docs/adr/0005-tailcat-connection-candidates.md).

## Language

**Environment Node**:
A deployable element of an Environment whose desired configuration participates in reviewed snapshots. Services and Environment Resources are Environment Nodes, while retaining distinct storage and lifecycle behavior.
_Avoid_: Canvas node when referring to deployment identity

**Environment Resource**:
A non-Service Environment Node with stable identity and type-owned configuration and lifecycle behavior. Variable Groups and Volumes are the current Environment Resource types; effects on a Service's container template remain Service-owned.
_Avoid_: Generic canvas item, Service subtype

**Cloud Bootstrap Invite**:
A time-limited Cloud permission that can issue one or more single-redemption Cloud Bootstrap Tokens for an Organization Cluster. A valid token redeem request is the approval boundary for each tokenized machine use; an invite grants bootstrap permission, not cluster truth.
_Avoid_: One-time bootstrap token, join token, cluster token, cluster intent

**Cloud Bootstrap Session**:
A short-lived Cloud session for interactive bootstrap from a target machine. The session lets a browser user choose the Cloud organization without putting a Cloud Bootstrap Token in the copied command; Cloud derives founder, joiner, or wait behavior from that organization's Organization Cluster state. A session that expires before approval creates no Cloud Bootstrap Redemption.
_Avoid_: Invite, localhost callback, browser-owned machine session

**Organization Cluster**:
The single runtime cluster owned by an organization. Adding machines expands the organization's cluster rather than selecting a separate cluster target.
_Avoid_: Project cluster, environment cluster, cluster draft

**Server**:
The user-facing name for a host participating in an Organization Cluster. Rust calls its runtime identity a machine; Cloud does not define a separate server truth.
_Avoid_: Machine in user-facing copy, Cloud server record

**Server Policy**:
The roles (accepts builds, services, ingress) and labels of one Server, mirroring the runtime's Machine Role and Machine Label. Cloud requests a policy change as a queued operation and reads the resulting policy back from machine observation; it keeps no separate desired-policy record and policy is not part of any Environment's Saved State.
_Avoid_: Server settings draft, machine config, cluster-wide roles

**Volume Kind**:
The explicit, user-chosen kind of a Cloud Volume: a Provisioned Volume (sized, quota-enforced, hosted only on a Server with a managed pool) or a plain Docker Volume (unsized, any Server). Both are machine-local; the kind is chosen at creation and shown with its trade-offs, never inferred from whether a size was typed.
_Avoid_: Storage class, volume type dropdown, managed volume toggle

**Cloud Bootstrap Token**:
The single-redemption bearer secret embedded in a copied Cloud Bootstrap Invite command. The token is not the org, cluster, machine identity, join token, or callback credential.
_Avoid_: Bootstrap token, server bootstrap token, callback token

**Cloud Bootstrap Redemption**:
One machine's approved use of a Cloud Bootstrap Session or Cloud Bootstrap Token. For interactive bootstrap, browser approval creates the redemption by binding the session to an organization. An unapproved session and an unredeemed invite are not redemptions; the redemption is machine-local evidence that Cloud can turn into founder or joiner bootstrap material.
_Avoid_: Token use, bootstrap report, machine acceptance

**Founding Claim**:
The Organization-scoped assignment of one Server to found its Organization Cluster. It has no automatic expiry or transfer; the matching Server resumes it until completion or an operator performs a Manual Founding Reset.
_Avoid_: Token claim, founder election, leader election, founder failover

**Connection Candidate**:
An Organization's protected access descriptor for one Server in its current Cloud Pairing. It permits a connection attempt but establishes neither membership nor live presence.
_Avoid_: Server catalog, online Server, registered member

**Manual Founding Reset**:
An operator-confirmed abandonment of a pending Founding Claim after endpoint access has been revoked. An absent Connection Candidate, failed connection, or timeout is not evidence permitting reset.
_Avoid_: Automatic reclaim, founder failover, token reset

**Waiting Cloud Bootstrap Redemption**:
A Cloud Bootstrap Redemption approved while an Organization Cluster has an active Founding Claim but no Cloud Connection. It has its own post-approval expiry separate from Cloud Bootstrap Session expiry, waits for the founder to establish a Cloud Connection, be abandoned, or for the waiting redemption to expire, and does not preissue runtime join authority, perform local machine mutation, or become founder automatically. Once expired, it is terminal and cannot later receive join material.
_Avoid_: Founder candidate, standby founder, pending machine join

**Abandon Founder Attempt**:
A Cloud-side operator action for the interactive Cloud Bootstrap workflow. It does not clean up, revoke, or mutate the already-formed local machine and is distinct from a Manual Founding Reset.
_Avoid_: Founder failover, automatic promotion, Cloud cleanup, machine removal

**Cloud Connection**:
Cloud's durable product-side relationship to an Organization Cluster after Cloud has confirmed access to the intended Server. A Cloud Connection exists only after reachability succeeds; a Cloud Bootstrap Redemption may establish one, but they are separate concepts and a connection is not cluster truth, machine membership, or recovery authority.
_Avoid_: Runtime authority, machine membership, Cloud control plane, recovery authority

**Accepted Machine Evidence**:
Durable machine-local material proving a host has held an accepted runtime machine identity or machine control-plane authority, such as accepted machine id state, NATS machine credentials, role authority material, or assigned substrate state. Failed or abandoned bootstrap attempt state, keeper binary presence, and generic install residue are not Accepted Machine Evidence.
_Avoid_: Install residue, failed attempt evidence, abandoned session evidence, Cloud-side redemption status

**Substrate Uninstall**:
An explicit local action that removes Ployz substrate and machine-local Ployz material from one machine. It may be forced despite Accepted Machine Evidence, but it does not remove cluster truth, delete user workloads, Docker images, Docker volumes, service containers, arbitrary networks, or runtime data by default. If no Accepted Machine Evidence and no removable Ployz substrate or material remain, it is an idempotent no-op success.
_Avoid_: Runtime wipe, machine removal, Cloud cleanup, destructive reset, force removed machine

**Cloud Lens**:
Cloud's role after bootstrap is to observe, display, and request operations against the Organization Cluster. Cloud is not the source of runtime truth and must not be the only authority needed to recover the cluster.
_Avoid_: Cloud control plane, cloud authority, hosted source of truth

**Cloud Deployment Attempt**:
The Cloud-owned, user-visible attempt to turn one frozen Attempt Target into runtime state through queueing, planning, building, and authoritative deploy. It remains one Environment-level attempt even when successful Environment Nodes apply and failed nodes remain pending independently.
_Avoid_: Prepared snapshot, build workflow, Core Deploy

**Working State**:
The mutable Environment configuration currently being edited, with a revision that advances as edits are persisted. Persisting edits preserves Working State without publishing it as Saved State or making it eligible for deployment.
_Avoid_: Saved State, deployable revision, client diff ledger

**Saved State**:
The latest explicitly published immutable revision of authored Environment configuration, including its reviewed destructive authority. Save and Deploy both plan against a selected Working State revision and obtain any required approval before publishing it; Save stops at publication without building images or changing running resources, while Deploy starts an attempt against that exact Saved revision.
_Avoid_: Applied state, frozen attempt target, unsaved draft

**Environment Publication Review**:
Authority to publish one exact captured Working State revision against one exact Saved State basis; later Working State edits remain unpublished and do not invalidate that review. It always names the reviewed Working fingerprint, the Saved revision observed by the reviewer (or that no Saved State existed), and the complete destructive Service and Volume set, including Volume evidence; the set is explicit even when empty. Every Saved-state publisher supplies this authority, including automated publishers that are permitted to publish only non-destructive changes. Publication conflicts when its Saved basis is no longer latest; commands never silently rebase onto another user's revision.
_Avoid_: Optional destructive callback, deploy-only review, implicit safe publisher

**Saved State Command**:
One atomic mutation of Saved State that names the exact Saved revision it was constructed from. The Saved State aggregate serializes commands per Environment and refuses a stale basis. Discard publishes at most one replacement revision in the same transaction as its Working State reset.
_Avoid_: Latest-state mutation, automatic rebase, loop of Saved writes

**Derived Service Configuration**:
The disposable compiler output produced from a complete Saved State authoring graph. It resolves attached Variable Groups and Volumes into each Service's environment, mounts, and variable producer index. It belongs to an Attempt Target and is never independently edited or read as Saved authority.
_Avoid_: Saved Service config, copied consumer snapshot, second source of truth

**Applied State**:
The per-Environment-Node projection of the latest confirmed runtime outcomes. Successful or removed nodes advance independently; failed or skipped nodes retain their previous Applied State.
_Avoid_: Latest deployment, active attempt, all-or-nothing baseline

**Attempt Target**:
The immutable complete runtime target frozen when a queued deployment request starts. One compiler materializes Derived Service Configuration from Saved State, then combines it with trigger-specific source revisions and required or opportunistic deployment requirements.
_Avoid_: Saved state, deploy preview, mutable queued request

**Deployment Requirement**:
The caller-selected failure policy for updating one Service in an Attempt Target: required or opportunistic; manual Deploy makes every included Service update required. An opportunistic failure preserves or attempts to restore the prior working version and allows deployment work to continue only when that version is retained or recovery succeeds; a required failure or failed recovery cancels later phases.
_Avoid_: Healthcheck policy, application version constraint, optional service, independent deployment

**Opportunistic Update**:
An approved pending update to an opted-in Service included alongside another Service's Git-triggered deployment, where the triggering Service is required. An eligible pending revision is attempted once per new triggering deployment, even if an earlier attempt failed and recovered; pending updates do not start background retry loops.
_Avoid_: Optimistic UI update, background updater

**Node Introduction**:
The strictly versioned configuration an environment node had immediately after its creation transaction finalized. It is the reset source for edits made before the node has Saved or Applied State; it is not a second editable draft.
_Avoid_: Initial diff, creation event log, default config

**Environment Change Set**:
One pure, serializable comparison from the latest queued or running Cloud Deployment Attempt's authored Saved revision to Working State, falling back to per-node Applied State when no attempt is active. Accepted deployment hides the submitted changes; later edits compare against that submission. Failed or cancelled work reappears against confirmed Applied State. Node Introductions supply field resets only before a node has Saved or Applied State. Lifecycle changes and setting changes are counted once; deployment progress is separate.
_Avoid_: Persisted diff, mutation log, deployment snapshot

**Discard**:
One command restoring a field, node, or the whole Environment to the Environment Change Set's comparison baseline in Working and Saved State. It guards the Working revision, Saved basis, and comparison baseline and writes both states atomically. A new-node field reset uses its Node Introduction without publishing that node. Discard never changes an accepted deployment's target.
_Avoid_: Layered reset plans, loop of Saved writes, implicit deployment cancellation

**Cloud Deployment Stage**:
The current progress of a Cloud Deployment Attempt: queued, planning, building, or deploying before a terminal outcome. It is distinct from a runtime Phase, which groups dependency-ordered services inside a Deploy Plan.
_Avoid_: Phase, prepared, build status

**Deploy Preview**:
The read-only Core projection Cloud persists before building to explain tentative machines and per-service Build Platform Requirements. It is product history rather than runtime authority and may differ from the later authoritative Deploy Plan.
_Avoid_: Deploy Plan, reservation, dry run

**Build Platform Requirement**:
The set of target platforms a service image must cover for one Cloud Deployment Attempt, derived from that service's tentative targets in its Deploy Preview. A reused image receipt may cover a superset.
_Avoid_: Organization Cluster architecture, global build platform, builder architecture
