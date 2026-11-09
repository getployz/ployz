# Security 0.4 thread D plan, automatic GitHub deploy, builds and previews

A compromised Cloud must not run code of its choosing on a Server or read its secrets. Today it can do both through builds. Under Device enforcement, `Build` and `MintBuildGrant` are not gated (#4002). Cloud can start a Build whose `RUN` steps it wrote, with the Build's Sealed values opened, or mint a Build Grant for its own key and read any Service's Sealed values opened. Slice D1 closes that first. The later slices replace the blanket refusal with a proof each Machine checks for itself.

The design is in [d-build-proof.md](d-build-proof.md). A person approves an automatic deploy rule once. A pinned GitHub builder attests, through the audience of GitHub-signed OIDC tokens, which holder gets the build secrets and which image it built from which commit under which policy. Each Machine checks the token, the signed rule and the branch head it reads from GitHub itself. [d-inventory.md](d-inventory.md) lists every input Cloud chooses today and the slice that closes it.

Seven PRs, in dependency order:

| Slice | Outcome | Closes |
| --- | --- | --- |
| D1 | Enforcing Machines refuse Cloud's `Build` and `MintBuildGrant` | #4002, today's bar crossing |
| D2 | Machines can verify a GitHub OIDC token and read a branch head. Rule types exist. | Nothing yet. It is the verifier. |
| D3 | A push builds under enforcement. The grant is bound to a verified token. | Cloud picking the commit, inputs, builder or holder |
| D4 | A push deploys under enforcement with no prompt | Cloud substituting source, settings, artifact, stale or replayed proof |
| D5 | The dashboard approves and re-approves rules | Settings riding along shows as "approve again", not as a silent deploy |
| D6 | A person can deploy or roll back a built image under enforcement | Built-image deploys refused since thread C |
| D7 | Trusted previews deploy, and inherit only the Sealed values a rule names | Trust from Cloud rows, preview secrets kept apart by Cloud alone (checkpoint C3) |

## Operator decisions

Each one has a recommendation. Nothing below blocks D1 or D2.

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

## How to read this

Same conventions as `c-plan.md`. One box is one unit of work, and every box names the evidence that checks it. Check a box only when that evidence exists. Tests alone are not sufficient verification. A PR is verified only when its unit, live and perf boxes are all checked.

Terms follow `core/CONTEXT.md` (Build, Build Grant, Build Receipt, Uploaded Source, Deploy, Attempt Target, Requested Service Spec) and `dashboard/CONTEXT.md` (PR Environment, Approval card). "Rule", "preview rule", "template digest", "policy digest" and "build claim" are new and defined in `d-build-proof.md`. No log, test, fixture or screenshot prints a secret value or a whole OIDC token. Nothing written for this program names a Cloud host, an address or a path on Cloud's servers.

## Program checklist

The program runs the way `c-plan.md` "Program checklist" says: arming, the audit tick, owners, PR mechanics, verdict and merge. These differences apply.

- [ ] Dependency graph. D1 branches from `main` now. D2 after D1. D3 after D2 and after decisions 8 and 9. D4 after D3. D5 after D4 and after Appendix A gap 2 with thread B. D6 after D4. D7 after D4, and its prototype box closes before its build.
- [ ] File boundaries. D1 touches `core/crates/ployzd/src/machine_api/local.rs`, `core/crates/ployzd/src/machine/local_machine/restart_approval.rs`, `core/crates/ployz/src/sdk/github_build.rs` (refusal wording only) and `docs/user/`. D2 adds `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployzd/src/github/{oidc,head}.rs`. D3 touches `core/crates/ployzd/src/management/build_grant.rs`, `core/crates/ployz-core/src/rpc.rs` (two fields), `rpc_catalog.rs` (one row), `core/crates/ployz/src/handlers/build.rs`, `core/crates/ployz/src/sdk/github_build.rs`, `core/crates/ployz-store/src/build.rs`, `dashboard/src/modules/config-store/store-github*`, `dashboard/src/modules/github/**`, `.github/workflows/ployz-build.yml` and the external builder workflow. D4 touches `core/crates/ployz-core/src/authority/rule.rs`, `core/crates/ployzd/src/machine/local_machine/{deploy_approval,restart_approval}.rs`, `core/crates/ployz-core/src/rpc.rs` (one field), `core/crates/ployz-store/src/automation.rs`, `core/crates/ployz/src/sdk/store_runner.rs`, `core/crates/ployz/src/cluster.rs`, `core/crates/ployz/src/handlers/github.rs` and `docs/user/deploy/`. D5 touches `dashboard/src/modules/github/**`, `dashboard/src/modules/approvals/**` and the SDK export. D6 touches `core/crates/ployz-core/src/authority/deploy.rs` and `core/crates/ployz/src/handlers/deploy.rs`. D7 touches `core/crates/ployz-core/src/authority/{rule,deploy}.rs`, `core/crates/ployz-core/src/sealed.rs`, `core/crates/ployzd/src/sealed_env.rs`, `core/crates/ployz-store/src/pull_request/**`, `dashboard/src/modules/config-store/store-pull-request.server.ts` and `docs/user/`.
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

- [ ] Create `core/crates/ployz-core/src/authority/rule.rs` with `AutoDeployRule`, `RuleService`, `BuildPolicy`, `PolicyDigest`, `SealedDigest`, `BuildClaim` (with `audience()`) and `GithubClaims`, the parsed claims the rule checks. It has no crypto, so it builds for the browser SDK.
- [ ] Create `core/crates/ployzd/src/github/oidc.rs` with `verify(token, keys, now) -> GithubClaims` (RS256 through `ring::signature::RSA_PKCS1_2048_8192_SHA256`, fixed issuer) and `IssuerKeys`, which fetches the JWKS over TLS, caches it, and fetches again once on an unknown `kid`.
- [ ] Create `core/crates/ployzd/src/github/head.rs` with `branch_head(repository_id, branch, credential)`. It is a `GET https://api.github.com/repositories/<id>/git/ref/heads/<branch>` with a 10 second timeout, and it maps 404 to "unreadable".
- [ ] Commit fixtures under `core/crates/ployzd/src/github/fixtures/`. They are a real token from the Appendix B repository, the JWKS at that time and the claims, with the signature intact and the issued token long expired.

**Build.**

- [ ] `AutoDeployRule::verify_claims(&GithubClaims)` returns `Mismatch { field }` naming the first failing claim, in the table order of `d-build-proof.md`.
- [ ] `BuildClaim::audience` is `"ployz-build:" + hex(SHA-256(canonical JSON))`. It has a golden test whose bytes the builder copies in D3.
- [ ] The verifier never logs the token. A refusal logs the claim name and the rule ID.

**Verify, unit.**

- [ ] The real fixture verifies at its `iat`. Then one changed byte in each part is refused, and so are an unknown `kid`, another issuer, an expired token, `alg: none`, and HS256 signed with the JWKS modulus.
- [ ] `verify_claims` refuses, one per case, another `repository_id`, another owner, `ref_type: tag`, another branch, an event not in the rule, another `workflow_ref`, another `job_workflow_ref`, another `job_workflow_sha`, and `runner_environment: self-hosted`. Run `cargo nextest run -p ployz-core authority::rule` and `-p ployzd github::`.
- [ ] `PolicyDigest` and `BuildClaim::audience` golden tests.

**Verify, live.** Six lanes. Lanes marked **GitHub** use Appendix B.

- [ ] Lane 1. **GitHub.** A workflow in the test repository prints `exp - iat` and the claim names (not values) of its token. Save `d2-lifetime.txt`. Pass when the lifetime is recorded. It feeds D4's accepted-build record.
- [ ] Lane 2. **GitHub.** From a Boat Machine, `branch_head` on the private test repository with an installation token narrowed to `contents: read` and that one repository. Save `d2-private-head.png`. Pass when it returns the head `git ls-remote` shows.
- [ ] Lane 3. **GitHub.** The same with a token for another installation that cannot see the repository, and with none. Save `d2-head-refused.png`. Pass when both are "unreadable".
- [ ] Lane 4. **GitHub.** Re-run an old run of the test workflow. Save `d2-rerun.png`. Pass when its token's `sha` is the old commit and `run_attempt` went up, as thread Y's report says.
- [ ] Lane 5. **GitHub.** Force push the test branch back one commit. Save `d2-force.png`. Pass when the new push run's `sha` is the older commit and `branch_head` returns it.
- [ ] Lane 6. Block egress to `api.github.com` on one Machine. Save `d2-offline.png`. Pass when `branch_head` fails within 15 seconds.

**Verify, perf.**

- [ ] Metric. Token verification with a warm JWKS, and `branch_head` latency from a Boat Machine. Rule: verification under 2 ms, head read under 1 s at the median of ten.

**Review gate.** None.

## Build a push under enforcement (D3)

Outcome. On an enforcing Cluster with a rule, a push to the approved branch runs the pinned builder, and the grant Machine gives the build secrets only to the holder key in a verified token. The image lands on that Machine. It does not deploy until D4. Attack path closed. Cloud cannot pick the commit, the inputs, the builder, the runner or the holder, and cannot read build secrets.

**Depends on.** D2. Operator decisions 8 and 9.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/rpc.rs` so `MintBuildGrantRequest` gains `rule: Option<Signed<AutoDeployRule>>` and `proof: Option<String>` (the raw token). Edit `rpc_catalog.rs` to add `BUILD_GRANT_RULE_CAPABILITY: ("ployz.build.grant.rule.v1", Always)`.
- [ ] Edit `core/crates/ployzd/src/management/build_grant.rs` so `Mint::check` under enforcement needs the rule and proof, runs the grant checks in `d-build-proof.md`, consumes the `jti`, and checks the Sealed record against the rule's `secrets` digest. Edit `admit_operator_build` from D1 to let a rule-bearing `MintBuildGrant` through.
- [ ] Edit `core/crates/ployz/src/handlers/build.rs` (the runner's `ployz build`) to make the holder key, compute the policy digest from the relayed rule, request both tokens with their audiences, refuse any registry cache, and set the Actions cache `scope` to the policy digest.
- [ ] Edit `core/crates/ployz/src/sdk/github_build.rs` so check-in for a rule build returns the rule, the Cluster key and the grant Machine. It returns no commit and no `ployzVersion`. It relays the runner's grant token to `MintBuildGrant`.
- [ ] Edit `dashboard/src/modules/config-store/store-github-builds.server.ts` and `dashboard/src/modules/github/github-build.server.ts` so a rule build follows the push-triggered run of the rule's workflow, rather than dispatching one, and receives the image token with the final Build Steps report.
- [ ] Edit `core/crates/ployz-store/src/build.rs` so `github_authorize` matches a push run on the rule's branch by `run_id`. Claims checked there are bookkeeping only.
- [ ] Edit `.github/workflows/ployz-build.yml` to the file in `d-build-proof.md`.
- [ ] Outside this repository. Add the reusable workflow to `getployz/build` and record its commit in `core/crates/ployz-core/src/authority/rule.rs` as `BUILDER`, the default a CLI approval pins.

**Build.**

- [ ] The builder never checks out a commit from Cloud. It never passes `ACTIONS_ID_TOKEN_REQUEST_TOKEN` or `ACTIONS_RUNTIME_TOKEN` into BuildKit.
- [ ] Run the `verify-build-handoff` model check, since the walk changes.

**Verify, unit.**

- [ ] `build_grant.rs` tests use D2's fixture token with a rule built to match. A valid token mints. Then each grant check fails once: another holder in `aud`, another policy in `aud`, a `jti` already consumed, a Sealed record that does not match `secrets`, an old generation, an `ended` rule, a Machine not in `machines`, and a head that differs (with a stubbed head reader). A mint without a rule under enforcement is refused as in D1.
- [ ] `handlers/build.rs` tests show the audience bytes equal D2's golden test, and a policy with `cache_from` of type `registry` refused before the build.
- [ ] `pnpm vitest run src/modules/config-store/store-github-builds` covers following a push run and receiving the image token.

**Verify, live.** Ten lanes, all **GitHub**, against a Boat Cluster with enforcement on and a rule approved from `cli --device`. D3 has no rule CLI yet, so the lanes sign the rule with a test helper. D4 adds the CLI.

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

**Verify, perf.**

- [ ] Metric. Push to image landed, at trunk with the old workflow and enforcement off, and at head with a rule. Five runs each side. Rule: head within 1.2 times trunk, mostly the second token request and a head read.

**Review gate.** The operator reviews the new workflow file and the builder before merge, since customers copy them. Post both in chat with lane 2's screenshot.

## Deploy a push under enforcement (D4)

Outcome. With a rule approved once from the CLI, every push to the approved branch deploys on an enforcing Cluster with no prompt, force pushes included. A settings change stops automatic deploys until the rule is approved again. Attack path closed. Cloud cannot deploy another commit, another image or other settings, or replay old proof.

**Depends on.** D3. Appendix A gaps 1 and 3.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/authority/rule.rs` to add `verify_creation`, the creation checks in `d-build-proof.md`, and `RuleProof { rule, token }`.
- [ ] Edit `core/crates/ployz-core/src/rpc.rs` so `EnsureContainerRequest` gains `rule: Option<RuleProof>`. Edit `rpc_catalog.rs` to add `DEPLOY_RULE_CAPABILITY: ("ployz.container.deploy-rule.v1", Always)`.
- [ ] Edit `core/crates/ployzd/src/machine/local_machine/deploy_approval.rs` to add `admit_rule_creation` and the rule's lifecycle steps. Edit `restart_approval.rs` so a creation with a rule goes there. The approval record gains the generation high-water mark per rule, accepted builds, and hook consumption per (rule, generation, commit, Service).
- [ ] Edit `core/crates/ployz-store/src/automation.rs` so a push admitted under a rule records the rule and waits for the image token. A Saved revision whose template no longer matches makes the push wait as "approve the rule again", with the changed Services listed.
- [ ] Edit `core/crates/ployz/src/sdk/store_runner.rs` and `core/crates/ployz/src/cluster.rs` so the runner attaches the rule proof to every `EnsureContainer` of that Deployment.
- [ ] Create the CLI commands `ployz github rule approve <service>...` and `ployz github rule end`. Approve prints the repository, branch, Environment, builder commit, each Service's policy and the settings it pins, then signs with the Device key. Run the `ui-copy` skill.
- [ ] Edit `docs/user/deploy/github.md` and `docs/user/servers/manage-servers.md`.

**Verify, unit.**

- [ ] `rule.rs` tests take one approved spec and token, and change one thing per case, each refused naming it. The cases are the image digest, a plain value, a Sealed ciphertext, `privileged`, a Bind Mount, a host port, the hook command, the generation (lower), the commit (not the head), and `aud` naming another image.
- [ ] `deploy_approval.rs` tests admit a second creation of the same image after the token's `exp` through the accepted-build record, and refuse it once the head moved. They also admit a hook once per commit.
- [ ] `automation.rs` tests hold a push for re-approval when the Saved revision changes a setting, and admit it when only the commit changed. Run `cargo nextest run -p ployz-core authority::rule`, `-p ployzd deploy_approval`, `-p ployz-store automation`.

**Verify, live.** Ten lanes, all **GitHub**, two Machines, enforcement on, rule approved with `cli --device -- github rule approve web`.

- [ ] Lane 1. Regression lane against trunk. Enforcement off. A push deploys at trunk and at head. Save `d4-enforce-off.png`.
- [ ] Lane 2. A push deploys with no prompt. Save `d4-push.png`. Pass when both Machines run the new digest and log `rule <id> gen <n> commit <12 hex>`.
- [ ] Lane 3. A force push to an older commit deploys. Save `d4-force.png`. Pass when both Machines run that commit's image.
- [ ] Lane 4. Attacker harness. Replay lane 2's `EnsureContainer` after lane 3. Save `d4-stale.png`. Pass when it is refused naming the commit.
- [ ] Lane 5. Attacker harness. Lane 2's request with an image Cloud pushed through `EnsureImageIngest`. Save `d4-artifact.png`. Pass when it is refused naming the image.
- [ ] Lane 6. Attacker harness. Lane 2's request with `privileged: true`, then with a new Sealed variable. Save `d4-settings.png`. Pass when both are refused naming the payload.
- [ ] Lane 7. Change a plain variable in the dashboard, Save, and push. Save `d4-reapprove.png`. Pass when the push waits as "approve the rule again", and after `github rule approve` the next push deploys at generation n+1.
- [ ] Lane 8. Attacker harness. Replay a generation n token after lane 7. Save `d4-old-gen.png`. Pass when it is refused naming the generation.
- [ ] Lane 9. A pre-deploy hook that appends a line. Push once, then replay the hook creation. Save `d4-hook.png`. Pass when the file has one line.
- [ ] Lane 10. `github rule end`, then push. Save `d4-ended.png`. Pass when nothing deploys and the Machines log the ended rule.

**Verify, perf.**

- [ ] Metric. Push to Deployment success, at D3's head with enforcement off and at D4's head with a rule, five each, interleaved. Rule: within 1.2 times. Admission per creation under 300 ms at the median, which includes one head read.

**Review gate.** The operator reviews the CLI output. Post lanes 2 and 7 with a 30 to 60 second terminal video, saved under `docs/agents/plans/security-0.4/media/d4-review.*`.

## Approve rules in the dashboard (D5)

Outcome. A dashboard user turns automatic deploy on for an Environment with one passkey prompt. A held push shows an Approval card listing the changed settings, and one prompt approves the next generation. Attack path. A session grant cannot sign a rule.

**Depends on.** D4. Appendix A gap 2 with thread B.

**Files.**

- [ ] Edit `core/crates/ployz-sdk/src/lib.rs` to add `autoDeployRuleBody(view, signer)`, which returns the canonical body and the change lines. Export the rule types from `@ployz/sdk/authority`.
- [ ] Edit `dashboard/src/modules/github/**` and `dashboard/src/modules/approvals/**` for the rule card and its prompt, through B's passkey-direct helper.

**Verify.** Unit tests for the body's golden digest and for a session-signed rule refused. Live per `verify-dashboard` with `SERVERS=1`. Record turning a rule on with one prompt, a held push shown as a card, approving it, the next push deploying, and a session-only attempt refused by the Machine. Check light and dark themes and phone width per `dashboard/DESIGN.md`. Perf: the body is built in under 20 ms for 20 Services.

**Review gate.** The operator reviews screenshots and a video before merge.

## Approve a built image deploy (D6)

Outcome. A person deploys or rolls back a Git-sourced Service at a named commit under enforcement, from the CLI or the dashboard, with thread C's deploy approval. Attack path. Cloud cannot swap the image for one built from another commit or policy.

**Depends on.** D4.

**Build.**

- [ ] `ApprovedService` (`ployz-core/src/authority/deploy.rs:139`) gains `built: Option<Built { rule, commit, policy }>`. When present, the digest is the template digest (image emptied), and the creation needs an image token whose `aud` names that commit. A person's approval replaces the head check.
- [ ] `ployz deploy` of a Git Service builds it under the rule, then signs the approval with the commit it printed.

**Verify.** Unit tests change the commit, the policy and the image, one per case. Live, **GitHub**: a rollback to the previous commit deploys, a substituted commit is refused, and the Machines log the approver. Perf as C1.

## Previews under a rule (D7, checkpoint C3)

Outcome. Pushes by trusted GitHub accounts to branches of the repository deploy their PR Environment with no review. A preview inherits only the parent Sealed values the preview rule names, opened in place on the Machine. Untrusted PRs keep the review path, bound to the reviewed commit. Attack path closed. Cloud's membership rows and bot flag no longer decide trust. Cloud cannot give a preview a parent secret the rule does not name.

**Depends on.** D4. The prototype box below.

**Prototype first.**

- [ ] Lower a parent Environment and its PR Environment for the same commit, and diff their requested specs. Pass when they differ only in the Namespace, preview hostnames and Own Copy Volume names, so `PreviewTemplate::of` can name every PR-derived field. If more fields differ, stop and redesign the template before building.

**Files.**

- [ ] Edit `core/crates/ployz-core/src/sealed.rs` so `SealedValue` gains `environment: Option<Uuid>`, which replaces the identity's Environment in the associated data.
- [ ] Edit `core/crates/ployzd/src/sealed_env.rs` to open such a value only when admission handed it the preview rule's `inherit` list for that Environment.
- [ ] Edit `core/crates/ployz-core/src/authority/rule.rs` to add `PreviewRule`, `PreviewTemplate` and `PreviewReview`. Edit `deploy.rs` to widen `sealed_scope_allows` to inherited keys under a review.
- [ ] Edit `core/crates/ployz-store/src/pull_request/mod.rs` and `dashboard/src/modules/config-store/store-pull-request.server.ts` so trust comes from the preview rule. The members join stays only as the label shown before a rule exists.
- [ ] Edit `docs/user/environments/preview-environments.md` and `docs/user/services/variables.md`.

**Verify, unit.** An inherited value opens under its rule, and is refused for a key not in `inherit`, for another parent, under a production deploy approval, and relabelled as another key. An untrusted `actor_id` is refused. A review is refused for another commit. An older daemon refuses the new field.

**Verify, live.** Ten lanes, all **GitHub**. A trusted account's push deploys a preview that reads `DATABASE_URL` (length only) and not `STRIPE_KEY`. An untrusted account's push waits for review. A review deploys exactly that commit. Adding the untrusted account to Cloud's `member ⋈ account` rows changes nothing. Relaying the parent's `STRIPE_KEY` ciphertext to the preview is refused. A fork PR does not count as trusted. Lane 1 is a regression lane against trunk with enforcement off.

**Verify, perf.** Admission per creation within 10 ms of D4.

**Review gate.** The operator reviews the preview rule UI before merge.

## Appendix A. Contract gaps

1. **A `Rule` purpose (thread A).** `Purpose` is `Workflow`, `Join` and `Enforce` (`ployz-core/src/authority/keys.rs:23`). A rule must not be signable by a session grant, which `Workflow` allows. D2 needs `Purpose::Rule`, which `verify_signers` admits for a Device or a passkey's own assertion only.
2. **Passkey-direct signing in the dashboard (thread B).** D5 needs a passkey assertion over a rule body outside a session. B's enrollment and proposal approval already prompt directly, so the helper may exist. Confirm it with the B owner.
3. **Generation and accepted-build records (thread A).** D4 adds a per-rule high-water mark and an accepted-build entry to the consumption log. Like C1's hook entry, this is "C inside authority", and A must agree to the record kinds.
4. **`SealedValue.environment` (threads A and C).** D7 changes Sealed associated data. That is a wire change in a type C's digest covers. An older daemon refuses it, which fails closed.
5. **The GitHub minter (no owner).** See operator decision 1.

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
- **Documented, not run.** A job asks for a token with a custom audience through `$ACTIONS_ID_TOKEN_REQUEST_URL&audience=…`. `job_workflow_ref` and `job_workflow_sha` name the reusable workflow and its commit.

Unproven until the slice that proves it:

- An installation token narrowed to `contents: read` and one repository reads the head by repository ID. D2 lane 2.
- The token lifetime. D2 lane 1.
- A re-run keeps `sha` and raises `run_attempt`, and a force push gives the new tip as `sha`. D2 lanes 4 and 5.
- An audience as long as `ployz-build:` plus 64 hex characters is accepted. D3 lane 2.
- Actions cache isolation by branch. D3 lane 9.
- A PR Environment's spec differs from its parent's only in fields `PreviewTemplate` names. D7's prototype.
- A Boat Cloud runs against a real GitHub App, and runners reach a Boat Machine's grant door. Appendix B.

## Appendix D. Handoffs

- **Thread A.** Gaps 1, 3 and 4 above.
- **Thread B.** Gap 2. Decision 6 changes what a session may approve for previews.
- **Thread C.** D6 extends `ApprovedService`. D7 widens `sealed_scope_allows`, as C4 left it to do. C's cache decision is applied as `d-build-proof.md` says.
- **Thread F.** `MintBuildGrant`, `Build` and the two new capability rows join F's coverage table. Removing the old dispatched workflow path belongs to F's cutover.
- **Operator, outside code.** Y's O1 and O2 on the `getployz` organization and the App, the minter's owner, and Appendix B.
