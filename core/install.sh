#!/bin/sh

set -eu

PLOYZ_GITHUB_URL=${PLOYZ_GITHUB_URL:-https://github.com/getployz/ployz}
# Passes GitHub's release assets through over IPv6, for a server that can't connect to github.com.
PLOYZ_FALLBACK_URL=${PLOYZ_FALLBACK_URL:-https://ployz.sh}
PLOYZ_CHANNEL_URL=${PLOYZ_CHANNEL_URL:-https://ployz.sh}
ENROLL=
VERSION_ARG=
for arg do
    case "$arg" in
        --enroll) ENROLL=1 ;;
        *) VERSION_ARG=${VERSION_ARG:-$arg} ;;
    esac
done
PLOYZ_VERSION=${PLOYZ_VERSION:-${VERSION_ARG:-stable}}
INSTALL_BIN_DIR=${INSTALL_BIN_DIR:-/usr/local/bin}
# crates/ployz-core/release.pub; checksums.txt is signed with its secret key.
PLOYZ_RELEASE_PUBLIC_KEY=${PLOYZ_RELEASE_PUBLIC_KEY:-RWTNGOGCZ4e3WGAYER3C3IPgx1GyuYNe1zMivQNzjvDmN23dZ6gYmyGt}
# Same grammar as scripts/release-tag.sh; this file is fetched alone, so it cannot source it.
RELEASE_NUMBER='(0|[1-9][0-9]{0,18})'
STABLE_VERSION="$RELEASE_NUMBER\.$RELEASE_NUMBER\.$RELEASE_NUMBER"
RELEASE_VERSION="$STABLE_VERSION(-beta\.$RELEASE_NUMBER)?"

error() {
    echo "ERROR: $1" >&2
    exit 1
}

cli_archive() {
    case "$1:$2" in
        Linux:x86_64) echo ployz_linux_amd64.tar.gz ;;
        Linux:aarch64) echo ployz_linux_arm64.tar.gz ;;
        Darwin:x86_64) echo ployz_macos_amd64.tar.gz ;;
        Darwin:arm64) echo ployz_macos_arm64.tar.gz ;;
        *) return 1 ;;
    esac
}

verify_checksum() {
    archive=$1
    checksums=$2
    directory=$3
    grep "  ${archive}$" "$checksums" > "$directory/archive.sha256" || return 1
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$directory" && sha256sum -c archive.sha256)
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$directory" && shasum -a 256 -c archive.sha256)
    else
        return 1
    fi
}

# A minisign signature in legacy mode is pure Ed25519, which OpenSSL 3 verifies directly:
# line 2 holds "Ed", the key id and the signature over checksums.txt; line 4 signs that
# signature followed by the trusted comment.
verify_signature_openssl() {
    checksums=$1
    signature=$2
    comment=$3
    directory=$4
    printf '%s' "$PLOYZ_RELEASE_PUBLIC_KEY" | openssl base64 -d -A > "$directory/key.bin" || return 1
    sed -n 2p "$signature" | tr -d '\r\n' | openssl base64 -d -A > "$directory/sig.bin" || return 1
    sed -n 4p "$signature" | tr -d '\r\n' | openssl base64 -d -A > "$directory/global.bin" || return 1
    [ "$(wc -c < "$directory/key.bin")" -eq 42 ] || return 1
    [ "$(wc -c < "$directory/sig.bin")" -eq 74 ] || return 1
    [ "$(wc -c < "$directory/global.bin")" -eq 64 ] || return 1
    [ "$(head -c 2 "$directory/key.bin")" = Ed ] || return 1
    [ "$(head -c 2 "$directory/sig.bin")" = Ed ] || return 1
    dd if="$directory/key.bin" of="$directory/key.id" bs=1 skip=2 count=8 2>/dev/null || return 1
    dd if="$directory/sig.bin" of="$directory/sig.id" bs=1 skip=2 count=8 2>/dev/null || return 1
    cmp -s "$directory/key.id" "$directory/sig.id" || return 1
    # Ed25519 SubjectPublicKeyInfo header, then the 32-byte public key.
    printf '\060\052\060\005\006\003\053\145\160\003\041\000' > "$directory/key.der"
    dd if="$directory/key.bin" bs=1 skip=10 2>/dev/null >> "$directory/key.der" || return 1
    dd if="$directory/sig.bin" of="$directory/sig.ed25519" bs=1 skip=10 2>/dev/null || return 1
    cp "$directory/sig.ed25519" "$directory/global.msg"
    printf '%s' "$comment" >> "$directory/global.msg"
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$directory/key.der" -rawin \
        -in "$checksums" -sigfile "$directory/sig.ed25519" >/dev/null 2>&1 || return 1
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$directory/key.der" -rawin \
        -in "$directory/global.msg" -sigfile "$directory/global.bin" >/dev/null 2>&1 || return 1
    [ "$(sed -n 3p "$signature")" = "trusted comment: $comment" ]
}

has_openssl3() {
    command -v openssl >/dev/null 2>&1 && openssl pkeyutl -help 2>&1 | grep -q -- -rawin
}

# Returns 2 when this host has no tool that can check the signature.
verify_signature() {
    checksums=$1
    signature=$2
    version=$3
    directory=$4
    comment="ployz v$version"
    if has_openssl3; then
        verify_signature_openssl "$checksums" "$signature" "$comment" "$directory"
    elif command -v minisign >/dev/null 2>&1; then
        trusted=$(minisign -V -Q -P "$PLOYZ_RELEASE_PUBLIC_KEY" -m "$checksums" -x "$signature" 2>/dev/null) || return 1
        [ "$trusted" = "$comment" ]
    else
        return 2
    fi
}

# grep matches per line, so a value holding any whitespace never matches.
matches_version() {
    case "$1" in *[![:graph:]]*) return 1 ;; esac
    echo "$1" | grep -Eq "^$2\$"
}

channel_version_from_file() {
    version=$(cat "$1")
    matches_version "$version" "v?$2" || return 1
    echo "$version"
}

resolve_install() {
    requested=${1#v}
    case "$requested" in
        stable) pattern=$STABLE_VERSION ;;
        beta) pattern=$RELEASE_VERSION ;;
        *)
            printf '%s\n' "$requested"
            return 0
            ;;
    esac
    dest=$(mktemp)
    if ! curl -fsSL -o "$dest" "$PLOYZ_CHANNEL_URL/$requested" || ! resolved=$(channel_version_from_file "$dest" "$pattern"); then
        rm -f "$dest"
        error "$requested channel is unavailable"
    fi
    rm -f "$dest"
    printf '%s\n' "${resolved#v}"
}

# busybox's `ip` ignores a `default` selector and lists every route, so match the line instead.
has_default_route() {
    ip "$1" route show 2>/dev/null | grep -q '^default'
}

# Why the release host didn't answer. github.com has no IPv6 address, so a server with only an
# IPv6 route can never reach it.
unreachable() {
    host=${PLOYZ_GITHUB_URL#*://}
    host=${host%%/*}
    if [ "$host" = github.com ] && command -v ip >/dev/null 2>&1 &&
        ! has_default_route -4 && has_default_route -6; then
        error "Can't reach $host, which serves Ployz releases: this server has no IPv4 route, and github.com has no IPv6 address. Give the server outbound IPv4, or NAT64 and DNS64: https://ployz.dev/docs/servers/add-a-server#ipv6-only-servers"
    fi
    error "Can't reach $host, which serves Ployz releases. Check this server's outbound internet access."
}

install_cli() {
    [ -d "$INSTALL_BIN_DIR" ] ||
        error "$INSTALL_BIN_DIR is not a directory. Create it first, or set INSTALL_BIN_DIR to a directory that exists."
    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT
    trap 'exit 130' HUP INT TERM
    version=$(resolve_install "$PLOYZ_VERSION")
    matches_version "$version" "$RELEASE_VERSION" || error "Invalid version: $PLOYZ_VERSION"

    archive=$(cli_archive "$(uname -s)" "$(uname -m)") || \
        error "Unsupported platform: $(uname -s) $(uname -m)"
    base_url="$PLOYZ_GITHUB_URL/releases/download/v$version"

    # Every release has checksums.txt, so a 404 for it means the release doesn't exist.
    # A file:// mirror, as verify-release.sh uses, answers 000 when curl succeeds.
    # curl exits 6 when the name doesn't resolve and 7 when nothing answers. Then the release comes
    # through the fallback, and only if that can't connect either does the install stop.
    status=$(curl -sSL -w '%{http_code}' -o "$tmp_dir/checksums.txt" "$base_url/checksums.txt") || {
        case $? in
            6|7)
                echo "Can't reach $PLOYZ_GITHUB_URL; downloading the release through $PLOYZ_FALLBACK_URL" >&2
                base_url="$PLOYZ_FALLBACK_URL/releases/download/v$version"
                status=$(curl -sSL -w '%{http_code}' -o "$tmp_dir/checksums.txt" "$base_url/checksums.txt") || {
                    case $? in 6|7) unreachable ;; esac
                    status=
                }
                ;;
            *) status= ;;
        esac
    }
    case "$status" in
        2??|000) ;;
        404) error "There's no Ployz release $version. Releases: $PLOYZ_GITHUB_URL/releases" ;;
        *) error "Failed to download checksums.txt${status:+ (HTTP $status)}" ;;
    esac
    curl -fsSL -o "$tmp_dir/$archive" "$base_url/$archive" || error "Failed to download $archive"
    curl -fsSL -o "$tmp_dir/checksums.txt.minisig" "$base_url/checksums.txt.minisig" || \
        error "Failed to download checksums.txt.minisig; ployz $version may predate signed releases"
    status=0
    verify_signature "$tmp_dir/checksums.txt" "$tmp_dir/checksums.txt.minisig" "$version" "$tmp_dir" || status=$?
    case $status in
        0) ;;
        2) error "Cannot verify the release signature: install OpenSSL 3 or minisign, then rerun" ;;
        *) error "Release signature verification failed: checksums.txt is not signed by the Ployz release key for v$version" ;;
    esac
    verify_checksum "$archive" "$tmp_dir/checksums.txt" "$tmp_dir" || error "Checksum verification failed"
    tar -xzf "$tmp_dir/$archive" -C "$tmp_dir" ployz || error "Release archive is incomplete"
    if [ ! -f "$tmp_dir/ployz" ] || [ -L "$tmp_dir/ployz" ] || [ ! -x "$tmp_dir/ployz" ]; then
        error "Release binary ployz is invalid"
    fi
    installed_version=$("$tmp_dir/ployz" --version) || error "Cannot run ployz"
    [ "$installed_version" = "$version" ] || error "Release binary ployz has version $installed_version, expected $version"

    if [ -w "$INSTALL_BIN_DIR" ]; then
        install -m 0755 "$tmp_dir/ployz" "$INSTALL_BIN_DIR/"
    else
        sudo install -m 0755 "$tmp_dir/ployz" "$INSTALL_BIN_DIR/"
    fi
    echo "Installed ployz to $INSTALL_BIN_DIR/ployz"
}

# An old install line passes --enroll or PLOYZ_FLEET_TOKEN. Refuse it before installing anything, so it doesn't
# look like it worked.
FLEET_TOKEN_GONE="Ployz Cloud no longer adds Servers from a Fleet Token. Run \`ployz server add --command\` where you're signed in, or choose Add server in Ployz Cloud, and run the line it prints on this server."
[ -z "$ENROLL${PLOYZ_FLEET_TOKEN:-}" ] || error "$FLEET_TOKEN_GONE"
case "${PLOYZ_ENROLL_TOKEN:-}" in
    pflt_*) error "PLOYZ_ENROLL_TOKEN holds a Fleet Token. $FLEET_TOKEN_GONE" ;;
esac
install_cli
