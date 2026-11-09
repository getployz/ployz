# Thread D inventory: where Cloud chooses build and deploy inputs

This file lists every place where Ployz Cloud chooses an input to a Build, to a Build Grant, to an automatic deploy or to a preview, as of `main` at `7c5cc4571e`. Each row says what that input reaches on a Machine and which slice in [d-plan.md](d-plan.md) closes it. [d-build-proof.md](d-build-proof.md) holds the design. Paths are relative to `core/crates/` unless they start with `dashboard/` or `.github/`.

A row **crosses the bar** when a compromised Cloud can use it, under Device enforcement, to read secrets, run code of its choosing on a Server or take data off it. Other rows are inputs the design must still bind, so the closing slices name them.

## The kickoff anchors, checked against main

| Kickoff anchor | Where it is now | State |
| --- | --- | --- |
| `ployzd/src/management/build_grant.rs:111` | `Mint::check` at `:111`. It takes any `holder` key (`:113`) and any Sealed record (`:128`) and opens the record to check it (`:143`). | Open under enforcement. **Crosses the bar.** |
| `ployzd/src/management/build_grant.rs:480` | `sealed` at `:482` serves the opened values to the grant's holder once (`GET /ployz/sealed`, route at `:355`). | Open. With the row above, Cloud names its own key as holder and reads any Service's opened Sealed values. **Crosses the bar.** |
| `dashboard/src/modules/github/github-build.server.ts:74` | `dispatchGithubBuildWorkflow` dispatches on the default branch with Cloud's `inputs` (`:63`, body at `:74`). | Open. The run checks out the commit Cloud returns at check-in, not the run's own. |
| `dashboard/src/modules/config-store/store-github-builds.server.ts:229` | `runnerClaims` at `:234`. It verifies the token with the Cloud origin as audience (`:239`), and the Store compares claim strings (`ployz-store/src/build.rs:1052`). | Open. Only Cloud checks GitHub's signature. No Rust code verifies a JWT. |
| Settings riding along with auto-deploys | `ployz-store/src/automation.rs:690` admits the latest Saved revision, whatever changed in it since the last approval. | Open. |

## Machine routes (closed first, by D1)

| Route | Catalog | Scope | Gated under enforcement today | What Cloud chooses | Reaches |
| --- | --- | --- | --- | --- | --- |
| `Build` | `ployz-core/src/rpc_catalog.rs:111` | Operator | **No.** The handler (`ployzd/src/machine_api/local.rs:665`) checks only `accepts_builds`, concurrency and the queue (`ployzd/src/build.rs:104`, `:117`). | The whole build: source files, recipe, `Definition` (`ployz-build/src/remote.rs:48`), output (Load, Registry or Validate), image contexts and their peer address (`ployzd/src/build.rs:415`), Docker `auths` and `proxies` (`ployz-build/src/received_recipe.rs:252`), the Sealed record and the identity it names (`ployzd/src/build.rs:375`, `ployzd/src/build/sealed.rs:14`) | BuildKit `RUN` steps on the Server, with Sealed values opened as secret files and with network egress. **Crosses the bar** (#4002). |
| `MintBuildGrant` | `rpc_catalog.rs:93` | Operator | **No** (`local.rs:946`) | Holder key, Sealed record, repository | Opened Sealed values to any key Cloud names. One image push under any repository name. **Crosses the bar** (#4002). |
| `InspectBuildGrant` | `rpc_catalog.rs:94` | Operator | No | Grant ID | A read of what a grant pushed. Not on the bar. |
| `EnsureImageIngest`, `PullImage`, `PullImageFromMachine` | `rpc_catalog.rs:90` to `:92` | Operator | No | Any image under any tag | The image store. Thread C made deploys content-addressed, so a planted tag runs nothing. Not D's. |

Under enforcement, a deploy of a built image is already refused. A built image is named by the tag `<repository>:ployz-sha256-<hex>` (`ployz/src/handlers/build.rs:296`), and thread C's `content_addressed` (`ployz-core/src/authority/deploy.rs:122`) refuses a tag. So today a Cloud-started build on an enforcing Machine wastes work and opens secrets, and its image can never deploy.

## The GitHub build walk

| What Cloud chooses | Where | Reaches | Closed by |
| --- | --- | --- | --- |
| That a build runs, and on which ref (the default branch) | `dashboard/src/modules/github/github-build.server.ts:74` | The run | D3. The customer's workflow runs `on: push`, and a dispatch only builds the head. |
| `build`, `cloud` and `runner` dispatch inputs. `runs-on: ${{ inputs.runner }}` can pick a self-hosted label. | `.github/workflows/ployz-build.yml:4`, `:13` | The run, and where the runner sends its token | D3. The new workflow takes no inputs. `runner_environment` is checked. |
| The workflow ref the Store expects | `dashboard/src/modules/config-store/store-github-builds.server.ts:157` | Claim comparison in the Store | D3. The rule pins `job_workflow_ref` and `job_workflow_sha`. |
| The commit the runner checks out | `ployz/src/sdk/github_build.rs:212`, returned at `:267` | Source built | D3. The builder checks out `github.sha`. |
| The `ployz` version the runner installs | `ployz/src/sdk/github_build.rs:268` | The build tool on the runner | D3. The builder pins it at its own commit. |
| The fingerprint and the narrowed Deployment input (the build recipe) | `ployz/src/sdk/github_build.rs:213`, `:157` | BuildKit on the runner | D3. The builder reports the policy digest in `aud`. |
| The Sealed record sent to the grant Machine | `ployz/src/sdk/github_build.rs:219`, `:280` | Opened by the Machine for the holder | D1, then D3 (rule `secrets` digest) |
| The holder key | `dashboard/src/modules/config-store/store-github-builds.server.ts:262` (relayed from the runner), sent at `ployz/src/sdk/github_build.rs:316` | Who receives opened Sealed values | D1, then D3 (holder bound in `aud`) |
| Which Machine mints the grant | `ployz/src/sdk/github_build.rs:299` | Where the push lands | None needed. Outage only. |
| The digest the build produced, read through `InspectBuildGrant` | `ployz/src/sdk/github_build.rs:524` | The Deployment's image | D4. The Machine admits only the digest in a GitHub-signed `aud`. |
| The OIDC verdict | `dashboard/src/modules/github/github-oidc.server.ts:51`, audience = Cloud origin at `store-github-builds.server.ts:239` | Whether a runner gets a grant | D2 and D3. Machines verify the raw token themselves. |
| The builder action `getployz/build@v1`, a mutable tag | `.github/workflows/ployz-build.yml:15` | Code that runs in the job holding `id-token: write` and the grant | D3. A reusable workflow pinned by commit. |

## Automatic push deploy

| What Cloud chooses | Where | Reaches | Closed by |
| --- | --- | --- | --- |
| The branch head, read from GitHub by Cloud | `dashboard/src/modules/config-store/store-github.server.ts:127` | Which commit deploys | D4. Each Machine reads the head itself. |
| Changed paths, which pick the Services that deploy | `dashboard/src/modules/config-store/store-github.server.ts:89`, `ployz-store/src/automation.rs:708` | Which Services redeploy | None needed. Choosing fewer Services is dropping work. |
| The check-suite wait | `ployz-store/src/automation.rs:322`, `:760` | When it deploys | None needed. Delay only. |
| The Saved revision deployed | `ployz-store/src/automation.rs:690`, admitted at `:1069` | Every setting of every Service | D4. The spec must match the rule's `template`. |
| Per-Service commit pins | `ployz-store/src/automation.rs:779`, `ployz-store/src/build.rs:219` | Which commit each Service builds | D4. The pinned commit must be the head the Machine reads. |
| Build Receipt reuse, which skips a build | `ployz/src/sdk/preparation.rs:172`, `ployz-store/src/build.rs:1129` | Which image deploys | D4. A reused image still needs a token naming it for the current head. |
| The source tarball, downloaded by Cloud by commit | `dashboard/src/modules/github/github-source.server.ts:225` | Servers builder source. Nothing checks the bytes against the commit. | D1 (Cloud cannot start `Build` under enforcement). A later Servers-builder slice is an operator decision. |
| Uploaded Source and its digest check, which runs in the Cloud-hosted SDK | `dashboard/src/modules/config-store/upload.server.ts:80`, `ployz/src/sdk/preparation.rs:207` | Servers builder source | D1, as above |

## Previews (PR Environments)

| What Cloud chooses | Where | Reaches | Closed by |
| --- | --- | --- | --- |
| Trusted or not: same repository, and the author is a bot or in the members list | `ployz-store/src/pull_request/mod.rs:211` | Whether every push to the PR deploys without review | D7. The preview rule's signed `trusted` list of GitHub account IDs, checked against `actor_id`. |
| The members list, from Cloud's `member ⋈ account` join | `dashboard/src/modules/config-store/config-store.server.ts:239` | Trusted | D7 |
| The `bot` flag and the PR facts (author, head repository, head commit) | `dashboard/src/modules/config-store/store-pull-request.server.ts:46` | Trusted, and the commit built | D7. `repository_id` and `actor_id` come from GitHub's token. |
| The reviewed head for an untrusted PR | `ployz-store/src/pull_request/mod.rs:415`, `:1125` | The only commit that deploys | D7. A browser-signed `PreviewReview` names the commit, and `aud` must match it. |
| The PR Environment's settings, published without review | `ployz-store/src/pull_request/mod.rs:1411` | Every setting in the preview | D7. The spec must match the preview rule's normalised template. |
| Auto-deploy forced on for the repository's Services | `ployz-store/src/pull_request/mod.rs:1387` | That pushes deploy | None needed once each deploy needs the proof |
| Setup commands from the plan | `ployz-store/src/pull_request/mod.rs:1343` | Code run in the Own Copies | D7. They are in the spec, so the template covers them. |
| Which Sealed values a preview Deploy carries | `ployz-store/src/variables.rs:1126` keeps only values sealed for the deploying Environment. The parent's become Not set. | Sealed open | Today this keeps parent values out, but only Cloud enforces it. If Cloud sends the parent's ciphertext with the parent's `SealedIdentity` (`ployz-store/src/deployment/lowering.rs:906`), the Machine opens it. Under enforcement thread C's deploy approval binds the identity, and C4's `sealed_scope_allows` stops a session from crossing Environments. D7 adds the inherited path. |
| Registry pull credentials, sealed with the Store's own key and copied into PR Environments | `ployz-store/src/registry.rs:215`, `ployz-store/src/branch/pair.rs:2306` | Image pulls | Not D's. Cloud already holds these. A pull credential cannot change a content-addressed image. |

## Caches

Thread C decided these (`c-decisions.md`). D applies them in its policy.

| Cache | Where | Under a rule |
| --- | --- | --- |
| BuildKit on the runner | Fresh per GitHub-hosted job | Nothing to trust |
| `cache_from` and `cache_to` of type `registry` or `inline` | Allowed by `ployz-build/src/received_recipe.rs:220`. Cloud's capture does not set them today. | Refused in a rule's policy |
| GitHub Actions cache | `ployz-build/src/lib.rs:186`, `:706`. On only when `ACTIONS_RUNTIME_TOKEN` is set. | Scoped by the policy digest. Proven or turned off in D3. |
| BuildKit on Machines | `ployzd/src/build.rs` | Cloud cannot write it under enforcement after D1 |

## What is not on D's list

- `ployz-build/src/policy.rs` is host resource policy (CPU, memory, cache size), set from the Machine's own `build.yaml`. It is Machine-local, and nothing Cloud sends changes it.
- Build network egress (`ployz-build/src/received_recipe.rs:204`). A Dockerfile can send a build secret out. That is approved code leaking its own secrets, the residual every thread carries.
- Log scrubbing of build output opens values only to hide them, and the plaintext stays on the Machine.
