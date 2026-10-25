//! Rung 2: the public Store runner cancels during real connection and preview RPC waits.

use super::support::*;
use ployz_store::{
    Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId,
    DeploymentStatus, EnvironmentId, EnvironmentRef, OrganizationId, ProjectId, ProjectName,
    RunnerId, SealingKey, ServiceLineageId,
};
use std::{sync::Arc, time::Duration};

fn deployment() -> (Arc<ConfigStore>, Actor, DeploymentId) {
    let store =
        Arc::new(ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap());
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ployz_core::ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    let id = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write_trusted(
            &who,
            &Admit::Deploy(Deploy {
                id: id.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    (store, who, id)
}

#[tokio::test]
async fn cancel_during_initial_connection_returns_before_the_unresponsive_entry() {
    let (store, actor, id) = deployment();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("connecting").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    ));
    let (_held_connection, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    store
        .write(&actor, &ployz_store::Cancel { deployment: id })
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(4), running)
        .await
        .expect("cancel must stop the read-only connection wait")
        .unwrap()
        .unwrap();
    assert_eq!(summary.status, DeploymentStatus::Cancelled);
}

#[tokio::test]
async fn cancel_during_image_only_preview_stops_the_rpc_without_admitting_work() {
    let (store, actor, id) = deployment();
    let blocked = Arc::new(tokio::sync::Notify::new());
    let mut service = DeployService::new(machine('a', "entry"));
    let mutations = service.mutating_rpcs();
    service.listing_blocked = Some(Arc::clone(&blocked));
    let (address, server) = listening(service).await;
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("previewing").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    ));
    tokio::time::timeout(Duration::from_secs(3), blocked.notified())
        .await
        .unwrap();
    store
        .write(&actor, &ployz_store::Cancel { deployment: id })
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(4), running)
        .await
        .expect("cancel must stop the image-only preview wait")
        .unwrap()
        .unwrap();
    assert_eq!(summary.status, DeploymentStatus::Cancelled);
    assert_eq!(mutations.load(std::sync::atomic::Ordering::SeqCst), 0);
    server.abort();
}

/// Runs the Deployment until it creates, then has another runner take the Environment over.
async fn taken_over_mid_create() -> (Arc<ConfigStore>, Actor, DeploymentId) {
    let (store, actor, id) = deployment();
    let service = DeployService::new(machine('a', "entry")).hold_health();
    let (address, server) = listening(service.clone()).await;
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("first").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    ));
    tokio::time::timeout(Duration::from_secs(10), service.health_waited())
        .await
        .expect("the run starts executing");
    let next = DeploymentId::parse("00000000-0000-4000-8000-000000000102").unwrap();
    store
        .write_trusted(
            &actor,
            &Admit::Deploy(Deploy {
                id: next.clone(),
                environment: EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            }),
            &ployz_store::Trusted::default(),
        )
        .unwrap();
    store
        .claim(&next, &RunnerId::parse("second").unwrap())
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .expect("a lost runner stops at its next poll")
        .unwrap()
        .expect("the store takes the interrupted outcome");
    assert_eq!(summary.status, DeploymentStatus::Unknown);
    server.abort();
    (store, actor, id)
}

#[tokio::test]
async fn a_run_another_runner_takes_over_mid_execution_records_it_interrupted() {
    let (store, actor, id) = taken_over_mid_create().await;
    let view = store
        .read(&actor, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let Some(ployz_store::Outcome::Executed { summary, .. }) = &view.deployment.outcome else {
        panic!(
            "expected an executed outcome, got {:?}",
            view.deployment.outcome
        );
    };
    assert_eq!(
        summary.get("reason").and_then(serde_json::Value::as_str),
        Some("interrupted"),
        "{summary}"
    );
    assert_eq!(
        summary.get("completed"),
        Some(&serde_json::json!(0)),
        "{summary}"
    );
    assert_eq!(
        summary.get("unexecuted"),
        Some(&serde_json::json!(1)),
        "{summary}"
    );
    assert!(
        view.nodes
            .iter()
            .all(|node| node.outcome == ployz_store::NodeStatus::NotAttempted),
        "an interrupted run confirms nothing into Applied State: {:?}",
        view.nodes
    );
}

#[tokio::test]
async fn a_takeover_nobody_cancelled_records_no_row_or_node_cancelled() {
    let (store, actor, id) = taken_over_mid_create().await;
    let view = store
        .read(&actor, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let nodes = serde_json::to_value(&view.nodes).unwrap();
    let web = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "web")
        .unwrap();
    assert_eq!(web.get("cancelled"), None, "{web}");
    let rows = web["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "{web}");
    assert!(
        rows.iter().all(|row| row.get("cancelled").is_none()),
        "only the Deployment's cancel stops work as cancelled: {web}"
    );
}

#[tokio::test]
async fn a_takeover_records_the_work_it_stopped_interrupted_not_failed() {
    let (store, actor, id) = taken_over_mid_create().await;
    let view = store
        .read(&actor, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let nodes = serde_json::to_value(&view.nodes).unwrap();
    let web = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "web")
        .unwrap();
    let states: Vec<_> = web["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["state"].clone(), row["interrupted"].clone()))
        .collect();
    // Failed as before Interrupted, so older readers read it; marked for newer ones.
    assert_eq!(
        states,
        [(serde_json::json!("failed"), serde_json::json!(true))],
        "{web}"
    );
}

#[tokio::test]
async fn cancel_during_a_create_records_the_work_it_stopped_cancelled_not_failed() {
    let (store, actor, id) = deployment();
    let service = DeployService::new(machine('a', "entry")).hold_health();
    let (address, server) = listening(service.clone()).await;
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("cancelled").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    ));
    tokio::time::timeout(Duration::from_secs(10), service.health_waited())
        .await
        .expect("the run starts creating");
    store
        .write(
            &actor,
            &ployz_store::Cancel {
                deployment: id.clone(),
            },
        )
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("cancel must stop the create")
        .unwrap()
        .unwrap();
    assert_eq!(summary.status, DeploymentStatus::Cancelled);

    let view = store
        .read(&actor, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let nodes = serde_json::to_value(&view.nodes).unwrap();
    let web = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "web")
        .unwrap();
    // Failed as before Cancelled, so older readers read it; marked for newer ones.
    assert_eq!(
        (&web["outcome"], &web["cancelled"]),
        (&serde_json::json!("failed"), &serde_json::json!(true)),
        "{web}"
    );
    let states: Vec<_> = web["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["server"].clone(),
                row["state"].clone(),
                row["cancelled"].clone(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [(
            serde_json::json!("entry"),
            serde_json::json!("failed"),
            serde_json::json!(true)
        )],
        "{web}"
    );
    server.abort();
}

#[tokio::test]
async fn cancel_during_cleanup_records_each_copy_left_running_by_machine_name() {
    let (store, actor, id) = deployment();
    let (entry, edge) = (machine('a', "entry"), machine('b', "edge"));
    let earlier = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "web:1", "pull_policy": "always" }
    }))
    .unwrap();
    let mut old = running_container_in(&entry, &earlier, "shop-production", '1');
    old.try_update(|parts| parts.rotation = Some(ployz_core::CopyRotation::In))
        .unwrap();
    let old_id = old.container_id;
    let service = DeployService::new(entry.clone())
        .with_machines(vec![entry, edge.clone()])
        .never_unroute_on(edge.machine.id);
    service.listed_containers().lock().unwrap().push(old);
    let (address, server) = listening(service.clone()).await;
    let running = tokio::spawn(ployz::sdk::run_deployment(
        ployz_core::Operator::Cloud,
        Arc::clone(&store),
        id.clone(),
        RunnerId::parse("cleaning").unwrap(),
        vec![ployz::context::Connection::tcp(address)],
        Ok(Default::default()),
    ));
    tokio::time::timeout(Duration::from_secs(10), service.unrouted_copy(&old_id))
        .await
        .expect("cleanup takes the old copy out of rotation");
    store
        .write(
            &actor,
            &ployz_store::Cancel {
                deployment: id.clone(),
            },
        )
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("cancel must stop the unrouted wait")
        .unwrap()
        .unwrap();

    assert_eq!(
        summary.status,
        DeploymentStatus::Applied,
        "the new copy serves; only its cleanup was cut short"
    );
    let view = store
        .read(&actor, &ployz_store::DeploymentQuery { id })
        .unwrap();
    let left = view
        .deployment
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            ployz_store::DeploymentWarning::Deploy { warning } => Some(warning.to_string()),
            ployz_store::DeploymentWarning::Fragile { .. }
            | ployz_store::DeploymentWarning::NotSet { .. }
            | ployz_store::DeploymentWarning::NotSetInFile { .. }
            | ployz_store::DeploymentWarning::IngressNotServing { .. }
            | ployz_store::DeploymentWarning::IngressUnchecked { .. } => None,
        })
        .filter(|line| line.contains("left running"))
        .collect::<Vec<_>>();
    assert_eq!(
        left,
        ["shop-production/web left running on entry (interrupted). The next deploy cleans it up."]
    );
    server.abort();
}
