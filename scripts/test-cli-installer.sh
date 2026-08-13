#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/release" "$TMP/install"

cat > "$TMP/ployz" <<'EOF'
#!/bin/sh
echo installed
EOF
chmod 0755 "$TMP/ployz"
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
    tar -czf "$TMP/release/$archive" -C "$TMP" ployz
done
(cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)

cat > "$TMP/bin/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; esac
EOF
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/sh
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then output=$2; shift 2; else url=$1; shift; fi
done
cp "$FAKE_RELEASE/${url##*/}" "$output"
EOF
chmod 0755 "$TMP/bin/uname" "$TMP/bin/curl"

for platform in Linux:x86_64 Linux:aarch64 Darwin:x86_64 Darwin:arm64; do
    FAKE_OS=${platform%:*}
    FAKE_ARCH=${platform#*:}
    PATH="$TMP/bin:$PATH" FAKE_OS="$FAKE_OS" FAKE_ARCH="$FAKE_ARCH" FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh" latest
    [ "$("$TMP/install/ployz")" = installed ]
done

printf corrupt >> "$TMP/release/ployz_linux_amd64.tar.gz"
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
    PLOYZ_GITHUB_URL=https://example.invalid sh "$ROOT/install.sh" 1.2.3 >/dev/null 2>&1; then
    echo "corrupt CLI archive was installed" >&2
    exit 1
fi
[ "$("$TMP/install/ployz")" = installed ]

echo "CLI installer accepts valid checksums and rejects corrupt archives"
