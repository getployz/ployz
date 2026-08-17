#!/bin/sh

set -eu

PLOYZ_GITHUB_URL=${PLOYZ_GITHUB_URL:-https://github.com/getployz/ployz2}
PLOYZ_CHANNEL_URL=${PLOYZ_CHANNEL_URL:-https://ployz.sh}
PLOYZ_VERSION=${PLOYZ_VERSION:-${1:-latest}}
INSTALL_BIN_DIR=${INSTALL_BIN_DIR:-/usr/local/bin}

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

channel_version_from_file() {
    version=$(tr -d ' \t\r\n' < "$1")
    echo "$version" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$' || return 1
    echo "$version"
}

resolve_install() {
    requested=${1#v}
    name=
    case "$requested" in
        latest | stable | '') name=stable ;;
        beta) name=beta ;;
        *)
            printf '%s\n' "$requested"
            return 0
            ;;
    esac
    dest=$(mktemp)
    if ! curl -fsSL -o "$dest" "$PLOYZ_CHANNEL_URL/$name" || ! resolved=$(channel_version_from_file "$dest"); then
        rm -f "$dest"
        error "$name channel is unavailable"
    fi
    rm -f "$dest"
    printf '%s\n' "${resolved#v}"
}

install_cli() {
    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
    requested=${PLOYZ_VERSION#v}
    [ "$requested" != nightly ] || error "nightly is not a supported release channel"
    version=$(resolve_install "$requested")
    case "$version" in
        [0-9A-Za-z]*) ;;
        *) error "Invalid version: $PLOYZ_VERSION" ;;
    esac
    case "$version" in
        *[!0-9A-Za-z.-]*) error "Invalid version: $PLOYZ_VERSION" ;;
    esac

    archive=$(cli_archive "$(uname -s)" "$(uname -m)") || \
        error "Unsupported platform: $(uname -s) $(uname -m)"
    base_url="$PLOYZ_GITHUB_URL/releases/download/v$version"

    curl -fsSL -o "$tmp_dir/$archive" "$base_url/$archive" || error "Failed to download $archive"
    curl -fsSL -o "$tmp_dir/checksums.txt" "$base_url/checksums.txt" || error "Failed to download checksums.txt"
    verify_checksum "$archive" "$tmp_dir/checksums.txt" "$tmp_dir" || error "Checksum verification failed"
    tar -xzf "$tmp_dir/$archive" -C "$tmp_dir"

    if [ -w "$INSTALL_BIN_DIR" ]; then
        install -m 0755 "$tmp_dir/ployz" "$INSTALL_BIN_DIR/ployz"
    else
        sudo install -m 0755 "$tmp_dir/ployz" "$INSTALL_BIN_DIR/ployz"
    fi
    echo "Installed ployz to $INSTALL_BIN_DIR/ployz"
}

if [ "${PLOYZ_CLI_INSTALL_TEST_ONLY:-false}" != true ]; then
    install_cli
fi
