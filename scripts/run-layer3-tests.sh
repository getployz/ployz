#!/usr/bin/env bash
set -euo pipefail

retry_once() { "$@" || "$@"; }

cargo test --locked --no-run \
    --package ployz \
    --package ployz-testkit \
    --package ployzd \
    --tests \
    --lib

retry_once cargo test --locked \
    --package ployz \
    --test build_layer3 \
    --test service_cluster \
    --test internal_dns_cluster \
    --test caddy_cluster \
    --test deploy_cluster \
    --test operator_cluster \
    --test volume_layer3 \
    --test workflow_layer3 \
    --test hosted_dns_cluster \
    -- --ignored --test-threads=1

retry_once cargo test --locked \
    --package ployz-testkit \
    --test cluster \
    -- --ignored --test-threads=1

retry_once cargo test --locked --package ployzd --lib \
    corrosion::integration_tests::replicated_store_preserves_partial_and_contradictory_observations \
    -- --ignored --exact --test-threads=1
