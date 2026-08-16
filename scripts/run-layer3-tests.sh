#!/usr/bin/env bash
set -euo pipefail

retry_once() { "$@" || "$@"; }

cargo test --locked --no-run --workspace --all-targets

retry_once cargo test --package ployz --test build_layer3 --locked -- --ignored

mapfile -t scenarios < <(cargo test --package ployz-testkit --test cluster --locked -- --ignored --list | sed -n 's/: test$//p')
test "${#scenarios[@]}" -gt 0
for scenario in "${scenarios[@]}"; do
    retry_once cargo test --package ployz-testkit --test cluster --locked "$scenario" -- --ignored --exact
done
for suite in service_cluster internal_dns_cluster caddy_cluster deploy_cluster operator_cluster volume_layer3 workflow_layer3 hosted_dns_cluster; do
    retry_once cargo test --package ployz --test "$suite" --locked -- --ignored
done
retry_once cargo test --package ployzd --lib corrosion::integration_tests::replicated_store_preserves_partial_and_contradictory_observations --locked -- --ignored --exact
