//! Tests for the Machine RPC boundary.

use super::{MachineService, hosted_dns_error, ingress_config_missing, store_error};
use crate::corrosion::{AdminClient, fake_cluster};
use crate::machine::{LocalMachineStore, StoreError};
use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, GET_CONTAINER_OBSERVATIONS_CAPABILITY,
    GetContainerObservationsRequest, HealthObservation, MachineId, MachineRelease, MachineRpc,
    MachineUpgradeAttemptId, ProjectName, RequestMachineUpgradeRequest, ResetRequest,
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
fn out_of_order_machine_transitions_are_conflicts() {
    for error in [
        StoreError::NotParticipating,
        StoreError::NotResetting,
        StoreError::NotJoining,
        StoreError::AlreadyRunning("/var/lib/ployz".into()),
    ] {
        assert_eq!(store_error(error).code, RpcErrorCode::Conflict);
    }
}

#[test]
fn hosted_dns_codes_follow_who_caused_the_failure() {
    use crate::hosted_dns::Error;
    for (error, code) in [
        (
            Error::InvalidEndpoint("bad".into()),
            RpcErrorCode::InvalidArgument,
        ),
        (Error::AlreadyReserved, RpcErrorCode::Conflict),
        (Error::NotFound, RpcErrorCode::NotFound),
        (Error::Authentication, RpcErrorCode::Unauthenticated),
        (Error::AuthNoDomain, RpcErrorCode::Unauthenticated),
        (
            Error::InvalidReservation("invalid DNS hostname"),
            RpcErrorCode::Unavailable,
        ),
        (
            Error::Json(serde_json::from_str::<()>("nope").unwrap_err()),
            RpcErrorCode::Unavailable,
        ),
        (Error::InvalidReservationCleared, RpcErrorCode::Internal),
        (
            Error::Status(400, "bad record".into()),
            RpcErrorCode::Unavailable,
        ),
        (
            Error::Status(403, "forbidden".into()),
            RpcErrorCode::Unauthenticated,
        ),
        (
            Error::Status(404, "no such domain".into()),
            RpcErrorCode::NotFound,
        ),
        (Error::Status(409, "taken".into()), RpcErrorCode::Conflict),
        (
            Error::Status(429, "slow down".into()),
            RpcErrorCode::Unavailable,
        ),
        (
            Error::Status(502, "bad gateway".into()),
            RpcErrorCode::Unavailable,
        ),
    ] {
        assert_eq!(hosted_dns_error(error).code, code);
    }
    let error = hosted_dns_error(Error::Status(502, "bad gateway".into()));
    assert_eq!(error.details.get("status"), Some(&serde_json::json!(502)));
}

#[test]
fn missing_ingress_config_names_its_path_in_details() {
    let path = "/data/ingress/caddy.json";
    let error = ingress_config_missing(std::path::Path::new(path));
    assert_eq!(error.code, RpcErrorCode::NotFound);
    assert_eq!(error.details.get("path"), Some(&serde_json::json!(path)));
}

#[tokio::test]
async fn upgrade_and_machine_mutations_refuse_each_other_at_the_rpc_boundary() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-upgrade-rpc-admission-{}",
        MachineId::random()
    ));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(store, watch::channel(false).0, None);
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mutation = tokio::spawn({
        let local = service.local();
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        async move { local.hold_mutation_for_test(started, release).await }
    });
    started.notified().await;

    let response = service
        .request_machine_upgrade(Request::new(
            op::RequestMachineUpgrade::into_request(RequestMachineUpgradeRequest {
                attempt_id: MachineUpgradeAttemptId::parse("a".repeat(32)).unwrap(),
                release: MachineRelease::parse("1.2.3").unwrap(),
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
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::Conflict
    ));
    release.notify_one();
    mutation.await.unwrap().unwrap();

    std::fs::write(data_dir.join(".upgrade-active"), "attempt").unwrap();
    let response = service
        .reset(Request::new(
            op::Reset::into_request(ResetRequest {}).encode().unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    assert!(matches!(
        response.body,
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::Conflict
    ));

    drop(service);
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn upgrade_rpc_rejects_nonstandard_machine_paths_before_acceptance() {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-upgrade-rpc-paths-{}", MachineId::random()));
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
    let service = MachineService::with_cluster(store, watch::channel(false).0, None);

    let response = service
        .request_machine_upgrade(Request::new(
            op::RequestMachineUpgrade::into_request(RequestMachineUpgradeRequest {
                attempt_id: MachineUpgradeAttemptId::parse("b".repeat(32)).unwrap(),
                release: MachineRelease::parse("1.2.3").unwrap(),
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
        RpcResponseBody::Error(error)
            if error.code == RpcErrorCode::InvalidArgument
                && error.message.contains("system installation requires --data-dir")
    ));
    assert!(!data_dir.join("upgrade-attempt.json").exists());
    assert!(!data_dir.join(".upgrade-active").exists());

    drop(service);
    std::fs::remove_dir_all(data_dir).unwrap();
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
        .err()
        .expect("Watch without a Cluster store must fail");
    assert_eq!(error.code(), Code::Unavailable);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn keyed_creation_replays_conflicts_and_obeys_new_work_admission() {
    use crate::docker::test_support::{FakeDocker, fake_runtime_with};
    use ployz_core::{CreateContainerRequest, InitializeRequest, MachineName};
    use serde_json::json;
    let data_dir = std::env::temp_dir().join(format!("ployzd-keyed-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .initialize(InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("local").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                "192.0.2.1:51820".parse().unwrap(),
            )],
            wireguard_mtu: None,
            cloud_pairing: None,
        })
        .unwrap();
    let store = Arc::new(Mutex::new(store));
    let containers = Arc::new(Mutex::new(BTreeMap::new()));
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(containers.clone()),
        ..Default::default()
    })
    .await;
    let service = MachineService::with_cluster(store.clone(), watch::channel(false).0, None)
        .with_optional_containers(Some(runtime));
    let request = CreateContainerRequest {
        creation_key: Some("retry/1".into()),
        kind: ContainerKind::ServiceContainer,
        project_name: ProjectName::parse("app").unwrap(),
        resolved_spec: serde_json::from_value(json!({
            "service_id": ServiceId::parse("a".repeat(32)).unwrap(), "name":"api",
            "mode":{"mode":"replicated", "replicas":1},
            "container":{"image":"example.test/api", "pull_policy":"missing"},
            "pre_deploy":{"command":["true"]}
        }))
        .unwrap(),
    };
    async fn create(
        service: &MachineService,
        request: CreateContainerRequest,
    ) -> Result<ployz_core::ContainerCreated, ployz_core::RpcError> {
        let response = service
            .create_container(Request::new(
                op::CreateContainer::into_request(request).encode().unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        if let RpcResponseBody::Error(error) = response.body {
            return Err(error);
        }
        Ok(response.decode::<op::CreateContainer>().unwrap())
    }
    let (first, simultaneous) = tokio::join!(
        create(&service, request.clone()),
        create(&service, request.clone())
    );
    let first = first.unwrap();
    assert_eq!(first, simultaneous.unwrap());
    assert_eq!(first, create(&service, request.clone()).await.unwrap());
    assert_eq!(containers.lock().unwrap().len(), 1);
    let original = containers.lock().unwrap().clone();
    let mut mismatch = request.clone();
    mismatch.resolved_spec.mode = ployz_core::ServiceMode::Replicated {
        replicas: 2.try_into().unwrap(),
    };
    assert_eq!(
        mismatch.resolved_spec.serving_shape(),
        request.resolved_spec.serving_shape()
    );
    assert_eq!(
        create(&service, mismatch).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    let mut mismatch = request.clone();
    mismatch.resolved_spec.service_id =
        ServiceId::parse(format!("{}{}", "a".repeat(8), "b".repeat(24))).unwrap();
    assert_eq!(
        create(&service, mismatch).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    assert_eq!(*containers.lock().unwrap(), original);
    let mut other_project = request.clone();
    other_project.project_name = ProjectName::parse("other").unwrap();
    let mut hook = request.clone();
    hook.kind = ContainerKind::PreDeployHook;
    let mut unkeyed = request.clone();
    unkeyed.creation_key = None;
    for independent in [other_project, hook, unkeyed.clone(), unkeyed] {
        let count = containers.lock().unwrap().len();
        assert_ne!(
            first.container_id,
            create(&service, independent).await.unwrap().container_id
        );
        assert_eq!(containers.lock().unwrap().len(), count + 1);
    }
    let mut contender = request.clone();
    contender.creation_key = Some("contended".into());
    let mut incompatible = contender.clone();
    incompatible.resolved_spec.container.image = "example.test/other".into();
    let (left, right) = tokio::join!(
        create(&service, contender.clone()),
        create(&service, incompatible.clone())
    );
    let (winner, loser, retry) = match (left, right) {
        (Ok(winner), Err(loser)) => (winner, loser, contender),
        (Err(loser), Ok(winner)) => (winner, loser, incompatible),
        other => panic!("one conflicting creation must win: {other:?}"),
    };
    assert_eq!(loser.code, RpcErrorCode::Conflict);
    assert_eq!(winner, create(&service, retry).await.unwrap());
    store
        .lock()
        .unwrap()
        .update(
            serde_json::from_value(json!({"accepts_services":false})).unwrap(),
            &[],
        )
        .unwrap();
    assert_eq!(first, create(&service, request.clone()).await.unwrap());
    let mut new_request = request.clone();
    new_request.creation_key = Some("new".into());
    assert_eq!(
        create(&service, new_request).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    service
        .remove_container(Request::new(
            op::RemoveContainer::into_request(ployz_core::RemoveContainerRequest {
                container_id: first.container_id,
                remove_volumes: false,
                force: false,
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::RemoveContainer>()
        .unwrap();
    assert_eq!(
        create(&service, request.clone()).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    store
        .lock()
        .unwrap()
        .update(
            serde_json::from_value(json!({"accepts_services":true})).unwrap(),
            &[],
        )
        .unwrap();
    assert_ne!(
        first.container_id,
        create(&service, request.clone())
            .await
            .unwrap()
            .container_id
    );
    let mut unsafe_request = request.clone();
    unsafe_request.creation_key = Some("unsafe".into());
    unsafe_request.resolved_spec.placement = ployz_core::Placement {
        constraints: ["node.labels.target==other".parse().unwrap()].into(),
    };
    assert_eq!(
        create(&service, unsafe_request).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    for source in [
        crate::docker::test_support::ordinary_source("unsafe"),
        crate::docker::test_support::provisioned_source("bounded", 1_073_741_824),
    ] {
        let mut unsafe_request = request.clone();
        unsafe_request.creation_key = Some("unsafe-storage".into());
        unsafe_request.resolved_spec = crate::docker::test_support::spec_with_sources(vec![source]);
        fake.volumes.lock().unwrap().insert(
            "app_unsafe".into(),
            json!({"Name":"app_unsafe", "Driver":"local", "Mountpoint":"/volumes/app_unsafe"}),
        );
        fake.volumes.lock().unwrap().insert(
            "app_bounded".into(),
            json!({"Name":"app_bounded", "Driver":"local", "Mountpoint":"/volumes/app_bounded"}),
        );
        let error = create(&service, unsafe_request).await.unwrap_err();
        assert!(
            matches!(
                error.code,
                RpcErrorCode::Conflict | RpcErrorCode::Unavailable | RpcErrorCode::Unsupported
            ),
            "{error}"
        );
    }
    assert!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, path)| !path.ends_with("/start") && !path.ends_with("/stop"))
    );
    std::fs::remove_dir_all(data_dir).unwrap();
}
