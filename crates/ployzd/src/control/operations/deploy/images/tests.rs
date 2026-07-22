use super::*;
use ployz_core::deploy::{
    ContainerRuntimeSpec, DeployPhasePlan, DeployPlanStep, DeployPlanningInput,
    DeployPlanningPlacementInput, DeployRequest, DeployServiceSpec, ImageReference, PlatformImage,
    PushedImageReceipt, ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::{
    MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId, ServiceId,
    StepId,
};
use ployz_core::image::{OciDigest, OciPlatform};
use ployz_core::machine::runtime::{ManagedContainerIdentity, ManagedContainerKind};

use crate::control::operations::deploy::types::ServingIntentDisposition;
use crate::control::role_client::machine::MachineClockTestimony;
use crate::tests::deploy_operation::fixtures::RecordingRuntime;

#[tokio::test(start_paused = true)]
async fn registry_resolution_obeys_the_supplied_step_timeout() {
    let service_id = ServiceId::try_new("api").expect("service id");
    let machine_id = MachineId::try_new("machine_a").expect("machine id");
    let requested = ImageReference::try_new("nginx:1.27-alpine").expect("image reference");
    let mut runtime = RecordingRuntime::with_containers([]).with_hanging_resolution();

    let result = resolve_registry_image(
        &service_id,
        &requested,
        &machine_id,
        None,
        std::time::Duration::from_millis(50),
        &mut runtime,
    )
    .await;
    assert!(matches!(
        result,
        Err(ImagePreparationError::ResolutionTimedOut { .. })
    ));
}

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

    let error = machine_image_reference(
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
        environment_revision_key: {
            let seed = ployz_core::nats_config::NatsUserSeed::try_new(
                "SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ",
            )
            .expect("valid deterministic controller seed");
            ployz_core::deploy::EnvironmentRevisionKey::derive_from_controller_seed(&seed)
        },
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
                    work: ployz_core::deploy::DeployServiceWork::Ordinary {
                        steps: vec![DeployPlanStep::RunContainer {
                            machine_id: target_machine.clone(),
                            slot: ReplicaSlot::Replicated {
                                number: ployz_core::deploy::ReplicatedReplicaSlot::try_new(1)
                                    .expect("replica slot"),
                            },
                        }],
                    },
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

fn test_digest(value: char) -> OciDigest {
    OciDigest::try_new(format!("sha256:{}", value.to_string().repeat(64))).expect("digest")
}

fn coordinator_job(machine: &str, step: &str) -> ImageEnsureJob {
    let service = pushed_service();
    let image = ImageReference::try_new(format!("registry.example/api@{}", test_digest('9')))
        .expect("image");
    ImageEnsureJob {
        machine_id: machine_id(machine),
        owner: ManagedContainerIdentity {
            namespace_id: NamespaceId::try_new("default").expect("namespace"),
            service_id: service.service.service_id.clone(),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry-a")
                .expect("entry"),
            operation_id: OperationId::try_new("operation_a").expect("operation"),
            step_id: StepId::try_new(step).expect("step"),
            kind: ManagedContainerKind::Service,
        },
        source: ImageEnsureSource::Registry {
            reference: image,
            platform: platform("amd64"),
            credential: None,
        },
        service,
        target_machine: machine_id(machine),
        platform: platform("amd64"),
        seed: None,
        evidence_targets: vec![machine_id(machine)],
    }
}

fn completed_status() -> ImageEnsureStatus {
    ImageEnsureStatus::Completed {
        reference: ImageReference::try_new(format!("registry.example/api@{}", test_digest('9')))
            .expect("image"),
    }
}

#[tokio::test]
async fn ensure_wave_starts_every_independent_job_before_polling_status() {
    let jobs = vec![
        coordinator_job("machine_a", "run_a"),
        coordinator_job("machine_b", "run_b"),
    ];
    let mut runtime = RecordingRuntime::with_containers([]).with_image_ensure_script([
        Ok(ImageEnsureStatus::Accepted),
        Ok(ImageEnsureStatus::Accepted),
        Ok(completed_status()),
        Ok(completed_status()),
    ]);

    let completed = run_image_ensure_wave(&mut runtime, &jobs)
        .await
        .expect("wave completes");

    assert_eq!(completed.len(), 2);
    let image_ensures = runtime.image_ensures();
    let [first_start, second_start, first_status, second_status] = image_ensures.as_slice() else {
        panic!("two jobs must each start and report status");
    };
    assert!(
        [first_start, second_start]
            .iter()
            .all(|(_, request)| matches!(request, ImageEnsureRequest::Start { .. }))
    );
    assert!(
        [first_status, second_status]
            .iter()
            .all(|(_, request)| matches!(request, ImageEnsureRequest::Status { .. }))
    );
}

#[tokio::test]
async fn ensure_wave_failure_cancels_every_unfinished_peer() {
    let jobs = vec![
        coordinator_job("machine_a", "run_a"),
        coordinator_job("machine_b", "run_b"),
    ];
    let mut runtime = RecordingRuntime::with_containers([]).with_image_ensure_script([
        Ok(ImageEnsureStatus::Accepted),
        Ok(ImageEnsureStatus::Accepted),
        Ok(ImageEnsureStatus::Failed {
            failure: ImageEnsureFailure::PullFailed {
                message: FailureMessage::try_new("pull failed").expect("message"),
            },
        }),
    ]);

    let (failed_index, _) = run_image_ensure_wave(&mut runtime, &jobs)
        .await
        .expect_err("first job fails");

    assert_eq!(failed_index, 0);
    assert!(runtime.image_ensures().iter().any(|(machine, request)| {
        machine == &machine_id("machine_b") && matches!(request, ImageEnsureRequest::Cancel { .. })
    }));
}

#[tokio::test]
async fn uncertain_start_acknowledgement_is_cancelled_before_the_wave_fails() {
    let jobs = vec![coordinator_job("machine_a", "run_a")];
    let mut runtime =
        RecordingRuntime::with_containers([]).with_image_ensure_script(std::iter::repeat_n(
            Err(crate::roles::machine::MachineRuntimeUnavailableReason::RequestTimedOut),
            3,
        ));

    run_image_ensure_wave(&mut runtime, &jobs)
        .await
        .expect_err("lost start acknowledgement fails the wave");

    assert!(runtime.image_ensures().iter().any(|(machine, request)| {
        machine == &machine_id("machine_a") && matches!(request, ImageEnsureRequest::Cancel { .. })
    }));
}

#[tokio::test]
async fn duplicate_target_jobs_start_once() {
    let mut jobs = Vec::new();
    push_unique_target_job(&mut jobs, coordinator_job("machine_a", "run_a"));
    push_unique_target_job(&mut jobs, coordinator_job("machine_a", "run_a"));
    let mut runtime = RecordingRuntime::with_containers([]);

    run_image_ensure_wave(&mut runtime, &jobs)
        .await
        .expect("deduplicated wave completes");

    assert_eq!(jobs.len(), 1);
    assert_eq!(
        runtime
            .image_ensures()
            .iter()
            .filter(|(_, request)| matches!(request, ImageEnsureRequest::Start { .. }))
            .count(),
        1
    );
}

#[derive(Clone, Copy)]
enum ConcurrentEnsureMode {
    SlowFailingStart,
    SlowCancels,
}

#[derive(Clone)]
struct ConcurrentEnsureRuntime {
    mode: ConcurrentEnsureMode,
    requests: std::sync::Arc<std::sync::Mutex<Vec<(MachineId, &'static str)>>>,
    active_cancels: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    maximum_cancels: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ConcurrentEnsureRuntime {
    fn new(mode: ConcurrentEnsureMode) -> Self {
        Self {
            mode,
            requests: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            active_cancels: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            maximum_cancels: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn request_actions(&self) -> Vec<(MachineId, &'static str)> {
        self.requests.lock().expect("requests").clone()
    }
}

impl MachineImageEnsureRuntime for ConcurrentEnsureRuntime {
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        request: ImageEnsureRequest,
    ) -> Result<ployz_core::image::ImageEnsureOk, MachineImageEnsureError> {
        let action = match &request {
            ImageEnsureRequest::Start { .. } => "start",
            ImageEnsureRequest::Status { .. } => "status",
            ImageEnsureRequest::Cancel { .. } => "cancel",
        };
        self.requests
            .lock()
            .expect("requests")
            .push((machine_id.clone(), action));
        let status = match (self.mode, request) {
            (ConcurrentEnsureMode::SlowFailingStart, ImageEnsureRequest::Start { .. })
                if machine_id.as_str() == "machine_a" =>
            {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                return Err(MachineImageEnsureError::Domain {
                    machine_id: machine_id.clone(),
                    error: ImageRpcDomainError::InvalidRequest {
                        message: FailureMessage::try_new("synthetic rejected start")
                            .expect("message"),
                    },
                });
            }
            (ConcurrentEnsureMode::SlowFailingStart, ImageEnsureRequest::Start { .. })
            | (ConcurrentEnsureMode::SlowCancels, ImageEnsureRequest::Start { .. }) => {
                ImageEnsureStatus::Accepted
            }
            (ConcurrentEnsureMode::SlowCancels, ImageEnsureRequest::Status { .. })
                if machine_id.as_str() == "machine_a" =>
            {
                ImageEnsureStatus::Failed {
                    failure: ImageEnsureFailure::PullFailed {
                        message: FailureMessage::try_new("synthetic pull failure")
                            .expect("message"),
                    },
                }
            }
            (ConcurrentEnsureMode::SlowCancels, ImageEnsureRequest::Status { .. }) => {
                std::future::pending().await
            }
            (ConcurrentEnsureMode::SlowCancels, ImageEnsureRequest::Cancel { .. }) => {
                let active = self
                    .active_cancels
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.maximum_cancels
                    .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                self.active_cancels
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                ImageEnsureStatus::Cancelled
            }
            (ConcurrentEnsureMode::SlowFailingStart, ImageEnsureRequest::Cancel { .. }) => {
                ImageEnsureStatus::Cancelled
            }
            (ConcurrentEnsureMode::SlowFailingStart, ImageEnsureRequest::Status { .. }) => {
                completed_status()
            }
        };
        Ok(ployz_core::image::ImageEnsureOk {
            machine_id: machine_id.clone(),
            ensure_status: status,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn slow_failing_start_does_not_block_other_starts() {
    let runtime = ConcurrentEnsureRuntime::new(ConcurrentEnsureMode::SlowFailingStart);
    let observer = runtime.clone();
    let task = tokio::spawn(async move {
        let jobs = vec![
            coordinator_job("machine_a", "run_a"),
            coordinator_job("machine_b", "run_b"),
        ];
        run_image_ensure_wave_with_runtime(runtime, &jobs).await
    });
    tokio::task::yield_now().await;
    let starts = observer
        .request_actions()
        .into_iter()
        .filter(|(_, action)| *action == "start")
        .count();
    assert_eq!(
        starts, 2,
        "both Starts are dispatched before the slow failure"
    );
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let (index, _) = task.await.expect("task joins").expect_err("start fails");
    assert_eq!(index, 0);
    let requests = observer.request_actions();
    assert!(
        requests
            .iter()
            .any(|(machine, action)| machine.as_str() == "machine_b" && *action == "cancel")
    );
    assert!(
        !requests
            .iter()
            .any(|(machine, action)| machine.as_str() == "machine_a" && *action == "cancel")
    );
}

#[tokio::test(start_paused = true)]
async fn peer_cancels_overlap_instead_of_serializing() {
    let runtime = ConcurrentEnsureRuntime::new(ConcurrentEnsureMode::SlowCancels);
    let observer = runtime.clone();
    let task = tokio::spawn(async move {
        let jobs = vec![
            coordinator_job("machine_a", "run_a"),
            coordinator_job("machine_b", "run_b"),
            coordinator_job("machine_c", "run_c"),
        ];
        run_image_ensure_wave_with_runtime(runtime, &jobs).await
    });
    tokio::task::yield_now().await;
    assert_eq!(
        observer
            .maximum_cancels
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both unfinished peers enter Cancel concurrently"
    );
    tokio::time::advance(std::time::Duration::from_secs(29)).await;
    assert!(!task.is_finished());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let (index, _) = task.await.expect("task joins").expect_err("status fails");
    assert_eq!(index, 0);
}
