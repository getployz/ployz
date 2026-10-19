//! Rung 1: cancellation polls the real Store and waits for owned SDK child outcomes.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

fn run() -> (Run, ployz_store::Actor) {
    use ployz_store::{
        Actor, Admit, CreateProject, Deploy, EnvironmentId, EnvironmentRef, OrganizationId,
        ProjectId, ProjectName, SealingKey,
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
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
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
    let work = async {
        token.cancelled().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
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
    assert_eq!(run.status().await.unwrap(), DeploymentStatus::Unknown);
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

#[test]
fn a_taken_over_run_reports_what_it_completed_and_leaves_the_stopped_operation_unexecuted() {
    let operation = |id: char| DeployOperation::RemoveContainer {
        machine_id: ployz_core::MachineId::parse("a".repeat(32)).unwrap(),
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
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
            let work_token = token.clone();
            let stopped = Arc::clone(&stopped);
            let running =
                super::super::running::Running::spawn(token, move |_reporter| async move {
                    work_token.cancelled().await;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    stopped.fetch_add(1, Ordering::SeqCst);
                    Err(RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "Remote termination is unknown".into(),
                        details: serde_json::json!({"preparation":{"kind":"unknown"}}),
                        cause: Vec::new(),
                    })
                });
            (target, running)
        })
        .collect();
    let error = stop_builds(
        &builds,
        RpcError {
            code: RpcErrorCode::Conflict,
            message: "The initial build report was refused".into(),
            details: serde_json::json!({"revision":7}),
            cause: vec!["runner lost ownership".into()],
        },
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
    let secret_env = BTreeMap::from([
        (
            ServiceName::parse("web").unwrap(),
            ["DB_PASSWORD".to_owned()].into(),
        ),
        (
            ServiceName::parse("worker").unwrap(),
            ["API_TOKEN".to_owned()].into(),
        ),
    ]);
    Scrub::new(&target, &secret_env)
}

// Catches a failed log that shows a Sealed value, from its own Service or another the
// claim lowered, even where a plain variable holds the same value, or that scrubs a plain
// or image value such as NODE_ENV=production.
#[test]
fn a_failed_log_is_scrubbed_of_sealed_values_only() {
    use ployz_store::tools::{LineCap, LogExcerpt};
    let scrub = scrub_target();
    let sealed = scrub
        .sealed(&ServiceName::parse("worker").unwrap())
        .expect("a lowered Service has its Sealed values");
    let line = "npm warn config production; login hunter2-long at debug-verbose with tok-worker-9 password=open-sesame\n";
    let excerpt = LogExcerpt::scrub([line.to_owned()], sealed, LineCap::default());
    assert_eq!(
        excerpt.lines(),
        [
            "npm warn config production; login [secret DB_PASSWORD] at debug-verbose with [secret API_TOKEN] password=[redacted]"
        ]
    );
}

// Catches a removed Service's log shown with no idea of the Sealed values it ran under.
#[test]
fn a_service_the_claim_did_not_lower_has_no_sealed_values() {
    assert!(
        scrub_target()
            .sealed(&ServiceName::parse("api").unwrap())
            .is_none()
    );
}
