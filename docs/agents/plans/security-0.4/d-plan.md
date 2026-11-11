# Security 0.4 thread D plan, automatic GitHub deploy, builds and previews

A compromised Cloud must not run code of its choosing on a Server or read its secrets. Today it can do both through builds. Under Device enforcement, `Build` and `MintBuildGrant` are not gated (#4002). Cloud can start a Build whose `RUN` steps it wrote, with the Build's Sealed values opened, or mint a Build Grant for its own key and read any Service's Sealed values opened. Slice D1 closes that first. The later slices replace the blanket refusal with a proof each Machine checks for itself.

The design is in [d-build-proof.md](d-build-proof.md). A person approves an automatic deploy rule once. A pinned GitHub builder attests, through the audience of GitHub-signed OIDC tokens, which commit it checked out, which recipient gets the build secrets, and which image it built under which policy. Each Machine checks the token, the signed rule, the commit's authority and the branch head it reads from GitHub itself before any secret opens. Secrets reach the builder sealed from a key only Cluster Machines can derive. [d-inventory.md](d-inventory.md) lists every input Cloud chooses today and the slice that closes it.

Seven PRs, in dependency order:

| Slice | Outcome | Closes |
| --- | --- | --- |
| D1 | Enforcing Machines refuse Cloud's `Build` and `MintBuildGrant` | #4002, today's bar crossing |
| D2 | Machines can verify a GitHub OIDC token and read a branch head. Rule types exist. | Nothing yet. It is the verifier. |
| D3 | A push builds under enforcement. The grant is bound to a verified token and the commit checked out, and secrets reach only the builder. | Cloud picking the commit, inputs, builder, holder or secrets endpoint |
| D4 | A push deploys under enforcement with no prompt | Cloud substituting source, settings, artifact, stale or replayed proof |
| D5 | The dashboard approves and re-approves rules | Settings riding along shows as "approve again", not as a silent deploy |
| D6 | A person can deploy or roll back a built image under enforcement | Built-image deploys refused since thread C |
| D7 | Trusted previews deploy into a Namespace and hostnames the Machine derives, and inherit only the Sealed values a rule names | Trust from Cloud rows, Cloud-chosen preview Namespaces, preview secrets kept apart by Cloud alone (checkpoint C3) |

## Operator decisions

The operator took every recommendation on 2026-11-09, with the additions below. Nothing here blocks D1 or D2.

- Decision 1: the minter becomes its own slice after D4, run by the program's root. Until it is live, the docs suggest the branch ruleset.
- Decision 8: the root applies Y's O1 to `getployz/build`.
- Decision 9: an agent builds the Appendix B environment.
- Decision 11: accepted on 2026-11-10. Previews take the derived Namespace and hostnames.
- Decision 12: accepted on 2026-11-10. A `BuildApproval` takes the same signers as a thread C deploy approval, a browser session included.

1. **Automatic deploy under enforcement before the GitHub minter is live.** While Cloud holds the App key, or any GitHub token with Contents or Workflows write, it can push its own commit to the approved branch, and no Machine check can tell. The minter decision in the program's decision record removes that, but no thread owns building it. *Recommendation.* Build D1 to D4 now. Name an owner for the minter. Until it is live, the docs say automatic deploy under enforcement resists everything except a pushed commit, and suggest a branch ruleset that blocks direct pushes. Thread R tests D4 with Cloud's write credentials removed.
2. **Who may sign a rule.** *Recommendation.* A Device, or a passkey through its own prompt. A browser key under a session grant may not, because a rule is standing authority over every future push.
3. **Rules name Machines.** A Machine that joins later runs no rule deploys until the rule is approved again. The alternative is a Cluster identity unique per Cluster, which does not exist yet. *Recommendation.* Name Machines for 0.4, as thread C does, and have `ployz server add` print the rules that need approving again.
4. **The Servers builder and Uploaded Source under enforcement.** After D1, Cloud cannot start a `Build` on an enforcing Machine. Builds on Servers and CLI deploys of a local directory through Cloud stop there. Making them safe needs the Machine to fetch source itself and a second attester. *Recommendation.* Leave them out of 0.4. GitHub builds are the enforced path. A Full caller (local socket or a Device's own connection) still builds.
5. **Person-approved built deploys (D6).** Without D6, a built Service under enforcement deploys only through a rule. A manual deploy of an older commit, or a rollback, does not. *Recommendation.* Keep D6 in 0.4. It is small after D4.
6. **Reviewing an untrusted pull request.** A review signed under a session lets a compromised dashboard approve any commit of the repository in that preview Environment. *Recommendation.* A session covers reviews of previews that inherit no Sealed values. A preview that inherits values needs a passkey prompt for each review.
7. **The GitHub Actions cache in rule builds.** *Recommendation.* Keep it, scoped by the policy digest. If D3's lane shows another branch can write that scope, turn it off for rule builds.
8. **The builder repository.** The reusable workflow lives in `getployz/build`. Thread Y found that repository has an unprotected `main` and no tag rulesets, and that Cloud's own App installations can write to it. *Recommendation.* Apply Y's O1 to the `getployz` organization before D3 merges. Rules pin the builder by commit either way, so this guards releases of the builder, not the proof.
9. **A real GitHub environment for live lanes.** `verify-dashboard` fakes SHAs and OIDC, and no isolated real setup exists in this repository, its skills or its docs. *Recommendation.* Create the one in Appendix B before D2's live lanes.
10. **Moving customers to the new workflow file.** A rule needs `on: push` and the pinned reusable workflow. *Recommendation.* The dashboard's existing "Add workflow" link opens GitHub's editor with the new file. The old file keeps working on Clusters that do not enforce, until thread F's cutover removes the old path.

Added after Codex's review on 2026-11-09. The operator accepted both recommendations on 2026-11-10.

11. **Preview Namespaces and URLs change shape under enforcement.** To stop a preview proof from landing in a production Namespace, the Machine derives a preview's Namespace (`pv-<rule>-<branch or PR hash>`) and its hostnames (`<label>.<Namespace>.<preview domain>`). Today a PR Environment gets `{project}-pr-N` and its hostnames from the Organization's generated domain. Existing PR Environments keep their old Namespace, which is fixed at first admission, so they need closing and reopening once a preview rule exists. *Recommendation.* Accept the derived names. They are long but stable, and the preview's page shows its URL. A shorter readable form would need a name the Machine can check, and Cloud chooses every readable name today.
12. **Who may sign a `BuildApproval` (D6).** It authorizes a grant, and so build secrets, for a commit that is not the head. The Machine accepts only commits on the approved branch's history, read from GitHub. *Recommendation.* The same signers as a thread C deploy approval, a browser session included. A compromised dashboard could then rebuild an old commit from the approved branch with today's build secrets. That is code someone with push rights already put on the branch, and the deploy still needs a person's approval of the image.

## How to read this

Same conventions as `c-plan.md`. One box is one unit of work, and every box names the evidence that checks it. Check a box only when that evidence exists. Tests alone are not sufficient verification. A PR is verified only when its unit, live and perf boxes are all checked.

Terms follow `core/CONTEXT.md` (Build, Build Grant, Build Receipt, Uploaded Source, Deploy, Attempt Target, Requested Service Spec) and `dashboard/CONTEXT.md` (PR Environment, Approval card). "Rule", "preview rule", "template digest", "policy digest" and "build claim" are new and defined in `d-build-proof.md`. No log, test, fixture or screenshot prints a secret value or a whole OIDC token. Nothing written for this program names a Cloud host, an address or a path on Cloud's servers.

## Program checklist

The program runs the way `c-plan.md` "Program checklist" says: arming, the audit tick, owners, PR mechanics, verdict and merge. These differences apply.

- [ ] Dependency graph. D1 branches from `main` now. D2 after D1 and Appendix A gap 1. D3 after D2, after decisions 8 and 9, and after Appendix A gap 6 (the grant-endpoint authentication contract: the Cluster attest key and HPKE authenticated mode). D4 after D3 and Appendix A gap 3. D5 after D4 and Appendix A gap 2 with thread B. D6 after D4, Appendix A gap 7 (`BuildApproval` and its grant authority) and decision 12. D7 after D4. Before D7 builds, its prototype box must close, along with Appendix A gaps 4, 8 (`PreviewReview` grant authority and B's decision 6 signing path) and 9 (reserved preview Namespaces), and decision 11.
- [ ] File boundaries. D1 touches `core/crates/ployzd/src/machine_api/local.rs`, `core/crates/ployzd/src/machine/local_machine/restart_approval.rs`, `core/crates/ployz/src/sdk/github_build.rs` (refusal wording only) and `docs/user/`. D2 adds `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployzd/src/github/{oidc,head}.rs`. D3 touches `core/crates/ployzd/src/management/build_grant.rs`, `core/crates/ployzd/src/sealed_env.rs` (attest key), `core/crates/ployz/src/connect/build_grant.rs`, `core/crates/ployz-core/src/sealed_record.rs`, `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployz-core/src/rpc.rs` (two fields), `rpc_catalog.rs` (one row), `core/crates/ployz/src/handlers/build.rs`, `core/crates/ployz/src/sdk/github_build.rs`, `core/crates/ployz-store/src/build.rs`, `dashboard/src/modules/config-store/store-github*`, `dashboard/src/modules/github/**`, `.github/workflows/ployz-build.yml` and the external builder workflow. D4 touches `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployzd/src/machine/local_machine/{deploy_approval,restart_approval}.rs`, `core/crates/ployz-core/src/rpc.rs` (one field), `core/crates/ployz-store/src/automation.rs`, `core/crates/ployz/src/sdk/store_runner.rs`, `core/crates/ployz/src/cluster.rs`, `core/crates/ployz/src/handlers/github.rs` and `docs/user/deploy/`. D5 touches `dashboard/src/modules/github/**`, `dashboard/src/modules/approvals/**` and the SDK export. D6 touches `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployzd/src/management/build_grant.rs`, `core/crates/ployzd/src/github/head.rs`, `core/crates/ployz/src/handlers/deploy.rs`, `core/crates/ployz-sdk/src/lib.rs` and `docs/user/deploy/history-and-rollback.md`. D7 touches `core/crates/ployz-core/src/authority/{rule,deploy}.rs`, `core/crates/ployz-core/src/sealed.rs`, `core/crates/ployzd/src/sealed_env.rs`, `core/crates/ployzd/src/management/build_grant.rs`, `core/crates/ployzd/src/machine/local_machine/deploy_approval.rs`, `core/crates/ployz-store/src/deployment/mod.rs`, `core/crates/ployz-store/src/pull_request/**`, `dashboard/src/modules/config-store/store-pull-request.server.ts` and `docs/user/`.
- [ ] Bets every D PR names. Bet 6, Machine-local authority, because each Machine checks the token, rule and head itself, and no Machine acts on another's record. Bet 7, Cloud drives and never owns, because Cloud relays rules and tokens it cannot mint. Bet 9, lean on proven primitives, because GitHub OIDC is verified with `ring`'s RS256, already in the tree. Bet 1, observer-relative truth, because freshness is the head this Machine read at admission. The stable promise, because of new optional request fields and capability rows.
- [ ] Residuals each D PR names once (see `d-build-proof.md`, "What remains"). Anyone who can push to the approved branch deploys. Cloud can withhold a new rule generation, re-run or dispatch the current head, cancel builds and show false status. Until the minter is live, Cloud can push a commit.
- [ ] Live lanes run per `c-plan.md` "Boot recipe", plus the real GitHub environment in Appendix B for every lane marked **GitHub**. The attacker harness is C1's `.audit/c-attacker/`, extended with `MintBuildGrant` and token relay.

## Gate builds under enforcement (D1)

Outcome. On a Machine with Device enforcement on, an Operator `Build` or `MintBuildGrant` is refused before anything opens, with a refusal that names the capability. A Full caller (the local socket, or a Device's own connection) is unchanged, and so is a Machine that does not enforce. Attack path closed. Cloud can no longer run build steps of its choosing on an enforcing Server, or read opened Sealed values through a grant for its own key (#4002).

What users lose until D3 and D4. On an enforcing Cluster, every Build that Cloud starts now fails at its start. That covers GitHub builds, Servers builds, automatic push deploys of Git-sourced Services, previews with builds, and `ployz deploy` of a Git or local-directory Service through Cloud. No working deploy is lost, because thread C already refuses a built image on an enforcing Machine (`d-inventory.md`). What changes is that the build no longer runs, no longer spends runner minutes, and no longer opens secrets. Clusters that do not enforce are unchanged.

**Depends on.** `main`. Nothing else.

**Files.**

- [ ] Edit `core/crates/ployzd/src/machine/local_machine/restart_approval.rs` to add `admit_operator_build`, beside `admit_operator_deploy` (`:90`). It refuses under enforcement with `AuthorityRefusal::Unsigned { purpose: Workflow }`.
- [ ] Edit `core/crates/ployzd/src/machine_api/local.rs` so `build` (`:665`) and `mint_build_grant` (`:946`) call it when `from_operator`. `build` refuses before it reads the request stream. `mint_build_grant` refuses before `Mint::check`, so nothing opens.
- [ ] Edit `core/crates/ployz/src/sdk/github_build.rs` and the Servers build path so the refusal reaches the Deployment as "This server runs only builds you approve. Builds start again once automatic deploy rules are on." Run it through the `ui-copy` skill.
- [ ] Edit `docs/user/servers/manage-servers.md` (enforcement), `docs/user/builds/` and `docs/user/deploy/github.md` to say what enforcement stops, and that a Device can still build over its own connection.

**Verify, unit.**

- [ ] `ployzd` tests show an Operator `MintBuildGrant` refused under enforcement with a Sealed record present, and the record never opened. Use a key directory that would fail to open, so an open attempt would show as a different error.
- [ ] `ployzd` tests show an Operator `Build` refused under enforcement, a Full `Build` admitted, and both admitted with enforcement off. Run `cargo nextest run -p ployzd build_grant build`.

**Verify, live.** Ten lanes per the boot recipe.

- [ ] Lane 1. Regression lane against trunk. Enforcement off. Deploy a Dockerfile Service from uploaded source at trunk and head. Save `d1-enforce-off.png`. Pass when both build and run.
- [ ] Lane 2. Enforcement on. Deploy the same Service from `cli`. Save `d1-build-refused.png`. Pass when the Deployment fails at its build with the new message, and `journalctl -u ployzd` shows the refusal before any BuildKit output.
- [ ] Lane 3. Attacker harness. `MintBuildGrant` with Cloud's own holder key and the Sealed record of a Service that has a Sealed variable. Save `d1-mint-refused.png`. Pass when it is refused and no grant exists (`InspectBuildGrant` says not found).
- [ ] Lane 4. Attacker harness. A hand-built `Build` stream whose Dockerfile runs `RUN cat /run/secrets/*`. Save `d1-build-harness.png`. Pass when it is refused and `docker buildx du` shows no new cache record.
- [ ] Lane 5. Enforcement on. `ployz build` over the local socket on the Machine. Save `d1-full-build.png`. Pass when it builds.
- [ ] Lane 6. Enforcement on, then off. Save `d1-toggle.png`. Pass when a build refused while enforcing succeeds after enforcement is off.
- [ ] Lane 7. **GitHub**, once Appendix B exists, else marked skipped. A GitHub build of an enforcing Cluster. Save `d1-github.png`. Pass when the check-in fails, the run ends, and no Sealed value reaches the runner log.
- [ ] Lane 8. Enforcement on, a PR Environment with a build. Save `d1-preview.png`. Pass when the preview's build fails with the message, and the parent Environment is untouched.
- [ ] Lane 9. Kill `ployzd` during a refused build. Save `d1-kill.png`. Pass when the restarted daemon still refuses.
- [ ] Lane 10. Read the changed `docs/user` pages against lanes 2 and 5. Save `d1-docs.png`. Pass when every sentence matches.

**Verify, perf.**

- [ ] Metric. Time to refusal for an Operator `Build` under enforcement. Rule: under 100 ms at the median of ten calls. No trunk equivalent. Record that.

**Review gate.** None.

## Verify GitHub tokens and branch heads on a Machine (D2)

Outcome. A Machine can verify a raw GitHub OIDC token against GitHub's keys and read a branch head by repository ID, and `ployz_core::authority` has the rule types. Nothing is wired to a route yet. Attack path. None closed. This is the verifier D3 and D4 call.

**Depends on.** D1. Appendix A gap 1 agreed with thread A.

**Files.**

- [x] Create `core/crates/ployz-core/src/authority/rule.rs` with `AutoDeployRule`, `RuleService`, `BuildPolicy`, `PolicyDigest`, `SealedDigest`, `BuildClaim` (with `audience()`) and `GithubClaims`, the parsed claims the rule checks. It has no crypto, so it builds for the browser SDK.
- [x] Create `core/crates/ployzd/src/github/oidc.rs` with `verify(token, keys, now) -> GithubClaims` (RS256 through `ring::signature::RSA_PKCS1_2048_8192_SHA256`, fixed issuer) and `IssuerKeys`, which fetches the JWKS over TLS, caches it, and fetches again once on an unknown `kid`.
- [x] Create `core/crates/ployzd/src/github/head.rs` with `branch_head(repository_id, branch, credential)`. It is a `GET https://api.github.com/repositories/<id>/git/ref/heads/<branch>` with a 10 second timeout, and it maps 404 to "unreadable".
- [x] Commit fixtures under `core/crates/ployzd/src/github/fixtures/`. They are a real token from the Appendix B repository, the JWKS at that time and the claims, with the signature intact and the issued token long expired.

**Build.**

- [x] `AutoDeployRule::verify_claims(&GithubClaims)` returns `Mismatch { field }` naming the first failing claim, in the table order of `d-build-proof.md`.
- [x] `BuildClaim::audience` is `"ployz-build:" + hex(SHA-256(canonical JSON))`. It has a golden test whose bytes the builder copies in D3.
- [x] The verifier never logs the token. A refusal logs the claim name and the rule ID.

**Verify, unit.**

- [x] The real fixture verifies at its `iat`. Then one changed byte in each part is refused, and so are an unknown `kid`, another issuer, an expired token, `alg: none`, and HS256 signed with the JWKS modulus.
- [x] `verify_claims` refuses, one per case, another `repository_id`, another owner, `ref_type: tag`, another branch, an event not in the rule, another `workflow_ref`, another `job_workflow_ref`, another `job_workflow_sha`, and `runner_environment: self-hosted`. Run `cargo nextest run -p ployz-core authority::rule` and `-p ployzd github::`.
- [x] `PolicyDigest` and `BuildClaim::audience` golden tests.

**Verify, live.** Six lanes. Lanes marked **GitHub** use Appendix B.

- [x] Lane 1. **GitHub.** A workflow in the test repository prints `exp - iat` and the claim names (not values) of its token. Save `d2-lifetime.txt`. Pass when the lifetime is recorded. It feeds D4's accepted-build record.
- [x] Lane 2. **GitHub.** From a Boat Machine, `branch_head` on the private test repository with an installation token narrowed to `contents: read` and that one repository. Save `d2-private-head.png`. Pass when it returns the head `git ls-remote` shows.
- [x] Lane 3. **GitHub.** The same with a token for another installation that cannot see the repository, and with none. Save `d2-head-refused.png`. Pass when both are "unreadable".
- [x] Lane 4. **GitHub.** Re-run an old run of the test workflow. Save `d2-rerun.png`. Pass when its token's `sha` is the old commit and `run_attempt` went up, as thread Y's report says.
- [x] Lane 5. **GitHub.** Force push the test branch back one commit. Save `d2-force.png`. Pass when the new push run's `sha` is the older commit and `branch_head` returns it.
- [x] Lane 6. Block egress to `api.github.com` on one Machine. Save `d2-offline.png`. Pass when `branch_head` fails within 15 seconds.

**Verify, perf.**

- [x] Metric. Token verification with a warm JWKS, and `branch_head` latency from a Boat Machine. Rule: verification under 2 ms, head read under 1 s at the median of ten.

Lane evidence is text, not screenshots: `media/d2-*.txt`. The perf numbers are in `media/d2-perf.txt`.

**Review gate.** None.

## Build a push under enforcement (D3)

Outcome. On an enforcing Cluster with a rule, a push to the approved branch runs the pinned builder. The grant Machine checks that the commit the builder checked out is the head, before anything opens. It then seals the build secrets to the builder's recipient key from the Cluster attest key, and the builder refuses values from any other sender. The image lands on that Machine. It does not deploy until D4. Attack path closed. Cloud cannot pick the commit, the inputs, the builder, the runner or the holder, cannot read build secrets, and cannot feed the builder secret values of its own through an endpoint it names.

**Depends on.** D2. Operator decisions 8 and 9. Appendix A gap 6.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/rpc.rs` so `MintBuildGrantRequest` gains `rule: Option<Signed<AutoDeployRule>>` and `proof: Option<String>` (the raw token). Edit `rpc_catalog.rs` to add `BUILD_GRANT_RULE_CAPABILITY: ("ployz.build.grant.rule.v1", Always)`.
- [ ] Edit `core/crates/ployzd/src/management/build_grant.rs` so `Mint::check` under enforcement needs the rule and proof. It runs the token checks, then the commit's authority (`Head` only in D3), all before it opens the record. It consumes the `jti` and checks the Sealed record against the rule's `secrets` digest. `sealed` (`:482`) stops serving plaintext. It returns the opened values HPKE auth-sealed from the attest key to the audience's recipient, with the audience bytes in the info. Edit `admit_operator_build` from D1 to let a rule-bearing `MintBuildGrant` through.
- [ ] Edit `core/crates/ployzd/src/sealed_env.rs` (or its key module) to derive the attest key pair from the Sealing Key with HKDF-SHA-256 and the info `ployz build-secrets attest v1`, and expose its public half to `CheckIn`.
- [x] `BuildClaim` already carries `commit` and `authority` in both stages, `Grant { holder, recipient }` and `Built { image, attest }` (D2), so its golden audience is final.
- [ ] Edit `core/crates/ployz/src/connect/build_grant.rs` and `core/crates/ployz-core/src/sealed_record.rs` so the runner opens the grant's response only in HPKE authenticated mode with the relayed attest key and the expected info. A response that fails to open stops the build. `OpenedSealed::covering` (`sealed_record.rs:131`) still checks the names, after authentication.
- [ ] Edit `core/crates/ployz/src/handlers/build.rs` (the runner's `ployz build`) to check out `github.sha` or the `commit` input, make the holder and recipient keys, compute the policy digest from the relayed rule, request both tokens with their audiences, put the attest key it opened with into `Built`, refuse any registry cache, and set the Actions cache `scope` to the policy digest.
- [ ] Edit `core/crates/ployz/src/sdk/github_build.rs` so check-in for a rule build returns the rule, the Cluster key, the attest key and the grant Machine. It returns no commit and no `ployzVersion`. It relays the runner's grant token to `MintBuildGrant`.
- [ ] Edit `dashboard/src/modules/config-store/store-github-builds.server.ts` and `dashboard/src/modules/github/github-build.server.ts` so a rule build follows the push-triggered run of the rule's workflow, rather than dispatching one, and receives the image token with the final Build Steps report.
- [ ] Edit `core/crates/ployz-store/src/build.rs` so `github_authorize` matches a push run on the rule's branch by `run_id`. Claims checked there are bookkeeping only.
- [ ] Edit `.github/workflows/ployz-build.yml` to the file in `d-build-proof.md`.
- [ ] Outside this repository. Add the reusable workflow to `getployz/build` and record its commit in `core/crates/ployz-core/src/authority/rule.rs` as `BUILDER`, the default a CLI approval pins.

**Build.**

- [ ] The builder never checks out a commit from Cloud. It never passes `ACTIONS_ID_TOKEN_REQUEST_TOKEN` or `ACTIONS_RUNTIME_TOKEN` into BuildKit.
- [ ] Run the `verify-build-handoff` model check, since the walk changes.

**Verify, unit.**

- [ ] `build_grant.rs` tests use D2's fixture token with a rule built to match. A valid token mints. Then each grant check fails once: another holder in `aud`, another recipient, another policy in `aud`, a `jti` already consumed, a Sealed record that does not match `secrets`, an old generation, an `ended` rule, a Machine not in `machines`, and a head that differs (with a stubbed head reader). A mint without a rule under enforcement is refused as in D1.
- [ ] `build_grant.rs` tests show the commit's authority is checked before the record opens. A `commit` that differs from `sha` under `Head`, and `Approved` or `Review` with no statement, are each refused, using a key directory that would fail to open, so an open attempt shows as a different error.
- [ ] `sealed_record.rs` and `connect/build_grant.rs` tests open a response sealed by the right attest key, and refuse one sealed by another key, one sealed in base mode, and one sealed for another audience's info. Run `cargo nextest run -p ployz-core sealed_record` and `-p ployz connect::build_grant`.
- [ ] `handlers/build.rs` tests show the audience bytes equal D2's golden test, `Built.attest` equals the key the secrets opened with, and a policy with `cache_from` of type `registry` refused before the build.
- [ ] `pnpm vitest run src/modules/config-store/store-github-builds` covers following a push run and receiving the image token.

**Verify, live.** Twelve lanes, all **GitHub**, against a Boat Cluster with enforcement on and a rule approved from `cli --device`. D3 has no rule CLI yet, so the lanes sign the rule with a test helper. D4 adds the CLI.

- [ ] Lane 1. Regression lane against trunk. Enforcement off, the old workflow file, a dispatched build. Save `d3-legacy.png`. Pass when it builds at trunk and at head.
- [ ] Lane 2. Push to the approved branch. Save `d3-push-build.png`. Pass when the image lands on the grant Machine and its log names the rule, generation and commit.
- [ ] Lane 3. Attacker harness. Relay the runner's grant token with Cloud's own holder key. Save `d3-holder-swap.png`. Pass when it is refused and no secret is served.
- [ ] Lane 4. Attacker harness. Give the runner a policy with an extra build arg. Save `d3-policy-swap.png`. Pass when the grant is refused naming the audience.
- [ ] Lane 5. Re-run the lane 2 run after a newer push. Save `d3-rerun.png`. Pass when the grant is refused naming the commit.
- [ ] Lane 6. Relay one grant token twice. Save `d3-replay.png`. Pass when the second is refused as consumed.
- [ ] Lane 7. Dispatch the workflow on branch `attacker`, holding a modified copy of the builder call. Save `d3-other-branch.png`. Pass when it is refused naming `ref`.
- [ ] Lane 8. Push to a builder fork at a new commit, and point a copy of the workflow at it. Save `d3-builder.png`. Pass when it is refused naming `job_workflow_sha`.
- [ ] Lane 9. Actions cache. A run on branch `other` writes the policy-digest scope, then the approved branch builds. Save `d3-cache.png`. Pass when the approved build does not hit the `other` entry. If it does, record that and turn the cache off for rule builds (operator decision 7).
- [ ] Lane 10. A build secret in the Dockerfile (`RUN --mount=type=secret`). Save `d3-secret.png`. Pass when the build reads it, and neither the runner log, Cloud's database nor the Machine log holds its value (`grep -c` of a marker only).
- [ ] Lane 11. Attacker harness. The Dockerfile runs a script from a Sealed URL. Check-in names an iroh endpoint the harness runs, which serves its own value for that URL, first with the real attest key and then with its own key in place of it. Save `d3-endpoint.png`. Pass when the first build stops at opening the secrets, and the second build's image token names the harness's key, which D4's lane 11 then refuses.
- [ ] Lane 12. Attacker harness. Dispatch the workflow on the approved branch's head with `commit` set to a commit from a fork. Save `d3-commit-input.png`. Pass when the grant is refused naming the commit, the Machine log shows no Sealed open, and the runner log shows no secret file.

**Verify, perf.**

- [ ] Metric. Push to image landed, at trunk with the old workflow and enforcement off, and at head with a rule. Five runs each side. Rule: head within 1.2 times trunk, mostly the second token request and a head read.

**Review gate.** The operator reviews the new workflow file and the builder before merge, since customers copy them. Post both in chat with lane 2's screenshot.

## Deploy a push under enforcement (D4)

Outcome. With a rule approved once from the CLI, every push to the approved branch deploys on an enforcing Cluster with no prompt, force pushes included. A settings change stops automatic deploys until the rule is approved again. Attack path closed. Cloud cannot deploy another commit, another image or other settings, or replay old proof.

**Depends on.** D3. Appendix A gaps 1 and 3.

**Files.**

- [x] Edit `core/crates/ployz-core/src/authority/rule.rs` to add `verify_creation`, the creation checks in `d-build-proof.md`, and `RuleProof { rule, token }`.
- [x] Edit `core/crates/ployz-core/src/rpc.rs` so `EnsureContainerRequest` gains `rule: Option<RuleProof>`. Edit `rpc_catalog.rs` to add `DEPLOY_RULE_CAPABILITY: ("ployz.container.deploy-rule.v1", Always)`.
- [x] Edit `core/crates/ployzd/src/machine/local_machine/deploy_approval.rs` to add `admit_rule_creation` and the rule's lifecycle steps. Edit `restart_approval.rs` so a creation with a rule goes there. The approval record gains the generation high-water mark per rule, accepted builds, and hook consumption per (rule, generation, commit, Service).
- [x] Edit `core/crates/ployz-store/src/automation.rs` so a push admitted under a rule records the rule and waits for the image token. A Saved revision whose template no longer matches makes the push wait as "approve the rule again", with the changed Services listed.
- [x] Edit `core/crates/ployz/src/sdk/store_runner.rs` and `core/crates/ployz/src/cluster.rs` so the runner attaches the rule proof to every `EnsureContainer` of that Deployment.
- [x] Create the CLI commands `ployz github rule approve <service>...` and `ployz github rule end`. Approve prints the repository, branch, Environment, builder commit, each Service's policy and the settings it pins, then signs with the Device key. Run the `ui-copy` skill.
- [x] Edit `docs/user/deploy/github.md` and `docs/user/servers/manage-servers.md`.

**Verify, unit.**

- [x] `rule.rs` tests take one approved spec and token, and change one thing per case, each refused naming it. The cases are the image digest, a plain value, a Sealed ciphertext, `privileged`, a Bind Mount, a host port, the hook command, the generation (lower), the commit (not the head), `aud` naming another image, `attest` naming another key, and authority `Approved` (a rule never admits it at creation).
- [x] `deploy_approval.rs` tests admit a second creation of the same image after the token's `exp` through the accepted-build record, and refuse it once the head moved. They also admit a hook once per push. A force push back to an earlier commit can run that commit's hook again. A concurrent test refuses a generation replaced while its creation waited on GitHub.
- [x] `automation.rs` tests hold a push for re-approval when the Saved revision changes a setting, and admit it when only the commit changed. Run `cargo nextest run -p ployz-core authority::rule`, `-p ployzd deploy_approval`, `-p ployz-store automation`.

**Verify, live.** Eleven lanes, all **GitHub**, two Machines, enforcement on, rule approved with `cli --device -- github rule approve web`. Evidence is the terminal output of each lane, kept with the verify run rather than as screenshots.

- [ ] Lane 1. Regression lane against trunk. Enforcement off. A push deploys at trunk and at head. Save `d4-enforce-off.png`. Ran at head only: a push deployed through a Servers build. Trunk not run.
- [x] Lane 2. A push deploys with no prompt. Save `d4-push.png`. Pass when both Machines run the new digest and log `rule <id> gen <n> commit <12 hex>`.
- [x] Lane 3. A force push to an older commit deploys. Save `d4-force.png`. Pass when both Machines run that commit's image.
- [x] Lane 4. Attacker harness. Replay lane 2's `EnsureContainer` after lane 3. Save `d4-stale.png`. Pass when it is refused naming the commit.
- [x] Lane 5. Attacker harness. Lane 2's request with an image Cloud pushed through `EnsureImageIngest`. Save `d4-artifact.png`. Pass when it is refused naming the image.
- [x] Lane 6. Attacker harness. Lane 2's request with `privileged: true`, then with a new Sealed variable. Save `d4-settings.png`. Pass when both are refused naming the payload.
- [x] Lane 7. Change a plain variable in the dashboard, Save, and push. Save `d4-reapprove.png`. Pass when the push waits as "approve the rule again", and after `github rule approve` the next push deploys at generation n+1.
- [x] Lane 8. Attacker harness. Replay a generation n token after lane 7. Save `d4-old-gen.png`. Pass when it is refused naming the generation.
- [x] Lane 9. A pre-deploy hook that appends a line. Push once, then replay the hook creation. Save `d4-hook.png`. Pass when the file has one line.
- [x] Lane 10. `github rule end`, then push. Save `d4-ended.png`. Pass when nothing deploys and the Machines log the ended rule.
- [ ] Lane 11. Attacker harness. Deploy D3 lane 11's image, whose token names the harness's attest key. Save `d4-attest.png`. Pass when every Machine refuses naming the attest key. Not run live: D3's attacker endpoint is not kept. With Cloud handing the runner a foreign attest key, the pinned builder refused to build. The Machine refusal is covered by a unit test.

**Verify, perf.**

- [ ] Metric. Push to Deployment success, at D3's head with enforcement off and at D4's head with a rule, five each, interleaved. Rule: within 1.2 times. Admission per creation under 500 ms at the median, including one head read. Admission measured 487 ms median (n=18, 318 to 1169 ms), accepted by the operator: the head read is the check, 50 ms warm and 234 ms cold. Push to applied under a rule: 64 s median (n=8). The D3 baseline comparison was not run.

**Review gate.** The operator reviews the CLI output. Post lanes 2 and 7 with a 30 to 60 second terminal video, saved under `docs/agents/plans/security-0.4/media/d4-review.*`. Saved as `d4-review.mp4`, 40 s.

## Approve rules in the dashboard (D5)

Outcome. A dashboard user turns automatic deploy on for an Environment with one passkey prompt. A held push shows an Approval card listing the changed settings, and one prompt approves the next generation. Attack path. A session grant cannot sign a rule.

**Depends on.** D4. Appendix A gap 2 with thread B.

**Files.**

- [x] Edit `core/crates/ployz-sdk/src/lib.rs` to add `autoDeployRuleBody(view, signer)`, which returns the canonical body and the change lines. Export the rule types from `@ployz/sdk/authority`. Shipped as `autoDeployRuleBody(view, terms, services?)` and `autoDeployRuleEndBody`, over drafting in `ployz-core`'s `authority::rule_draft` that the CLI shares.
- [x] Edit `dashboard/src/modules/github/**` and `dashboard/src/modules/approvals/**` for the rule card and its prompt, through B's passkey-direct helper. B had none, so D5 adds `dashboard/src/modules/approval-session/passkey-statement.ts` for E8 and D7. The card sits in the Environment's settings, since an Approval card is the agent sidebar's.

**Verify.** Unit tests for the body's golden digest and for a session-signed rule refused. Live per `verify-dashboard` with `SERVERS=1`. Record turning a rule on with one prompt, a held push shown as a card, approving it, the next push deploying, and a session-only attempt refused by the Machine. Check light and dark themes and phone width per `dashboard/DESIGN.md`. Perf: the body is built in under 20 ms for 20 Services. Done: `media/d5-review.mp4` and `media/d5-*.png`, 1.31 ms median for 20 Services.

**Review gate.** The operator reviews screenshots and a video before merge. Waived under the operator's autopilot grant.

## Approve a built image deploy (D6)

Outcome. A person deploys or rolls back a Git-sourced Service at a named commit under enforcement, from the CLI or the dashboard. Attack path closed. Cloud cannot swap the image for one built from another commit or policy. It cannot get build secrets for a commit that never sat on the approved branch.

The semantics are in `d-build-proof.md`, "A person deploys a built image (D6)". In short, a rollback has no `Head` authority, so a person supplies it twice. A `BuildApproval` names the commit and lets the grant Machine release secrets for it. The client then verifies the image token itself and signs thread C's ordinary `DeployApproval` with the content-addressed image. Machines never check an expired image token. They check a person's approval of a digest, as thread C already does.

**Depends on.** D4. Appendix A gap 7. Operator decision 12.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/authority/rule.rs` to add `BuildApproval` (purpose `Workflow`, thread C's 15-minute window) and `Authority::Approved`.
- [ ] Edit `core/crates/ployzd/src/management/build_grant.rs` so grant admission accepts `Approved(d)`. That means the signature, the rule, generation, Service and commit, a `workflow_dispatch` run on the rule's branch, and the commit on the branch's history. The Machine reads `GET /repositories/<id>/compare/<commit>...<branch>` now and accepts `identical` or `ahead` (the branch descends from the commit) and refuses `behind` and `diverged`. Add the compare read to `core/crates/ployzd/src/github/head.rs`.
- [ ] Edit `core/crates/ployz/src/handlers/deploy.rs` (and `core/crates/ployz-sdk/src/lib.rs` for the dashboard) so a deploy of a Git Service at a commit verifies a stored image token for that commit, if one exists. The client checks the signature against the JWKS it fetches, every claim and the audience against the rule, `attest` equal to the Cluster's attest key, and ignores `exp`. With no usable token, it signs a `BuildApproval`, waits for the build, and verifies the new token. Then it signs `DeployApproval` with `image = <repository>@<digest>`.
- [ ] Edit `docs/user/deploy/history-and-rollback.md`.

**Verify, unit.**

- [ ] `build_grant.rs` tests admit `Approved` for a commit the branch descends from (compare `ahead` or `identical`), and refuse a commit not on the branch (compare `diverged`), an unmerged descendant of the head (compare `behind`), another Service, another generation, an expired approval and a `push` run.
- [ ] `handlers/deploy.rs` tests verify an expired fixture token whose claims match, and refuse one with another commit, another policy, another attest key, and a `kid` missing from the JWKS (which falls back to a rebuild).

**Verify, live.** Six lanes, all **GitHub**.

- [ ] Lane 1. Roll back to the previous commit's image from `cli --device`. Save `d6-rollback.png`. Pass when both Machines run it and log the approving Device.
- [ ] Lane 2. Deploy an older commit with no stored token. Save `d6-rebuild.png`. Pass when it builds under a `BuildApproval` and deploys.
- [ ] Lane 3. Attacker harness. A `BuildApproval` for a fork commit. Save `d6-fork.png`. Pass when the grant is refused naming the commit, with no Sealed open.
- [ ] Lane 4. Attacker harness. Cloud stores another image's token against the rollback commit. Save `d6-token-swap.png`. Pass when the CLI refuses before signing.
- [ ] Lane 5. A build that outlasts 15 minutes (a `sleep` in the Dockerfile). Save `d6-long.png`. Pass when the deploy approval, signed after the build, is accepted.
- [ ] Lane 6. Regression lane against trunk. A rollback with enforcement off. Save `d6-enforce-off.png`.

**Verify, perf.** As C1. Signing and token verification add under 1 s to a rollback.

**Review gate.** None.

## Previews under a rule (D7, checkpoint C3)

Outcome. Pushes by trusted GitHub accounts to branches of the repository deploy their PR Environment with no review. The Machine derives each preview's Namespace and hostnames from the signed preview rule and facts it verified, so a preview can land only in a Namespace holding nothing but that preview. A preview inherits only the parent Sealed values the preview rule names, opened in place on the Machine. Untrusted PRs keep the review path. A review is checked before the reviewed build gets any secret, and it is bound to the commit the builder checked out. Attack path closed. Cloud's membership rows and bot flag no longer decide trust. Cloud cannot place preview code in a production Namespace or behind a production hostname. It cannot give a preview a parent secret the rule does not name, or release preview secrets to an unreviewed commit.

**Depends on.** D4. The prototype box below. Appendix A gaps 4, 8, 9 and 10. Operator decisions 6 and 11, and thread B's passkey-direct signing path for decision 6 (gap 2).

**Prototype first.**

- [x] Lower a parent Environment and its PR Environment for the same commit, and diff their requested specs. Pass when they differ only in the Namespace (including strings that embed it), preview hostnames and the Sealed identity, and when Own Copies keep their parent's logical Volume names. If Own Copies are renamed, D7 changes the lowering to keep the names. If any other field differs, stop and redesign the template before building. Ran on 2026-11-10 and stopped at this gate. Beyond the allowed set, Own Copy Volume names, the Service ID label, Generated Values and Setup Commands differed, and so did one replica and Live Node references to the parent's Namespace. Two previews of one rule also differed in the first three. The root redesigned the template as gap 10, and D7's test now checks that two previews of one rule differ only in derived fields.

**Files.**

- [x] Edit `core/crates/ployz-core/src/sealed.rs` so `SealedValue` gains `environment: Option<Uuid>`, which replaces the identity's Environment in the associated data.
- [x] Edit `core/crates/ployzd/src/sealed_env.rs` to check the override on every Sealed part, template parts and build records included. It opens a value carrying one only when admission handed it the preview rule's `inherit` list for that Environment.
- [x] Edit `core/crates/ployz-core/src/authority/rule.rs` to add `PreviewRule`, `PreviewReview`, `Authority::Review`, `preview_namespace(rule, key)`, `preview_hostname(label, namespace, domain)` and `PreviewTemplate::of(spec, derived)`. Placeholders replace only strings the Machine derived. Built in `authority/preview.rs`. `preview_hostname` became `preview_prefix(label, namespace)`, giving `<label>-<Namespace>.<domain>` (`d-build-proof.md`, "Hostnames").
- [x] Edit `core/crates/ployzd/src/management/build_grant.rs` so grant admission accepts a trusted push (`Head` on a non-parent branch, `actor_id` in `trusted`) and `Review(d)`. A review needs its signature, rule, generation and commit, a `workflow_dispatch` run on the parent's branch, and the signer rule of decision 6. All of that is checked before the record opens. D7 admits only the trusted push. `Review(d)` moved to D7b (below), and until it ships every Machine refuses a reviewed preview build or deploy.
- [x] Edit `core/crates/ployzd/src/machine/local_machine/deploy_approval.rs`. A preview-rule creation must be in the derived Namespace, with derived hostnames, and Sealed parts inherited or, under a review, sealed for the review's Environment. Thread C's deploy approval and the automatic deploy rule refuse the reserved preview Namespace pattern.
- [x] Edit `core/crates/ployz-core/src/authority/deploy.rs` to widen `sealed_scope_allows` for reviews only, as `d-build-proof.md` says.
- [x] Edit `core/crates/ployz-store/src/deployment/mod.rs` (`namespace`, `:1539`) so a PR Environment under a preview rule gets the derived Namespace, and no other Environment ever gets the reserved pattern. Edit the preview lowering to emit derived hostnames.
- [x] Edit `core/crates/ployz-store/src/build.rs`, `core/crates/ployz/src/sdk/github_build.rs`, `sdk/store_runner.rs`, `cluster.rs` and `handlers/build/rule.rs` so a preview's GitHub build follows its push's run, checks in and asks its grant under the preview rule, and the runner attaches a `PreviewProof` to each `EnsureContainer`. A preview build's Actions cache scope also names the rule, so no preview build writes layers a production build reads.
- [ ] Edit `core/crates/ployz-store/src/pull_request/mod.rs` and `dashboard/src/modules/config-store/store-pull-request.server.ts` so trust comes from the preview rule. The members join stays only as the label shown before a rule exists. Partly: `pull_request` opens a preview under the rule in its derived Namespace, but Cloud still decides with the members join whether a push deploys. Every Machine checks the rule's `trusted` list, so a member the rule doesn't trust gets a refused preview, not a deployed one.
- [x] Create the CLI `ployz github preview approve`, which prints the trusted accounts, inherited keys, preview domain, and any Bind Mount or External Volume as "shared with production". It refuses those mounts unless `--share-mounts` is given. D7d (below) took Cloud's members out of its default.
- [x] Edit `docs/user/environments/preview-environments.md` and `docs/user/services/variables.md`.

**Verify, unit.**

- [ ] An inherited value opens under its rule, and is refused for a key not in `inherit`, for another parent, under a production deploy approval, under an automatic deploy rule, and relabelled as another key. A Sealed part in a template carrying an override is checked like any other.
- [ ] A preview-rule creation is refused in the parent's Namespace, in another rule's derived Namespace, in an ordinary Namespace, and with a hostname not derived. Thread C's approval and the automatic deploy rule are refused in a reserved Namespace. `PreviewTemplate::of` leaves a production Namespace string inside an environment value literal, so it fails the template.
- [ ] An untrusted `actor_id` is refused. A trusted push to the parent's branch is refused as a preview.
- [ ] A reviewed grant is refused for another commit, for another rule, and with no review. Each refusal happens before the record opens, using a key directory that would fail to open.
- [ ] Decision 6 on the Machine. A session-signed review is refused at grant and at creation for a Service with a non-empty `inherit` or a `secrets` digest. The same review signed by a passkey's own assertion is admitted. A session-signed review of a Service with neither is admitted.
- [ ] An older daemon refuses the new Sealed field.

**Verify, live.** Ten lanes, all **GitHub**, enforcement on, a preview rule approved from `cli --device`.

- [x] Lane 1. Regression lane against trunk. A trusted preview deploys with enforcement off. Save `d7-enforce-off.png`. Ran at head only, in `Ployz-test/lab-preview`: PR #1 deployed `lab-pr-1` through a Servers build, with no production secret. Trunk not run.
- [x] Lane 2. A trusted account's push deploys a preview that reads `DATABASE_URL` (length only) and not `STRIPE_KEY`. Save `d7-trusted.png`. Pass when the Namespace and URL have the derived form. `pv-6652ff952822497d-d8ffd8f7a2bd`, served as `web-2-pv-6652ff952822497d-d8ffd8f7a2bd.ada.ployz.test` on both Machines, `DATABASE_URL` length 38. `STRIPE_KEY` lives on `billing`, which the rule doesn't cover, since inherit takes every Not set key of a covered Service.
- [x] Lane 3. Adding an untrusted account to Cloud's `member ⋈ account` rows, then pushing as that account. Save `d7-members.png`. Pass when nothing deploys without review. Ran as a member's account the rule no longer trusts (approved again with `--trust` naming another ID): the Machine refused the grant naming an account, and the Deployment failed at its build.
- [x] Lane 4. Attacker harness. Lane 2's proof with the parent's Namespace and a production Volume name. Save `d7-prod-namespace.png`. Pass when it is refused naming the Namespace, and no Container mounts the Volume. The harness rebuilt lane 2's `PreviewProof` from Cloud's rows; the unchanged spec was admitted as the control.
- [x] Lane 5. Attacker harness. Lane 2's proof with a production hostname. Save `d7-prod-host.png`. Pass when it is refused naming the payload.
- [x] Lane 6. Attacker harness. Relay the parent's `STRIPE_KEY` ciphertext into the preview. Save `d7-uninherited.png`. Pass when it is refused before any open. Refused at admission naming a Sealed value.
- [x] Lane 7. A fork PR, reviewed with a passkey prompt. Save `d7-review.png`. Pass when exactly the reviewed commit builds and deploys. Run by D7b with an untrusted account's PR from a branch of the lab repository: Appendix B has no second account to fork with. The grant logged `reviewed_by="passkey …"`, and the preview served the PR's page. The lane found two bugs, both fixed: a signed-in approval dropped the review, and the review's view lowered no generated hostname, so its digest never matched (`d7b-review-dialog.png`).
- [x] Lane 8. Attacker harness. Dispatch the reviewed build with another commit as `commit`. Save `d7-review-commit.png`. Pass when the grant is refused before any Sealed open. Run by D7b. Cloud was patched to dispatch an unreviewed commit and accept its check-in, and the lab builder's CLI skipped its own commit check. The Machine refused the grant naming the commit.
- [x] Lane 9. A session-signed review of a preview that inherits `DATABASE_URL`. Save `d7-session-review.png`. Pass when the Machine refuses naming the signer. Run by D7b: a browser key under a passkey-answered session grant. Cloud stored it, and the Machine refused the grant naming the signer.
- [x] Lane 10. Read the changed `docs/user` pages against lanes 2 and 7. Save `d7-docs.png`. Read against lanes 2 and 3; lane 7 is D7b's. The approve example gained its `host ports` line and the untrusted-account sentence its outcome.

**Verify, perf.** Admission per creation within 10 ms of D4. Medians 185 to 236 µs against D4's 181 to 202 µs (n=300, three runs, release build, head read faked). Live, with the head read: 276 to 664 ms per creation, the head read dominating as in D4.

**Review gate.** The operator reviews the preview rule UI and the derived URLs before merge.

## Reviewed previews (D7b)

Split from D7 by the program's root on 2026-11-10 under the operator's autopilot grant. D7 merges without it.

**Outcome.** A person reviews an untrusted or fork pull request's commit, and exactly that commit builds and deploys its preview.

**Depends on.** D7. D5's passkey-direct helper (#4063) for decision 6.

- [x] Admit `Review(d)` at grant and creation on the Machine: the checks D7 wrote in `verify_review` and `PreviewRule::preview_key`, which D7 leaves unused. Today the Machine refuses every reviewed preview, naming "a reviewed preview".
- [x] The dashboard review prompt on D5's helper, and its session path for previews that inherit nothing. The dashboard signs every review with a passkey, one prompt each. Machines still take a session-signed review of a preview that opens no secret, for the CLI or a later UI.
- [x] D7's live lanes 7 to 9.

## Trusted accounts only from the person (D7d)

**Outcome.** A preview rule trusts only the GitHub accounts the person names with `--trust`. Without it, the rule trusts nobody and every pull request needs a review. The CLI reads each named account from GitHub itself and shows its login with its ID before it signs.

**Attack path closed.** Before D7d, approve without `--trust` filled `trusted` from the plan's `members`, Cloud's member rows, and printed bare numeric IDs. A compromised Cloud could plant an attacker's account there. The person can't judge a number, so they signed it, and the attacker's pushes then built and deployed with the parent's inherited values and no review. A value Cloud relays may be shown or echoed, but never fills a signed field the person can't judge.

- [x] Remove `members` from `PreviewRuleView`, so the plan reply carries no account at all.
- [x] `--trust` takes a login or an ID. The CLI asks `api.github.com` for each account, never Cloud, and refuses one GitHub doesn't name. A re-approval notes the accounts the replaced rule trusted that this one drops.
- [x] Check every other drafted rule. The dashboard drafts no preview rule. An automatic deploy rule's Cloud-relayed `repository_id` and `owner_id` are bound by the repository name the person sees, since each Server matches `workflow_ref` against it. Its Cluster key and Servers are echoed: each Server admits a rule only when it names that Server's own key and Machine ID.
- [x] Show every setting a rule signs. Codex's review found that the summary, which the CLI and the dashboard's passkey dialog both print, showed only variables, the pre-deploy command, host ports and `privileged`, while the template signs the whole spec. Cloud could plant a start command, entrypoint, user or capability unseen, and every push would run it with the opened secrets. `rule_draft::settings` now prints every set field by its wire name, so a field added later shows too. A preview approval also prints what its first-deploy spec adds, such as the setup command, where it said only "runs once".
- [x] Show every field of the build policy. A second Codex review found that `builds` named a Railpack policy only as `Railpack`, dropping the build command the builder runs, and a preview approval printed no policy at all. `builds` now destructures the whole policy, so a new field fails to compile until it shows, and a preview approval prints it with its parent branch. A second lane with a planted `echo planted-build-step` showed it in the CLI rule, the preview rule and the dashboard's passkey dialog.
- [x] Show every relayed string so it can't pass for another. A third Codex review found that `builds`, the settings lines and the summary's repository, branch and builder printed relayed strings raw. A carriage return or escape sequence could rewrite the line in the terminal, `{"X":"a, Y=b"}` read the same as `{"X":"a","Y":"b"}`, and a Dockerfile named `Railpack` read as the Railpack method. Every such string now reaches the output only through `authority::shown::Shown`. It shows a plain token bare, quotes anything else, and escapes every character outside printable ASCII, keys included. Each method has its own label. A preview approval also shows each generated variable with its generator, such as `${{ secret(32, "a-zA-Z0-9") }}`, where it showed only the name. The dashboard now reads the repository it checks from the signed body, not from the summary text.
- [x] Unit. A plan made with a planted member carries no member ID (fails on the code before D7d). No `--trust` asks GitHub nothing and trusts nobody. Trusted accounts show as `login (id)`. An account GitHub doesn't name is refused before signing. Each of ten fields Cloud could plant, the command among them, changes the summary. Every field and variant of a build policy changes both the signed body and what shows, and no two policies show alike. A carriage return, line feed, escape sequence, C1 control or bidi override in each string of a policy, a spec, the repository, the branch and the builder shows escaped, in printable ASCII only. A value holding `, Y=b` shows apart from two entries. A preview's generated variable shows its length and charset. Unquoting relayed text turns seven of these tests red.
- [x] Live. On a `--cloud` run whose seeded member has a linked GitHub account, approve without `--trust`, and read the kept rule's `trusted`. Pass when it is empty and the output says nobody is trusted. Ran on 2026-11-11 at `44ac88fef4` with the fake GitHub, one Boat Machine, enforcement on. The member's account 9001 was in Cloud's rows. Approve printed `trusted nobody: every pull request needs a review`, and `--json` gave `"trusted": []`. `--trust octocat` read GitHub itself and printed `octocat (583231)`. Approving again without it noted the dropped account, and an unknown login was refused before signing. Both the preview rule and the automatic deploy rule printed the planted `command ["sleep infinity"]`, and so did the dashboard's passkey dialog (`d7d-rule-dialog.png`). The lane needed the fake GitHub to answer a repository's `owner.id`, which it now does. A third lane at `42dfcafc35` planted `X=a, Y=b`, a value holding a carriage return and an erase-line escape, and a generated `SESSION`. The CLI preview rule, the CLI rule and the dashboard's passkey dialog (`d7d-r2-dialog.png`) each printed `X="a, Y=b"` and `LOG="ok\r\u{1b}[2Kfake line"`, and the preview printed `SESSION=${{ secret(48, "a-f0-9") }}`. The dialog's repository check read the signed body and enabled the passkey button. The lane also showed that a preview's policy signs the rehearsal's generated value as a build arg, which the runner would refuse for every preview. That is filed as #4094.

**Residual.** Cloud still maps the Environment name to its ID and Namespace, so it could mislabel which parent a rule's previews inherit from.

## Appendix A. Contract gaps

1. **A `Rule` purpose (thread A).** `Purpose` is `Workflow`, `Join` and `Enforce` (`ployz-core/src/authority/keys.rs:23`). A rule must not be signable by a session grant, which `Workflow` allows. D2 needs `Purpose::Rule`, which `verify_signers` admits for a Device or a passkey's own assertion only.
2. **Passkey-direct signing in the dashboard (thread B).** D5 needs a passkey assertion over a rule body outside a session. B's enrollment and proposal approval already prompt directly, so the helper may exist. Confirm it with the B owner. Closed by D5: B's prompts were session-bound, so D5 adds `passkey-statement.ts`, one prompt over a statement's own challenge, which gap 8 reuses.
3. **Generation and accepted-build records (thread A).** D4 adds a per-rule high-water mark and an accepted-build entry to the consumption log. Like C1's hook entry, this is "C inside authority", and A must agree to the record kinds.
4. **`SealedValue.environment` (threads A and C).** D7 changes Sealed associated data. That is a wire change in a type C's digest covers. An older daemon refuses it, which fails closed.
5. **The GitHub minter.** The root runs it as its own slice after D4 (operator decision 1).
6. **The grant-endpoint authentication contract (thread A).** D3 has every Machine derive an attest key pair from the Sealing Key (HKDF-SHA-256, info `ployz build-secrets attest v1`). The grant response is sealed with HPKE in authenticated mode (RFC 9180 §5.1.3, already in `hpke` 0.13), from that key to the builder's recipient key, with the grant audience as info. This is a new use of the Sealing Key. A owns key derivation and must agree before D3 builds.
7. **`BuildApproval` and its grant authority (thread A, with C).** D6 adds a `Workflow` statement that authorizes a grant for a non-head commit on the approved branch's history. It also uses thread C's existing `DeployApproval` with a content-addressed image for the deploy, so C's digest semantics are unchanged. A must agree to the statement. C confirms that a built image's `<repository>@<digest>` passes `content_addressed` (`ployz-core/src/authority/deploy.rs:122`).
8. **`PreviewReview` grant authority, and the decision 6 signing path (threads A and B).** D7 checks a review at grant, before secrets open, and refuses a session-signed review of a Service that inherits values or has build secrets. A must agree to the statement and to the signer rule. B must provide a passkey-direct assertion over a review body in the dashboard, the same helper as gap 2.
9. **Reserved preview Namespaces (thread C).** D7 makes thread C's deploy approval refuse the `pv-<16 hex>-<12 hex>` Namespace pattern, and makes Cloud's Namespace allocator never assign it. That is a check inside C's `deploy::verify`, so C's owner reviews it.
10. **The preview template after D7's prototype (threads A and C).** Taken by the program's root on 2026-11-10 under the operator's autopilot grant, on behalf of threads A and C. `d-build-proof.md`, "Derived fields, checked before hashing", has the detail.
    1. *Volume names.* An Own Copy Volume is lowered under its parent's logical name, so the derived Namespace alone keeps the data apart. D7 applies this only in a derived preview Namespace, because renaming it in every Branch would leave a Kept Branch on a new, empty Volume.
    2. *Service ID label.* `cloud.ployz.service.id` is lowered from the lineage ID, which a Branch shares with its Parent. It is bookkeeping, so a wrong label is a mislabel residual.
    3. *Generated Values.* The template keeps the generator, `secret(LEN, "CHARSET")`. Before hashing, the Machine checks that the real value has exactly that length and charset.
    4. *Setup Commands.* `PreviewService` signs two digests, with and without the Setup Commands. The Machine accepts the first only for a preview's first creation, when no Service Container of that Service exists in the derived Namespace on this Machine.
    5. *The test.* It checks that two previews of one rule differ only in derived fields: the Namespace, hostnames, the Sealed identity and the checked Generated Values. The CLI builds the template through the preview lowering, so one replica and Live Node references to the parent's Namespace are part of it.

## Appendix B. The real GitHub environment

`verify-dashboard` fakes GitHub (`dashboard/scripts/verify/up.sh:176` sets a fake App ID, and its SKILL.md says any 40-hex commit resolves). Nothing in this repository, its skills or its docs names an isolated real setup. Thread Y's report says the same. The operator creates one:

- [ ] A GitHub organization used for nothing else, with a private repository holding a Dockerfile Service, a `main` branch and the new `ployz-build.yml`.
- [ ] A fork of `getployz/build` inside it, so lanes can pin and move a builder commit without touching the real one.
- [ ] A GitHub App owned by that organization, with Thread Y's narrowed permissions (Contents read, Metadata read, Pull requests read, Checks write, Actions write), installed on the repository. Its key and webhook secret stay on the operator's host and are passed to lanes through `request_secret`, never committed.
- [ ] A Boat Cloud reachable from GitHub over HTTPS, for webhooks and runner check-ins. That means a public tunnel to the `verify-server` Cloud, plus `AppConfig` GitHub origins set to the real GitHub. Whether `verify-server --cloud` accepts a real App's credentials is unproven. D2's owner checks it first.
- [ ] Two GitHub accounts besides the operator's: one in the preview rule's `trusted` list and one outside it, for D7.
- [ ] GitHub-hosted runners reaching the grant Machine. The runner dials the Machine's Build Grant door through iroh, as production GitHub builds do today. Unproven from Boat.

## Appendix C. Prototype evidence

Run on 2026-11-09, read-only, from this design thread.

- **Proven.** `GET https://api.github.com/repositories/<id>/git/ref/heads/main` returns the head for a public repository with no credential, and for a private repository with a read credential. Without one, the private repository returns `404`. The credential was a user token.
- **Proven.** GitHub's OIDC discovery document lists `job_workflow_ref`, `job_workflow_sha`, `runner_environment`, `run_attempt`, `repository_id`, `repository_owner_id`, `actor_id` and `jti` among its claims. Its JWKS keys are RSA with `RS256`.
- **Proven in D2.** An installation token narrowed to `contents: read` and one repository reads the head by repository ID (lane 2). A token lives 300 seconds, `exp - iat`, and GitHub backdates `nbf` by 300 seconds (lane 1). A re-run keeps `sha` and raises `run_attempt`, and a force push gives the new tip as `sha` (lanes 4 and 5).
- **Documented, not run.** A job asks for a token with a custom audience through `$ACTIONS_ID_TOKEN_REQUEST_URL&audience=…`. `job_workflow_ref` and `job_workflow_sha` name the reusable workflow and its commit.

Unproven until the slice that proves it:

- An audience as long as `ployz-build:` plus 64 hex characters is accepted. D3 lane 2.
- Actions cache isolation by branch. D3 lane 9.
- A PR Environment's spec differs from its parent's only in fields `PreviewTemplate` derives, and Own Copies keep their logical Volume names. D7's prototype found otherwise on 2026-11-10, and gap 10 redesigned the template.
- HPKE authenticated mode in `hpke` 0.13 opens on the runner with the attest public key, and refuses a base-mode or wrongly keyed response. D3's unit tests and lane 11.
- `workflow_dispatch` with a `commit` input can check out a fork pull request's commit from the base repository. D7b's lane 7 proved it for another branch's commit; a fork's needs a second GitHub account.
- `GET /repositories/<id>/compare/<base>...<head>` by repository ID reports `ahead` or `identical` when the head descends from the base, and `behind` for an unmerged descendant of the head. Checked read-only during review. D6 lane 2 confirms it on the lab repository.
- A Boat Cloud runs against a real GitHub App, and runners reach a Boat Machine's grant door. Appendix B.

## Appendix D. Handoffs

- **Thread A.** Gaps 1, 3, 4, 6, 7, 8 and 10 above.
- **Thread B.** Gaps 2 and 8. Decision 6 changes what a session may approve for previews, and the Machine enforces it.
- **Thread C.** Gap 10. D6 uses C's `DeployApproval` unchanged, with a content-addressed built image. D7 widens `sealed_scope_allows` for reviews, as C4 left it to do, and adds gap 9's reserved Namespace refusal to `deploy::verify`. C's cache decision is applied as `d-build-proof.md` says.
- **Thread F.** `MintBuildGrant`, `Build` and the two new capability rows join F's coverage table. Removing the old dispatched workflow path belongs to F's cutover.
- **Operator, outside code.** Y's O1 and O2 on the `getployz` organization and the App, the minter's owner, and Appendix B.
