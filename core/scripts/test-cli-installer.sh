#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/release" "$TMP/install"

pack_release() {
    local version=$1
    # shellcheck disable=SC2016
    printf '#!/bin/sh\nif [ "${1:-}" = --version ]; then echo %s; else echo installed; fi\n' "$version" > "$TMP/ployz"
    chmod 0755 "$TMP/ployz"
    for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
        tar -czf "$TMP/release/$archive" -C "$TMP" ployz
    done
    (cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
}
pack_release 1.2.3
printf 'v1.2.3\n' > "$TMP/release/stable"

cat > "$TMP/bin/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; esac
EOF
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/sh
echo "$*" >> "${FAKE_CURL_LOG:-/dev/null}"
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then output=$2; shift 2; else url=$1; shift; fi
done
src="$FAKE_RELEASE/${url##*/}"
[ -f "$src" ] || exit 1
cp "$src" "$output"
EOF
chmod 0755 "$TMP/bin/uname" "$TMP/bin/curl"

for platform in Linux:x86_64 Linux:aarch64 Darwin:x86_64 Darwin:arm64; do
    FAKE_OS=${platform%:*}
    FAKE_ARCH=${platform#*:}
    PATH="$TMP/bin:$PATH" FAKE_OS="$FAKE_OS" FAKE_ARCH="$FAKE_ARCH" FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh"
    [ "$("$TMP/install/ployz")" = installed ]
    [ "$("$TMP/install/ployz" --version)" = 1.2.3 ]
done

pack_release 9.9.9
printf 'v9.9.9\n' > "$TMP/release/stable"
FAKE_CURL_LOG=$TMP/curl.log
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" stable
grep -Fq '/releases/download/v9.9.9/ployz_linux_amd64.tar.gz' "$FAKE_CURL_LOG"

pack_release 8.8.8-beta.1
printf 'v8.8.8-beta.1\n' > "$TMP/release/beta"
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" beta
grep -Fq '/releases/download/v8.8.8-beta.1/ployz_linux_amd64.tar.gz' "$FAKE_CURL_LOG"

# A pointer is one raw line: whitespace inside it is corruption, not formatting.
for pointer in 'v1.2.\n3' 'v1.2.3\ngarbage'; do
    printf '%b\n' "$pointer" > "$TMP/release/stable"
    if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh" stable > "$TMP/error" 2>&1; then
        echo "installed from pointer '$pointer'" >&2
        exit 1
    fi
    grep -Fq 'stable channel is unavailable' "$TMP/error"
done

# stable never installs a prerelease, and retired channel names are not versions.
printf 'v8.8.8-beta.1\n' > "$TMP/release/stable"
for case in 'stable:stable channel is unavailable' 'latest:Invalid version' \
    'nightly:Invalid version' '1.2.03:Invalid version' '18446744073709551616.0.0:Invalid version'; do
    requested=${case%%:*}
    if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh" "$requested" > "$TMP/error" 2>&1; then
        echo "$requested was installed" >&2
        exit 1
    fi
    grep -Fq "${case#*:}" "$TMP/error"
done

# Missing and mismatched binaries must fail before replacing the installed one.
for defect in missing wrong-version; do
    pack_release 1.2.3
    if [ "$defect" = missing ]; then
        touch "$TMP/README"
        tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" README
    else
        printf '#!/bin/sh\necho wrong\n' > "$TMP/ployz"
        tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz
    fi
    (cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
    if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh" 1.2.3 > "$TMP/error" 2>&1; then
        echo "accepted $defect binary" >&2
        exit 1
    fi
    grep -Eq 'Release archive is incomplete|Release binary ployz has version' "$TMP/error"
    [ "$("$TMP/install/ployz" --version)" = 8.8.8-beta.1 ]
done
pack_release 1.2.3
printf corrupt >> "$TMP/release/ployz_linux_amd64.tar.gz"
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
    PLOYZ_GITHUB_URL=https://example.invalid sh "$ROOT/install.sh" 1.2.3 >/dev/null 2>&1; then
    echo "corrupt CLI archive was installed" >&2
    exit 1
fi
[ "$("$TMP/install/ployz")" = installed ]

# The checkin stub records what it was given, then answers as FAKE_ANSWER says Cloud did.
pack_release 1.2.3
cat > "$TMP/ployz" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then echo 1.2.3; exit 0; fi
{
    echo "argv=$*"
    echo "enroll=${PLOYZ_ENROLL_TOKEN:-}"
    echo "skip_zfs=${PLOYZ_SKIP_ZFS:-}"
    echo "mode=$(stat -c %a "$FLEET_TOKEN_FILE")"
    echo "token=$(cat "$FLEET_TOKEN_FILE")"
} > "$CHECKIN_LOG"
stop() {
    printf '{\n  "answer": "stop",\n  "reason": "%s"\n}\n' "$1" > "$CHECKIN_STATUS"
    rm -f "$FLEET_TOKEN_FILE"
}
case "${FAKE_ANSWER:-wait}" in
    wait) echo "Waiting for approval" ;;
    expired) stop token_expired; echo "Token expired. The Fleet Token expired." >&2; exit 1 ;;
    revoked) stop token_revoked; echo "Token revoked. The Fleet Token was revoked." >&2; exit 1 ;;
    unreachable) echo "Could not reach Cloud." >&2; exit 1 ;;
esac
EOF
tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz
(cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
cat > "$TMP/bin/id" <<'EOF'
#!/bin/sh
echo "${FAKE_UID:-0}"
EOF
chmod 0755 "$TMP/bin/id"

TOKEN=pflt_abc123
export FLEET_TOKEN_FILE=$TMP/etc/ployz/fleet-token CHECKIN_STATUS=$TMP/status.json CHECKIN_LOG=$TMP/checkin.log
enroll() {
    rm -rf "${TMP:?}/etc" "$CHECKIN_LOG" "$TMP/install/ployz"
    env PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
        PLOYZ_GITHUB_URL=https://example.invalid "$@" sh "$ROOT/install.sh" 1.2.3 > "$TMP/out" 2> "$TMP/err"
}

for value in unset '' cet_abc123 PFLT_abc123; do
    if [ "$value" = unset ]; then enroll; else enroll PLOYZ_ENROLL_TOKEN="$value"; fi
    printf '%s\n' 'ployz_linux_amd64.tar.gz: OK' "Installed ployz to $TMP/install/ployz" | diff - "$TMP/out"
    [ ! -s "$TMP/err" ]
    [ ! -e "$FLEET_TOKEN_FILE" ]
    [ ! -e "$CHECKIN_LOG" ]
done

enroll PLOYZ_ENROLL_TOKEN=$TOKEN PLOYZ_SKIP_ZFS=1
[ "$(head -n 1 "$TMP/out")" = "Installs Docker and Ployz. Nothing joins until you say so." ]
[ "$(tail -n 1 "$TMP/out")" = "Waiting for approval" ]
[ "$(stat -c %a "$FLEET_TOKEN_FILE")" = 600 ]
[ "$(cat "$FLEET_TOKEN_FILE")" = $TOKEN ]
printf '%s\n' "argv=server checkin --install --token-file $FLEET_TOKEN_FILE" enroll= skip_zfs=1 mode=600 \
    "token=$TOKEN" | diff - "$CHECKIN_LOG"

if enroll PLOYZ_ENROLL_TOKEN=$TOKEN FAKE_UID=1000; then
    echo "enrolled without root" >&2
    exit 1
fi
grep -Fq 'Run it as root' "$TMP/err"
[ ! -e "$TMP/install/ployz" ]
[ ! -e "$FLEET_TOKEN_FILE" ]

for case in 'revoked:no' 'expired:yes' 'unreachable:no'; do
    if enroll PLOYZ_ENROLL_TOKEN=$TOKEN FAKE_ANSWER="${case%:*}"; then
        echo "${case%:*} answer exited 0" >&2
        exit 1
    fi
    # The unreachable run inherits the expired status.json the earlier runs left behind.
    if grep -Fq 'Token expired. Run the install again with a new token.' "$TMP/err"; then said=yes; else said=no; fi
    [ "$said" = "${case#*:}" ] || { echo "${case%:*}: expired line printed=$said" >&2; exit 1; }
done

echo "CLI installer verifies the checksum and binary before installing, and checks in with a Fleet Token"
