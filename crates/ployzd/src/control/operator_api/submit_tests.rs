use std::collections::BTreeMap;

use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployRequest, DeployReservationId, DeployServiceSpec, ImageReference,
    ImageSource, ReplicaCount,
};
use ployz_core::ids::{MachineId, NamespaceId, OperationId, ServiceId};
use ployz_core::image::OciDigest;
use ployz_core::ops::OperationIdempotencyKey;
use ployz_sdk_types::{DeploySubmitError, NetworkRepairError};

use crate::control::operator_api::admission::DeploySubmitCommand;

use super::{
    validate_internal_dns_name, validate_network_repair_preconditions,
    validate_registry_credentials,
};

fn operation_id() -> OperationId {
    OperationId::try_new("op_network_repair").expect("operation id")
}

fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("machine id")
}

#[test]
fn network_repair_requires_an_active_machine_before_admission() {
    let error = validate_network_repair_preconditions(&operation_id(), None, &[])
        .expect_err("empty roster must be rejected");

    assert!(matches!(error, NetworkRepairError::NoActiveMachines { .. }));
}

#[test]
fn targeted_network_repair_requires_the_target_before_admission() {
    let error = validate_network_repair_preconditions(
        &operation_id(),
        Some(&machine_id("machine_b")),
        &[machine_id("machine_a")],
    )
    .expect_err("unknown target must be rejected");

    assert!(matches!(
        error,
        NetworkRepairError::TargetMachineNotFound { .. }
    ));
}

#[test]
fn deploy_admission_rejects_ids_that_cannot_form_internal_dns_labels() {
    let namespace_id = NamespaceId::try_new("default").expect("namespace id");
    let service_id = ServiceId::try_new("s".repeat(64)).expect("service id");

    let failure = validate_internal_dns_name(&namespace_id, &service_id)
        .expect_err("oversized DNS label must be rejected");

    assert!(failure.as_str().contains("limited to 63 bytes"));
}

#[test]
fn pushed_image_digest_must_match_the_manifest_digest() {
    let manifest_digest = OciDigest::sha256(b"manifest");
    let image = ImageReference::try_new("local/api:latest")
        .expect("valid image")
        .with_digest(&OciDigest::sha256(b"different"))
        .expect("image accepts digest");
    let command = DeploySubmitCommand {
        operation_id: OperationId::try_new("op_test").expect("valid operation id"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_test")
            .expect("valid idempotency key"),
        reservation_id: DeployReservationId::first(),
        target: DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
            origin: None,
            services: vec![DeployServiceSpec {
                service_id: ServiceId::try_new("api").expect("valid service id"),
                image,
                image_source: ImageSource::PushedToSeed {
                    seed: MachineId::try_new("machine_a").expect("valid machine id"),
                    manifest_digest,
                    image_id: OciDigest::sha256(b"image"),
                },
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        },
        registry_credentials: BTreeMap::new(),
    };

    assert!(matches!(
        validate_registry_credentials(&command),
        Err(DeploySubmitError::InvalidTarget { message, .. })
            if message.as_str().contains("must match its pushed manifest digest")
    ));
}
