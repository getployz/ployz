# Next steps

This replaces the thread structure in [plan.md](plan.md) and [kickoffs.md](kickoffs.md). The goal, decisions, and evidence in [decisions.md](decisions.md) still hold. Work happens in one coordinating thread that delegates each step to subagents.

## What already exists

Ployz already has most of the trust root this needs. A **Device** is a person's computer whose key every Machine admits at Full Scope. Only a Full caller can add or remove one, and the first Device is approved over SSH on a server (`docs/user/servers/manage-servers.md#approve-a-device`, `core/CONTEXT.md` "Device"). Cloud can relay a Device's request but cannot approve one.

The gap is the other tier. The `cloud` and `cli-<id>` slots are **Operator**, and Operator can do almost everything: deploy any image, change settings, start, stop, move Volumes, enroll Machines, and choose upgrades. Root on np1 holds the `cloud` slot. So the work is not a new key system. It is two changes:

1. Close the holes that let Cloud create trust it should not have: a joining Machine takes its Devices from Cloud's registration (`ployz/src/handlers/cloud.rs:434`, `ployzd/src/machine/store.rs:480`), Register wraps the Sealing Key to whatever Machine key it is given (`ployzd/src/machine/local_machine.rs:751`, `:780`), and a mesh peer can reach the API as Full (`ployzd/src/machine_api/mod.rs:187`).
2. Require a Device signature for every change an Operator sends. Cloud keeps relaying requests. A Machine runs a change only if a Device it already trusts signed an approval that matches it.

## The steps

Each step lands as a small PR, proven on real Machines with `verify-server` or in the browser with `verify-dashboard`. Enforcement stays off until step 4, so nothing breaks for customers in between.

| # | Step | What it closes | What users notice |
| --- | --- | --- | --- |
| 1 | **Signed approvals.** A Device-signed approval (operation, Environment, targets, payload digest, expiry) and the Machine-side check against the Machine's own Device list. Reuse thread A's partial `ployz-core/src/authority` envelope and canonical encoding. Not enforced yet. | Nothing yet. It is the foundation. | Nothing. |
| 2 | **The browser becomes a Device.** A passkey registers as a Device through the existing approve flow. A passkey prompt opens a short browser session key that signs approvals. Dashboard buttons sign through one shared helper. | Lets dashboard users approve without the CLI. | Each browser is approved once, like `ployz device approve` today. One passkey prompt per session after that. |
| 3 | **Joins need a Device.** Register, Enroll, ShareSealingKey, and a joining Machine's initial Devices all require a Device-signed join approval. A mesh peer no longer counts as Full. | np1 adding its own Machine or Device and receiving the Sealing Key. | Adding a server asks for one approval. |
| 4 | **Deploys and lifecycle need a Device.** Deploy, settings, start, stop, and restart carry a signed approval bound to the full effective payload: image digest, command, mounts, Config Mount hashes, privileges, routes, and hooks. Ingress image and Volume driver options are included. Turn on enforcement per Cluster. | np1 running its own image or settings, and so reading Sealed values through a container. np1 stopping or restarting workloads. | CLI with a Device: nothing. Dashboard: buttons work inside a session. |
| 5 | **GitHub auto-deploy rule.** A Device signs a rule once (repository, branch, Environment, settings revision, build policy). The Machine checks GitHub's OIDC token itself and accepts only the branch head, so force pushes work. The build output digest is bound to that commit. | np1 swapping source, settings, artifact, or using stale proof. | Approve once per branch. Pushes deploy with no prompt. |
| 6 | **Volumes and upgrades.** One approval per Volume workflow, using E0's source-decides handoff ([e0-move-report.md](e0-move-report.md)). Upgrades only to a release the customer chose. | np1 copying a Volume to its own Machine, or choosing an upgrade. | One approval per Move, copy, or restore. |
| 7 | **Take keys off np1.** A GitHub token minter with the App key in KMS, and Hosted DNS with its Route 53 and ACME credentials, both outside np1. Each signs or writes only what a customer key approved. See decisions.md, "GitHub App: broad permissions, key off np1". | np1 pushing to customer repos, redirecting generated domains, or getting their certificates. | Nothing. |

Steps 1 and 7 can run in parallel. Step 2 needs step 1. Steps 3 to 6 need steps 1 and 2. Step 5 needs a design check first. A Machine must confirm a private repository's branch head without trusting np1. Section 6 of [y-github-report.md](y-github-report.md) lists the options.

## Decisions needed from Nick

1. **First trust for dashboard-only customers.** Today the first Device needs SSH on a server. A customer who only uses the dashboard would have to do that once before enforcement turns on. The alternative: the install command prints a short code, and the user checks it matches the code their browser shows. np1 writes that command, so the comparison is the only thing stopping a swapped key. Recommendation: keep SSH approval as the rule and offer the code comparison during first server install.
2. **Session length.** Recommendation: 15 minutes per Environment for deploy, settings, start, stop, and restart. Volume workflows and other destructive actions are approved one at a time.
3. **The 0.x compatibility promise.** `core/DESIGN.md` promises daemons keep working across 0.x. Turning on enforcement makes Operator-only callers stop working on enforced Clusters. Recommendation: enforcement is something a customer turns on per Cluster with a Device, which is additive. Document the change in `DESIGN.md`.

## Start here

Step 1, signed approvals, delegated to one subagent. The work:

- Finish the envelope in `ployz-core/src/authority`. Thread A's uncommitted work is in the `t3/sec04-a-authority` worktree.
- Have the CLI sign with its Device key.
- Add a daemon check that verifies against the Machine's Device list and records consumed approvals so they survive a restart.
- Prove on Boat that a valid approval verifies and that tampering with any field, replaying, or using an expired approval fails.

Behaviour does not change until step 4.
