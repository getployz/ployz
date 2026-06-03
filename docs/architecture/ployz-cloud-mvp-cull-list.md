# Ployz Cloud MVP Cull List

Date: 2026-06-03

Source plan: `docs/plans/2026-06-03-001-feat-cloud-mvp-operation-spine-plan.md`

This list cuts the existing repo against the smallest useful Cloud MVP:

- SSH stdio external control first.
- Rust-owned protocol and schema.
- Durable operation records, events, status, and advisory leases.
- One equal-node `ployzd`.
- Polis as product-neutral substrate.
- Minimal local runtime commands.
- First primitives: `machine.add` and `deploy.apply`.

`KILL` means remove from the MVP path and stop expanding it. It can mean delete,
hide from public exports, quarantine behind future docs, or leave tests archived
until the next slice removes them cleanly.

## Scan Signal

The largest current Rust surfaces are not the MVP spine:

- `crates/ployz-e2e/src/scenarios/volume_transfer.rs` is 972 lines.
- `crates/ployz/src/volume/mod.rs` is 905 lines.
- `crates/ployz/src/domain/tests.rs` is 878 lines.
- `crates/ployz/src/adapters/polis/acme_attempt.rs` is 781 lines.
- `crates/ployz/src/deploy/mod.rs` is 746 lines and is HTTPS/cert-first.
- `crates/ployz/src/acme/issuer.rs` is 694 lines.
- `crates/ployz/src/domain/mod.rs` is 576 lines.
- `crates/ployz/src/acme/mod.rs` is 570 lines.
- `crates/ployz/src/serving/mod.rs` is 564 lines.

The cull should remove those modules as drivers of the first build. The MVP
should make operation truth, daemon command serving, and local runtime actions
boring before reintroducing ACME, volumes, branch deploys, drains, or image
distribution.

## Test Surface Leaks

These are cases where test, fixture, or harness behavior leaks into normal
runtime or public API shape. The rule is simple: tests may use fakes, memory
stores, fixture ports, and bypass constructors, but user-facing runtime code
should not see them as normal choices.

### Kill or make private

- `crates/ployzd/src/config.rs` `CorrosionStartMode` and
  `with_corrosion_start_mode` - Public users should not pick Corrosion process
  lifecycle modes. The product behavior is `StartOrAdopt`. Keep strict
  `StartManaged` and `AdoptExisting` only as internal/test helpers if tests
  still need them.
- `crates/ployzd/src/lib.rs` `pub use config::CorrosionStartMode` - Remove
  from public exports.
- `crates/ployzd/src/report.rs` `StartupReport::configured`,
  `with_corrosion_shutdown`, `mark_corrosion_ready`, `mark_schema_ready`,
  `mark_peer_ready`, and failure mutators - Consumers should read daemon
  startup status, not manufacture lifecycle states. Make state transitions
  daemon-private.
- `crates/ployz/src/lib.rs` public `composition` module - Composition is
  internal wiring, not product API. Keep adapter assembly behind daemon/runtime
  construction.
- `crates/ployz/src/composition.rs` `certificate_readiness_with_attempts`,
  `corrosion_*`, `product_schema_statements`, `verify_product_schema`, and
  `iroh_peer_rpc_probe` - Keep as crate/internal adapter wiring unless a narrow
  daemon dependency needs it.
- `crates/ployz/src/acme/attempt.rs` public attempt store/types - ACME attempt
  persistence is an internal mini-ledger and should not be public runtime API.
  MVP defers ACME anyway.

### Gate to tests only

- `crates/polis/src/peers/probe.rs` `FakePeerProbe` - Currently public through
  `crates/polis/src/peers.rs` and `crates/polis/src/lib.rs`. Move to
  `#[cfg(test)]` or `test_support`; production peer checks should use real
  iroh/RPC probes.
- `crates/ployz/src/composition.rs` `in_memory_machine_membership`,
  `in_memory_domain_status`, and `in_memory_serving_snapshots` - Public helpers
  expose `Rc<RefCell>` memory adapters as normal runtime choices. Move to test
  modules or `test_support`.
- `crates/ployz/src/operation/context.rs` `MutationContext::test_authorized` -
  This bypasses `MutationAuthorizer` and `AuthorityPort`. Keep inside tests.
- `crates/ployz/src/operation/claims.rs` `ClaimGuard::test_new` - This
  fabricates claim guards without acquisition. Keep inside tests.
- `crates/ployz/src/domain/mod.rs` `DomainClaim::test_new` - This wraps a fake
  claim boundary. Keep inside tests.
- `crates/ployz/src/domain/mod.rs` `DomainServingActivation::test_active` -
  Kill outright. It is only a test-named alias for already-public `active`.
- `crates/polis/src/corrosion_agent/process.rs` `LocalCorrosionAgent::process_id`
  - Gated by `test-support`, but still public when enabled. Keep as
  crate-local test helper.

### Fix workspace and harness shape

- `Cargo.toml` `default-members` includes `crates/ployz-e2e` - Remove e2e from
  default workspace builds. The default build should not activate
  `test-support` paths.
- `crates/ployz-e2e/Cargo.toml` normal dependencies enable
  `ployz/test-support` and `polis/test-support` - Keep e2e explicit and
  test-only, or move fake acceptance tests into crate-level tests.
- `justfile` `e2e` runs `cargo run -p ployz-e2e` - The binary exits with
  harness wording. Change the recipe to the actual test command or remove it
  until real e2e exists.
- `crates/ployz-e2e/src/main.rs` - Kill the runnable binary surface that says
  the crate is only in-process tests.
- `crates/ployz-e2e/src/scenarios/machine_add.rs`,
  `crates/ployz-e2e/src/scenarios/domain_add.rs`,
  `crates/ployz-e2e/src/scenarios/https_deploy.rs`, and
  `crates/ployz-e2e/src/scenarios/volume_transfer.rs` - These are fake-backed
  acceptance tests living in the e2e crate. Move useful cases into crate tests
  or rename the harness. E2E should cross real process/network/runtime
  boundaries.
- `docs/testing/e2e.md` - Rewrite stale claims that `just e2e` runs real
  boundary scenarios.
- `docs/architecture/ployz-1-0-roadmap.md` "`ployz deploy preview` rendering a
  plan from an in-memory fixture" - Kill this as a user CLI milestone. Fixtures
  can test planning; they should not define runtime behavior.

## Keep

### Polis substrate

- `crates/polis/src/lib.rs` - Keep the product-neutral guardrail.
- `crates/polis/src/store.rs` - Keep typed Corrosion query, transaction, and
  subscription helpers for operations and events.
- `crates/polis/src/schema.rs` - Keep schema verification helpers.
- `crates/polis/src/corrosion_agent/` - Keep managed/adopted Corrosion
  lifecycle.
- `crates/polis/src/identity.rs` - Keep substrate identity typing.
- `crates/polis/src/peers/identity.rs` - Keep endpoint identity generation and
  persistence.
- `crates/polis/src/peers/tickets.rs` - Keep iroh tickets as substrate
  addresses.
- `crates/polis/src/peers/runtime.rs` - Keep peer runtime ownership, then wire
  it into command delivery.
- `crates/polis/src/peers/rpc.rs` - Keep the iroh RPC base, but only as
  product-neutral transport.
- `crates/polis/src/membership/` - Keep rows for machine substrate presence.
  Do not add Ployz join, deploy, drain, or install policy here.

### Daemon foundation

- `crates/ployzd/src/config.rs` - Keep state-dir, implicit `StartOrAdopt`
  Corrosion lifecycle, and peer identity path ownership.
- `crates/ployzd/src/substrate.rs` - Keep Corrosion, product schema, and peer
  startup as daemon-owned lifecycle.
- `crates/ployzd/src/daemon.rs` - Keep `DaemonRuntime` as lifecycle owner and
  router.
- `crates/ployzd/src/report.rs` - Keep startup/status reporting, but do not use
  it as operation status.

### Ployz product core

- `crates/ployz/src/operation/identity.rs` - Keep typed operation,
  idempotency, principal, scope, and resource IDs.
- `crates/ployz/src/operation/context.rs` - Keep mutation context as the seed
  for operation identity and idempotency.
- `crates/ployz/src/machine.rs` - Keep machine ID, epoch, network identity, and
  idempotent membership add shape.
- `crates/ployz/src/adapters/polis/machine_membership.rs` - Keep this adapter
  pattern: Ployz product semantics translated onto Polis rows.
- `crates/ployz/src/runtime/mod.rs` - Keep `WorkloadId`, `MachineId`, receipt,
  and status types.
- `crates/ployz/src/error.rs` - Keep typed failures, but move generic operation
  lifecycle out of feature errors.
- `crates/ployz/src/serving/mod.rs` - Keep only the minimal route commit ideas
  needed for gateway apply. Rich snapshot/proof semantics are not first.
- `crates/ployz/src/adapters/polis/serving.rs` - Keep as candidate minimal
  route row adapter after it is narrowed.

### Docs

- `VISION.md` - Keep as source of truth.
- `docs/plans/2026-06-03-001-feat-cloud-mvp-operation-spine-plan.md` - Keep as
  the current MVP center.
- `docs/plans/2026-05-24-001-feat-corrosion-store-iroh-membership-plan.md` -
  Keep for substrate and membership grounding.
- `docs/plans/2026-05-25-001-feat-substrate-spine-e2e-plan.md` - Keep for the
  substrate proof.
- `docs/plans/2026-05-25-002-feat-daemon-substrate-boot-plan.md` - Keep for
  daemon boot.
- `docs/plans/2026-05-25-003-refactor-ruthless-polis-cleanup-plan.md` - Keep
  for Polis boundary cleanup.
- `docs/solutions/architecture-patterns/operator-perspective-commands-with-corrosion-rows-2026-05-24.md`
  - Keep for command plus row ownership.
- `docs/solutions/architecture-patterns/authority-status-separates-truth-from-observation-2026-05-08.md`
  - Keep for status versus liveness.
- `docs/solutions/performance-issues/machine-add-timeout-tests-2026-05-10.md` -
  Keep the fake waits and transport testing lesson.

## Rework

### Operation spine

- `crates/ployz/src/operation/mod.rs` - Expand from identity/context into the
  central operation model:
  - operation record,
  - operation kind,
  - operation owner,
  - advisory lease,
  - ordered events,
  - terminal status,
  - liveness state,
  - cursor reads.
- `crates/ployz/src/operation/authority.rs` - Recut around operation owner and
  lease epoch. Keep authority checks narrow.
- `crates/ployz/src/operation/claims.rs` - Recut fence/claim ideas into the
  shared operation lease model. Volume-specific fencing should not shape MVP.
- `crates/ployz/src/operation/context.rs` - Let operation submission and replay
  own idempotency. Do not make every feature invent its own mini ledger.

### Daemon command serving

- `crates/ployzd/src/main.rs` - Replace the failure stub with real startup and
  command serving.
- `crates/ployzd/src/daemon.rs` - Add command-server ownership without adding
  feature-specific state.
- `crates/ployzd/src/substrate.rs` - Expose store and peer handles through
  narrow dependencies used by command handlers and operation runners.
- Add `crates/ployzd/src/control.rs` - Local control socket for `ployzctl`.
- Add `crates/ployzd/src/commands.rs` - Primitive command dispatch.
- Add `crates/ployzd/src/operations/` - Operation runner, lease renewal, event
  append, status reads, and stream reads.
- Add `crates/ployzd/src/runtime/` - Policy-free local handlers for
  capabilities, container start/stop/inspect, readiness probe, logs, and
  gateway apply.

### Peer RPC

- `crates/polis/src/peers/rpc.rs` - Extend preflight-only RPC into
  deadline-bound command delivery.
- Add `crates/polis/src/commands.rs` or `crates/polis/src/peers/command.rs` -
  Product-neutral envelope, target identity, correlation ID, deadline, and
  substrate failure types.
- Do not add `deploy`, `machine.add`, runtime command names, or product payloads
  to Polis.

### Runtime and deploy

- `crates/ployz/src/runtime/mod.rs` - Replace `activate_participant` and
  `verify_participant` with concrete local runtime commands:
  capabilities, start, stop, inspect, readiness, logs, and gateway apply.
- `crates/ployz/src/deploy/mod.rs` - Replace `deploy_https` with sequential
  `deploy.apply` over an image-backed service manifest.
- `crates/ployz/src/deploy/mod.rs` - Move progress out of one synchronous
  `DeployOutcome` and into operation events plus terminal status.
- `crates/ployz/src/machine.rs` - Lift membership join into a `machine.add`
  operation with install, check, join, preflight, events, and idempotent replay.
- `crates/ployz/src/composition.rs` - Stop making domain, certificate attempt,
  and serving schemas daemon startup prerequisites. Operation and membership
  schema should come first.
- `crates/ployz/src/adapters/memory.rs` - Keep only fakes needed for
  operation, deploy, machine, and runtime MVP tests.

### Membership rows

- `crates/polis/src/membership/model.rs` - Shrink product lifecycle influence.
  `Removing`, `Tombstoned`, and `Deleted` should not shape first machine add.
- `crates/polis/src/membership/schema.rs` - Revisit required
  `wireguard_public_key` and `overlay_ip`. First cloud add/status should not
  require WireGuard if the MVP uses SSH stdio plus iroh peer RPC.

### Docs

- `docs/architecture/ployz-cloud-backwards-roadmap.md` - Rebase around the
  operation spine as M0/M1.
- `docs/architecture/ployz-1-0-roadmap.md` - Recast as post-MVP roadmap.
  Current 1.0 asks for HTTPS, ACME, WireGuard, and ZFS volumes from day one.
- `docs/architecture/functional-system-roadmap.md` - Keep as a capability
  catalog, not execution sequence.
- `docs/architecture/deploy-primitives-roadmap.md` - Keep deploy-as-compiler,
  but put operation ledger and runner first.
- `docs/plans/2026-05-24-002-feat-ployz-1-0-cli-shape-and-workflows-plan.md` -
  Narrow first CLI to `rpc-stdio`, status, operation status/stream, deploy, and
  machine add.
- `docs/plans/2026-05-24-003-feat-ployz-1-0-state-and-substrate-plan.md` -
  Update for operation records. Its old "no operations table up front" choice
  is superseded.
- `docs/plans/2026-05-24-004-feat-ployz-1-0-deploy-branch-volume-plan.md` -
  Split first deploy from branch, rolling, volume, and drain work.

## Defer

### Product primitives

- `crates/ployz/src/acme/*` - Defer ACME, HTTP-01, certificate issuance,
  renewal, revocation freshness, and challenge ownership.
- `crates/ployz/src/domain/mod.rs` - Defer domain status rows.
- `crates/ployz/src/domain/readiness.rs` - Defer domain and cert readiness.
- `crates/ployz/src/adapters/polis/domain.rs` - Defer domain Corrosion schema.
- `crates/ployz/src/adapters/polis/acme_attempt.rs` - Defer ACME attempt
  schema. If it returns, fold lifecycle into the shared operation ledger.
- `crates/ployz/src/volume/mod.rs` - Defer fork, move, transfer, receive,
  cleanup, snapshots, and ZFS details.
- `crates/ployz/src/serving/mod.rs` - Defer rich serving commit proofs beyond
  minimal gateway config.

### E2E scenarios

- `crates/ployz-e2e/src/scenarios/volume_transfer.rs` - Defer with volumes.
- `crates/ployz-e2e/src/scenarios/acme_ownership.rs` - Defer with ACME.
- `crates/ployz-e2e/src/scenarios/https_deploy.rs` - Defer until HTTPS deploy
  returns.
- `crates/ployz-e2e/src/scenarios/domain_add.rs` - Defer domain control.
- `crates/ployz-e2e/src/scenarios/coordinator_restart.rs` - Revisit only after
  operation runner ownership exists.

### Roadmap/docs

- `docs/plans/2026-05-08-004-feat-service-branching-deploy-plan.md`
- `docs/plans/2026-05-09-001-feat-zfs-volume-move-execution-plan.md`
- `docs/plans/2026-05-10-001-feat-migrate-service-command.md`
- `docs/plans/2026-05-10-002-feat-machine-availability-aware-placement.md`
- `docs/plans/2026-05-10-003-feat-deploy-volume-snapshot-clone-branching.md`
- `docs/plans/2026-05-10-004-feat-core-build-image-availability-plan.md`
- `docs/plans/2026-05-10-004-fix-deploy-clone-replacement-preflight.md`
- `docs/plans/2026-05-10-005-feat-clone-replacement-preflight-preview.md`
- `docs/plans/2026-05-10-005-feat-image-inspect-availability.md`
- `docs/plans/2026-05-10-006-feat-layer-delta-image-placement-slice.md`
- `docs/plans/2026-05-10-006-feat-service-source-primitives.md`
- `docs/plans/2026-05-10-007-feat-image-receive-session-listener.md`
- `docs/plans/2026-05-10-007-feat-service-source-preview-baseline.md`
- `docs/plans/2026-05-10-008-feat-deploy-preview-baseline-envelope.md`
- `docs/plans/2026-05-10-008-feat-single-target-image-distribute.md`
- `docs/plans/2026-05-11-001-feat-durable-prepared-deploys.md`
- `docs/plans/2026-05-11-001-feat-image-push-existing-image-plan.md`
- `docs/plans/2026-05-11-002-feat-deploy-image-availability-preflight.md`
- `docs/plans/2026-05-11-003-feat-branch-command-plan-compiler.md`
- `docs/plans/2026-05-11-003-feat-local-build-image-availability.md`
- `docs/plans/2026-05-11-004-feat-branch-prepare-apply-prepared.md`
- `docs/plans/2026-05-11-005-feat-multi-target-image-distribute-plan.md`
- `docs/plans/2026-05-11-006-feat-volume-clone-branching-hardening.md`
- `docs/plans/2026-05-11-007-feat-railpack-frontend-executor.md`
- `docs/solutions/integration-issues/drain-aware-deploy-self-target-drain-nats-timeout-2026-05-10.md`

## Kill

### Code surface to remove from MVP

- `crates/ployz/src/lib.rs` public `volume` export - Stop advertising a
  deferred primitive as part of the current core surface.
- `crates/ployz/src/volume/mod.rs` - Remove or quarantine from the MVP build
  path.
- `crates/ployz/src/error.rs` `VolumeFailure` - Remove with volume surface.
- `crates/ployz/src/deploy/mod.rs` HTTPS/domain/certificate-first path - Kill
  as the first deploy command shape.
- `crates/ployz/src/acme/issuer.rs` owner-deadline replay and interrupt logic -
  Kill as an MVP dependency. Central operations own lease/liveness.
- `crates/ployz/src/acme/attempt.rs` and
  `crates/ployz/src/adapters/polis/acme_attempt.rs` separate attempt ledger -
  Kill as a parallel operation ledger for MVP.
- `crates/ployz/src/composition.rs` product schema startup set for
  domain/cert/serving - Kill as daemon boot prerequisite.
- `crates/ployz/src/machine.rs` `MachineStatus::Removing` and
  `MachineStatus::Tombstoned` - Kill from first machine add. Drain/remove comes
  later.
- `crates/polis/src/membership/model.rs`
  `MembershipLifecycle::{Removing,Tombstoned,Deleted}` - Kill from substrate
  MVP unless a later machine-remove slice justifies them.
- `crates/polis/src/peers/rpc.rs` product-payload enum growth - Do not add
  deploy or machine commands directly to Polis.
- `crates/ployzd/src/main.rs` "library surface only" behavior - Kill the stub
  binary behavior once command serving starts.

### Architecture choices to reject

- No dedicated coordinator daemon.
- No separate orchestration daemon.
- No "CLI is the orchestrator" model for durable work.
- No TypeScript-owned deploy orchestration for MVP. TypeScript submits and
  streams Rust operations.
- No global desired-state reconciler.
- No background loop that silently rewrites durable truth.
- No product-shaped Polis APIs such as `machines.join`, `deploy.apply`, or
  `capacity.reserve`.
- No external cloud iroh transport before SSH stdio works.
- No WireGuard NAT punching before operation, status, stream, and local runtime
  commands work.
- No ZFS volume movement before deploy and machine operations share the common
  runner.

### Docs to archive or supersede

- `docs/plans/2026-05-08-001-feat-authority-roadmap-plan.md`
- `docs/plans/2026-05-08-002-feat-authority-status-slice-plan.md`
- `docs/plans/2026-05-08-003-feat-nats-storage-promotion-plan.md`
- `docs/plans/2026-05-08-004-feat-compute-only-region-placement-plan.md`
- `docs/plans/2026-05-09-001-feat-dashboard-deploy-execution-skeleton.md`
- `docs/plans/2026-05-11-004-feat-open-core-docs-site.md`
- `docs/plans/2026-05-11-007-feat-branch-environment-lifecycle.md`
- `docs/nats.md` - Keep only as historical note if it remains.

## First Cull Order

1. Remove public test/harness surfaces from normal runtime API.
2. Remove `ployz-e2e` from default workspace builds or make it a real test-only
   harness.
3. Land operation ledger, events, status, and lease schema as the new center.
4. Make `ployzd` run and serve local `rpc-stdio` commands.
5. Shrink startup schema prerequisites to membership plus operations.
6. Hide or quarantine `volume`, ACME attempt, domain readiness, and HTTPS deploy
   from the public MVP surface.
7. Rework runtime into concrete local commands.
8. Implement `machine.add` as an operation.
9. Implement image-backed `deploy.apply` as an operation.
10. Reintroduce gateway route commit only as minimal config apply.

## Cull Rule

Keep code if it directly helps submit, run, observe, stream, lease, or finish a
machine/deploy operation on equal nodes.

Rework code if the concept is right but the current shape makes ACME, volume,
WireGuard, branch, drain, or HTTPS assumptions.

Defer code if it is a good future primitive but not needed to prove cloud can
submit operations and stream progress.

Kill code if it creates another operation ledger, makes a deferred primitive
mandatory, pushes product policy into Polis, or makes daemon startup depend on
non-MVP domains.
