#!/usr/bin/env bash
set -euo pipefail

# Build TAG: this checkout's ployz, ployzd and entrypoint over the published
# testkit image, whose nested Docker graph they reuse. testkit:main is
# published only by a manual Cluster checks run on main, so its own binaries
# and entrypoint can be any age. Host binaries need Ubuntu 24.04's glibc.
#   PLOYZ_TESTKIT_IMAGE=$(scripts/testkit-image.sh ployz-testkit-checkout:dev)
tag=${1:?usage: scripts/testkit-image.sh TAG}
base=ghcr.io/getployz/ployz-testkit:main
# From core/, so rustup takes the pinned rust-toolchain.toml.
cd "$(dirname "$0")/.."
core=$PWD

cargo build --locked --package ployz --package ployzd >&2
target=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
docker image inspect "$base" >/dev/null 2>&1 || docker pull "$base" >&2

context=$(mktemp -d)
trap 'rm -rf "$context"' EXIT
cp "$target/debug/ployz" "$target/debug/ployzd" "$core/crates/ployz-testkit/entrypoint.sh" "$context/"
printf 'FROM %s\nCOPY ployzd ployz /usr/local/bin/\nCOPY --chmod=755 entrypoint.sh /usr/local/bin/ployz-testkit-entrypoint\n' "$base" >"$context/Dockerfile"
docker build --quiet --tag "$tag" "$context" >/dev/null
echo "$tag"
