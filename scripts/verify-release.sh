#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST="$ROOT/dist"
EXPECTED_VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)

fail() { echo "release verification failed: $1" >&2; exit 1; }

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

expected_archives=$(printf '%s\n' \
    ployz_linux_amd64.tar.gz \
    ployz_linux_arm64.tar.gz \
    ployz_macos_amd64.tar.gz \
    ployz_macos_arm64.tar.gz \
    ployzd_linux_amd64.tar.gz \
    ployzd_linux_arm64.tar.gz | sort)
actual_archives=$(find "$DIST" -maxdepth 1 -name '*.tar.gz' -exec basename {} \; | sort)
[ "$actual_archives" = "$expected_archives" ] || fail "archive set differs from the six approved names"

check_archive() {
    archive=$1
    expected=$2
    members=$(tar -tzf "$DIST/$archive" | sort)
    [ "$members" = "$expected" ] || fail "$archive members are '$members'"
    while IFS= read -r mode; do
        [ "$mode" = -rwxr-xr-x ] || fail "$archive contains mode $mode instead of 0755"
    done < <(tar -tvzf "$DIST/$archive" | awk '{print $1}')
}

for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
    check_archive "$archive" ployz
done
for archive in ployzd_linux_amd64.tar.gz ployzd_linux_arm64.tar.gz; do
    check_archive "$archive" $'ployz-uninstall\nployzd'
done

checksum_names=$(awk '{print $2}' "$DIST/checksums.txt" | sort)
expected_checksums=$(printf '%s\n' \
    ployz_linux_amd64.tar.gz \
    ployz_linux_arm64.tar.gz \
    ployz_macos_amd64.tar.gz \
    ployz_macos_arm64.tar.gz | sort)
[ "$checksum_names" = "$expected_checksums" ] || fail "checksums.txt must cover only CLI archives"

formula=$(find "$DIST" -name 'ployz.rb' -print -quit)
[ -n "$formula" ] || fail "Homebrew formula was not generated"
for archive in ployz_linux_amd64.tar.gz ployz_linux_arm64.tar.gz ployz_macos_amd64.tar.gz ployz_macos_arm64.tar.gz; do
    grep -Fq "${archive%.tar.gz}" "$formula" || fail "Homebrew formula omits $archive"
    checksum=$(sha256 "$DIST/$archive")
    grep -Fq "$checksum" "$formula" || fail "Homebrew formula has no checksum for $archive"
done
grep -Fq 'bin.install "ployz"' "$formula" || fail "Homebrew formula does not install ployz"
grep -Fiq 'clean break' "$formula" || fail "Homebrew formula omits the clean-break statement"

run_archive() {
    archive=$1 binary=$2 runner=${3:-}
    directory=$(mktemp -d)
    tar -xzf "$DIST/$archive" -C "$directory"
    install -m 0755 "$directory/$binary" "$directory/installed"
    if [ -n "$runner" ]; then
        output=$($runner "$directory/installed" version)
    else
        output=$("$directory/installed" version)
    fi
    rm -rf "$directory"
    [ "$output" = "$EXPECTED_VERSION" ] || fail "$archive returned version '$output'"
}

case "${1:-}" in
    macos)
        run_archive ployz_macos_arm64.tar.gz ployz "arch -arm64"
        run_archive ployz_macos_amd64.tar.gz ployz "arch -x86_64"
        ;;
    linux)
        run_archive ployz_linux_amd64.tar.gz ployz
        run_archive ployzd_linux_amd64.tar.gz ployzd
        run_archive ployz_linux_arm64.tar.gz ployz qemu-aarch64
        run_archive ployzd_linux_arm64.tar.gz ployzd qemu-aarch64
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
            binary=ployz; case "$archive" in ployzd_*) binary=ployzd ;; esac
            file "$directory/$binary" | grep -Fq 'statically linked' || fail "$archive is dynamically linked"
            rm -rf "$directory"
        done
        ;;
    *) fail "usage: $0 macos|linux" ;;
esac

echo "six release artifacts verified on $1"
