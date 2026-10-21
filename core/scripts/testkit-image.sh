#!/usr/bin/env bash
set -euo pipefail

# Build TAG: this checkout's ployz, ployzd and entrypoint over a testkit base
# built here from crates/ployz-testkit/Dockerfile. The primed base is cached
# under ployz-testkit-base:local and primed again only when Docker's layer
# cache produced a different image. Host binaries need Ubuntu 24.04's glibc.
#   PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev)
tag=${1:?usage: scripts/testkit-image.sh TAG}
base=ployz-testkit-base:local
# Another checkout can retag $base while this one compiles, so this run reads
# and writes only image ids and tags carrying its own run id.
run=$(od -An -N4 -tx4 /dev/urandom | tr -d ' ')
source=ployz-testkit-base:source-$run
pinned=ployz-testkit-base:run-$run
# From core/, so rustup takes the pinned rust-toolchain.toml.
cd "$(dirname "$0")/.."
core=$PWD

context=$(mktemp -d)
prime_pid=
cleanup() {
    # A prime container still holds $source, so stop the prime and wait for it
    # to remove its container before untagging.
    if [ -n "$prime_pid" ]; then
        kill "$prime_pid" 2>/dev/null || true
        wait "$prime_pid" 2>/dev/null || true
    fi
    rm -rf "$context"
    for owned in "$source" "$pinned"; do
        docker image inspect "$owned" >/dev/null 2>&1 || continue
        docker image rm "$owned" >/dev/null 2>&1 \
            || { sleep 2; docker image rm "$owned" >/dev/null; } \
            || echo "could not remove $owned; remove it with: docker image rm $owned" >&2
    done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# A provenance attestation carries the build time, so the id would change on
# every cached build and force a fresh prime.
docker build --provenance=false --tag "$source" --file crates/ployz-testkit/Dockerfile "$core" >&2
source_id=$(docker image inspect --format '{{.Id}}' "$source")
read -r base_id primed_from < <(docker image inspect --format '{{.Id}} {{index .Config.Labels "dev.ployz.testkit.primed-from"}}' "$base" 2>/dev/null) || true
if [ "${primed_from:-}" != "$source_id" ]; then
    echo "priming $base from $source_id" >&2
    # In the background, so a signal reaches cleanup without waiting for it.
    scripts/prime-testkit-image.sh "$source_id" "$base" >"$context/base.id" &
    prime_pid=$!
    wait "$prime_pid"
    prime_pid=
    base_id=$(cat "$context/base.id")
fi
docker tag "$base_id" "$pinned"

cargo build --locked --package ployz --package ployzd >&2
target=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)

cp "$target/debug/ployz" "$target/debug/ployzd" "$core/crates/ployz-testkit/entrypoint.sh" "$context/"
printf 'FROM %s\nCOPY ployzd ployz /usr/local/bin/\nCOPY --chmod=755 entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint\n' "$pinned" >"$context/Dockerfile"
docker build --quiet --tag "$tag" "$context" >/dev/null
echo "$tag"
