use std::{collections::BTreeMap, sync::Arc};

use ployz_core::{ContainerKind, Namespace, ServiceName};
use ployz_store::tools::cloud::ContainerState;

use super::{ServiceCopy, Started, copies_of, start_one};
use crate::global_fixtures::{container_id, machine, running_on};
use crate::sdk::test_support as support;

#[test]
fn a_services_copies_include_its_hook_containers_marked_as_hooks() {
    let server = machine('a', "alpha");
    let web = running_on(&server, '1');
    let mut hook = running_on(&server, '2');
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    let names = BTreeMap::from([(server.id, &server.name)]);

    let copies = copies_of(
        [&web, &hook].into_iter(),
        &names,
        &Namespace::parse("app").unwrap(),
        &ServiceName::parse("api").unwrap(),
    );

    let copy = |hex, hook| ServiceCopy {
        server: server.name.clone(),
        machine: server.id,
        container: container_id(hex),
        state: ContainerState::Running,
        hook,
    };
    assert_eq!(copies, [copy('1', false), copy('2', true)]);
}

async fn started(service: support::DeployService) -> Started {
    let machine = support::machine('a', "one").machine.id;
    let (address, server) = support::listening(service).await;
    let session = crate::sdk::connect_connections(
        vec![crate::context::Connection::tcp(address)],
        Arc::new(crate::connect::SystemConnector::default()),
    )
    .await
    .unwrap();
    let started = start_one(&session, machine, container_id('1'))
        .await
        .unwrap();
    session.close().await;
    server.abort();
    started
}

#[tokio::test]
async fn a_start_is_serving_once_the_machine_sees_it_serve() {
    let service =
        support::DeployService::new(support::machine('a', "one")).with_observation_barrier();
    assert_eq!(started(service).await, Started::Serving);
}

#[tokio::test(start_paused = true)]
async fn a_start_the_machine_never_sees_serve_is_unconfirmed() {
    let service = support::DeployService::new(support::machine('a', "one"))
        .with_observation_barrier()
        .with_dropped_observations();
    assert_eq!(started(service).await, Started::Unconfirmed);
}
