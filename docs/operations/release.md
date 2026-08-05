# Release Operations

Ployz uses two release surfaces:

- GitHub Releases hold immutable, versioned binaries and platform manifests
  under exact `v*` tags.
- `https://ployz.sh` serves the installer and mutable channel files that point
  at one exact tag.
- npm publishes `@ployz/sdk` from published GitHub Releases.

The mutable channel is only Bootstrap Delivery convenience. `ployz.sh` resolves
it before downloading the Host Runner artifact. Host Runner update, substrate update, and
Release Source resolution continue to require exact versions.

## Publish An Exact Release

Use an exact `v*` tag, for example `v0.0.2-alpha.5`.

1. Start from a pushed commit on `main`. Keep unrelated local changes
   unstaged.
2. Run the tests that cover the changed release surface. At minimum, run
   formatter checks plus focused crate tests for the changed code. For example:

```sh
cargo fmt --check
cargo test -p ployz-host-runner --test bootstrap_first_machine --test local
cargo test -p ployzd --lib dataplane_runtime::tests::default_command_plans_ensure_wireguard_interface_and_key
```

3. Tag and push the exact release commit, then wait for that `main` push's
   `warm-linux-cache` jobs to finish:

```sh
tag=v0.0.2-alpha.5
git tag "${tag}"
git push origin main
git push origin "${tag}"
sha="$(git rev-list -n 1 "${tag}")"
warm_run="$(gh run list --repo getployz/ployz --workflow release.yml \
  --event push --commit "${sha}" --json databaseId --jq '.[0].databaseId')"
gh run watch "${warm_run}" --repo getployz/ployz --exit-status
```

4. Dispatch packaging from `main`, passing the exact tag, then watch that run:

```sh
gh workflow run release.yml --repo getployz/ployz --ref main -f tag="${tag}"
gh run list --repo getployz/ployz --workflow release.yml --limit 5
gh run watch <run-id> --repo getployz/ployz --exit-status
```

The workflow rejects a dispatch from any commit other than the tag's exact
commit. Running it on `main` lets packaging reuse the caches warmed for that
commit while checkout and release artifacts remain pinned to the tag.

The `release` workflow packages:

   - a prechecked `@ployz/sdk` npm tarball for the release tag version.
   - `ployz-release-linux-amd64.env`
   - `ployz-release-linux-arm64.env`
   - `ployz-release-darwin-amd64.env`
   - `ployz-release-darwin-arm64.env`
   - the binaries referenced by those manifests.

5. Review the draft GitHub Release and publish it as a prerelease only after
   all assets are attached. Publishing the GitHub Release also publishes
   `@ployz/sdk` to npm with the same version as the tag:

```sh
gh release view "${tag}" --repo getployz/ployz \
  --json isDraft,isPrerelease,tagName,url,assets
gh release edit "${tag}" --repo getployz/ployz \
  --draft=false --prerelease=true
```

The npm package must have trusted publishing configured for
`getployz/ployz`, workflow filename `release.yml`, and action `npm publish`.
Prereleases publish under the npm `alpha` dist-tag.
The dispatched workflow checks that the SDK version is not already published and
that `npm publish --dry-run` succeeds before building release assets.

6. Verify the release assets before promotion:

```sh
scripts/verify-release-assets.sh "${tag}" --print-channel
```

The verifier downloads assets with `gh release download` unless `--assets-dir`
points at a local staged asset directory. It checks every platform manifest,
every referenced asset, and every SHA-256 value.

The verifier uses the GitHub API, so also check that unauthenticated installer
URLs work before promoting a channel:

```sh
curl -fsSL \
  "https://github.com/getployz/ployz/releases/download/${tag}/ployz-release-linux-amd64.env" \
  | sed -n '1,12p'
curl -fsSL \
  "https://github.com/getployz/ployz/releases/download/${tag}/ployz-release-darwin-arm64.env" \
  | sed -n '1,12p'
```

## Promote The Alpha Channel

After the exact release is published and verified, update
`site/channels/alpha.env` with the verifier:

```sh
scripts/verify-release-assets.sh "${tag}" \
  --write-channel site/channels/alpha.env
git diff -- site/channels/alpha.env
git add site/channels/alpha.env
git commit -m "chore(release): promote alpha to ${tag}"
git push origin main
```

The `ployz.sh` workflow stages `scripts/ployz.sh` as both `index.html` and
`install.sh`, copies `site/channels/` and `site/_headers`, and deploys the
static site to Cloudflare Pages with Wrangler Direct Upload.

Watch the deploy and verify the live channel:

```sh
gh run list --repo getployz/ployz --workflow ployz-sh.yml --limit 5
gh run watch <run-id> --repo getployz/ployz --exit-status
curl -fsSL https://ployz.sh/channels/alpha.env
```

Do not upload channel pointers as GitHub Release assets, and do not use
`gh release upload --clobber` for channel promotion.

## Smoke A Promoted Release

Verify the public installer resolves the promoted channel and installs Host Runner:

```sh
curl -fsSL https://ployz.sh | PLOYZ_CHANNEL=alpha sh
ployz --help
```

For a server smoke, use a controlled test hostname that already points at the
gateway machine:

```sh
ployz deploy --image nginx:alpine --route asdf.ployz.dev:80
ployz ops watch <operation-id> --json
curl -i --max-time 10 http://asdf.ployz.dev/
```

Success means the deploy reaches `deploy_completed` and public HTTP returns
`200 OK`.

## Roll Back A Channel Promotion

Revert the `site/channels/alpha.env` change and let the Cloudflare Pages
workflow deploy the previous pointer. Versioned release assets are not modified
during rollback.

## Recover A Broken Release Object

If `gh release download "${tag}"` works but unauthenticated
`https://github.com/getployz/ployz/releases/download/${tag}/...` URLs return
`404`, the GitHub Release object is not serving public assets correctly. This
can happen if the draft was created against an untagged release object.

First confirm the release state and public failure:

```sh
gh release view "${tag}" --repo getployz/ployz \
  --json isDraft,isPrerelease,tagName,url,assets
curl -I -L \
  "https://github.com/getployz/ployz/releases/download/${tag}/ployz-release-darwin-arm64.env"
```

Then recreate the release object without deleting the git tag. Only run this
after the asset download into `tmpdir` succeeds:

```sh
tmpdir="$(mktemp -d)"
gh release download "${tag}" --repo getployz/ployz \
  --dir "${tmpdir}" --pattern '*' --clobber
gh release delete "${tag}" --repo getployz/ployz --yes
gh release create "${tag}" "${tmpdir}"/* --repo getployz/ployz \
  --verify-tag --prerelease --title "${tag}" --notes "Ployz ${tag}"
```

Re-run the public `curl` checks before channel promotion.

## Exact Installs

Default channel install:

```sh
curl -fsSL https://ployz.sh | sh
ployz --help
```

Explicit channel install:

```sh
curl -fsSL https://ployz.sh | sh -s -- --channel alpha
ployz --help
```

Exact release install:

```sh
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.2-alpha.1
ployz --help
```

Update existing machine substrate to an exact release after Host Runner is installed:

```sh
curl -fsSL https://ployz.sh | sh -s -- --version v0.0.2-alpha.1
sudo ployz host substrate-update --version v0.0.2-alpha.1
```

Cloud Bootstrap Delivery installs Host Runner first, then runs the same
`ployz init` primitive used by the local and SSH paths. Noninteractive tokens are
passed to Host Runner with `--cloud-token`; they are not passed to `ployz.sh`.
Host Runner and substrate update
commands reject channels, version ranges, and `latest`. The public bootstrap
installer targets Linux machines.

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
