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
