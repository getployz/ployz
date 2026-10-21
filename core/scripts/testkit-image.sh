#!/usr/bin/env bash
set -euo pipefail

# Build TAG: this checkout's ployz, ployzd and entrypoint over a testkit base
# built here from crates/ployz-testkit/Dockerfile. The base keeps the stable
# local tag ployz-testkit-base:local and is primed again only when Docker's
# layer cache produced a different image. Host binaries need Ubuntu 24.04's glibc.
#   PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev)
tag=${1:?usage: scripts/testkit-image.sh TAG}
unprimed=ployz-testkit-base:unprimed
base=ployz-testkit-base:local
# From core/, so rustup takes the pinned rust-toolchain.toml.
cd "$(dirname "$0")/.."
core=$PWD

# A provenance attestation carries the build time, so the id would change on
# every cached build and force a fresh prime.
docker build --provenance=false --tag "$unprimed" --file crates/ployz-testkit/Dockerfile "$core" >&2
built=$(docker image inspect --format '{{.Id}}' "$unprimed")
primed_from=$(docker image inspect --format '{{index .Config.Labels "dev.ployz.testkit.primed-from"}}' "$base" 2>/dev/null || true)
if [ "$primed_from" != "$built" ]; then
    echo "priming $base from $built" >&2
    scripts/prime-testkit-image.sh "$unprimed" "$base" >&2
fi

cargo build --locked --package ployz --package ployzd >&2
target=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)

context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
cp "$target/debug/ployz" "$target/debug/ployzd" "$core/crates/ployz-testkit/entrypoint.sh" "$context/"
printf 'FROM %s\nCOPY ployzd ployz /usr/local/bin/\nCOPY --chmod=755 entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint\n' "$base" >"$context/Dockerfile"
docker build --quiet --tag "$tag" "$context" >/dev/null
echo "$tag"
