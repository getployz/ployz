//! Owned runner teardown still joins when following and cancellation both fail.

use super::*;
use ployz_store::{
    Actor, CreateProject, EnvironmentId, OrganizationId, ProjectId, ProjectName, RunEvidence,
    SealingKey,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn a_follow_read_error_cannot_detach_owned_work_even_when_cancel_is_refused() {
    let local =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    let owner = Actor::system(OrganizationId::parse("owner").unwrap());
    local
        .write(
            &owner,
            &CreateProject {
                id: ProjectId::parse(mint()).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(mint()).unwrap(),
                region: None,
            },
        )
        .unwrap();
    let admitted = local
        .write(
            &owner,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse(mint()).unwrap(),
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
        )
        .unwrap();
    let runner = RunnerId::parse("owned-runner").unwrap();
    local.claim(&admitted.id, &runner).unwrap();
    let denied = Actor::system(OrganizationId::parse("other").unwrap());
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let store = Store::for_test(
        super::super::store::Backend::Local(Arc::clone(&local), denied),
        leaf_matches(&matches),
    );
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = Arc::clone(&finished);
    let worker_store = Arc::clone(&local);
    let id = admitted.id.clone();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        released.recv().unwrap();
        let summary = worker_store.record(
            &id,
            &runner,
            RunEvidence::NotExecuted(ployz_store::Failure {
                reason: "owned setup ended".into(),
                cause: Vec::new(),
                same_again: false,
            }),
        )?;
        worker_finished.store(true, Ordering::SeqCst);
        Ok(summary)
    });
    {
        let owned = OwnedRunner {
            store: &store,
            id: admitted.id.clone(),
            lease: crate::sdk::RunLease::default(),
            runtime: LeaseRuntime::new().unwrap(),
            handle: Some(handle),
            ended: tokio_util::sync::CancellationToken::new(),
        };
        let signal = tokio_util::sync::CancellationToken::new();
        let followed = follow(
            leaf_matches(&matches),
            &store,
            &admitted,
            None,
            Some(&owned),
            &signal,
        );
        release.send(()).unwrap();
        assert!(followed.is_err());
    }
    assert!(
        finished.load(Ordering::SeqCst),
        "following failure detached its owned runner"
    );
    let view = local
        .read(&owner, &ployz_store::DeploymentQuery { id: admitted.id })
        .unwrap();
    assert!(!view.deployment.status.in_flight());
}

#[test]
fn following_returns_when_the_owned_runner_ends_not_at_the_next_poll() {
    let local =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap());
    let owner = Actor::system(OrganizationId::parse("owner").unwrap());
    local
        .write(
            &owner,
            &CreateProject {
                id: ProjectId::parse(mint()).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(mint()).unwrap(),
                region: None,
            },
        )
        .unwrap();
    let admitted = local
        .write(
            &owner,
            &Admit::Deploy(Deploy {
                id: DeploymentId::parse(mint()).unwrap(),
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
        )
        .unwrap();
    let runner = RunnerId::parse("owned-runner").unwrap();
    local.claim(&admitted.id, &runner).unwrap();
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let store = Store::for_test(
        super::super::store::Backend::Local(Arc::clone(&local), owner),
        leaf_matches(&matches),
    );
    let ended = tokio_util::sync::CancellationToken::new();
    let worker_ended = ended.clone();
    let worker_store = Arc::clone(&local);
    let id = admitted.id.clone();
    let (followed_reader, followed_writer) = std::io::pipe().unwrap();
    let (ended_at, runner_ended_at) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let ended = worker_ended.drop_guard();
        let mut first_read = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(followed_reader),
            &mut first_read,
        )
        .unwrap();
        // real-wait: following writes its event just before it blocks in its wait, which signals nothing.
        std::thread::sleep(Duration::from_millis(100));
        let summary = worker_store.record(
            &id,
            &runner,
            RunEvidence::NotExecuted(ployz_store::Failure {
                reason: "owned setup ended".into(),
                cause: Vec::new(),
                same_again: false,
            }),
        )?;
        ended_at.send(std::time::Instant::now()).unwrap();
        drop(ended);
        Ok(summary)
    });
    let owned = OwnedRunner {
        store: &store,
        id: admitted.id.clone(),
        lease: crate::sdk::RunLease::default(),
        runtime: LeaseRuntime::new().unwrap(),
        handle: Some(handle),
        ended,
    };
    let events = std::io::BufWriter::new(std::fs::File::from(std::os::fd::OwnedFd::from(
        followed_writer,
    )));
    let followed = follow(
        leaf_matches(&matches),
        &store,
        &admitted,
        Some(events),
        Some(&owned),
        &tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    let since_end = runner_ended_at.recv().unwrap().elapsed();
    assert!(!followed.view.deployment.status.in_flight());
    assert!(
        since_end < FOLLOW_POLL / 2,
        "following returned {since_end:?} after the runner ended"
    );
}

#[test]
fn the_local_cli_reports_unconfirmed_at_expiry_while_a_store_write_is_blocked() {
    use ployz_core::RpcError;
    use ployz_store::{Call, CleanupStatus, DeploymentQuery, Tell, Written};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(serde::Serialize)]
    struct HoldWrite {
        #[serde(skip)]
        entered: mpsc::Sender<()>,
        #[serde(skip)]
        release: mpsc::Receiver<()>,
        #[serde(skip)]
        done: mpsc::Sender<()>,
    }

    impl Tell for HoldWrite {
        type Written = ();

        fn apply(&self, _: &mut Call<'_>) -> Result<(), RpcError> {
            self.entered.send(()).unwrap();
            let _ = self.release.recv_timeout(Duration::from_secs(7));
            self.done.send(()).unwrap();
            Err(RpcError::unavailable("Finished could not commit"))
        }

        fn to_command(&self) -> serde_json::Value {
            unreachable!()
        }

        fn written(_: Written) -> Result<(), RpcError> {
            unreachable!()
        }
    }

    let (local, actor) = ployz_store::testing::shop::shop();
    let (applied, _) = applied_with_cleanup(&local, &actor);
    let id = applied.id.clone();
    let mut followed = Followed {
        view: local
            .read(&actor, &DeploymentQuery { id: id.clone() })
            .unwrap(),
        progress: None,
        reported_live: None,
        successor: None,
    };
    let local = Arc::new(local);
    let worker_store = Arc::clone(&local);
    let worker_actor = actor.clone();
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let store = Store::for_test(super::super::store::Backend::Local(local, actor), &matches);
    let lease = crate::sdk::RunLease::default();
    let worker_lease = lease.clone();
    let (entered, entered_store) = mpsc::channel();
    let (release_store, release) = mpsc::channel();
    let (done, store_done) = mpsc::channel();
    let mut owned = OwnedRunner::start(&store, id, lease, async move {
        let _ = worker_lease
            .wait(crate::sdk::store_call(&worker_store, move |store| {
                store.write(
                    &worker_actor,
                    &HoldWrite {
                        entered,
                        release,
                        done,
                    },
                )
            }))
            .await;
        Ok(applied)
    })
    .unwrap();

    entered_store.recv_timeout(Duration::from_secs(5)).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    owned.lease.acknowledge(i64::try_from(now).unwrap() + 2);
    let settled = owned.settle(false).unwrap();
    owned.refresh(&mut followed, settled.as_ref());
    assert!(owned.lease.expired());
    assert_eq!(
        followed.view.deployment.cleanup,
        Some(CleanupStatus::Unconfirmed)
    );
    assert_eq!(store_done.try_recv(), Err(mpsc::TryRecvError::Empty));
    let result = progress::completion(
        &followed.view,
        None,
        progress::Again::Services(&[]),
        Rerun::plain,
    );
    assert_eq!(ui::after_result(&result.unwrap_err()), 3);
    release_store.send(()).unwrap();
    store_done.recv_timeout(Duration::from_secs(5)).unwrap();
}

fn applied_with_cleanup(local: &ConfigStore, actor: &Actor) -> (DeploymentSummary, RunnerId) {
    let id = DeploymentId::parse(mint()).unwrap();
    let runner = RunnerId::parse("owned").unwrap();
    let request =
        serde_json::from_value(serde_json::json!({ "id": id, "services": ["web"] })).unwrap();
    local.write(actor, &Admit::Deploy(request)).unwrap();
    let claimed = local.claim(&id, &runner).unwrap();
    let preview = serde_json::from_value(serde_json::json!({
        "namespace": claimed.intent.namespace, "operations": [], "warnings": [],
        "would_remove": [], "preserved_volumes": [],
    }))
    .unwrap();
    local
        .record(&id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    local
        .record(&id, &runner, RunEvidence::Finishing { live_at: Some(2) })
        .unwrap();
    let applied = local
        .record(
            &id,
            &runner,
            RunEvidence::Executed {
                outcome: Box::new(ployz_core::DeployOutcome::Success {
                    completed: vec![],
                    services: vec![],
                }),
                progress: vec![],
                removed: vec![],
            },
        )
        .unwrap();
    assert_eq!(applied.status, DeploymentStatus::Applied);
    assert_eq!(applied.cleanup, Some(ployz_store::CleanupStatus::Pending));
    (applied, runner)
}

#[test]
fn a_follower_returns_after_a_late_renewal_of_expired_cleanup() {
    use ployz_store::testing::{backend, shop};
    use std::sync::mpsc;

    let home = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", home.path().join("store.db").display());
    let (local, actor) = shop::shop_on(ConfigStore::open(&url, backend::key()).unwrap());
    let (applied, runner) = applied_with_cleanup(&local, &actor);
    backend::execute(&url, "UPDATE config_deployment SET cleanup_deadline = 1");
    let _ = local.record(
        &applied.id,
        &runner,
        RunEvidence::Finishing { live_at: None },
    );
    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "deploy"])
        .unwrap();
    let store = Store::for_test(
        super::super::store::Backend::Local(Arc::new(local), actor),
        &matches,
    );

    let signal = tokio_util::sync::CancellationToken::new();
    let stop = signal.clone();
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(5)).is_err() {
            stop.cancel();
        }
    });
    let result = follow(
        leaf_matches(&matches),
        &store,
        &applied,
        None,
        None,
        &signal,
    );
    let _ = done.send(());
    watchdog.join().unwrap();
    assert!(
        !signal.is_cancelled(),
        "the follower waited for a new cleanup lease"
    );
    assert_eq!(
        result.unwrap().view.deployment.cleanup,
        Some(ployz_store::CleanupStatus::Unconfirmed),
    );
}
