# Ployz Cloud

Ployz Cloud is the product and workflow context around creating, connecting, and operating Ployz runtime machines. Shared runtime bootstrap terms follow the [Ployz runtime glossary](../core/CONTEXT.md) and are mirrored here for Cloud product language.

## Language

**Environment Node**:
A deployable element of an Environment whose desired configuration participates in reviewed snapshots. Services and Environment Resources are Environment Nodes, while retaining distinct storage and lifecycle behavior.
_Avoid_: Canvas node when referring to deployment identity

**Service Metadata**:
The display name of a Service, stored on its stable identity and saved immediately. Renaming does not change Private DNS, the stable service slug, Working State, or any accepted deployment. References target IDs; managed `PLOYZ_SERVICE_NAME` exports the stable slug.
_Avoid_: Deployable name, DNS alias

**Deployment Policy**:
Immediate Service preferences controlling automated admission and where its Image Builds start: automatic Git deployment, waiting for CI, watch paths, image update preference, and the Preferred Builder. Trigger evaluation combines current policy with Saved configuration and rechecks policy under the Environment lock before admission. Policy never enters configuration comparison or Discard. Waiting Git triggers resume after check-suite events or the ingestion sweep; all selected Services share one Environment admission.
_Avoid_: Staged source settings, runtime configuration

**Registry Credential**:
Current encrypted authentication material owned by a Service identity, with a new revision on rotation. Deployable configuration contains only its stable credential reference. Connecting or disconnecting that reference is staged; rotating its contents is immediate. Admission freezes the credential revision and encrypted material with the deployment snapshots. Discard cannot undo a rotation.
_Avoid_: Saved credential contents, credential revision as configuration

**Environment Resource**:
A non-Service Environment Node with stable identity and type-owned configuration and lifecycle behavior. Volumes are the current Environment Resource type; effects on a Service's container template remain Service-owned.
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

**Dedicated Volume**:
A Volume mounted by exactly one Service, presented as part of that Service rather than as its own element. Usage, not storage: a Dedicated Volume may be of either Volume Kind.
_Avoid_: Attached volume (attachment is the mount relationship, whatever the count)

**Shared Volume**:
A Volume mounted by two or more Services, presented as its own element linked to each of them.

**Unmounted Volume**:
A Volume no Service mounts, presented as its own element with no Links.

**Link**:
A variable reference or a Volume mount between two Environment Nodes, read as one node using another. Links arrange the canvas; they do not order deployment.
_Avoid_: Dependency (deployment ordering, references only), edge, connection

**Cloud Bootstrap Token**:
The single-redemption bearer secret embedded in a copied Cloud Bootstrap Invite command. The token is not the org, cluster, machine identity, join token, or callback credential.
_Avoid_: Bootstrap token, server bootstrap token, callback token

**Cloud Bootstrap Redemption**:
One machine's approved use of a Cloud Bootstrap Session or Cloud Bootstrap Token. For interactive bootstrap, browser approval creates the redemption by binding the session to an organization. An unapproved session and an unredeemed invite are not redemptions; the redemption is machine-local evidence that Cloud can turn into founder or joiner bootstrap material.
_Avoid_: Token use, bootstrap report, machine acceptance

**Founding Claim**:
The Organization-scoped assignment of one Server to found its Organization Cluster. It has no automatic expiry or transfer; the matching Server resumes it until completion or an operator performs a Manual Founding Reset.
_Avoid_: Token claim, founder election, leader election, founder failover

**Cloud Pairing**:
Cloud's Organization-scoped association with one Cluster generation, held on each Server as its `cloud` Management Client. The runtime knows only the Management Client, never the Organization or the Pairing Credential.
_Avoid_: Management Client, Cluster membership, live connection

**Connection Candidate**:
An Organization's protected access descriptor for one Server in its current Cloud Pairing. It permits a connection attempt but establishes neither membership nor live presence.
_Avoid_: Server catalog, online Server, registered member

**Management Capability**:
The protected bearer a Connection Candidate holds for reaching one Server over the in-process management transport. It grants shared administrative access; rotation revokes every previous holder, and removal is confirmed by a successful Clear response or an authenticated response explicitly confirming that the Server's `cloud` Management Client is cleared, never by absence or timeout.
_Avoid_: per-user permission, Pairing Credential, presence proof

**Management Identity**:
The Server's iroh public key that a Management Capability dials. It is not a mesh peer, a Machine ID, or evidence of presence.
_Avoid_: Machine ID, WireGuard key, online Server

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

**Self-hosted Cloud**:
A Cloud instance an operator runs on their own infrastructure from the released image: Cloud web, Cloud worker, Inngest, Redis and Postgres, with their own GitHub apps. It has no billing: Polar is never configured and every Organization runs unlimited, meaning every Custom Domain Capability check is granted. It still uses the Ployz-hosted relay, Hosted DNS, installer and release binaries.
_Avoid_: Standalone Cluster, on-prem control plane, self-hosted relay

**Billing Plan**:
The single paid Ployz Cloud subscription an Organization holds through Polar, sold as "Pro". Holding it is the plan; there is no plan column and no second tier. It grants only the Custom Domain Capability: an Organization without it keeps every other capability, with unlimited members. Its display name is product copy, not a stored value. A Self-hosted Cloud has no Billing Plan.
_Avoid_: Free plan, Teams plan, plan slug, subscription tier

**Custom Domain Capability**:
Whether an Organization may link a custom hostname to a Service. Granted when the Cloud is self-hosted or the Organization holds an active Billing Plan, judged from Cloud's cached subscription state, never from a live billing call. It is the only plan-gated capability at the first stable release.
_Avoid_: Entitlement, feature flag, paid feature check

**Cloud Lens**:
Cloud's role after bootstrap is to observe, display, and request operations against the Organization Cluster. Cloud is not the source of runtime truth and must not be the only authority needed to recover the cluster.
_Avoid_: Cloud control plane, cloud authority, hosted source of truth

**Organization change log**:
Cloud's record of which rows of an Organization's organization-owned tables changed, written by database triggers and read by transaction horizon (xid) cursor. Open tabs follow it through one change stream per Organization and re-read only the changed rows; runtime sessions follow it to notice a removed pairing. It keeps 24 hours; a cursor older than the oldest retained change reads in full. It names changes, not their content, and is never a source of truth.
_Avoid_: Event log, audit log, outbox, notification channel

**Cloud Deployment Attempt**:
The Cloud-owned, user-visible attempt to turn one frozen Attempt Target into runtime state through queueing, planning, building, and authoritative deploy. It remains one Environment-level attempt even when successful Environment Nodes apply and failed nodes remain pending independently.
_Avoid_: Prepared snapshot, build workflow, Core Deploy

**Working State**:
The mutable Environment configuration currently being edited, with a revision that advances as edits are persisted. Persisting edits preserves Working State without publishing it as Saved State or making it eligible for deployment. Removing a Volume from Working State also deletes its draft identity and Node Introduction when no Saved revision, deployment snapshot, removal attempt, or other Node Introduction retains it. Retained identity alone does not make a Volume visible on the canvas; runtime connectivity does not determine draft retention.
_Avoid_: Saved State, deployable revision, client diff ledger

**Cluster Domain**:
The generated base hostname an Organization holds from Hosted DNS, owned by the Organization rather than by any Cloud Pairing, so it survives teardown and re-pairing. Cloud reserves it on the first deployment that needs a generated hostname; until then the Organization has none. Hosted DNS picks the name and it never changes: no rename, no manual release, and a name Hosted DNS reaps or retires is an operator incident, not a replacement. Cloud publishes the apex records for reachable ingress Servers, renews its lease hourly whether or not a Cluster is paired, keeps one wildcard certificate for the name and `*.name` (replaced within 30 days of expiry, its key stored encrypted in Cloud) published to the Cluster as Certificate Material, and releases it only when the Organization is deleted. The runtime never holds it; managed hostnames reach the runtime already expanded into explicit hostnames.
_Avoid_: Hosted DNS hostname as runtime state, generated domain as pairing state, observed cluster domain

**Public Domain Variable**:
`PLOYZ_PUBLIC_DOMAIN` is the last linked custom domain in a Service's captured route list, otherwise the last generated hostname expanded against the Organization's Cluster Domain during deployment preparation. DNS and certificate health do not affect selection. Domain lists retain link order; port edits retain position, removal falls back to the preceding domain, and relinking appends. With no public hostname the managed variable is absent. Cloud exposes it for references and injects it into the deployment environment; authored overrides retain the usual variable precedence. Running containers keep the value captured for their deployment.

**Environment Publication Review**:
Authority to publish the current Working State revision against one exact Saved State basis; later Working State edits invalidate that review. It always names the reviewed Working fingerprint, the Saved revision observed by the reviewer (or that no Saved State existed), and the complete destructive Service and Volume set, including Volume evidence; the set is explicit even when empty. Save and manual Deploy supply this authority. Automated deployment triggers consume existing Saved State. Publication conflicts when its Saved basis is no longer latest; commands never silently rebase onto another user's revision.
_Avoid_: Optional destructive callback, deploy-only review, implicit safe publisher

**Saved State Command**:
One atomic mutation of Saved State that names the exact Saved revision it was constructed from. The Saved State aggregate serializes commands per Environment and refuses a stale basis. Discard publishes at most one replacement revision in the same transaction as its Working State reset.
_Avoid_: Latest-state mutation, automatic rebase, loop of Saved writes

**Derived Service Configuration**:
The disposable compiler output produced from a complete Saved State authoring graph. It resolves attached Volumes into each Service's environment, mounts, and variable producer index. It belongs to an Attempt Target and is never independently edited or read as Saved authority.
At runtime lowering, Core supplies `PORT=8080` only when resolved authored variables omit `PORT`. Generated and custom domains with a null target port follow this container `PORT`; explicit targets override routing only. HTTP healthchecks use the container `PORT`. Invalid authored values are not replaced by the default and fail lowering when a port is required.
_Avoid_: Saved Service config, copied consumer snapshot, second source of truth

**Applied State**:
The per-Environment-Node projection of the latest confirmed runtime outcomes. Successful or removed nodes advance independently; failed or skipped nodes retain their previous Applied State.
_Avoid_: Latest deployment, active attempt, all-or-nothing baseline, "Applied" in user-facing copy

**Node Outcome**:
One Environment Node's result within a Cloud Deployment Attempt: Deployed, Removed, Failed, Not attempted (an earlier failure stopped work before reaching it), or Unchanged (in the Attempt Target without a difference). A Service is Deployed the moment its container is replaced, not when the attempt ends. User-facing copy uses these labels verbatim.
_Avoid_: Applied, Skipped, Succeeded, Live (Editor Mode is the current view, not an outcome)

**Deployment Mode**:
The canvas viewing one Cloud Deployment Attempt: it draws that attempt's Environment Nodes with their Node Outcomes, including nodes since deleted or removed by the attempt, and omits nodes created afterwards. **Editor Mode** is the default canvas, drawing the Environment as it is now; it is the only mode that edits.
_Avoid_: Deployment page, deployment detail screen, Live Mode (for Editor Mode)

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
One pure, serializable comparison from the latest queued or running Cloud Deployment Attempt's authored Saved revision to Working State, falling back to per-node Applied State when no attempt is active. Accepted deployment hides the submitted changes; later edits compare against that submission. Failed or cancelled work reappears against confirmed Applied State. Node Introductions supply field resets only while a node is absent from Head, Saved State, and Applied State. Lifecycle changes and setting changes are counted once; deployment progress is separate.
_Avoid_: Persisted diff, mutation log, deployment snapshot

**Discard**:
One command restoring a field, node, or the whole Environment to the Environment Change Set's comparison baseline in Working and Saved State. It guards the Working revision, Saved basis, and comparison baseline and writes both states atomically. A new-node field reset uses its Node Introduction without publishing that node. Discard never changes an accepted deployment's target.
_Avoid_: Layered reset plans, loop of Saved writes, implicit deployment cancellation

**Cloud Deployment Stage**:
The current progress of a Cloud Deployment Attempt. Durable statuses are queued, planning, and deploying before a terminal outcome. Image Builds start when the attempt is dispatched and are progress within any non-terminal status; they never hold the Environment execution slot. Image delivery is progress within deploying; that status owns the Environment execution slot until cleanup completes or the outcome is recorded as unknown. Image Cleanup runs after the terminal outcome releases the slot and never changes the status. It is distinct from a runtime Phase, which groups dependency-ordered services inside a Deploy Plan.
_Avoid_: Phase, prepared, build status

**Pending Attempt**:
A queued Cloud Deployment Attempt admitted while another queued attempt is already building (it has a run). An Environment has at most one building and one pending attempt, besides the attempt holding the execution slot. The newest admission, manual or automatic, always replaces the pending attempt, including a Retry or one carrying a reviewed volume removal; the replacement's Saved revision still carries that review, so the removal still happens. The building attempt is never replaced. The pending attempt is dispatched, and its Image Builds start, only when the building attempt leaves queued: it takes the slot, fails, or is cancelled.
_Avoid_: Waiting attempt, second queue, backlog

**Deploy Preview**:
The read-only Core projection Cloud persists after preparation and image delivery, before confirming application execution. It is product history rather than runtime authority; the live prepared handle owns confirmation and retained image resources.
_Avoid_: Deploy Plan, reservation, dry run

**Build Receipt**:
Private evidence retained from a completed Image Build so deployment in the same or a later Cloud Deployment Attempt can reuse matching build output. Core rechecks content availability and required platforms; receipt retention does not advance Applied State.
_Avoid_: Applied image, deployment success

**Build Platform Requirement**:
The set of target platforms a service image must cover for one Cloud Deployment Attempt, derived by shared Core preparation from placement and build settings. Preparation checks actual destinations again before image delivery. A reused image receipt may cover a superset.
_Avoid_: Organization Cluster architecture, global build platform, builder architecture


**Deployment Logs**:
The user-facing output for a Cloud Deployment Attempt: its lifecycle events together with output from the Service Containers and Hook Containers created by that attempt. Availability of container output is distinct from retention of the attempt’s lifecycle history.
_Avoid_: Deploy Progress alone, Build Logs

**Image Build**:
The build of one Service image within a Cloud Deployment Attempt, with its own Build Steps, output, and outcome. An attempt's Image Builds may run on different Builders at the same time; when one fails, the others still finish and leave Build Receipts before the attempt fails.
_Avoid_: Build batch, combined build log, Bake run

**Builder**:
A place that runs Image Builds: the Organization Cluster, which chooses one of its Servers, or GitHub Actions in the Service's own repository. The Cluster picks the Server named in the Service's latest Build Receipt while it accepts Builds and is reachable (its build cache is warm), otherwise it spreads the attempt's builds across Servers that accept Builds; each Image Build records the Server and why it was chosen.
_Avoid_: Build host, build runner, builder Server; Builder for Dockerfile or Railpack

**Build Order**:
The Organization's ordered list of Builders that an Image Build tries, moving to the next only when the current one does not start the build in time. The last Builder in the order waits instead. A Service's Preferred Builder is tried before it. A build that has started moves on only when GitHub fails it for infrastructure reasons (the runner stopped before its final report, the run pushed nothing without a failed Build Step, the runner couldn't install ployz, or the run ran out of time); a failed Build Step is final, and the last Builder has nowhere to move, so the build fails with that reason. Until the Organization chooses one, its Build Order is its servers only, then GitHub first once any repository its Services build from has the Build Workflow.
_Avoid_: Build pool, build preference, fallback builder

**Preferred Builder**:
One Builder a Service tries first, before the Organization's Build Order: GitHub Actions or one specific Server. When it does not start the build in time, the Image Build continues with the Build Order; it never forbids the others. A preferred Server that is gone or no longer accepts Builds when the build starts sends the Image Build back to Auto (the Build Order alone), and the Image Build records why.
_Avoid_: Builder override, pinned builder, build target

**Build Workflow**:
The `.github/workflows/ployz-build.yml` file that lets GitHub Actions be a Builder for one repository. It only runs when Cloud dispatches it, and calls the `getployz/build` Action. A repository is ready when the workflow is active on its default branch; Cloud checks this from GitHub and never writes the file itself.
_Avoid_: CI pipeline, build config, GitHub integration

**Build Method**:
How a Service's image is described for building: a Dockerfile or Railpack.
_Avoid_: Builder, builder type

**Build Step**:
One unit of an Image Build as the Engine reports it: a BuildKit step (a Dockerfile instruction, image resolution, or context transfer) or a Ployz-owned phase such as source upload or image delivery. A Build Step is keyed stably within its attempt, changes state until it completes, and owns the output attributed to it. Build Steps are retained with the attempt, separately from lifecycle history.
_Avoid_: Build log line, vertex, build stage (a Cloud Deployment Stage is not a Build Step)

Git repository identity and access are separate. Cloud can read a public GitHub repository anonymously or use an Organization member's connected GitHub App installation. Public access never falls back to installation credentials. Both paths pin a commit per Cloud Deployment Attempt and materialize it through the same source acquisition module. Automatic Git deployment and CI gating require installation access; public sources deploy manually.

Cloud infers deployment ordering from bound Service variable references in the frozen Attempt Target. Dependencies complete normal startup monitoring before dependent hooks and containers; explicitly configured HTTP health checks also gate unchanged dependencies. Edges within reference cycles are ignored, while dependencies entering or leaving those cycles remain. Literal text, self references, and references to empty Services do not impose ordering.
