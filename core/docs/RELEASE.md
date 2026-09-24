# Release

Two human steps. Everything else is automation.

```text
1. git tag && git push     → CI creates a GitHub draft
2. Edit Notes, click Publish
     stable  → ployz.sh/stable, /v<major>/stable, both betas + Homebrew
     beta    → ployz.sh/beta, /v<major>/beta
```

No `next` branch. Features and fixes both land on `main`. Beta is a tag.

## Cut a release

1. From `core/`, set `[workspace.package] version` in `Cargo.toml` to the version you will tag (`0.2.0` or `0.2.0-beta.1`). `check-release-tag.sh` rejects a tag if the Cargo version is missing or differs.
2. Merge that commit to `main`.
3. Tag and push:

```sh
git tag v0.2.0
git push origin v0.2.0
```

Beta: `v0.2.0-beta.1` with Cargo version `0.2.0-beta.1`. `-rc` and every other suffix are rejected.

4. Wait for the Release workflow. The tag run validates the tag and commit, builds the six CLI and daemon archives using the shared kache/R2 cache, and opens a **draft** GitHub release (`--prerelease` on beta tags).
5. Fill `## Notes`. Click **Publish**. That click is the review gate. Drafts are not public downloads.

Automatic releases run the workflow version stored in the tagged commit. For recovery using the current workflow, dispatch `release.yml` from `main` with `tag` and its expected commit `sha`.

Protected `main` pushes populate the shared R2 compiler cache through the checks selected for that change. Release archive checks run for packaging inputs and conservative full checks. Cloud reuses a native SDK and config WASM artifact only when their source-input hash matches exactly, and builds from source on a miss. These checks do not publish releases. Only protected `main` pushes receive R2 write credentials. PR, tag, release, scheduled, and manual runs use separate R2 read-only credentials. GitHub Actions variables `KACHE_S3_BUCKET`, `KACHE_S3_ENDPOINT`, and `KACHE_S3_REGION` select the bucket; secrets `KACHE_S3_ACCESS_KEY_ID` and `KACHE_S3_SECRET_ACCESS_KEY` provide write access. Create a separate R2 token with **Object Read only** permission scoped to the cache bucket, and store its credentials as `KACHE_S3_READ_ACCESS_KEY_ID` and `KACHE_S3_READ_SECRET_ACCESS_KEY`. Same-repository PRs reuse the shared cache with these read-only keys. Both credential pairs must be configured. GitHub does not expose these secrets to fork PRs.

## Confidence before Publish

`ci.yml` selects checks for PR and `main` changes and reports one `CI ready` result. Require that check in branch protection after the workflow lands. Rust lint and tests run independently; Cloud static checks, SDK preparation, frontend builds, and tests have separate results. The ignored cluster suite runs through `cluster.yml` nightly or by manual dispatch, with per-suite logs, five-minute suite limits, and no whole-batch retries. It is informing: a red nightly does not block a tag or Publish.

Before Publish, use the beta on real Machines through Cloud: enrol, deploy, attach a domain and certificate, and upgrade. There is no scripted qualification; when the informing cluster suite and those Machines disagree, the real Machines are the authority. Testkit bugs do not block a release.

## What Publish does

`scripts/promote-release.sh` runs on `release: published`.

- Writes one-line pointer files (`v0.2.0`) on the `channels` branch: the tag's line pointer (`v0/stable` or `v0/beta`) and the unscoped one (`stable` or `beta`). A stable tag also writes both `beta` pointers.
- A pointer only moves to a higher semver tag. Publishing an older-line fix moves that line's pointers only; an older tag moves nothing.
- Only when the unscoped `stable` pointer names the published tag: regenerates `Formula/ployz.rb` from `checksums.txt` and pushes `getployz/homebrew-ployz`.

Promotion runs one at a time. GitHub keeps only one waiting run, so publishing three releases in quick succession cancels the middle one's promotion: re-run any cancelled **Promote published release** run. Re-running is safe; pointers only move forward.

Needs repo secret `HOMEBREW_TAP_TOKEN` (write access to the tap). Channel updates use `GITHUB_TOKEN`. Publish then dispatches `ployz.sh`, which deploys `install.sh` plus the `channels` branch files to Cloudflare Pages.

## Install

```sh
curl -fsSL https://ployz.sh | sh              # stable
curl -fsSL https://ployz.sh | sh -s beta      # highest published release, beta or stable
curl -fsSL https://ployz.sh | sh -s 0.2.0     # pin
brew install getployz/ployz/ployz             # stable
```

`stable` and `beta` are the only channels.

The installer reads the unscoped `https://ployz.sh/stable` (one line, `vX.Y.Z`) or `/beta` (`vX.Y.Z` or `vX.Y.Z-beta.N`). Missing or invalid channel files fail the install, including a prerelease on `stable`. Pins skip the channel fetch.

Artifacts stay on GitHub Releases. `ployz.sh` is the pointer plus CLI installer.

## Cloudflare

The repository's `.github/workflows/ployz-sh.yml` Direct-Uploads the staged site to the existing `ployz-sh` Pages project. Installer changes on `main` deploy immediately. Pointer updates deploy because Publish dispatches this workflow (`channels` has no workflow file, so a push there cannot). Production always uses `--branch=main`.

| URL | Body |
| --- | --- |
| `https://ployz.sh` | `install.sh` from this repo |
| `https://ployz.sh/stable` | `channels` branch file `stable` |
| `https://ployz.sh/beta` | `channels` branch file `beta` |
| `https://ployz.sh/v<major>/stable` | `channels` branch file `v<major>/stable` |
| `https://ployz.sh/v<major>/beta` | `channels` branch file `v<major>/beta` |

Needs repo secrets `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN`.

Apex and channel URLs must serve these bodies.

## Homebrew

Goreleaser does not touch the tap (`--skip=homebrew`); `scripts/promote-release.sh` writes the formula. Bottles 404 if the formula is pushed while the GitHub release is still a draft. The tap updates only when a **stable** Publish leaves the unscoped `stable` pointer on that tag.

## Machine daemon

`ployzd install` on Linux installs or replaces a Machine daemon. It accepts `--version stable`, `--version beta`, or an exact version. A channel reads the daemon's own line pointer, `ployz.sh/v<major>/<channel>`, and keeps an installed daemon that is newer than the pointer; only an exact version moves a Machine backwards or across lines. Use `--software-only` for ordinary replacement after the Machine has already been prepared. Setup downloads and verifies the CLI release's daemon as a temporary bootstrap, then that daemon installs the selected Machine release through this interface.

## Management transport

Each of the six archives contains a single binary, covered by `checksums.txt`;
Homebrew, the CLI installer, and Machine installation copy one file. The iroh
management transport is in-process in the CLI, the daemon, and every native SDK
binding, so no helper is packaged and no extra systemd unit is installed.

Clients reach Machines through the self-hosted Ployz Relay at `relay.ployz.dev`,
an Uncloud service on the Hetzner host running the iroh relay release binary. It
is deployed separately from these releases; the hostname is a compiled constant
in `ployz-core`.
