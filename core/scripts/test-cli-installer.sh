#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/release" "$TMP/install" "$TMP/keys"

# install.sh pins the key in crates/ployz-core/release.pub.
grep -Fqx "PLOYZ_RELEASE_PUBLIC_KEY=\${PLOYZ_RELEASE_PUBLIC_KEY:-$(sed -n 2p "$ROOT/crates/ployz-core/release.pub")}" "$ROOT/install.sh"

# A throwaway key stands in for the release key.
minisign -G -W -p "$TMP/keys/test.pub" -s "$TMP/keys/test.key" >/dev/null
minisign -G -W -p "$TMP/keys/other.pub" -s "$TMP/keys/other.key" >/dev/null
PLOYZ_RELEASE_PUBLIC_KEY=$(sed -n 2p "$TMP/keys/test.pub")
export PLOYZ_RELEASE_PUBLIC_KEY

sign_release() {
    local version=$1 key=${2:-$TMP/keys/test.key}
    rm -f "$TMP/release/checksums.txt.minisig"
    minisign -S -l -s "$key" -m "$TMP/release/checksums.txt" -x "$TMP/release/checksums.txt.minisig" \
        -t "ployz v$version" < /dev/null >/dev/null
}

pack_release() {
    local version=$1
    # shellcheck disable=SC2016
    printf '#!/bin/sh\nif [ "${1:-}" = --version ]; then echo %s; else echo installed; fi\n' "$version" > "$TMP/ployz"
    chmod 0755 "$TMP/ployz"
    for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
        tar -czf "$TMP/release/$archive" -C "$TMP" ployz
    done
    (cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
    sign_release "$version"
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
    sign_release 1.2.3
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

install_fails() {
    local message=$1 path=${2:-$TMP/bin:$PATH}
    if PATH="$path" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
        PLOYZ_GITHUB_URL=https://example.invalid /bin/sh "$ROOT/install.sh" 1.2.3 > "$TMP/error" 2>&1; then
        echo "installed despite: $message" >&2
        exit 1
    fi
    grep -Fq "$message" "$TMP/error" || { cat "$TMP/error" >&2; exit 1; }
    [ "$("$TMP/install/ployz" --version)" = 8.8.8-beta.1 ]
}

# The signature over checksums.txt is checked before any archive is trusted.
pack_release 1.2.3
rm "$TMP/release/checksums.txt.minisig"
install_fails 'Failed to download checksums.txt.minisig'
sign_release 1.2.3 "$TMP/keys/other.key"
install_fails 'Release signature verification failed'
sign_release 9.9.9
install_fails 'Release signature verification failed'
sign_release 1.2.3
# A rehashed archive: checksums.txt agrees with it, but the signature does not.
printf '#!/bin/sh\necho 1.2.3\n' > "$TMP/ployz"
tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz
(cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
install_fails 'Release signature verification failed'
pack_release 1.2.3
printf 'X' | dd of="$TMP/release/checksums.txt" bs=1 seek=0 conv=notrunc 2>/dev/null
install_fails 'Release signature verification failed'

# Without OpenSSL 3, minisign checks the signature; with neither, install.sh refuses.
mkdir -p "$TMP/no-openssl3" "$TMP/no-verifier"
printf '#!/bin/sh\nexit 1\n' > "$TMP/no-openssl3/openssl"
chmod 0755 "$TMP/no-openssl3/openssl"
pack_release 1.2.3
printf 'X' | dd of="$TMP/release/checksums.txt" bs=1 seek=0 conv=notrunc 2>/dev/null
install_fails 'Release signature verification failed' "$TMP/no-openssl3:$TMP/bin:$PATH"
for tool in cat cmp cp dd grep gzip head install mktemp rm sed sha256sum tar tr wc; do
    ln -s "$(command -v "$tool")" "$TMP/no-verifier/$tool"
done
install_fails 'Cannot verify the release signature: install OpenSSL 3 or minisign' "$TMP/bin:$TMP/no-verifier"
pack_release 1.2.3
PATH="$TMP/no-openssl3:$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid sh "$ROOT/install.sh" 1.2.3
[ "$("$TMP/install/ployz" --version)" = 1.2.3 ]

echo "CLI installer verifies the signature, checksum and binary before installing"
