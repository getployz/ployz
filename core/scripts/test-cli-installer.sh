#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/release" "$TMP/install"

pack_release() {
    local version=$1
    # shellcheck disable=SC2016
    printf '#!/bin/sh\nif [ "${1:-}" = version ]; then echo %s; else echo installed; fi\n' "$version" > "$TMP/ployz"
    printf '#!/bin/sh\necho %s\n' "$version" > "$TMP/ployz-tailcat"
    chmod 0755 "$TMP/ployz" "$TMP/ployz-tailcat"
    for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
        tar -czf "$TMP/release/$archive" -C "$TMP" ployz ployz-tailcat
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
        sh "$ROOT/install.sh" latest
    [ "$("$TMP/install/ployz")" = installed ]
    [ "$("$TMP/install/ployz-tailcat" version)" = 1.2.3 ]
done

pack_release 9.9.9
printf 'v9.9.9\n' > "$TMP/release/stable"
FAKE_CURL_LOG=$TMP/curl.log
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" latest
grep -Fq '/releases/download/v9.9.9/ployz_linux_amd64.tar.gz' "$FAKE_CURL_LOG"

pack_release 8.8.8-beta.1
printf 'v8.8.8-beta.1\n' > "$TMP/release/beta"
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" beta
grep -Fq '/releases/download/v8.8.8-beta.1/ployz_linux_amd64.tar.gz' "$FAKE_CURL_LOG"

if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" nightly >/dev/null 2>&1; then
    echo "nightly channel was accepted" >&2
    exit 1
fi

# Missing and mismatched helpers must fail before replacing either installed binary.
for defect in missing wrong-version; do
    pack_release 1.2.3
    if [ "$defect" = missing ]; then
        tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz
    else
        printf '#!/bin/sh\necho wrong\n' > "$TMP/ployz-tailcat"
        tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz ployz-tailcat
    fi
    (cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
    if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh" 1.2.3 > "$TMP/error" 2>&1; then
        echo "accepted $defect helper" >&2
        exit 1
    fi
    grep -Eq 'Release archive is incomplete|Release binary ployz-tailcat has version' "$TMP/error"
    [ "$("$TMP/install/ployz" version)" = 8.8.8-beta.1 ]
    [ "$("$TMP/install/ployz-tailcat" version)" = 8.8.8-beta.1 ]
done
pack_release 1.2.3
printf corrupt >> "$TMP/release/ployz_linux_amd64.tar.gz"
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
    PLOYZ_GITHUB_URL=https://example.invalid sh "$ROOT/install.sh" 1.2.3 >/dev/null 2>&1; then
    echo "corrupt CLI archive was installed" >&2
    exit 1
fi
[ "$("$TMP/install/ployz")" = installed ]

echo "CLI installer verifies checksums and both binaries before installing"
