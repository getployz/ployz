#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

assert_eq() {
    if [ "$1" != "$2" ]; then
        echo "expected '$2', got '$1'" >&2
        exit 1
    fi
}

assert_contains() {
    if ! grep -Fq -- "$2" "$1"; then
        echo "$1 does not contain: $2" >&2
        exit 1
    fi
}

assert_contains "$ROOT/.goreleaser.yaml" "x86_64-unknown-linux-musl"
assert_contains "$ROOT/.goreleaser.yaml" "aarch64-unknown-linux-musl"
assert_contains "$ROOT/.goreleaser.yaml" "x86_64-apple-darwin"
assert_contains "$ROOT/.goreleaser.yaml" "aarch64-apple-darwin"
assert_contains "$ROOT/.goreleaser.yaml" 'name_template: >-'
assert_contains "$ROOT/.goreleaser.yaml" '{{- if eq .Os "darwin" }}macos'
assert_contains "$ROOT/.goreleaser.yaml" 'name_template: "ployzd_{{ .Os }}_{{ .Arch }}"'
assert_contains "$ROOT/.goreleaser.yaml" 'name_template: "ployz-relay_{{ .Os }}_{{ .Arch }}"'
assert_contains "$ROOT/.goreleaser.yaml" "--package=ployz-relay"
assert_contains "$ROOT/.goreleaser.yaml" "mode: 0755"
assert_contains "$ROOT/.goreleaser.yaml" "ids: [ployz]"
assert_contains "$ROOT/.goreleaser.yaml" "skip_upload: true"
assert_contains "$ROOT/.goreleaser.yaml" "clean break"
assert_contains "$ROOT/.github/workflows/release.yml" "check-release-tag.sh"
assert_contains "$ROOT/.github/workflows/release.yml" "release --clean --skip=publish"
assert_contains "$ROOT/.github/workflows/release.yml" "publish-github-release.sh"
assert_contains "$ROOT/.github/workflows/release.yml" "uses: ./.github/workflows/release-contracts.yml"
assert_contains "$ROOT/.github/workflows/release.yml" "needs: [tag, artifacts]"
assert_contains "$ROOT/.github/workflows/release-published.yml" "promote-release.sh"
assert_contains "$ROOT/.github/workflows/release-published.yml" "types: [published]"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "branches: [main]"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "workflow_dispatch"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "stage-ployz-sh-site.sh"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "ref: \${{ github.event.repository.default_branch }}"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "PLOYZ_SH_CHANNELS_DIR"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "pages deploy dist/ployz-sh-site --project-name=ployz-sh --branch=main"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "secrets.CLOUDFLARE_API_TOKEN"
assert_contains "$ROOT/.github/workflows/ployz-sh.yml" "secrets.CLOUDFLARE_ACCOUNT_ID"
assert_contains "$ROOT/.github/workflows/release-published.yml" "actions: write"
assert_contains "$ROOT/scripts/promote-release.sh" "gh workflow run ployz-sh.yml --ref main"
if grep -Fq 'scripts/ployz.sh' "$ROOT/.github/workflows/ployz-sh.yml"; then
    echo "ployz.sh workflow still stages the rust installer" >&2
    exit 1
fi
if grep -Eq 'branches:.*channels' "$ROOT/.github/workflows/ployz-sh.yml"; then
    echo "ployz.sh workflow still pretends a channels-branch push can deploy" >&2
    exit 1
fi
if grep -Fq 'channels/alpha.env' "$ROOT/.github/workflows/ployz-sh.yml" "$ROOT/scripts/stage-ployz-sh-site.sh" "$ROOT/site/_headers"; then
    echo "ployz.sh site still uses rust channel files" >&2
    exit 1
fi
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-release.sh macos"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-release.sh linux"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-relay-image.sh"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "publish-relay-image.sh"
assert_contains "$ROOT/.github/workflows/release.yml" "push-relay-image: true"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-release.sh artifacts"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "runs-on: ubuntu-latest"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "runs-on: macos-15"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "RELEASE_OS: linux"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "RELEASE_OS: darwin"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "taiki-e/install-action@v2"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "Swatinem/rust-cache@v2"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "scripts/pack-release.sh"
assert_contains "$ROOT/.goreleaser.yaml" 'RELEASE_OS'
if grep -q 'needs: gate' "$ROOT/.github/workflows/release.yml"; then
    echo "release still blocks artifacts on the CI gate" >&2
    exit 1
fi
if grep -q 'cargo install cargo-zigbuild' "$ROOT/.github/workflows/release-contracts.yml"; then
    echo "release still compiles cargo-zigbuild from source" >&2
    exit 1
fi

manifest=$(mktemp)
printf 'version = "1.2.3"\n' > "$manifest"
"$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest"
printf 'version = "1.2.3-beta.1"\n' > "$manifest"
"$ROOT/scripts/check-release-tag.sh" v1.2.3-beta.1 "$manifest"
if "$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest" >/dev/null 2>&1; then
    echo "stable tag was accepted against a beta workspace version" >&2
    exit 1
fi
printf 'version = "1.2.3"\n' > "$manifest"
if "$ROOT/scripts/check-release-tag.sh" v1.2.4 "$manifest" >/dev/null 2>&1; then
    echo "mismatched workspace version was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" nightly "$manifest" >/dev/null 2>&1; then
    echo "nightly tag was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" v1.2.3-rc.1 "$manifest" >/dev/null 2>&1; then
    echo "rc tag was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" v1.2.3-beta "$manifest" >/dev/null 2>&1; then
    echo "beta tag without N was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" v1.2 "$manifest" >/dev/null 2>&1; then
    echo "non-SemVer tag was accepted" >&2
    exit 1
fi
rm -f "$manifest"

PLOYZ_RELEASE_TEST_ONLY=true source "$ROOT/scripts/publish-github-release.sh"
release_dist=$(mktemp -d)
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz ployz-relay_linux_amd64.tar.gz ployz-relay_linux_arm64.tar.gz; do
    : > "$release_dist/$archive"
done
: > "$release_dist/checksums.txt"
: > "$release_dist/ployz.rb"
assert_eq "$(release_assets "$release_dist" | xargs -n1 basename | sort)" \
    "$(printf '%s\n' checksums.txt ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz ployz-relay_linux_amd64.tar.gz ployz-relay_linux_arm64.tar.gz | sort)"
assert_eq "$(release_create_flags v1.2.3 | tr '\n' ' ')" "--draft "
assert_eq "$(release_create_flags v1.2.3-beta.1 | tr '\n' ' ')" "--draft --prerelease "
if ! printf '%s\n' "$(release_notes v1.2.3)" | grep -Fq "clean break"; then
    echo "release notes omit the clean-break statement" >&2
    exit 1
fi
if ! printf '%s\n' "$(release_notes v1.2.3)" | grep -Fq "https://ployz.sh"; then
    echo "release notes omit ployz.sh install" >&2
    exit 1
fi
if printf '%s\n' "$(release_notes v1.2.3)" | grep -Fq "raw.githubusercontent.com"; then
    echo "release notes still point at raw.githubusercontent.com" >&2
    exit 1
fi
: > "$release_dist/ployz_windows_amd64.tar.gz"
if release_assets "$release_dist" >/dev/null 2>&1; then
    echo "extra archive was accepted as a release asset" >&2
    exit 1
fi
rm -f "$release_dist/ployz_windows_amd64.tar.gz" "$release_dist/checksums.txt"
if release_assets "$release_dist" >/dev/null 2>&1; then
    echo "release without checksums.txt was accepted" >&2
    exit 1
fi
rm -rf "$release_dist"

PLOYZ_HOMEBREW_TEST_ONLY=true source "$ROOT/scripts/repoint-homebrew-tap.sh"
tap=$(mktemp -d)
mkdir -p "$tap/Casks"
printf 'cask "ployz"\n' > "$tap/Casks/ployz.rb"
printf 'old tap\n' > "$tap/README.md"
formula=$(mktemp)
printf 'class Ployz < Formula\n  url "https://github.com/getployz/ployz2/releases/download/v1.2.3/ployz_linux_amd64.tar.gz"\n  sha256 "abc"\nend\n' > "$formula"
repoint_homebrew_tap "$tap" "$formula"
assert_eq "$(cat "$tap/Formula/ployz.rb")" "$(cat "$formula")"
if [ -e "$tap/Casks/ployz.rb" ]; then
    echo "legacy Homebrew cask was retained" >&2
    exit 1
fi
assert_contains "$tap/README.md" "clean break"
assert_contains "$tap/README.md" "getployz/ployz2"
rm -rf "$tap" "$formula"

PLOYZ_PROMOTE_TEST_ONLY=true source "$ROOT/scripts/promote-release.sh"
assert_eq "$(channel_name_for_tag v1.2.3)" stable
assert_eq "$(channel_name_for_tag v1.2.3-beta.2)" beta
if channel_name_for_tag v1.2.3-rc.1 >/dev/null 2>&1; then
    echo "rc tag was accepted as a channel" >&2
    exit 1
fi
channels=$(mktemp -d)
write_channel_file "$channels" v1.2.3
assert_eq "$(cat "$channels/stable")" v1.2.3
write_channel_file "$channels" v1.2.3-beta.1
assert_eq "$(cat "$channels/beta")" v1.2.3-beta.1
assert_eq "$(cat "$channels/stable")" v1.2.3
checksums=$(mktemp)
printf '%s  %s\n' aaa ployz_linux_amd64.tar.gz bbb ployz_linux_arm64.tar.gz ccc ployz_macos_amd64.tar.gz ddd ployz_macos_arm64.tar.gz > "$checksums"
tap=$(mktemp -d)
formula=$(mktemp)
write_homebrew_formula_from_checksums "$checksums" "$formula" 9.9.9 v9.9.9 getployz/ployz2
repoint_homebrew_tap "$tap" "$formula"
assert_contains "$tap/Formula/ployz.rb" "version \"9.9.9\""
assert_contains "$tap/Formula/ployz.rb" "releases/download/v9.9.9/ployz_linux_amd64.tar.gz"
assert_contains "$tap/Formula/ployz.rb" aaa
if stable_release_tag v9.9.9-beta.1; then
    echo "beta tag was classified stable" >&2
    exit 1
fi
rm -rf "$channels" "$tap"
rm -f "$checksums" "$formula"

PLOYZ_INSTALL_TEST_ONLY=true source "$ROOT/scripts/install.sh"
assert_eq "$(daemon_archive x86_64)" "ployzd_linux_amd64.tar.gz"
assert_eq "$(daemon_archive aarch64)" "ployzd_linux_arm64.tar.gz"
if daemon_archive riscv64 >/dev/null 2>&1; then
    echo "unsupported daemon architecture was accepted" >&2
    exit 1
fi
assert_eq "$(daemon_action 1.2.3 1.2.3 floating)" "keep"
assert_eq "$(daemon_action 1.2.2 1.2.3 floating)" "replace"
assert_eq "$(daemon_action 1.2.4 1.2.3 floating)" "keep"
assert_eq "$(daemon_action 1.2.3 1.2.2 pin)" "replace"
assert_eq "$(daemon_action 1.2.2 1.2.3 pin)" "replace"

PLOYZ_CLI_INSTALL_TEST_ONLY=true source "$ROOT/install.sh"
assert_eq "$(cli_archive Linux x86_64)" "ployz_linux_amd64.tar.gz"
assert_eq "$(cli_archive Linux aarch64)" "ployz_linux_arm64.tar.gz"
assert_eq "$(cli_archive Darwin x86_64)" "ployz_macos_amd64.tar.gz"
assert_eq "$(cli_archive Darwin arm64)" "ployz_macos_arm64.tar.gz"
if cli_archive FreeBSD x86_64 >/dev/null 2>&1; then
    echo "unsupported CLI platform was accepted" >&2
    exit 1
fi
channel_file=$(mktemp)
printf 'v1.2.3\n' > "$channel_file"
assert_eq "$(channel_version_from_file "$channel_file")" v1.2.3
printf '0.2.0-beta.1\n' > "$channel_file"
assert_eq "$(channel_version_from_file "$channel_file")" 0.2.0-beta.1
printf 'not-a-version\n' > "$channel_file"
if channel_version_from_file "$channel_file" >/dev/null 2>&1; then
    echo "non-version channel file was accepted" >&2
    exit 1
fi
rm -f "$channel_file"

PLOYZ_UNINSTALL_TEST_ONLY=true source "$ROOT/scripts/uninstall.sh"
assert_eq "$(uninstall_disposition docker)" "retain"
assert_eq "$(uninstall_disposition docker-images)" "retain"
assert_eq "$(uninstall_disposition docker-volumes)" "retain"
assert_eq "$(uninstall_disposition ployz.service)" "remove"
assert_eq "$(uninstall_disposition ployzd)" "remove"
assert_eq "$(uninstall_disposition ployz-state)" "remove"

PLOYZ_RELEASE_ARTIFACTS_TEST_ONLY=true source "$ROOT/scripts/release-artifacts-needed.sh"
assert_eq "$(release_artifacts_needed push)" true
assert_eq "$(release_artifacts_needed workflow_call)" true
assert_eq "$(release_artifacts_needed pull_request install.sh scripts/install.sh)" false
assert_eq "$(release_artifacts_needed pull_request .goreleaser.yaml)" true
assert_eq "$(release_artifacts_needed pull_request scripts/verify-release.sh)" true
assert_eq "$(release_artifacts_needed pull_request scripts/pack-release.sh)" true
assert_eq "$(release_artifacts_needed pull_request scripts/homebrew-formula.sh)" true
assert_eq "$(release_artifacts_needed pull_request scripts/release-tag.sh)" false
assert_eq "$(release_artifacts_needed pull_request .github/workflows/release-contracts.yml)" true
assert_eq "$(release_artifacts_needed pull_request ployz-relay/Dockerfile)" true
assert_eq "$(release_artifacts_needed pull_request scripts/build-relay-image.sh)" true
assert_eq "$(release_artifacts_needed pull_request scripts/verify-relay-image.sh)" true
assert_eq "$(release_artifacts_needed pull_request scripts/publish-relay-image.sh)" true

pack_dist=$(mktemp -d)
pack_bin=$(mktemp -d)
printf 'x' > "$pack_bin/ployz"
cp "$ROOT/scripts/uninstall.sh" "$pack_bin/ployz-uninstall"
printf 'x' > "$pack_bin/ployzd"
printf 'x' > "$pack_bin/ployz-relay"
chmod 0755 "$pack_bin/ployz" "$pack_bin/ployz-uninstall" "$pack_bin/ployzd" "$pack_bin/ployz-relay"
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
    tar -czf "$pack_dist/$archive" -C "$pack_bin" ployz
done
for archive in ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    tar -czf "$pack_dist/$archive" -C "$pack_bin" ployzd ployz-uninstall
done
for archive in ployz-relay_linux_amd64.tar.gz ployz-relay_linux_arm64.tar.gz; do
    tar -czf "$pack_dist/$archive" -C "$pack_bin" ployz-relay
done
bash "$ROOT/scripts/pack-release.sh" "$pack_dist"
DIST="$pack_dist" bash "$ROOT/scripts/verify-release.sh" artifacts
if DIST="$pack_dist" bash "$ROOT/scripts/verify-release.sh" linux >/dev/null 2>&1; then
    echo "linux verification accepted macos archives" >&2
    exit 1
fi
rm -rf "$pack_dist" "$pack_bin"

site=$(mktemp -d)
channels=$(mktemp -d)
printf 'v1.2.3\n' > "$channels/stable"
printf 'v1.2.3-beta.1\n' > "$channels/beta"
printf 'PLOYZ_RELEASE_TAG=v0.0.1\n' > "$channels/alpha.env"
PLOYZ_SH_SITE_DIR="$site" PLOYZ_SH_CHANNELS_DIR="$channels" bash "$ROOT/scripts/stage-ployz-sh-site.sh"
assert_eq "$(cat "$site/index.html")" "$(cat "$ROOT/install.sh")"
assert_eq "$(cat "$site/install.sh")" "$(cat "$ROOT/install.sh")"
assert_eq "$(cat "$site/stable")" v1.2.3
assert_eq "$(cat "$site/beta")" v1.2.3-beta.1
if [ -e "$site/alpha.env" ] || [ -e "$site/channels/alpha.env" ]; then
    echo "staged site included rust channel files" >&2
    exit 1
fi
assert_contains "$site/_headers" "text/x-shellscript"
assert_contains "$site/_headers" "/stable"
assert_contains "$site/_headers" "/beta"
assert_contains "$site/_headers" "text/plain"
rm -rf "$site" "$channels"

site=$(mktemp -d)
PLOYZ_SH_SITE_DIR="$site" bash "$ROOT/scripts/stage-ployz-sh-site.sh"
assert_eq "$(cat "$site/index.html")" "$(cat "$ROOT/install.sh")"
if [ -e "$site/stable" ] || [ -e "$site/beta" ]; then
    echo "staged site invented channel files" >&2
    exit 1
fi
rm -rf "$site"

assert_contains "$ROOT/ployz-relay/Dockerfile" "FROM scratch"
assert_contains "$ROOT/ployz-relay/Dockerfile" "COPY ployz-relay /ployz-relay"
assert_contains "$ROOT/ployz-relay/Dockerfile" 'ENTRYPOINT ["/ployz-relay"]'
if grep -Eq '^RUN |^ADD ' "$ROOT/ployz-relay/Dockerfile"; then
    echo "relay Dockerfile is not scratch-only" >&2
    exit 1
fi

echo "release contracts passed"
