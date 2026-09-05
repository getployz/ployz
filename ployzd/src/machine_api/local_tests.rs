//! Tests for the Machine RPC boundary.

use super::{MachineService, local_error, store_error};
use crate::corrosion::{AdminClient, fake_cluster};
use crate::machine::{LocalMachineError, LocalMachineStore, StoreError};
use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, GET_CONTAINER_OBSERVATIONS_CAPABILITY,
    GetContainerObservationsRequest, HealthObservation, MachineId, MachineRpc, ProjectName,
    ResolvedServiceSpec, RpcErrorCode, RpcResponseBody, RuntimeWatchRequest, ServiceId,
    ServiceName, op,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use tonic::{Code, Request};

#[test]
fn non_participating_update_is_a_typed_conflict() {
    assert_eq!(
        store_error(StoreError::NotParticipating).code,
        RpcErrorCode::Conflict
    );
}

#[test]
fn allocator_not_quiet_is_retryable_unavailable() {
    let RpcResponseBody::Error(error) = local_error(LocalMachineError::AllocatorNotQuiet)
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .body
    else {
        panic!("expected error payload");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Allocator is not quiet");
}

#[test]
fn not_allocator_does_not_allocate() {
    let RpcResponseBody::Error(error) = local_error(LocalMachineError::NotAllocator)
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .body
    else {
        panic!("expected error payload");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "this Machine is not the Allocator");
}

#[tokio::test]
async fn replicated_container_observations_are_advertised_only_with_a_cluster_store() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-container-observation-capability-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let without_cluster =
        MachineService::with_cluster(Arc::clone(&store), watch::channel(false).0, None)
            .describe_contract(Request::new(
                op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::DescribeContract>()
            .unwrap();
    assert!(!without_cluster.supports(GET_CONTAINER_OBSERVATIONS_CAPABILITY));

    let (replicated, server) = fake_cluster::store().await;
    let with_cluster = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    )
    .describe_contract(Request::new(
        op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
            .encode()
            .unwrap(),
    ))
    .await
    .unwrap()
    .into_inner()
    .decode_response()
    .unwrap()
    .decode::<op::DescribeContract>()
    .unwrap();
    assert!(with_cluster.supports(GET_CONTAINER_OBSERVATIONS_CAPABILITY));
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn replicated_container_observation_wait_rejects_a_long_hold() {
    let (replicated, server) = fake_cluster::store().await;
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-container-observation-bound-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    );
    let response = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        service.get_container_observations(Request::new(
            op::GetContainerObservations::into_request(GetContainerObservationsRequest {
                container_ids: vec![ContainerId::parse("a".repeat(64)).unwrap()],
                wait_millis: 5_001,
            })
            .encode()
            .unwrap(),
        )),
    )
    .await
    .expect("an overlong hold is rejected immediately")
    .unwrap()
    .into_inner()
    .decode_response()
    .unwrap();
    assert!(matches!(
        response.body,
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::InvalidArgument
    ));
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn replicated_container_observations_are_complete_and_do_not_use_docker() {
    let (replicated, server) = fake_cluster::store().await;
    let present = container_observation('a');
    replicated.publish_container(&present).await.unwrap();
    let missing = ContainerId::parse("b".repeat(64)).unwrap();
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-container-observations-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    );

    let response = service
        .get_container_observations(Request::new(
            op::GetContainerObservations::into_request(GetContainerObservationsRequest {
                container_ids: vec![present.container_id, missing],
                wait_millis: 0,
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::GetContainerObservations>()
        .unwrap();

    assert_eq!(
        response.containers,
        BTreeMap::from([(present.container_id, Some(present)), (missing, None)])
    );
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn replicated_container_observation_store_failure_is_not_absent() {
    let (replicated, server) = fake_cluster::store().await;
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-container-observation-store-error-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated, AdminClient::new("/no/such/admin.sock"))),
    );
    server.abort();
    let _ = server.await;

    let response = service
        .get_container_observations(Request::new(
            op::GetContainerObservations::into_request(GetContainerObservationsRequest {
                container_ids: vec![ContainerId::parse("a".repeat(64)).unwrap()],
                wait_millis: 0,
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();

    assert!(matches!(
        response.body,
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::Internal
    ));
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn replicated_container_observation_wait_returns_on_change_or_timeout() {
    let (replicated, server) = fake_cluster::store_with_container_changes().await;
    let initial = container_observation('a');
    replicated.publish_container(&initial).await.unwrap();
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-container-observation-wait-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(
        store,
        watch::channel(false).0,
        Some((replicated.clone(), AdminClient::new("/no/such/admin.sock"))),
    );

    let initial_id = initial.container_id;
    let waiting = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .get_container_observations(Request::new(
                    op::GetContainerObservations::into_request(GetContainerObservationsRequest {
                        container_ids: vec![initial_id],
                        wait_millis: 1_000,
                    })
                    .encode()
                    .unwrap(),
                ))
                .await
                .unwrap()
                .into_inner()
                .decode_response()
                .unwrap()
                .decode::<op::GetContainerObservations>()
                .unwrap()
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut changed = initial.clone();
    changed
        .try_update(|parts| parts.runtime = ContainerRuntimeObservation::Exited { code: 0 })
        .unwrap();
    replicated.publish_container(&changed).await.unwrap();
    let response = tokio::time::timeout(std::time::Duration::from_millis(500), waiting)
        .await
        .expect("requested observation change returns before the hold expires")
        .unwrap();
    assert_eq!(
        response
            .containers
            .get(&initial.container_id)
            .and_then(Option::as_ref),
        Some(&changed)
    );

    let started = tokio::time::Instant::now();
    let response = service
        .get_container_observations(Request::new(
            op::GetContainerObservations::into_request(GetContainerObservationsRequest {
                container_ids: vec![initial_id],
                wait_millis: 20,
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::GetContainerObservations>()
        .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(20));
    assert_eq!(
        response
            .containers
            .get(&initial.container_id)
            .and_then(Option::as_ref),
        Some(&changed)
    );

    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

fn container_observation(id: char) -> ContainerObservation {
    let machine_id = MachineId::parse("1".repeat(32)).unwrap();
    let service_id = ServiceId::parse("2".repeat(32)).unwrap();
    let service_name = ServiceName::parse("api").unwrap();
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/api", "pull_policy": "missing" }
    }))
    .unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: format!("api-{id}"),
        created_at_unix_nanos: 0,
        machine_id,
        project_name: ProjectName::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: Some(ContainerAddress([10, 210, 1, 2].into())),
        labels: BTreeMap::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn runtime_watch_without_a_cluster_store_is_unavailable() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-runtime-watch-{}",
        ployz_core::MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let (restart, _) = watch::channel(false);
    let service = MachineService::with_cluster(store, restart, None);
    let error = service
        .runtime_watch(Request::new(
            op::RuntimeWatch::into_request(RuntimeWatchRequest {})
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    let _ = std::fs::remove_dir_all(data_dir);
}
