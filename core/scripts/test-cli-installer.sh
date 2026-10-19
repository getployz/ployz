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
    echo "enroll=${PLOYZ_ENROLL_TOKEN:-}"
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
    printf '%s\n' "argv=server checkin --install --token-file $FLEET_TOKEN_FILE --token-stdin" enroll= "skip_zfs=$1" \
        leaked=0 "token=$TOKEN" | diff - "$CHECKIN_LOG"
}

for value in unset ''; do
    if [ "$value" = unset ]; then enroll; else enroll PLOYZ_ENROLL_TOKEN="$value"; fi
    installed_only
    [ ! -s "$TMP/err" ]
done
for value in cet_abc123 PFLT_abc123; do
    enroll PLOYZ_ENROLL_TOKEN="$value"
    installed_only
    [ "$(cat "$TMP/err")" = "PLOYZ_ENROLL_TOKEN doesn't start with pflt_, so this installs only the CLI." ]
done

enroll PLOYZ_ENROLL_TOKEN=$TOKEN PLOYZ_SKIP_ZFS=1
[ "$(head -n 1 "$TMP/out")" = "Installs Docker and Ployz." ]
[ "$(tail -n 1 "$TMP/out")" = "Waiting for approval" ]
[ "$(cat "$FLEET_TOKEN_FILE")" = $TOKEN ]
checked_in_with 1
[ ! -e "$STTY_LOG" ]

# A FLEET_TOKEN or TTY the caller exported must not carry the token into ployz's environment.
enroll PLOYZ_ENROLL_TOKEN=$TOKEN FLEET_TOKEN=exported TTY=exported
checked_in_with ''

mkdir -p "$TMP/dev"
for args in '--enroll 1.2.3' '1.2.3 --enroll'; do
    printf '%s\n' "$TOKEN" > "$TMP/dev/tty"
    INSTALL_ARGS=$args enroll
    [ "$(tail -n 1 "$TMP/out")" = "Waiting for approval" ]
    checked_in_with ''
    printf '%s\n' -g -echo saved-mode | diff - "$STTY_LOG"
    grep -Fq 'Fleet Token: ' "$TMP/dev/tty"
done

# With echo still on, the prompt never shows, so nothing is typed into an echoing terminal.
printf '%s\n' "$TOKEN" > "$TMP/dev/tty"
if ENROLL_FLAG=--enroll enroll FAKE_NO_ECHO_OFF=1; then
    echo "read a token with echo on" >&2
    exit 1
fi
grep -Fq 'Could not turn off echo' "$TMP/err"
if grep -Fq 'Fleet Token: ' "$TMP/dev/tty"; then echo "prompted with echo on" >&2; exit 1; fi
[ ! -e "$CHECKIN_LOG" ]

if enroll PLOYZ_ENROLL_TOKEN=$TOKEN FAKE_UID=1000; then
    echo "enrolled without root" >&2
    exit 1
fi
grep -Fq 'Run it as root: curl -fsSL https://ployz.sh | sudo sh -s -- 1.2.3 --enroll' "$TMP/err"
[ ! -e "$TMP/install/ployz" ]
[ ! -e "$FLEET_TOKEN_FILE" ]

for value in "${TOKEN%?}" "${TOKEN}0" "${TOKEN%?}!"; do
    for flag in '' --enroll; do
        printf '%s\n' "$value" > "$TMP/dev/tty"
        if [ -n "$flag" ]; then
            if ENROLL_FLAG=$flag enroll; then echo "accepted $value from the terminal" >&2; exit 1; fi
        elif enroll PLOYZ_ENROLL_TOKEN="$value"; then
            echo "accepted $value" >&2
            exit 1
        fi
        grep -Fq "That isn't a Fleet Token" "$TMP/err"
        [ ! -e "$TMP/install/ployz" ]
        [ ! -e "$FLEET_TOKEN_FILE" ]
    done
done

# A refused token changes nothing: no token file on a first install, the old token on a rerun.
for answer in expired refused; do
    if enroll PLOYZ_ENROLL_TOKEN=$TOKEN FAKE_ANSWER=$answer; then
        echo "$answer answer exited 0" >&2
        exit 1
    fi
    [ "$(wc -l < "$TMP/err")" = 1 ] || { echo "$answer: the script added to ployz's error" >&2; exit 1; }
    [ ! -e "$FLEET_TOKEN_FILE" ]
    mkdir -p "${FLEET_TOKEN_FILE%/*}"
    printf '%s\n' "$OLD_TOKEN" > "$FLEET_TOKEN_FILE"
    if KEEP_ETC=1 enroll PLOYZ_ENROLL_TOKEN=$TOKEN FAKE_ANSWER=$answer; then
        echo "$answer rerun exited 0" >&2
        exit 1
    fi
    [ "$(cat "$FLEET_TOKEN_FILE")" = "$OLD_TOKEN" ]
done

python3 "$ROOT/scripts/test-cli-installer-tty.py" "$ROOT/install.sh"

echo "CLI installer verifies the checksum and binary before installing, and checks in with a Fleet Token"
