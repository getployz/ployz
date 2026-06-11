---
title: "refactor: Full-project simplification pass"
type: refactor
status: planned
date: 2026-06-12
origin:
  - whole-project thermonuclear audit (6 parallel reviewers + synthesis)
---

# refactor: Full-project simplification pass

## Assessment

The codebase is far healthier than the owner's felt pain suggests. The defensive discipline (typed ids, exhaustive matches, enum state machines, bounded I/O, evidence-on-failure) is consistently applied and genuinely good — none of it should be touched. The real rot is almost entirely COPY-AMPLIFICATION, not design failure: one correct pattern gets written once and pasted N times with names swapped (positive-u64 wire newtype x6, with_timeout wrapper x13, TestNats fixture x14, node_id test helper x47, the submit-operation flow x3, the submit-error enum x4 across 3 crates, NKey minting x3, staged-temp-file machinery x3). Roughly 60-70% of the badness is this duplication plus genuinely dead/speculative code (empty time.rs, zero-caller constructors, MachineDrain/ServiceRemove variants for operations that don't exist, a 733-line 'do not run' script that still receives edits, an 11-line re-export with zero importers) — all fixable by deletion and consolidation with zero behavior change, on the order of 5,000-7,000 lines removable. Another ~20% is file organization: operation_api.rs (1533 lines, five jobs), ops.rs (1060), runtime.rs (1078), local.rs (1016), and the node_* family that carves at accretion order rather than at joints — fixable by pure moves. Only ~10-15% of the felt pain is the verbose-by-design style itself, and that part is deliberate and correct per AGENTS.md; it should stay. The two worst compounding rots are the dual install model (a 23-field flattened twin bridged by 130 lines of From shuffling, metastasizing into hand-built fixtures across 5 crates) and the four-times-written operation-submit failure model (every new operation kind currently adds a full new column of parallel enums and mappers). The single highest-leverage structural fix is breaking ployz-test-support's dependency on ployzd, which unlocks deleting ~1,500-2,000 lines of copy-pasted test scaffolding. Behavior is pinned throughout: every change-set below is deletions, moves, and shape-preserving consolidation verified by the existing 83 suites plus the gated DinD e2e run.

## Change sets (ordered)

### CS1-scripts: Delete dead scripts, trim superseded layers, consolidate live script helpers

**Effort:** small — **Order:** independent — **Risk:** Low. Verification: run scripts/dind-e2e.sh (gated DinD suite) and scripts/local-dataplane-proof.sh Layer A end-to-end; grep confirms no Rust code references the deleted scripts.

hetzner-two-node-acceptance.sh is self-marked 'Do not run' and invokes a ployzd subcommand that no longer exists (verified: its e2e driver tests/h0_script.rs is already deleted from disk); prepare-h0-artifacts.sh exists only to feed it. local-dataplane-proof.sh Layer B duplicates the DinD harness. Live scripts have already drifted copies of docker_platform/sha256/cargo-build recipes.

- Delete /Users/nick/dev/ployz-rust/scripts/hetzner-two-node-acceptance.sh and /Users/nick/dev/ployz-rust/scripts/prepare-h0-artifacts.sh; delete or attic /Users/nick/dev/ployz-rust/docs/operations/two-node-acceptance.md with a pointer to ADR-0013 and the DinD harness
- Trim /Users/nick/dev/ployz-rust/scripts/local-dataplane-proof.sh to Layer A only (lines 1-152); Layer B behavior is owned by crates/ployz-e2e/tests/dind_cluster.rs
- Add /Users/nick/dev/ployz-rust/scripts/lib.sh owning docker_platform(), sha256_of(), and the 4-crate binary list; source it from build-dind-machine-image.sh, dind-e2e.sh, local-dataplane-proof.sh
- Make build-dind-machine-image.sh the only builder of linux artifacts/eBPF bytecode; local-dataplane-proof.sh consumes its outputs

### CS2-dead-rust: Delete dead, speculative, and test-only-alive Rust code across all crates

**Effort:** medium — **Order:** independent (run before CS3 — both touch ployz-sdk-types/tests/exports.rs and ops.rs) — **Risk:** Low. Verification: cargo test --workspace (all 83 suites) + ployz-sdk-types exports test + gated DinD e2e; every deletion was grep-verified to have zero production callers.

Verified dead by grep: empty time.rs, zero-importer permissions.rs re-export, zero-caller constructors, enum variants for operations that don't exist, an identity-wrapper file, a self-testing in-memory JetStream fake, and production types kept alive only by their own tests. Pure deletion, zero behavior change.

- ployz-core: delete src/time.rs + its lib.rs line; delete WireGuardEbpfPrepareRequest::peerless and the two unused for_deploy_plan* constructors in src/dataplane.rs (rename the surviving maximal one to for_deploy_plan, repoint the unit test and ployzd/src/deploy_worker/types.rs:97); delete OperationSubject::MachineDrain/ServiceRemove variants in src/ops.rs:945-946 (verified: definition-only, no producer/consumer) and update ployz-sdk-types/tests/exports.rs + TS export; delete both is_schedulable() constant-boolean methods (src/machine.rs, src/state.rs) and the no-op .filter() at ployzd/src/deploy_worker/facts.rs:114
- ployz-nats: delete src/permissions.rs (verified zero ployz_nats::permissions importers) + lib.rs line; delete the in-memory OperationEventStream/OperationStreamMessage/OperationStreamAppend fake in src/streams.rs:87-205 and its two self-tests in tests/operations.rs (real dedupe is covered by tests/operations_nats/)
- ployzd: delete src/node_agent/observer.rs (InMemoryObservationStore, zero usages); delete NodeServiceCallError + its only (test) importer, the three unused api_*_endpoint helpers, and the node_endpoint_subject identity wrapper in src/services.rs; delete the never-produced NodeContainerRunDomainError::StartedContainerUnhealthy wire variant in src/node_protocol.rs plus its client/failure plumbing in node_rpc.rs and deploy_worker/failure.rs; delete src/node_runtime.rs (46-line identity layer) pointing callers at node_service_runtime directly
- ployzctl/keeper: delete src/operation_handle.rs (verified: only lib.rs references it; runtime.rs hand-rolls the same pagination — keep the live copy); delete consume_join_token_file in ployz-keeper/src/join.rs; delete the production-unused bootstrap_script_plan/BootstrapScriptTarget/KeeperArtifactTarget/ArtifactKind::Keeper family in ployz-keeper/src/steps.rs and move its tests onto a target kind production installs; move load_startup/KeeperStartupCli and the test-only KeeperStepPlan predicates into the test files that use them
- ployz-core/src/ops/routes.rs: rename infallible RouteTarget::try_new to new

### CS3-install-model: Collapse the dual install model and unify NKey identity types in ployz-core

**Effort:** medium — **Order:** after CS2-dead-rust (shared files: install.rs neighborhood, sdk-types exports test) — **Risk:** Medium — touches serde types, but all wire shapes are preserved by construction. Verification: ployz-core install_contract + wire_contract tests, ployz-sdk-types exports test, full workspace suite, gated DinD e2e (exercises the real join/install JSON end to end).

The worst single rot, flagged independently by three reviewers. KeeperFirstNodeInstall is a 23-field flattened twin of FirstNodeInstallSpec bridged by ~130 lines of From shuffling — verified that the keeper stdin wire contract is the NESTED spec (ployzctl/src/runtime.rs:416 converts flat back to nested before serializing), so the flat type never crosses a wire. Three byte-identical artifact structs metastasize into hand-built fixtures across 5 crates. NKey minting is implemented 3x backed by a weakly-validated duplicate seed newtype with a literal round-trip conversion in mint.rs.

- Delete KeeperFirstNodeInstall and both From impls from ployz-core/src/install.rs (~200 lines); ployz-keeper/src/cli.rs first_node_install_target destructures FirstNodeInstallSpec.artifacts directly; ployzctl/src/commands/init.rs FirstNodeInitMode carries FirstNodeInstallSpec and runtime.rs serializes it without conversion; update ployz-core/tests/install_contract.rs
- Delete MachineJoinPloyzdArtifact and MachineJoinArtifact (field-for-field identical to InstallArtifactSpec, identical serde so wire shape unchanged); MachineJoinMaterial holds three InstallArtifactSpec fields; shrink ployz-sdk-types re-exports/TS bindings by two types; update the 7 fixture sites across 5 crates (they get consolidated in CS4)
- Add MintedNatsUser { public, seed } with ::generate() next to NatsUserSeed in ployz-core/src/nats_config.rs; delete the three local mints (ployz-keeper/src/nats_identity.rs:144-156, ployzd/src/nats_authorization/mint.rs:411-425, ployz-test-support/src/nats.rs:252-262) and both duplicate Minted* structs
- Replace MachineJoinNatsCredentials with NatsUserSeed in the join material (serde wire shape stays a plain string; real values are always valid 58-char SU seeds), deleting the mint.rs:427-434 round-trip conversion and the install.rs newtype

### CS4-test-support: Break ployz-test-support's ployzd dependency and consolidate the workspace fixture zoo

**Effort:** large — **Order:** after CS3-install-model (fixtures must build against the final type vocabulary) — **Risk:** Low-medium — test-only plus one dependency-graph change; production untouched except the keeper lib call. Verification: cargo test --workspace (all 83 suites must stay green with identical assertions), cargo check on the dependency direction, gated DinD e2e.

The single highest-leverage structural fix. Verified: ployz-test-support/Cargo.toml depends on ployzd via node.rs imports, so the crates that need shared fixtures most (ployz-core, ployz-nats, ployz-keeper) cannot import it. Result: 47 copies of node_id, ~14 hand-rolled TestNats wrappers, 5 terminal-status pollers, 9+ hand-rolled JetStream bootstraps drifting from production, triplicated join-template fixtures, 3 shell_quote copies.

- Move ObservingContainerRunner and the ployzd-coupled parts of ployz-test-support/src/node.rs into ployzd/tests/support/ (its only natural consumers); ployz-test-support becomes a leaf over ployz-core + ployz-nats
- Add ids/fixtures modules to ployz-test-support: typed-id constructors (promote ployz-e2e/tests/support/ids.rs), deploy_target/deploy_request builders, the canonical machine_join_template builder (replacing the two byte-identical JSON literals in ployz-e2e/tests/operations.rs and ployzd/tests/backup_restore.rs), the install-spec/artifact fixture family (keeper tests bootstrap.rs/local.rs/systemd.rs, ployzctl cli_contract.rs), make_executable/assert_file_mode cfg(unix) helpers, and shell_quote published once
- Add one connected TestNats fixture (controller/user/node clients, jetstream context, optional resource bootstrap) replacing the ~14 per-file wrappers and 15 duplicate timeout constants; ployzd/tests/support/control.rs keeps only control-process wiring
- Add bootstrap_resources() delegating to the production BootstrapPlan::for_single_server_client + assure_nats_resources, deleting the parallel stream/bucket config language in ployz-nats/tests/operations_nats/fixtures.rs and the 8 ad-hoc create_key_value/create_stream recipes
- Add wait_for_terminal_status(api, operation_id, budget) using OperationStatus::is_terminal (already exists at ops.rs:585) plus a poll_until helper, deleting the five per-kind pollers and absorbing ~15 ad-hoc sleep loops
- Make ployz-test-support call ployz-keeper's generate_cluster_nats_identity instead of duplicating the rcgen CA+server-cert recipe; drop test-support's rcgen dependency
- Split ployz-nats/tests/operations_nats/evidence.rs (1028 lines) and submission.rs (1007 lines) by scenario while their fixtures are being rewritten anyway

### CS5-core-ops: ployz-core internals: split ops.rs, single-source the projection mappings, macro-ize the wire newtype scaffolding

**Effort:** medium — **Order:** after CS2-dead-rust and CS4-test-support (shared ops.rs and core test files); parallel-safe with CS6/CS10/CS13/CS14 (different crates) — **Risk:** Medium on the macro (less greppable) — mitigated by keeping expansion mechanical and identical to today; wire_contract.rs pins every serialized shape. Verification: ployz-core full test suite, especially wire_contract.rs and operation_projection.rs, plus workspace build.

ops.rs is 1060 lines mixing five concerns; the deploy evidence→stage mapping is written three times in projection.rs and must silently agree; three structurally identical projection-result enums exist only to be converted into each other; the positive-u64 wire newtype machinery is copy-pasted six times (the TYPES are deliberate style — the sextuplicated ~60-line scaffolding around each is not).

- Split ployz-core/src/ops.rs: leases to ops/lease.rs, replay types to ops/replay.rs, OperationEvent+OperationSubject to ops/events.rs, re-exported from ops (pure moves, root drops to ~600 lines)
- ops/projection.rs: introduce one fn evidence_requirement(&DeployEvidence) -> EvidenceRequirement {Planning|RunningStage(..)|Cleanup} consumed by validate_fresh_deploy_evidence, deploy_evidence_required_state, and project_deploy_event (five arms collapse to one)
- Collapse DeployProjection/CertProjection/OperationEventProjection into one OperationProjection enum; delete both adapter functions; share one kind_mismatch with ops/backup.rs; pass already-destructured fields into status_cursor/evidence_status so the unreachable!() and the silent current.clone() fallback become unrepresentable
- Add positive_u64_wire_newtype! and nonempty_text_newtype! declarative macros in src/wire.rs expanding to the EXACT current impls; apply to OperationLeaseExpiresAt, EventSequence, JoinTokenExpiresAt, JoinTokenRedeemedAt, CertValidAt, AcmeChallengeTtlSeconds and the three ops/text.rs newtypes (~250-300 lines deleted, wire format identical)
- dataplane.rs nit: store ready: WireGuardEbpfReady inside WireGuardEbpfNodeReady behind the existing Wire-struct pattern, deleting the three pass-through getters and flattening constructor

### CS6-nats-infra: ployz-nats infrastructure dedup: one timeout wrapper, one KV scan pipeline, canonical name constants

**Effort:** medium — **Order:** independent of CS3-CS5 (different crates); must precede CS7 (shared status_store.rs) — **Risk:** Low. Verification: ployz-nats unit tests + the *_nats integration suites (core_state_nats, observations_nats, operations_nats) against a real NATS server; error-message text changes are not asserted by tests.

13 copies of the same two-line timeout wrapper with 8 identical 10-second constants; the Put/Delete/Purge fold duplicated 6x and the full scan→decode→key-verify→sort pipeline 7x; bootstrap manifest() re-stringifies bucket/stream names whose constants it already imports.

- Add crate-level with_io_timeout + one NATS_IO_TIMEOUT constant in kv.rs; convert the 13 wrappers and 8 constants (kv.rs, observations.rs, core_state.rs, core_state/active_machine.rs, core_state/active_route.rs, objects.rs, bootstrap/assurance.rs, operations/events.rs, operations/status_store.rs) via From<NatsIoTimeout> impls per error enum
- Make bounded_bucket_key_scan_entries_with_prefix return only current (Put) entries — the six identical folds vanish; add one generic list_current<T> (decode + canonical-key verify + sort) replacing the 7 hand-rolled list pipelines; collapse the per-type Corrupt*Key variants into one CorruptKey { key, actual_key } shape — which also fixes the verified bug where active_service reports the same id as both expected and actual (core_state/active_service.rs:183-199)
- Merge active_route's Read/Write error enum split (status_store proves one enum suffices), deleting the duplicate decode_active_route_state_for_write and the paired timeout wrappers
- Use the existing KV_*/PLZ_* constants in bootstrap.rs manifest(); add NODE_CONTAINER_OBSERVATION_PREFIX to ployz-core/src/state.rs next to its siblings and use it in observations.rs

### CS7-nats-ops: ployz-nats operations layer: generic record helpers in status_store, repository delegation and trim

**Effort:** medium — **Order:** after CS6-nats-infra (shared status_store.rs); must precede CS8 (shared submission.rs/repository.rs) — **Risk:** Medium — generic helpers must reproduce the create-conflict re-read semantics exactly. Verification: tests/operations_nats/ (submission, evidence, dedupe against real JetStream) is the behavioral pin; plus the full workspace suite and DinD e2e.

status_store.rs (1002 lines, verified) is two patterns written ~7x and ~6x; record_deploy_event duplicates the back half of record_operation_event_with_validator line-for-line; projection.rs re-implements OperationStatus accessors that already exist in ployz-core/src/ops/accessors.rs; eight repository methods are verbatim one-line delegations to status_store; operation_api_client re-implements service_runtime's request pipeline and failure enum.

- status_store.rs: add private get_record<T> and create_or_adopt<T> (with AdoptPolicy::{AdoptExisting, RequireEqual}) helpers; every public method becomes 3-5 lines (~350-450 lines deleted, file drops well under 1k); merge the three byte-identical Stored*Submission structs into one with type aliases
- repository.rs: give record_operation_event_with_validator a pre-projection seam (enum PreCheck::{None, DeployEvidence(..)}) and have record_deploy_event delegate — one place appends and projects events
- Delete projection.rs status_id/status_sequence; call status.id()/status.last_event_sequence() at the ~10 call sites; move next_event_sequence onto OperationStatus in ployz-core if that empties the file
- Replace the boolean plan_mismatch in StoredEventMismatch with StoredEventMismatchKind { Generic, DeployPlan }; make RecordOperationEventError the single public error with the three per-flavor enums becoming aliases (the lifecycle aliases at repository.rs:752-754 already prove this works), deleting ~110 lines of hand-written converters
- Expose the status_store from the repository (records() accessor) and delete the eight pure pass-through wrapper methods
- Build OperationApiClient::request_api on service_runtime::request_json; delete OperationApiRequestFailure and its duplicate mapper in favor of NatsServiceRequestFailure

### CS8-submit-error: Unify the operation-submit flow and failure model across ployz-nats, ployz-sdk-types, and ployzd

**Effort:** medium — **Order:** after CS7-nats-ops (shared submission.rs/repository.rs) and CS3 (shared sdk-types surface) — **Risk:** Highest in the plan: serialized error JSON is a client-visible contract. Verification: assert serialized error JSON unchanged via ployz-sdk-types exports test and ployz-nats wire-level tests BEFORE landing; operations_nats submission suite; gated DinD e2e; ployzctl machine_cli_contract/api_client_nats tests.

The submit-error model is written four times across three crates (verified by two reviewers independently): three byte-identical sdk UnavailableSource enums, four near-identical repo Submit*Error enums, three structurally identical 30-line mappers in operation_api.rs, three one-line lease_claim wrappers in controllers.rs. submit_deploy/submit_cert/submit_backup are the same ~100-line idempotent-accept algorithm written three times. Every future operation kind currently adds a full new column.

- ployz-nats/src/operations/repository/submission.rs: extract one submit_operation parameterized by a small per-kind adapter (submitted-event constructor, duplicate-re-read matcher, accepted-status constructor); submit_machine_add wraps the shared core with its join-token indexing; one SubmitOperationError replaces three of the four enums (machine-add keeps a thin extension), ~250-300 lines deleted
- ployz-sdk-types/src/lib.rs: one shared OperationSubmitUnavailableSource (serde representation identical to today's three enums — wire JSON preserved) replacing DeploySubmitUnavailableSource/BackupCreateUnavailableSource and the core of MachineAddUnavailableSource; share the MachineJoinRedeem/MachineJoinReport source core
- ployzd/src/operation_api.rs: the three mapping functions collapse to one; the two machine-add unavailable-source mappers collapse to one; extract one machine_add_state_conflict accessor consumed by both completed_machine_add_operation_id and machine_join_report_error (deleting the duplicated five-level destructure)
- ployzd/src/controllers.rs: delete two of the three lease_claim wrappers; move the Clock error variant ployz-nats never constructs into ployzd-owned error types

### CS9-opapi-split: Split operation_api.rs (1533 lines) into a module and make the layers honest

**Effort:** medium — **Order:** after CS8-submit-error (error_map.rs contents shrink first; shared operation_api.rs/controllers.rs) — **Risk:** Low-medium — mostly moves; the activation-ownership move is internal rewiring with identical NATS-visible behavior. Verification: ployzd tests (control_runtime, machine_add_mint, api_client_nats, deploy_operation) + DinD e2e first-node/join path.

Verified at 1533 lines doing five jobs. After CS8 deletes most of the error-translation forest, the split is pure file moves. Also fixes two layering lies: MachineQueryRuntime (the read side) performs cluster-truth writes, and init_first_node_activate runs a 30-second polling workflow inside a request handler.

- Convert ployzd/src/operation_api.rs to operation_api/{queries.rs, submit.rs, machine_join.rs, first_node.rs, error_map.rs}; unit tests move with their functions (pure moves)
- Move machine activation (activate_reported_machine) off MachineQueryRuntime onto OperationControllers/machine_join.rs as record-then-activate; the query runtime loses its write method and the first_node_active_machine field reach-through, becoming genuinely read-only
- Extract the init_first_node_activate redeem-wait/seed-write/report workflow into first_node.rs so the handler is a thin trigger (option (b) only — moving it into the mint worker changes the handler's behavior shape and is out of scope for this pinned-behavior pass)
- controllers.rs: delete the three Accepted* twin structs in favor of the repository's already-public submitted types (they already leak through other signatures); strip the twelve pure-delegate methods via a repository accessor, leaving an honest ~250-line struct owning lease policy + owner id + join-token issuance
- Delete the five identity-wrapper free functions (machine_list/machine_inspect/service_list/service_inspect/logs_tail) and call runtime methods directly from api_runtime.rs closures; keep the endpoint match itself (table-driving it would need the type-level subject language AGENTS.md forbids)

### CS10-node-seam: Unify the node RPC seam: one envelope, one request model, one node/ module

**Effort:** large — **Order:** after CS2-dead-rust (node_runtime.rs/observer.rs/StartedContainerUnhealthy deletions land first) and after CS4 (test-support's node.rs move); parallel-safe with CS6-CS8 (different files) — **Risk:** Medium — touches the deploy↔node wire path, but request/response JSON shapes are unchanged (node_id moves from payload struct to subject parameter only on the in-process port, not the wire). Verification: ployzd node_rpc/node_service_runtime/node_runtime/role_process test suites + DinD e2e deploy path.

The node surface models everything twice across the seam: a 6x-copied request/validate/unwrap envelope with the responder check duplicated in both match arms, a 3x byte-identical transport-error mapping, and node_runtime_types.rs whose five request structs differ from node_protocol.rs twins only by a node_id field that is routing, not payload (bridged by five field-copying From impls). The node_* file family carves at accretion order — 'node_agent' contains no agent.

- Add one generic NodeRpcResponse<T, E> envelope in node_protocol.rs and one call_node helper doing request_json + responder validation + the NatsJsonServiceRequestError mapping exactly once; the five per-endpoint response enums, eight wrong_responder checks, and two of three error-mapping functions disappear; node_rpc.rs drops from 779 to ~300 lines
- Delete the request half of node_runtime_types.rs and all five From impls; the NodeContainerRuntime port (deploy_worker/ports.rs) takes (node_id: &NodeId, request: NodeXxxRpcRequest); remove the deploy_worker.rs re-export seam
- Fold the family into a node/ directory: node/protocol.rs (wire types + envelope), node/service.rs (server handlers), node/client.rs (NATS adapters), node/runner.rs (NodeContainerRunner trait + decide_container_run, absorbing node_agent/), node/process.rs (process runtime + observer loop) — pure moves with re-exports
- node_service_runtime: one bind helper replacing the six 10-line clone-dance blocks; merge the twin created/existing container start-error mappers into one function taking the variant constructor

### CS11-worker-dedup: Deploy/backup worker dedup: delete the facts.rs mirror enums, single-writer failure recording, one TaskRegistry, one lease protocol

**Effort:** medium — **Order:** after CS10-node-seam (shared deploy_worker files) and CS6 (no file overlap but failure-message rendering should land on final nats error Display text) — **Risk:** Medium — failure-recording consolidation must preserve which transition gets recorded for each failure path; deploy_operation and backup_restore suites pin this. Verification: ployzd deploy_operation, deploy_runtime_nats, backup_restore, role_process suites + DinD e2e.

facts.rs re-states four ployz-nats store-error enums variant-for-variant (~350 lines) only for deploy_runtime to flatten everything to a static string. IMPORTANT CORRECTION to the reviewer's primary suggestion: verified CoreStateStoreError contains Encode(serde_json::Error) which is not Clone/PartialEq, so use the reviewer's stated fallback — carry the rendered message String in DeployFactLoadError. Plus verbatim-triplicated TaskRegistry, a twice-written lease renew/verify protocol, and backup_runtime's dual failure-recording scheme.

- deploy_worker/facts.rs: replace the four mirror enums, four mappers, and four Display impls with variants carrying the rendered source message (ployz-nats errors already implement Display) — ~330 lines deleted and operators get strictly more detail than today's static strings in deploy_runtime.rs fact_load_failure_message
- deploy_worker.rs: collapse the five identical record_* evidence wrappers into one record_evidence(command, recorder, evidence); introduce a small DeployRun struct carrying the failure-mapping internally so execute_deploy reads as its seven stages; delete failure.rs's failure/failure_with_stop_targets closures
- One pub TaskRegistry in ployzd (e.g. src/tasks.rs) replacing the three character-identical copies in deploy_runtime.rs, backup_runtime.rs, nats_authorization/tasks.rs; control_runtime keeps distinct field names
- Move one renew_verified_owner_lease into operation_lease.rs (the canonical lease home); both runtimes wrap its single typed error, deleting ~80 lines and one parallel enum
- backup_runtime.rs: make BackupExecutionError variants carry their evidence message; delete the four inline record-failure helpers so the start() catch-all is the single writer of Failed transitions; collapse the three record-or-fail wrappers into one; move BackupRestoreRuntime into backup_restore.rs (file drops under 800 lines)

### CS12-proc-dataplane: Process-runtime scaffolding, lazy Docker runner deletion, honest dataplane provisioning model

**Effort:** medium — **Order:** after CS10-node-seam and CS11 (shared node_process_runtime.rs/control_runtime.rs) — **Risk:** Medium on dataplane_runtime — host provisioning order must be byte-identical. Verification: gated wireguard_dataplane test + local-dataplane-proof.sh Layer A + ployzd role_process/gateway_process_runtime suites; the docker change is pinned by node_service_runtime and DinD e2e.

wait_for_shutdown_signal x3, backoff+health recording x2, lazy-store double-expect x2; LazyLocalDockerManagedContainerRunner is a 160-line connect-then-delegate wrapper that hardcodes '10.42.1.0/24'/'br-ployz' in its list path instead of using its own constructor config; HostDataplaneRequirement::check reads as a probe but mutates the host (wg genkey, ip link add, sysctl, iptables) and re-provisions on every public-key read, with test-only modes baked into the production enum.

- Add a small ployzd process-support module: shutdown_signal(), BackoffSchedule { interval, cap }, a generic attempt/health recorder, LazyHandle<T>.get_or_open(); node_process_runtime, gateway_process_runtime, control_runtime compose from it (~150 lines, three subtly-divergent copies become one)
- Delete LazyLocalDockerManagedContainerRunner (docker/runner.rs:38-196): give DockerManagedContainerRunner an internal lazily-connecting docker() over tokio OnceCell; the duplicate trait impls, both connect_* helpers, and the hardcoded magic network values disappear; node_process_runtime constructs the real runner (runner.rs drops to ~830 lines)
- dataplane_runtime.rs: rename the requirement model to an explicit HostCommandPlan with ProvisioningStep vs ReadinessCheck purposes so 'check' stops lying and the public-key read provisions explicitly once; replace the three telescoping constructors with one config struct; extract one run_host_command(program, args, timeout) used by both duplicated call sites; move the Static('test-public-key')/with_requirements test seams to a #[cfg(test)] fake (pattern already exists as ReadyWireGuardEbpf)
- config.rs: add env_value(env, key) for the 14x env+filter idiom; resolve the build-then-flatten NodeProcessArtifacts/NodeDataplaneConfig bundles (keep as real nested fields or delete — not both)

### CS13-keeper: Keeper: collapse the five artifact-target clones and split local.rs into effects/fsx/command

**Effort:** medium — **Order:** after CS3-install-model (artifact type vocabulary changes first) and CS4 (keeper test fixtures); parallel-safe with CS14 (different crates) — **Risk:** Low-medium — install-plan rendering is pinned by keeper's bootstrap/local/systemd suites which assert exact step plans and file modes. Verification: ployz-keeper full test suite + ployzctl cli_contract (keeper subprocess path) + DinD e2e machine image build.

Five byte-identical artifact-target structs re-flattened through three 5-arm matches and five From impls, forcing triplicated 12-line conversion blocks in main.rs and cli.rs; local.rs (verified 1016 lines) fuses step effects with a generic durable-file library and a command runner, and the staged-temp-path 0..16 retry machinery is implemented twice within this crate alone.

- artifacts.rs: one ArtifactTarget { kind: ArtifactKind, version, source, digest, install_path } replacing the five structs, five From impls, and three 5-arm projection matches; one fn artifact_target(kind, &InstallArtifactSpec) conversion used by main.rs:393-429 and cli.rs:190-213 (~250 lines deleted)
- Split local.rs into local.rs (step effects, ~300 lines), fsx.rs (staged/durable writes, one implementation parameterized by FileMode { Plain, Secret0600 } replacing the fn-pointer-threaded pairs), command.rs (subprocess runner); delete the identity wrapper unique_staged_file_path; make artifacts.rs create_staged_artifact use the fsx primitive (its bespoke StagedArtifact disappears)
- steps.rs line_value(): pass the empty-error variant (or a small field enum) instead of dispatching on the magic string label 'cluster name'

### CS14-ctl: ployzctl: split runtime.rs, trust clap, dedup the dispatch

**Effort:** medium — **Order:** after CS3-install-model (FirstNodeInitMode carries the nested spec) and CS4; parallel-safe with CS13 (different crates) — **Risk:** Low — CLI surface is pinned by cli_contract.rs (1115 lines of exact argument/help/exit assertions) and machine_cli_contract/machine_add_binary_nats. Verification: those suites plus DinD e2e init path.

runtime.rs (verified 1078 lines) fuses 12 near-identical command-dispatch arms with a complete 600-line subprocess-capture library; help is smuggled through a fake PloyzctlCommand::Help(String) built from a throwaway Command; init_command re-implements four constraints clap already enforces, including a nested re-match with unreachable!() — a direct 'make invalid states unrepresentable' violation per AGENTS.md.

- Extract the keeper subprocess machinery (run_keeper_first_node_install, OutputCapture, CapturedFile, LimitedOutput, LocalKeeperInstallError) into keeper_install.rs — runtime.rs drops to ~450 lines; if CS13's fsx primitive is exposed, CapturedFile reuses it instead of being the third staged-temp-path copy
- Collapse the 12 dispatch arms via one generic call helper or per-command execute(self, api) methods; replace the 20-arm is_first_node_activation_retryable match with retryability declared on the one variant that can be retryable; collapse the 11 bare-{source} Display arms
- Set arg_required_else_help on InvocationCli, deleting help_text(), the throwaway Command build, and PloyzctlCommand::Help(String); keep clap::Error typed (the keeper's shape) so help/exit-code policy is one match on error.kind(), consistent across the three binaries
- init.rs: delete the four imperative checks clap already enforces; use args_conflicts_with_subcommands like the keeper (deleting InitCli::has_values()); restructure as one match over a 3-variant mode so the inner re-match and its unreachable!() disappear
- Make watch_operation_until_terminal the single pagination implementation (the dead OperationHandle twin was deleted in CS2)

## Deliberately dropped

- facts.rs 'derive Clone + PartialEq on ployz-nats store errors' (the reviewer's PRIMARY suggestion): verified infeasible — CoreStateStoreError::Encode(serde_json::Error) is not Clone/PartialEq; the stated fallback (carry rendered message) is what CS11 plans instead.
- init_first_node_activate option (a) — moving the redeem-wait into the mint worker: a behavior-shape change to a request handler's contract (handler would return before material exists), which the pinned-behavior constraint forbids in this pass; option (b) extraction is folded into CS9. Revisit as a deliberate product change with e2e coordination.
- Full generic commit_guarded<S> CAS helper unifying active_route/active_service (NATS reviewer, marked large): borders on the generic-engine territory AGENTS.md forbids and is the riskiest proposal in the NATS findings; the minimal, safe portion (merge active_route's Read/Write error split, dedup decode/timeout) is captured in CS6. The two pure classifiers staying separate is acceptable verbosity-by-design.
- objects.rs 'unknown' digest sentinel → typed MissingDigest error: a real latent bug, but fixing it changes behavior (write fails instead of storing a sentinel) and no test pins that path — out of scope for a no-behavior-change pass; file as a standalone bug-fix PR.
- NatsClientUrl vs MachineJoinRuntimeNatsUrl unification: strengthening NatsClientUrl's validation to host:port rules would reject inputs it accepts today — a behavior change at the edges; defer.
- KeeperStepLabel 14-variant mirror of KeeperStep: largely style-not-rot — the label is a deliberate redaction projection plus three genuine join pseudo-steps, so it is not a pure mirror; consolidation is medium effort for nit-level gain and risks churning the report/event text the keeper tests assert.
- is_schedulable as future enum modeling: the deletion of the constant booleans is planned (CS2), but the reviewers' suggestion to pre-model a schedulability enum now is itself the speculative-variant mistake AGENTS.md warns against — add it when the operation exists.
- Table-driving api_runtime.rs's 117-line endpoint match: correctly rejected by the reviewer themselves — it would require the complex type-level subject language AGENTS.md explicitly forbids; only the pass-through hop removal survives (CS9).
- OperationSubject full deletion in favor of deriving the SDK shape from OperationStatus: only the two speculative variants are deleted (CS2); the enum itself is a public SDK contract with TS exports and removing it is an API decision for the owner, not a rot fix.
- The 'verbose defensive style' itself (typed ids everywhere, exhaustive matches, destructure-in-impls, per-domain error enums with evidence): explicitly NOT rot — it is the codebase's deliberate contract per AGENTS.md and is what keeps the 83 suites trustworthy as a behavior pin; no change-set weakens it.
