//! Pipeline tests for `@ployz/sdk` generated TypeScript and Rust examples.

use std::collections::BTreeMap;

use ployz_core::{
    CERTIFICATE_POLICY_CAPABILITY, CertificateAvailability, CertificateFailureKind,
    ClusterTeardown, ContainerObservation, ContainerRuntimeObservation, ContractDescription,
    CreateVolumeReport, DESCRIBE_CONTRACT_CAPABILITY, DataLoss, DataLossConfirmation, DeployIntent,
    DeployOutcome, DeployPreview, DockerVolume, DockerVolumeStorageObservation, ExecutionError,
    HealthObservation, HealthcheckSpec, IngressHostname, LocalMachineRemoved,
    MembershipObservation, ObservedDataLoss, PlanOptions, RequestedServiceSpec,
    ResolvedServiceSpec, RpcError, RuntimeWatchFrame, ServiceAttempt, StorageChoice,
    UnconfirmedDataLoss, VolumeInventory, VolumeSource, VolumeToCreate,
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
fn generated_declarations_match_checked_in_file() {
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
    assert!(matches!(loss, DataLoss::DockerVolume { .. }));
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
    let external: VolumeSource = decode_fixture(fixture(&fixtures, "external_volume_source"));
    assert!(matches!(
        external,
        VolumeSource::External { name } if name.as_str() == "shared"
    ));
    let ordinary: VolumeSource = decode_fixture(fixture(&fixtures, "ordinary_volume_source"));
    assert!(matches!(
        ordinary,
        VolumeSource::Ordinary { driver, labels, .. }
            if driver.name() == "local" && driver.options().is_empty() && labels.is_empty()
    ));
    let provisioned: VolumeSource = decode_fixture(fixture(&fixtures, "provisioned_volume_source"));
    assert!(matches!(
        provisioned,
        VolumeSource::Provisioned { maximum_bytes, labels, .. }
            if maximum_bytes.get() == 1_073_741_824
                && labels.get("backup").map(String::as_str) == Some("daily")
    ));
    let volume: VolumeToCreate = decode_fixture(fixture(&fixtures, "volume_to_create"));
    assert!(matches!(
        volume,
        VolumeToCreate { maximum_bytes: Some(maximum_bytes), .. }
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
    assert_eq!(preview.volumes_to_create.len(), 1);
    assert_eq!(preview.warnings.len(), 6);
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
    assert_eq!(container.resolved_spec.name.as_str(), "api");
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

    let legacy_json = fixture(&fixtures, "container_runtime_legacy_unknown");
    let unknown: ContainerRuntimeObservation = decode_fixture(legacy_json);
    assert_eq!(
        unknown,
        ContainerRuntimeObservation::Unknown {
            raw: legacy_json.clone()
        }
    );
    let unknown_json = fixture(&fixtures, "container_runtime_unknown");
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
        Some(VolumeSource::Ordinary { driver, .. }) => assert_eq!(driver.name(), "nfs"),
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
fn write_generated_fails_when_the_package_root_is_a_file() {
    let path = std::env::temp_dir().join(format!(
        "ployz-sdk-write-generated-{}.tmp",
        std::process::id()
    ));
    std::fs::write(&path, b"not a directory").unwrap();
    assert!(write_generated(&path).is_err());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn sdk_intent_and_resolved_spec_reject_negative_resource_quantities() {
    let fixtures = fixtures();
    for field in [
        "cpu_nanos",
        "memory_bytes",
        "memory_reservation_bytes",
        "shared_memory_bytes",
    ] {
        let mut spec = fixture(&fixtures, "requested_service_spec").clone();
        *spec
            .pointer_mut(&format!("/container/resources/{field}"))
            .unwrap() = serde_json::json!(-1);
        let mut intent = fixture(&fixtures, "deploy_intent").clone();
        *intent.get_mut("target").unwrap() = serde_json::json!([spec]);
        assert!(
            serde_json::from_value::<DeployIntent>(intent).is_err(),
            "{field}"
        );
        let mut resolved = fixture(&fixtures, "resolved_service_spec_typed").clone();
        *resolved
            .pointer_mut(&format!("/container/resources/{field}"))
            .unwrap() = serde_json::json!(-1);
        assert!(
            serde_json::from_value::<ResolvedServiceSpec>(resolved).is_err(),
            "{field}"
        );
    }
}
