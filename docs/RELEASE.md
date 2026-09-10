# Release

Two human steps. Everything else is automation.

```text
1. git tag && git push     → CI creates a GitHub draft
2. Edit Notes, click Publish
     stable  → ployz.sh/stable + Homebrew
     beta    → ployz.sh/beta
```

No `next` branch. Features and fixes both land on `main`. Beta is a tag.

## Cut a release

1. Set `[workspace.package] version` in `Cargo.toml` and `crates/ployz-sdk/package.json` to the version you will tag (`0.2.0` or `0.2.0-beta.1`). Update `cloud/package.json` SDK and native-binding pins to the same version and refresh `cloud/pnpm-lock.yaml` against the published packages; Fast CI checks their equality. `check-release-tag.sh` rejects a tag if either is missing or differs.
2. Merge that commit to `main`.
3. Tag and push:

```sh
git tag v0.2.0
git push origin v0.2.0
```

Beta: `v0.2.0-beta.1` with Cargo version `0.2.0-beta.1`. Nightly, `-rc`, and other suffixes are rejected.

4. Wait for the Release workflow. The tag run validates the tag and commit, builds the six CLI and daemon archives using the shared kache/R2 cache, and opens a **draft** GitHub release (`--prerelease` on beta tags).
5. Fill `## Notes`. Click **Publish**. That click is the review gate. Drafts are not public downloads.

Automatic releases run the workflow version stored in the tagged commit. For recovery using the current workflow, dispatch `release.yml` from `main` with `tag` and its expected commit `sha`; dispatch `publish-sdk.yml` from `main` with `tag` to retry SDK publication.

Protected `main` pushes populate the shared R2 compiler cache through the checks selected for that change. Release archive checks run for packaging inputs and conservative full checks; SDK npm builds run only when publishing a release. Cloud reuses a native SDK and browser WASM artifact only when their source-input hash matches exactly, and builds from source on a miss. These checks do not publish releases or npm packages. Only protected `main` pushes receive R2 write credentials. PR, tag, release, scheduled, and manual runs use separate R2 read-only credentials. GitHub Actions variables `KACHE_S3_BUCKET`, `KACHE_S3_ENDPOINT`, and `KACHE_S3_REGION` select the bucket; secrets `KACHE_S3_ACCESS_KEY_ID` and `KACHE_S3_SECRET_ACCESS_KEY` provide write access. Create a separate R2 token with **Object Read only** permission scoped to the cache bucket, and store its credentials as `KACHE_S3_READ_ACCESS_KEY_ID` and `KACHE_S3_READ_SECRET_ACCESS_KEY`. Same-repository PRs reuse the shared cache with these read-only keys. Both credential pairs must be configured. GitHub does not expose these secrets to fork PRs.

## Confidence before Publish

`ci.yml` selects checks for PR and `main` changes and reports one `CI ready` result. Require that check in branch protection after the workflow lands. Rust lint and tests run independently; Cloud static checks, SDK preparation, frontend builds, and tests have separate results. The ignored cluster suite runs through `cluster.yml` nightly or by manual dispatch, with per-suite logs, five-minute suite limits, and no whole-batch retries. It is informing: a red nightly does not block a tag or Publish.

Before Publish, run `scripts/qualify-release.sh` against real Linux Machines using two draft musl archive sets: `PLOYZ_ARTIFACT_DIR` is the source release and `PLOYZ_UPGRADE_ARTIFACT_DIR` is the target release. The versions must differ. The script does not pick a cloud vendor. You pass SSH targets. Those hosts must be uninitialized Machines unless you set `PLOYZ_QUALIFY_RESET=1`, which accepts a reset and destroys managed containers. Pass a qualification key with `PLOYZ_QUALIFY_SSH_KEY`; the normal `ployz machine init`/`add` path installs the verified source, then the qualifier proves a target upgrade through client disconnection, persistent ZFS-backed traffic, corrupt preflight rejection, failed activation evidence, and explicit previous-binary repair.

When the informing cluster suite and that run disagree, the real Machines are the authority. Testkit bugs do not block a release.

## What Publish does

`scripts/promote-release.sh` runs on `release: published`.

- Writes a one-line file (`v0.2.0`) on the `channels` branch: `stable` or `beta`.
- Stable only: regenerates `Formula/ployz.rb` from `checksums.txt` and pushes `getployz/homebrew-ployz`.

Needs repo secret `HOMEBREW_TAP_TOKEN` (write access to the tap). Channel updates use `GITHUB_TOKEN`. Publish then dispatches `ployz.sh`, which deploys `install.sh` plus the `channels` branch files to Cloudflare Pages.

The same `release: published` event runs `publish-sdk.yml`, which builds the tagged SDK directly using the shared kache/R2 cache and publishes `@ployz/sdk` to npm (`beta` dist-tag on beta tags, `latest` on stable). `@ployz/sdk` itself is JavaScript only; each native binding ships as its own package (`@ployz/sdk-linux-x64`, `@ployz/sdk-linux-arm64`, `@ployz/sdk-darwin-arm64`, `@ployz/sdk-darwin-x64`, built by the workflow's matrix; the linux bindings are cross-linked by `cargo-zigbuild` against a glibc 2.28 floor, so they are not musl builds) and is listed as an optional dependency, so npm installs just the one matching the host. The `optionalDependencies` are generated by `scripts/pack-sdk-package.sh` from the bindings it is handed, so the matrix is the only place the platform list lives, and the bindings publish before `@ployz/sdk`. npm trusted publishing is configured for `getployz/ployz2` workflow `publish-sdk.yml` (no `NPM_TOKEN`); each binding package needs the same trusted publisher entry. To publish a tag whose GitHub Release already exists, dispatch `publish-sdk.yml` from `main` with that tag (the job checks out the tag).

## Install

```sh
curl -fsSL https://ployz.sh | sh              # stable
curl -fsSL https://ployz.sh | sh -s beta      # latest published beta
curl -fsSL https://ployz.sh | sh -s 0.2.0     # pin
brew install getployz/ployz/ployz             # stable
```

`latest` and `stable` mean the same thing. `nightly` is rejected.

The installer reads `https://ployz.sh/stable` or `/beta` (one line, `vX.Y.Z` or `vX.Y.Z-beta.N`). Missing or invalid channel files fail the install. Pins skip the channel fetch.

Artifacts stay on GitHub Releases. `ployz.sh` is the pointer plus CLI installer.

## Cloudflare

`.github/workflows/ployz-sh.yml` Direct-Uploads the staged site to the existing `ployz-sh` Pages project. Installer changes on `main` deploy immediately. Pointer updates deploy because Publish dispatches this workflow (`channels` has no workflow file, so a push there cannot). Production always uses `--branch=main`.

| URL | Body |
| --- | --- |
| `https://ployz.sh` | `install.sh` from this repo |
| `https://ployz.sh/stable` | `channels` branch file `stable` |
| `https://ployz.sh/beta` | `channels` branch file `beta` |

Needs repo secrets `CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` (same Pages project as before). Disable the rust repo's `ployz-sh` workflow so it cannot overwrite this deploy.

Apex and channel URLs must serve these bodies. The installer does not detect or tolerate the old v1 script.

## Homebrew

Goreleaser does not touch the tap (`--skip=homebrew`); `scripts/promote-release.sh` writes the formula. Bottles 404 if the formula is pushed while the GitHub release is still a draft. The tap updates only after a **stable** Publish.

## Machine daemon

`ployzd install` on Linux installs or replaces a Machine daemon. It accepts `--version stable`, `--version beta`, or an exact version; use `--software-only` for ordinary replacement after the Machine has already been prepared. Setup downloads and verifies the CLI release's daemon as a temporary bootstrap, then that daemon installs the selected Machine release through this interface.

## Tailcat helper

Every CLI and daemon archive includes the matching native `ployz-tailcat` helper;
all six archives are covered by `checksums.txt`. Homebrew and the CLI installer
install the helper beside the CLI. Machine installation manages the helper as
`ployzd-tailcat` through the existing daemon lifecycle. Each native SDK package
also bundles its platform helper.

Tailcat uses public DERP infrastructure; Ployz builds and deploys no hosted relay
process or image.
