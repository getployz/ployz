#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/release-tag.sh
source "$ROOT/scripts/release-tag.sh"

minor_of() {
    local rest=${1#*.}
    printf '%s\n' "${rest%%.*}"
}

# advance_pointer <dest_dir> <tag> <channel> [line]: moves the channel's pointer (the line's
# pointer when a line is given) to the tag only when the tag is higher, so an older-line fix never
# moves a channel backwards. A corrupt pointer (not a tag, a prerelease on stable, or another
# line's tag) stops the release rather than being kept or overwritten. The v0 pointers stay on
# the minor they hold.
advance_pointer() {
    local dest_dir=$1 tag=$2 channel=$3 line=${4:-} current='' valid=release_tag
    local file=$dest_dir/${line:+$line/}$channel
    [ "$channel" = stable ] && valid=stable_release_tag
    if [ -f "$file" ]; then
        current=$(< "$file")
        if ! "$valid" "$current" || { [ -n "$line" ] && [ "${current%%.*}" != "$line" ]; }; then
            echo "channel pointer $file holds '$current', not a ${line:+$line }$channel tag" >&2
            return 1
        fi
        release_tag_higher "$tag" "$current" || return 0
        # Under 0.x a minor is a breaking line, but daemons and Cloud read the major's pointer
        # (v0/beta), so a 0.4 tag left 0.3 Servers upgrading into a release their Cloud can't drive.
        [ "$line" = v0 ] && [ "$(minor_of "$tag")" != "$(minor_of "$current")" ] && return 0
    fi
    mkdir -p "$(dirname "$file")"
    printf '%s\n' "$tag" > "$file"
}

# Daemons read their line's pointer (v0/stable); only install.sh reads the unscoped one.
# beta is the highest release, so a stable tag advances it too.
write_channel_files() {
    local dest_dir=$1 tag=$2 line pointer pointers=beta channel
    channel=$(channel_name_for_tag "$tag")
    line=${tag%%.*}
    [ "$channel" = beta ] || pointers="stable beta"
    for pointer in $pointers; do
        advance_pointer "$dest_dir" "$tag" "$pointer" "$line"
        advance_pointer "$dest_dir" "$tag" "$pointer"
    done
}

git_identity() {
    git -C "$1" config user.name "github-actions[bot]"
    git -C "$1" config user.email "41898282+github-actions[bot]@users.noreply.github.com"
}

commit_if_changed() {
    local work=$1 message=$2
    git -C "$work" add -A
    if git -C "$work" diff --cached --quiet; then
        return 0
    fi
    git -C "$work" commit -m "$message"
}

push_channel_files() {
    local work=$1 tag=$2
    local token=${GITHUB_TOKEN:-${GH_TOKEN:-}}
    local repo=${GITHUB_REPOSITORY:-getployz/ployz}
    local remote
    [ -n "$token" ] || {
        echo "GITHUB_TOKEN is required to update the channels branch" >&2
        return 1
    }
    remote="https://x-access-token:${token}@github.com/${repo}.git"
    if git ls-remote --heads "$remote" channels | grep -q .; then
        git clone --depth 1 --branch channels "$remote" "$work"
    else
        git -C "$work" init
        git -C "$work" checkout -b channels
        git -C "$work" remote add origin "$remote"
    fi
    git_identity "$work"
    write_channel_files "$work" "$tag"
    commit_if_changed "$work" "channels -> $tag"
    git -C "$work" push origin "HEAD:channels"
}

dispatch_ployz_sh_site() {
    # channels has no workflow file, so a push there cannot deploy Pages.
    gh workflow run ployz-sh.yml --ref main
}

promote_published_release() {
    local tag=$1 channels
    channels=$(mktemp -d)
    push_channel_files "$channels" "$tag"
    dispatch_ployz_sh_site
    rm -rf "$channels"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    tag=${1:-}
    [ -n "$tag" ] || {
        echo "usage: $0 <tag>" >&2
        exit 1
    }
    promote_published_release "$tag"
fi
