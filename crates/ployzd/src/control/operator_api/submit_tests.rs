use std::collections::BTreeMap;

use ployz_core::deploy::{
    ContainerMountPath, ContainerRuntimeSpec, DeployRequest, DeployReservationId,
    DeployServiceSpec, ImageReference, ImageSource, ReplicaCount, ServiceVolumeMount,
    VolumeDeclaredDeployRequest, VolumeName,
};
use ployz_core::ids::{MachineId, NamespaceId, OperationId, ServiceId};
use ployz_core::image::OciDigest;
use ployz_core::operation::OperationIdempotencyKey;
use ployz_sdk_types::{DeploySubmitError, DeploySubmitRequest, NetworkRepairError};

use crate::control::sequencer::DeploySubmitCommand;

use super::{
    normalize_deploy_submit, validate_internal_dns_name, validate_network_repair_preconditions,
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
fn pushed_image_digest_must_match_the_index_digest() {
    let image = ImageReference::try_new("local/api:latest")
        .expect("valid image")
        .with_digest(&OciDigest::sha256(b"different"))
        .expect("image accepts digest");
    let command = DeploySubmitCommand {
        operation_id: OperationId::try_new("op_test").expect("valid operation id"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_test")
            .expect("valid idempotency key"),
        reservation_id: DeployReservationId::first(),
        target: VolumeDeclaredDeployRequest::try_new(DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: vec![DeployServiceSpec {
                service_id: ServiceId::try_new("api").expect("valid service id"),
                image,
                image_source: ImageSource::PushedToSeed(
                    ployz_core::deploy::PushedImageReceipt::try_new([(
                        ployz_core::image::OciPlatform::try_new("linux", "amd64")
                            .expect("platform"),
                        ployz_core::deploy::PlatformImage {
                            seed: MachineId::try_new("machine_a").expect("valid machine id"),
                            manifest_digest: OciDigest::sha256(b"manifest"),
                            image_id: OciDigest::sha256(b"image"),
                        },
                    )])
                    .expect("pushed receipt"),
                ),
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        })
        .expect("deploy request normalizes"),
        registry_credentials: BTreeMap::new(),
    };

    assert!(matches!(
        validate_registry_credentials(&command),
        Err(DeploySubmitError::InvalidTarget { message, .. })
            if message.as_str().contains("must match its pushed index digest")
    ));
}

#[test]
fn deploy_admission_rejects_a_mounted_volume_without_a_declaration() {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: VolumeName::try_new("data").expect("volume name"),
        target: ContainerMountPath::try_new("/data").expect("mount path"),
    }];
    let request = DeploySubmitRequest {
        idempotency_key: OperationIdempotencyKey::try_new("idem_missing_volume_declaration")
            .expect("idempotency key"),
        reservation_id: DeployReservationId::first(),
        target: DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            origin: None,
            volumes: BTreeMap::new(),
            services: vec![DeployServiceSpec {
                service_id: ServiceId::try_new("api").expect("service id"),
                image: ImageReference::try_new("nginx:latest").expect("image"),
                image_source: ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("replicas"),
                runtime,
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        },
        registry_credentials: BTreeMap::new(),
    };

    assert!(matches!(
        normalize_deploy_submit(request),
        Err(DeploySubmitError::InvalidTarget { message, .. })
            if message.as_str().contains("service api mounts volume data without a declaration")
    ));
}
