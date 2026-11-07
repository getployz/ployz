# Ployz 0.4 security decisions

Status is planned, not implemented. This record supersedes the earlier HTML explainer wherever they differ. Source review uses commit `38666a984131d1c2a94d624abe5bb1ba8125f474`. It does not establish deployed versions, credentials, or operational isolation. The target release is **0.4**.

Read [the execution plan](plan.md) for ordering and acceptance evidence. Use [the kickoff prompts](kickoffs.md) to start individual threads later. [The external authority investigation](external-authority.md) records the separate DNS and GitHub track.

## The security promise

The goal is trust. A customer comparing hosted Ployz Cloud with running the dashboard on their own VPS should conclude that hosted Cloud is the safer choice. Someone with full access to np1 must not be able to steal a customer's secrets, run new containers or code, change settings, take data, or add servers or devices. Customer servers enforce this themselves, so the guarantee doesn't depend on np1 staying uncompromised. A self-hosted dashboard gets the same enforcement, so hosting is never the weaker option.

Root on np1 gives the attacker Cloud code, database writes, environment secrets, emails, sessions, workflows, and dashboard JavaScript. Customer Machines must reject new runtime effects unless customer-held authority permits them. Cloud may carry requests and proofs, but cannot manufacture that authority.

This is a bounded promise. Cloud still exposes data it already sees, including plain variables, logs, metadata, and credentials retained there. Those credentials can confer authority outside the Machine verifier. DNS and GitHub authority remain separately reported until their investigations establish a smaller boundary. A compromised dashboard can use an active browser approval session within its scope. Effects accepted during that session can outlast it.

The attacker can always withhold requests, proofs, responses, and Cloud service. They can lie about status. They cannot use that permission to stop or restart customer workloads directly. Start, stop, and restart require authorization too.

## Settled product choices

| Choice | Result for customers |
| --- | --- |
| Every runtime mutation requires Machine-side authorization | Deploy, lifecycle, storage, membership, key, build, ingress, and upgrade effects share the boundary. Read access does not imply write authority. |
| One approval covers a bounded workflow | Moving a Volume includes its planned stops, transfer, switch, restart, permitted retries, and recovery. It does not prompt for each RPC. |
| Browser sessions can authorize repeated actions | A native authentication prompt opens a short, scoped session. Ordinary action buttons keep working within it. Expiry or a broader request prompts again. |
| Malicious dashboard use of that session is accepted | No mandatory separate approval website, CLI-only sensitive-field rule, or repeated cryptographic ceremony. Scope and expiry limit authority, not the honesty of the page. |
| CLI uses the customer's Device key | Existing trusted CLI actions need no browser tap. MCP identity or an Organization Token alone cannot approve its own proposal. |
| GitHub deploy remains automatic | Approve a repository, branch, Environment, workflow/build policy, and settings revision once. Eligible pushes, including intentional force pushes, then deploy without taps. |
| Plain values stay plain | This work adds no default sealing and makes no confidentiality promise for plain credentials. |
| Trusted previews may inherit selected Sealed values | Explicit authorization names eligible previews and parent values. Server-side resolution or rewrapping keeps plaintext out of Cloud. |
| Trust stays on customer Machines | Existing trusted authority approves another key or Machine. Email, support access, and database edits cannot reset trust. |
| 0.4 uses a coordinated cutover | Upgrade the small fleet and enable enforcement with customer authority. Avoid permanent mixed-version dispatch and compatibility products. |
| DNS and GitHub get bounded investigations | Findings decide whether separation helps and what must enforce requests outside np1. No speculative agent product or new repository-read restrictions. |

Existing loss confirmations remain product decisions about deleted data. Authentication does not replace a confirmation that explains what disappears. Reuse the existing action or Approval card rather than adding a second crypto review screen. Exact system prompt wording is controlled partly by the platform.

## Shared contracts to settle before consumers

These are proposed implementation shapes. Thread A owns the final wire types, canonical encoding, errors, and examples. Other owners consume them after checkpoint C1.

### Trusted authority and transport

A trusted key record includes key identity, verification material, allowed authority, and a signed generation. A new key must chain to locally trusted authority. Transport authentication identifies the caller but does not authorize an effect. A WireGuard peer, Cloud connection, `cli-<id>` slot, staff session, or replicated Machine row is insufficient.

The local administrative socket remains a customer-root recovery boundary. Mesh TCP must authenticate the request's authority explicitly. It must not infer Full authority from the absence of an iroh connection. Replicated key and Machine records are observations until their authorization chain is verified.

The first trusted key must be installed through an independently authenticated customer-controlled path, normally a trusted CLI over local administration or verified SSH. A joining Machine pins the Cluster authority and its own enrollment authorization. Approving a join binds the new Machine identity, its recipient key, and allocation. `Register`, `Enroll`, `ShareSealingKey`, and initial Devices all consume that same authority before publication or key wrapping.

Browser passkeys require an enrollment authorization from an existing trusted Device or direct administration. **Decided 2026-11-07:** a passkey is not a Device. It is a new approve-only trusted key: it can approve workflows and nothing else, and it never opens connections or manages Devices. Machines verify its ES256 or EdDSA assertion locally, adding RustCrypto `p256` under bet 9. It must work for customers who only use the browser. The first passkey is approved on a Server's own terminal (`sudo ployz device approve --passkey CODE` over the local socket, also offered at the end of install), with the code matched against the browser, so a CLI Device is never required. Later passkeys are approved by any trusted Device, or by an existing passkey through its own prompt, never through a live session. Email login alone cannot establish the first runtime key. Recovery code use must prove possession to the Machine and must not become an unsigned Cloud reset route. Exact first-install and recovery commands are A1 deliverables, not assumed present today.

### Workflow authorization

An authorization binds a protocol version, Cluster identity, authority generation, operation ID, workflow kind, Environment, participants, targets, and an immutable effective payload digest. It also binds admission expiry, maximum execution bounds, allowed transitions, permitted recovery, and any destructive effects.

Deployment payloads include image digests, commands, arguments, mounts and their content hashes, plain values, Sealed references, build inputs, placement, privileges, routes, and hooks. A settings draft in Cloud is not an approved revision. Machines must enforce the complete effective payload at the effect boundary, including low-level calls and peer forwarding.

Retries reuse the operation and step identity. A changed payload is a different request. Machines persist the minimum local admission and consumption records needed across crashes. A step may repeat only according to its own retry rule. A hook that already ran is not a fresh execution simply because Cloud lost its receipt. Where an external effect cannot be proven complete, report uncertainty rather than promising exactly-once execution.

A workflow is a constrained transition graph, not a bag of allowed RPCs. Authorized recovery cannot change recipients, erase extra data, run different code, or select arbitrary rollback steps. Admission expiry stops new work. Already admitted work has a separately bounded completion/recovery allowance so expiry does not strand a half-moved Volume. The UI reports that distinction.

Resource-local freshness and consumption checks must survive daemon restart. There is no claim of one global Cluster sequence. For a destructive operation spanning Machines, each participant requires the receipts relevant to its own transition. Thread E must prove that a malicious driver cannot obtain both incompatible outcomes from an approved Move.

### Browser approval sessions and proposals

A passkey assertion authorizes an ephemeral browser public key, a scope, an expiry, and delegation limits. The private session key stays in the browser, preferably memory. Cloud stores public material and signed envelopes only. A WebAuthn authentication cookie is not runtime authority.

The session may authorize workflows within an Environment and named action families. It cannot extend its own expiry, widen its scope, or grant permanent keys. Machines verify the chain and request, including WebAuthn challenge, origin, RP identity, and user verification requirements. The browser need not expose this terminology.

A proposal describes one complete workflow and its effects. Approval binds its immutable digest. MCP gets an executable approved envelope, not the browser's session key. An unattended request with no applicable authorization returns an actionable approval-required result. Reads and analysis continue under existing access rules. Existing CLI-hosted MCP must not silently turn access to the CLI process into permission to use its Device key for arbitrary tool calls.

#### More than one Cluster per Organization

Not planned yet. The direction: an Organization later runs several Clusters, with a default Cluster and Environments assigned to one, such as production on a production Cluster. Approvals are per Cluster, and nothing crosses Clusters. This work must not block migrating there. It need not build it.

The Sealing public key that `ployz_core::authority` calls "Cluster identity" is stored once per Organization in Cloud (`dashboard/src/modules/machines/tables.ts:543`), so two future Clusters may share it. Every approval therefore also names something unique to its Cluster. Machine IDs are minted by each Machine and are unique, so binding the Machines that may act is enough. The restart approval in #3926 binds target Machine IDs, and a test proves another Machine refuses it. Join approvals bind the admitting Machine IDs. A draft proposal belongs to one Cluster. No current hard blocker is known. Whether one Device approval covers every Cluster or each Cluster approves its own is left to the multi-Cluster work.

#### Draft proposals (thread B)

Today each gated agent call makes its own Approval for one command on one subject. The agent calls the tool again with the approval ID to run it, and any Working State edit in the Environment supersedes a pending Deploy Approval (`dashboard/src/modules/approvals/approvals.server.ts:127`, `:251`, `agent-chat.server.ts:118`). An agent that deploys two Services and moves a Volume asks three times.

Decision: **an explicit draft that gated tools stage into.** `proposal_create` returns a draft ID. Any gated tool called with `proposal: <id>` stages its step into the draft instead of asking. It returns the step's preview and effects. `proposal_present` turns the draft into one Approval card and one prompt. A gated tool called without a draft keeps working as a one-step proposal, so every Approval becomes a proposal of one or more steps on one code path.

- **Staging freezes the step.** A Deploy step publishes and freezes its Saved revision and the effective payload digest that servers will check. An operation step freezes its preview and effects. Working State edits after staging don't supersede the step, because the step no longer depends on Working State.
- **One step per command and subject.** Staging the same command on the same subject again replaces the earlier step, so an agent can fix a mistake before presenting.
- **Supersession is per step.** A step goes stale only when the thing it froze can't run any more. Examples are a newer Saved revision for that Environment published outside the draft, or an operation whose fresh preview differs. Presenting or approving a draft with a stale step asks again for that draft. A partial approval is never granted.
- **Order is the staging order.** Servers enforce authorization per step. They can't enforce ordering across Machines without a barrier, which bet 6 forbids. So each step must be safe to run on its own once approved. A step that is only safe after another declares the dependency, and the Machine requires the earlier step's receipt. Volume Moves already do this with E0's handoff receipt. Anything else stays sequential under Cloud's driving as an outage risk, not an authority risk.
- **One signature covers the bundle.** Approval signs the ordered list of step digests, with Cluster, Environment, expiry and the bounded recovery allowance. Each Machine checks that the step it's asked to run is in a bundle a trusted key signed and that its payload matches.
- **Cloud runs the approved proposal.** This generalises today's `review.apply` path (`apply.server.ts:293`) instead of having the agent redeem each step. `proposal_status` lets the agent follow progress. Partial results are reported per step.

Rejected: an implicit draft per agent conversation, which hides state and has no stable identity across MCP clients. Also rejected: a single `propose` call that takes the whole step list at once. That one is stateless, but it forces the agent to know every change before it can preview any of them.

Proposed UX defaults need confirmation during B, not an architecture rewrite. Start with a short Environment-scoped session for ordinary deploy and lifecycle work. Keep destructive storage requests bound to the reviewed workflow. Do not silently interpret a restart approval as authority to delete a Volume.

### Automatic GitHub deploy and builds

A device-authorized automatic deployment rule binds immutable repository identity, branch, Environment, approved settings revision, workflow identity, build policy, and policy generation. Machines validate GitHub's raw signed OIDC proof against a fixed issuer and Cluster-specific audience. Cloud's successful validation result is not proof.

OIDC alone does not authenticate arbitrary checkout inputs or arbitrary output images. Bind the authorized commit, build configuration, actual builder identity, and resulting image digest. An approved workflow must establish that relationship independently of Cloud-supplied dispatch inputs. Build secret release must be restricted to the authorized build and holder key. Mutable tags, Cloud Build Receipts, Uploaded Source locations, and cache labels are not artifact authority.

The preferred design accepts a commit independently observed as the approved branch's current head at admission. This permits intentional force pushes. It rejects old proof for a commit the branch no longer names. Replay consumption, short proof lifetime, and per-Machine accepted observations supplement the head check. Run number, ancestry, webhook authenticity, or timestamp alone is insufficient.

Thread D must determine how a Machine obtains authenticated head evidence for private repositories without trusting np1. Options include direct GitHub access with appropriately held credentials, or an independent verifier with explicitly delegated narrow authority. Dependence on an np1 bearer proxy does not meet this requirement. Final proof construction is an unresolved blocking design choice, not an implementation detail to improvise.

A branch can advance immediately after a head observation. The promise is head-at-observation/admission with bounded freshness, not globally latest at every instant. Duplicate proof may resume its admitted operation but cannot create a new execution or restore a superseded settings revision. An intentionally force-pushed old commit can be a new authorized branch head. That is different from Cloud replaying old evidence.

Automatic deployment changes only the commit. Repo files may change application code, but must not silently revise protected build arguments, mounts, network configuration, settings, or secret eligibility. Resolve that distinction in the approved build policy. Config Mount bytes need an approved content digest or an explicitly authorized derivation from that commit.

### Previews and inherited secrets

The rule states which existing PR workflows qualify, which immutable repositories and GitHub identities are trusted, and which Sealed references may be inherited. Mutable Cloud membership rows cannot expand the rule. For an untrusted PR, the existing review path binds the exact approved head.

Sealed values are opened only for an authorized build or workload. Their existing associated-data scope needs explicit support for cross-Environment inheritance. Preview settings cannot widen production settings or copy all secrets by default. Existing flat mesh networking means an authorized preview is not automatically isolated from production network services. No new network isolation product is promised here.

### Existing automatic maintenance

Inventory current automatic effects such as ingress startup, certificate renewal, image pruning, preview cleanup, and Volume recovery. Each needs either a bounded standing authorization or an explicit consequence of an already authorized workflow. Daemon-owned infrastructure maintenance stays local and narrow. Broad Cloud maintenance exemptions are excluded. A repeated redeploy must not rerun hooks as fresh work without authority.

## GitHub App: broad permissions, key off np1

Decision: keep the App's broad permissions (code, workflows, repository settings) so the future coding agent never forces a re-request. Make them safe by taking the App private key off np1. The constraint is removing np1's ability to use the key. Narrowing the grant is not the approach.

- **Token minter.** The App private key lives in a cloud KMS key that cannot be exported. A small minter service outside np1, in a separate cloud account with its own deploy credentials, is the only thing that can sign App JWTs. np1 holds no App key.
- **Reads stay free.** np1 asks the minter for installation tokens and receives them narrowed at mint time to read permissions (`contents`, `metadata`, `pull_requests`, `checks`, `actions`: read) and the requested repositories. Cloud's existing reads keep working unchanged.
- **Today's writes are narrow operations, not tokens.** The minter performs `dispatch` of `ployz-build.yml` on a rule's branch, `cancel` of that workflow's runs, and Ployz check runs itself. It never hands np1 a token with write permissions.
- **Every other write needs customer authority.** A write token, or a write the minter performs, requires a request signed by a key the customer trusts, bound to the installation, repository, permissions, and expiry. The minter pins each installation's trusted keys on first link. Only an existing trusted key can change them, which is the same rule as customer servers. np1 cannot edit that list.
- **Agent writes don't deploy.** When the coding agent exists, it writes to `ployz/*` branches and opens pull requests. Automatic deploy only accepts the approved branch's head, so the agent's pushes never run on a server until a person merges in GitHub.
- **OAuth and webhooks.** The App's OAuth client secret and code exchange move to the minter, so np1 never receives user-to-server tokens. Cloud stops storing GitHub OAuth tokens in `account`. Webhooks keep going to Cloud. They inform Cloud and never authorize a server.
- **Our own org.** Remove the `getployz`, `getployz-dev`, and `getployz-staging` installs from the `getployz` org, or limit them to repositories that matter to nothing. Add `v*` tag rulesets and release reviewers, protect `getployz/build` `main`, pin customer workflows to a build action commit SHA, and move `CLOUDFLARE_API_TOKEN` into a protected environment. The minter must not deploy from any repository np1 can write. Then decide whether to rotate `MINISIGN_SECRET_KEY` and the Cloudflare token. See [the Y report](y-github-report.md).

Cost is one small service and one KMS key. Requests, token caching, and the minter's own key list are cheap. Residual: np1 can read every repository the App can read, withhold or cancel builds, forge Ployz checks, and uninstall the App. It cannot push code, edit workflows, change settings, or obtain a write token without a customer signature.

DNS follows the same pattern per [the X report](x-dns-report.md). Hosted DNS and its Route 53 and ACME credentials move off np1, and the service only changes a name's records or certificates on a request signed by that name's pinned customer key.

## Demonstrated paths and their owners

These are source-confirmed mechanisms. Success on a deployed Machine still depends on its version, eligibility, network, and runtime configuration.

| Path from np1 | Evidence at audited source | Owner |
| --- | --- | --- |
| Operator chooses container image and host-sensitive Docker fields | `core/crates/ployz-core/src/rpc_catalog.rs:73`, `core/crates/ployzd/src/machine_api/local.rs:469`, `core/crates/ployzd/src/docker/create.rs:157` and `:401` | A2 admission, C effective payload, F coverage |
| Volume driver options bypass a shallow bind-mount denylist | `core/crates/ployz-core/src/domain/volume.rs:364`, `core/crates/ployzd/src/docker/volume.rs:155` | C |
| Register wraps the Cluster Sealing Key to an attacker-chosen Machine key | `core/crates/ployzd/src/machine/local_machine.rs:751` and `:780` | A1 |
| Registered mesh peer reaches API without application authority and can become Full | `core/crates/ployzd/src/network.rs:187`, `core/crates/ployzd/src/machine_api/mod.rs:176`, `core/crates/ployzd/src/machine/local_machine/management_client.rs:78` | A1, A2 |
| Cloud-provided Devices seed a joining Machine | `core/crates/ployz/src/handlers/cloud.rs:434`, `core/crates/ployzd/src/machine/store.rs:480` | A1 |
| Replacement code or a forged Build Grant opens valid Sealed values | `core/crates/ployzd/src/sealed_env.rs:56`, `core/crates/ployzd/src/management/build_grant.rs:111` and `:480` | C, D |
| Ingress validation accepts caller-selected image with certificate mounts | `core/crates/ployz-core/src/domain/ingress.rs:75`, `core/crates/ployzd/src/ingress.rs:1144` | C |
| Cloud controls Volume operations and the Move commit decision | `dashboard/src/modules/volume-run/volume-run.server.ts:1333`, `dashboard/src/modules/volume-run/volume-run.inngest.ts:680` | E |
| Mesh access reaches a bulk Volume stream | `core/crates/ployzd/src/volume_send.rs:52` | E recipient-bound transfer, A1 membership |
| Cloud-held approvals and sessions can be rewritten | `dashboard/CONTEXT.md:228`, `dashboard/src/server/auth.server.ts:268` | B, with A2 enforcement |
| Cloud selects a forward signed release outside UI channel choice | `core/crates/ployzd/src/installer/release.rs:132`, `dashboard/src/modules/server-upgrade/server-upgrade.server.ts:253` | F |

Direct unsigned release replacement and Cloud-origin downgrade are already blocked by signature/hash verification and downgrade checks. Preserve these protections. Release signing key access from np1 is not established. See `.github/workflows/release.yml:63`, `core/crates/ployzd/src/installer/release.rs:234`, and `core/crates/ployzd/src/installer/upgrade.rs:205`. A successful host takeover through another path defeats those local protections.

## Remaining choices and their decision gates

| Decision | Recommended direction | Owner and deadline |
| --- | --- | --- |
| First authority and join identity UX | Trusted CLI/local administration establishes authority, existing trusted key authorizes each join | A1 before C1 |
| Local recovery details | Existing Device, recovery-code possession, or direct root only. Explicitly rotate compromised authority | A1 before C1 |
| Workflow crash records and incompatible transitions | Durable local consumption plus participant receipts. Prototype hostile-driver Move before freezing E schema | A2 with E design input before C1 |
| Session duration and exact action families | Short Environment scope, explicit destructive workflow approval, no silent scope growth | B before UX acceptance |
| Private-repo head and build proof | Independently authenticated evidence bound to challenge, source, builder, settings, and output | D design checkpoint C2, informed by Y |
| Cache trust | Treat Cloud-writable caches as untrusted until verified. Disable an unsafe cache before building a new trust service | C before C2 |
| Preview identity and inheritance representation | Preserve existing eligibility with authenticated GitHub facts and explicit selected secret references | D before C3 |
| Expiry and clock failures | Define skew, persistence, rollback detection, and fail-closed admission. Do not use time as a Volume safety fence | A2 before C1 |
| Revocation during partitions | Per-Machine acknowledgement, direct delivery, bounded expiry. No instantaneous Cluster-wide revocation promise | A2 before C1 |
| Stable promise conflict | Record a deliberate 0.4 security exception or additive explicit enforcement activation with unsigned route refusal | F proposal at C1, resolved before release |
| Upgrade approval frequency | One approval per upgrade, or a standing customer-approved channel the Machine enforces. Cloud must not pick outside it either way | F before C4 |
| Automatic maintenance authority | Inventory each effect. Bounded standing authorization or consequence of an approved workflow. No broad Cloud exemption | F inventory before C1 |
| DNS and GitHub separation | Decide from X and Y's evidence. No credential relocation without independent enforcement | X and Y reports before external implementation proposal |

`core/DESIGN.md` promises compatibility across later 0.x releases. Calling this 0.4 does not waive that promise. The chosen coordinated security cutover needs an explicit, narrow argument and documentation change. The plan does not build a permanent legacy dispatcher.

## Bets and explicit tensions

Machine-local authority and Cloud drives, never owns force effect checks and local consumption state into Machines. Client-first, daemon when forced permits this security boundary while keeping planning in clients. Bounded imperative commands keeps approved payload records as authorization evidence, not a new desired-state reconciler. Partial results are outcomes requires per-Machine completion and refusal reports.

Observer-relative truth and AP over C prohibit invented global freshness, revocation, or exactly-once promises. A participant receipt can authorize its own local transition. Missing evidence can block that operation without introducing a Cluster quorum. Any proposal to make a Machine decide another Machine's state must argue the Machine-local authority red flag.

Lean on proven primitives favors standard signature, WebAuthn, OIDC, and hash verification libraries. Stable promise is the explicit 0.4 exception above. Evidence over claims means the UI cannot announce protection for unreachable or unacknowledged Machines.

## What remains after the complete server track

Root on np1 can deny Cloud service, suppress authorized work, forge Cloud displays, read Cloud-visible information, and exercise any remaining external credentials. It can use a compromised dashboard's live delegated authority within the accepted scope. Authorized application code can disclose its own secrets. Expired authority does not undo past data theft or persistent effects.

It cannot, solely by editing Cloud state, add trusted Devices or Machines, obtain the Sealing Key, introduce a new executable or settings payload, start/stop/restart a workload, export a Volume, approve a destructive workflow, or select an unauthorized upgrade on an enforced Machine. Each claim requires the corresponding acceptance evidence in the plan.

This does not repair an already compromised customer Machine. Rotate exposed external credentials and Sealing Keys, and reprovision where trust was lost. Deliver migration guidance without claiming that upgrading erases previous compromise.
