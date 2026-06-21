# Release Operations

Ployz uses two release surfaces:

- GitHub Releases hold immutable, versioned binaries and platform manifests
  under exact `v*` tags.
- `https://ployz.sh` serves the installer and mutable channel files that point
  at one exact tag.

The mutable channel is only Bootstrap Delivery convenience. Machine bootstrap
resolves it before downloading artifacts. Keeper update, substrate update, and
Release Source resolution continue to require exact versions.

## Publish An Exact Release

1. Tag the release with an exact `v*` tag, for example `v0.0.2-alpha.1`.
2. Push the tag. The `release` workflow packages:
   - `ployz-release-linux-amd64.env`
   - `ployz-release-linux-arm64.env`
   - `ployz-release-darwin-amd64.env`
   - `ployz-release-darwin-arm64.env`
   - the binaries referenced by those manifests.
3. Review the draft GitHub Release and publish it only after all assets are
   attached.
4. Verify the release assets before promotion:

```sh
scripts/verify-release-assets.sh v0.0.2-alpha.1 --print-channel
```

The verifier downloads assets with `gh release download` unless `--assets-dir`
points at a local staged asset directory. It checks every platform manifest,
every referenced asset, and every SHA-256 value.

## Promote The Alpha Channel

After the exact release is published and verified, update
`site/channels/alpha.env`:

```text
PLOYZ_CHANNEL=alpha
PLOYZ_RELEASE_TAG=v0.0.2-alpha.1
PLOYZ_VERSION=0.0.2-alpha.1
PLOYZ_RELEASE_BASE_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.1
```

Then commit the channel-file change through the normal default-branch path.
The `ployz.sh` workflow stages `scripts/ployz.sh` as both `index.html` and
`install.sh`, copies `site/channels/` and `site/_headers`, and deploys the
static site to Cloudflare Pages with Wrangler Direct Upload.

Do not upload channel pointers as GitHub Release assets, and do not use
`gh release upload --clobber` for channel promotion.

## Roll Back A Channel Promotion

Revert the `site/channels/alpha.env` change and let the Cloudflare Pages
workflow deploy the previous pointer. Versioned release assets are not modified
during rollback.

## Exact Installs

Default channel install:

```sh
curl -fsSL https://ployz.sh | sh
```

Explicit channel install:

```sh
curl -fsSL https://ployz.sh | sh -s -- --channel alpha
```

Exact release install:

```sh
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.2-alpha.1
```

Machine bootstrap may receive a channel at delivery time, but the installer
resolves it to exact artifact metadata before keeper runs. Keeper and
substrate update commands reject channels, version ranges, and `latest`.

## Repository Settings

- Enable immutable releases for versioned `v*` releases.
- Protect release tags with a ruleset that limits creation, update, and
  deletion.
- Create a Cloudflare Pages Direct Upload project named `ployz-sh`; do not use
  Cloudflare Git integration for this site.
- Configure the `ployz.sh` custom domain on that Cloudflare Pages project and
  require HTTPS.
- Add GitHub Actions secrets:
  - `CLOUDFLARE_ACCOUNT_ID`
  - `CLOUDFLARE_API_TOKEN`
- Give the Cloudflare API token permission to deploy the `ployz-sh` Pages
  project.
- Keep GitHub Releases as the only host for versioned binaries and manifests.

After deployment, check:

```sh
curl -fsSL https://ployz.sh/channels/alpha.env
```

The response should be the tracked `site/channels/alpha.env` file over HTTPS.

## Attestations

The release workflow generates GitHub artifact attestations in each platform
packaging job when GitHub supports attestations for the repository. Installer
enforcement remains SHA-256 based; attestation verification is follow-up work.
