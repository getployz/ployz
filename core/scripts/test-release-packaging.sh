#!/usr/bin/env bash
set -euo pipefail
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-archives.sh
source "$ROOT/scripts/release-archives.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/dist" "$TMP/bin"
for binary in ployz ployzd ployz-uninstall; do
    printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/$binary"
    chmod 0755 "$TMP/bin/$binary"
done
for archive in $(release_archives); do
    # shellcheck disable=SC2046
    tar -czf "$TMP/dist/$archive" -C "$TMP/bin" $(release_archive_binaries "$archive")
done
bash "$ROOT/scripts/pack-release.sh" "$TMP/dist"
DIST="$TMP/dist" bash "$ROOT/scripts/verify-release.sh" artifacts
# Archive members are exact: the CLI ships alone, the daemon ships with its uninstaller.
for archive in "$(release_archive ployz linux amd64):ployz,ployz-uninstall" "$(release_archive ployzd linux amd64):ployzd"; do
    members=${archive#*:}
    archive=${archive%%:*}
    cp "$TMP/dist/$archive" "$TMP/original.tar.gz"
    tar -czf "$TMP/dist/$archive" -C "$TMP/bin" ${members//,/ }
    if DIST="$TMP/dist" bash "$ROOT/scripts/verify-release.sh" artifacts > "$TMP/error" 2>&1; then
        echo "accepted $archive with members $members" >&2
        exit 1
    fi
    grep -Fq 'members are' "$TMP/error"
    mv "$TMP/original.tar.gz" "$TMP/dist/$archive"
done
# Publication must accept exactly these six archives with signed checksums and stop before gh on extras.
cat > "$TMP/bin/gh" <<'SH'
#!/bin/sh
printf '%s\n' "$*" >> "$GH_CALLS"
SH
chmod 0755 "$TMP/bin/gh"
export GH_CALLS="$TMP/gh-calls"
if PATH="$TMP/bin:$PATH" DIST="$TMP/dist" bash "$ROOT/scripts/publish-github-release.sh" v1.2.3 > "$TMP/error" 2>&1; then
    echo 'published without a checksums signature' >&2
    exit 1
fi
grep -Fq 'release asset set differs' "$TMP/error"
[ ! -e "$GH_CALLS" ]
touch "$TMP/dist/checksums.txt.minisig"
PATH="$TMP/bin:$PATH" DIST="$TMP/dist" bash "$ROOT/scripts/publish-github-release.sh" v1.2.3
grep -Fq 'checksums.txt.minisig' "$GH_CALLS"
grep -Fq 'release create v1.2.3 --draft' "$GH_CALLS"
rm "$GH_CALLS"
# Every script reads the manifest: a row whose archive the release lacks is refused by each of them.
cp "$RELEASE_ARCHIVES_FILE" "$TMP/release-archives.txt"
echo 'ployz  freebsd  amd64  ployz_freebsd_amd64.tar.gz  ployz' >> "$TMP/release-archives.txt"
if RELEASE_ARCHIVES_FILE="$TMP/release-archives.txt" bash "$ROOT/scripts/pack-release.sh" "$TMP/dist" > "$TMP/error" 2>&1; then
    echo 'packed a release that lacks a manifest archive' >&2
    exit 1
fi
grep -Fq 'missing ployz_freebsd_amd64.tar.gz' "$TMP/error"
if RELEASE_ARCHIVES_FILE="$TMP/release-archives.txt" DIST="$TMP/dist" bash "$ROOT/scripts/verify-release.sh" artifacts > "$TMP/error" 2>&1; then
    echo 'verified a release that lacks a manifest archive' >&2
    exit 1
fi
grep -Fq 'archive set differs' "$TMP/error"
if RELEASE_ARCHIVES_FILE="$TMP/release-archives.txt" PATH="$TMP/bin:$PATH" DIST="$TMP/dist" bash "$ROOT/scripts/publish-github-release.sh" v1.2.3 > "$TMP/error" 2>&1; then
    echo 'published a release that lacks a manifest archive' >&2
    exit 1
fi
grep -Fq 'release asset set differs' "$TMP/error"
[ ! -e "$GH_CALLS" ]
# The manifest is found from any working directory, as the release CI runs the scripts from core/.
for directory in / "$ROOT" "$ROOT/scripts"; do
    [ "$(cd "$directory" && bash -c 'source "$1"; release_archives | wc -l' _ "$ROOT/scripts/release-archives.sh")" -eq 6 ]
done
touch "$TMP/dist/retired_linux_amd64.tar.gz"
if DIST="$TMP/dist" bash "$ROOT/scripts/verify-release.sh" artifacts > "$TMP/error" 2>&1; then
    echo 'accepted an extra release archive' >&2
    exit 1
fi
grep -Fq 'archive set differs' "$TMP/error"
if PATH="$TMP/bin:$PATH" DIST="$TMP/dist" bash "$ROOT/scripts/publish-github-release.sh" v1.2.3 > "$TMP/error" 2>&1; then
    echo 'published an extra release archive' >&2
    exit 1
fi
grep -Fq 'release asset set differs' "$TMP/error"
[ ! -e "$GH_CALLS" ]
# Channel pointers only move forward: beta follows a newer stable, an older-line fix moves only
# its own line, and the ployz.sh site serves both the line and unscoped pointers.
(
    # shellcheck source=scripts/promote-release.sh
    source "$ROOT/scripts/promote-release.sh"
    channels="$TMP/channels"
    publish() {
        write_channel_files "$channels" "$1"
    }
    expect() {
        local dir=$1
        shift
        while [ "$#" -gt 0 ]; do
            if [ "$(cat "$dir/$1" 2>/dev/null)" != "$2" ]; then
                echo "pointer $1 is '$(cat "$dir/$1" 2>/dev/null)', expected '$2'" >&2
                exit 1
            fi
            shift 2
        done
    }
    publish v0.2.0-beta.1
    expect "$channels" v0/beta v0.2.0-beta.1 beta v0.2.0-beta.1 v0/stable '' stable ''
    publish v0.2.0
    expect "$channels" v0/stable v0.2.0 v0/beta v0.2.0 stable v0.2.0 beta v0.2.0
    publish v0.2.1-beta.2
    publish v0.2.1-beta.10
    publish v0.2.0
    expect "$channels" v0/stable v0.2.0 v0/beta v0.2.1-beta.10 beta v0.2.1-beta.10
    publish v1.0.0
    publish v0.2.10
    expect "$channels" v0/stable v0.2.10 v0/beta v0.2.10 v1/stable v1.0.0 stable v1.0.0 beta v1.0.0
    PLOYZ_SH_SITE_DIR="$TMP/site" PLOYZ_SH_CHANNELS_DIR="$channels" \
        bash "$ROOT/scripts/stage-ployz-sh-site.sh" > /dev/null
    expect "$TMP/site" v0/stable v0.2.10 v1/beta v1.0.0 stable v1.0.0
    # A separate shell keeps errexit live, as in the Publish workflow.
    rejects() {
        if bash -c 'source "$1"; write_channel_files "$2" "$3"' _ "$ROOT/scripts/promote-release.sh" \
            "$channels" "$1" 2> /dev/null; then
            echo "promotion accepted $1" >&2
            exit 1
        fi
    }
    rejects v1.0.1-rc.1
    rejects v1.0.01
    rejects v18446744073709551616.0.0
    printf 'v1.0.\n0\n' > "$channels/stable"
    rejects v1.0.0
    printf 'v1.1.0-beta.2\n' > "$channels/stable"
    rejects v1.0.0
    printf 'v1.0.0\n' > "$channels/stable"
    expect "$channels" stable v1.0.0 beta v1.0.0
    printf 'v2.0.0\n' > "$channels/v1/stable"
    rejects v1.0.1
    printf 'v1.0.1-beta.01\n' > "$channels/v1/beta"
    rejects v1.0.1-beta.2
    printf 'v1.0.0\n' > "$channels/v1/beta"
    printf 'garbage\n' > "$channels/beta"
    rejects v1.0.1-beta.1
)
echo 'release packaging contracts passed'
