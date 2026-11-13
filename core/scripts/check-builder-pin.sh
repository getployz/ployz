#!/usr/bin/env bash
# A Server trusts a rule build because the getployz/build commit the rule pins ran it, so the
# ployz CLI that commit installs must be one getployz/ployz's release pipeline built. Checks that
# the builder commit Ployz pins by default (BUILDER_SHA) installs a getployz/ployz release archive,
# by a digest that release's checksums.txt lists under the release key's signature.
# Given a build.yml path, checks that file in place of the pinned commit's.

set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() {
    echo "builder pin: $1" >&2
    exit 1
}

if [ $# -gt 0 ]; then
    cp -- "$1" "$work/build.yml"
else
    sha=$(sed -n 's/^pub const BUILDER_SHA: &str = "\([0-9a-f]\{40\}\)";$/\1/p' "$root/crates/ployz-core/src/authority/rule.rs")
    [ -n "$sha" ] || fail "no BUILDER_SHA in crates/ployz-core/src/authority/rule.rs"
    curl -fsSL --proto '=https' --retry 3 -o "$work/build.yml" \
        "https://raw.githubusercontent.com/getployz/build/$sha/.github/workflows/build.yml"
fi

pinned() {
    sed -n "s/^      $1: \([^ ]*\)\$/\1/p" "$work/build.yml"
}
release=$(pinned PLOYZ_RELEASE)
digest=$(pinned PLOYZ_ARCHIVE_SHA256)
# Rule builds need 0.4: no 0.3 release refuses a Service in two snapshots or scopes the cache.
[[ $release =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-beta\.(0|[1-9][0-9]*))?$ ]] ||
    fail "build.yml pins no getployz/ployz release in PLOYZ_RELEASE"
((BASH_REMATCH[1] > 0 || BASH_REMATCH[2] >= 4)) || fail "$release predates rule builds; pin 0.4 or later"
[[ $digest =~ ^[0-9a-f]{64}$ ]] || fail "build.yml pins no archive digest in PLOYZ_ARCHIVE_SHA256"

# The rest of the file, the install step included, must be the builder Ployz reviewed. A builder
# change updates this digest: of build.yml with the two values above as RELEASE-PENDING and DIGEST-PENDING.
reviewed=6da1cc9ed0b7e0ac488434ede9c4b4cbe0f34c6e4183e4d0b95169dd3cd22e8b
masked=$(sed -e "s/^      PLOYZ_RELEASE: $release\$/      PLOYZ_RELEASE: RELEASE-PENDING/" \
    -e "s/^      PLOYZ_ARCHIVE_SHA256: $digest\$/      PLOYZ_ARCHIVE_SHA256: DIGEST-PENDING/" "$work/build.yml" |
    sha256sum | cut -d' ' -f1)
[ "$masked" = "$reviewed" ] || fail "build.yml differs from the builder Ployz reviewed (digest $masked)"

for file in checksums.txt checksums.txt.minisig; do
    curl -fsSL --proto '=https' --retry 3 -o "$work/$file" \
        "https://github.com/getployz/ployz/releases/download/$release/$file"
done
trusted=$(minisign -V -Q -P "$(sed -n 2p "$root/crates/ployz-core/release.pub")" \
    -m "$work/checksums.txt" -x "$work/checksums.txt.minisig") ||
    fail "checksums.txt of $release is not signed by the release key"
[ "$trusted" = "ployz $release" ] || fail "the signature on checksums.txt names '$trusted', not ployz $release"
grep -qxF "$digest  ployz_linux_amd64.tar.gz" "$work/checksums.txt" ||
    fail "$digest is not ployz_linux_amd64.tar.gz of getployz/ployz $release"

echo "builder pin: ployz_linux_amd64.tar.gz of getployz/ployz $release, signed"
