#!/bin/sh

set -eu

PLOYZ_GITHUB_URL=${PLOYZ_GITHUB_URL:-https://github.com/getployz/ployz}
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
FLEET_TOKEN_FILE=${PLOYZ_TEST_ROOT:-}/etc/ployz/fleet-token
# Unset first, so a name exported by the caller doesn't carry the token into ployz's environment.
unset FLEET_TOKEN TTY
TTY=${PLOYZ_TEST_ROOT:-}/dev/tty
FLEET_TOKEN=${PLOYZ_ENROLL_TOKEN:-}
unset PLOYZ_ENROLL_TOKEN
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

install_cli() {
    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT
    trap 'exit 130' HUP INT TERM
    version=$(resolve_install "$PLOYZ_VERSION")
    matches_version "$version" "$RELEASE_VERSION" || error "Invalid version: $PLOYZ_VERSION"

    archive=$(cli_archive "$(uname -s)" "$(uname -m)") || \
        error "Unsupported platform: $(uname -s) $(uname -m)"
    base_url="$PLOYZ_GITHUB_URL/releases/download/v$version"

    curl -fsSL -o "$tmp_dir/$archive" "$base_url/$archive" || error "Failed to download $archive"
    curl -fsSL -o "$tmp_dir/checksums.txt" "$base_url/checksums.txt" || error "Failed to download checksums.txt"
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

# The same shape ployz checks, spelled out so no locale widens the range.
is_fleet_token() {
    case "$1" in pflt_*) ;; *) return 1 ;; esac
    set -- "${1#pflt_}"
    [ "${#1}" -eq 43 ] || return 1
    case "$1" in
        *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-]*) return 1 ;;
    esac
}

# The script itself arrives on stdin, so the token is read from the terminal, never echoed.
read_fleet_token() {
    tty_mode=$(stty -g 2>/dev/null < "$TTY") ||
        error "No terminal to read the Fleet Token from. Set PLOYZ_ENROLL_TOKEN in the root environment instead, as cloud-init does."
    trap 'stty "$tty_mode" < "$TTY"; exit 130' HUP INT TERM
    stty -echo < "$TTY" || error "Could not turn off echo on the terminal, so the Fleet Token isn't read from it."
    # The prompt waits until echo is off: a token pasted the moment it shows must not echo.
    printf 'Fleet Token (Servers > Add servers in Ployz Cloud): ' >> "$TTY"
    read -r FLEET_TOKEN < "$TTY" || FLEET_TOKEN=
    stty "$tty_mode" < "$TTY"
    trap - HUP INT TERM
    printf '\n' >> "$TTY"
}

check_in() {
    echo "Installs Docker and Ployz."
    [ "$(uname -s)" = Linux ] || error "A Fleet Token needs a Linux server."
    [ "$(id -u)" = 0 ] || error "Run it as root: curl -fsSL https://ployz.sh | sudo sh -s -- ${VERSION_ARG:+$VERSION_ARG }--enroll"
    command -v systemctl >/dev/null 2>&1 || error "A Fleet Token needs a server that runs systemd."
    [ -n "$FLEET_TOKEN" ] || read_fleet_token
    is_fleet_token "$FLEET_TOKEN" ||
        error "That isn't a Fleet Token. It is pflt_ and 43 more characters. Copy it again."
    install_cli
    # printf is a builtin, so the token reaches ployz on stdin without entering any process's argv.
    printf '%s\n' "$FLEET_TOKEN" |
        "$INSTALL_BIN_DIR/ployz" server checkin --install --token-file "$FLEET_TOKEN_FILE" --token-stdin
}

case "$ENROLL:$FLEET_TOKEN" in
    1:* | :pflt_*) check_in ;;
    :) install_cli ;;
    *)
        echo "PLOYZ_ENROLL_TOKEN doesn't start with pflt_, so this installs only the CLI." >&2
        install_cli
        ;;
esac
