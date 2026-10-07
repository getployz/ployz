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
