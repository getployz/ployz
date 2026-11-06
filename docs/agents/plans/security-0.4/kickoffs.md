# Ployz 0.4 security thread kickoffs

Each section below is a prompt to start one top-level thread. None has been started. Start them in the order and parallelism set by [the plan](plan.md). Each prompt assumes the thread starts on a branch that contains `docs/agents/plans/security-0.4/`. Commit that directory first, or the threads cannot read it.

Paste the common brief, then the thread's own section, as one prompt.

## Common brief

> You own one thread of the Ployz 0.4 security work. The goal: root on np1, the server running Ployz Cloud, can still cause an outage and read what Cloud already sees, but cannot run new code or settings on a customer Machine, start, stop or restart a workload, open Sealed values, take a Volume, add a Machine or Device, or pick an upgrade. Customer Machines enforce this with keys the customer holds. Customers keep ordinary buttons, native authentication prompts and automatic GitHub deploys.
>
> Read these before you change anything: `docs/agents/plans/security-0.4/decisions.md` (the promise, settled choices, shared contracts and demonstrated attack paths), `docs/agents/plans/security-0.4/plan.md` (your workstream, checkpoints, dependencies and acceptance), `core/DESIGN.md`, `core/CONTEXT.md`, `dashboard/CONTEXT.md` when you touch Cloud, `core/CODING_STANDARDS.md` when you touch core, and `docs/agents/landing.md`.
>
> Rules that hold for every thread:
> - Settled choices in `decisions.md` are not yours to reopen. If evidence shows one cannot work, stop and report the evidence.
> - Stay inside your workstream's code boundaries in `plan.md`. If you need a change outside them, write it down and send it to the owning thread through the operator. Do not make it yourself.
> - Contract types belong to thread A. After checkpoint C1, consume them and do not edit them.
> - Plain variables stay plain. No new default sealing.
> - No speculative agent features, no new repository-read restrictions, no broad automation-policy product.
> - No permanent mixed-version dispatch. The enforcement switch is per Cluster and temporary until cutover.
> - Each PR body names every DESIGN bet it touches and the choice it changed. If you cross a red flag, argue it there.
> - Run focused local checks only (typecheck, lint, fmt, tests for code you changed). GitHub CI is the gate. Prove Machine behaviour with the `verify-server` skill and dashboard UI with the `verify-dashboard` skill, as `plan.md` lists for your workstream.
> - A bug outside your task: score frequency and severity per `AGENTS.md` and fix it only if it meets that bar, otherwise file an issue.
> - Never print secret values. Report key locations and scope only.
> - Do not run releases. They are Nick's.

## A. Trust roots and workflow authorization

> You own thread A. You define the shared contracts every other thread consumes, then implement Machine-side trust and admission.
>
> First deliverable, which gates checkpoint C1. Write the contracts as Rust types in a new authority module in `core/crates/ployz-core`, with generated TypeScript in `ployz-sdk`, canonical encoding, refusal shapes and golden examples. Cover: trusted key records and their signed generations, first-key install, join authorization binding Machine identity, recipient key and allocation, the workflow authorization envelope with every field `decisions.md` lists under "Workflow authorization", the local consumption store, expiry and clock-failure rules, and revocation with per-Machine acknowledgement. Close the five open decisions you own in `decisions.md` (first authority and join UX, local recovery, crash records and incompatible transitions, expiry and clock, revocation) by editing that file with the chosen answer and its reason. Fold in thread E0's hostile-Move result before you freeze Volume transitions. C1 closes when B, C, D, E and F have each read the contract and recorded "consumable" or a concrete gap, and you have fixed every gap.
>
> Then implement, as small PRs that each end in a check:
> 1. Trusted key storage and chain verification in `ployzd/src/machine/store.rs` and the authority module. A replicated key row counts only after its chain verifies locally.
> 2. First-key install and Device add in the `ployz` CLI, over local administration or verified SSH.
> 3. Register, Enroll, ShareSealingKey and initial Devices gated on join authorization (`ployzd/src/machine/local_machine.rs:751`, `:780`, `ployz/src/handlers/cloud.rs:434`, `ployzd/src/machine/store.rs:480`).
> 4. The mesh path no longer infers Full authority (`ployzd/src/network.rs:187`, `machine_api/mod.rs:176`, `machine/local_machine/management_client.rs:78`).
> 5. The dispatch-level authorization hook in `machine_api` with the local consumption store. Every mutating catalog row declares its workflow kind. Other threads add payload checks in their own handlers.
> 6. Recovery with an existing Device, the recovery code, or local root, plus key rotation.
>
> Acceptance is in `plan.md` under workstream A. Include the `verify-server` attack angles: Register with an attacker key, an injected Device row, a mesh peer claiming Full, replay after restart, and expiry mid-step. Stop when C1 has closed and your PRs are merged or queued with evidence. Report the contract location, each closed decision and residual risks.

## E0. Hostile-driver Move sketch

> You own thread E0, a throwaway prototype. Do not open a PR. Your result feeds thread A's contract before C1.
>
> Question: when Cloud is the driver of a Volume Move and is malicious, what local records and participant receipts must each Machine require so the driver cannot get both incompatible outcomes from one approved Move? Examples are two Writers, data sent to an unapproved recipient, or both sides committed. The answer must keep bet 6 (Machine-local authority), use no clock-based safety wait, and allow bounded recovery after a crash.
>
> Read `core/crates/ployz-verify` (the Stateright Volume Runs model and the `verify-volume-runs` skill), `ployzd/src/volume_send.rs`, `machine_api/local_volume_copy.rs`, `dashboard/src/modules/volume-run/volume-run.inngest.ts:680` and `volume-run.server.ts:1333`. Sketch two or three candidate receipt designs in a scratch copy of the model with a hostile driver. Report which design leaves no bad reachable state, the exact fields each receipt binds, which records each Machine persists, and where the design leans on another Machine's signed receipt so that E can argue the bet 6 red flag. Stop at that report.

## X. DNS authority investigation

> You own thread X, a read-only investigation. Change no runtime files. Mutate no domains or services.
>
> Read `docs/agents/plans/security-0.4/external-authority.md` section X in full. It gives the starting evidence and the deliverable. Inventory every reserve, sync, renew, certificate and release caller in `dashboard/src/modules/cluster-domain` and anything else that calls Hosted DNS. Inspect the Hosted DNS service implementation and its deployed grants only if authorized read-only access exists. Otherwise record each as unknown. Treat `getployz/ops` as unknown unless you can read it.
>
> Return: a credential-to-endpoint matrix saying exactly what np1 can read or change, a generated versus custom domain matrix, token and key rotation needs, and a recommendation among keeping the boundary, narrowing the API, or separating authority. A separation proposal must show a concrete rejected request for an arbitrary address, an unauthorized retirement and an arbitrary CSR key, even with np1 holding every Cloud credential. Moving a credential to another server is not containment if np1 can still ask that server for the same effects. Mark each claim as source-confirmed, deployed, fixture-only or unknown. Write the report to `docs/agents/plans/security-0.4/x-dns-report.md` and stop.

## Y. GitHub authority investigation

> You own thread Y, a read-only investigation. Change no runtime files. Dispatch no workflows and push no commits.
>
> Read `docs/agents/plans/security-0.4/external-authority.md` section Y in full. Inventory the App key, installation token minting, OAuth tokens and webhook secret: where each is stored, actual versus documented permissions, installation reach, token export, and every current caller in `dashboard/src/modules/github`, `modules/config-store/store-github*`, `modules/identity` and preview code. Inspect `.github/workflows/ployz-build.yml` and the external `getployz/build` action where you can read it, for checkout inputs and output provenance. Treat production grants and `getployz/ops` as unknown unless you can read them.
>
> Answer for thread D: how a customer Machine can get authenticated head evidence for a private repository without trusting np1; what GitHub's OIDC claims actually bind (repository ID, workflow ref, job workflow ref, run attempt, SHA, ref) and how reruns and force pushes appear in them; and how source can be bound to the produced image digest. Give concrete options with what each trusts.
>
> Return current read and write reach and a recommendation among retaining, narrowing or separating authority. A broker proposal must not export unrestricted installation tokens or proxy GitHub freely, and must show rejected requests for the mutations it claims to protect. Existing repository reads stay allowed. Mark each claim as source-confirmed, deployed, fixture-only or unknown. Write the report to `docs/agents/plans/security-0.4/y-github-report.md` and stop.

## B. Browser approval sessions, proposals and MCP

> You own thread B. Before C1, build only a throwaway UX sketch of the session flow. Do not open PRs. After C1, implement.
>
> Outcome: a dashboard button that needs authority ensures an Environment-scoped browser session. If none is live, the browser's native passkey prompt appears once. The passkey assertion authorizes an ephemeral key held in browser memory, with a scope, an expiry and delegation limits. Later buttons sign workflow envelopes with that key and send no prompt. Expiry or an out-of-scope action prompts again. Destructive storage actions keep their existing loss confirmation. MCP proposals use the existing Approval card. One prompt approves the proposal's full workflow and produces an executable envelope. MCP never gets the session key. Organization Tokens and MCP identity cannot approve their own proposals. CLI-hosted MCP must not turn access to the CLI process into free use of the Device key. No UI text says "sign" or "signature".
>
> Close the open decision on session length and action families before UX acceptance. Write the choice and its reason into `decisions.md`.
>
> Boundaries: `dashboard/src/server/auth*.ts` passkey code and its client, `modules/approvals`, `modules/agent`, `modules/devices`, one shared helper for action buttons, `ployz mcp` in the `ployz` crate, and session chain verification in the core module thread A names. Cloud stores public material and signed envelopes only.
>
> Closes: `dashboard/CONTEXT.md:228`, `dashboard/src/server/auth.server.ts:268`. Acceptance is in `plan.md` under B. That includes `verify-dashboard` with `SERVERS=1` and recordings of one prompt, several actions, expiry, out-of-scope and MCP approval. It also includes `verify-server` showing a database-forged "approved" row refused and a cross-Environment action refused. Stop when merged or queued with evidence. Report residual risk, including the accepted malicious-dashboard window.

## C. Deploy and lifecycle effective payload

> You own thread C. Before C1, write the payload inventory into `docs/agents/plans/security-0.4/c-inventory.md`: every field that reaches Docker, ingress, hooks, mounts and Sealed open, marked approved, derived or Machine-local. Also close the cache-trust decision for C2. The recommendation is to disable any Cloud-writable cache unless its content is verified against an approved digest. Record the decision in `decisions.md`. After C1, implement.
>
> Outcome: deploys, settings changes, start, stop and restart work as today from the dashboard, CLI and approved MCP proposals. Each Machine checks the full effective payload at the handler that calls Docker, against the approved digest. Sealed open is bound to that digest. Config Mount bytes must match an approved content hash. Redeploying an approved revision is idempotent and reruns no hooks as fresh work. Plain variables stay plain.
>
> Closes: `ployz-core/src/rpc_catalog.rs:73`, `ployzd/src/machine_api/local.rs:469`, `ployzd/src/docker/create.rs:157` and `:401`, `ployz-core/src/domain/volume.rs:364`, `ployzd/src/docker/volume.rs:155`, `ployz-core/src/domain/ingress.rs:75`, `ployzd/src/ingress.rs:1144`, `ployzd/src/sealed_env.rs:56`.
>
> Boundaries: `ployzd/src/docker`, `ployzd/src/ingress.rs`, `ployzd/src/sealed_env.rs`, `ployz-core/src/domain`, `ployz` deploy handlers, and Cloud `modules/service-act`, `modules/runtime`, the non-GitHub parts of `modules/config-store`, and `modules/variables`. Build Grants and GitHub builds belong to D. Volume Runs belong to E.
>
> Acceptance is in `plan.md` under C. It includes single-field-change refusal tests and a `verify-server` swarm covering normal lifecycle, Cloud-credential attacks (image swap, privileged flag, driver-option bind, ingress image, Config Mount bytes) and daemon kill mid-deploy. Stop when merged or queued with evidence.

## D. Automatic GitHub deploy, builds and previews

> You own thread D. Design first. Do not implement until checkpoint C2 closes. Do not implement preview inheritance until C3 closes.
>
> Inputs: thread A's contract after C1, thread Y's report at `docs/agents/plans/security-0.4/y-github-report.md`, and thread C's cache decision in `decisions.md`.
>
> C2 design: a device-authorized rule binds immutable repository ID, branch, Environment, approved settings revision, workflow identity, build policy and policy generation. A Machine verifies GitHub's raw OIDC token against a fixed issuer and a Cluster-specific audience. It accepts a commit only if it independently sees that commit as the approved branch's head at admission. That allows intentional force pushes and rejects stale proof. It binds source, builder and build policy to the image digest it runs. Build secrets are released only to the authorized build and holder key. Name the authority source for every input. Show a rejected example for substituted source, substituted settings, substituted artifact, stale proof and replayed proof. If no design meets this without trusting np1, stop and report. Do not build a weaker fallback. Write the design to `docs/agents/plans/security-0.4/d-build-proof.md` and update `decisions.md`.
>
> C3 design: preview rules name eligible PR workflows, trusted immutable repositories and GitHub identities, and the Sealed references a preview may inherit. The Machine resolves or rewraps them without plaintext reaching Cloud. Mutable Cloud membership rows cannot widen the rule. Untrusted PRs keep the existing review path bound to the exact head.
>
> Closes: `ployzd/src/management/build_grant.rs:111` and `:480`, `dashboard/src/modules/github/github-build.server.ts:74`, `dashboard/src/modules/config-store/store-github-builds.server.ts:229`, and settings riding along with auto-deploys.
>
> Boundaries: `build_grant.rs`, `ployz-build`, an OIDC verifier next to A's authority module, `.github/workflows/ployz-build.yml`, Cloud `modules/github`, `modules/config-store/store-github*` and preview code.
>
> Acceptance is in `plan.md` under D. It needs a real isolated GitHub repository and installation, because `verify-dashboard` fakes SHAs and OIDC. Prove push deploy, force push deploy, stale-proof refusal, Cloud substitution refusal and preview inheritance of only the named secret. Stop when merged or queued with evidence.

## E. Volume Runs

> You own thread E. Start after C1. Thread E0's result is part of A's contract.
>
> Outcome: Move, copy, mirror and restore each take one approval of the reviewed workflow, plus the existing loss confirmation where data disappears. The approval covers planned stops, transfer, switch, restart, permitted retries and bounded recovery. The Volume stream is recipient-bound to the approved target. Each participant requires the receipts for its own transition, so a hostile driver cannot get two Writers, an unapproved recipient, or both sides committed. Admission expiry stops new work. Bounded completion finishes work already admitted. The UI shows the difference.
>
> Closes: `dashboard/src/modules/volume-run/volume-run.server.ts:1333`, `volume-run.inngest.ts:680`, `ployzd/src/volume_send.rs:52`.
>
> Boundaries: `ployzd/src/volume_send.rs`, `machine_api/local_volume_copy.rs`, `ployz-core` volume copy planning, the `ployz-verify` model, and Cloud `modules/volume-run`, `modules/volume-copies`, `modules/volume-placement`. Argue the bet 6 red flag in each PR that makes a Machine act on a receipt from another Machine.
>
> Acceptance is in `plan.md` under E. It includes the `verify-volume-runs` model with a hostile driver, plus a `verify-server` swarm covering normal runs with a daemon kill, Cloud-credential recipient and commit attacks, and expiry mid-run. Stop when merged or queued with evidence.

## F. Coverage, upgrades, maintenance and cutover

> You own thread F. Start the coverage audit and the stable-promise proposal now, because both feed C1. Implement after C1.
>
> Before C1, deliver three things. (1) A table of every mutating Machine RPC catalog row in `ployz-core/src/rpc_catalog.rs` and every Cloud caller, with its workflow kind and owning thread. Any row no thread owns is a gap you raise with A. (2) An inventory of automatic effects (ingress startup, certificate renewal, image pruning, preview cleanup, Volume recovery, and any others you find). Each gets either a bounded standing authorization or a statement that it follows from an approved workflow. Daemon-local maintenance stays local and narrow. (3) A written argument for how 0.4 meets `core/DESIGN.md`'s stable promise: a narrow, documented security exception, or additive enforcement activation where an enforced Machine refuses unsigned mutating routes by capability name. Write these to `docs/agents/plans/security-0.4/f-coverage.md`.
>
> After C1, implement: a catalog coverage test that fails on any mutating row without an authorization class; upgrade selection bound to customer authority (`ployzd/src/installer/release.rs:132`, `dashboard/src/modules/server-upgrade/server-upgrade.server.ts:253`), keeping today's signature and downgrade checks (`installer/release.rs:234`, `installer/upgrade.rs:205`); the per-Cluster enforcement switch turned on by customer authority; deletion of the unenforced Cloud paths before release; the `core/DESIGN.md` and `docs/user/` updates. Close the open decision between per-upgrade approval and a standing channel choice by C4.
>
> Own C5: on a `verify-server` Boat Cluster built on the latest 0.3 release, upgrade to the 0.4 candidate, enable enforcement, run normal workflows, and show old unsigned mutating routes refused with `details.capability`. Do not run the release. Stop when C5 evidence is ready for Nick.

## R. Red team at C4

> You own thread R, a read-only attack on the integrated 0.4 candidate. Start only when checkpoint C4 has closed. Change no code.
>
> Boot a `verify-server` Boat Cluster with enforcement on. Act as root on np1: you hold the Cloud database, every Cloud environment secret, the dashboard JavaScript you serve, Cloud's emails and Cloud's Machine connections. You do not hold customer Machine root, customer devices, passkeys, Device keys, customer GitHub accounts or the release signing key.
>
> Try to run your own code or settings, read Sealed values, take a Volume, hijack traffic, ship a binary, start, stop or restart a workload, or add a Machine or Device. Hit every seam in `decisions.md`: first key and joining Machines, OIDC claims and audience, replay, rollback, previews, settings riding along, builds and caches, registry images, Config Mounts, the browser session window beyond its accepted scope, recovery, staff access and the release pipeline.
>
> Report only attacks that work against the build, with the commands you ran, the end state, the owning thread, and a suggested fix. Separately, list what the accepted risks still allow, so the release notes can state it. Stop at the report.
