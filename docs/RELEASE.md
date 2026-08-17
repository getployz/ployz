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

1. Set `[workspace.package] version` in `Cargo.toml` to the version you will tag (`0.2.0` or `0.2.0-beta.1`).
2. Merge that commit to `main`.
3. Tag and push:

```sh
git tag v0.2.0
git push origin v0.2.0
```

Beta: `v0.2.0-beta.1` with Cargo version `0.2.0-beta.1`. Nightly, `-rc`, and other suffixes are rejected.

4. Wait for the Release workflow. It builds the seven archives, then opens a **draft** GitHub release (`--prerelease` on beta tags).
5. Fill `## Notes`. Click **Publish**. That click is the review gate. Drafts are not public downloads.

## What Publish does

`scripts/promote-release.sh` runs on `release: published`.

- Writes a one-line file (`v0.2.0`) on the `channels` branch: `stable` or `beta`.
- Stable only: regenerates `Formula/ployz.rb` from `checksums.txt` and pushes `getployz/homebrew-ployz`.

Needs repo secret `HOMEBREW_TAP_TOKEN` (write access to the tap). Channel updates use `GITHUB_TOKEN`.

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

Point the zone:

| URL | Body |
| --- | --- |
| `https://ployz.sh` | `install.sh` from this repo |
| `https://ployz.sh/stable` | `channels` branch file `stable` |
| `https://ployz.sh/beta` | `channels` branch file `beta` |

Apex and channel URLs must serve these bodies. The installer does not detect or tolerate the old v1 script.

## Homebrew

Goreleaser does not upload the tap (`skip_upload: true`). Bottles 404 if the formula is pushed while the GitHub release is still a draft. The tap updates only after a **stable** Publish.

## Machine daemon

`scripts/install.sh` on Linux. Same version tokens: `latest` / `stable` / `beta` / pin. Set `PLOYZ_VERSION`.
