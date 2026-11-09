//! Shipping Git Services again as they run: at the commit they run, beside a push
//! that waits for CI. On SQLite and on Postgres (see `backend`).

use ployz_core::{DeployOutcome, DeployPreview, ServiceName};
use ployz_store::{
    Actor, Admit, Command, ConfigStore, DeploymentId, DeploymentStatus, EnvironmentRef, Reapply,
    RunEvidence, RunnerId, Trusted, Written,
};
use serde_json::{Value, json};

mod backend;
mod shop;
use shop::*;

/// Run Deployment `id` to success, confirming each of `services`.
fn apply(store: &ConfigStore, id: &DeploymentId, services: &[&str]) {
    let runner = RunnerId::parse("runner").unwrap();
    store.claim(id, &runner).unwrap();
    let operations: Vec<Value> = (0..services.len())
        .map(|at| json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": format!("{at:064}")}))
        .collect();
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": services.iter().zip(&operations).enumerate().map(|(index, (service, operation))| json!({
            "index": index, "machine_id": "a".repeat(32), "service_name": service,
            "operation": operation, "status": {"type": "pending"}
        })).collect::<Vec<_>>(),
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({ "type": "success", "completed": operations })).unwrap();
    store
        .record(
            id,
            &runner,
            RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

/// `web` and `api` deployed by a push of `H1`.
fn deployed_at_h1() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    publish(&store, &who);
    let pushed = system(&store, &push(None, Some(H1), None));
    let [admitted] = <[_; 1]>::try_from(pushed.admitted).unwrap();
    apply(&store, &admitted.deployment.id, &["web", "api"]);
    (store, who)
}

fn reapply(store: &ConfigStore, who: &Actor, n: u8) -> DeploymentId {
    let id = DeploymentId::parse(format!("00000000-0000-4000-8000-0000000002{n:02}")).unwrap();
    let written = store
        .write_trusted(
            who,
            &Command::Admit(Admit::Reapply(Reapply {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: vec![ServiceName::parse("web").unwrap()],
                message: None,
            })),
            &Trusted::default(),
        )
        .unwrap();
    assert!(matches!(written, Written::Deployment(_)), "{written:?}");
    id
}

#[test]
fn a_reapply_builds_the_commit_a_service_runs() {
    let (store, who) = deployed_at_h1();
    let id = reapply(&store, &who, 1);
    let sources = store.sources(&id).unwrap();
    let web = sources
        .iter()
        .find(|source| source.service.as_str() == "web")
        .unwrap();
    assert_eq!(
        web.commit.as_ref().map(ToString::to_string).as_deref(),
        Some(H1)
    );
}

#[test]
fn a_reapply_leaves_a_push_waiting_for_ci_waiting() {
    let (store, who) = deployed_at_h1();
    set(&store, &who, "web.waitForCi", json!(true));
    publish(&store, &who);
    system(&store, &push(Some(H1), Some(H2), None));
    let [hold] = <[_; 1]>::try_from(held(&store, &who)).unwrap();

    reapply(&store, &who, 1);
    let still = deployments(&store, &who)
        .into_iter()
        .find(|deployment| deployment.id == hold.id)
        .unwrap();
    assert_eq!(still.status, DeploymentStatus::WaitingForCi);
}
