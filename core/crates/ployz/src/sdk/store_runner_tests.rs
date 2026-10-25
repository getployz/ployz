//! Rung 1: cancellation polls the real Store and waits for owned SDK child outcomes.

use super::*;
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
    use ployz_store::{
        Actor, Admit, CreateProject, CreateService, Deploy, EnvironmentRef, OrganizationId,
        ProjectId, ProjectName, SealingKey, ServiceLineageId,
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
                prune: false,
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
            let stopped = Arc::clone(&stopped);
            let cleanup = after_cancel(&token);
            let running =
                super::super::running::Running::spawn(token, move |_reporter| async move {
                    cleanup.await.unwrap();
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
            Err(internal("released"))
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
    let (followed, shown) = tokio::join!(run.follow(&target, &running), watch);
    assert!(followed.unwrap().is_err());
    assert!(
        shown.is_ok(),
        "the log said nothing while queued: {:?}",
        log()
    );
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
                        build_concurrency: None,
                    },
                    reason: super::super::prepare::BuilderReason::Spread,
                    rejections: Vec::new(),
                },
            )));
            Err(internal("the build failed"))
        });
    assert!(run.follow(&target, &running).await.unwrap().is_err());
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
    use ployz_store::{Change, CreateService, Edit, EnvironmentRef, ServiceLineageId, SettingPath};
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
    run.store
        .write(
            actor,
            &Edit {
                environment: EnvironmentRef::default(),
                expect: None,
                changes: vec![Change::Set {
                    path: SettingPath::parse(&format!("web.env.{name}")).unwrap(),
                    value: serde_json::json!({ "secret": "sealed-later" }),
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
    use ployz_store::tools::{LineCap, LogExcerpt};
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
    use ployz_store::tools::{LineCap, LogExcerpt};
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
    service.uninspectable.insert(container_id('d'));
    service.uninspectable.insert(container_id('e'));
    service.retained_envs.as_mut().unwrap().insert(
        container_id('e'),
        BTreeMap::from([("PLAIN".to_owned(), "from-docker".to_owned())]),
    );
    let (address, server) = support::listening(service).await;
    let session = connect_connections(
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
        .failed_log(&client, &scrub, machine, container_id('c'))
        .await
        .0;
    let unreadable = run
        .failed_log(&client, &scrub, machine, container_id('d'))
        .await
        .0;
    let removed = run
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
        lines(&removed).contains(&"plain [secret PLAIN]".to_owned()),
        "{removed:?}"
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
        .failed_log(&client, &scrub, machine, container_id('c'))
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
        .failed_log(&client, &scrub, machine, container_id('c'))
        .await
        .0;
    let unreadable = run
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
fn a_sealed_build_with_no_server_in_its_walk_fails_saying_it_runs_on_a_server() {
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
            "sealed": "NPM_TOKEN",
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
        },
    ) else {
        panic!("a sealed build no Server takes fails");
    };
    assert_eq!(
        message,
        "Build failed. api: Builds that read a sealed value run on your server. \
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

struct Recording {
    client: Client,
    wants: std::sync::Mutex<Vec<ployz_core::ContainerWant>>,
}

impl MachineOperations for Recording {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        MachineOperations::prepare_volumes(&self.client, machine_id, specs).await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ployz_core::ContainerObservation>, RpcError> {
        MachineOperations::service_containers(&self.client, service).await
    }

    async fn replicated_containers(
        &self,
    ) -> Result<Vec<ployz_core::ContainerObservation>, RpcError> {
        MachineOperations::replicated_containers(&self.client).await
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ployz_core::ContainerWant,
    ) -> Result<(ployz_core::ContainerNow, Option<ployz_core::HealthVerdict>), RpcError> {
        self.wants.lock().unwrap().push(want.clone());
        MachineOperations::ensure_container(&self.client, machine_id, want).await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ployz_core::ContainerDetails, RpcError> {
        MachineOperations::inspect_container(&self.client, machine_id, container_id).await
    }

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ployz_core::ResolvedServiceSpec,
    ) -> Result<ployz_core::ContainerLimitsUpdated, RpcError> {
        MachineOperations::update_container_limits(&self.client, machine_id, container_id, spec)
            .await
    }

    async fn remove_volume(&self, id: &ployz_core::DockerVolumeId) -> Result<(), RpcError> {
        MachineOperations::remove_volume(&self.client, id).await
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &ployz_core::SlotKey,
        spec: &ployz_core::ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        MachineOperations::runs_global(&self.client, machine_id, slot, spec).await
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ployz_core::ResolvedServiceSpec,
    ) -> Result<ployz_core::ImagePulled, RpcError> {
        MachineOperations::ensure_image(&self.client, machine_id, spec).await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ployz_core::ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<ployz_core::EnsuredContainer, RpcError> {
        self.wants.lock().unwrap().push(want.clone());
        MachineOperations::ensure_unsettled(&self.client, machine_id, want, timeout).await
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<ployz_core::RoutesRead, RpcError> {
        MachineOperations::read_routes(&self.client, machine_id, service).await
    }

    async fn read_entry_routes(
        &self,
        service: &QualifiedService,
    ) -> Result<ployz_core::RoutesRead, RpcError> {
        MachineOperations::read_entry_routes(&self.client, service).await
    }
}

fn left_running(
    run: &Run,
    actor: &ployz_store::Actor,
    services: &[(&str, NodeStatus, char)],
) -> DeploymentView {
    use crate::global_fixtures::container_id;
    let mut view = run
        .store
        .read(
            actor,
            &DeploymentQuery {
                id: run.deployment.clone(),
            },
        )
        .unwrap();
    for (name, outcome, copy) in services {
        let name = ServiceName::parse(*name).unwrap();
        view.nodes.push(ployz_store::NodeOutcome {
            node: DeployedNode::Service {
                id: ployz_store::ServiceLineageId::parse("00000000-0000-4000-8000-000000000006")
                    .unwrap(),
                name: name.clone(),
            },
            outcome: *outcome,
            rows: Vec::new(),
            cancelled: false,
        });
        view.runtime_names.insert(name.clone(), name.clone());
        view.deployment.warnings.push(DeploymentWarning::Deploy {
            warning: DeployWarning::LeftRunning {
                service: QualifiedService::new(view.namespace.clone(), name),
                container_id: container_id(*copy),
                machine: "one".into(),
                reason: "proxy on two did not confirm".into(),
            },
        });
    }
    view
}

fn holding_c() -> super::super::test_support::DeployService {
    use super::super::test_support as support;
    support::DeployService::new(support::machine('a', "one")).with_rotation(
        crate::global_fixtures::container_id('c'),
        ployz_core::CopyRotation::In,
    )
}

struct Retired {
    outcome: RetireOutcome,
    wants: Vec<ployz_core::ContainerWant>,
    removed: BTreeSet<ContainerId>,
}

async fn retire(
    view: &DeploymentView,
    machine: super::super::test_support::DeployService,
) -> Retired {
    let removed = machine.removed_containers();
    let (mut client, server) = super::super::test_support::connected(machine).await;
    let machines = client.machines().await.unwrap();
    let recording = Recording {
        client,
        wants: std::sync::Mutex::default(),
    };
    let outcome = named_left(
        view,
        retire_copies(&recording, &machines, left_copies(view)).await,
    );
    server.abort();
    Retired {
        outcome,
        wants: recording.wants.into_inner().unwrap(),
        removed: removed.lock().unwrap().clone(),
    }
}

#[tokio::test]
async fn a_deployment_that_left_nothing_running_retires_without_connecting() {
    let (run, actor) = run();
    let retired = retire_old(run.store, actor, run.deployment, Vec::new())
        .await
        .unwrap();
    assert_eq!(retired, RetireOutcome::default());
}

#[tokio::test]
async fn another_organizations_deployment_is_not_found() {
    let (run, _) = run();
    let other = ployz_store::Actor::system(ployz_store::OrganizationId::parse("other").unwrap());
    let error = retire_old(run.store, other, run.deployment, Vec::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, RpcErrorCode::NotFound);
}

#[tokio::test]
async fn a_left_copy_already_gone_counts_as_retired() {
    let (run, actor) = run();
    let view = left_running(&run, &actor, &[("web", NodeStatus::Deployed, 'c')]);
    let mut machine = holding_c();
    machine
        .uninspectable
        .insert(crate::global_fixtures::container_id('c'));
    let retired = retire(&view, machine).await;
    assert_eq!(retired.outcome, RetireOutcome::default());
    assert!(retired.wants.is_empty(), "{:?}", retired.wants);
}

#[tokio::test]
async fn a_left_copy_whose_machine_does_not_answer_stays_left_naming_it() {
    let (run, actor) = run();
    let view = left_running(&run, &actor, &[("web", NodeStatus::Deployed, 'c')]);
    let retired = retire(&view, holding_c().fail_inspects("unreachable")).await;
    assert_eq!(
        retired.outcome.left,
        [RetireLeft {
            machine: "one".into(),
            service: Some(ServiceName::parse("web").unwrap()),
            reason: "one did not answer".into(),
        }]
    );
}

/// What the planner records when `machine`, which may hold an old copy, did not
/// answer before the Deploy.
fn cut_off(view: &mut DeploymentView, machine: &str) {
    use crate::global_fixtures::{machine as named, requested};
    use ployz_core::{MachineFailure, MembershipObservation};
    let away = named('3', machine);
    let snapshot = crate::deploy::DeploySnapshot {
        machines: vec![
            MachineObservation::new(named('1', "observer"), MembershipObservation::Up),
            MachineObservation::new(away.clone(), MembershipObservation::Up),
        ],
        container_failures: vec![MachineFailure {
            machine_id: away.id,
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target Machine RPC timed out".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        }],
        ..Default::default()
    };
    let spec = requested(ployz_core::ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::MIN,
    });
    let intent =
        ployz_core::DeployIntent::apply_all(view.namespace.clone(), [&spec], Default::default());
    let preview = crate::deploy::preview_deploy(&intent, &snapshot).unwrap();
    assert!(
        preview.warnings.contains(&DeployWarning::MixedAfterReturn {
            machine: machine.into()
        }),
        "{:?}",
        preview.warnings
    );
    match &mut view.preview {
        Some(recorded) => recorded.warnings.extend(preview.warnings),
        None => view.preview = Some(preview),
    }
}

type CutOff = fn(&mut DeploymentView, &str);

/// What the executor records when it loses `machine` mid-run.
fn lost_mid_run(view: &mut DeploymentView, machine: &str) {
    view.deployment.warnings.push(DeploymentWarning::Deploy {
        warning: DeployWarning::MixedAfterReturn {
            machine: machine.into(),
        },
    });
}

#[tokio::test]
async fn a_machine_the_deploy_could_not_reach_stays_left_once_without_a_service() {
    let (run, actor) = run();
    let sources = [("planner", cut_off as CutOff), ("executor", lost_mid_run)];
    for (source, record) in sources {
        let mut view = left_running(&run, &actor, &[]);
        record(&mut view, "machine-3");
        record(&mut view, "machine-3");
        let retired = retire(&view, holding_c()).await;
        assert_eq!(
            retired.outcome.kept,
            [RetireLeft {
                machine: "machine-3".into(),
                service: None,
                reason: "machine-3 was cut off during the Deploy, so a copy there may still run. \
                         Deploy again once it is back."
                    .into(),
            }],
            "{source}"
        );
        assert!(retired.outcome.left.is_empty(), "{source}");
        assert!(retired.wants.is_empty(), "{:?}", retired.wants);
        assert!(retired.removed.is_empty());
    }
}

#[tokio::test]
async fn a_cut_off_machine_with_a_copy_left_running_is_named_once() {
    let (run, actor) = run();
    let mut view = left_running(&run, &actor, &[("web", NodeStatus::Deployed, 'c')]);
    cut_off(&mut view, "one");
    let retired = retire(&view, holding_c().fail_inspects("unreachable")).await;
    assert_eq!(
        retired.outcome.left,
        [RetireLeft {
            machine: "one".into(),
            service: Some(ServiceName::parse("web").unwrap()),
            reason: "one did not answer".into(),
        }]
    );
}

#[tokio::test]
async fn a_machine_whose_reported_copy_retired_is_not_named_cut_off() {
    let (run, actor) = run();
    let mut view = left_running(&run, &actor, &[("web", NodeStatus::Deployed, 'c')]);
    cut_off(&mut view, "one");
    let retired = retire(&view, holding_c()).await;
    assert_eq!(retired.outcome, RetireOutcome::default());
}

#[tokio::test]
async fn a_failed_services_left_copy_is_kept_on_every_try_without_a_retry() {
    let (run, actor) = run();
    let view = left_running(&run, &actor, &[("web", NodeStatus::Failed, 'c')]);
    for attempt in 1..=2 {
        let retired = retire(&view, holding_c()).await;
        assert_eq!(
            retired.outcome,
            RetireOutcome {
                left: Vec::new(),
                kept: vec![RetireLeft {
                    machine: "one".into(),
                    service: Some(ServiceName::parse("web").unwrap()),
                    reason:
                        "web did not deploy, so its old copy stays. Deploy again to replace it."
                            .into(),
                }],
            },
            "try {attempt}"
        );
        assert!(
            retired.wants.is_empty(),
            "try {attempt}: {:?}",
            retired.wants
        );
        assert!(retired.removed.is_empty(), "try {attempt}");
    }
}

#[tokio::test]
async fn only_a_removed_services_left_copy_may_leave_it_empty() {
    let (run, actor) = run();
    for (outcome, allow_empty) in [(NodeStatus::Removed, true), (NodeStatus::Deployed, false)] {
        let view = left_running(&run, &actor, &[("web", outcome, 'c')]);
        let retired = retire(&view, holding_c()).await;
        assert_eq!(retired.outcome, RetireOutcome::default(), "{outcome:?}");
        assert!(
            matches!(
                retired.wants.first(),
                Some(ployz_core::ContainerWant::Out { allow_empty: sent, .. }) if *sent == allow_empty
            ),
            "{outcome:?}: {:?}",
            retired.wants
        );
        assert_eq!(
            retired.removed,
            BTreeSet::from([crate::global_fixtures::container_id('c')]),
            "{outcome:?}"
        );
    }
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
