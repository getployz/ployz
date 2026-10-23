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
# Like curl when nothing answers on any address it resolved: no HTTP status, exit 7.
[ -z "${FAKE_UNREACHABLE:-}" ] || { printf 000; exit 7; }
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

for platform in Linux:x86_64 Linux:aarch64 Darwin:x86_64 Darwin:arm64; do
    FAKE_OS=${platform%:*}
    FAKE_ARCH=${platform#*:}
    PATH="$TMP/bin:$PATH" FAKE_OS="$FAKE_OS" FAKE_ARCH="$FAKE_ARCH" FAKE_RELEASE="$TMP/release" \
        INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
        sh "$ROOT/install.sh"
    [ "$("$TMP/install/ployz")" = installed ]
    [ "$("$TMP/install/ployz" --version)" = 1.2.3 ]
done

# By default the release comes from ployz.sh, which serves GitHub's assets over IPv6 too.
env -u PLOYZ_GITHUB_URL PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" \
    FAKE_CURL_LOG="$TMP/default-urls" INSTALL_BIN_DIR="$TMP/install" sh "$ROOT/install.sh" 1.2.3 > /dev/null
for asset in checksums.txt checksums.txt.minisig ployz_linux_amd64.tar.gz; do
    grep -Fq "https://ployz.sh/releases/download/v1.2.3/$asset" "$TMP/default-urls" ||
        { cat "$TMP/default-urls" >&2; exit 1; }
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

# A version with no release says so, and a missing install directory is named before any download.
if PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/nonexistent" \
    INSTALL_BIN_DIR="$TMP/install" PLOYZ_GITHUB_URL=https://example.invalid \
    sh "$ROOT/install.sh" 0.3.0-beta.99 > "$TMP/error" 2>&1; then
    echo "installed a release that doesn't exist" >&2
    exit 1
fi
grep -Fqx "ERROR: There's no Ployz release 0.3.0-beta.99. Releases: https://github.com/getployz/ployz/releases" "$TMP/error"
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
cp "$TMP/release/ployz_linux_amd64.tar.gz" "$TMP/release/checksums.txt" "$TMP/release/checksums.txt.minisig" "$TMP/mirror/releases/download/v1.2.3/"
PATH="$TMP/real-curl-bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 INSTALL_BIN_DIR="$TMP/mirror-install" \
    PLOYZ_GITHUB_URL="file://$TMP/mirror" sh "$ROOT/install.sh" 1.2.3 > /dev/null
[ "$("$TMP/mirror-install/ployz" --version)" = 1.2.3 ]

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


# The checkin stub records what it was given, then answers as FAKE_ANSWER says Cloud did.
# Like ployz, it writes the token file only once Cloud answers the token.
pack_release 1.2.3
cat > "$TMP/ployz" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then echo 1.2.3; exit 0; fi
token_file=$5
token=
case " $* " in *" --token-stdin "*) token=$(cat) ;; esac
{
    echo "argv=$*"
    echo "fleet=${PLOYZ_FLEET_TOKEN:-}"
    echo "skip_zfs=${PLOYZ_SKIP_ZFS:-}"
    echo "leaked=$(env | sed -n 's/^\(FLEET_TOKEN\|TTY\)=.*/\1 /p' | tr -d '\n')$(env | grep -c pflt_)"
    echo "token=$token"
} > "$CHECKIN_LOG"
case "${FAKE_ANSWER:-wait}" in
    wait)
        mkdir -p "${token_file%/*}"
        (umask 077 && printf '%s\n' "$token" > "$token_file")
        echo "Waiting for approval"
        ;;
    expired) echo "error: Token expired. Run the install again with a new token." >&2; exit 1 ;;
    refused) echo "error: Cloud refused this Fleet Token." >&2; exit 1 ;;
esac
EOF
tar -czf "$TMP/release/ployz_linux_amd64.tar.gz" -C "$TMP" ployz
(cd "$TMP/release" && sha256sum ./*.tar.gz | sed 's|  \./|  |' > checksums.txt)
sign_release 1.2.3
cat > "$TMP/bin/id" <<'EOF'
#!/bin/sh
echo "${FAKE_UID:-0}"
EOF
cat > "$TMP/bin/stty" <<'EOF'
#!/bin/sh
echo "$*" >> "$STTY_LOG"
[ "$*" != -g ] || echo saved-mode
[ "$*" != -echo ] || [ -z "${FAKE_NO_ECHO_OFF:-}" ]
EOF
printf '#!/bin/sh\n' > "$TMP/bin/systemctl"
chmod 0755 "$TMP/bin/id" "$TMP/bin/stty" "$TMP/bin/systemctl"

TOKEN=pflt_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ-_01234
OLD_TOKEN=pflt_ZYXWVUTSRQPONMLKJIHGFEDCBAzyxwvutsrqponmlkj
FLEET_TOKEN_FILE=$TMP/etc/ployz/fleet-token
export PLOYZ_TEST_ROOT=$TMP CHECKIN_LOG=$TMP/checkin.log STTY_LOG=$TMP/stty.log
enroll() {
    [ -n "${KEEP_ETC:-}" ] || rm -rf "${TMP:?}/etc"
    rm -f "$CHECKIN_LOG" "$STTY_LOG" "$TMP/install/ployz"
    # shellcheck disable=SC2086
    env PATH="$TMP/bin:$PATH" FAKE_OS=Linux FAKE_ARCH=x86_64 FAKE_RELEASE="$TMP/release" INSTALL_BIN_DIR="$TMP/install" \
        PLOYZ_GITHUB_URL=https://example.invalid "$@" sh "$ROOT/install.sh" ${INSTALL_ARGS:-${ENROLL_FLAG:-} 1.2.3} \
        > "$TMP/out" 2> "$TMP/err"
}
installed_only() {
    printf '%s\n' 'ployz_linux_amd64.tar.gz: OK' "Installed ployz to $TMP/install/ployz" | diff - "$TMP/out"
    [ ! -e "$FLEET_TOKEN_FILE" ]
    [ ! -e "$CHECKIN_LOG" ]
}
checked_in_with() {
    printf '%s\n' "argv=server checkin --install --token-file $FLEET_TOKEN_FILE --token-stdin" fleet= "skip_zfs=$1" \
        leaked=0 "token=$TOKEN" | diff - "$CHECKIN_LOG"
}

for value in unset ''; do
    if [ "$value" = unset ]; then enroll; else enroll PLOYZ_FLEET_TOKEN="$value"; fi
    installed_only
    [ ! -s "$TMP/err" ]
done
# PLOYZ_ENROLL_TOKEN is for the ployz server add that a pasted line runs next, so it installs only the CLI.
enroll PLOYZ_ENROLL_TOKEN=pmet_abc123
installed_only
[ ! -s "$TMP/err" ]
if enroll PLOYZ_ENROLL_TOKEN=$TOKEN; then
    echo "a Fleet Token in PLOYZ_ENROLL_TOKEN was not refused" >&2
    exit 1
fi
grep -Fq 'PLOYZ_ENROLL_TOKEN holds a Fleet Token. Set PLOYZ_FLEET_TOKEN instead' "$TMP/err"
[ ! -e "$TMP/install/ployz" ]

enroll PLOYZ_FLEET_TOKEN=$TOKEN PLOYZ_SKIP_ZFS=1
[ "$(head -n 1 "$TMP/out")" = "Installs Docker and Ployz." ]
[ "$(tail -n 1 "$TMP/out")" = "Waiting for approval" ]
[ "$(cat "$FLEET_TOKEN_FILE")" = $TOKEN ]
checked_in_with 1
[ ! -e "$STTY_LOG" ]

# A FLEET_TOKEN or TTY the caller exported must not carry the token into ployz's environment.
enroll PLOYZ_FLEET_TOKEN=$TOKEN FLEET_TOKEN=exported TTY=exported
checked_in_with ''

mkdir -p "$TMP/dev"
for args in '--enroll 1.2.3' '1.2.3 --enroll'; do
    printf '%s\n' "$TOKEN" > "$TMP/dev/tty"
    INSTALL_ARGS=$args enroll
    [ "$(tail -n 1 "$TMP/out")" = "Waiting for approval" ]
    checked_in_with ''
    printf '%s\n' -g -echo saved-mode | diff - "$STTY_LOG"
    grep -Fq 'Fleet Token (from Servers in Ployz Cloud): ' "$TMP/dev/tty"
done

# With echo still on, the prompt never shows, so nothing is typed into an echoing terminal.
printf '%s\n' "$TOKEN" > "$TMP/dev/tty"
if ENROLL_FLAG=--enroll enroll FAKE_NO_ECHO_OFF=1; then
    echo "read a token with echo on" >&2
    exit 1
fi
grep -Fq 'Could not turn off echo' "$TMP/err"
if grep -Fq 'Fleet Token (from Servers in Ployz Cloud): ' "$TMP/dev/tty"; then echo "prompted with echo on" >&2; exit 1; fi
[ ! -e "$CHECKIN_LOG" ]

if enroll PLOYZ_FLEET_TOKEN=$TOKEN FAKE_UID=1000; then
    echo "enrolled without root" >&2
    exit 1
fi
grep -Fq 'Run it as root: curl -fsSL https://ployz.sh | sudo sh -s -- 1.2.3 --enroll' "$TMP/err"
[ ! -e "$TMP/install/ployz" ]
[ ! -e "$FLEET_TOKEN_FILE" ]

for case in "${TOKEN%?}:this one is 47 characters" "${TOKEN}0:this one is 49 characters" \
    "${TOKEN%?}!:this one has other characters" "PFLT_${TOKEN#pflt_}:this one has other characters" \
    "cet_abc123:this one is 10 characters"; do
    value=${case%%:*}
    for flag in '' --enroll; do
        printf '%s\n' "$value" > "$TMP/dev/tty"
        if [ -n "$flag" ]; then
            if ENROLL_FLAG=$flag enroll; then echo "accepted $value from the terminal" >&2; exit 1; fi
        elif enroll PLOYZ_FLEET_TOKEN="$value"; then
            echo "accepted $value" >&2
            exit 1
        fi
        grep -Fqx "ERROR: A Fleet Token is pflt_ and 43 more letters, digits, - or _; ${case#*:}. Copy it again." "$TMP/err"
        if grep -Fq "$value" "$TMP/err"; then echo "printed $value" >&2; exit 1; fi
        [ ! -e "$TMP/install/ployz" ]
        [ ! -e "$FLEET_TOKEN_FILE" ]
    done
done

# A refused token changes nothing: no token file on a first install, the old token on a rerun.
for answer in expired refused; do
    if enroll PLOYZ_FLEET_TOKEN=$TOKEN FAKE_ANSWER=$answer; then
        echo "$answer answer exited 0" >&2
        exit 1
    fi
    [ "$(wc -l < "$TMP/err")" = 1 ] || { echo "$answer: the script added to ployz's error" >&2; exit 1; }
    [ ! -e "$FLEET_TOKEN_FILE" ]
    mkdir -p "${FLEET_TOKEN_FILE%/*}"
    printf '%s\n' "$OLD_TOKEN" > "$FLEET_TOKEN_FILE"
    if KEEP_ETC=1 enroll PLOYZ_FLEET_TOKEN=$TOKEN FAKE_ANSWER=$answer; then
        echo "$answer rerun exited 0" >&2
        exit 1
    fi
    [ "$(cat "$FLEET_TOKEN_FILE")" = "$OLD_TOKEN" ]
done

python3 "$ROOT/scripts/test-cli-installer-tty.py" "$ROOT/install.sh"

echo "CLI installer verifies the signature, checksum and binary before installing, and checks in with a Fleet Token"
