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
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let summary = worker_store.record(
            &id,
            &runner,
            RunEvidence::NotExecuted(ployz_store::Failure {
                reason: "owned setup ended".into(),
                cause: Vec::new(),
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
        };
        let signal = tokio_util::sync::CancellationToken::new();
        assert!(follow(&store, &admitted, None, Some(&owned), &signal).is_err());
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
