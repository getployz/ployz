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
assert_contains "$ROOT/.github/workflows/release.yml" "workflow_dispatch"
assert_contains "$ROOT/.github/workflows/release.yml" "bounce-release-to-main.sh"
assert_contains "$ROOT/.github/workflows/release.yml" 'bounce-release-to-main.sh release.yml "Release ${{ github.ref_name }}"'
assert_contains "$ROOT/.github/workflows/release.yml" "run-name: Release \${{ inputs.tag || github.ref_name }}"
assert_contains "$ROOT/.github/workflows/release.yml" "if: github.event_name == 'push'"
assert_contains "$ROOT/.github/workflows/release.yml" "if: github.event_name == 'workflow_dispatch'"
assert_contains "$ROOT/.github/workflows/release.yml" 'publish-github-release.sh "${{ inputs.tag }}"'
assert_contains "$ROOT/.github/workflows/release.yml" 'check-release-tag.sh "${{ inputs.tag }}"'
assert_contains "$ROOT/.github/workflows/release.yml" "release-tag: \${{ inputs.tag }}"
if grep -Fq 'publish-github-release.sh "${{ github.ref_name }}"' "$ROOT/.github/workflows/release.yml"; then
    echo "publish still uses github.ref_name, which is main after the bounce" >&2
    exit 1
fi
assert_contains "$ROOT/.github/workflows/release-published.yml" "promote-release.sh"
assert_contains "$ROOT/.github/workflows/release-published.yml" "types: [published]"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "publish-sdk-package.sh"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "id-token: write"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "actions: write"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "Swatinem/rust-cache@v2"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "registry-url: https://registry.npmjs.org"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "types: [published]"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "workflow_dispatch"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "bounce-release-to-main.sh"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" 'bounce-release-to-main.sh publish-sdk.yml "Publish SDK ${{ github.event.release.tag_name }}"'
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "run-name: Publish SDK \${{ inputs.tag || github.event.release.tag_name }}"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "if: github.event_name == 'release'"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" "if: github.event_name == 'workflow_dispatch'"
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" 'ref: ${{ inputs.tag }}'
assert_contains "$ROOT/.github/workflows/publish-sdk.yml" 'publish-sdk-package.sh "${{ inputs.tag }}"'
if grep -Fq 'publish-sdk-package.sh "${{ github.event.release.tag_name || inputs.tag }}"' "$ROOT/.github/workflows/publish-sdk.yml"; then
    echo "sdk publish still uses the release event tag, which is empty after the bounce" >&2
    exit 1
fi
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
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "ubuntu-24.04-arm"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "RELEASE_ID: \${{ format('{0}-linux-{1}', matrix.binary, matrix.arch) }}"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "RELEASE_ID: ployz-darwin"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "runner: ubuntu-24.04-arm"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "**/dist/*.tar.gz"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "**/target/\${{ matrix.rust_target }}/release/sqlite-probe"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "binary: [ployz, ployzd, ployz-relay]"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "arch: [amd64, arm64]"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "pattern: release-linux-*"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "merge-multiple: true"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "taiki-e/install-action@v2"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "Swatinem/rust-cache@v2"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "scripts/pack-release.sh"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "release-tag:"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "ref: \${{ inputs.release-tag || github.sha }}"
assert_contains "$ROOT/.github/workflows/release-contracts.yml" 'publish-relay-image.sh "${{ inputs.release-tag || github.ref_name }}"'
assert_contains "$ROOT/.github/workflows/release-contracts.yml" "scripts/bounce-release-to-main.sh"
if grep -Fq 'publish-relay-image.sh "${{ github.ref_name }}"' "$ROOT/.github/workflows/release-contracts.yml"; then
    echo "relay publish still uses github.ref_name, which is main after the bounce" >&2
    exit 1
fi
assert_contains "$ROOT/.goreleaser.yaml" 'RELEASE_ID'
assert_contains "$ROOT/.goreleaser.yaml" "id: ployz-linux-amd64"
assert_contains "$ROOT/.goreleaser.yaml" "id: ployz-linux-arm64"
assert_contains "$ROOT/.goreleaser.yaml" "id: ployzd-linux-amd64"
assert_contains "$ROOT/.goreleaser.yaml" "id: ployzd-linux-arm64"
assert_contains "$ROOT/.goreleaser.yaml" "id: ployz-relay-linux-amd64"
assert_contains "$ROOT/.goreleaser.yaml" "id: ployz-relay-linux-arm64"
assert_contains "$ROOT/.goreleaser.yaml" "builds: [ployz-linux-amd64, ployz-linux-arm64, ployz-darwin]"
assert_contains "$ROOT/.goreleaser.yaml" "builds: [ployzd-linux-amd64, ployzd-linux-arm64]"
assert_contains "$ROOT/.goreleaser.yaml" "builds: [ployz-relay-linux-amd64, ployz-relay-linux-arm64]"
if grep -Fq 'x86_64-unknown-linux-musl,aarch64-unknown-linux-musl' "$ROOT/.github/workflows/release-contracts.yml"; then
    echo "linux still compiles both musl targets in one job" >&2
    exit 1
fi
if grep -Eq '^[[:space:]]*name: release-linux$' "$ROOT/.github/workflows/release-contracts.yml"; then
    echo "linux still publishes a single combined artifact" >&2
    exit 1
fi
if grep -q 'needs: gate' "$ROOT/.github/workflows/release.yml"; then
    echo "release still blocks artifacts on the CI gate" >&2
    exit 1
fi
if grep -q 'cargo install cargo-zigbuild' "$ROOT/.github/workflows/release-contracts.yml"; then
    echo "release still compiles cargo-zigbuild from source" >&2
    exit 1
fi

manifest=$(mktemp)
sdk_package=$(mktemp)
printf 'version = "1.2.3"\n' > "$manifest"
printf '{ "version": "1.2.3" }\n' > "$sdk_package"
"$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest" "$sdk_package"
printf 'version = "1.2.3-beta.1"\n' > "$manifest"
printf '{ "version": "1.2.3-beta.1" }\n' > "$sdk_package"
"$ROOT/scripts/check-release-tag.sh" v1.2.3-beta.1 "$manifest" "$sdk_package"
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
printf 'version = "1.2.3"\n' > "$manifest"
if "$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest" >/dev/null 2>&1; then
    echo "default sdk package mismatch was accepted" >&2
    exit 1
fi
printf '{ "version": "9.9.9" }\n' > "$sdk_package"
if "$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest" "$sdk_package" >/dev/null 2>&1; then
    echo "mismatched sdk package version was accepted" >&2
    exit 1
fi
missing_sdk=$(mktemp)
rm -f "$missing_sdk"
if "$ROOT/scripts/check-release-tag.sh" v1.2.3 "$manifest" "$missing_sdk" >/dev/null 2>&1; then
    echo "missing sdk package was accepted" >&2
    exit 1
fi
rm -f "$manifest" "$sdk_package"

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

inherited_apt_config=$(mktemp)
printf 'Acquire::Retries "7";' > "$inherited_apt_config"
APT_CONFIG=$inherited_apt_config configure_apt_lock_wait
assert_eq "$(cat "$APT_CONFIG")" $'Acquire::Retries "7";\nDPkg::Lock::Timeout "300";'
assert_eq "$(run_with_apt_lock_wait sh -c 'printf %s "$LC_ALL"')" C
rm -f "$inherited_apt_config"
apt_root=$(mktemp -d)
mkdir -p "$apt_root/lists/partial" "$apt_root/cache/archives/partial" "$apt_root/dpkg" "$apt_root/etc/sources.list.d"
: > "$apt_root/dpkg/status"
: > "$apt_root/etc/sources.list"
apt_args=(
    -o "Dir::State::lists=$apt_root/lists"
    -o "Dir::Cache=$apt_root/cache"
    -o "Dir::State::status=$apt_root/dpkg/status"
    -o "Dir::Etc::sourcelist=$apt_root/etc/sources.list"
    -o "Dir::Etc::sourceparts=$apt_root/etc/sources.list.d"
)
start_apt_lock_owner() {
    local lock=$1 ready=$1.ready
    rm -f "$ready"
    python3 - "$lock" "$ready" "$2" <<'PY' &
import fcntl
import sys
import time

with open(sys.argv[1], "w") as lock:
    fcntl.lockf(lock, fcntl.LOCK_EX)
    open(sys.argv[2], "w").close()
    time.sleep(int(sys.argv[3]))
PY
    lock_owner=$!
    while [ ! -e "$ready" ]; do
        kill -0 "$lock_owner" 2>/dev/null || return 1
        sleep 0.01
    done
}

APT_LOCK_TIMEOUT_SECONDS=3
start_apt_lock_owner "$apt_root/lists/lock" 1
run_with_apt_lock_wait apt-get "${apt_args[@]}" update >/dev/null
wait "$lock_owner"

APT_LOCK_TIMEOUT_SECONDS=1
start_apt_lock_owner "$apt_root/lists/lock" 2
if lock_error=$(run_with_apt_lock_wait apt-get "${apt_args[@]}" update 2>&1); then
    echo "apt accepted a lock held past its timeout" >&2
    exit 1
fi
wait "$lock_owner"
printf '%s\n' "$lock_error" | grep -Fq "$apt_root/lists/lock"
printf '%s\n' "$lock_error" | grep -Fq "python3"
printf '%s\n' "$lock_error" | grep -Fq "retry the installer"
archive_attempts=$(mktemp)
if archive_error=$(run_with_apt_lock_wait sh -c 'printf x >> "$1"; printf "%s\n" "E: Could not get lock /var/cache/apt/archives/lock. It is held by process 1 (apt-get)" "E: Unable to lock directory /var/cache/apt/archives/" >&2; exit 100' _ "$archive_attempts" 2>&1); then
    echo "fake archive lock unexpectedly succeeded" >&2
    exit 1
fi
assert_eq "$(cat "$archive_attempts")" xx
printf '%s\n' "$archive_error" | grep -Fq "/var/cache/apt/archives/lock"
printf '%s\n' "$archive_error" | grep -Fq "retry the installer"
rm -f "$archive_attempts"
if apt_error=$(run_with_apt_lock_wait apt-get "${apt_args[@]}" install -y definitely-not-a-package 2>&1); then
    echo "apt installed a nonexistent package" >&2
    exit 1
else
    apt_status=$?
fi
assert_eq "$apt_status" 100
printf '%s\n' "$apt_error" | grep -Fq "Unable to locate package definitely-not-a-package"
if printf '%s\n' "$apt_error" | grep -Fq "retry the installer"; then
    echo "non-lock apt failure was reported as a lock timeout" >&2
    exit 1
fi
rm -rf "$apt_root"
rm -f "$APT_CONFIG"

assert_eq "$(zfs_arc_max_for_memory_kib 524288)" 268435456
assert_eq "$(zfs_arc_max_for_memory_kib 2097152)" 536870912
assert_eq "$(zfs_arc_max_for_memory_kib 8388608)" 1073741824

assert_zfs_module_package() {
    local kernel=$1 owner=$2 available installed package_log
    shift 2
    available=$(printf '%s\n' "$@")
    installed=$(mktemp)
    rm -f "$installed"
    package_log=$(mktemp)
    (
        run_with_apt_lock_wait() {
            printf '%s\n' "$*" >> "$package_log"
            case "$*" in
                'apt-get download '*) : > "${!#}.deb" ;;
                *'apt-get install '*) printf '%s\n' "${!#}" > "$installed" ;;
            esac
        }
        function apt-cache {
            [ "$1" = show ] && printf '%s\n' "$available" | grep -Fxq "$2"
        }
        function dpkg-deb {
            [ "$1" = -c ] && [ "${2##*/}" = "$owner.deb" ] && \
                printf '%s\n' "./lib/modules/$kernel/kernel/fs/zfs/zfs.ko.zst"
        }
        function dpkg-query {
            [ "$1" = -L ] && [ -e "$installed" ] && [ "$2" = "$owner" ] && \
                printf '%s\n' "/lib/modules/$kernel/kernel/fs/zfs/zfs.ko.zst"
        }
        install_zfs_packages_ubuntu "$kernel"
    )
    assert_contains "$package_log" "apt-get update -qq"
    assert_contains "$package_log" "zfsutils-linux $owner"
    assert_eq "$(grep -Fc 'apt-get install' "$package_log")" 1
    rm -f "$installed" "$package_log"
}

assert_zfs_module_package 6.8.12-mainline \
    linux-main-modules-zfs-6.8.12-mainline \
    linux-main-modules-zfs-6.8.12-mainline
assert_zfs_module_package 6.8.0-1008-azure \
    linux-modules-zfs-6.8.0-1008-azure \
    linux-modules-zfs-6.8.0-1008-azure
assert_zfs_module_package 6.8.0-1008-aws \
    linux-modules-extra-6.8.0-1008-aws \
    linux-modules-extra-6.8.0-1008-aws
assert_zfs_module_package 5.15.0-139-generic \
    linux-modules-5.15.0-139-generic \
    linux-main-modules-zfs-5.15.0-139-generic \
    linux-modules-5.15.0-139-generic \
    linux-modules-extra-5.15.0-139-generic

if missing_kernel_error=$(
    (
        run_with_apt_lock_wait() { return 0; }
        function apt-cache { return 100; }
        install_zfs_packages_ubuntu 6.8.0-unsupported
    ) 2>&1
); then
    echo "installer accepted a running kernel without packaged ZFS support" >&2
    exit 1
fi
printf '%s\n' "$missing_kernel_error" | grep -Fq \
    "no packaged ZFS module for the running kernel 6.8.0-unsupported"

if module_download_error=$(
    (
        run_with_apt_lock_wait() {
            [ "$1 $2" != 'apt-get download' ]
        }
        function apt-cache {
            [ "$1" = show ] && [ "$2" = linux-modules-extra-6.8.0-download-failure ]
        }
        function dpkg-query { return 1; }
        install_zfs_packages_ubuntu 6.8.0-download-failure
    ) 2>&1
); then
    echo "installer hid a ZFS module package download failure" >&2
    exit 1
fi
printf '%s\n' "$module_download_error" | grep -Fq \
    "Could not download running-kernel module package linux-modules-extra-6.8.0-download-failure to inspect it for ZFS support"

if module_archive_error=$(
    (
        run_with_apt_lock_wait() {
            if [ "$1 $2" = 'apt-get download' ]; then
                : > "$3.deb"
            fi
        }
        function apt-cache {
            [ "$1" = show ] && [ "$2" = linux-modules-extra-6.8.0-archive-failure ]
        }
        function dpkg-query { return 1; }
        function dpkg-deb { return 2; }
        install_zfs_packages_ubuntu 6.8.0-archive-failure
    ) 2>&1
); then
    echo "installer hid a ZFS module package archive inspection failure" >&2
    exit 1
fi
printf '%s\n' "$module_archive_error" | grep -Fq \
    "Could not inspect downloaded running-kernel module package linux-modules-extra-6.8.0-archive-failure for ZFS support"

assert_eq "$PLOYZ_STORAGE" none
storage_selection_log=$(mktemp)
(
    prepare_zfs() { echo zfs >> "$storage_selection_log"; }
    prepare_storage
)
[ ! -s "$storage_selection_log" ]
if invalid_storage_error=$(PLOYZ_STORAGE=other prepare_storage 2>&1); then
    echo "installer accepted an unsupported storage preparation" >&2
    exit 1
fi
printf '%s\n' "$invalid_storage_error" | grep -Fq \
    "Unsupported storage preparation 'other'; expected none or zfs"
rm -f "$storage_selection_log"

zfs_prepare_log=$(mktemp)
(
    operating_system_id() { echo ubuntu; }
    container_virtualization() { echo none; }
    command_exists() { return 0; }
    uname() { echo 6.8.0-test-cloud; }
    require_host_root_reserve() { printf 'reserve %s\n' "$1" >> "$zfs_prepare_log"; }
    install_zfs_packages_ubuntu() { printf 'packages %s\n' "$1" >> "$zfs_prepare_log"; }
    zfs_arc_max() { echo 536870912; }
    persist_zfs_arc_max() { printf 'persist %s\n' "$1" >> "$zfs_prepare_log"; }
    modprobe() { printf 'modprobe %s\n' "$1" >> "$zfs_prepare_log"; }
    set_and_verify_zfs_arc_max() { printf 'verify %s\n' "$1" >> "$zfs_prepare_log"; }
    validate_zfs() { echo smoke >> "$zfs_prepare_log"; }
    PLOYZ_STORAGE=zfs prepare_storage >/dev/null
)
assert_eq "$(cat "$zfs_prepare_log")" "$(printf '%s\n' \
    'reserve 134217728' \
    'packages 6.8.0-test-cloud' \
    'persist 536870912' \
    'modprobe zfs' \
    'verify 536870912' \
    smoke)"
rm -f "$zfs_prepare_log"

if reserve_error=$(
    (
        function df { printf '%s\n' 'Size Avail' '21474836480 10737418240'; }
        require_host_root_reserve 1
    ) 2>&1
); then
    echo "ZFS preparation accepted an inadequate host-root reserve" >&2
    exit 1
fi
printf '%s\n' "$reserve_error" | grep -Fq "preserving the 10737418240-byte host-root reserve"

setup_zfs_smoke_fake() {
    zfs_smoke_scenario=$1
    zfs_smoke_state=$(mktemp)
    zfs_smoke_log=$(mktemp)
    rm -f "$zfs_smoke_state"
}

fake_zpool() {
    printf 'zpool %s\n' "$*" >> "$zfs_smoke_log"
    case "$1" in
        create) printf '%s\n' "${@: -2:1}" > "$zfs_smoke_state" ;;
        list)
            if [ "$2" = -Hp ]; then
                if [ "$zfs_smoke_scenario" = interrupt ]; then
                    zfs_smoke_scenario=interrupted
                    kill -TERM "${BASHPID:-$$}"
                fi
                [ "$zfs_smoke_scenario" != query-failure ] && [ -e "$zfs_smoke_state" ]
            elif [ "$zfs_smoke_scenario" = cleanup-probe-failure ]; then
                echo "simulated zpool list failure" >&2
                return 2
            else
                [ ! -e "$zfs_smoke_state" ] || cat "$zfs_smoke_state"
            fi
            ;;
        destroy)
            [ "$zfs_smoke_scenario" != destroy-failure ] || return 1
            rm -f "$zfs_smoke_state"
            ;;
    esac
}

fake_zfs() {
    printf 'zfs %s\n' "$*" >> "$zfs_smoke_log"
    [ ! -e "$zfs_smoke_state" ] || cat "$zfs_smoke_state"
}

run_fake_zfs_smoke() {
    zfs_smoke_bytes() { echo 1048576; }
    require_host_root_reserve() { return 0; }
    findmnt() { [ "$1" = -n ] && echo /; }
    zpool() { fake_zpool "$@"; }
    zfs() { fake_zfs "$@"; }
    validate_zfs
}

cleanup_zfs_smoke_fake() {
    local backing
    backing=$(awk '$1 == "zpool" && $2 == "create" { print $NF }' "$zfs_smoke_log")
    rm -f "$zfs_smoke_state" "$zfs_smoke_log" "$backing"
}

setup_zfs_smoke_fake success
(
    trap : INT
    trap : TERM
    previous_int_trap=$(trap -p INT)
    previous_term_trap=$(trap -p TERM)
    run_fake_zfs_smoke
    [ "$(trap -p INT)" = "$previous_int_trap" ]
    [ "$(trap -p TERM)" = "$previous_term_trap" ]
)
assert_contains "$zfs_smoke_log" "zpool create -f -m none -o cachefile=none"
assert_contains "$zfs_smoke_log" "zpool list -Hp -o name,size,alloc,free"
assert_contains "$zfs_smoke_log" "zfs list -Hp -o name,mountpoint"
assert_contains "$zfs_smoke_log" "zpool destroy -f"
smoke_backing=$(awk '$1 == "zpool" && $2 == "create" { print $NF }' "$zfs_smoke_log")
[ ! -e "$zfs_smoke_state" ]
[ ! -e "$smoke_backing" ]
cleanup_zfs_smoke_fake

setup_zfs_smoke_fake query-failure
if smoke_error=$(
    (run_fake_zfs_smoke) 2>&1
); then
    echo "installer accepted a failed ZFS smoke query" >&2
    exit 1
fi
printf '%s\n' "$smoke_error" | grep -Fq "Could not query temporary ZFS smoke Pool"
assert_contains "$zfs_smoke_log" "zpool destroy -f"
smoke_backing=$(awk '$1 == "zpool" && $2 == "create" { print $NF }' "$zfs_smoke_log")
[ ! -e "$smoke_backing" ]
cleanup_zfs_smoke_fake

setup_zfs_smoke_fake destroy-failure
if cleanup_error=$(
    (run_fake_zfs_smoke) 2>&1
); then
    echo "installer accepted a failed ZFS smoke cleanup" >&2
    exit 1
fi
printf '%s\n' "$cleanup_error" | grep -Fq "Could not destroy temporary ZFS smoke Pool"
printf '%s\n' "$cleanup_error" | grep -Fq "then remove /var/tmp/ployz-zfs-smoke."
cleanup_zfs_smoke_fake

setup_zfs_smoke_fake cleanup-probe-failure
if cleanup_probe_error=$(
    (run_fake_zfs_smoke) 2>&1
); then
    echo "installer accepted an unverified ZFS smoke cleanup" >&2
    exit 1
fi
printf '%s\n' "$cleanup_probe_error" | grep -Fq \
    "Could not verify cleanup of temporary ZFS smoke Pool"
printf '%s\n' "$cleanup_probe_error" | grep -Fq "simulated zpool list failure"
cleanup_zfs_smoke_fake

setup_zfs_smoke_fake interrupt
if (run_fake_zfs_smoke); then
    echo "installer accepted an interrupted ZFS smoke test" >&2
    exit 1
else
    interrupt_status=$?
fi
assert_eq "$interrupt_status" 143
assert_contains "$zfs_smoke_log" "zpool destroy -f"
smoke_backing=$(awk '$1 == "zpool" && $2 == "create" { print $NF }' "$zfs_smoke_log")
[ ! -e "$zfs_smoke_state" ]
[ ! -e "$smoke_backing" ]
cleanup_zfs_smoke_fake

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
assert_eq "$(uninstall_disposition ployz-volume-plugin.socket)" "remove"
assert_eq "$(uninstall_disposition ployz-volume-plugin.service)" "remove"
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

PLOYZ_BOUNCE_RELEASE_TEST_ONLY=true source "$ROOT/scripts/bounce-release-to-main.sh"
assert_eq "$(printf '%s\n' '[{"databaseId":2,"displayTitle":"Release v1.2.3"},{"databaseId":3,"displayTitle":"Release v1.2.3"}]' | newest_run_id_named_except "Release v1.2.3" $'2\n')" "3"
assert_eq "$(printf '%s\n' '[]' | newest_run_id_named_except "Release v1.2.3" "")" ""
assert_contains "$ROOT/scripts/bounce-release-to-main.sh" 'wanted=${2:-}'
assert_contains "$ROOT/scripts/bounce-release-to-main.sh" 'gh workflow run "$workflow"'
assert_contains "$ROOT/scripts/bounce-release-to-main.sh" "--event=workflow_dispatch"
assert_contains "$ROOT/scripts/bounce-release-to-main.sh" "gh run watch"
assert_contains "$ROOT/scripts/bounce-release-to-main.sh" "--exit-status"
if grep -Fq "gh workflow run release.yml" "$ROOT/scripts/bounce-release-to-main.sh"; then
    echo "bounce script is still hardcoded to release.yml" >&2
    exit 1
fi
if grep -Fq GITHUB_REF_NAME "$ROOT/scripts/pack-release.sh"; then
    echo "pack-release still takes the tag from GITHUB_REF_NAME, which is main after the bounce" >&2
    exit 1
fi
assert_contains "$ROOT/scripts/pack-release.sh" "tag=v\$version"

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

PLOYZ_SDK_PUBLISH_TEST_ONLY=true source "$ROOT/scripts/publish-sdk-package.sh"
assert_eq "$(sdk_npm_publish_args v1.2.3-beta.1 | tr '\n' ' ')" "--access public --tag beta "
assert_eq "$(sdk_npm_publish_args v1.2.3 | tr '\n' ' ')" "--access public --tag latest "
if sdk_npm_publish_args nightly >/dev/null 2>&1; then
    echo "nightly tag was accepted for npm publish" >&2
    exit 1
fi

sdk_src=$(mktemp -d)
printf '%s\n' '{"name":"@ployz/sdk","version":"1.2.3","private":true,"main":"index.js"}' > "$sdk_src/package.json"
printf 'js\n' > "$sdk_src/index.js"
printf 'dts\n' > "$sdk_src/index.d.ts"
printf 'browser\n' > "$sdk_src/browser.mjs"
mkdir -p "$sdk_src/generated"
printf 'gen\n' > "$sdk_src/generated/payloads.d.ts"
sdk_bindings=$(mktemp -d)
printf 'linux\n' > "$sdk_bindings/ployz-sdk.linux-x64.node"
printf 'darwin\n' > "$sdk_bindings/ployz-sdk.darwin-arm64.node"
sdk_dest=$(mktemp -d)
if PLOYZ_SDK_PACKAGE_ROOT="$sdk_src" bash "$ROOT/scripts/pack-sdk-package.sh" "$sdk_dest" "$sdk_bindings"/*.node >/dev/null 2>&1; then
    echo "private sdk package was packed" >&2
    exit 1
fi
printf '%s\n' '{"name":"@ployz/sdk","version":"1.2.3","main":"index.js","files":["index.js","index.d.ts","generated"],"engines":{"node":">=18"},"publishConfig":{"access": "public"}}' > "$sdk_src/package.json"
PLOYZ_SDK_PACKAGE_ROOT="$sdk_src" bash "$ROOT/scripts/pack-sdk-package.sh" "$sdk_dest" "$sdk_bindings"/*.node
assert_eq "$(cat "$sdk_dest/npm/linux-x64/ployz-sdk.node")" linux
assert_eq "$(cat "$sdk_dest/npm/darwin-arm64/ployz-sdk.node")" darwin
assert_contains "$sdk_dest/npm/linux-x64/package.json" '"name": "@ployz/sdk-linux-x64"'
assert_contains "$sdk_dest/npm/linux-x64/package.json" '"os": ['
assert_contains "$sdk_dest/npm/darwin-arm64/package.json" '"arm64"'
assert_contains "$sdk_dest/package.json" '"@ployz/sdk-darwin-arm64": "1.2.3"'
assert_contains "$sdk_dest/package.json" '"access": "public"'
assert_eq "$(cat "$sdk_dest/browser.mjs")" browser
if [ -e "$sdk_dest/ployz-sdk.node" ]; then
    echo "the js package still ships a native binding" >&2
    exit 1
fi
if grep -Eq '"private":[[:space:]]*true' "$sdk_dest/package.json"; then
    echo "packed sdk package is still private" >&2
    exit 1
fi
if PLOYZ_SDK_PACKAGE_ROOT="$sdk_src" bash "$ROOT/scripts/pack-sdk-package.sh" "$sdk_dest" >/dev/null 2>&1; then
    echo "sdk pack without a native binding was accepted" >&2
    exit 1
fi
if PLOYZ_SDK_PACKAGE_ROOT="$sdk_src" bash "$ROOT/scripts/pack-sdk-package.sh" "$sdk_dest" "$sdk_src/index.js" >/dev/null 2>&1; then
    echo "sdk pack accepted a misnamed binding" >&2
    exit 1
fi
rm -rf "$sdk_src" "$sdk_dest" "$sdk_bindings"

echo "release contracts passed"
