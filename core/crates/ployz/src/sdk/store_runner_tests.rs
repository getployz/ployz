//! Rung 1: cancellation polls the real Store and waits for owned SDK child outcomes.

use super::*;
use ployz_store::testing::backend::PinAs;
use ployz_store::{Actor, DeploymentQuery};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

fn run() -> (Run, ployz_store::Actor) {
    admitted(None)
}

/// A claimed Deploy of an uploaded `app` Service.
fn uploaded() -> (Run, ployz_store::Actor) {
    admitted(Some(ployz_store::UploadedSource {
        digest: UploadDigest::parse("d".repeat(64)).unwrap(),
        base: None,
        uploader: None,
    }))
}

fn admitted(upload: Option<ployz_store::UploadedSource>) -> (Run, ployz_store::Actor) {
    use ployz_store::SealingKey;
    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    admitted_in(store, upload)
}

fn admitted_in(
    store: Arc<ConfigStore>,
    upload: Option<ployz_store::UploadedSource>,
) -> (Run, ployz_store::Actor) {
    use ployz_store::{
        Actor, Admit, CreateProject, CreateService, Deploy, EnvironmentRef, OrganizationId,
        ProjectId, ProjectName, ServiceLineageId,
    };
    let actor = Actor::system(OrganizationId::parse("test").unwrap());
    store
        .write(
            &actor,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("test").unwrap(),
                default_environment: environment(),
                region: None,
            },
        )
        .unwrap();
    if upload.is_some() {
        store
            .write(
                &actor,
                &CreateService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse("app").unwrap(),
                    image: None,
                    template: None,
                },
            )
            .unwrap();
    }
    let deployment = DeploymentId::parse("00000000-0000-4000-8000-000000000003").unwrap();
    store
        .write_trusted(
            &actor,
            &Admit::Deploy(Deploy {
                id: deployment.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    let runner = RunnerId::parse("owner").unwrap();
    store.claim(&deployment, &runner).unwrap();
    (
        Run {
            store,
            deployment,
            runner,
            over: Latch::new(),
            steps: std::sync::Mutex::default(),
            pending_live_at: std::sync::Mutex::default(),
            lease: RunLease::default(),
            reader: None,
        },
        actor,
    )
}

/// Another runner claims a new Deploy in `run`'s environment, which marks `run`'s
/// Deployment Unknown.
fn take_over(run: &Run, actor: &ployz_store::Actor) {
    use ployz_store::{Admit, Deploy, EnvironmentRef};
    let deployment = DeploymentId::parse("00000000-0000-4000-8000-000000000004").unwrap();
    run.store
        .write_trusted(
            actor,
            &Admit::Deploy(Deploy {
                id: deployment.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    run.store
        .claim(&deployment, &RunnerId::parse("other").unwrap())
        .unwrap();
}

fn cancel(run: &Run, actor: &ployz_store::Actor) {
    run.store
        .write(
            actor,
            &ployz_store::Cancel {
                deployment: run.deployment.clone(),
            },
        )
        .unwrap();
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Cleanup that is still pending when cancellation lands: another task wakes it only after the cancel.
fn after_cancel(token: &CancellationToken) -> tokio::sync::oneshot::Receiver<()> {
    let (done, cleanup) = tokio::sync::oneshot::channel();
    let token = token.clone();
    tokio::spawn(async move {
        token.cancelled().await;
        tokio::task::yield_now().await;
        let _ = done.send(());
    });
    cleanup
}

/// A runner plans with the runs its claim reports ended, so its Deploy removes the
/// Pending copies they left.
#[tokio::test]
async fn a_runner_plans_with_the_runs_its_claim_reports_ended() {
    let (superseded, actor) = run();
    take_over(&superseded, &actor);
    let run = Run {
        store: superseded.store.clone(),
        deployment: DeploymentId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
        runner: RunnerId::parse("other").unwrap(),
        over: Latch::new(),
        steps: std::sync::Mutex::default(),
        pending_live_at: std::sync::Mutex::default(),
        lease: RunLease::default(),
        reader: None,
    };

    let claimed = run.claim(Operator::Cloud).await.unwrap();

    assert_eq!(
        claimed.intent.options.ended_runs,
        BTreeSet::from([
            ployz_core::DeploymentLogId::parse(superseded.deployment.as_str()).unwrap()
        ])
    );
}

#[tokio::test]
async fn a_refusal_that_fails_the_same_way_again_is_recorded_as_one() {
    use crate::deploy::{DeployError, PlanError};
    let (run, actor) = run();
    let refusal = DeployError::Plan(PlanError::DependencyCycle {
        service: "app".into(),
    });
    run.unprepared(&BTreeSet::new(), &[], Unprepared::from(refusal))
        .await
        .unwrap();
    let view = run
        .store
        .read(
            &actor,
            &ployz_store::DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert!(
        matches!(
            &view.deployment.outcome,
            Some(ployz_store::Outcome::NotExecuted {
                same_again: true,
                reason,
                ..
            }) if reason == "dependency cycle at service 'app'"
        ),
        "{:?}",
        view.deployment.outcome
    );
}

#[tokio::test]
async fn cancellation_drops_pending_connection_setup() {
    let (run, actor) = run();
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(Arc::clone(&dropped));
    let (started, observed) = tokio::sync::oneshot::channel();
    let work = async move {
        let _guard = guard;
        started.send(()).unwrap();
        std::future::pending::<Result<(), RpcError>>().await
    };
    let cancel = async {
        observed.await.unwrap();
        cancel(&run, &actor);
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(run.connecting(work), cancel)
    })
    .await
    .unwrap();
    assert!(result.unwrap().is_none());
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn renewing_aborts_but_keeps_owned_work_driven_until_cleanup_finishes() {
    let (run, actor) = run();
    cancel(&run, &actor);
    let token = CancellationToken::new();
    let cleaned = Arc::new(AtomicBool::new(false));
    let cleanup = after_cancel(&token);
    let work = async {
        cleanup.await.unwrap();
        cleaned.store(true, Ordering::SeqCst);
        "actual outcome"
    };
    let result = run.renewing(work, || token.cancel()).await;
    assert_eq!(result, "actual outcome");
    assert!(cleaned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn renewing_aborts_once_the_store_hands_the_environment_to_another_runner() {
    let (run, actor) = run();
    take_over(&run, &actor);
    assert_eq!(
        run.record(RunEvidence::Alive).await.unwrap().status,
        DeploymentStatus::Unknown
    );
    let token = CancellationToken::new();
    let work = async {
        token.cancelled().await;
        "aborted"
    };
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run.renewing(work, || token.cancel()),
    )
    .await
    .expect("a lost runner stops at its next poll");
    assert_eq!(result, "aborted");
}

/// `forget_cluster` (ployz-store teardown.rs) cancels every Deployment in flight without
/// waiting on its runner; every later read of it answers "This Deployment already ended".
fn forget_cluster(run: &Run) {
    run.store
        .system(
            &ployz_store::OrganizationId::parse("test").unwrap(),
            &ployz_store::SystemEvent::ClusterForgotten,
            &ployz_store::Trusted::default(),
        )
        .unwrap();
}

/// Whether `connecting` lets go of a dial that never ends within a few of its polls.
async fn connecting_lets_go(run: &Run) -> bool {
    let dial = std::future::pending::<Result<(), RpcError>>();
    tokio::time::timeout(Duration::from_secs(5), run.connecting(dial))
        .await
        .is_ok()
}

#[tokio::test]
async fn renewing_aborts_once_the_store_ended_the_run() {
    let (run, _) = run();
    forget_cluster(&run);
    let token = CancellationToken::new();
    let work = async {
        token.cancelled().await;
        "aborted"
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run.renewing(work, || token.cancel()),
    )
    .await
    .expect("renewing keeps the work going for a Deployment the store ended");
    assert_eq!(result, "aborted");
}

#[tokio::test]
async fn connecting_stops_once_the_store_hands_the_environment_to_another_runner() {
    let (run, actor) = run();
    take_over(&run, &actor);
    assert!(
        connecting_lets_go(&run).await,
        "connecting dials on after the store said the run is Unknown"
    );
}

#[tokio::test]
async fn connecting_stops_once_the_store_ended_the_run() {
    let (run, _) = run();
    forget_cluster(&run);
    assert!(
        connecting_lets_go(&run).await,
        "connecting dials on for a Deployment the store ended"
    );
}

/// The store never hands a Deployment it set Unknown back, so a read that fails after
/// one said Unknown still leaves the run lost.
#[tokio::test]
async fn a_run_stays_lost_when_a_later_read_fails() {
    let (run, actor) = run();
    take_over(&run, &actor);
    let token = CancellationToken::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        run.renewing(token.cancelled(), || token.cancel()),
    )
    .await
    .expect("renewing aborts once it reads the takeover");
    // A Store that never held the Deployment: every later read fails.
    let run = Run {
        store: Arc::new(
            ConfigStore::open(
                "sqlite::memory:",
                ployz_store::SealingKey::new(b"test").unwrap(),
            )
            .unwrap(),
        ),
        ..run
    };
    let read = run.record(RunEvidence::Alive).await.unwrap_err();
    assert_eq!(read.code, RpcErrorCode::NotFound);
    assert_eq!(
        run.standing().await,
        Standing::Over(Over::Lost),
        "a read that fails after the takeover lets the run go on"
    );
}

/// The Deployment `run` claimed, with an empty Deploy Preview recorded.
async fn prepared(run: &Run) {
    let claimed = run.claim(Operator::Cloud).await.unwrap();
    let preview = serde_json::from_value(serde_json::json!({
        "namespace": claimed.intent.namespace,
        "operations": [], "warnings": [], "would_remove": [], "preserved_volumes": [],
    }))
    .unwrap();
    run.record(RunEvidence::Prepared(preview)).await.unwrap();
}

/// What the store kept as the outcome of `run`'s execution.
fn kept_summary(run: &Run, actor: &ployz_store::Actor) -> Option<Value> {
    let view = run
        .store
        .read(
            actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    match view.deployment.outcome {
        Some(ployz_store::Outcome::Executed { summary, .. }) => Some(summary),
        _ => None,
    }
}

fn interrupted_summary() -> Value {
    serde_json::to_value(ployz_core::config::RuntimeOutcomeSummary::Failed {
        completed: 0,
        unexecuted: 0,
        reason: ployz_core::config::RuntimeFailureKind::Interrupted,
    })
    .unwrap()
}

/// A takeover that lands after the run's last read: the run sends what executing did,
/// and the store keeps it as Interrupted, the one outcome it takes from a lost run.
#[tokio::test]
async fn a_run_taken_over_after_its_last_read_keeps_its_outcome_as_interrupted() {
    let (run, actor) = run();
    prepared(&run).await;
    take_over(&run, &actor);
    let outcome = DeployOutcome::Success {
        completed: Vec::new(),
        services: Vec::new(),
    };
    let recorded = run.executed(outcome, Vec::new(), Vec::new()).await;
    let summary = recorded.expect("the store refuses the run's outcome, and nothing keeps it");
    assert_eq!(summary.status, DeploymentStatus::Unknown);
    assert_eq!(kept_summary(&run, &actor), Some(interrupted_summary()));
}

/// A lost run whose outcome the store refused sends it again as Interrupted only when it
/// tried to delete no Volume, since the store reads an Interrupted outcome's removed
/// Volumes as never attempted.
#[tokio::test]
async fn a_lost_run_that_tried_to_delete_a_volume_keeps_the_refusal() {
    let volume = ployz_core::DockerVolumeId {
        machine_id: MachineId::parse("a".repeat(32)).unwrap(),
        name: ployz_core::DockerVolumeName::parse("app_data").unwrap(),
    };
    let removal = |outcome| ployz_core::VolumeRemoval {
        id: volume.clone(),
        outcome,
    };
    let cases = [
        (ployz_core::VolumeRemovalOutcome::Omitted, true),
        (ployz_core::VolumeRemovalOutcome::Removed, false),
        (
            ployz_core::VolumeRemovalOutcome::Failed {
                error: RpcError::unavailable("no reply"),
            },
            false,
        ),
    ];
    for (outcome, resent) in cases {
        let (run, actor) = run();
        prepared(&run).await;
        take_over(&run, &actor);
        let success = DeployOutcome::Success {
            completed: Vec::new(),
            services: Vec::new(),
        };
        let recorded = run
            .executed(success, Vec::new(), vec![removal(outcome.clone())])
            .await;
        if resent {
            assert_eq!(recorded.unwrap().status, DeploymentStatus::Unknown);
            assert_eq!(kept_summary(&run, &actor), Some(interrupted_summary()));
        } else {
            assert_eq!(
                recorded.unwrap_err().code,
                RpcErrorCode::Conflict,
                "{outcome:?}"
            );
            assert_eq!(kept_summary(&run, &actor), None, "{outcome:?}");
        }
    }
}

/// Each answer to a record, as the standing the run acts on.
#[tokio::test]
async fn a_read_is_the_standing_its_status_or_error_code_names() {
    let (run, _) = run();
    let summary = run.record(RunEvidence::Alive).await.unwrap();
    let read = |status| {
        let mut summary = summary.clone();
        summary.status = status;
        standing(read_of(&Ok(summary)))
    };
    let failed = |code| standing(read_of(&Err(RpcError::new(code, String::new()))));
    assert_eq!(read(DeploymentStatus::Running), Standing::Keep);
    assert_eq!(read(DeploymentStatus::Cancelling), Standing::Cancel);
    assert_eq!(read(DeploymentStatus::Unknown), Standing::Over(Over::Lost));
    for ended in [
        DeploymentStatus::Applied,
        DeploymentStatus::Failed,
        DeploymentStatus::Cancelled,
        DeploymentStatus::WaitingForCi,
        DeploymentStatus::Queued,
        DeploymentStatus::Superseded,
    ] {
        assert_eq!(read(ended), Standing::Over(Over::Ended), "{ended:?}");
    }
    assert_eq!(failed(RpcErrorCode::Conflict), Standing::Over(Over::Ended));
    for unread in [
        RpcErrorCode::Unavailable,
        RpcErrorCode::NotFound,
        RpcErrorCode::Internal,
    ] {
        assert_eq!(failed(unread.clone()), Standing::Unread, "{unread:?}");
    }
}

/// The first read that says the store no longer holds the run is the one every later
/// read says: an ended Deployment's refusal cannot turn a lost run into an ended one.
#[tokio::test]
async fn the_first_read_that_says_the_run_is_over_is_the_one_it_keeps() {
    let (run, actor) = run();
    take_over(&run, &actor);
    assert_eq!(run.standing().await, Standing::Over(Over::Lost));
    let run = Run {
        store: run.store.clone(),
        runner: RunnerId::parse("other").unwrap(),
        ..run
    };
    assert_eq!(
        run.record(RunEvidence::Alive).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    assert_eq!(run.standing().await, Standing::Over(Over::Lost));
}

#[test]
fn a_taken_over_run_reports_what_it_completed_and_leaves_the_stopped_operation_unexecuted() {
    let operation = |id: char| DeployOperation::RemoveContainer {
        machine_id: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
        anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
    };
    let outcome = DeployOutcome::Failed {
        completed: vec![operation('a')],
        failed: FailedOperation::Operation {
            operation: operation('b'),
            error: ExecutionError::Cancelled,
        },
        unexecuted: vec![operation('c')],
        services: Vec::new(),
    };
    assert_eq!(
        interrupted(outcome),
        DeployOutcome::Interrupted {
            completed: vec![operation('a')],
            unexecuted: vec![operation('b'), operation('c')],
            services: Vec::new(),
        }
    );
}

#[tokio::test]
async fn a_launch_or_initial_report_failure_settles_all_started_builds_and_keeps_unknown() {
    let targets: Vec<_> = ["web", "worker"]
        .into_iter()
        .map(|name| Target {
            service: ServiceName::parse(name).unwrap(),
            build: Build::Git {
                commit: CommitSha::parse("a".repeat(40)).unwrap(),
                checkout: PathBuf::new(),
                preferred_machine: None,
            },
        })
        .collect();
    let stopped = Arc::new(AtomicUsize::new(0));
    let builds: Vec<_> = targets
        .iter()
        .map(|target| {
            let token = CancellationToken::new();
            let stopped = Arc::clone(&stopped);
            let cleanup = after_cancel(&token);
            let running =
                super::super::running::Running::spawn(token, move |_reporter| async move {
                    cleanup.await.unwrap();
                    stopped.fetch_add(1, Ordering::SeqCst);
                    Err(RpcError::unavailable("Remote termination is unknown")
                        .with_details(serde_json::json!({"preparation":{"kind":"unknown"}})))
                });
            (target, running)
        })
        .collect();
    let error = stop_builds(
        &builds,
        RpcError::conflict("The initial build report was refused")
            .with_details(serde_json::json!({"revision":7}))
            .with_causes(vec!["runner lost ownership".into()]),
    )
    .await;
    assert_eq!(stopped.load(Ordering::SeqCst), 2);
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.message, "The initial build report was refused");
    assert_eq!(error.cause, ["runner lost ownership"]);
    assert_eq!(error.details.get("revision"), Some(&serde_json::json!(7)));
    let cleanup = error
        .details
        .get("build_cleanup")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(cleanup.len(), 2);
    assert!(
        cleanup
            .iter()
            .all(|entry| entry.pointer("/error/details/preparation/kind")
                == Some(&serde_json::json!("unknown")))
    );
    assert_eq!(
        cleanup
            .iter()
            .map(|entry| entry.get("service").unwrap().as_str().unwrap())
            .collect::<Vec<_>>(),
        ["web", "worker"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_queued_build_says_so_in_its_log_while_it_waits() {
    let (run, actor) = uploaded();
    let target = Target {
        service: ServiceName::parse("app").unwrap(),
        build: Build::Upload {
            digest: UploadDigest::parse("d".repeat(64)).unwrap(),
            source: None,
            earlier: false,
            github_skipped: false,
        },
    };
    run.report(
        &target.service,
        BuildStatus::Building,
        None,
        String::new(),
        None,
    )
    .await
    .unwrap();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let running =
        super::super::running::Running::spawn(CancellationToken::new(), |reporter| async move {
            reporter.report(super::super::prepare::Progress::Build(
                ployz_build::Progress::Stage(ployz_build::Stage::Queued),
            ));
            let _ = released.await;
            Err(RpcError::internal("released"))
        });
    let log = || {
        run.store
            .read(
                &actor,
                &ployz_store::BuildLogQuery {
                    deployment: run.deployment.clone(),
                    service: target.service.clone(),
                },
            )
            .unwrap()
            .log
    };
    let watch = async {
        let shown = tokio::time::timeout(Duration::from_secs(30), async {
            while !log().contains("Waiting for a build slot") {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        release.send(()).unwrap();
        shown
    };
    let (followed, shown) = tokio::join!(run.follow(&target, &running, 0), watch);
    assert!(followed.unwrap().is_err());
    assert!(
        shown.is_ok(),
        "the log said nothing while queued: {:?}",
        log()
    );
}

#[tokio::test]
async fn a_failed_build_leaves_the_deployment_in_the_step_it_failed_in() {
    let (run, actor) = uploaded();
    let target = Target {
        service: ServiceName::parse("app").unwrap(),
        build: Build::Upload {
            digest: UploadDigest::parse("d".repeat(64)).unwrap(),
            source: None,
            earlier: false,
            github_skipped: false,
        },
    };
    let running =
        super::super::running::Running::spawn(CancellationToken::new(), |reporter| async move {
            for stage in [
                ployz_build::Stage::Upload,
                ployz_build::Stage::Preparation,
                ployz_build::Stage::Building,
            ] {
                reporter.report(super::super::prepare::Progress::Build(
                    ployz_build::Progress::Stage(stage),
                ));
            }
            Err(RpcError::internal("the build failed"))
        });
    assert!(run.follow(&target, &running, 1).await.unwrap().is_err());
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    let steps: Vec<_> = view
        .steps
        .iter()
        .map(|step| (step.name, step.service.clone(), step.ended_at.is_some()))
        .collect();
    let app = Some(target.service.clone());
    assert_eq!(
        steps,
        [
            (StepName::Uploading, app.clone(), true),
            (StepName::StartingBuilder, app.clone(), true),
            (StepName::Building, app, false),
        ]
    );
    assert_eq!(view.steps.first().map(|step| step.started_at), Some(1));
}

#[tokio::test]
async fn a_build_reports_the_server_it_runs_on_by_name() {
    let (run, actor) = uploaded();
    let target = Target {
        service: ServiceName::parse("app").unwrap(),
        build: Build::Upload {
            digest: UploadDigest::parse("d".repeat(64)).unwrap(),
            source: None,
            earlier: false,
            github_skipped: false,
        },
    };
    run.report(
        &target.service,
        BuildStatus::Building,
        None,
        String::new(),
        None,
    )
    .await
    .unwrap();
    let running =
        super::super::running::Running::spawn(CancellationToken::new(), |reporter| async move {
            reporter.report(super::super::prepare::Progress::Selected(Box::new(
                super::super::prepare::SelectedBuilder {
                    machine: ployz_core::Machine {
                        labels: Default::default(),
                        accepts_builds: true,
                        accepts_services: true,
                        accepts_ingress: true,
                        id: MachineId::parse("1".repeat(32)).unwrap(),
                        name: ployz_core::MachineName::parse("machine-1").unwrap(),
                        subnet: "10.210.1.0/24".parse().unwrap(),
                        public_key: ployz_core::WireGuardPublicKey([1; 32]),
                        public_ip: None,
                        advertised_endpoints: Vec::new(),
                        runtime: Default::default(),
                        join_approval: None,
                        build_concurrency: None,
                    },
                    reason: super::super::prepare::BuilderReason::Spread,
                    rejections: Vec::new(),
                },
            )));
            Err(RpcError::internal("the build failed"))
        });
    assert!(run.follow(&target, &running, 0).await.unwrap().is_err());
    let build = run
        .store
        .read(
            &actor,
            &ployz_store::BuildLogQuery {
                deployment: run.deployment.clone(),
                service: target.service.clone(),
            },
        )
        .unwrap()
        .build;
    assert_eq!(build.server.as_deref(), Some("machine-1"));
}

fn scrub_target() -> Scrub {
    let spec = |name: &str, env: serde_json::Value| -> ployz_core::RequestedServiceSpec {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing", "environment": env },
        }))
        .unwrap()
    };
    let target = [
        spec(
            "web",
            serde_json::json!({ "DB_PASSWORD": "hunter2-long", "LOG_LEVEL": "debug-verbose" }),
        ),
        spec(
            "worker",
            serde_json::json!({ "API_TOKEN": "tok-worker-9", "MIRROR": "hunter2-long" }),
        ),
    ];
    Scrub {
        environment: environment(),
        sealed_at_claim: sealed(["DB_PASSWORD", "API_TOKEN"]),
        claim: target
            .iter()
            .flat_map(|spec| spec.container.environment.iter())
            .filter_map(|(key, value)| Some((key.clone(), value.text()?.to_owned())))
            .collect(),
    }
}

fn environment() -> ployz_store::EnvironmentId {
    ployz_store::EnvironmentId::parse("00000000-0000-4000-8000-000000000002").unwrap()
}

fn sealed<const N: usize>(names: [&str; N]) -> BTreeSet<String> {
    names.map(str::to_owned).into_iter().collect()
}

fn seal(run: &Run, actor: &ployz_store::Actor, name: &str) {
    use base64::Engine as _;
    use ployz_store::{
        Change, CreateService, Edit, EnvironmentRef, ServiceLineageId, ServiceQuery, SettingPath,
    };
    run.store
        .write(
            actor,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx:1".into()),
                template: None,
            },
        )
        .unwrap();
    let web = run
        .store
        .read(
            actor,
            &ServiceQuery {
                environment: EnvironmentRef::default(),
                service: ServiceName::parse("web").unwrap(),
            },
        )
        .unwrap();
    let sealed = base64::engine::general_purpose::STANDARD.encode([7; 48]);
    run.store
        .write(
            actor,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(&format!("web.env.{name}")).unwrap(),
                    value: serde_json::json!({ "secret": {
                        "sealedFor": environment().as_str(),
                        "value": {
                            "sealed": sealed,
                            "scope": { "service": web.service.service.id },
                            "key": name,
                        },
                    } }),
                }],
            },
        )
        .unwrap();
}

// Catches a failed log that shows a Sealed value, from its own Service or another the
// claim lowered, even where a plain variable holds the same value, or that scrubs a plain
// or image value such as NODE_ENV=production.
#[test]
fn a_failed_log_is_scrubbed_of_sealed_values_only() {
    use ployz_agent_tools::{LineCap, LogExcerpt};
    let scrub = scrub_target();
    let sealed = scrub.values(&scrub.sealed_at_claim, BTreeMap::new());
    let line = "npm warn config production; login hunter2-long at debug-verbose with tok-worker-9 password=open-sesame\n";
    let excerpt = LogExcerpt::scrub([line.to_owned()], &sealed, LineCap::default());
    assert_eq!(
        excerpt.lines(),
        [
            "npm warn config production; login [secret DB_PASSWORD] at debug-verbose with [secret API_TOKEN] password=[redacted]"
        ]
    );
}

#[test]
fn an_older_containers_log_is_scrubbed_of_its_own_sealed_values_too() {
    use ployz_agent_tools::{LineCap, LogExcerpt};
    let env = BTreeMap::from([
        ("DB_PASSWORD".to_owned(), "old-rotated-out".to_owned()),
        ("LOG_LEVEL".to_owned(), "debug-verbose".to_owned()),
    ]);
    let scrub = scrub_target();
    let sealed = scrub.values(&scrub.sealed_at_claim, env);
    let line = "was old-rotated-out now hunter2-long at debug-verbose\n";
    let excerpt = LogExcerpt::scrub([line.to_owned()], &sealed, LineCap::default());
    assert_eq!(
        excerpt.lines(),
        ["was [secret DB_PASSWORD] now [secret DB_PASSWORD] at debug-verbose"]
    );
}

async fn mock_machine() -> (
    Session,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    MachineId,
) {
    use crate::global_fixtures::container_id;
    use crate::sdk::test_support as support;
    let one = support::machine('a', "one");
    let mut service = support::DeployService::new(one.clone());
    service.uninspectable.insert(container_id('e'));
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    (session, server, one.machine.id)
}

#[tokio::test]
async fn a_runner_names_its_entry_after_a_plain_transport_failure() {
    let (session, server, machine_id) = mock_machine().await;
    let client = session.client().unwrap();
    assert_eq!(client.entry_machine(), Some(machine_id));
    assert_eq!(client.server().as_deref(), Some(machine_id.as_str()));
    let names = BTreeMap::from([(machine_id, MachineName::parse("machine-1").unwrap())]);
    let failure = crate::failure::Failure::from(crate::connect::ConnectError::Rpc(
        crate::connect::TransportError::budget_expired("no reply"),
    ));

    assert_eq!(
        ingress_failure_message(&client, &names)(failure),
        "Server machine-1 isn't answering (no reply)"
    );

    session.close().await;
    server.abort();
}

/// A creation of `kind` for the Service `spec` names, in slot `key`.
fn creation(
    spec: &ployz_core::ResolvedServiceSpec,
    kind: ployz_core::ContainerKind,
    key: &str,
) -> ployz_core::ContainerWant {
    ployz_core::ContainerWant::Running {
        container: ployz_core::RunTarget::New(ployz_core::NewContainer {
            r#gen: ployz_core::Gen::FIRST,
            slot: ployz_core::SlotKey {
                namespace: ployz_core::Namespace::parse("app").unwrap(),
                service: spec.name.clone(),
                kind,
                creation_key: key.to_owned().try_into().unwrap(),
            },
            resolved_spec: spec.clone(),
            deployment_id: None,
            registry_auth: None,
            registry_login: None,
            image_prepared: false,
            image_id: None,
            answer: ployz_core::AnswerWhen::Started,
            replaces: None,
            free_receipt: None,
        }),
        rotation: ployz_core::RunRotation::Pending,
    }
}

/// The proof a rule's build carries before any Server checked it: the image token.
fn online_proof(service: &ServiceName) -> ployz_core::authority::RuleProof {
    use ployz_core::authority::rule::{Builder, RuleEvent, Runner};
    use ployz_core::authority::{
        AttestKey, AutoDeployRule, BuildClaim, BuildStage, BuiltImageDigest, CommitAuthority,
        CommitSha, GithubSource, PolicyDigest, RuleEvidence, RuleId, RuleProof, RuleSource, Signed,
        Signer, SignerKey, Timestamp,
    };
    let cluster = ployz_core::SealingPublicKey::from_bytes([3; 32]);
    let signer = Signer::from_seed(&[1; 32]);
    let rule = AutoDeployRule {
        proposal: None,
        preview: None,
        cluster,
        rule: RuleId::from_bytes([9; 16]),
        generation: 1,
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        machines: Vec::new(),
        source: RuleSource::Github(GithubSource {
            repository_id: 11,
            owner_id: 1,
            repository: "acme/web".into(),
            branch: "main".into(),
            workflow: ".github/workflows/ployz-build.yml".into(),
            events: vec![RuleEvent::Push],
            builder: Builder::pinned(),
            runner: Runner::GithubHosted,
        }),
        services: Vec::new(),
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        ended: false,
        signer: SignerKey::Device(signer.public().into()),
    };
    RuleProof {
        rule: Signed::sign(&rule, &signer).unwrap(),
        evidence: RuleEvidence::Online(ployz_core::GithubToken::new("image-token".to_owned())),
        claim: Some(BuildClaim {
            cluster,
            rule: rule.rule,
            generation: 1,
            service: service.clone(),
            policy: PolicyDigest::from_bytes([0; 32]),
            commit: CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
            authority: CommitAuthority::Head,
            stage: BuildStage::Built {
                image: BuiltImageDigest::parse(&format!("sha256:{}", "d".repeat(64))).unwrap(),
                attest: AttestKey::from_bytes([0x55; 32]),
            },
        }),
        reader: None,
    }
}

// A pre-deploy hook can outlast the image token. The Server that admits the hook answers
// a verified build, and the Service the same Deploy creates next must carry it, since the
// token has expired by then. The run keeps the same verified build for later Deployments.
#[tokio::test]
async fn a_service_created_after_its_hook_carries_the_verified_build_the_hook_got() {
    use crate::sdk::test_support as support;
    use ployz_core::ContainerKind;
    let one = support::machine('a', "one");
    let issued = ployz_core::authority::VerifiedBuild::from_bytes([0x5a; 32]);
    let service = support::DeployService::new(one.clone()).checking_rules(issued);
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let spec = crate::global_fixtures::requested(ployz_core::ServiceMode::Global)
        .to_resolved(
            crate::global_fixtures::service_id('a'),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    let mut proofs = crate::deploy::RuleProofs::default();
    proofs.insert(
        spec.name.clone(),
        ployz_core::authority::KeptRule::built(online_proof(&spec.name)),
    );
    let client = crate::deploy::Approved::new(
        session.client().unwrap(),
        crate::deploy::Approvals::signed(Vec::new(), proofs.clone()),
    );

    for (kind, key) in [
        (ContainerKind::PreDeployHook, "hook:test"),
        (ContainerKind::ServiceContainer, "global:test"),
    ] {
        let created = crate::deploy::CopyLifecycle::ensure_unsettled(
            &client,
            &one.machine.id,
            creation(&spec, kind, key),
            None,
        )
        .await;
        assert!(
            matches!(created, Ok(ployz_core::EnsuredContainer::Ensured { .. })),
            "the {kind} is created: {created:?}"
        );
    }
    assert_eq!(
        proofs.issued().get(&spec.name).map(|kept| kept.verified),
        Some(issued)
    );

    session.close().await;
    server.abort();
}

// A registry rule's Server pulls the image itself when it creates the Container, and the
// registry refuses a pull without a login. The Deploy pulls ahead with the Service's
// login and then creates with none, so each creation under the rule, hook and Service
// alike, must carry that login again.
#[tokio::test]
async fn each_creation_under_a_registry_rule_carries_its_services_login() {
    use crate::sdk::test_support as support;
    use ployz_core::ContainerKind;
    use ployz_core::authority::{RuleEvidence, RuleProof};
    let one = support::machine('a', "one");
    let unused = ployz_core::authority::VerifiedBuild::from_bytes([0x5a; 32]);
    let service = support::DeployService::new(one.clone()).checking_rules(unused);
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let spec = crate::global_fixtures::requested(ployz_core::ServiceMode::Global)
        .to_resolved(
            crate::global_fixtures::service_id('a'),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    let mut proofs = crate::deploy::RuleProofs::default();
    proofs.insert(
        spec.name.clone(),
        ployz_core::authority::KeptRule {
            proof: Box::new(RuleProof {
                evidence: RuleEvidence::Registry,
                claim: None,
                ..online_proof(&spec.name)
            }),
            login: Some(ployz_core::SealedLogin {
                host: "registry.example".into(),
                sealed: ployz_core::Ciphertext::from_bytes(vec![7; 48]),
            }),
        },
    );
    let client = crate::deploy::Approved::new(
        session.client().unwrap(),
        crate::deploy::Approvals::signed(Vec::new(), proofs),
    );

    for (kind, key) in [
        (ContainerKind::PreDeployHook, "hook:test"),
        (ContainerKind::ServiceContainer, "global:test"),
    ] {
        let created = crate::deploy::CopyLifecycle::ensure_unsettled(
            &client,
            &one.machine.id,
            creation(&spec, kind, key),
            None,
        )
        .await;
        assert!(
            matches!(created, Ok(ployz_core::EnsuredContainer::Ensured { .. })),
            "the {kind} is created: {created:?}"
        );
    }

    session.close().await;
    server.abort();
}

// A Server drain recreates a registry rule's copy from what the Store keeps for it.
// The destination pulls the private image itself, so the kept rule must bring the
// login of what's applied with it.
#[tokio::test]
async fn a_drained_copy_under_a_registry_rule_pulls_with_the_applied_login() {
    use crate::sdk::test_support as support;
    use ployz_core::ContainerKind;
    use ployz_store::testing::shop;
    let one = support::machine('a', "one");
    let unused = ployz_core::authority::VerifiedBuild::from_bytes([0x5a; 32]);
    let service = support::DeployService::new(one.clone()).checking_rules(unused);
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let (store, who) = shop::shop();
    shop::registry_worker(&store, &who);
    let view: ployz_store::RuleView = store
        .read(
            &who,
            &ployz_store::RuleQuery {
                environment: ployz_store::EnvironmentRef::default(),
                staged: false,
            },
        )
        .unwrap();
    let spec = view
        .target
        .iter()
        .find(|spec| spec.name.as_str() == "worker")
        .unwrap()
        .to_resolved(
            crate::global_fixtures::service_id('a'),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    let mut want = creation(&spec, ContainerKind::ServiceContainer, "global:test");
    if let ployz_core::ContainerWant::Running {
        container: ployz_core::RunTarget::New(new),
        ..
    } = &mut want
    {
        new.slot.namespace = view.namespace;
    }
    let client = crate::deploy::Approved::new(
        session.client().unwrap(),
        crate::deploy::Approvals::kept(crate::deploy::KeptVersions::new(
            Arc::new(store),
            who.organization,
        )),
    );

    let created =
        crate::deploy::CopyLifecycle::ensure_unsettled(&client, &one.machine.id, want, None).await;

    assert!(
        matches!(created, Ok(ployz_core::EnsuredContainer::Ensured { .. })),
        "{created:?}"
    );
    session.close().await;
    server.abort();
}

fn lines(log: &LogText) -> Vec<String> {
    match log {
        LogText::Shown(excerpt) => excerpt.lines().to_vec(),
        withheld => panic!("a read tail is shown: {withheld:?}"),
    }
}

#[tokio::test]
async fn an_older_containers_failed_tail_is_scrubbed_of_the_value_its_machine_reports() {
    use crate::global_fixtures::container_id;
    let (run, _) = run();
    let (session, server, machine) = mock_machine().await;
    let client = session.client().unwrap();
    let scrub = Scrub {
        environment: environment(),
        sealed_at_claim: sealed(["PLAIN"]),
        claim: BTreeSet::from([("PLAIN".to_owned(), "rotated-new-value".to_owned())]),
    };

    let older = run
        .failed_log(&client, &scrub, machine, container_id('d'))
        .await
        .0;
    let unreadable = run
        .failed_log(&client, &scrub, machine, container_id('e'))
        .await
        .0;
    session.close().await;
    server.abort();

    assert!(
        lines(&older).contains(&"plain [secret PLAIN]".to_owned()),
        "{older:?}"
    );
    assert!(
        matches!(
            unreadable,
            LogText::Withheld {
                why: Withheld::NoSecretContext
            }
        ),
        "{unreadable:?}"
    );
}

// Catches a failed tail scrubbed under the names Sealed at the claim alone: a name sealed
// after the claim and before the tail is read must be scrubbed too.
#[tokio::test]
async fn a_name_sealed_after_the_claim_is_scrubbed_from_a_failed_tail() {
    use crate::global_fixtures::container_id;
    let (run, actor) = run();
    let (session, server, machine) = mock_machine().await;
    let client = session.client().unwrap();
    let scrub = Scrub {
        environment: environment(),
        sealed_at_claim: BTreeSet::new(),
        claim: BTreeSet::from([("PLAIN".to_owned(), "from-docker".to_owned())]),
    };
    seal(&run, &actor, "PLAIN");

    let log = run
        .failed_log(&client, &scrub, machine, container_id('d'))
        .await
        .0;
    session.close().await;
    server.abort();

    let lines = lines(&log);
    assert!(
        lines.contains(&"plain [secret PLAIN]".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("from-docker")),
        "{lines:?}"
    );
}

// Catches a failed tail scrubbed of the claim's values alone: an image can supply a
// variable the claim never named, and a Machine reports it. Sealed after the claim, its
// value must be scrubbed, and withheld when the Machine cannot report it.
#[tokio::test]
async fn an_image_supplied_value_sealed_after_the_claim_is_scrubbed_from_a_failed_tail() {
    use crate::global_fixtures::container_id;
    let (run, actor) = run();
    let (session, server, machine) = mock_machine().await;
    let client = session.client().unwrap();
    let scrub = Scrub {
        environment: environment(),
        sealed_at_claim: BTreeSet::new(),
        claim: BTreeSet::new(),
    };
    seal(&run, &actor, "PLAIN");

    let log = run
        .failed_log(&client, &scrub, machine, container_id('d'))
        .await
        .0;
    let unreadable = run
        .failed_log(&client, &scrub, machine, container_id('e'))
        .await
        .0;
    session.close().await;
    server.abort();

    let lines = lines(&log);
    assert!(
        lines.contains(&"plain [secret PLAIN]".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("from-docker")),
        "{lines:?}"
    );
    assert!(
        matches!(
            unreadable,
            LogText::Withheld {
                why: Withheld::NoSecretContext
            }
        ),
        "{unreadable:?}"
    );
}

/// Two uploaded Services, `front` and `back`, each sent its own upload, then a full
/// Deploy of both: claimed.
fn separately_uploaded() -> Claimed {
    use ployz_store::{
        Actor, Admit, CreateProject, CreateService, Deploy, EnvironmentRef, OrganizationId,
        ProjectId, ProjectName, SealingKey, ServiceLineageId, UploadedSource,
    };
    let store = ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
    let actor = Actor::system(OrganizationId::parse("test").unwrap());
    store
        .write(
            &actor,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("test").unwrap(),
                default_environment: environment(),
                region: None,
            },
        )
        .unwrap();
    for (n, name) in [(5, "front"), (6, "back")] {
        store
            .write(
                &actor,
                &CreateService {
                    id: ServiceLineageId::parse(format!("00000000-0000-4000-8000-00000000000{n}"))
                        .unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ServiceName::parse(name).unwrap(),
                    image: None,
                    template: None,
                },
            )
            .unwrap();
    }
    let runner = RunnerId::parse("owner").unwrap();
    let deploy = |n: u8, services: &[&str], digest: Option<char>| {
        let deployment =
            DeploymentId::parse(format!("00000000-0000-4000-8000-00000000010{n}")).unwrap();
        store
            .write_trusted(
                &actor,
                &Admit::Deploy(Deploy {
                    id: deployment.clone(),
                    environment: EnvironmentRef::default(),
                    services: services
                        .iter()
                        .map(|name| ServiceName::parse(*name).unwrap())
                        .collect(),
                    version: None,
                    upload: digest.map(|digest| UploadedSource {
                        digest: UploadDigest::parse(digest.to_string().repeat(64)).unwrap(),
                        base: None,
                        uploader: None,
                    }),
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                    versions: Vec::new(),
                    copies: Vec::new(),
                    recreate: Vec::new(),
                    rule: None,
                }),
                &ployz_store::Trusted::default(),
            )
            .unwrap();
        store.claim(&deployment, &runner).unwrap()
    };
    let end = |claimed: &Claimed| {
        store
            .record(
                &claimed.deployment.id,
                &runner,
                ployz_store::RunEvidence::NotExecuted("stopped".into()),
            )
            .unwrap();
    };
    end(&deploy(1, &["back"], Some('b')));
    end(&deploy(2, &["front"], Some('f')));
    deploy(3, &[], None)
}

#[test]
fn a_build_with_no_server_in_its_walk_fails_with_the_reason_github_gave() {
    let mut claimed = separately_uploaded();
    claimed.sources.push(
        serde_json::from_value(serde_json::json!({
            "service": "api",
            "repository": "acme/api",
            "repository_id": 42,
            "access": { "type": "public" },
            "branch": "main",
            "root_dir": "/",
            "dockerfile_path": null,
            "commit": "a".repeat(40),
            "builders": [],
            "preferred_machine": null,
            "status": "pending",
            "message": "GitHub has no ployz-build.yml workflow",
        }))
        .unwrap(),
    );
    let Err(Unbuilt::Failed(message)) = targets(
        &claimed,
        Sources {
            checkouts: BTreeMap::new(),
            upload: Some(PathBuf::from("/extracted")),
            reader: None,
        },
    ) else {
        panic!("a build no Server takes fails");
    };
    assert_eq!(
        message,
        "Build failed. api: GitHub has no ployz-build.yml workflow. \
         No other Builder in your Build Order can take it"
    );
}

#[test]
fn a_full_deploy_builds_each_uploaded_service_from_its_own_upload() {
    let claimed = separately_uploaded();
    let extracted = PathBuf::from("/extracted");
    let (targets, _) = match targets(
        &claimed,
        Sources {
            checkouts: BTreeMap::new(),
            upload: Some(extracted.clone()),
            reader: None,
        },
    ) {
        Ok(targets) => targets,
        Err(_) => panic!("both Services build"),
    };
    let builds: Vec<_> = targets
        .iter()
        .map(|target| match &target.build {
            Build::Upload {
                digest,
                source,
                earlier,
                ..
            } => (
                target.service.as_str(),
                digest.as_str().chars().next().unwrap(),
                source.clone(),
                *earlier,
            ),
            Build::Git { .. } => panic!("no Git Service"),
        })
        .collect();
    // Cloud extracts the newer upload, front's; back builds from its own b, from its
    // image unless it must build again.
    assert_eq!(
        builds,
        [
            ("back", 'b', None, true),
            ("front", 'f', Some(extracted), false)
        ]
    );
    let mut input = PreparationInput::default();
    for target in &targets {
        target.input(&mut input);
    }
    assert_eq!(
        input
            .uploads
            .values()
            .map(|digest| digest.as_str().chars().next().unwrap())
            .collect::<Vec<_>>(),
        ['b', 'f']
    );
    // back must build again: it needs its own Deployment, not a new upload.
    let back = ServiceName::parse("back").unwrap();
    let Unbuilt::Failed(why) = upload_needed(&earlier(&targets), vec![back.clone()]) else {
        panic!("back still has its upload");
    };
    assert!(
        why.ends_with("Deploy it on its own: ployz deploy back"),
        "{why}"
    );
    // front's upload is gone: that needs a new upload, first.
    let front = ServiceName::parse("front").unwrap();
    let Unbuilt::UploadNeeded(services) =
        upload_needed(&earlier(&targets), vec![back, front.clone()])
    else {
        panic!("front needs a new upload");
    };
    assert_eq!(services, [front]);
}

#[test]
fn a_storage_refusal_says_this_deploy_s_deletions_free_space_only_after_it() {
    let machine = |c: char| ployz_core::MachineId::parse(c.to_string().repeat(32)).unwrap();
    let storage = || crate::deploy::PlanError::Storage {
        machine_id: machine('a'),
        machine: ployz_core::MachineName::parse("machine-1").unwrap(),
        source: ployz_core::StorageCapacityError::InsufficientStorage {
            required_growth_bytes: 4 << 30,
            available_bytes: 1 << 30,
            reserve_bytes: 0,
        },
    };
    let short = || crate::deploy::DeployError::Plan(storage());
    let wrapped = || {
        crate::deploy::DeployError::Plan(crate::deploy::PlanError::Service {
            service: "web".into(),
            source: Box::new(storage()),
        })
    };
    let deleting = |on: char| {
        vec![ployz_core::DockerVolumeId {
            machine_id: machine(on),
            name: "vs-production-m1".parse().unwrap(),
        }]
    };
    let note = " Volumes this deploy deletes free their space only after it succeeds, so they don't make room for its new volumes.";

    // Planned directly, and planned after a Build, which rewrites the error's details;
    // each also wrapped in the Service it was planned for.
    for refused in [
        crate::sdk::Unprepared::from(short()),
        crate::sdk::Unprepared::from(wrapped()),
        crate::sdk::unprepared(crate::sdk::prepare::PreparationError::Plan(short()), false),
        crate::sdk::unprepared(
            crate::sdk::prepare::PreparationError::Plan(wrapped()),
            false,
        ),
    ] {
        let plain = Failure::from(&refused.error).reason;
        let failure = refused_before_deletes(&refused, &deleting('a'));
        assert_eq!(failure.reason, format!("{plain}{note}"));
        for unchanged in [
            refused_before_deletes(&refused, &deleting('b')),
            refused_before_deletes(&refused, &[]),
        ] {
            assert_eq!(unchanged.reason, plain);
        }
    }
}

// Catches a runner that drops its claim's recreate list once a build sends the Deploy
// through preparation: a key rotation that carries `web` beside the Git-built `api` it
// references, whose image a receipt reuses, must still replace `web`'s unchanged Container.
#[tokio::test]
async fn a_deploy_with_a_reused_build_still_recreates_the_services_its_claim_lists() {
    use crate::sdk::test_support as support;
    use ployz_core::config::ServiceGitAccess;
    use ployz_store::{
        Admit, AuthorizedRepository, Change, CreateGitService, CreateProject, CreateService,
        Deploy, Edit, EnvironmentRef, OrganizationId, ProjectId, ProjectName, SealingKey,
        ServiceLineageId, SettingPath, Trusted,
    };
    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    let actor = Actor::system(OrganizationId::parse("test").unwrap());
    store
        .write(
            &actor,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("test").unwrap(),
                default_environment: environment(),
                region: None,
            },
        )
        .unwrap();
    let (api, web) = (
        ServiceName::parse("api").unwrap(),
        ServiceName::parse("web").unwrap(),
    );
    let repository = ployz_store::RepositoryName::parse("acme/api").unwrap();
    store
        .write_trusted(
            &actor,
            &CreateGitService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000005").unwrap(),
                environment: EnvironmentRef::default(),
                name: api.clone(),
                repository: repository.clone(),
                branch: None,
            },
            &Trusted {
                repositories: vec![AuthorizedRepository {
                    repository,
                    repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
                    access: ServiceGitAccess::Public,
                    default_branch: ployz_store::BranchName::parse("main").unwrap(),
                    branches: Vec::new(),
                }],
                ..Trusted::default()
            },
        )
        .unwrap();
    store
        .write(
            &actor,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000006").unwrap(),
                environment: EnvironmentRef::default(),
                name: web.clone(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &actor,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: [
                    ("api.buildMethod", "dockerfile"),
                    ("api.env.PORT", "80"),
                    ("web.env.API_PORT", "${{ api.PORT }}"),
                ]
                .into_iter()
                .map(|(path, value)| Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value: serde_json::json!(value),
                })
                .collect(),
            },
        )
        .unwrap();
    let deployment = DeploymentId::parse("00000000-0000-4000-8000-000000000003").unwrap();
    store
        .write_trusted(
            &actor,
            &Admit::Deploy(Deploy {
                id: deployment.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: vec![web.clone()],
                rule: None,
            }),
            &Trusted::default(),
        )
        .unwrap();
    let commit = "a".repeat(40);
    store
        .pin_as(
            &deployment,
            &[(api.clone(), CommitSha::parse(commit.as_str()).unwrap())].into(),
            &Trusted::default(),
            "owner",
        )
        .unwrap();
    let run = Run {
        store,
        deployment,
        runner: RunnerId::parse("owner").unwrap(),
        over: Latch::new(),
        steps: std::sync::Mutex::default(),
        pending_live_at: std::sync::Mutex::default(),
        lease: RunLease::default(),
        reader: None,
    };
    let claimed = run.claim(Operator::Cloud).await.unwrap();

    // A Machine that builds and one that runs `web`'s Container, unchanged since the
    // Deploy that started it.
    let root = std::env::temp_dir().join(format!("ployz-recreate-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut builder = support::machine('a', "builder");
    let mut runs = support::machine('b', "application");
    builder.machine.accepts_services = false;
    runs.machine.accepts_builds = false;
    builder.machine.runtime.architecture = "x86_64".into();
    runs.machine.runtime.architecture = "x86_64".into();
    builder.machine.runtime.capabilities = [ployz_core::BUILD_FREE_CAPABILITY.to_owned()].into();
    runs.machine.runtime.capabilities = [ployz_core::FREE_RECEIPT_CAPABILITY.to_owned()].into();
    let mut service = support::DeployService::new(builder.clone())
        .with_machines(vec![builder, runs.clone()])
        .with_exec_exit(0);
    service.builds = Some(Arc::new(support::BuildFixture::default()));
    let spec = claimed
        .intent
        .target
        .iter()
        .find(|spec| spec.name == web)
        .unwrap();
    let mut running =
        support::running_container_in(&runs, spec, claimed.intent.namespace.as_str(), 'c');
    running
        .try_update(|parts| parts.rotation = Some(ployz_core::CopyRotation::In))
        .unwrap();
    service.listed_containers().lock().unwrap().push(running);
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    // The image an earlier Deployment built for `api`, as its receipt names it. `api` reaches
    // no data, so it built free.
    assert_eq!(claimed.free, [api.clone()].into());
    let built = session
        .prepare(PreparationInput {
            deployment: claimed.input.clone(),
            sources: BTreeMap::from([(api.clone(), root.clone())]),
            source_commits: BTreeMap::from([(api.clone(), commit)]),
            free: claimed.free.clone(),
            ..PreparationInput::default()
        })
        .unwrap()
        .finished()
        .await
        .unwrap();
    let receipt = built.build_receipts().get(&api).unwrap().clone();
    assert!(receipt.free.is_some());
    built.close();

    let ran = pins(&claimed.ran);
    let prepared = run
        .prepare(
            &session,
            claimed,
            Vec::new(),
            (BTreeMap::from([(api.clone(), receipt)]), ran),
        )
        .await
        .unwrap_or_else(|unprepared| panic!("{}", unprepared.error));
    let replaced: Vec<_> = prepared
        .preview()
        .operations
        .iter()
        .filter_map(|row| {
            let DeployOperation::ReplaceContainer(replace) = &row.operation else {
                return None;
            };
            Some(replace.spec.name.clone())
        })
        .collect();
    prepared.close();
    session.close().await;
    server.abort();
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(replaced, vec![web]);
}

/// A claimed Deployment of every Service in `store`'s Environment, its Git Services
/// pinned at `commit`.
async fn redeploy(
    store: &Arc<ConfigStore>,
    actor: &Actor,
    id: u128,
    pinned: &[ServiceName],
    commit: &str,
) -> (Run, Claimed) {
    let deployment = DeploymentId::parse(uuid::Uuid::from_u128(id).to_string()).unwrap();
    store
        .write_trusted(
            actor,
            &ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: deployment.clone(),
                environment: ployz_store::EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                copies: Vec::new(),
                recreate: Vec::new(),
                rule: None,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    store
        .pin_as(
            &deployment,
            &pinned
                .iter()
                .map(|service| (service.clone(), CommitSha::parse(commit).unwrap()))
                .collect(),
            &ployz_store::Trusted::default(),
            "owner",
        )
        .unwrap();
    let run = Run {
        store: store.clone(),
        deployment,
        runner: RunnerId::parse("owner").unwrap(),
        over: Latch::new(),
        reader: None,
        steps: std::sync::Mutex::default(),
        pending_live_at: std::sync::Mutex::default(),
        lease: RunLease::default(),
    };
    let claimed = run.claim(Operator::Cloud).await.unwrap();
    (run, claimed)
}

// Catches a same-commit redeploy that builds again: five Services built from one
// repository and Dockerfile, deployed twice at one commit, build five images and then
// none, and a changed build argument or commit builds again.
#[tokio::test]
async fn a_same_commit_redeploy_of_services_sharing_a_dockerfile_builds_nothing() {
    use crate::sdk::test_support as support;
    use ployz_core::config::ServiceGitAccess;
    use ployz_store::{
        AuthorizedRepository, Change, CreateGitService, CreateProject, Edit, EnvironmentRef,
        OrganizationId, ProjectId, ProjectName, SealingKey, ServiceLineageId, SettingPath, Trusted,
    };
    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    let actor = Actor::system(OrganizationId::parse("test").unwrap());
    store
        .write(
            &actor,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("test").unwrap(),
                default_environment: environment(),
                region: None,
            },
        )
        .unwrap();
    let repository = ployz_store::RepositoryName::parse("acme/app").unwrap();
    let services: Vec<ServiceName> = ["api", "web", "worker", "cron", "jobs"]
        .into_iter()
        .map(|name| ServiceName::parse(name).unwrap())
        .collect();
    for (index, service) in (5_u128..).zip(&services) {
        store
            .write_trusted(
                &actor,
                &CreateGitService {
                    id: ServiceLineageId::parse(uuid::Uuid::from_u128(index).to_string()).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: service.clone(),
                    repository: repository.clone(),
                    branch: None,
                },
                &Trusted {
                    repositories: vec![AuthorizedRepository {
                        repository: repository.clone(),
                        repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
                        access: ServiceGitAccess::Public,
                        default_branch: ployz_store::BranchName::parse("main").unwrap(),
                        branches: Vec::new(),
                    }],
                    ..Trusted::default()
                },
            )
            .unwrap();
    }
    let edit = |changes: Vec<(String, &str)>| {
        store
            .write(
                &actor,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: changes
                        .into_iter()
                        .map(|(path, value)| Change::Set {
                            path: SettingPath::parse(&path).unwrap(),
                            value: serde_json::json!(value),
                        })
                        .collect(),
                },
            )
            .unwrap();
    };
    edit(
        services
            .iter()
            .flat_map(|service| {
                [
                    (format!("{service}.buildMethod"), "dockerfile"),
                    (format!("{service}.env.ROLE"), service.as_str()),
                ]
            })
            .collect(),
    );

    let root = std::env::temp_dir().join(format!("ployz-redeploy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    let mut builder = support::machine('a', "builder");
    let mut runs = support::machine('b', "application");
    builder.machine.accepts_services = false;
    runs.machine.accepts_builds = false;
    builder.machine.runtime.architecture = "x86_64".into();
    runs.machine.runtime.architecture = "x86_64".into();
    builder.machine.runtime.capabilities = [ployz_core::BUILD_FREE_CAPABILITY.to_owned()].into();
    runs.machine.runtime.capabilities = [ployz_core::FREE_RECEIPT_CAPABILITY.to_owned()].into();
    let mut service = support::DeployService::new(builder.clone())
        .with_machines(vec![builder, runs])
        .with_exec_exit(0);
    let builds = Arc::new(support::BuildFixture::default());
    service.builds = Some(builds.clone());
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![Connection::tcp(address)],
        Arc::new(SystemConnector::default()),
    )
    .await
    .unwrap();
    let checkouts = || Sources {
        checkouts: services
            .iter()
            .map(|service| (service.clone(), root.clone()))
            .collect(),
        ..Sources::default()
    };
    // Every Build and BuildFree call the Server gets while the Deployment builds and then
    // prepares: a rebuild that lands on the same digest still counts.
    let sent = |commit: String, id: u128| {
        let (store, actor, services, session, checkouts, builds) =
            (&store, &actor, &services, &session, &checkouts, &builds);
        async move {
            let before = builds.calls.load(Ordering::SeqCst);
            let (run, claimed) = redeploy(store, actor, id, services, &commit).await;
            let Ok((targets, _)) = targets(&claimed, checkouts()) else {
                panic!("every Service has its checkout at the pinned commit");
            };
            let Ok(Ok(receipts)) = run.build(session, &claimed, &targets).await else {
                panic!("every Service builds");
            };
            assert_eq!(receipts.len(), 5);
            let ran = pins(&claimed.ran);
            run.prepare(session, claimed, targets, (receipts, ran))
                .await
                .unwrap_or_else(|unprepared| panic!("{}", unprepared.error))
                .close();
            builds.calls.load(Ordering::SeqCst) - before
        }
    };
    let started = || builds.definitions.lock().unwrap().len();
    let first = || "a".repeat(40);

    let rollout = sent(first(), 20).await;
    assert_eq!(started(), 5, "the first Deployment builds every Service");
    assert_eq!(
        sent(first(), 21).await,
        0,
        "a same-commit redeploy sends no Build"
    );
    assert_eq!(started(), 5);

    edit(vec![("worker.env.ROLE".into(), "changed")]);
    let changed = sent(first(), 22).await;
    assert!(0 < changed && changed < rollout, "{changed} of {rollout}");
    assert_eq!(
        started(),
        6,
        "a changed build argument builds its Service again"
    );

    sent("b".repeat(40), 23).await;
    assert_eq!(started(), 11, "a new commit builds every Service again");

    session.close().await;
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn planning_ends_when_the_first_image_is_sent() {
    let mut steps = Steps::default();
    steps.start(StepName::Planning, None, 100);
    for event in [
        serde_json::json!({ "Sending": { "service": "api" } }),
        serde_json::json!({ "Sending": { "service": "web" } }),
        serde_json::json!({ "Delivered": { "service": "api", "machine": "m1" } }),
    ] {
        delivery(&mut steps, &event);
    }
    let steps: Vec<_> = steps
        .take_changed()
        .into_iter()
        .map(|step| {
            (
                step.name,
                step.service.map(|s| s.to_string()),
                step.ended_at.is_some(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            (StepName::Planning, None, true),
            (StepName::DeliveringImage, Some("api".to_owned()), true),
            (StepName::DeliveringImage, Some("web".to_owned()), false),
        ]
    );
}

#[tokio::test]
async fn a_join_failure_after_live_keeps_the_applied_result_and_marks_cleanup_unconfirmed() {
    let (run, actor) = run();
    prepared(&run).await;
    let at = now_ms() / 1000;
    run.record(RunEvidence::Finishing { live_at: Some(at) })
        .await
        .unwrap();
    let applied = run
        .executed(
            DeployOutcome::Success {
                completed: Vec::new(),
                services: Vec::new(),
            },
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert_eq!(applied.status, DeploymentStatus::Applied);

    let task = tokio::spawn(std::future::pending());
    task.abort();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(None),
        join: tokio::sync::Mutex::new(Some(task)),
    };

    let result = run
        .finish_retirement(&running, Ok(applied), Vec::new())
        .await;
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        (result.is_err(), view.deployment.cleanup),
        (true, Some(ployz_store::CleanupStatus::Unconfirmed)),
        "a failed task must leave its cleanup unconfirmed and surface the error",
    );
    let error = result.unwrap_err();
    assert_eq!(error.message, "Deploy task failed");
    assert_eq!(
        crate::ui::after_result(&crate::failure::Failure::from(error)),
        3
    );
    assert_eq!(view.deployment.status, DeploymentStatus::Applied);
    assert_eq!(view.deployment.live_at, Some(at));
}

#[tokio::test]
async fn a_join_failure_before_the_outcome_is_returned_and_marks_the_run_unknown() {
    let (run, actor) = run();
    prepared(&run).await;
    let task = tokio::spawn(std::future::pending());
    task.abort();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(None),
        join: tokio::sync::Mutex::new(Some(task)),
    };
    let error = running.finished().await.unwrap_err();

    let reported = run.runtime_failed(error.clone()).await;
    let summary = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap()
        .deployment;
    assert_eq!(summary.status, DeploymentStatus::Unknown);
    assert_eq!((summary.live_at, summary.cleanup), (None, None));
    assert_eq!(reported.unwrap_err(), error);
}

#[tokio::test]
async fn a_rejected_switch_write_is_retried_with_the_next_evidence() {
    use ployz_store::testing::backend;

    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (run, actor) = admitted_in(
        Arc::new(ConfigStore::open(&url, backend::key()).unwrap()),
        None,
    );
    prepared(&run).await;
    backend::execute(
        &url,
        "CREATE TRIGGER reject_switch BEFORE UPDATE ON config_deployment         BEGIN SELECT RAISE(ABORT, 'reject first switch write'); END",
    );
    let at = now_ms() / 1000;
    assert!(run.record_switch(Some(at)).await.is_err());
    backend::execute(&url, "DROP TRIGGER reject_switch");

    assert_eq!(run.record(RunEvidence::Alive).await.unwrap().live_at, None);
    run.record(RunEvidence::Finishing { live_at: None })
        .await
        .unwrap();
    let summary = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap()
        .deployment;
    assert_eq!(summary.live_at, Some(at));
}

#[tokio::test]
async fn a_switch_event_survives_abort_before_the_outcome() {
    let (run, actor) = run();
    prepared(&run).await;
    let (session, server, _) = mock_machine().await;
    let at = now_ms() / 1000;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(ployz_core::DeployEvent::Progress {
        completed: 0,
        total: 0,
        rows: vec![],
        live_at: Some(at),
    })
    .unwrap();
    let task = tokio::spawn(async move {
        let _events = tx;
        std::future::pending().await
    });
    let abort = task.abort_handle();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(Some(rx)),
        join: tokio::sync::Mutex::new(Some(task)),
    };
    let scrub = Scrub {
        environment: environment(),
        sealed_at_claim: BTreeSet::new(),
        claim: BTreeSet::new(),
    };
    let query = DeploymentQuery {
        id: run.deployment.clone(),
    };
    let observed = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let view = run.store.read(&actor, &query).unwrap();
                if view.deployment.live_at == Some(at) {
                    assert_eq!(view.deployment.status, DeploymentStatus::Running);
                    break;
                }
                // real-wait: wait for the blocking Store write while the runtime task is held.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the switch is durable before Outcome");
        abort.abort();
    };
    let (outcome, ()) = tokio::join!(run.executing(&session, &running, &scrub, None), observed);
    let error = outcome.unwrap_err();
    assert!(run.runtime_failed(error).await.is_err());
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Unknown);
    assert_eq!(view.deployment.live_at, Some(at));
    server.abort();
}

#[tokio::test]
async fn a_join_error_waits_for_the_store_to_record_unconfirmed_cleanup() {
    a_join_error_during_a_store_outage(false).await;
}

#[tokio::test]
async fn a_join_error_stops_retrying_after_the_cleanup_lease_expires() {
    a_join_error_during_a_store_outage(true).await;
}

async fn a_join_error_during_a_store_outage(expire: bool) {
    use ployz_store::testing::backend;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (run, actor) = admitted_in(
        Arc::new(ConfigStore::open(&url, backend::key()).unwrap()),
        None,
    );
    prepared(&run).await;
    let at = now_ms() / 1000;
    run.record_switch(Some(at)).await.unwrap();
    let recorded = run
        .executed(
            DeployOutcome::Success {
                completed: vec![],
                services: vec![],
            },
            vec![],
            vec![],
        )
        .await;
    assert_eq!(recorded.as_ref().unwrap().status, DeploymentStatus::Applied);
    backend::execute(
        &url,
        "CREATE TRIGGER reject_writes BEFORE UPDATE ON config_deployment
         BEGIN SELECT RAISE(ABORT, 'temporary outage'); END",
    );
    let task = tokio::spawn(std::future::pending());
    task.abort();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(None),
        join: tokio::sync::Mutex::new(Some(task)),
    };
    let finishing = run.finish_retirement(&running, recorded, Vec::new());
    tokio::pin!(finishing);
    assert!(
        // real-wait: let the blocking Store reject an actual evidence write.
        tokio::time::timeout(Duration::from_millis(250), &mut finishing)
            .await
            .is_err(),
        "the SDK returned the join error before the Store accepted unconfirmed cleanup"
    );
    backend::execute(&url, "DROP TRIGGER reject_writes");
    if expire {
        backend::execute(&url, "UPDATE config_deployment SET cleanup_deadline = 1");
    }
    // real-wait: the retry checks the durable lease through a blocking Store call.
    let reported = tokio::time::timeout(Duration::from_secs(5), finishing)
        .await
        .expect("the join error is returned after resolution of the retry");
    assert_eq!(reported.unwrap_err().message, "Deploy task failed");
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.deployment.live_at, Some(at));
    assert_eq!(view.deployment.status, DeploymentStatus::Applied);
    assert_eq!(
        view.deployment.cleanup,
        Some(ployz_store::CleanupStatus::Unconfirmed)
    );
}

#[tokio::test(start_paused = true)]
async fn an_unacknowledged_finished_write_returns_applied_with_cleanup_unconfirmed() {
    use ployz_store::testing::backend;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (run, actor) = admitted_in(
        Arc::new(ConfigStore::open(&url, backend::key()).unwrap()),
        None,
    );
    prepared(&run).await;
    let at = now_ms() / 1000;
    run.record_switch(Some(at)).await.unwrap();
    let outcome = DeployOutcome::Success {
        completed: vec![],
        services: vec![],
    };
    let recorded = run.executed(outcome.clone(), vec![], vec![]).await;
    let through = run.lease_deadline().unwrap();
    backend::execute(
        &url,
        r#"CREATE TRIGGER reject_finished BEFORE UPDATE ON config_deployment
           WHEN OLD.cleanup_status = 'pending' AND NEW.cleanup_status = 'done'
           BEGIN SELECT RAISE(ABORT, 'could not record Finished'); END"#,
    );
    let warning = DeployWarning::LeftRunning {
        service: "shop-production/web".parse().unwrap(),
        machine: "edge".into(),
        container_id: ployz_core::ContainerId::parse("a".repeat(64)).unwrap(),
        reason: "docker is busy".into(),
    };
    let (send, events) = tokio::sync::mpsc::unbounded_channel();
    send.send(ployz_core::DeployEvent::Warning {
        warning: warning.clone(),
    })
    .unwrap();
    drop(send);
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(Some(events)),
        join: tokio::sync::Mutex::new(Some(tokio::spawn(async move { Ok(outcome) }))),
    };
    let finishing = run.finish_retirement(&running, recorded, Vec::new());
    tokio::pin!(finishing);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut finishing)
            .await
            .is_err()
    );
    tokio::time::advance(through.saturating_duration_since(tokio::time::Instant::now())).await;
    let summary = tokio::time::timeout(Duration::from_secs(1), finishing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.status, DeploymentStatus::Applied);
    assert_eq!(summary.live_at, Some(at));
    assert!(
        summary
            .warnings
            .contains(&DeploymentWarning::Deploy { warning })
    );
    assert_eq!(
        summary.cleanup,
        Some(ployz_store::CleanupStatus::Unconfirmed)
    );
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert!(view.deployment.warnings.is_empty());
    assert_eq!(
        view.deployment.cleanup,
        Some(ployz_store::CleanupStatus::Pending)
    );
}

#[tokio::test(start_paused = true)]
async fn the_join_error_returns_when_the_store_is_unreadable_past_the_lease() {
    use ployz_store::testing::backend;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (run, actor) = admitted_in(
        Arc::new(ConfigStore::open(&url, backend::key()).unwrap()),
        None,
    );
    prepared(&run).await;
    let at = now_ms() / 1000;
    run.record_switch(Some(at)).await.unwrap();
    let recorded = run
        .executed(
            DeployOutcome::Success {
                completed: vec![],
                services: vec![],
            },
            vec![],
            vec![],
        )
        .await;
    assert_eq!(recorded.as_ref().unwrap().status, DeploymentStatus::Applied);
    let through = run.lease_deadline().unwrap();
    backend::execute(
        &url,
        "ALTER TABLE config_deployment RENAME TO offline_deployment",
    );
    let task = tokio::spawn(std::future::pending());
    task.abort();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(None),
        join: tokio::sync::Mutex::new(Some(task)),
    };
    let finishing = run.finish_retirement(&running, recorded, Vec::new());
    tokio::pin!(finishing);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut finishing)
            .await
            .is_err()
    );
    // Jump past the acknowledged lease with the deployment table still unavailable.
    tokio::time::advance(through.saturating_duration_since(tokio::time::Instant::now())).await;
    let reported = tokio::time::timeout(Duration::from_secs(1), finishing)
        .await
        .expect("the local lease bounds retries while the Store is unreadable");
    assert_eq!(reported.unwrap_err().message, "Deploy task failed");

    backend::execute(
        &url,
        "ALTER TABLE offline_deployment RENAME TO config_deployment",
    );
    backend::execute(&url, "UPDATE config_deployment SET cleanup_deadline = 1");
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Applied);
    assert_eq!(view.deployment.live_at, Some(at));
    assert_eq!(
        view.deployment.cleanup,
        Some(ployz_store::CleanupStatus::Unconfirmed)
    );
}

#[tokio::test]
async fn a_join_failure_records_pending_live_at_after_the_store_recovers() {
    use ployz_store::testing::backend;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("store.db").display());
    let (run, actor) = admitted_in(
        Arc::new(ConfigStore::open(&url, backend::key()).unwrap()),
        None,
    );
    prepared(&run).await;
    backend::execute(
        &url,
        "CREATE TRIGGER reject_writes BEFORE UPDATE ON config_deployment BEGIN SELECT RAISE(ABORT, 'temporary outage'); END",
    );
    let at = now_ms() / 1000;
    assert!(run.record_switch(Some(at)).await.is_err());
    let recorded = run
        .executed(
            DeployOutcome::Success {
                completed: vec![],
                services: vec![],
            },
            vec![],
            vec![],
        )
        .await;
    assert!(recorded.is_err());
    backend::execute(&url, "DROP TRIGGER reject_writes");
    let task = tokio::spawn(std::future::pending());
    task.abort();
    let running = crate::sdk::RunningDeploy {
        cancel: CancellationToken::new(),
        events: tokio::sync::Mutex::new(None),
        join: tokio::sync::Mutex::new(Some(task)),
    };

    assert!(
        run.finish_retirement(&running, recorded, Vec::new())
            .await
            .is_err()
    );
    let view = run
        .store
        .read(
            &actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    assert_eq!(view.deployment.status, DeploymentStatus::Unknown);
    assert_eq!(view.deployment.live_at, Some(at));
    assert_eq!(
        view.deployment.cleanup,
        Some(ployz_store::CleanupStatus::Pending)
    );
}
