//! Pipeline tests for `@ployz/sdk` generated TypeScript and JSON fixtures.

use std::collections::BTreeMap;

use ployz_core::{
    CERTIFICATE_POLICY_CAPABILITY, CertificateAvailability, CertificateFailureKind,
    ContainerRuntimeObservation, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, DeployIntent,
    DeployOutcome, DockerVolume, ExecutionError, HealthObservation, MembershipObservation,
    PlanOptions, RUNTIME_WATCH_CAPABILITY, RpcError, RpcErrorCode, RuntimeWatchFrame,
    ServiceAttempt,
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
    assert_eq!(volume.driver, "local");
    assert_eq!(
        serde_json::to_value(&volume).unwrap(),
        *fixture(&fixtures, "docker_volume")
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
    assert_eq!(volume.driver, "local");
    let error: RpcError = decode_fixture(
        failures
            .first()
            .and_then(|row| row.get("error"))
            .expect("failure error"),
    );
    assert_eq!(error.code.as_str(), "unsupported");

    let intent: DeployIntent = decode_fixture(fixture(&fixtures, "deploy_intent"));
    assert!(intent.target.is_empty());
    assert!(intent.apply.is_empty());
    assert_eq!(intent.options, PlanOptions::default());
    assert!(intent.dependencies().is_empty());
    assert_eq!(
        serde_json::to_value(&intent).unwrap(),
        *fixture(&fixtures, "deploy_intent")
    );

    let attempt: ServiceAttempt = decode_fixture(fixture(&fixtures, "service_attempt"));
    assert_eq!(attempt.name.as_str(), "web");

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

    let frame: RuntimeWatchFrame = decode_fixture(fixture(&fixtures, "runtime_watch_frame"));
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

    let frame: RuntimeWatchFrame =
        decode_fixture(fixture(&fixtures, "runtime_watch_frame_unknown_fields"));
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
    assert!(dts.contains("export type ContractDescription = Additive<{"));
    assert!(dts.contains("export type DeployIntent = Additive<{"));
    assert!(dts.contains("target: RequestedServiceSpec[]"));
    assert!(dts.contains("export type DeployOperation ="));
    assert!(dts.contains("export type FailedOperation<E = ExecutionError> ="));
    assert!(dts.contains("export type DeployOutcome<E = ExecutionError> ="));
    assert!(dts.contains("export type ExecutionError ="));
    assert!(dts.contains("export type MachineAction ="));
    assert!(dts.contains("export type HealthFailure ="));
    assert!(dts.contains("export type HookFailure ="));
    assert!(dts.contains("Success: { completed: DeployOperation[] }"));
    assert!(dts.contains("unexecuted: DeployOperation[]"));
    assert!(dts.contains("failed: FailedOperation<E>"));
    assert!(dts.contains("export type RuntimeWatchFrame = Additive<{"));
    assert!(dts.contains("incomplete_ids: RuntimeWatchIncompleteIds"));
    assert!(dts.contains("hosted_dns_hostname?: string"));
    assert!(dts.contains("export type MachineObservation = Additive<{"));
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
    assert!(dts.contains("DeployOutcome"));
    assert!(dts.contains("ExecutionError"));
    assert!(dts.contains("MachineId"));
    assert!(dts.contains("RuntimeWatchFrame"));
    assert!(dts.contains("export * from \"./generated/payloads\""));
    assert!(dts.contains("export declare function connect"));
    assert!(dts.contains("relayUrl: string"));
    assert!(dts.contains("bearer: string"));
    assert!(dts.contains("machineId: MachineId"));
    assert!(dts.contains("about(): Promise<ContractDescription>"));
    assert!(dts.contains("readonly runtime:"));
    assert!(dts.contains("watch(options?: WatchOptions): AsyncIterable<RuntimeWatchFrame>"));
    assert!(dts.contains("deploy(intent: DeployIntent): Promise<DeployOutcome<ExecutionError>>"));
    assert!(dts.contains("close(): Promise<void>"));
    assert!(!dts.contains("connectSsh"));
    assert!(!dts.contains("connectTcp"));
    assert!(!dts.contains("connectUnix"));
    assert!(!dts.contains("watchRuntime"));
    assert!(!dts.contains("ops.watch"));
    assert!(!dts.contains("export declare function call"));
    assert!(!dts.contains("export declare function request"));
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
