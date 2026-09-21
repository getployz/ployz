#!/usr/bin/env bash
set -euo pipefail

log_dir=${PLOYZ_LAYER3_LOG_DIR:-target/layer3-logs}
mkdir -p "$log_dir"
failed=0

run_suite() {
    local name=$1 started=$SECONDS status=0 result
    shift
    printf '\n::group::%s\n' "$name"
    printf 'Reproduce: '
    printf '%q ' "$@"
    printf '\n'
    timeout --kill-after=10s 5m "$@" 2>&1 | tee "$log_dir/$name.log" || status=$?
    printf '::endgroup::\n'
    result=passed
    if [ "$status" -ne 0 ]; then
        result="failed (exit $status)"
        failed=1
        printf '::error title=%s::%s; see cluster-test-logs/%s.log\n' "$name" "$result" "$name"
    fi
    printf '| %s | %s | %ss |\n' "$name" "$result" "$((SECONDS - started))" | tee -a "$log_dir/results.md"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        tail -1 "$log_dir/results.md" >> "$GITHUB_STEP_SUMMARY"
    fi
}

printf '| Suite | Result | Duration |\n|---|---|---|\n' > "$log_dir/results.md"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$log_dir/results.md" >> "$GITHUB_STEP_SUMMARY"
fi

timeout --kill-after=10s 10m cargo test --locked --no-run \
    --package ployz \
    --package ployz-testkit \
    --package ployzd \
    --tests \
    --lib 2>&1 | tee "$log_dir/compile.log"

run_suite build_layer3 cargo test --locked --no-fail-fast \
    --package ployz \
    --test build_layer3 \
    -- --ignored --test-threads=1

run_suite service_cluster cargo test --locked --no-fail-fast --package ployz \
    --test service_cluster \
    -- --ignored --test-threads=1

run_suite internal_dns_cluster cargo test --locked --no-fail-fast --package ployz \
    --test internal_dns_cluster \
    -- --ignored --test-threads=1

run_suite ingress_cluster cargo test --locked --no-fail-fast --package ployz \
    --test ingress_cluster \
    -- --ignored --test-threads=1

run_suite operator_cluster cargo test --locked --no-fail-fast --package ployz \
    --test operator_cluster \
    -- --ignored --test-threads=1

run_suite volume_layer3 cargo test --locked --no-fail-fast --package ployz \
    --test volume_layer3 \
    -- --ignored --test-threads=1

run_suite workflow_layer3 cargo test --locked --no-fail-fast --package ployz \
    --test workflow_layer3 \
    -- --ignored --test-threads=1

run_suite hosted_dns_cluster cargo test --locked --no-fail-fast --package ployz \
    --test hosted_dns_cluster \
    -- --ignored --test-threads=1

run_suite certificates_cluster cargo test --locked --no-fail-fast --package ployz \
    --test certificates_cluster \
    -- --ignored --test-threads=1

run_suite cluster cargo test --locked --no-fail-fast \
    --package ployz-testkit \
    --test cluster \
    -- --ignored --test-threads=1

run_suite replicated_store cargo test --locked --no-fail-fast --package ployzd --lib \
    corrosion::integration_tests::replicated_store_preserves_partial_and_contradictory_observations \
    -- --ignored --exact --test-threads=1

run_suite deploy_execution cargo test --locked --no-fail-fast --package ployz --lib \
    deploy::exec::cluster_tests::deploy_execution_preserves_partial_effects_and_never_repairs_them \
    -- --ignored --exact --test-threads=1

exit "$failed"
