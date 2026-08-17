#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# shellcheck source=scripts/release-tag.sh
source "$ROOT/scripts/release-tag.sh"

release_assets() {
    local dist=$1 name
    local expected actual
    expected=$(printf '%s\n' \
        checksums.txt \
        ployz_linux_amd64.tar.gz \
        ployz_linux_arm64.tar.gz \
        ployz_macos_amd64.tar.gz \
        ployz_macos_arm64.tar.gz \
        ployzd_linux_amd64.tar.gz \
        ployzd_linux_arm64.tar.gz | sort)
    actual=$(find "$dist" -maxdepth 1 \( -name '*.tar.gz' -o -name checksums.txt \) -exec basename {} \; | sort)
    if [ "$actual" != "$expected" ]; then
        echo "release asset set differs from the seven approved files" >&2
        echo "expected:"$'\n'"$expected" >&2
        echo "actual:"$'\n'"$actual" >&2
        return 1
    fi
    while IFS= read -r name; do
        printf '%s/%s\n' "$dist" "$name"
    done <<< "$expected"
}

release_create_flags() {
    local tag=$1
    printf '%s\n' --draft
    if beta_release_tag "$tag"; then
        printf '%s\n' --prerelease
    fi
}

release_notes() {
    local tag=$1
    local changes=${2:-}
    local version=${tag#v}
    printf '## Notes\n\n'
    printf '## %s\n\n' "$tag"
    if [ -n "$changes" ]; then
        printf '%s\n\n' "$changes"
    fi
    cat <<EOF
## Install

\`\`\`sh
curl -fsSL https://ployz.sh | sh
curl -fsSL https://ployz.sh | sh -s $version
brew install getployz/ployz/ployz
\`\`\`

Homebrew tracks stable only. Beta: \`curl -fsSL https://ployz.sh | sh -s beta\`.

This is a **clean break** with manual transition. There is no in-place compatibility promise, configuration converter, or stored-state migration.
EOF
}

generated_changelog() {
    local tag=$1
    local repo=${GITHUB_REPOSITORY:-getployz/ployz2}
    gh api "repos/${repo}/releases/generate-notes" -f tag_name="$tag" --jq .body
}

if [ "${PLOYZ_RELEASE_TEST_ONLY:-false}" != true ]; then
    tag=${1:-}
    [ -n "$tag" ] || { echo "usage: $0 <tag>" >&2; exit 1; }
    dist=${DIST:-"$ROOT/dist"}
    mapfile -t assets < <(release_assets "$dist")
    mapfile -t flags < <(release_create_flags "$tag")
    gh release create "$tag" "${flags[@]}" --title "$tag" --notes "$(release_notes "$tag" "$(generated_changelog "$tag")")" "${assets[@]}"
fi
