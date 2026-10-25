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
