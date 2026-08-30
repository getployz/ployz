#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
action="$ROOT/.github/actions/build-testkit-image/action.yml"
workflow="$ROOT/.github/workflows/ci.yml"
dockerfile="$ROOT/ployz-testkit/Dockerfile"

assert_contains() {
    if ! grep -Fq -- "$2" "$1"; then
        echo "$1 does not contain: $2" >&2
        exit 1
    fi
}

if grep -Fq "FROM rust" "$dockerfile"; then
    echo "$dockerfile still contains: FROM rust" >&2
    exit 1
fi
assert_contains "$dockerfile" "FROM ubuntu:24.04"
assert_contains "$dockerfile" "COPY ployz-testkit/prebuilt/ployz /usr/local/bin/ployz"
assert_contains "$dockerfile" "COPY ployz-testkit/prebuilt/ployzd /usr/local/bin/ployzd"
assert_contains "$dockerfile" 'VOLUME ["/var/lib/ployz"]'
assert_contains "$dockerfile" 'VOLUME ["/var/lib/docker"]'

entrypoint="$ROOT/ployz-testkit/entrypoint.sh"
assert_contains "$entrypoint" "docker load --input"
assert_contains "$entrypoint" "/opt/ployz/docker-graph.tar"
assert_contains "$entrypoint" "! -f /opt/ployz/docker-graph.tar"
assert_contains "$entrypoint" "/opt/ployz/images/*.tar"

prime="$ROOT/scripts/prime-testkit-image.sh"
assert_contains "$prime" "docker commit"
assert_contains "$prime" "PLOYZ_TESTKIT_PRIME=1"
assert_contains "$prime" "ENV PLOYZ_TESTKIT_PRIME=0"
assert_contains "$entrypoint" "PLOYZ_TESTKIT_PRIME"
assert_contains "$action" "scripts/prime-testkit-image.sh"
assert_contains "$action" "docker push"
assert_contains "$action" "load: true"

layer3="$ROOT/scripts/run-layer3-tests.sh"
assert_contains "$layer3" "--test-threads=1"
assert_contains "$layer3" "--no-fail-fast"
assert_contains "$layer3" "--test certificates_cluster"
if grep -Fq "for scenario" "$layer3"; then
    echo "$layer3 still isolates cluster scenarios" >&2
    exit 1
fi

assert_contains "$action" "Swatinem/rust-cache@v2"
assert_contains "$action" "shared-key: testkit-release"
assert_contains "$action" "cargo build --locked --release --package ployz --package ployzd"
assert_contains "$action" "install -D target/release/ployz ployz-testkit/prebuilt/ployz"
assert_contains "$action" "install -D target/release/ployzd ployz-testkit/prebuilt/ployzd"
assert_contains "$action" "type=registry,ref=ghcr.io/getployz/ployz2-testkit:main"
assert_contains "$action" "cache-to: type=inline"

assert_contains "$ROOT/.gitignore" "ployz-testkit/prebuilt/"

for job in layer3-image-amd64 layer3-image-arm64; do
    if ! grep -A12 "^  ${job}:" "$workflow" | grep -Fq "actions: write"; then
        echo "$job must have actions: write for rust-cache" >&2
        exit 1
    fi
done
