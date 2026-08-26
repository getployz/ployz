//! Pipeline tests for `@ployz/sdk` generated TypeScript and JSON fixtures.

use std::collections::BTreeMap;

use ployz_core::{
    CERTIFICATE_POLICY_CAPABILITY, CertificateAvailability, CertificateFailureKind,
    ClusterTeardown, ContainerObservation, ContainerRuntimeObservation, ContractDescription,
    CreateVolumeReport, DESCRIBE_CONTRACT_CAPABILITY, DataLoss, DataLossConfirmation, DeployIntent,
    DeployOperation, DeployOutcome, DeployPreview, DockerVolume, DockerVolumeStorageObservation,
    ExecutionError, HealthObservation, HealthcheckSpec, IngressHostname, LocalMachineRemoved,
    MembershipObservation, ObservedDataLoss, PlanOptions, RUNTIME_WATCH_CAPABILITY,
    RequestedServiceSpec, ResolvedServiceSpec, RpcError, RpcErrorCode, RuntimeWatchFrame,
    ServiceAttempt, StorageChoice, UnconfirmedDataLoss, VolumeInventory, VolumeSource,
};
use ployz_sdk_payloads::{
    PACKAGE_NAME, decode_fixture, drift, fixtures, sdk_package_root, write_generated,
};
use serde_json::Value;

fn fixture<'a>(fixtures: &'a BTreeMap<String, Value>, name: &str) -> &'a Value {
    fixtures
        .get(name)
        .unwrap_or_else(|| panic!("missing fixture {name}"))
}

fn pkg_field<'a>(pkg: &'a Value, name: &str) -> &'a Value {
    pkg.get(name)
        .unwrap_or_else(|| panic!("missing package.json field {name}"))
}

#[test]
fn npm_package_identity_matches_the_napi_crate() {
    let pkg: Value = serde_json::from_str(include_str!("../../ployz-sdk/package.json")).unwrap();
    assert_eq!(pkg_field(&pkg, "name"), PACKAGE_NAME);
    assert_eq!(pkg_field(&pkg, "name"), "@ployz/sdk");
    assert_eq!(pkg_field(&pkg, "version"), env!("CARGO_PKG_VERSION"));
    assert_eq!(pkg_field(&pkg, "main"), "index.js");
    assert_eq!(pkg_field(&pkg, "types"), "index.d.ts");
    assert_ne!(pkg.get("private"), Some(&Value::Bool(true)));
    assert_eq!(pkg_field(&pkg, "publishConfig")["access"], "public");
    assert!(
        pkg_field(&pkg, "devDependencies")
            .get("typescript")
            .and_then(Value::as_str)
            .is_some(),
        "TypeScript must be an @ployz/sdk dev dependency"
    );
}

#[test]
fn workspace_forbids_unsafe_outside_the_napi_crate() {
    let workspace = include_str!("../../Cargo.toml");
    assert!(workspace.contains("unsafe_code = \"forbid\""));
    assert!(!workspace.contains("unsafe_code = \"deny\""));
    assert!(!workspace.contains("unsafe_code = \"allow\""));
    let sdk_manifest = include_str!("../../ployz-sdk/Cargo.toml");
    assert!(sdk_manifest.contains("unsafe_code = \"allow\""));
    assert!(
        !sdk_manifest
            .lines()
            .any(|line| line.trim() == "workspace = true"),
        "ployz-sdk must not inherit workspace lints"
    );
    let payloads = include_str!("../src/lib.rs");
    assert!(!payloads.contains("allow(unsafe_code)"));
}

#[test]
fn generated_artifacts_match_checked_in_files() {
    if let Some(drift) = drift(&sdk_package_root()) {
        panic!("{drift}");
    }
}

#[test]
fn json_fixtures_round_trip_through_rust_types() {
    let fixtures = fixtures();
    let description: ContractDescription =
        decode_fixture(fixture(&fixtures, "contract_description"));
    assert!(description.supports(DESCRIBE_CONTRACT_CAPABILITY));
    assert_eq!(
        serde_json::to_value(&description).unwrap(),
        *fixture(&fixtures, "contract_description")
    );

    let error: RpcError = decode_fixture(fixture(&fixtures, "rpc_error"));
    assert_eq!(error.code.as_str(), "unsupported");
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        *fixture(&fixtures, "rpc_error")
    );

    let volume: DockerVolume = decode_fixture(fixture(&fixtures, "docker_volume"));
    assert_eq!(volume.driver(), "ployz");
    assert!(matches!(
        volume.storage,
        DockerVolumeStorageObservation::Provisioned {
            bound_bytes,
            used_bytes: 966_367_642,
            ..
        } if bound_bytes.get() == 1_073_741_824
    ));
    assert_eq!(
        serde_json::to_value(&volume).unwrap(),
        *fixture(&fixtures, "docker_volume")
    );

    let inventory: VolumeInventory = decode_fixture(fixture(&fixtures, "volume_inventory"));
    assert_eq!(inventory.volumes.len(), 1);
    assert_eq!(inventory.failures.len(), 1);
    assert_eq!(
        inventory
            .failures
            .first()
            .expect("fixture includes one failure")
            .id
            .name
            .as_str(),
        "unavailable"
    );
    let verified: CreateVolumeReport =
        decode_fixture(fixture(&fixtures, "create_volume_report_verified"));
    assert!(matches!(verified, CreateVolumeReport::Verified { .. }));
    let unverified: CreateVolumeReport =
        decode_fixture(fixture(&fixtures, "create_volume_report_unverified"));
    assert!(matches!(
        unverified,
        CreateVolumeReport::Unverified { id, .. } if id.name.as_str() == "data"
    ));

    let remove: ployz_core::RemoveVolumesRequest =
        decode_fixture(fixture(&fixtures, "remove_volumes_request"));
    assert_eq!(remove.volumes.len(), 1);
    assert!(!remove.force);
    assert_eq!(
        serde_json::to_value(&remove).unwrap(),
        *fixture(&fixtures, "remove_volumes_request")
    );

    let loss: DataLoss = decode_fixture(fixture(&fixtures, "data_loss"));
    assert!(matches!(loss, DataLoss::DockerVolume(_)));
    assert_eq!(
        serde_json::to_value(&loss).unwrap(),
        *fixture(&fixtures, "data_loss")
    );

    let observed: ObservedDataLoss = decode_fixture(fixture(&fixtures, "observed_data_loss"));
    assert_eq!(observed.data_loss.len(), 1);
    assert_eq!(
        serde_json::to_value(&observed).unwrap(),
        *fixture(&fixtures, "observed_data_loss")
    );
    let empty: ObservedDataLoss = decode_fixture(fixture(&fixtures, "observed_data_loss_empty"));
    assert!(empty.data_loss.is_empty());

    let confirmation: DataLossConfirmation =
        decode_fixture(fixture(&fixtures, "data_loss_confirmation"));
    assert_eq!(
        serde_json::to_value(&confirmation).unwrap(),
        *fixture(&fixtures, "data_loss_confirmation")
    );

    let unconfirmed: UnconfirmedDataLoss =
        decode_fixture(fixture(&fixtures, "unconfirmed_data_loss"));
    assert_eq!(unconfirmed.missing.len(), 1);
    assert_eq!(
        serde_json::to_value(&unconfirmed).unwrap(),
        *fixture(&fixtures, "unconfirmed_data_loss")
    );

    let removed: LocalMachineRemoved = decode_fixture(fixture(&fixtures, "local_machine_removed"));
    assert!(removed.reset_warning.is_none());
    let warned: LocalMachineRemoved =
        decode_fixture(fixture(&fixtures, "local_machine_removed_reset_warning"));
    assert_eq!(
        warned.reset_warning.as_deref(),
        Some("replicated delete failed")
    );

    let teardown: ClusterTeardown = decode_fixture(fixture(&fixtures, "cluster_teardown"));
    assert!(teardown.pairing_revoked);
    assert_eq!(teardown.destroyed_projects.len(), 1);
    assert_eq!(teardown.machines.successes.len(), 1);
    assert_eq!(teardown.machines.failures.len(), 1);

    let identity: ployz_core::RegisterRequest =
        decode_fixture(fixture(&fixtures, "register_request"));
    assert_eq!(identity.name.as_str(), "joiner");
    assert_eq!(identity.storage, StorageChoice::Zfs);
    assert_eq!(
        serde_json::to_value(&identity).unwrap(),
        *fixture(&fixtures, "register_request")
    );

    let registered: ployz_core::Registered = decode_fixture(fixture(&fixtures, "registered"));
    assert_eq!(registered.assigned_machine.name.as_str(), "edge");
    assert_eq!(registered.visible_peers.len(), 1);
    assert_eq!(registered.target_versions.get("machines"), Some(&1));
    assert_eq!(
        serde_json::to_value(&registered).unwrap(),
        *fixture(&fixtures, "registered")
    );

    let encoded = fixture(&fixtures, "partial_result");
    let successes = encoded
        .get("successes")
        .and_then(Value::as_array)
        .expect("partial_result.successes");
    let failures = encoded
        .get("failures")
        .and_then(Value::as_array)
        .expect("partial_result.failures");
    let omissions = encoded
        .get("omissions")
        .and_then(Value::as_array)
        .expect("partial_result.omissions");
    assert_eq!(successes.len(), 1);
    assert_eq!(failures.len(), 1);
    assert_eq!(omissions.len(), 1);
    let volume: DockerVolume = decode_fixture(
        successes
            .first()
            .and_then(|row| row.get("value"))
            .expect("success value"),
    );
    assert_eq!(volume.driver(), "ployz");
    let error: RpcError = decode_fixture(
        failures
            .first()
            .and_then(|row| row.get("error"))
            .expect("failure error"),
    );
    assert_eq!(error.code.as_str(), "unsupported");

    let intent: DeployIntent = decode_fixture(fixture(&fixtures, "deploy_intent"));
    assert_eq!(intent.project_name.as_str(), "app");
    assert!(intent.target.is_empty());
    assert!(intent.provisioned_volumes.is_empty());
    let provisioned: ployz_core::ProvisionedVolume =
        decode_fixture(fixture(&fixtures, "provisioned_volume"));
    assert_eq!(provisioned.service.as_str(), "api");
    assert_eq!(provisioned.reference.as_str(), "data");
    assert_eq!(provisioned.maximum_bytes.get(), 1_073_741_824);
    let operation: DeployOperation =
        decode_fixture(fixture(&fixtures, "create_provisioned_volume_operation"));
    assert!(matches!(
        operation,
        DeployOperation::CreateProvisionedVolume { maximum_bytes, .. }
            if maximum_bytes.get() == 1_073_741_824
    ));
    assert!(intent.options.selected.is_empty());
    assert_eq!(intent.options, PlanOptions::default());
    assert!(intent.dependencies().is_empty());
    assert_eq!(
        serde_json::to_value(&intent).unwrap(),
        *fixture(&fixtures, "deploy_intent")
    );

    let attempt: ServiceAttempt = decode_fixture(fixture(&fixtures, "service_attempt"));
    assert_eq!(attempt.name.as_str(), "web");

    let preview: DeployPreview = decode_fixture(fixture(&fixtures, "deploy_preview"));
    assert_eq!(preview.operations.len(), 1);
    assert_eq!(preview.warnings.len(), 5);
    assert!(matches!(
        preview.operations.first().map(|row| &row.status),
        Some(ployz_core::OperationStatus::Pending)
    ));
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        *fixture(&fixtures, "deploy_preview")
    );

    let spec: RequestedServiceSpec = decode_fixture(fixture(&fixtures, "requested_service_spec"));
    assert_eq!(spec.name.as_str(), "api");
    assert_eq!(
        serde_json::to_value(&spec).unwrap(),
        *fixture(&fixtures, "requested_service_spec")
    );

    assert_typed_spec_fixtures(&fixtures);

    let automatic: IngressHostname =
        decode_fixture(fixture(&fixtures, "ingress_hostname_cluster_domain"));
    assert_eq!(automatic, IngressHostname::cluster_domain());
    let chosen: IngressHostname =
        decode_fixture(fixture(&fixtures, "ingress_hostname_cluster_domain_label"));
    assert_eq!(
        chosen,
        IngressHostname::cluster_domain_label("api").unwrap()
    );
    let explicit: IngressHostname = decode_fixture(fixture(&fixtures, "ingress_hostname_explicit"));
    assert_eq!(
        explicit,
        IngressHostname::explicit("api.example.com").unwrap()
    );

    let event: ployz_core::DeployEvent =
        decode_fixture(fixture(&fixtures, "deploy_event_progress"));
    let ployz_core::DeployEvent::Progress {
        rows, completed, ..
    } = &event
    else {
        panic!("deploy_event_progress fixture must be Progress");
    };
    assert_eq!(*completed, 0);
    assert_eq!(rows.len(), 1);

    let outcome: DeployOutcome<ExecutionError> =
        decode_fixture(fixture(&fixtures, "deploy_outcome"));
    let DeployOutcome::Success { completed } = &outcome else {
        panic!("deploy_outcome fixture must be Success");
    };
    assert_eq!(completed.len(), 1);
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        *fixture(&fixtures, "deploy_outcome")
    );

    let failed: DeployOutcome<ExecutionError> =
        decode_fixture(fixture(&fixtures, "deploy_outcome_failed"));
    let DeployOutcome::Failed {
        completed,
        failed: failed_op,
        unexecuted,
    } = &failed
    else {
        panic!("deploy_outcome_failed fixture must be Failed");
    };
    assert!(completed.is_empty());
    assert_eq!(unexecuted.len(), 1);
    let ployz_core::FailedOperation::Operation { error, .. } = failed_op else {
        panic!("deploy_outcome_failed fixture must wrap an operation error");
    };
    assert!(matches!(error, ExecutionError::Machine { .. }));
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        *fixture(&fixtures, "deploy_outcome_failed")
    );

    let frame = decode_fixture::<RuntimeWatchFrame>(fixture(&fixtures, "runtime_watch_frame"));
    assert_eq!(frame.observed_at, "2024-01-01T00:00:00Z");
    assert_eq!(
        frame
            .hosted_dns_hostname
            .as_deref()
            .expect("hosted DNS hostname"),
        "cluster.example.ts.net"
    );
    assert_eq!(frame.machines.len(), 1);
    assert_eq!(frame.containers.len(), 1);
    let container = frame
        .containers
        .first()
        .expect("runtime_watch_frame fixture has one Container");
    assert_eq!(container.project_name.as_str(), "app");
    assert_eq!(container.service_name.as_str(), "api");
    assert_eq!(frame.services.len(), 1);
    assert_eq!(frame.certificates.len(), 2);
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        *fixture(&fixtures, "runtime_watch_frame")
    );
    let text = fixture(&fixtures, "runtime_watch_frame").to_string();
    for forbidden in [
        "BEGIN CERTIFICATE",
        "BEGIN PRIVATE KEY",
        "private_key",
        "challenge_token",
        "challenge_response",
        "renewal_token",
        "dns_endpoint",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} must not appear on the Watch frame fixture"
        );
    }
}

#[test]
fn unknown_fields_are_accepted_on_public_payloads() {
    let fixtures = fixtures();
    let description: ContractDescription =
        decode_fixture(fixture(&fixtures, "contract_description_unknown_fields"));
    assert!(description.supports(DESCRIBE_CONTRACT_CAPABILITY));

    let volume: DockerVolume = decode_fixture(fixture(&fixtures, "docker_volume_unknown_fields"));
    assert_eq!(
        serde_json::to_value(&volume).unwrap(),
        *fixture(&fixtures, "docker_volume")
    );

    let outcome: DeployOutcome<ExecutionError> =
        decode_fixture(fixture(&fixtures, "deploy_outcome_unknown_fields"));
    let DeployOutcome::Success { .. } = &outcome else {
        panic!("deploy_outcome_unknown_fields must decode as Success");
    };
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        *fixture(&fixtures, "deploy_outcome")
    );

    let preview: DeployPreview =
        decode_fixture(fixture(&fixtures, "deploy_preview_unknown_fields"));
    assert_eq!(preview.operations.len(), 1);
    assert_eq!(
        serde_json::to_value(&preview).unwrap(),
        *fixture(&fixtures, "deploy_preview")
    );

    let frame = decode_fixture::<RuntimeWatchFrame>(fixture(
        &fixtures,
        "runtime_watch_frame_unknown_fields",
    ));
    assert_eq!(
        serde_json::to_value(&frame).unwrap(),
        *fixture(&fixtures, "runtime_watch_frame")
    );
}

#[test]
fn observation_enums_keep_an_unknown_case() {
    let fixtures = fixtures();
    let membership: MembershipObservation =
        decode_fixture(fixture(&fixtures, "membership_observation_unknown"));
    assert_eq!(membership.as_str(), "future_membership");
    assert_eq!(
        serde_json::to_value(&membership).unwrap(),
        *fixture(&fixtures, "membership_observation_unknown")
    );

    let health: HealthObservation =
        decode_fixture(fixture(&fixtures, "health_observation_unknown"));
    assert_eq!(health, HealthObservation::Unrecognized("degraded".into()));

    let unknown_json = fixture(&fixtures, "container_runtime_unknown");
    let unknown: ContainerRuntimeObservation = decode_fixture(unknown_json);
    assert_eq!(
        unknown,
        ContainerRuntimeObservation::Unknown {
            raw: unknown_json.clone()
        }
    );
    assert_eq!(serde_json::to_value(&unknown).unwrap(), *unknown_json);

    let known: ContainerRuntimeObservation =
        decode_fixture(fixture(&fixtures, "container_runtime_known_unknown_fields"));
    assert_eq!(
        known,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy
        }
    );

    let availability: CertificateAvailability =
        decode_fixture(fixture(&fixtures, "certificate_availability_unknown"));
    assert_eq!(
        availability,
        CertificateAvailability::Unrecognized("renewing".into())
    );
    let kind: CertificateFailureKind =
        decode_fixture(fixture(&fixtures, "certificate_failure_kind_unknown"));
    assert_eq!(
        kind,
        CertificateFailureKind::Unrecognized("rate_limited".into())
    );
}

#[test]
fn generated_typescript_encodes_additive_evolution_rules() {
    let dts = ployz_sdk_payloads::artifacts().payloads_dts;
    assert!(dts.contains("export type Additive<T extends object> = T & JsonObject;"));
    assert!(dts.contains("export type MembershipObservation ="));
    assert!(dts.contains("| (string & {});"));
    assert!(dts.contains("export type ContainerRuntimeObservation ="));
    assert!(dts.contains("state?: string"));
    assert!(dts.contains("export type DockerVolume = Additive<{"));
    assert!(dts.contains("export type DockerVolumeStorageObservation ="));
    assert!(dts.contains("kind: \"plain\"; driver: string"));
    assert!(dts.contains(
        "kind: \"provisioned\"; mountpoint: MachinePath; bound_bytes: number; used_bytes: number"
    ));
    assert!(dts.contains("storage: DockerVolumeStorageObservation"));
    assert!(dts.contains("export type DataLoss ="));
    assert!(dts.contains("DockerVolume: DockerVolumeId"));
    assert!(dts.contains("export type ObservedDataLoss = Additive<{"));
    assert!(dts.contains("data_loss: DataLoss[]"));
    assert!(dts.contains("export type DataLossConfirmation = Additive<{"));
    assert!(dts.contains("confirmed: DataLoss[]"));
    assert!(dts.contains("export type UnconfirmedDataLoss = Additive<{"));
    assert!(dts.contains("missing: DataLoss[]"));
    assert!(dts.contains("export type LocalMachineRemoved = Additive<{"));
    assert!(dts.contains("reset_warning?: string"));
    assert!(dts.contains("export type ClusterTeardown = Additive<{"));
    assert!(dts.contains("destroyed_projects: ProjectName[]"));
    assert!(dts.contains("machines: PartialResult<LocalMachineRemoved, RpcError>"));
    assert!(dts.contains("pairing_revoked: boolean"));
    assert!(dts.contains("export type MachineTarget = string"));
    assert!(dts.contains("export type ContractDescription = Additive<{"));
    assert!(dts.contains("readonly __brand: \"ProjectName\""));
    assert!(dts.contains("export type QualifiedService = string"));
    assert!(dts.contains("identity: QualifiedService"));
    assert!(dts.contains("export type DeployIntent = Additive<{"));
    assert!(dts.contains("project_name: ProjectName"));
    assert!(dts.contains("target: RequestedServiceSpec[]"));
    assert!(dts.contains("export type ProvisionedVolume = Additive<{"));
    assert!(dts.contains("service: ServiceName"));
    assert!(dts.contains("reference: ServiceVolumeReference"));
    assert!(dts.contains("export type ProvisionedVolumeMaximumBytes = string"));
    assert!(dts.contains("maximum_bytes: ProvisionedVolumeMaximumBytes"));
    assert!(dts.contains("provisioned_volumes: ProvisionedVolume[]"));
    assert!(dts.contains("export type RequestedServiceSpec = Additive<{"));
    assert!(dts.contains("export type ResolvedServiceSpec = Additive<{"));
    assert!(dts.contains("export type IngressProxyFragment ="));
    assert!(dts.contains("backend: \"caddy\"; config: string"));
    assert!(dts.contains("export type IngressProxyConfig ="));
    assert!(dts.contains("backend: \"zentinel\"; config: string"));
    assert!(dts.contains("backend: \"envoy\"; config: string"));
    assert!(dts.contains(
        "GET_INGRESS_PROXY_CONFIG_CAPABILITY: CapabilityName = \"ployz.ingress.config.v1\""
    ));
    assert!(!dts.contains("GET_CADDY_CONFIG_CAPABILITY"));
    assert!(dts.contains("ingress_proxy_fragment?: IngressProxyFragment"));
    assert!(!dts.contains("caddy_config?: string"));
    assert!(dts.contains("export type ServiceVolume = Additive<{"));
    assert!(dts.contains("export type VolumeDriver = Additive<{"));
    assert!(dts.contains("driver?: VolumeDriver"));
    assert!(dts.contains("export type ConfigSpec = Additive<{"));
    assert!(dts.contains("content?: number[]"));
    assert!(dts.contains("configs?: ConfigSpec[]"));
    assert!(dts.contains("export type ConfigMount = Additive<{"));
    assert!(dts.contains("config_mounts?: ConfigMount[]"));
    assert!(dts.contains("export type DeviceMapping = Additive<{"));
    assert!(dts.contains("devices?: DeviceMapping[]"));
    assert!(dts.contains("export type DeviceReservation = Additive<{"));
    assert!(dts.contains("device_reservations?: DeviceReservation[]"));
    assert!(dts.contains("export type Ulimit = Additive<{"));
    assert!(dts.contains("ulimits?: { readonly [key: string]: Ulimit }"));
    assert!(dts.contains("effective_healthcheck: HealthcheckSpec | null"));
    assert!(dts.contains("details?: JsonValue;"));
    assert!(!dts.contains("export type RequestedServiceSpec = JsonValue"));
    assert!(!dts.contains("export type ResolvedServiceSpec = JsonValue"));
    assert!(dts.contains("readonly __brand: \"MachineId\""));
    assert!(dts.contains("readonly __brand: \"ServiceId\""));
    assert!(dts.contains("readonly __brand: \"ContainerId\""));
    assert!(dts.contains("readonly __brand: \"ServiceName\""));
    assert!(dts.contains("export type ObservationKind ="));
    assert!(dts.contains("export type DeployWarning ="));
    assert!(dts.contains("SkippedDependencyHealth:"));
    assert!(dts.contains("export type DeployPreview = Additive<{"));
    assert!(dts.contains("operations: OperationRow[]"));
    assert!(dts.contains("export type OperationRow = Additive<{"));
    assert!(dts.contains("export type DeployEvent ="));
    assert!(dts.contains("export type OperationStatus ="));
    assert!(dts.contains("export type OperationPhase ="));
    assert!(dts.contains("type: \"waiting_for_health\""));
    assert!(dts.contains("elapsed_ms: number"));
    assert!(dts.contains("deadline_ms: number"));
    assert!(dts.contains("warnings: DeployWarning[]"));
    assert!(dts.contains("would_remove: QualifiedService[]"));
    assert!(dts.contains("preserved_volumes: PreservedVolume[]"));
    assert!(dts.contains("prune_refusal?: PruneRefusal"));
    assert!(dts.contains("export type PruneRefusal ="));
    assert!(dts.contains("selected: ServiceAttempt[]"));
    assert!(dts.contains("export type DeployOperation ="));
    assert!(dts.contains("type: \"run_container\""));
    assert!(dts.contains("type: \"wait_healthy\""));
    assert!(dts.contains(
        "type: \"create_provisioned_volume\"; machine_id: MachineId; volume: ServiceVolume; maximum_bytes: ProvisionedVolumeMaximumBytes"
    ));
    assert!(dts.contains("export type FailedOperation<E = ExecutionError> ="));
    assert!(dts.contains("export type DeployOutcome<E = ExecutionError> ="));
    assert!(dts.contains("export type ExecutionError ="));
    assert!(dts.contains("export type MachineAction ="));
    assert!(dts.contains("export type HealthFailure ="));
    assert!(dts.contains("export type DependencyHealthFailure ="));
    assert!(dts.contains("export type HookFailure ="));
    assert!(dts.contains("type: \"success\""));
    assert!(dts.contains("type: \"failed\""));
    assert!(!dts.contains("Success: { completed: DeployOperation[] }"));
    assert!(dts.contains("unexecuted: DeployOperation[]"));
    assert!(dts.contains("failed: FailedOperation<E>"));
    assert!(dts.contains("export type RuntimeWatchFrame = Additive<{"));
    assert!(dts.contains("incomplete_ids: RuntimeWatchIncompleteIds"));
    assert!(dts.contains("hosted_dns_hostname?: string"));
    assert!(dts.contains("export type Machine = Additive<{"));
    assert!(dts.contains("export type StorageChoice = \"none\" | \"zfs\";"));
    assert!(dts.contains("export type MachineStorageObservation ="));
    assert!(
        dts.contains("state: \"pool\"; size_bytes: number; used_bytes: number; free_bytes: number")
    );
    assert!(dts.contains("export type RegisterRequest = Additive<{"));
    assert!(dts.contains("storage: StorageChoice"));
    assert!(dts.contains("storage?: MachineStorageObservation"));
    assert!(dts.contains("public_key: WireGuardPublicKey"));
    assert!(dts.contains("export type Registered = Additive<{"));
    assert!(dts.contains("assigned_machine: Machine"));
    assert!(dts.contains("visible_peers: Machine[]"));
    assert!(dts.contains("target_versions: { readonly [key: string]: number }"));
    assert!(dts.contains("export type ContainerObservation = Additive<{"));
    assert!(dts.contains("export type ServiceObservation = Additive<{"));
    assert!(dts.contains("export type RttStatistics = Additive<{"));
    assert!(dts.contains("export type CertificateObservation = Additive<{"));
    assert!(dts.contains("resolved_spec: ResolvedServiceSpec"));
    assert!(!dts.contains("export declare function connect"));
    assert!(!dts.contains("export declare class Client"));
    assert!(dts.contains("export const RUNTIME_WATCH_CAPABILITY: CapabilityName"));
    assert!(dts.contains(RUNTIME_WATCH_CAPABILITY));
    assert!(dts.contains("export const DESCRIBE_CONTRACT_CAPABILITY: CapabilityName"));
    assert!(dts.contains(DESCRIBE_CONTRACT_CAPABILITY));
    assert!(dts.contains(CERTIFICATE_POLICY_CAPABILITY));
    assert!(dts.contains("export declare function packageName(): \"@ployz/sdk\";"));
    for wire in MembershipObservation::known_wires() {
        assert!(
            dts.contains(&format!("\"{wire}\"")),
            "MembershipObservation TypeScript is missing {wire}"
        );
    }
    for wire in HealthObservation::known_wires() {
        assert!(
            dts.contains(&format!("\"{wire}\"")),
            "HealthObservation TypeScript is missing {wire}"
        );
    }
    for wire in RpcErrorCode::known_wires() {
        assert!(
            dts.contains(&format!("\"{wire}\"")),
            "RpcErrorCode TypeScript is missing {wire}"
        );
    }
}

#[test]
fn handwritten_facade_types_use_generated_payloads() {
    let dts = include_str!("../../ployz-sdk/index.d.ts");
    assert!(dts.contains("from \"./generated/payloads\""));
    assert!(dts.contains("ContractDescription"));
    assert!(dts.contains("DeployIntent"));
    assert!(dts.contains("DeployPreview"));
    assert!(dts.contains("DeployOutcome"));
    assert!(dts.contains("DockerVolumeName"));
    assert!(dts.contains("ExecutionError"));
    assert!(dts.contains("MachineId"));
    assert!(dts.contains("PartialResult"));
    assert!(dts.contains("RemoveVolumesRequest"));
    assert!(dts.contains("RpcError"));
    assert!(dts.contains("MachineTarget"));
    assert!(dts.contains("ObservedDataLoss"));
    assert!(dts.contains("DataLoss"));
    assert!(dts.contains("LocalMachineRemoved"));
    assert!(dts.contains("ClusterTeardown"));
    assert!(dts.contains("RegisterRequest"));
    assert!(dts.contains("Registered"));
    assert!(dts.contains("RuntimeWatchFrame"));
    assert!(dts.contains("export * from \"./generated/payloads\""));
    assert!(dts.contains("export declare function connect"));
    assert!(dts.contains("export declare function listHeld"));
    assert!(dts.contains("export declare function register("));
    assert!(dts.contains("identity: RegisterRequest"));
    assert!(dts.contains("Promise<Registered>"));
    assert!(!dts.contains("connectHeld"));
    assert!(dts.contains("relayUrl: string"));
    assert!(dts.contains("bearer: string"));
    assert!(dts.contains("machineId: MachineId"));
    assert!(dts.contains("about(): Promise<ContractDescription>"));
    assert!(dts.contains("readonly runtime:"));
    assert!(dts.contains("watch(options?: WatchOptions): AsyncIterable<RuntimeWatchFrame>"));
    assert!(dts.contains("preview(intent: DeployIntent): Promise<PreparedDeploy>"));
    assert!(dts.contains("previewProjectRemoval("));
    assert!(dts.contains("destroy_volumes: boolean"));
    assert!(dts.contains("run("));
    assert!(dts.contains("Promise<DeployOutcome<ExecutionError>>"));
    assert!(dts.contains("confirm(options?: ConfirmOptions): RunningDeploy"));
    assert!(dts.contains("applyAll("));
    assert!(dts.contains("applyOne("));
    assert!(dts.contains("applyAll(\n  project_name: ProjectName,"));
    assert!(!dts.contains("deploy(intent: DeployIntent)"));
    assert!(dts.contains("removeVolumes("));
    assert!(dts.contains("RemoveVolumesRequest"));
    assert!(dts.contains("PartialResult<DockerVolumeName, RpcError>"));
    assert!(
        dts.contains("dataLossIfMachineRemoved(machine: MachineTarget): Promise<ObservedDataLoss>")
    );
    assert!(dts.contains("removeMachine("));
    assert!(dts.contains("confirmDataLoss: DataLossConfirmation"));
    assert!(dts.contains("Promise<LocalMachineRemoved>"));
    assert!(
        dts.contains(
            "dataLossIfProjectDestroyed(\n    project_name: ProjectName,\n    destroy_volumes?: boolean,\n  ): Promise<ObservedDataLoss>"
        )
    );
    assert!(dts.contains("destroyProject("));
    assert!(dts.contains("Promise<DeployOutcome<ExecutionError>>"));
    assert!(!dts.contains("destroyProject(\n    project_name: ProjectName,\n  )"));
    assert!(dts.contains("dataLossIfClusterDestroyed(): Promise<ObservedDataLoss>"));
    assert!(dts.contains(
        "destroyCluster(confirmDataLoss: DataLossConfirmation): Promise<ClusterTeardown>"
    ));
    assert!(!dts.contains("confirmAll"));
    assert!(!dts.contains("removeMachine(machine: MachineTarget):"));
    assert!(dts.contains("close(): Promise<void>"));
    assert!(!dts.contains("connectSsh"));
    assert!(!dts.contains("connectTcp"));
    assert!(!dts.contains("connectUnix"));
    assert!(!dts.contains("watchRuntime"));
    assert!(!dts.contains("ops.watch"));
    assert!(!dts.contains("export declare function call"));
    assert!(!dts.contains("export declare function request"));
}

fn assert_typed_spec_fixtures(fixtures: &BTreeMap<String, Value>) {
    let requested: RequestedServiceSpec =
        decode_fixture(fixture(fixtures, "requested_service_spec_typed"));
    let config = requested
        .configs()
        .first()
        .expect("typed spec includes ConfigSpec");
    assert_eq!(config.name, "settings");
    assert_eq!(config.content, b"port = 8080");
    assert_eq!(requested.config_mounts().len(), 2);
    match requested.volumes().first().map(|volume| &volume.source) {
        Some(VolumeSource::Named {
            driver: Some(driver),
            ..
        }) => assert_eq!(driver.name, "nfs"),
        other => panic!("typed requested spec must nest VolumeDriver, got {other:?}"),
    }
    assert_eq!(requested.container.resources.devices.len(), 1);
    assert_eq!(requested.container.resources.device_reservations.len(), 2);
    assert!(
        requested
            .container
            .resources
            .device_reservations
            .get(1)
            .is_some_and(|reservation| reservation.driver.is_none())
    );
    assert_eq!(
        requested
            .container
            .resources
            .ulimits
            .get("nofile")
            .map(|limit| limit.hard),
        Some(2048)
    );
    assert!(
        requested
            .container
            .healthcheck
            .as_ref()
            .and_then(HealthcheckSpec::as_configured)
            .is_some()
    );
    assert_eq!(
        serde_json::to_value(&requested).unwrap(),
        *fixture(fixtures, "requested_service_spec_typed")
    );

    let resolved: ResolvedServiceSpec =
        decode_fixture(fixture(fixtures, "resolved_service_spec_typed"));
    assert_eq!(resolved.configs(), requested.configs());
    assert_eq!(
        serde_json::to_value(&resolved).unwrap(),
        *fixture(fixtures, "resolved_service_spec_typed")
    );

    let observation: ContainerObservation = decode_fixture(fixture(
        fixtures,
        "container_observation_disabled_healthcheck",
    ));
    assert_eq!(
        observation.effective_healthcheck,
        Some(HealthcheckSpec::Disabled)
    );
    assert_eq!(
        serde_json::to_value(&observation).unwrap(),
        *fixture(fixtures, "container_observation_disabled_healthcheck")
    );
}

#[test]
fn capability_fixture_matches_the_rpc_catalog() {
    let fixtures = fixtures();
    let names = fixture(&fixtures, "capabilities")
        .as_array()
        .expect("capabilities array")
        .iter()
        .map(|value| value.as_str().expect("capability string"))
        .collect::<Vec<_>>();
    assert!(names.contains(&DESCRIBE_CONTRACT_CAPABILITY));
    assert!(names.contains(&CERTIFICATE_POLICY_CAPABILITY));
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[test]
fn no_nats_compatibility_package_is_introduced() {
    let root = sdk_package_root();
    let workspace = root.parent().expect("workspace root");
    assert!(!workspace.join("packages/ployz-sdk").exists());
    assert!(!workspace.join("ployz-nats-sdk").exists());
}

#[test]
fn write_generated_fails_when_the_package_root_is_a_file() {
    let path = std::env::temp_dir().join(format!(
        "ployz-sdk-write-generated-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&path, b"not a directory").unwrap();
    assert!(write_generated(&path).is_err());
    std::fs::remove_file(&path).unwrap();
}
