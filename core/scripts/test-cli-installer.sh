#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-archives.sh
source "$ROOT/scripts/release-archives.sh"
linux_amd64=$(release_archive ployz linux amd64)
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
    for archive in $(release_archives ployz); do
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
# Like curl when nothing answers on any address it resolved: no HTTP status, exit 7.
[ -z "${FAKE_UNREACHABLE:-}" ] || { printf 000; exit 7; }
case "$*" in *"${FAKE_UNREACHABLE_HOST:-unset.invalid}"/*) printf 000; exit 7 ;; esac
write_out=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) output=$2; shift 2 ;;
        -w) write_out=1; shift 2 ;;
        *) url=$1; shift ;;
    esac
done
src="$FAKE_RELEASE/${url##*/}"
# Like curl: without -f a missing file is a 404 it reports through -w; with -f it fails.
if [ ! -f "$src" ]; then
    [ -n "$write_out" ] || exit 22
    printf 404
    exit 0
fi
cp "$src" "$output"
[ -z "$write_out" ] || printf 200
EOF
chmod 0755 "$TMP/bin/uname" "$TMP/bin/curl"

# Each platform install.sh can detect, and the manifest os and arch of the archive it must fetch.
for platform in Linux:x86_64:linux:amd64 Linux:aarch64:linux:arm64 Darwin:x86_64:macos:amd64 Darwin:arm64:macos:arm64; do
    IFS=: read -r FAKE_OS FAKE_ARCH release_os release_arch <<< "$platform"
    rm -f "$TMP/urls"
    PATH="$TMP/bin:$PATH" FAKE_OS="$FAKE_OS" FAKE_ARCH="$FAKE_ARCH" FAKE_RELEASE="$TMP/release" \
        FAKE_CURL_LOG="$TMP/urls" INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh"
    grep -Fq "/$(release_archive ployz "$release_os" "$release_arch")" "$TMP/urls" || { cat "$TMP/urls" >&2; exit 1; }
    [ "$("$TMP/install/ployz")" = installed ]
    [ "$("$TMP/install/ployz" --version)" = 1.2.3 ]
done

# The release comes from github.com, and through ployz.sh only when github.com can't be reached, as
# on a server with only IPv6.
for reach in github fallback; do
    rm -f "$TMP/urls"
    env -u PLOYZ_GITHUB_URL -u PLOYZ_FALLBACK_URL PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 \
        FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$TMP/urls" INSTALL_BIN_DIR="$TMP/install" \
        FAKE_UNREACHABLE_HOST=$([ "$reach" = github ] || echo github.com) sh "$ROOT/install.sh" 1.2.3 \
        > /dev/null 2>&1
    [ "$("$TMP/install/ployz" --version)" = 1.2.3 ]
    case $reach in
        github) from=https://github.com/getployz/ployz/releases/download/v1.2.3 ;;
        fallback) from=https://ployz.sh/releases/download/v1.2.3 ;;
    esac
    for asset in checksums.txt checksums.txt.minisig "$linux_amd64"; do
        grep -Fq "$from/$asset" "$TMP/urls" || { cat "$TMP/urls" >&2; exit 1; }
    done
    if [ "$reach" = github ] && grep -Fq https://ployz.sh/releases "$TMP/urls"; then
        cat "$TMP/urls" >&2
        exit 1
    fi
done

pack_release 9.9.9
printf 'v9.9.9\n' > "$TMP/release/stable"
FAKE_CURL_LOG=$TMP/curl.log
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" stable
grep -Fq "/releases/download/v9.9.9/$linux_amd64" "$FAKE_CURL_LOG"

pack_release 8.8.8-beta.1
printf 'v8.8.8-beta.1\n' > "$TMP/release/beta"
: > "$FAKE_CURL_LOG"
PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" beta
grep -Fq "/releases/download/v8.8.8-beta.1/$linux_amd64" "$FAKE_CURL_LOG"

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

# A version with no release says so, and a missing install directory is named before any download.
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/nonexistent" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" 0.3.0-beta.99 > "$TMP/error" 2>&1; then
    echo "installed a release that doesn't exist" >&2
    exit 1
fi
grep -Fqx "ERROR: There's no Ployz release 0.3.0-beta.99. Releases: https://example.invalid/releases" "$TMP/error"
: > "$FAKE_CURL_LOG"
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_CURL_LOG="$FAKE_CURL_LOG" \
    INSTALL_BIN_DIR="$TMP/nonexistent" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" 1.2.3 > "$TMP/error" 2>&1; then
    echo "installed into a missing directory" >&2
    exit 1
fi
grep -Fq "$TMP/nonexistent is not a directory" "$TMP/error"
[ ! -s "$FAKE_CURL_LOG" ]
# Real curl over file://, as verify-release.sh installs: it reports status 000 on success.
mkdir -p "$TMP/real-curl-bin"
cp "$TMP/bin/uname" "$TMP/real-curl-bin/uname"
pack_release 1.2.3
mkdir -p "$TMP/mirror/releases/download/v1.2.3" "$TMP/mirror-install"
cp "$TMP/release/$linux_amd64" "$TMP/release/checksums.txt" "$TMP/release/checksums.txt.minisig" "$TMP/mirror/releases/download/v1.2.3/"
PATH="$TMP/real-curl-bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 INSTALL_BIN_DIR="$TMP/mirror-install" \
    PLOYZ_GITHUB_URL="file://$TMP/mirror" sh "$ROOT/install.sh" 1.2.3 > /dev/null
[ "$("$TMP/mirror-install/ployz" --version)" = 1.2.3 ]

# Missing and mismatched binaries must fail before replacing the installed one.
for defect in missing wrong-version; do
    pack_release 1.2.3
    if [ "$defect" = missing ]; then
        touch "$TMP/README"
        tar -czf "$TMP/release/$linux_amd64" -C "$TMP" README
    else
        printf '#!/bin/sh\necho wrong\n' > "$TMP/ployz"
        tar -czf "$TMP/release/$linux_amd64" -C "$TMP" ployz
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
printf corrupt >> "$TMP/release/$linux_amd64"
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
    PLOYZ_GITHUB_URL=https://example.invalid sh "$ROOT/install.sh" 1.2.3 >/dev/null 2>&1; then
    echo "corrupt CLI archive was installed" >&2
    exit 1
fi
[ "$("$TMP/install/ployz")" = installed ]

# A server that can't connect to the releases says why. An IPv6-only server can't reach
# github.com, which has no IPv6 address; `ip` reports the routes the server has. Like busybox's
# `ip`, the fake lists every route whatever selector it's given.
cat > "$TMP/bin/ip" <<'EOF'
#!/bin/sh
case "$1" in
    -4)
        echo '172.17.0.0/16 dev docker0 proto kernel scope link src 172.17.0.1'
        [ -z "${FAKE_IPV4_ROUTE:-}" ] || echo 'default via 192.0.2.1 dev eth0'
        ;;
    -6)
        echo 'fe80::/64 dev eth0 proto kernel metric 256'
        echo 'unreachable default dev lo table 8316 metric 4278198272'
        echo 'default via fe80::1 dev eth0 proto ra'
        ;;
esac
EOF
chmod 0755 "$TMP/bin/ip"
for routes in ipv6-only dual-stack; do
    if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" FAKE_UNREACHABLE=1 \
        FAKE_IPV4_ROUTE=$([ "$routes" = ipv6-only ] || echo 1) INSTALL_BIN_DIR="$TMP/install" \
        PLOYZ_GITHUB_URL=https://github.com/getployz/ployz sh "$ROOT/install.sh" 1.2.3 > "$TMP/error" 2>&1; then
        echo "installed with no route to the releases ($routes)" >&2
        exit 1
    fi
    case $routes in
        ipv6-only) expected="ERROR: Can't reach github.com, which serves Ployz releases: this server has no IPv4 route, and github.com has no IPv6 address. Give the server outbound IPv4, or NAT64 and DNS64: https://ployz.dev/docs/servers/add-a-server#ipv6-only-servers" ;;
        dual-stack) expected="ERROR: Can't reach github.com, which serves Ployz releases. Check this server's outbound internet access." ;;
    esac
    grep -Fqx "$expected" "$TMP/error" || { cat "$TMP/error" >&2; exit 1; }
done

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
tar -czf "$TMP/release/$linux_amd64" -C "$TMP" ployz
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


# Fleet Tokens are gone: an old line that carries one installs nothing and says what to run instead.
pack_release 1.2.3
GONE="Ployz Cloud no longer adds Servers from a Fleet Token. Run \`ployz server add --command\`"
TOKEN=pflt_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ-_01234
run_install() {
    rm -f "$TMP/install/ployz"
    # shellcheck disable=SC2086
    env PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
        PLOYZ_GITHUB_URL=https://example.invalid "$@" sh "$ROOT/install.sh" ${INSTALL_ARGS:-1.2.3} \
        > "$TMP/out" 2> "$TMP/err"
}
installed_only() {
    printf '%s\n' "$linux_amd64: OK" "Installed ployz to $TMP/install/ployz" | diff - "$TMP/out"
    [ ! -s "$TMP/err" ]
}
refused() {
    grep -Fq "$GONE" "$TMP/err"
    if grep -Fq "$TOKEN" "$TMP/err"; then echo "printed the token" >&2; exit 1; fi
    [ ! -e "$TMP/install/ployz" ]
}

for value in unset ''; do
    if [ "$value" = unset ]; then run_install; else run_install PLOYZ_FLEET_TOKEN="$value"; fi
    installed_only
done
# PLOYZ_ENROLL_TOKEN is for the ployz server add that a pasted line runs next, so it installs only the CLI.
run_install PLOYZ_ENROLL_TOKEN=pmet_abc123
installed_only
if run_install PLOYZ_ENROLL_TOKEN=$TOKEN; then
    echo "a Fleet Token in PLOYZ_ENROLL_TOKEN was not refused" >&2
    exit 1
fi
grep -Fq 'PLOYZ_ENROLL_TOKEN holds a Fleet Token.' "$TMP/err"
refused
if run_install PLOYZ_FLEET_TOKEN=$TOKEN; then
    echo "a Fleet Token was not refused" >&2
    exit 1
fi
refused
for args in '--enroll 1.2.3' '1.2.3 --enroll' '1.2.3 --enroll --key pk1_line'; do
    if INSTALL_ARGS=$args run_install; then
        echo "--enroll was not refused" >&2
        exit 1
    fi
    refused
done

echo "CLI installer verifies the signature, checksum and binary before installing, and refuses a Fleet Token"
