# Thread C decisions

Decisions thread C owns for the 0.4 security work. [decisions.md](decisions.md) holds the program-wide record. This file holds C's. Each entry gives the decision, why, and what it rules out.

## Cache trust (checkpoint C2)

**Decision.** A Machine treats no cache as authority. It may use cached content only when it checks that content, at the moment of use, against a digest a trusted key approved. A cache whose content Cloud can choose, and that the Machine cannot check that way, is disabled for approved work. There is no new trust service.

**Why.** The bar says a compromised Cloud must not run code of its choosing on a Server or read its secrets. A cache is a second way to deliver code, so the approval must reach through it. Content addressing already does that for images and Config files. A cache that cannot be addressed by content does not get trusted.

"Cloud-writable" means Cloud can put content there without a person's approval. Under enforcement, Cloud does that through Operator routes the enforcement switch does not gate (see `c-inventory.md`, "Other mutating routes"), or through credentials Cloud holds.

| Cache | Who can write it under enforcement | Decision | Owner |
| --- | --- | --- | --- |
| The Docker image store on a Machine | Cloud, through `PullImage`, `PullImageFromMachine`, `EnsureImageIngest`, a Build Grant push and `Build`, under any name and tag | Trusted only by content address. An approved spec names its image as `name@sha256:<manifest digest>` or `sha256:<image id>`. The Machine refuses a tag. Docker resolves a digest reference by content, so a planted tag changes nothing. | C, slice C1 |
| Named Docker Volumes made before a Container mounts them | Cloud, through `PrepareVolumes` | Checked at use. The existing Volume must match the approved declaration through `VolumeSource::matches_managed_volume`. An External mount refuses a Volume carrying Ployz's managed label. | C, slice C1 |
| Config files on disk | The daemon only, from approved bytes, named by their SHA-256 (`ployzd/src/docker/spec_store.rs:624`) | Trusted. Machine-local and content-addressed. | None |
| The spec store and the consumption log | The daemon only | Trusted. Machine-local. | None |
| The BuildKit cache on a Machine | Cloud, through the `Build` route, which enforcement does not gate. Secret mounts and network fetches are not in BuildKit's cache keys, so a Cloud-run build can store a layer under the key an approved build later hits. | Trusted only once every build on that Machine runs under an approval. Thread D gates `Build` under enforcement. Until then, built-image deploys stay refused under enforcement, as they are today. | D |
| Registry build caches (`cache_from` and `cache_to` of type `registry` or `inline`, allowed at `ployz-build/src/received_recipe.rs:220`) | Anyone holding registry write, which can include Cloud through Registry Credentials | Disabled for any build whose output an approval or an automatic deploy rule accepts, unless thread D's proof binds the output digest without trusting cache content. | D |
| The GitHub Actions cache (`type=gha`, `ployz-build/src/lib.rs:711`) | Workflow runs in the repository | Kept only if thread D shows that the cache scope an approved build reads is writable only by runs D's rule would accept anyway, for example runs on the approved branch. Otherwise disabled. | D, with Y's evidence |
| Cloud-side records: Build Receipts, Uploaded Source, the "reuse a usable image" choice, the Builder with a warm cache | Cloud | Never authority. They may pick which approved digest to fetch, or where to build. They cannot pick the digest. | C and D |

**Ruled out.**

- A cache trust service or signed cache manifests. Content addressing already gives the check for the caches that matter. A service would be one more thing for Cloud to drive and one more key to protect.
- Turning off the BuildKit cache on Machines. Once thread D gates `Build`, the cache is written only by approved builds, so it costs build time and buys nothing.
- Pinning tags at Save or Publish in the Config Store. Cloud runs the Config Store, so Cloud would choose the pin.

**What it leaves.** Approved code that is itself malicious can still poison what it writes. That is the residual "approved application code" already carries. A private image named by a tag that Cloud can push to, through a Registry Credential Cloud holds, can only be pinned by a digest the person checks. Slice C2 in `c-plan.md` pins public tags from the CLI and asks for a written digest otherwise.

## Settings changes need no statement of their own

**Decision.** A settings change reaches a Machine only through a Deploy. The deploy approval covers it. Machines check no separate settings statement, and the `settings` action family on a session grant has nothing to approve on a Machine.

**Why.** Saved State lives in the Config Store, which Cloud runs. A Machine never reads it. What a Machine runs comes from a Deploy's frozen Attempt Target, and the deploy approval binds that. A second statement would bind the same bytes twice.

**Ruled out.** A Machine-checked "settings revision" statement. Thread D still binds an approved settings revision inside its automatic deploy rule. That is a rule field D's verifier reads, not a statement a Machine checks on every Deploy.

## The approval binds the requested spec, not the resolved one

**Decision.** The digest is taken over each Service's `RequestedServiceSpec` from the frozen Attempt Target, minus `mode` and `update`. The Machine recomputes it from the `ResolvedServiceSpec` it is asked to create, through `ResolvedServiceSpec::to_requested`.

**Why.** The approver's client sees the Attempt Target before placement. Placement adds a Service ID, Namespace-scoped Volume names and a resolved rollout order. None of these can cross the bar, and the Machine can derive the scoped names itself. A digest over the resolved form would force the approver to plan the Deploy, which the runner does later from its own snapshot.

**Ruled out.** Digesting the whole `NewContainer`. Its slot key, generation, deployment ID and pull credential change on every retry, so a retry would need a new approval.

## The approver signs what its own client shows

**Decision.** The CLI and the browser build the approval body from the exact bytes they render. The CLI prints the change it approves, and that change is computed from the same body it signs. Neither one signs a digest Cloud computed.

**Why.** Cloud can propose a payload. Only a key holder can approve one. If Cloud lies about Saved State, the approver sees the lie in the change it approves. Showing one thing and signing another is the mislabelling case the bar sets aside, and in the browser it is B's accepted malicious-dashboard residual.

**What it leaves.** An unattended `ployz deploy` with a Device key, such as one in CI, approves whatever Cloud's Saved State says. That automation trusts Cloud by construction. The safe path for unattended deploys is thread D's automatic deploy rule, which binds the source repository and a settings revision once. The CLI docs say so in slice C1.
