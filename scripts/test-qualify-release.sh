#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

fail() {
    echo "qualify-release check failed: $*" >&2
    exit 1
}

bash -n "$ROOT/scripts/qualify-release.sh"
bash -n "$ROOT/scripts/check-layer3-runner.sh"
bash -n "$ROOT/scripts/check-product-paths.sh"

if grep -qi vultr "$ROOT/scripts/qualify-release.sh" "$ROOT/docs/RELEASE.md"; then
    fail "authority path still names a cloud vendor"
fi

grep -Fq 'PLOYZ_RELEASE_DIR' "$ROOT/scripts/install.sh" || fail "install.sh does not honor PLOYZ_RELEASE_DIR"
grep -Fq 'action=replace' "$ROOT/scripts/install.sh" || fail "install.sh does not force replace for a local archive"
grep -Fq 'PLOYZ_QUALIFY_RESET' "$ROOT/scripts/qualify-release.sh" || fail "qualify-release has no reset opt-in"
grep -Fq -- '--ssh-key' "$ROOT/scripts/qualify-release.sh" || fail "qualify-release does not forward an SSH identity to ployz"
grep -Fq 'qualify-data' "$ROOT/scripts/qualify-release/compose.yaml" || fail "compose fixture has no named volume"
grep -Fq -- '--no-fail-fast' "$ROOT/scripts/run-layer3-tests.sh" || fail "layer3 runner still fail-fasts"

if "$ROOT/scripts/qualify-release.sh" >/dev/null 2>&1; then
    fail "qualify-release accepted empty hosts"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
dummy=$TMP/ployz
printf '#!/bin/sh\necho dummy\n' > "$dummy"
chmod 0755 "$dummy"
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    tar -czf "$TMP/$archive" -C "$TMP" ployz
done

output=$(
    PLOYZ_QUALIFY_HOSTS='root@192.0.2.10 root@192.0.2.11' \
        PLOYZ_ARTIFACT_DIR="$TMP" \
        PLOYZ_QUALIFY_DRY_RUN=1 \
        "$ROOT/scripts/qualify-release.sh"
)
printf '%s\n' "$output" | grep -Fq 'qualify dry-run' || fail "dry-run did not print the plan"
printf '%s\n' "$output" | grep -Fq 'named volume' || fail "dry-run omitted the named-volume step"
printf '%s\n' "$output" | grep -Fq 'reset: no' || fail "dry-run omitted the default no-reset policy"
printf '%s\n' "$output" | grep -Fq 'always replace' || fail "dry-run omitted forced daemon replace"

key_output=$(
    PLOYZ_QUALIFY_HOSTS='root@192.0.2.10' \
        PLOYZ_ARTIFACT_DIR="$TMP" \
        PLOYZ_QUALIFY_DRY_RUN=1 \
        PLOYZ_QUALIFY_SSH_OPTS='-o StrictHostKeyChecking=accept-new -i /tmp/qualify-key' \
        "$ROOT/scripts/qualify-release.sh"
)
printf '%s\n' "$key_output" | grep -Fq 'ssh-key: /tmp/qualify-key' || fail "dry-run did not forward -i to ployz"

reset_output=$(
    PLOYZ_QUALIFY_HOSTS='root@192.0.2.10' \
        PLOYZ_ARTIFACT_DIR="$TMP" \
        PLOYZ_QUALIFY_DRY_RUN=1 \
        PLOYZ_QUALIFY_RESET=1 \
        "$ROOT/scripts/qualify-release.sh"
)
printf '%s\n' "$reset_output" | grep -Fq 'reset: yes' || fail "dry-run omitted reset when PLOYZ_QUALIFY_RESET=1"

"$ROOT/scripts/check-layer3-runner.sh"
"$ROOT/scripts/check-product-paths.sh"

echo "qualify-release contracts passed"
