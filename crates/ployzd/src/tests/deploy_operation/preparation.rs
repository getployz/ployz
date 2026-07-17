use crate::control::operations::deploy::{
    AutomaticHostnameMode, DeployExecutionCommand, DeployExecutionFacts,
    DeployServiceExecutionCommand, prepare_deploy_execution_command,
};
use ployz_core::deploy::{
    ContainerMountPath, DatasetName, DeployCleanupContainer, DeployRequest, DeployRoute,
    DeployRouteTarget, DeployServiceSpec, ImageAvailabilityExpiresAt, ImageReference, ImageSource,
    PlatformImage, PushedImageReceipt, ReplicaCount, ServiceVolumeMount, VolumeMaxSizeBytes,
    VolumeName, VolumeSpec, ZfsPoolName,
};
use ployz_core::ids::{NamespaceRevisionEntryId, RouteBindingId};
use ployz_core::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use ployz_core::intent::{RouteBindingState, VolumeKind, VolumePinState};
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::machine::{MachineUsabilityReason, StorageCapability};
use ployz_core::operation::{RouteHostname, RoutePort, RouteTarget};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, service_id,
};
use std::time::Duration;

#[tokio::test]
async fn separates_reusable_replicas_from_cleanup_candidates() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_members: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [
                    observed_service_container("machine_a", "ctr_old", "entry_old"),
                    observed_service_container_with_service(
                        "machine_a",
                        "ctr_other_service",
                        "entry_other",
                        "svc_worker",
                    ),
                    exited_observed_service_container("machine_a", "ctr_stopped", "entry_target"),
                ],
            )
            .expect("valid machine observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    let service = single_service(&command);
    assert!(service.existing_replicas().is_empty());
    assert_eq!(
        service.cleanup_candidates(),
        [
            cleanup_container("machine_a", "ctr_old", "entry_old"),
            stopped_cleanup_container("machine_a", "ctr_stopped", "entry_target"),
        ]
    );
}

#[tokio::test]
async fn does_not_reuse_an_unpromoted_running_target_entry() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_members: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [observed_service_container_with_entry(
                    "machine_a",
                    "ctr_target",
                    target_namespace_revision_entry_id(),
                )],
            )
            .expect("valid machine observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    let service = single_service(&command);
    assert!(service.existing_replicas().is_empty());
    assert_eq!(
        service.cleanup_candidates(),
        [cleanup_container_with_entry(
            "machine_a",
            "ctr_target",
            target_namespace_revision_entry_id()
        )]
    );
}

#[tokio::test]
async fn reuses_a_matching_running_promoted_target_entry() {
    let request = deploy_request();
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: vec![promoted],
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_members: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [observed_service_container_with_entry(
                    "machine_a",
                    "ctr_target",
                    target_namespace_revision_entry_id(),
                )],
            )
            .expect("valid machine observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    assert_eq!(
        single_service(&command).existing_replicas(),
        vec![existing_service_replica("machine_a", "ctr_target")]
    );
}

#[tokio::test]
async fn pushed_receipt_keeps_a_covered_existing_replica_outside_new_placement_candidates() {
    let amd64 = ployz_core::image::OciPlatform::try_new("linux", "amd64").expect("platform");
    let receipt = PushedImageReceipt::try_new([(
        amd64.clone(),
        PlatformImage {
            seed: machine_id("machine_seed"),
            manifest_digest: ployz_core::image::OciDigest::sha256(b"manifest"),
            image_id: ployz_core::image::OciDigest::sha256(b"image"),
            availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                .expect("expiry"),
        },
    )])
    .expect("receipt");
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request has one service");
    };
    service.image = ImageReference::try_new("local/api:build")
        .expect("image")
        .with_digest(receipt.index_digest())
        .expect("pinned image");
    service.image_source = ImageSource::PushedToSeed(receipt);
    let entry_id = service.namespace_revision_entry_id(&request.namespace_id);
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = entry_id.clone();
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::from([
            (
                machine_id("machine_new"),
                ployz_core::image::OciPlatform::try_new("linux", "arm64").expect("platform"),
            ),
            (machine_id("machine_existing"), amd64),
        ]),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: vec![promoted],
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_new")],
        dataplane_members: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_existing"),
                [observed_service_container_with_entry(
                    "machine_existing",
                    "ctr_existing",
                    entry_id,
                )],
            )
            .expect("observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    assert_eq!(
        single_service(&command).existing_replicas(),
        [existing_service_replica("machine_existing", "ctr_existing")]
    );
}

#[tokio::test]
async fn manifest_omission_removes_serving_entry_routes_and_containers() {
    // The manifest declares only `svc_api`; `svc_worker` is omitted. Its
    // serving entry is unpublished, its route binding detached, and its
    // running container becomes a cleanup candidate - manifest omission
    // removes a service from the namespace.
    let request = deploy_request();
    let omitted_target = RouteTarget::new(
        RouteHostname::try_new("worker.example.com").expect("valid route hostname"),
    );
    let omitted_container = MachineContainerObservationSnapshot::try_new(
        machine_id("machine_a"),
        [observed_service_container_with_service(
            "machine_a",
            "ctr_worker",
            "entry_worker",
            "svc_worker",
        )],
    )
    .expect("valid machine observation snapshot");
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: vec![RouteBindingState {
            id: RouteBindingId::try_new("route_worker").expect("valid route binding id"),
            namespace_id: namespace_id("default"),
            target: omitted_target.clone(),
            endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("svc_worker"),
            origin: RouteBindingOrigin::Declared,
        }],
        namespace_serving_entries: vec![
            serving_target_entry("svc_api", "entry_api"),
            serving_target_entry("svc_worker", "entry_worker"),
        ],
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_members: Vec::new(),
        observed_machines: vec![omitted_container.clone()],
        namespace_cleanup_candidates:
            crate::control::operations::deploy::namespace_cleanup_candidates(
                &namespace_id("default"),
                &[omitted_container],
            ),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    assert_eq!(
        command
            .route_binding_removals()
            .iter()
            .map(|binding| binding.target.clone())
            .collect::<Vec<_>>(),
        [omitted_target]
    );
    assert_eq!(
        command.serving_target_removals(),
        [serving_target_entry("svc_worker", "entry_worker")]
    );
    let [candidate] = command.namespace_cleanup_candidates() else {
        panic!("omitted service container is a cleanup candidate");
    };
    assert_eq!(candidate.identity.service_id, service_id("svc_worker"));
}

#[tokio::test]
async fn empty_manifest_prepares_no_services() {
    let facts = DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: vec![ployz_core::operation::UnusableMachine {
            machine_id: machine_id("machine_silent"),
            reason: MachineUsabilityReason::FactsUnavailable,
        }],
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_members: Vec::new(),
        observed_machines: Vec::new(),
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };
    let command = prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: Vec::new(),
        },
        facts,
    );

    assert!(command.services().is_empty());
    assert_eq!(
        command.unusable_machines(),
        [ployz_core::operation::UnusableMachine {
            machine_id: machine_id("machine_silent"),
            reason: MachineUsabilityReason::FactsUnavailable,
        }]
    );
}

#[test]
fn provisioned_mount_requires_fresh_ready_storage_without_filtering_plain_work() {
    let candidates = vec![machine_id("machine_a"), machine_id("machine_b")];
    let storage = std::collections::BTreeMap::from([
        (
            machine_id("machine_a"),
            Some(StorageCapability::Ready {
                pool: ZfsPoolName::try_new("ployz").expect("valid pool"),
                capacity: ployz_core::machine::PoolCapacityFacts {
                    total_bytes: 1024 * 1024,
                    provisioned_used_bytes: 0,
                    free_bytes: 1024 * 1024,
                    child_quotas: Vec::new(),
                },
            }),
        ),
        (machine_id("machine_b"), None),
    ]);
    let facts = |storage| DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: storage,
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: candidates.clone(),
        dataplane_members: Vec::new(),
        observed_machines: Vec::new(),
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let plain = prepare_deploy_execution_command(
        operation_id("op_plain"),
        deploy_request(),
        facts(storage.clone()),
    );
    assert_eq!(single_service(&plain).eligible_machines(), candidates);

    let mut provisioned_request = deploy_request();
    let volume_name = VolumeName::try_new("data").expect("valid volume name");
    provisioned_request.volumes.insert(
        volume_name.clone(),
        VolumeSpec::Provisioned {
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    );
    let [service] = provisioned_request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name,
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    let mut second_service = service.clone();
    second_service.service_id = service_id("svc_worker");
    provisioned_request.services.push(second_service);

    let provisioned = prepare_deploy_execution_command(
        operation_id("op_provisioned"),
        provisioned_request,
        facts(storage),
    );

    assert!(
        provisioned
            .services()
            .iter()
            .all(|service| { service.eligible_machines() == [machine_id("machine_a")] })
    );
    let [unusable] = provisioned.unusable_machines() else {
        panic!("legacy machine is the sole unusable candidate");
    };
    assert_eq!(
        unusable.reason,
        MachineUsabilityReason::StorageTestimonyNotReported
    );
}

#[test]
fn pinned_provisioned_mount_rejects_ready_testimony_from_the_wrong_pool() {
    let mut request = deploy_request();
    let volume_name = VolumeName::try_new("data").expect("valid volume name");
    request.volumes.insert(
        volume_name.clone(),
        VolumeSpec::Provisioned {
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    );
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name.clone(),
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    let namespace_id = namespace_id("default");
    let expected = ZfsPoolName::try_new("tank").expect("valid pool");
    let reported = ZfsPoolName::try_new("other").expect("valid pool");
    let pin = VolumePinState::try_new(
        namespace_id.clone(),
        volume_name.clone(),
        machine_id("machine_a"),
        VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(&expected, &namespace_id, &volume_name)
                .expect("canonical dataset"),
            max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
        },
    )
    .expect("valid volume pin");
    let command = prepare_deploy_execution_command(
        operation_id("op_mismatch"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::from([(
                machine_id("machine_a"),
                Some(StorageCapability::Ready {
                    pool: reported.clone(),
                    capacity: ployz_core::machine::PoolCapacityFacts {
                        total_bytes: 1024 * 1024,
                        provisioned_used_bytes: 0,
                        free_bytes: 1024 * 1024,
                        child_quotas: Vec::new(),
                    },
                }),
            )]),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: vec![pin],
            eligible_machines: vec![machine_id("machine_a")],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    );

    assert!(single_service(&command).eligible_machines().is_empty());
    assert_eq!(
        command.unusable_machines(),
        [ployz_core::operation::UnusableMachine {
            machine_id: machine_id("machine_a"),
            reason: MachineUsabilityReason::StoragePoolMismatch { expected, reported },
        }]
    );
}

#[test]
fn auto_hostname_is_stable_and_collision_safe() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.routes = vec![DeployRoute {
        target: DeployRouteTarget::AutoHostname {
            label: AutomaticHostnameLabel::try_new("svc-api-2")
                .expect("valid automatic hostname label"),
        },
        endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
    }];
    let existing = RouteBindingState {
        id: RouteBindingId::try_new("route_existing").expect("valid route binding id"),
        namespace_id: namespace_id("default"),
        target: RouteTarget::new(
            RouteHostname::try_new("svc-api-2.demo.up.ployz.app").expect("valid hostname"),
        ),
        endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
        service_id: service_id("svc_api"),
        origin: RouteBindingOrigin::Automatic,
    };
    let facts = DeployExecutionFacts {
        namespace_route_bindings: vec![
            RouteBindingState {
                id: RouteBindingId::try_new("route_other").expect("valid route binding id"),
                namespace_id: namespace_id("other"),
                service_id: service_id("svc_other"),
                target: RouteTarget::new(
                    RouteHostname::try_new("svc-api.demo.up.ployz.app").expect("valid hostname"),
                ),
                endpoint_port: existing.endpoint_port,
                origin: RouteBindingOrigin::Automatic,
            },
            existing.clone(),
        ],
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: Vec::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        dataplane_members: Vec::new(),
        observed_machines: Vec::new(),
        machine_platforms: std::collections::BTreeMap::new(),
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Ployz {
            suffix: RouteHostname::try_new("demo.up.ployz.app")
                .expect("valid automatic hostname suffix"),
        },
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts);

    assert_eq!(single_service(&command).route_binding_states(), [existing]);
}

#[test]
fn detached_hostname_recreation_mints_a_fresh_binding_identity() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.routes = vec![DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: RouteHostname::try_new("api.example.com").expect("valid hostname"),
        },
        endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
    }];
    let facts = DeployExecutionFacts {
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: Vec::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        dataplane_members: Vec::new(),
        observed_machines: Vec::new(),
        machine_platforms: std::collections::BTreeMap::new(),
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let first =
        prepare_deploy_execution_command(operation_id("op_first"), request.clone(), facts.clone());
    let recreated = prepare_deploy_execution_command(operation_id("op_recreated"), request, facts);
    let [first_binding] = single_service(&first).route_binding_states() else {
        panic!("first deploy has one route binding");
    };
    let [recreated_binding] = single_service(&recreated).route_binding_states() else {
        panic!("recreated deploy has one route binding");
    };

    assert_ne!(first_binding.id, recreated_binding.id);
}

fn single_service(command: &DeployExecutionCommand) -> &DeployServiceExecutionCommand {
    let [service] = command.services() else {
        panic!("deploy command has one service");
    };
    service
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:rev_2")
                .expect("valid image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    let request = deploy_request();
    let [service] = request.services.as_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.namespace_revision_entry_id(&namespace_id("default"))
}

fn existing_service_replica(
    machine_id: &str,
    container_id: &str,
) -> ployz_core::deploy::ExistingServiceReplica {
    ployz_core::deploy::ExistingServiceReplica {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        creation_gate: ployz_core::deploy::ExistingReplicaCreationGate::AlreadyPassed,
    }
}

fn cleanup_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ployz_core::deploy::ObservedCleanupCandidate {
    cleanup_container_with_entry(
        machine_id,
        container_id,
        self::namespace_revision_entry_id(namespace_revision_entry_id),
    )
}

fn cleanup_container_with_entry(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
) -> ployz_core::deploy::ObservedCleanupCandidate {
    ployz_core::deploy::ObservedCleanupCandidate {
        target: DeployCleanupContainer {
            machine_id: self::machine_id(machine_id),
            container_id: self::container_id(container_id),
            identity: containers::identity("svc_api")
                .entry(namespace_revision_entry_id.as_str())
                .operation("op_existing")
                .step(&format!("existing_{container_id}"))
                .build(),
        },
        state: ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable(),
        created_at_unix_seconds: None,
        observed_image_identity: None,
    }
}

fn stopped_cleanup_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ployz_core::deploy::ObservedCleanupCandidate {
    let mut target = cleanup_container(machine_id, container_id, namespace_revision_entry_id);
    target.state = ployz_core::machine::runtime::ContainerRuntimeState::Exited;
    target
}

fn observed_service_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ManagedContainerObservation {
    observed_service_container_with_entry(
        machine_id,
        container_id,
        self::namespace_revision_entry_id(namespace_revision_entry_id),
    )
}

fn observed_service_container_with_entry(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
) -> ManagedContainerObservation {
    containers::observation(machine_id, container_id)
        .with(
            containers::identity("svc_api")
                .entry(namespace_revision_entry_id.as_str())
                .operation("op_existing")
                .step(&format!("existing_{container_id}")),
        )
        .running_unroutable()
        .build()
}

fn observed_service_container_with_service(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
    service_id: &str,
) -> ManagedContainerObservation {
    let mut observation =
        observed_service_container(machine_id, container_id, namespace_revision_entry_id);
    observation.identity.service_id = self::service_id(service_id);
    observation
}

fn exited_observed_service_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ManagedContainerObservation {
    let mut observation =
        observed_service_container(machine_id, container_id, namespace_revision_entry_id);
    observation.state = ContainerRuntimeState::Exited;
    observation
}
