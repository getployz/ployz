#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST=${DIST:-"$ROOT/dist"}
EXPECTED_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)

fail() { echo "release verification failed: $1" >&2; exit 1; }

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

cli_archives() {
    printf '%s\n' \
        ployz_linux_amd64.tar.gz \
        ployz_linux_arm64.tar.gz \
        ployz_macos_amd64.tar.gz \
        ployz_macos_arm64.tar.gz
}

require_archives() {
    expected=$(printf '%s\n' "$@" | sort)
    actual=$(find "$DIST" -maxdepth 1 -name '*.tar.gz' -exec basename {} \; | sort)
    [ "$actual" = "$expected" ] || fail "archive set differs from the approved names"
    for archive in "$@"; do
        case $archive in
            ployzd_*) check_archive "$archive" $'ployz-uninstall\nployzd' ;;
            *) check_archive "$archive" ployz ;;
        esac
    done
}

check_archive() {
    archive=$1
    expected=$2
    members=$(tar -tzf "$DIST/$archive" | LC_ALL=C sort)
    [ "$members" = "$expected" ] || fail "$archive members are '$members'"
    while IFS= read -r mode; do
        [ "$mode" = -rwxr-xr-x ] || fail "$archive contains mode $mode instead of 0755"
    done < <(tar -tvzf "$DIST/$archive" | awk '{print $1}')
}

check_checksums_and_formula() {
    checksum_names=$(awk '{print $2}' "$DIST/checksums.txt" | sort)
    expected_checksums=$(printf '%s\n' "$(cli_archives)" ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz | sort)
    [ "$checksum_names" = "$expected_checksums" ] || fail "checksums.txt must cover CLI and daemon archives"

    formula=$(find "$DIST" -name 'ployz.rb' -print -quit)
    [ -n "$formula" ] || fail "Homebrew formula was not generated"
    while IFS= read -r archive; do
        grep -Fq "${archive%.tar.gz}" "$formula" || fail "Homebrew formula omits $archive"
        checksum=$(sha256 "$DIST/$archive")
        grep -Fq "$checksum" "$formula" || fail "Homebrew formula has no checksum for $archive"
    done < <(cli_archives)
    grep -Fq 'bin.install "ployz"' "$formula" || fail "Homebrew formula does not install ployz"
}

run_archive() {
    archive=$1 binary=$2 runner=${3:-}
    directory=$(mktemp -d)
    tar -xzf "$DIST/$archive" -C "$directory"
    install -m 0755 "$directory/$binary" "$directory/installed"
    # The CLI prints its version with a flag; the daemon with a subcommand.
    flag=version
    [ "$binary" = ployz ] && flag=--version
    if [ -n "$runner" ]; then
        output=$($runner "$directory/installed" "$flag")
    else
        output=$("$directory/installed" "$flag")
    fi
    [ "$output" = "$EXPECTED_VERSION" ] || fail "$archive returned version '$output'"
    [ "$binary" = ployz ] && smoke_cli "$archive" "$directory/installed" "$runner"
    rm -rf "$directory"
}

# The released CLI's two Config Store paths: the hidden in-process SQLite Store,
# and Cloud over HTTPS with the binary's own TLS roots (a refused token proves the
# handshake; `unavailable` would mean TLS or DNS failed).
smoke_cli() {
    archive=$1 binary=$2 runner=$3
    home=$(mktemp -d)
    # $runner is empty or a command prefix such as `qemu-aarch64`.
    # shellcheck disable=SC2086
    ployz() { env HOME="$home" PLOYZ_CONFIG="$home/config.yaml" $runner "$binary" --json "$@"; }
    export PLOYZ_STORE="sqlite:$home/store.db"
    ployz project new smoke >/dev/null || fail "$archive cannot create a Project in the hidden SQLite Store"
    ployz service add web --image nginx:1 --project smoke >/dev/null || fail "$archive cannot add a Service"
    ployz get web.replicas --project smoke | grep -Fq '"value": 1' || fail "$archive cannot read a Setting back"
    unset PLOYZ_STORE
    set +e
    cloud=$(PLOYZ_TOKEN=ployz_release_smoke ployz project ls)
    set -e
    code=$(printf '%s' "$cloud" | sed -n 's/.*"code":"\([a-z_]*\)".*/\1/p')
    case "$code" in
        unauthenticated | unsupported) ;;
        *) fail "$archive did not reach Cloud over HTTPS: $cloud" ;;
    esac
    rm -rf "$home"
}

# install.sh, fetched alone, installs this archive for the host it runs on.
install_native() {
    archive=$1
    release=$(mktemp -d)
    mkdir -p "$release/releases/download/v$EXPECTED_VERSION" "$release/bin"
    cp "$DIST/$archive" "$release/releases/download/v$EXPECTED_VERSION/"
    (cd "$release/releases/download/v$EXPECTED_VERSION" && printf '%s  %s\n' "$(sha256 "$archive")" "$archive" > checksums.txt)
    PLOYZ_GITHUB_URL="file://$release" INSTALL_BIN_DIR="$release/bin" sh "$ROOT/install.sh" "$EXPECTED_VERSION" \
        || fail "install.sh could not install $archive"
    [ "$("$release/bin/ployz" --version)" = "$EXPECTED_VERSION" ] || fail "install.sh installed the wrong ployz"
    rm -rf "$release"
}

case "${1:-}" in
    macos)
        require_archives ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz
        run_archive ployz_macos_arm64.tar.gz ployz "arch -arm64"
        run_archive ployz_macos_amd64.tar.gz ployz "arch -x86_64"
        install_native "ployz_macos_$(uname -m | sed 's/x86_64/amd64/').tar.gz"
        ;;
    linux)
        require_archives ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz
        run_archive ployz_linux_amd64.tar.gz ployz
        run_archive ployzd_linux_amd64.tar.gz ployzd
        run_archive ployz_linux_arm64.tar.gz ployz qemu-aarch64
        run_archive ployzd_linux_arm64.tar.gz ployzd qemu-aarch64
        install_native ployz_linux_amd64.tar.gz
        for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
            probe="$ROOT/target/$target/release/sqlite-probe"
            chmod +x "$probe"
            if [ "$target" = aarch64-unknown-linux-musl ]; then
                qemu-aarch64 "$probe" "$(mktemp)"
            else
                "$probe" "$(mktemp)"
            fi
        done
        for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
            directory=$(mktemp -d)
            tar -xzf "$DIST/$archive" -C "$directory"
            binary=ployz
            case "$archive" in
                ployzd_*) binary=ployzd ;;
            esac
            file "$directory/$binary" | grep -Fq 'statically linked' || fail "$archive is dynamically linked"
            rm -rf "$directory"
        done
        ;;
    artifacts)
        require_archives ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz
        check_checksums_and_formula
        ;;
    *) fail "usage: $0 macos|linux|artifacts" ;;
esac

echo "release artifacts verified on $1"
