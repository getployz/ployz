use super::*;
use crate::control::intent::ingress_intent::IngressIntentStore;
use crate::control::store::CoreStore;
use ployz_core::build::{BuildAdapter, BuildCacheScope, BuildPlatforms, GitSource};
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec,
    ImageReference, ImageSource, ReplicaCount,
};
use ployz_core::image::OciPlatform;
use ployz_core::ingress::{
    AutomaticHostnameConfiguration, AutomaticHostnameLabel, IngressConfiguration,
    PloyzDnsTargetIntent,
};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::operation::RoutePort;
use ployz_test_support::ids::{
    idempotency_key, machine_id, namespace_id, operation_id, service_id,
};

fn machine_update_command(operation: &str) -> MachineUpdateSubmitCommand {
    MachineUpdateSubmitCommand {
        operation_id: operation_id(operation),
        machine_id: machine_id("machine-a"),
        target_version: InstallArtifactVersion::try_new("0.2.0").expect("valid version"),
    }
}

fn storage_prepare_command(operation: &str) -> MachineStoragePrepareSubmitCommand {
    MachineStoragePrepareSubmitCommand {
        operation_id: operation_id(operation),
        machine_id: machine_id("machine-a"),
        requested_pool: None,
    }
}

fn build_cache_prune_command(operation: &str) -> MachineBuildCachePruneSubmitCommand {
    MachineBuildCachePruneSubmitCommand {
        operation_id: operation_id(operation),
        machine_id: machine_id("machine-a"),
    }
}

fn build_submit_command(operation: &str) -> BuildSubmitCommand {
    BuildSubmitCommand {
        operation_id: operation_id(operation),
        source: GitSource::try_new(
            "https://example.com/repository.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "private-token",
            None::<String>,
        )
        .expect("valid git source"),
        adapter: BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("build-scope").expect("valid cache scope"),
        },
        platforms: BuildPlatforms::try_new([
            OciPlatform::try_new("linux", "amd64").expect("valid platform")
        ])
        .expect("non-empty platforms"),
    }
}

#[tokio::test]
async fn build_submit_records_redacted_accepted_evidence_before_execution() {
    let (_nats, controllers, _intent) = test_controllers().await;
    let command = build_submit_command("build_accepted");

    let accepted = controllers
        .submit_build(command)
        .await
        .expect("build submits");
    let status = controllers
        .repository()
        .get(&accepted.submission.operation_id)
        .await
        .expect("status reads")
        .expect("status exists");

    let OperationStatus::Build { source, state, .. } = status else {
        panic!("build submission must project build status");
    };
    assert!(matches!(
        state,
        ployz_core::operation::BuildOperationState::Accepted
    ));
    assert!(
        !serde_json::to_string(&source)
            .expect("source evidence serializes")
            .contains("private-token")
    );
    assert!(accepted.submission.should_start_execution);
}

#[tokio::test]
async fn build_submit_rejects_same_operation_id_with_different_request() {
    let (_nats, controllers, _intent) = test_controllers().await;
    let original = build_submit_command("build_conflict");
    controllers
        .submit_build(original)
        .await
        .expect("original submits");
    let mut conflicting = build_submit_command("build_conflict");
    conflicting.platforms =
        BuildPlatforms::try_new([OciPlatform::try_new("linux", "arm64").expect("valid platform")])
            .expect("non-empty platforms");

    assert!(matches!(
        controllers.submit_build(conflicting).await,
        Err(SubmitCommandError::Submit(
            SubmitOperationError::DuplicateSequenceMismatch { .. }
        ))
    ));
}

#[tokio::test]
async fn update_and_storage_prepare_conflict_in_both_directions() {
    let (_nats, controllers, _intent) = test_controllers().await;
    controllers
        .submit_machine_update(machine_update_command("op_update"))
        .await
        .expect("update submits");
    assert!(matches!(
        controllers
            .submit_machine_storage_prepare(storage_prepare_command("op_prepare"))
            .await,
        Err(SubmitCommandError::MachineSubstrateBusy { owner, .. })
            if owner == operation_id("op_update")
    ));

    controllers
        .release_machine(&machine_id("machine-a"), &operation_id("op_update"))
        .await;
    controllers
        .submit_machine_storage_prepare(storage_prepare_command("op_prepare"))
        .await
        .expect("storage prepare submits");
    assert!(matches!(
        controllers
            .submit_machine_update(machine_update_command("op_next_update"))
            .await,
        Err(SubmitCommandError::MachineSubstrateBusy { owner, .. })
            if owner == operation_id("op_prepare")
    ));
}

#[tokio::test]
async fn build_cache_prune_uses_the_machine_substrate_fence() {
    let (_nats, controllers, _intent) = test_controllers().await;
    controllers
        .submit_machine_update(machine_update_command("op_update"))
        .await
        .expect("update submits");
    assert!(matches!(
        controllers
            .submit_machine_build_cache_prune(build_cache_prune_command("op_prune"))
            .await,
        Err(SubmitCommandError::MachineSubstrateBusy { owner, .. })
            if owner == operation_id("op_update")
    ));

    controllers
        .release_machine(&machine_id("machine-a"), &operation_id("op_update"))
        .await;
    controllers
        .submit_machine_build_cache_prune(build_cache_prune_command("op_prune"))
        .await
        .expect("prune submits");
    assert!(matches!(
        controllers
            .submit_machine_storage_prepare(storage_prepare_command("op_prepare"))
            .await,
        Err(SubmitCommandError::MachineSubstrateBusy { owner, .. })
            if owner == operation_id("op_prune")
    ));
}

#[tokio::test]
async fn build_cache_prune_round_trips_completed_evidence() {
    let (_nats, controllers, _intent) = test_controllers().await;
    let accepted = controllers
        .submit_machine_build_cache_prune(build_cache_prune_command("op_prune"))
        .await
        .expect("prune submits");
    controllers
        .repository()
        .record_machine_build_cache_prune_transition(
            &accepted.operation_id,
            &accepted.machine_id,
            ployz_core::operation::MachineBuildCachePruneTransition::Pruning,
        )
        .await
        .expect("pruning records");
    controllers
        .repository()
        .record_machine_build_cache_prune_transition(
            &accepted.operation_id,
            &accepted.machine_id,
            ployz_core::operation::MachineBuildCachePruneTransition::Completed {
                evidence: ployz_core::operation::BuildCachePruneEvidence {
                    before_available_bytes: 10,
                    reclaimed_bytes: 5,
                    after_available_bytes: 15,
                },
            },
        )
        .await
        .expect("completion records");

    assert!(matches!(
        controllers
            .repository()
            .get(&accepted.operation_id)
            .await
            .expect("status reads"),
        Some(OperationStatus::MachineBuildCachePrune {
            state: ployz_core::operation::MachineBuildCachePruneOperationState::Completed {
                evidence: ployz_core::operation::BuildCachePruneEvidence {
                    before_available_bytes: 10,
                    reclaimed_bytes: 5,
                    after_available_bytes: 15,
                }
            },
            ..
        })
    ));
}

#[tokio::test]
async fn machine_update_fence_rejects_a_different_owner() {
    let (_nats, controllers, _intent) = test_controllers().await;
    controllers
        .submit_machine_update(machine_update_command("op_original"))
        .await
        .expect("original update submits");
    controllers
        .release_machine(&machine_id("machine-a"), &operation_id("op_not_owner"))
        .await;

    let error = controllers
        .submit_machine_update(machine_update_command("op_conflicting"))
        .await
        .expect_err("a second substrate operation is fenced");

    assert!(matches!(
        error,
        SubmitCommandError::MachineSubstrateBusy { machine_id: busy_machine, owner }
            if busy_machine == machine_id("machine-a") && owner == operation_id("op_original")
    ));
}

#[tokio::test]
async fn machine_fence_releases_when_repository_submission_fails() {
    let (_nats, controllers, _intent) = test_controllers().await;
    let original = machine_update_command("op_reused");
    controllers
        .submit_machine_update(original.clone())
        .await
        .expect("original update submits");
    controllers
        .release_machine(&original.machine_id, &original.operation_id)
        .await;

    let error = controllers
        .submit_machine_storage_prepare(storage_prepare_command("op_reused"))
        .await;
    assert!(
        matches!(
            error,
            Err(SubmitCommandError::Submit(
                SubmitOperationError::DuplicateSequenceMismatch { .. }
            ))
        ),
        "unexpected submission result: {error:?}"
    );
    controllers
        .submit_machine_update(machine_update_command("op_after_error"))
        .await
        .expect("repository error released its acquired machine fence");

    let (_nats, controllers, _intent) = test_controllers().await;
    let original = storage_prepare_command("op_reused");
    controllers
        .submit_machine_storage_prepare(original.clone())
        .await
        .expect("original storage preparation submits");
    controllers
        .release_machine(&original.machine_id, &original.operation_id)
        .await;
    let error = controllers
        .submit_machine_update(machine_update_command("op_reused"))
        .await;
    assert!(
        matches!(
            error,
            Err(SubmitCommandError::Submit(
                SubmitOperationError::DuplicateSequenceMismatch { .. }
            ))
        ),
        "unexpected submission result: {error:?}"
    );
    controllers
        .submit_machine_storage_prepare(storage_prepare_command("op_after_error"))
        .await
        .expect("repository error released its acquired machine fence");
}

#[tokio::test]
async fn idempotent_machine_update_replay_releases_its_new_claim() {
    let (_nats, controllers, _intent) = test_controllers().await;
    let command = machine_update_command("op_original");
    let accepted = controllers
        .submit_machine_update(command.clone())
        .await
        .expect("original update submits");
    controllers
        .repository()
        .record_machine_update_transition(
            &accepted.operation_id,
            &accepted.machine_id,
            ployz_core::operation::MachineUpdateTransition::Running,
        )
        .await
        .expect("running transition records");
    controllers
        .repository()
        .record_machine_update_transition(
            &accepted.operation_id,
            &accepted.machine_id,
            ployz_core::operation::MachineUpdateTransition::Completed {
                reported: ployz_core::operation::MachineSubstrateVersions::default(),
            },
        )
        .await
        .expect("completed transition records");
    controllers
        .release_machine(&command.machine_id, &command.operation_id)
        .await;

    let replay = controllers
        .submit_machine_update(command)
        .await
        .expect("terminal replay is accepted idempotently");
    assert!(!replay.should_start_execution);
    controllers
        .submit_machine_update(machine_update_command("op_next"))
        .await
        .expect("replay released its newly acquired claim");
}

#[test]
fn deploy_reservation_expires_at_its_deadline() {
    let namespace_id = NamespaceId::try_new("default").expect("valid namespace id");
    let reservation_id = DeployReservationId::first();
    let expired_at = DeployReservationExpiresAt::try_new(10).expect("valid expiration");
    let mut reservations = BTreeMap::from([(
        namespace_id.clone(),
        BTreeMap::from([(reservation_id, expired_at)]),
    )]);

    let error = validate_deploy_reservation_at(
        &mut reservations,
        &namespace_id,
        reservation_id,
        expired_at.unix_seconds(),
    )
    .expect_err("reservation expires at its deadline");

    assert!(matches!(
        error,
        SubmitCommandError::ReservationExpired {
            namespace_id: expired_namespace,
            reservation_id: expired_reservation,
            expired_at: deadline,
        } if expired_namespace == namespace_id
            && expired_reservation == reservation_id
            && deadline == expired_at
    ));
}

#[tokio::test]
async fn duplicate_deploy_submission_keeps_ingress_fence_owned_by_original() {
    let (_nats, controllers, intent) = test_controllers().await;
    intent
        .replace(test_ingress_configuration())
        .await
        .expect("ingress configuration stores");
    let reservation_id = controllers
        .reserve_deploy(&namespace_id("default"))
        .await
        .expect("deploy reservation issues")
        .reservation_id;
    let command = DeploySubmitCommand {
        operation_id: operation_id("op_original"),
        idempotency_key: idempotency_key("idem_original"),
        reservation_id,
        target: automatic_deploy_request(),
        registry_credentials: BTreeMap::new(),
    };

    controllers
        .submit_deploy(command.clone())
        .await
        .expect("original deploy submits");
    controllers
        .submit_deploy(command)
        .await
        .expect("duplicate deploy submits idempotently");
    let conflicting_reservation = controllers
        .reserve_deploy(&namespace_id("default"))
        .await
        .expect("conflicting deploy reservation issues")
        .reservation_id;
    let error = controllers
        .submit_deploy(DeploySubmitCommand {
            operation_id: operation_id("op_conflicting"),
            idempotency_key: idempotency_key("idem_conflicting"),
            reservation_id: conflicting_reservation,
            target: automatic_deploy_request(),
            registry_credentials: BTreeMap::new(),
        })
        .await
        .expect_err("conflicting deploy remains fenced");

    assert!(matches!(
        error,
        SubmitCommandError::IngressBusy { owner, .. }
            if owner == operation_id("op_original")
    ));
}

#[tokio::test]
async fn duplicate_ingress_configure_submission_keeps_fence_owned_by_original() {
    let (_nats, controllers, intent) = test_controllers().await;
    let command = IngressConfigureSubmitCommand {
        operation_id: operation_id("op_original"),
        configuration: test_ingress_configuration(),
    };

    controllers
        .submit_ingress_configure(command.clone(), &intent)
        .await
        .expect("original ingress configure submits");
    controllers
        .submit_ingress_configure(command, &intent)
        .await
        .expect("duplicate ingress configure submits idempotently");
    let error = controllers
        .submit_ingress_configure(
            IngressConfigureSubmitCommand {
                operation_id: operation_id("op_conflicting"),
                configuration: test_ingress_configuration(),
            },
            &intent,
        )
        .await
        .expect_err("conflicting ingress configure remains fenced");

    assert!(matches!(
        error,
        IngressConfigureSubmitError::Busy { owner }
            if owner == operation_id("op_original")
    ));
}

async fn test_controllers() -> (
    ployz_test_support::nats::TestNats,
    OperationControllers,
    IngressIntentStore,
) {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store opens");
    let intent = IngressIntentStore::new(core_store.clone());
    let controllers = OperationControllers::new(
        OperationRepository::open(core_store, nats.controller.clone()),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(crate::config::DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    );
    (nats, controllers, intent)
}

fn test_ingress_configuration() -> IngressConfiguration {
    IngressConfiguration::try_new(
        AutomaticHostnameConfiguration::custom("apps.example.com")
            .expect("automatic hostname suffix is valid"),
        PloyzDnsTargetIntent::Disabled,
    )
    .expect("valid ingress configuration")
}

fn automatic_deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:latest")
                .expect("image reference is valid"),
            image_source: ImageSource::Registry,
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(1).expect("replica count is valid"),
            },
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::AutoHostname {
                    label: AutomaticHostnameLabel::try_new("api")
                        .expect("automatic hostname label is valid"),
                },
                endpoint_port: RoutePort::try_new(8080).expect("route port is valid"),
            }],
        }],
    }
}
