# Ponytail shrink: delete unused code and dependencies

## Context

A whole-repo ponytail audit (over-engineering scan; fresh-context subagent)
found ~1050 deletable lines and 6 removable dependencies, all
behavior-preserving. Every finding below was independently re-verified by
reference grep before entering this plan; two name collisions were checked
and cleared (ployz host defines its own `redeem_join_token` trait method,
and ployz's `MachineAddCommand` is an unrelated clap struct).

Git history is the parking lot: the two staged-feature deletions (cert
planning, machine-add lifecycle API) are recoverable verbatim from this
commit's parent when the wiring PR arrives.

## Phase 1 — Dependency cuts

- D1. `crates/ployzd/Cargo.toml`: remove `aws-config` and `aws-sdk-s3`
  (zero references in src or tests; ~100 transitive crates) and `sha2`
  (only string literals named "sha256" appear, in tests).
- D2. `crates/ployz-e2e/Cargo.toml`: remove `thiserror` (unused).
- D3. `crates/ployz-host-runner/Cargo.toml`: remove `async-nats` (unused).
- D4. `crates/ployz-nats`: remove `semver`. `NatsServerVersion::parse`
  (schedules.rs) keeps its shape but parses by hand: trim at the first
  `-` or `+`, `split('.')`, three `u16::parse` calls. Existing unit tests
  (`nats_server_version_keeps_core_semver_numbers`, error cases) pin the
  behavior; keep them passing unchanged.

## Phase 2 — Staged-feature deletions

- S1. Delete `crates/ployzd/src/controllers/cert.rs` (160 lines), its
  `pub mod cert;` declaration, and `crates/ployzd/tests/cert_operation.rs`
  (315 lines). No production caller exists; certs re-enter with the PR
  that wires renewal into a controller with an operation owner.
- S2. Delete the parallel machine-add lifecycle API in
  `crates/ployz-core/src/machine.rs`: `MachineAddCommand`,
  `MachineAddPlan`, `MachineReservation`, `plan_machine_add`,
  `redeem_join_token`, `MachineJoinOutcome`, `activate_joined_machine`,
  `MachineActivationOutcome` (~95 lines) and
  `crates/ployz-core/tests/machine_lifecycle.rs` (187 lines). Production
  path is `redeem_pending_join_token` + `active_machine_from_completed_add`,
  which stay.
- S3. Delete `PloyzNativeMeshPrepareRequest::for_deploy_plan` and private
  `for_machines` plus their unit tests in
  `crates/ployz-core/src/dataplane.rs` (~78 lines). Production constructs
  via `from_dataplane_request`.

## Phase 3 — Dead code removals

- C1. `crates/ployz-nats/src/observations.rs`: delete
  `watch_machine_public_ip_changes` and `watch_gateway_status_changes`
  (~28 lines, no callers).
- C2. `crates/ployz-core/src/deploy.rs`: delete `plan_service_deploy`;
  port its 7 test call sites to `plan_namespace_deploy` with a
  single-service request.
- C3. `crates/ployzd/src/controllers.rs`: delete
  `OperationControllers::for_test` (unused even by tests).
- C4. `crates/ployz-host-runner/src/fsx.rs`: drop the ignored `_staged_tag`
  parameter from `write_durable_file`; update the ~14 call sites in
  local.rs, cloud_bootstrap.rs, main.rs.
- C5. Delete `load_gateway_projection_input_from_nats`
  (gateway_source.rs) and `refresh_gateway_runtime_from_nats`
  (gateway_runtime.rs); production uses the `_update_` variants.
- C6. `crates/ployz-nats/src/operations/status_store.rs`: delete the
  `machine_add_mint_claim` getter (write path stays).
- C7. `crates/ployzd/src/machine_runtime/process.rs`: delete the
  write-only `observer_health` / `health` fields. Health visibility for
  the observer returns deliberately, with a consumer, not as a counter
  nothing reads.
- C8. Delete `ControlProcessConfig::with_machine_bootstrap_url`
  (ployzd config.rs), `load_default_cluster_context` (ployz
  config.rs), `NatsRequestFailure` + `SERVICE_API_INFO_SUBJECT` +
  `SERVICE_API_STATS_SUBJECT` (ployz-nats services.rs),
  `RECOMMENDED_NATS_SERVER_VERSION` (ployz-nats bootstrap.rs),
  `SecuredTestNats::system_config` (test-support),
  `CloudClient::get_text` and `Host RunnerTextRecorder::writer`
  (ployz host).

## Phase 4 — Shrinks

- K1. `packages/ployz-sdk/src/nats.ts`: inline the private `#request`
  into `request` (pure delegation).
- K2. `crates/ployz-core/src/wire.rs`: delete `format_u64_string`; the
  macro calls `.to_string()` directly.
- K3. Clippy-confirmed micro-cuts: `std::slice::from_ref` at
  `ployzd/tests/deploy_operation/fixtures.rs:752` and
  `ployz-e2e/tests/operations.rs:796`; drop the redundant
  `machine_id.clone()` at `ployzd/src/operation_api/queries.rs:726`.

## Not in scope

- Anything requiring new abstractions or moved modules.
- The `ployz-sdk` package-lock (stays untracked).
- Wiring observer health into visible health (deliberate future work).

## Verification

1. `cargo check --workspace --all-targets` — zero warnings (proves the
   dep and code removals left no dangling references).
2. `cargo test --workspace --exclude ployz-e2e --exclude ployz host` —
   full suite green; C2's ported planner tests still cover single-service
   planning.
3. `cargo clippy --workspace --all-targets` — clean, including the K3
   sites it previously flagged.
4. `npm run typecheck` in packages/ployz-sdk after K1.
5. `cargo tree -p ployzd | grep -c aws` returns 0; build time drop is the
   visible payoff.
