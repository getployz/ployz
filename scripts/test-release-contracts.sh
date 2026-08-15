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
assert_contains "$ROOT/.goreleaser.yaml" "mode: 0755"
assert_contains "$ROOT/.goreleaser.yaml" "ids: [ployz]"
assert_contains "$ROOT/.goreleaser.yaml" "skip_upload: true"
assert_contains "$ROOT/.goreleaser.yaml" "clean break"
assert_contains "$ROOT/.github/workflows/release.yml" "check-release-tag.sh"
assert_contains "$ROOT/.github/workflows/release.yml" "release --clean --skip=publish"
assert_contains "$ROOT/.github/workflows/release.yml" "publish-github-release.sh"
assert_contains "$ROOT/.github/workflows/release.yml" "uses: ./.github/workflows/release-contracts.yml"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-release.sh macos"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "verify-release.sh linux"

manifest=$(mktemp)
printf 'version = "1.2.3"\n' > "$manifest"
"$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest"
if "$ROOT/scripts/check-release-tag.sh" v1.2.4 "$manifest" >/dev/null 2>&1; then
    echo "mismatched workspace version was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" nightly "$manifest" >/dev/null 2>&1; then
    echo "nightly tag was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" v1.2.3-rc.1 "$manifest" >/dev/null 2>&1; then
    echo "prerelease tag was accepted" >&2
    exit 1
fi
if "$ROOT/scripts/check-release-tag.sh" v1.2 "$manifest" >/dev/null 2>&1; then
    echo "non-SemVer tag was accepted" >&2
    exit 1
fi
rm -f "$manifest"

PLOYZ_RELEASE_TEST_ONLY=true source "$ROOT/scripts/publish-github-release.sh"
release_dist=$(mktemp -d)
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    : > "$release_dist/$archive"
done
: > "$release_dist/checksums.txt"
: > "$release_dist/ployz.rb"
assert_eq "$(release_assets "$release_dist" | xargs -n1 basename | sort)" \
    "$(printf '%s\n' checksums.txt ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz | sort)"
if ! printf '%s\n' "$(release_notes v1.2.3)" | grep -Fq "clean break"; then
    echo "release notes omit the clean-break statement" >&2
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

PLOYZ_INSTALL_TEST_ONLY=true source "$ROOT/scripts/install.sh"
assert_eq "$(daemon_archive x86_64)" "ployzd_linux_amd64.tar.gz"
assert_eq "$(daemon_archive aarch64)" "ployzd_linux_arm64.tar.gz"
if daemon_archive riscv64 >/dev/null 2>&1; then
    echo "unsupported daemon architecture was accepted" >&2
    exit 1
fi
assert_eq "$(daemon_action 1.2.3 latest 1.2.3)" "keep"
assert_eq "$(daemon_action 1.2.2 latest 1.2.3)" "replace"
assert_eq "$(daemon_action 1.2.4 latest 1.2.3)" "keep"
assert_eq "$(daemon_action 1.2.3 1.2.2 '')" "replace"
assert_eq "$(daemon_action 1.2.2 1.2.3 '')" "replace"

PLOYZ_CLI_INSTALL_TEST_ONLY=true source "$ROOT/install.sh"
assert_eq "$(cli_archive Linux x86_64)" "ployz_linux_amd64.tar.gz"
assert_eq "$(cli_archive Linux aarch64)" "ployz_linux_arm64.tar.gz"
assert_eq "$(cli_archive Darwin x86_64)" "ployz_macos_amd64.tar.gz"
assert_eq "$(cli_archive Darwin arm64)" "ployz_macos_arm64.tar.gz"
if cli_archive FreeBSD x86_64 >/dev/null 2>&1; then
    echo "unsupported CLI platform was accepted" >&2
    exit 1
fi

PLOYZ_UNINSTALL_TEST_ONLY=true source "$ROOT/scripts/uninstall.sh"
assert_eq "$(uninstall_disposition docker)" "retain"
assert_eq "$(uninstall_disposition docker-images)" "retain"
assert_eq "$(uninstall_disposition docker-volumes)" "retain"
assert_eq "$(uninstall_disposition ployz.service)" "remove"
assert_eq "$(uninstall_disposition ployzd)" "remove"
assert_eq "$(uninstall_disposition ployz-state)" "remove"

echo "release contracts passed"
