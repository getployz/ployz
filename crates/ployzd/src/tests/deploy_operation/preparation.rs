use super::fixtures::environment_revision_key;
use crate::control::operations::deploy::{
    AutomaticHostnameMode, DeployExecutionCommand, DeployExecutionError, DeployExecutionFacts,
    DeployServiceExecutionCommand, deploy_plan, prepare_deploy_execution_command,
};
use ployz_core::deploy::{
    ContainerMountPath, ContainerRestartPolicy, DatasetName, DeployCleanupContainer, DeployRequest,
    DeployRequestEvidence, DeployRoute, DeployRouteTarget, DeployServiceSpec, EnvName, EnvValue,
    ImageAvailabilityExpiresAt, ImageReference, ImageSource, PlatformImage, PushedImageReceipt,
    ReplicaCount, ServiceEnvironment, ServiceMode, ServiceVolumeMount, VolumeAdmissionFailure,
    VolumeMaxSizeBytes, VolumeName, VolumeSpec, ZfsPoolName,
};
use ployz_core::ids::{NamespaceRevisionEntryId, RouteBindingId};
use ployz_core::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use ployz_core::intent::{RouteBindingState, VolumeKind, VolumePinState};
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::machine::{MachineUsabilityReason, StorageCapability};
use ployz_core::operation::{DeployOperationFailure, RouteHostname, RoutePort, RouteTarget};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, service_id,
};
use std::time::Duration;

#[test]
fn execution_threads_keyed_environment_revisions_end_to_end() {
    let mut first_value = deploy_request();
    let [first_service] = first_value.services.as_mut_slice() else {
        panic!("deploy request contains one service");
    };
    first_service.runtime.environment =
        ServiceEnvironment::from(std::collections::BTreeMap::from([(
            EnvName::try_new("TOKEN").expect("environment name"),
            EnvValue::try_new("first").expect("environment value"),
        )]));
    let mut second_value = first_value.clone();
    let [second_service] = second_value.services.as_mut_slice() else {
        panic!("deploy request contains one service");
    };
    second_service.runtime.environment =
        ServiceEnvironment::from(std::collections::BTreeMap::from([(
            EnvName::try_new("TOKEN").expect("environment name"),
            EnvValue::try_new("second").expect("environment value"),
        )]));

    let first = prepare_deploy_execution_command(
        operation_id("op_first"),
        first_value,
        empty_execution_facts(),
    );
    let same_operation = prepare_deploy_execution_command(
        operation_id("op_first"),
        second_value.clone(),
        empty_execution_facts(),
    );
    let next_operation = prepare_deploy_execution_command(
        operation_id("op_next"),
        second_value,
        empty_execution_facts(),
    );

    assert_ne!(
        deploy_plan(&first)
            .expect("first plan")
            .namespace_revision_id,
        deploy_plan(&same_operation)
            .expect("same operation plan")
            .namespace_revision_id
    );
    assert_eq!(
        deploy_plan(&same_operation)
            .expect("same operation plan")
            .namespace_revision_id,
        deploy_plan(&next_operation)
            .expect("next operation plan")
            .namespace_revision_id
    );
    assert_ne!(
        single_service(&first)
            .serving_target_entry_state(
                &namespace_id("default"),
                first.environment_revision_key(),
            ),
        single_service(&same_operation)
            .serving_target_entry_state(
                &namespace_id("default"),
                same_operation.environment_revision_key(),
            )
    );
    assert_eq!(
        single_service(&same_operation).serving_target_entry_state(
            &namespace_id("default"),
            same_operation.environment_revision_key(),
        ),
        single_service(&next_operation).serving_target_entry_state(
            &namespace_id("default"),
            next_operation.environment_revision_key(),
        )
    );

    let env_free_first = prepare_deploy_execution_command(
        operation_id("op_free_first"),
        deploy_request(),
        empty_execution_facts(),
    );
    let env_free_next = prepare_deploy_execution_command(
        operation_id("op_free_next"),
        deploy_request(),
        empty_execution_facts(),
    );
    assert_eq!(
        deploy_plan(&env_free_first)
            .expect("env-free first plan")
            .namespace_revision_id,
        deploy_plan(&env_free_next)
            .expect("env-free next plan")
            .namespace_revision_id
    );
}

#[test]
fn volume_backed_promoted_baseline_reuses_or_hands_off_for_every_container_shape_change() {
    let owner_machine = machine_id("machine_a");
    let owner_container = container_id("ctr_owner");
    let volume_name = VolumeName::try_new("data").expect("volume name");
    let baseline_environment_value = "baseline-environment-value-never-in-evidence";
    let changed_environment_value = "changed-environment-value-never-in-evidence";
    let mut baseline = deploy_request();
    baseline
        .volumes
        .insert(volume_name.clone(), VolumeSpec::Plain);
    let [service] = baseline.services.as_mut_slice() else {
        panic!("deploy request contains one service");
    };
    service.image = service
        .image
        .clone()
        .with_digest(&ployz_core::image::OciDigest::sha256(b"baseline image"))
        .expect("image accepts digest");
    service.runtime.environment = ServiceEnvironment::from(std::collections::BTreeMap::from([(
        EnvName::try_new("TOKEN").expect("environment name"),
        EnvValue::try_new(baseline_environment_value).expect("environment value"),
    )]));
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name.clone(),
        target: ContainerMountPath::try_new("/data").expect("mount path"),
    }];
    let pin = VolumePinState::plain(
        baseline.namespace_id.clone(),
        volume_name,
        owner_machine.clone(),
    );

    let baseline_command = prepare_deploy_execution_command(
        operation_id("op_baseline"),
        baseline.clone(),
        DeployExecutionFacts {
            namespace_volume_pins: vec![pin.clone()],
            eligible_machines: vec![owner_machine.clone()],
            ..empty_execution_facts()
        },
    );
    let promoted = single_service(&baseline_command).serving_target_entry_state(
        &baseline.namespace_id,
        baseline_command.environment_revision_key(),
    );
    let owner = cleanup_container_with_entry(
        owner_machine.as_str(),
        owner_container.as_str(),
        promoted.namespace_revision_entry_id.clone(),
    );
    let mut observed_owner = observed_service_container_with_entry(
        owner_machine.as_str(),
        owner_container.as_str(),
        promoted.namespace_revision_entry_id.clone(),
    );
    observed_owner
        .named_volume_names
        .insert(volume_name.as_str().to_owned());
    let owner_observation =
        MachineContainerObservationSnapshot::try_new(owner_machine.clone(), [observed_owner])
            .expect("running owner observation");
    let baseline_facts = || DeployExecutionFacts {
        namespace_serving_entries: vec![promoted.clone()],
        namespace_volume_pins: vec![pin.clone()],
        eligible_machines: vec![owner_machine.clone()],
        observed_machines: vec![owner_observation.clone()],
        ..empty_execution_facts()
    };

    let unchanged_command = prepare_deploy_execution_command(
        operation_id("op_unchanged"),
        baseline.clone(),
        baseline_facts(),
    );
    let unchanged_plan = deploy_plan(&unchanged_command).expect("unchanged baseline plans");
    let [unchanged_phase] = unchanged_plan.phases.as_slice() else {
        panic!("one phase");
    };
    let [unchanged_service] = unchanged_phase.services.as_slice() else {
        panic!("one service");
    };
    assert_eq!(
        &unchanged_service.work,
        &ployz_core::deploy::DeployServiceWork::Ordinary {
            steps: vec![ployz_core::deploy::DeployPlanStep::UseExistingContainer {
                machine_id: owner_machine.clone(),
                container_id: owner_container.clone(),
                slot: ployz_core::deploy::ReplicaSlot::Replicated {
                    number: ployz_core::deploy::ReplicatedReplicaSlot::try_new(1)
                        .expect("replica slot"),
                },
            }]
        }
    );
    assert!(unchanged_plan.cleanup_actions.is_empty());
    let unchanged_serialized = serde_json::to_string(&(
        &unchanged_plan,
        DeployRequestEvidence::from_request(&baseline),
    ))
    .expect("unchanged evidence serializes");
    assert!(!unchanged_serialized.contains(baseline_environment_value));

    let assert_replacement = |operation: &str, request: DeployRequest| {
        let evidence = DeployRequestEvidence::from_request(&request);
        let command =
            prepare_deploy_execution_command(operation_id(operation), request, baseline_facts());
        let plan = deploy_plan(&command).expect("replacement plans");
        let [phase] = plan.phases.as_slice() else {
            panic!("one phase");
        };
        let [service] = phase.services.as_slice() else {
            panic!("one service");
        };
        let ployz_core::deploy::DeployServiceWork::VolumeHandoff {
            replacement,
            remaining_steps,
            participants,
        } = &service.work
        else {
            panic!("replacement needs a volume handoff")
        };
        assert_eq!(replacement.machine_id, owner_machine);
        assert!(remaining_steps.is_empty());
        assert_eq!(
            participants
                .as_slice()
                .iter()
                .map(|participant| &participant.target)
                .collect::<Vec<_>>(),
            [&owner.target]
        );
        assert!(matches!(
            participants.as_slice(),
            [ployz_core::deploy::DeployVolumeHandoffParticipant {
                prior_state: ployz_core::deploy::DeployVolumeHandoffPriorState::Running,
                shared_volume_names,
                ..
            }] if shared_volume_names.as_slice() == [volume_name.clone()]
        ));
        let serialized = serde_json::to_string(&(plan, evidence)).expect("evidence serializes");
        assert!(!serialized.contains(baseline_environment_value));
        assert!(!serialized.contains(changed_environment_value));
    };

    let mut environment_changed = baseline.clone();
    let [service] = environment_changed.services.as_mut_slice() else {
        panic!("one service");
    };
    service.runtime.environment = ServiceEnvironment::from(std::collections::BTreeMap::from([(
        EnvName::try_new("TOKEN").expect("environment name"),
        EnvValue::try_new(changed_environment_value).expect("environment value"),
    )]));
    assert_replacement("op_environment_changed", environment_changed);

    let mut image_changed = baseline.clone();
    let [service] = image_changed.services.as_mut_slice() else {
        panic!("one service");
    };
    service.image = ImageReference::try_new("registry.example/api:rev_3")
        .expect("image")
        .with_digest(&ployz_core::image::OciDigest::sha256(b"changed image"))
        .expect("image accepts digest");
    assert_replacement("op_image_changed", image_changed);

    let mut runtime_changed = baseline;
    let [service] = runtime_changed.services.as_mut_slice() else {
        panic!("one service");
    };
    service.runtime.restart_policy = ContainerRestartPolicy::Always;
    assert_replacement("op_runtime_changed", runtime_changed);
}

fn empty_execution_facts() -> DeployExecutionFacts {
    DeployExecutionFacts {
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        namespace_volume_pins: Vec::new(),
        eligible_machines: vec![machine_id("machine_a")],
        unusable_machines: Vec::new(),
        dataplane_members: Vec::new(),
        observed_machines: Vec::new(),
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    }
}

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

#[test]
fn replicated_scale_change_reuses_equivalent_promoted_container() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("one service")
    };
    service.mode = ServiceMode::Replicated {
        replicas: ReplicaCount::try_new(3).expect("replicas"),
    };
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Replicated {
        replicas: ReplicaCount::try_new(1).expect("replicas"),
    };
    let facts = facts_with_target_observation(vec![machine_id("machine_a")], vec![promoted]);

    let command = prepare_deploy_execution_command(operation_id("op_scale"), request, facts);

    assert_eq!(
        single_service(&command).existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_target"),
            creation_gate: ployz_core::deploy::ExistingReplicaCreationGate::AlreadyPassed,
        }]
    );
}

#[test]
fn replicated_to_global_reuses_equivalent_container_on_selected_machine() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("one service")
    };
    service.mode = ServiceMode::Global;
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Replicated {
        replicas: ReplicaCount::try_new(1).expect("replicas"),
    };
    let facts = facts_with_target_observation(vec![machine_id("machine_a")], vec![promoted]);

    let command = prepare_deploy_execution_command(operation_id("op_global"), request, facts);
    let plan = deploy_plan(&command).expect("global plan");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };
    assert!(matches!(
        &service.work,
        ployz_core::deploy::DeployServiceWork::Ordinary { steps }
            if matches!(
                steps.as_slice(),
                [ployz_core::deploy::DeployPlanStep::UseExistingContainer {
                    machine_id: step_machine_id,
                    container_id: existing_container_id,
                    slot: ployz_core::deploy::ReplicaSlot::Global,
                }] if step_machine_id == &machine_id("machine_a")
                    && existing_container_id == &container_id("ctr_target")
            )
    ));
}

#[test]
fn global_empty_exception_requires_an_equivalent_global_serving_target() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("one service")
    };
    service.mode = ServiceMode::Global;
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Replicated {
        replicas: ReplicaCount::try_new(1).expect("replicas"),
    };
    let replicated_command = prepare_deploy_execution_command(
        operation_id("op_switch"),
        request.clone(),
        facts_with_target_observation(Vec::new(), vec![promoted.clone()]),
    );
    assert!(matches!(
        deploy_plan(&replicated_command),
        Err(DeployExecutionError::Plan(
            ployz_core::deploy::DeployPlanError::NoEligibleMachines { .. }
        ))
    ));

    promoted.mode = ServiceMode::Global;
    let global_command = prepare_deploy_execution_command(
        operation_id("op_already_global"),
        request,
        facts_with_target_observation(Vec::new(), vec![promoted]),
    );
    assert!(deploy_plan(&global_command).is_ok());
}

#[test]
fn observation_only_unusable_machine_is_not_a_global_candidate() {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("one service")
    };
    service.mode = ServiceMode::Global;
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Global;
    let mut facts = facts_with_target_observation(vec![machine_id("machine_a")], vec![promoted]);
    facts.observed_machines = vec![
        MachineContainerObservationSnapshot::try_new(
            machine_id("machine_observed"),
            [observed_service_container_with_entry(
                "machine_observed",
                "ctr_observed",
                target_namespace_revision_entry_id(),
            )],
        )
        .expect("valid observation"),
    ];

    let command = prepare_deploy_execution_command(operation_id("op_global"), request, facts);
    let plan = deploy_plan(&command).expect("global plan");
    let [phase] = plan.phases.as_slice() else {
        panic!("one phase")
    };
    let [service] = phase.services.as_slice() else {
        panic!("one service")
    };
    assert_eq!(
        service.placement,
        ployz_core::deploy::DeployServicePlacement::Global {
            candidates: vec![machine_id("machine_a")],
            selected: vec![machine_id("machine_a")],
            deferred: Vec::new(),
            draining: Vec::new(),
        }
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
    let entry_id =
        service.namespace_revision_entry_id(&request.namespace_id, &environment_revision_key());
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

#[test]
fn receipt_platform_rejections_are_scoped_to_the_service_that_requires_them() {
    let amd64 = ployz_core::image::OciPlatform::try_new("linux", "amd64").expect("platform");
    let arm64 = ployz_core::image::OciPlatform::try_new("linux", "arm64").expect("platform");
    let mut request = deploy_request();
    let registry_service = DeployServiceSpec {
        service_id: service_id("svc_registry"),
        ..request.services.remove(0)
    };
    request.services = vec![pushed_service("svc_arm", arm64.clone()), registry_service];
    let machine = machine_id("machine_amd64");
    let command = prepare_deploy_execution_command(
        operation_id("op_platform_scope"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::from([(machine.clone(), amd64.clone())]),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine.clone()],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    );

    assert!(command.unusable_machines().is_empty());
    let arm = command
        .service(&service_id("svc_arm"))
        .expect("arm service is prepared");
    assert!(arm.eligible_machines().is_empty());
    assert_eq!(
        arm.unusable_machines(),
        [ployz_core::operation::UnusableMachine {
            machine_id: machine.clone(),
            reason: MachineUsabilityReason::PlatformMismatch {
                supported: ployz_core::build::BuildPlatforms::try_new([arm64])
                    .expect("one supported platform"),
                reported: amd64,
            },
        }]
    );
    let registry = command
        .service(&service_id("svc_registry"))
        .expect("registry service is prepared");
    assert_eq!(registry.eligible_machines(), [machine]);
    assert!(registry.unusable_machines().is_empty());

    let failure =
        DeployExecutionError::Plan(ployz_core::deploy::DeployPlanError::NoEligibleMachines {
            service_id: service_id("svc_arm"),
        })
        .deploy_failure(&command, Vec::new());
    assert!(matches!(
        failure,
        DeployOperationFailure::NoUsableMachines { reasons }
            if reasons == arm.unusable_machines()
    ));
}

#[test]
fn machine_scoped_volume_admission_keeps_typed_operation_failure() {
    let machine = machine_id("machine_a");
    let command = prepare_deploy_execution_command(
        operation_id("op_volume_admission"),
        deploy_request(),
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine.clone()],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    );
    let capacity = VolumeAdmissionFailure::CapacityExceeded {
        total_bytes: 24_000,
        provisioned_used_bytes: 4_000,
        free_bytes: 5_000,
        required_headroom_bytes: 10_000,
        requested_total_bytes: 10_000,
    };

    assert_eq!(
        DeployExecutionError::Plan(
            ployz_core::deploy::DeployPlanError::VolumeAdmissionOnMachine {
                service_id: service_id("svc_api"),
                machine_id: machine.clone(),
                failure: Box::new(capacity.clone()),
            }
        )
        .deploy_failure(&command, Vec::new()),
        DeployOperationFailure::VolumeAdmissionFailed {
            service_id: service_id("svc_api"),
            machine_id: machine,
            failure: Box::new(capacity),
        }
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
    assert!(provisioned.unusable_machines().is_empty());
    for service in provisioned.services() {
        let [unusable] = service.unusable_machines() else {
            panic!("legacy machine is the sole service-scoped unusable candidate");
        };
        assert_eq!(
            unusable.reason,
            MachineUsabilityReason::StorageTestimonyNotReported
        );
    }
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
        single_service(&command).unusable_machines(),
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

fn facts_with_target_observation(
    eligible_machines: Vec<ployz_core::ids::MachineId>,
    namespace_serving_entries: Vec<ployz_core::intent::ServingTargetEntry>,
) -> DeployExecutionFacts {
    DeployExecutionFacts {
        machine_platforms: std::collections::BTreeMap::new(),
        seed_clock_testimony: std::collections::BTreeMap::new(),
        machine_storage_testimony: std::collections::BTreeMap::new(),
        unusable_machines: Vec::new(),
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries,
        namespace_volume_pins: Vec::new(),
        eligible_machines,
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
            .expect("valid observation"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        automatic_hostname_mode: AutomaticHostnameMode::Disabled,
        gateway_certificate_targets: Vec::new(),
        ployz_gateway_certificate_targets: Vec::new(),
        step_timeout: Duration::from_secs(5),
    }
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
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn pushed_service(service: &str, platform: ployz_core::image::OciPlatform) -> DeployServiceSpec {
    let receipt = PushedImageReceipt::try_new([(
        platform,
        PlatformImage {
            seed: machine_id("machine_seed"),
            manifest_digest: ployz_core::image::OciDigest::sha256(b"manifest"),
            image_id: ployz_core::image::OciDigest::sha256(b"image"),
            availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                .expect("expiry"),
        },
    )])
    .expect("receipt");
    DeployServiceSpec {
        keep: None,
        service_id: service_id(service),
        image: ImageReference::try_new(format!("local/{service}:build"))
            .expect("image")
            .with_digest(receipt.index_digest())
            .expect("pinned image"),
        image_source: ImageSource::PushedToSeed(receipt),
        mode: ployz_core::deploy::ServiceMode::Replicated {
            replicas: ReplicaCount::try_new(1).expect("replicas"),
        },
        runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        pre_start: None,
        depends_on: Vec::new(),
        routes: Vec::new(),
    }
}

fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    let request = deploy_request();
    let [service] = request.services.as_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.namespace_revision_entry_id(&namespace_id("default"), &environment_revision_key())
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
