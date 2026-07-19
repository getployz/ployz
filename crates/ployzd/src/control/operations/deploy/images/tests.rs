use super::*;
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployPhasePlan, DeployPlanningInput, DeployPlanningPlacementInput,
    DeployRequest, DeployServiceSpec, ImageReference, PlatformImage, PushedImageReceipt,
    ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::{MachineId, NamespaceId, NamespaceRevisionId, OperationId, ServiceId};
use ployz_core::image::{OciDigest, OciPlatform};

use crate::control::operations::deploy::types::ServingIntentDisposition;
use crate::control::role_client::machine::MachineClockTestimony;

#[test]
fn missing_seed_clock_testimony_is_rejected() {
    let service = pushed_service();
    let platform_image = pushed_platform(&service);

    assert!(matches!(
        seed_clock_failure(
            &service.service.service_id,
            platform_image,
            None,
        ),
        Some(ImagePreparationFailure::SeedUnavailable { message, .. })
            if message.as_str() == "fresh clock testimony from image seed is unavailable"
    ));
}

#[test]
fn seed_clock_more_than_the_safety_margin_ahead_is_rejected() {
    let service = pushed_service();
    let platform_image = pushed_platform(&service);
    let testimony = MachineClockTestimony {
        control_request_started_at_unix_ms: 1_000_000,
        machine_observed_at_unix_ms: 1_300_001,
    };

    assert!(matches!(
        seed_clock_failure(
            &service.service.service_id,
            platform_image,
            Some(&testimony),
        ),
        Some(ImagePreparationFailure::SeedUnavailable { message, .. })
            if message.as_str() == "image seed clock is more than 300 seconds ahead of Control"
    ));
}

#[test]
fn seed_clock_at_the_safety_margin_boundary_is_accepted() {
    let service = pushed_service();
    let platform_image = pushed_platform(&service);
    let testimony = MachineClockTestimony {
        control_request_started_at_unix_ms: 1_000_000,
        machine_observed_at_unix_ms: 1_300_000,
    };

    assert!(
        seed_clock_failure(
            &service.service.service_id,
            platform_image,
            Some(&testimony),
        )
        .is_none()
    );
}

#[test]
fn seed_clock_behind_control_is_accepted() {
    let service = pushed_service();
    let platform_image = pushed_platform(&service);
    let testimony = MachineClockTestimony {
        control_request_started_at_unix_ms: 1_000_000,
        machine_observed_at_unix_ms: 900_000,
    };

    assert!(
        seed_clock_failure(
            &service.service.service_id,
            platform_image,
            Some(&testimony),
        )
        .is_none()
    );
}

#[test]
fn pushed_receipt_without_target_platform_is_a_typed_failure() {
    let service = pushed_service();
    let target_machine = machine_id("machine_arm");
    let arm64 = platform("arm64");

    let error = machine_image_pull(
        &NamespaceId::try_new("default").expect("namespace id"),
        &service,
        &target_machine,
        &arm64,
        &[],
    )
    .expect_err("missing platform must fail");

    assert!(matches!(
        error,
        DeployExecutionError::Image { failure }
            if *failure == DeployOperationFailure::PlatformImageUnavailable {
                service_id: ServiceId::try_new("api").expect("service id"),
                machine_id: target_machine,
                target_platform: arm64,
            }
    ));
}

#[test]
fn expired_platform_is_rejected_before_image_ensure() {
    let service = pushed_service();
    let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
        panic!("pushed service");
    };
    let target_platform = platform("amd64");
    let platform_image = receipt.platform(&target_platform).expect("platform image");

    let failure = expired_platform_failure(
        &service.service.service_id,
        &target_platform,
        platform_image,
        platform_image.availability_expires_at.unix_seconds(),
    )
    .expect("expired receipt fails before RPC");

    assert_eq!(
        failure,
        ImagePreparationFailure::AvailabilityExpired {
            service_id: service.service.service_id.clone(),
            seed: platform_image.seed.clone(),
            target_platform,
            expired_at: platform_image.availability_expires_at,
        }
    );
}

#[test]
fn unexpired_platform_remains_eligible_for_image_ensure() {
    let service = pushed_service();
    let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
        panic!("pushed service");
    };
    let target_platform = platform("amd64");
    let platform_image = receipt.platform(&target_platform).expect("platform image");

    assert!(
        expired_platform_failure(
            &service.service.service_id,
            &target_platform,
            platform_image,
            platform_image
                .availability_expires_at
                .unix_seconds()
                .saturating_sub(1),
        )
        .is_none()
    );
}

#[test]
fn amd64_plan_ignores_an_unused_invalid_arm64_receipt_entry() {
    let amd64_seed = machine_id("machine_amd_seed");
    let receipt = PushedImageReceipt::try_new([
        (
            platform("amd64"),
            PlatformImage {
                seed: amd64_seed.clone(),
                manifest_digest: OciDigest::sha256(b"amd64 manifest"),
                image_id: OciDigest::sha256(b"amd64 image"),
                availability_expires_at: ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(
                    4_102_444_800,
                )
                .expect("future expiry"),
            },
        ),
        (
            platform("arm64"),
            PlatformImage {
                seed: machine_id("machine_removed"),
                manifest_digest: OciDigest::sha256(b"arm64 manifest"),
                image_id: OciDigest::sha256(b"arm64 image"),
                availability_expires_at: ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(1)
                    .expect("expired availability"),
            },
        ),
    ])
    .expect("pushed receipt");
    let target_machine = machine_id("machine_amd");

    validate_pushed_service_availability(
        &ServiceId::try_new("api").expect("service id"),
        &receipt,
        std::slice::from_ref(&target_machine),
        &BTreeMap::from([(target_machine.clone(), platform("amd64"))]),
        &BTreeMap::from([(
            amd64_seed,
            MachineClockTestimony {
                control_request_started_at_unix_ms: 1_000_000,
                machine_observed_at_unix_ms: 1_000_000,
            },
        )]),
        2,
    )
    .expect("unused invalid arm64 receipt entry does not affect an amd64 plan");
}

#[test]
fn pushed_platforms_are_validated_across_all_phases_before_execution() {
    let service = pushed_service();
    let target_machine = machine_id("machine_arm");
    let target_platform = platform("arm64");
    let request = DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("namespace id"),
        origin: None,
        volumes: BTreeMap::new(),
        services: vec![service.service.clone()],
    };
    let command = DeployExecutionCommand {
        operation_id: OperationId::try_new("op_platform_validation").expect("operation id"),
        request,
        services: vec![service],
        route_binding_removals: Vec::new(),
        serving_target_removals: Vec::new(),
        namespace_cleanup_candidates: Vec::new(),
        storage_testimony: BTreeMap::new(),
        machine_platforms: [(target_machine.clone(), target_platform.clone())]
            .into_iter()
            .collect(),
        seed_clock_testimony: BTreeMap::new(),
        dataplane_members: Vec::new(),
        exact_certificate_routes: Vec::new(),
        ployz_automatic_hostnames: false,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        unusable_machines: Vec::new(),
        step_timeout: std::time::Duration::from_secs(1),
    };
    let plan = DeployPlan {
        namespace_id: command.request.namespace_id.clone(),
        namespace_revision_id: NamespaceRevisionId::try_new("revision_platform_validation")
            .expect("revision id"),
        phases: vec![
            DeployPhasePlan {
                services: Vec::new(),
            },
            DeployPhasePlan {
                services: vec![DeployServicePlan {
                    service_id: ServiceId::try_new("api").expect("service id"),
                    placement: ployz_core::deploy::DeployServicePlacement::Replicated,
                    steps: vec![DeployPlanStep::RunContainer {
                        machine_id: target_machine.clone(),
                        slot: ReplicaSlot::Replicated {
                            number: ployz_core::deploy::ReplicatedReplicaSlot::try_new(1)
                                .expect("replica slot"),
                        },
                    }],
                    pre_start: None,
                }],
            },
        ],
        volume_pin_commits: Vec::new(),
        volume_ensures: Vec::new(),
        cleanup_actions: Vec::new(),
    };

    let error = validate_pushed_platforms(&command, &plan)
        .expect_err("later-phase missing platform must fail prevalidation");

    assert!(matches!(
        *error,
        DeployExecutionError::Image { failure }
            if *failure == DeployOperationFailure::PlatformImageUnavailable {
                service_id: ServiceId::try_new("api").expect("service id"),
                machine_id: target_machine,
                target_platform,
            }
    ));
}

fn pushed_service() -> DeployServiceExecutionCommand {
    let digest = |value: char| {
        OciDigest::try_new(format!("sha256:{}", value.to_string().repeat(64))).expect("OCI digest")
    };
    DeployServiceExecutionCommand {
        service: DeployServiceSpec {
            service_id: ServiceId::try_new("api").expect("service id"),
            image: ImageReference::try_new(format!("local/api@{}", digest('a')))
                .expect("image reference"),
            image_source: ImageSource::PushedToSeed(
                PushedImageReceipt::try_new([(
                    platform("amd64"),
                    PlatformImage {
                        seed: machine_id("machine_seed"),
                        manifest_digest: digest('b'),
                        image_id: digest('c'),
                        availability_expires_at:
                            ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                                .expect("expiry"),
                    },
                )])
                .expect("pushed receipt"),
            ),
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(1).expect("replica count"),
            },
            keep: None,
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        },
        registry_credential: None,
        route_commits: Vec::new(),
        planning_input: DeployPlanningInput {
            service_id: ServiceId::try_new("api").expect("service id"),
            placement: DeployPlanningPlacementInput::Replicated {
                eligible_machines: Vec::new(),
            },
            existing_replicas: Vec::new(),
            cleanup_candidates: Vec::new(),
            volume_pins: Vec::new(),
        },
        serving_intent: ServingIntentDisposition::Changed,
        unusable_machines: Vec::new(),
    }
}

fn pushed_platform(service: &DeployServiceExecutionCommand) -> &PlatformImage {
    let ImageSource::PushedToSeed(receipt) = &service.service.image_source else {
        panic!("pushed service");
    };
    let target_platform = platform("amd64");
    receipt.platform(&target_platform).expect("platform image")
}

fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("machine id")
}

fn platform(architecture: &str) -> OciPlatform {
    OciPlatform::try_new("linux", architecture).expect("platform")
}
