# Hosted Beta Acceptance

This runbook records the combined hosted Cloud and Core/CLI beta acceptance
run. It preserves the settled claims and proof boundaries from the hosted-beta
contract; it does not absorb product implementation, fixture implementation,
or release certification.

Run the successful journey and all three failure journeys in one sitting, on
one cluster, in this order:

```text
S → F1 → F2 → F3
```

Post the completed record as a comment on
[#391](https://github.com/getployz/ployz/issues/391). Keep the full transcripts
and screenshots at the evidence locations named in that comment.

## Proof rules

- Use the exact candidate for every proof and record its version and commit.
- Check all 27 stable-numbered claims. No claim may be left unchecked,
  weakened, renumbered, or substituted.
- The `Proof` column is authoritative. Automated proof may be linked in the
  record, but it never substitutes for a claim classified as `manual`.
- A claim with more than one proof classification requires every listed proof.
- The private GitHub repository is load-bearing. Its GitHub App installation
  token must be required for clone; a public repository cannot prove that
  authentication seam.
- Evidence must cover both native architectures (`amd64` and `arm64`) and both
  build adapters (Dockerfile and Railpack).
- Evidence must show that the volume is an actual ZFS dataset and that the
  hosted Cloud under test is opted into ZFS. Volume survival alone is
  insufficient.
- Evidence must cover both managed and custom HTTPS.
- Failure evidence must show the failure, a fresh retry operation, and
  preservation of the prior operation's evidence where the checklist requires
  it.

The proof classifications are:

- `unit`: in-process deterministic test
- `dind`: DinD suite
- `real-host`: `scripts/real-host-acceptance.sh`
- `manual`: only this recorded human run proves the claim

## Prerequisites

- A sealed release candidate promoted to the channel used by the public
  installer, with all automated gates green on that exact candidate.
- A fresh Rocky Linux 9 amd64 core and Ubuntu 24.04 arm64 edge satisfying the
  prerequisites in [Real-Host Acceptance](real-host-acceptance.md).
- Access to the hosted Dashboard under test, GitHub OAuth, the Ployz GitHub
  App, DNS, TLS, and the custom domain used for the run.
- Access to the private `getployz/ployz-beta-fixture` repository and its
  reviewed successful, compile-failure (F1), and boot-crash (F2) commits.
- Permission to create and delete one unique ephemeral fixture branch such as
  `acceptance/<run-id>`. Never mutate or force-push `main` or the standing
  `scenario/*` refs.
- The fixture's Dockerfile service, Railpack service with no Dockerfile, and
  volume-backed database.
- Hosted Cloud configured to opt the acceptance machines into ZFS preparation.
- A durable directory for command transcripts, outputs, timings, screenshots,
  and automated-test evidence.

## Candidate metadata and evidence

Record these fields before starting:

- exact version under test
- Core/CLI commit SHA and release/channel
- Dashboard commit SHA and deployment identifier
- fixture repository and successful, F1, and F2 commit SHAs
- ephemeral watched branch name
- core and edge provider identifiers, OS versions, native architectures, and
  public IPs
- managed hostname, custom hostname, and lease apex
- start and finish timestamps in UTC
- evidence root

For each push, record the ephemeral branch name, its prior and new exact SHAs,
the guarded push command and output, the resulting webhook, and the resulting
build operation identifier. Move the branch with `--force-with-lease` or an
equivalent expected-old-SHA guard so a concurrent change cannot be clobbered.

For every journey step, preserve the evidence fields verbatim:

- commands
- output
- timings
- Dashboard screenshots
- evidence locations

Redact credentials without removing proof that the private clone used GitHub
App-token authentication.

## Stable checklist

### Onboarding

| # | Claim | Proof |
| --- | --- | --- |
| 1 | Sign up at ployz.dev via GitHub OAuth, land in an org/project | `manual` |
| 2 | "Add machine" renders one copy-paste Cloud Bootstrap Token command | `manual` |
| 3 | amd64 machine joins | `real-host` — green today |
| 4 | arm64 machine joins | `real-host` — green today |
| 5 | Host firewall opens exactly the Ployz ports | `real-host` — green today |

### Push to build to deploy

| # | Claim | Proof |
| --- | --- | --- |
| 6 | Connect the private fixture repo; App install mints a token | `manual` |
| 7 | `git push` → webhook → build starts, no CLI touched | `manual` |
| 8 | Dockerfile service builds | `manual` + `unit` (adapter) |
| 9 | Railpack service builds — no Dockerfile, provider auto-detected | `manual` + `unit` (adapter) |
| 10 | Both build native amd64 **and** arm64 | `manual` |
| 11 | One platform-independent index digest | `unit` — #513 assigned |
| 12 | All-or-nothing: one arch fails ⇒ no index published | `unit` — #513 assigned |
| 13 | Replicas land on both machines | `real-host` — green today |

### Public HTTPS

| # | Claim | Proof |
| --- | --- | --- |
| 14 | Hosted Dashboard assigns `https://<service>-<env4>.<lease>.up.ployz.app` and it returns 200 | `manual` |
| 15 | Custom domain → CNAME to the lease apex → certificate → 200 | `manual` |
| 16 | Route survives a control-daemon restart | `real-host` — green today |

### Volumes

| # | Claim | Proof |
| --- | --- | --- |
| 17 | Write a row → redeploy → the row survives | `manual` |
| 18 | The volume is a ZFS dataset on the machine that holds it | `manual` |
| 19 | The hosted Cloud under test is opted into ZFS | `manual` |

### Evidence and retry

| # | Claim | Proof |
| --- | --- | --- |
| 20 | F1: the real compile error is visible in the Dashboard build log | `manual` |
| 21 | F1: the live environment is untouched | `manual` |
| 22 | F1: fix + push → green | `manual` |
| 23 | F2: typed failure, failed container retained for inspection | `dind` |
| 24 | **F2: the previous version is still serving** | `dind` — see below |
| 25 | F2: retry is a new operation, prior failure's evidence kept | `dind` |
| 26 | F3: core down ⇒ the request fails within its bound as typed `NoResponders` or `TimedOut`, with no operation accepted | `dind` (rejection) + `manual` (no accepted operation) |
| 27 | F3: gateway/DNS keep serving last-known-good | `dind` |

## Single-sitting procedure

### S — successful journey

1. Capture the candidate metadata and start timestamp. Link the green unit,
   DinD, and real-host transcripts for the exact candidate.
2. Sign up through GitHub OAuth and verify the resulting organization and
   project.
3. In the Dashboard, choose "Add machine" and capture the single copy-paste
   Cloud Bootstrap Token command. Use it to join the fresh amd64 and arm64
   machines. Record the firewall and architecture evidence.
4. Connect the private fixture repository through the GitHub App. Record the
   installation-token minting and authenticated private clone without exposing
   the credential. Create a unique ephemeral `acceptance/<run-id>` branch at
   the exact reviewed S commit, and configure the Dashboard environment to
   watch that branch. Never repoint the environment to a different branch.
5. Perform the initial guarded Git push that creates only the ephemeral branch
   at S. Do not invoke the CLI. Record the branch name, expected absent ref,
   new exact S SHA, push command and output, webhook, build start, and build
   operation identifier in the Dashboard.
6. Prove that the Dockerfile and Railpack adapters each build native `amd64`
   and `arm64` artifacts. Record the platform-independent index evidence and
   replicas placed on both machines.
7. Record the exact managed hostname assigned by the hosted Dashboard and
   prove HTTPS 200. Configure the custom domain's CNAME to the lease apex, wait
   for its certificate, and prove HTTPS 200.
8. Write a distinctive row to the volume-backed database, redeploy, and read
   the same row. On the machine holding the volume, record the actual ZFS
   dataset. In the Dashboard, record the hosted Cloud ZFS opt-in and its
   storage-preparation operation evidence.
9. Preserve commands, output, timings, Dashboard screenshots, and evidence
   locations for S before continuing.

### F1 — compile failure

1. With the ephemeral branch still at the exact reviewed S SHA, use
   `--force-with-lease` or equivalent expected-old-SHA semantics to push only
   that branch to the exact reviewed F1 commit containing the known compile
   error. Record the S-to-F1 SHAs and all push, webhook, and build-operation
   evidence.
2. Record the real compile error in the Dashboard build log and prove the live
   environment remains unchanged and serving.
3. Fix by guarded push of only the ephemeral branch from the exact reviewed F1
   SHA back to the exact reviewed S SHA. Record the F1-to-S SHAs, push output,
   webhook, and new build operation; prove that build and deploy become green.
4. Preserve commands, output, timings, Dashboard screenshots, and evidence
   locations for F1 before continuing.

### F2 — boot crash

1. With the ephemeral branch back at the exact reviewed S SHA, use a guarded
   push to move only that branch to the exact reviewed F2 commit that builds
   successfully and crashes on boot. Record the S-to-F2 SHAs and all push,
   webhook, and build-operation evidence.
2. Record the Dashboard-visible operation and failure context. Link the
   exact-candidate DinD evidence proving claims 23–25; the human observation
   does not replace that `dind` proof.
3. Retry deliberately and record its new operation identifier alongside the
   unchanged identifier and retained evidence of the prior failure.
4. Prove the previous version remains serving, then guarded-push only the
   ephemeral branch from the exact reviewed F2 SHA back to the exact reviewed
   S SHA. Record the F2-to-S SHAs, push output, webhook, and build operation;
   wait for the successful version before continuing.
5. Preserve commands, output, timings, Dashboard screenshots, and evidence
   locations for F2.

### F3 — core unavailable

1. Record `ployz ops list`, then stop the control-plane core while the
   successful version is serving.
2. Attempt an operation with a unique input marker and preserve its bounded
   non-zero `NoResponders` or `TimedOut` request rejection. Confirm the gateway
   and DNS continue serving last-known-good.
3. Link the exact-candidate DinD evidence proving the bounded rejection and
   last-known-good serving. The before/after operation lists supply claim 26's
   separate `manual` proof that nothing was accepted.
4. Restore the core, record `ployz ops list` again, and prove the rejected
   request created no operation or evidence. Record recovery and capture the
   finish timestamp.
5. Preserve commands, output, timings, Dashboard screenshots, and evidence
   locations for F3. Delete the ephemeral branch after preserving its final S
   SHA and the guarded deletion command and output. Do not delete or change
   `main` or any standing `scenario/*` ref.

## Completed-record template

Copy the following into a comment on #391 and replace every placeholder. Do
not post it with any unchecked claim.

```markdown
## Hosted beta acceptance — completed record

### Candidate

- Exact version under test: `<version>`
- Core/CLI commit SHA and release/channel: `<sha; release; channel>`
- Dashboard commit SHA and deployment: `<sha; deployment>`
- Fixture repository and commits: `getployz/ployz-beta-fixture`; S `<sha>`;
  F1 `<sha>`; F2 `<sha>`
- Ephemeral watched branch: `acceptance/<run-id>`
- Core host: `<provider id; Rocky 9 version; amd64; public IP>`
- Edge host: `<provider id; Ubuntu 24.04 version; arm64; public IP>`
- Managed hostname: `<hostname>`
- Custom hostname and lease apex: `<hostname>; <apex>`
- Run window UTC: `<start> → <finish>`
- Evidence root: `<location>`

### Journey evidence

| Journey | Commands | Output | Timings | Dashboard screenshots | Evidence locations |
| --- | --- | --- | --- | --- | --- |
| S | `<commands>` | `<output>` | `<timings>` | `<links>` | `<locations>` |
| F1 | `<commands>` | `<output>` | `<timings>` | `<links>` | `<locations>` |
| F2 | `<commands>` | `<output>` | `<timings>` | `<links>` | `<locations>` |
| F3 | `<commands>` | `<output>` | `<timings>` | `<links>` | `<locations>` |

Ordered single-sitting execution: S `<time>` → F1 `<time>` → F2 `<time>`
→ F3 `<time>`.

### Guarded fixture-branch transitions

| Transition | Branch | Expected prior SHA | New SHA | Push output | Webhook | Build operation |
| --- | --- | --- | --- | --- | --- | --- |
| create S | `acceptance/<run-id>` | `<absent>` | `<S SHA>` | `<evidence>` | `<evidence>` | `<operation id>` |
| S → F1 | `acceptance/<run-id>` | `<S SHA>` | `<F1 SHA>` | `<evidence>` | `<evidence>` | `<operation id>` |
| F1 → S | `acceptance/<run-id>` | `<F1 SHA>` | `<S SHA>` | `<evidence>` | `<evidence>` | `<operation id>` |
| S → F2 | `acceptance/<run-id>` | `<S SHA>` | `<F2 SHA>` | `<evidence>` | `<evidence>` | `<operation id>` |
| F2 → S | `acceptance/<run-id>` | `<F2 SHA>` | `<S SHA>` | `<evidence>` | `<evidence>` | `<operation id>` |
| delete | `acceptance/<run-id>` | `<S SHA>` | `<absent>` | `<evidence>` | `n/a` | `n/a` |

All transitions used `--force-with-lease` or equivalent expected-old-SHA
guards, and neither `main` nor a standing `scenario/*` ref changed: **yes**.

### Automated evidence on this candidate

- Unit: `<commands, result, timing, evidence location>`
- DinD: `<commands, result, timing, evidence location>`
- Real host: `<commands, result, timing, evidence location>`

### Stable checklist

- [ ] 1. Sign up at ployz.dev via GitHub OAuth, land in an org/project — `manual`
- [ ] 2. "Add machine" renders one copy-paste Cloud Bootstrap Token command — `manual`
- [ ] 3. amd64 machine joins — `real-host`
- [ ] 4. arm64 machine joins — `real-host`
- [ ] 5. Host firewall opens exactly the Ployz ports — `real-host`
- [ ] 6. Connect the private fixture repo; App install mints a token — `manual`
- [ ] 7. `git push` → webhook → build starts, no CLI touched — `manual`
- [ ] 8. Dockerfile service builds — `manual` + `unit` (adapter)
- [ ] 9. Railpack service builds — no Dockerfile, provider auto-detected — `manual` + `unit` (adapter)
- [ ] 10. Both build native amd64 **and** arm64 — `manual`
- [ ] 11. One platform-independent index digest — `unit`
- [ ] 12. All-or-nothing: one arch fails ⇒ no index published — `unit`
- [ ] 13. Replicas land on both machines — `real-host`
- [ ] 14. Hosted Dashboard assigns `https://<service>-<env4>.<lease>.up.ployz.app` and it returns 200 — `manual`
- [ ] 15. Custom domain → CNAME to the lease apex → certificate → 200 — `manual`
- [ ] 16. Route survives a control-daemon restart — `real-host`
- [ ] 17. Write a row → redeploy → the row survives — `manual`
- [ ] 18. The volume is a ZFS dataset on the machine that holds it — `manual`
- [ ] 19. The hosted Cloud under test is opted into ZFS — `manual`
- [ ] 20. F1: the real compile error is visible in the Dashboard build log — `manual`
- [ ] 21. F1: the live environment is untouched — `manual`
- [ ] 22. F1: fix + push → green — `manual`
- [ ] 23. F2: typed failure, failed container retained for inspection — `dind`
- [ ] 24. **F2: the previous version is still serving** — `dind`
- [ ] 25. F2: retry is a new operation, prior failure's evidence kept — `dind`
- [ ] 26. F3: core down ⇒ the request fails within its bound as typed `NoResponders` or `TimedOut`, with no operation accepted — `dind` (rejection) + `manual` (no accepted operation)
- [ ] 27. F3: gateway/DNS keep serving last-known-good — `dind`

### Required seam evidence

- Private-repository GitHub App-token authentication: `<evidence>`
- Dockerfile adapter, native amd64 and arm64: `<evidence>`
- Railpack adapter, native amd64 and arm64: `<evidence>`
- Managed HTTPS: `<evidence>`
- Custom HTTPS: `<evidence>`
- Actual ZFS dataset: `<evidence>`
- Hosted Cloud ZFS opt-in: `<evidence>`
- F1 failure and fresh push: `<evidence>`
- F2 failure, fresh operation id, and prior evidence preservation: `<evidence>`
- F3 bounded typed request rejection and last-known-good serving (`dind`), plus before/after no-accepted-operation evidence (`manual`): `<evidence>`

All 27 claims are checked without substitution: **yes**.
```
