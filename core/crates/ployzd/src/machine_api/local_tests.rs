//! Tests for the Machine RPC boundary.

use super::{MachineService, store_error};
use crate::machine::{LocalMachineStore, RecordOwner, StoreError};
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation, ForgetRequest,
    MachineId, MachineRelease, MachineRpc, MachineUpgradeAttemptId, Namespace,
    RequestMachineUpgradeRequest, ResolvedServiceSpec, RpcErrorCode, RpcResponseBody,
    RuntimeWatchRequest, ServiceId, op,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
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

#[tokio::test]
async fn image_ingest_answers_while_admitted_work_runs() {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-ingest-admission-{}", MachineId::random()));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let local = MachineService::with_cluster(store, None).local();
    let lock = local.owner().admission_lock();
    // A running image transfer, and exclusive work queued behind it (a Deploy's
    // container change) that would hold back any new admission.
    let _transfer = Arc::clone(&lock).read_owned().await;
    let queued = Arc::clone(&lock).write_owned();
    tokio::pin!(queued);
    assert!(futures_util::poll!(&mut queued).is_pending());

    // Ingest still answers (this Machine is not participating, so it refuses).
    let opened = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        local.ensure_image_ingest(crate::docker::ImageIngest::new(None, None)),
    )
    .await
    .expect("image ingest waited for its turn in Machine admission");
    assert!(opened.is_err());
    std::fs::remove_dir_all(data_dir).ok();
}

#[tokio::test]
async fn upgrade_and_machine_mutations_refuse_each_other_at_the_rpc_boundary() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-upgrade-rpc-admission-{}",
        MachineId::random()
    ));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);
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
        .forget(Request::new(
            op::Forget::into_request(ForgetRequest {
                machine_id: MachineId::random(),
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

    drop(service);
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn upgrade_rpc_rejects_nonstandard_machine_paths_before_acceptance() {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-upgrade-rpc-paths-{}", MachineId::random()));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);

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

async fn create_service_container(
    service: &MachineService,
    spec: ResolvedServiceSpec,
) -> crate::docker::ContainerCreated {
    service
        .local
        .create_container(
            ContainerKind::ServiceContainer,
            &Namespace::parse("app").unwrap(),
            &spec,
            None,
            None,
            None,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn runtime_watch_without_a_cluster_store_is_unavailable() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-runtime-watch-{}",
        ployz_core::MachineId::random()
    ));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);
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
async fn tail_logs_reads_each_target_from_its_live_source_without_the_observer() {
    use futures_util::StreamExt;
    use ployz_core::{LiveLogTarget, LogsOptions, MachineLogService, TailLogsRequest};
    let tail = |target| {
        Request::new(
            op::TailLogs::into_request(TailLogsRequest {
                target,
                options: LogsOptions {
                    follow: false,
                    tail: 10,
                    since_nanos: None,
                    until_unix_seconds: None,
                },
            })
            .encode()
            .unwrap(),
        )
    };
    let missing = ContainerId::parse("d".repeat(64)).unwrap();

    let outside_dir =
        std::env::temp_dir().join(format!("ployzd-tail-outside-{}", MachineId::random()));
    let outside = MachineService::with_cluster(
        RecordOwner::spawn(LocalMachineStore::open(&outside_dir).unwrap()).unwrap(),
        None,
    );
    for target in [
        LiveLogTarget::Container(missing),
        LiveLogTarget::Machine(MachineLogService::Ployz),
    ] {
        let error = outside
            .tail_logs(tail(target))
            .await
            .expect_err("a Machine outside the cluster has no live logs");
        assert_eq!(error.code(), Code::Unavailable);
        assert_eq!(error.message(), "Machine is not participating");
    }

    let (data_dir, _store, service, fake) = fake_docker_service("ployzd-tail-logs").await;
    let service = service.with_observe_socket(data_dir.join("no-observer.sock"));
    let error = service
        .tail_logs(tail(LiveLogTarget::Container(missing)))
        .await
        .expect_err("an unknown Container has no live logs");
    assert_eq!(error.code(), Code::NotFound);
    let mut corrosion = service
        .tail_logs(tail(LiveLogTarget::Machine(MachineLogService::Corrosion)))
        .await
        .unwrap()
        .into_inner();
    tokio::time::timeout(std::time::Duration::from_secs(5), corrosion.next())
        .await
        .expect("Corrosion logs answer from Docker");
    let paths: Vec<String> = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, path)| path.clone())
        .collect();
    assert!(
        paths
            .iter()
            .any(|path| path.ends_with(&format!("/containers/{missing}/json"))),
        "{paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.ends_with(&format!("/containers/{missing}/logs"))),
        "{paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.ends_with(&format!(
            "/containers/{}/logs",
            crate::corrosion::DEFAULT_CONTAINER_NAME
        ))),
        "{paths:?}"
    );
    let _ = std::fs::remove_dir_all(outside_dir);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn keyed_creation_replays_conflicts_and_obeys_new_work_admission() {
    use serde_json::json;
    #[derive(Clone)]
    struct Creation {
        creation_key: Option<String>,
        kind: ContainerKind,
        namespace: Namespace,
        resolved_spec: ployz_core::ResolvedServiceSpec,
    }
    let (data_dir, store, service, fake) = fake_docker_service("ployzd-keyed").await;
    let containers = fake.named_containers.clone().unwrap();
    let request = Creation {
        creation_key: Some("retry/1".into()),
        kind: ContainerKind::ServiceContainer,
        namespace: Namespace::parse("app").unwrap(),
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
        request: Creation,
    ) -> Result<crate::docker::ContainerCreated, ployz_core::RpcError> {
        let created = service
            .local
            .create_container(
                request.kind,
                &request.namespace,
                &request.resolved_spec,
                request.creation_key,
                None,
                None,
            )
            .await;
        created.map_err(|error| {
            let response = super::local_error(error)
                .unwrap()
                .into_inner()
                .decode_response()
                .unwrap();
            let RpcResponseBody::Error(error) = response.body else {
                panic!("a failed creation must answer an error");
            };
            error
        })
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
    let mut other_namespace = request.clone();
    other_namespace.namespace = Namespace::parse("other").unwrap();
    let mut hook = request.clone();
    hook.kind = ContainerKind::PreDeployHook;
    let mut unkeyed = request.clone();
    unkeyed.creation_key = None;
    for independent in [other_namespace, hook, unkeyed.clone(), unkeyed] {
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
        .mutate(|store| {
            store.update(
                serde_json::from_value(json!({"accepts_services":false})).unwrap(),
                &[],
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, create(&service, request.clone()).await.unwrap());
    let mut new_request = request.clone();
    new_request.creation_key = Some("new".into());
    assert_eq!(
        create(&service, new_request).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    service
        .local
        .remove_container(first.container_id, false, false)
        .await
        .unwrap();
    assert_eq!(
        create(&service, request.clone()).await.unwrap_err().code,
        RpcErrorCode::Conflict
    );
    store
        .mutate(|store| {
            store.update(
                serde_json::from_value(json!({"accepts_services":true})).unwrap(),
                &[],
            )
        })
        .await
        .unwrap()
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

#[tokio::test]
async fn volume_copy_inspect_refuses_a_machine_outside_the_cluster() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-switch-gate-{}",
        ployz_core::MachineId::random()
    ));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);
    let name: ployz_core::DockerVolumeName = "data".parse().unwrap();
    let inspect =
        op::InspectVolumeCopy::into_request(ployz_core::InspectVolumeCopyRequest { name });
    let response = service
        .inspect_volume_copy(Request::new(inspect.encode().unwrap()))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    let ployz_core::RpcResponseBody::Error(error) = response.body else {
        panic!("InspectVolumeCopy on an uninitialized Machine must refuse: {response:?}");
    };
    assert_eq!(error.code, ployz_core::RpcErrorCode::Unavailable);
    assert_eq!(error.message, "Machine is not participating");
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn image_pruning_without_docker_reports_an_error() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-prune-images-{}",
        ployz_core::MachineId::random()
    ));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);
    let response = service
        .prune_images(Request::new(
            op::PruneImages::into_request(ployz_core::PruneImagesRequest {
                repositories: vec!["ployz-build/web".into()],
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    let ployz_core::RpcResponseBody::Error(error) = response.body else {
        panic!("pruning without Docker must fail: {response:?}");
    };
    assert_eq!(error.code, ployz_core::RpcErrorCode::Unavailable);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn inspecting_an_unknown_build_grant_is_not_found() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-build-grant-{}",
        ployz_core::MachineId::random()
    ));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let service = MachineService::with_cluster(store, None);
    let code = |body: RpcResponseBody| {
        let RpcResponseBody::Error(error) = body else {
            panic!("expected a refusal: {body:?}");
        };
        error.code
    };
    let response = service
        .inspect_build_grant(Request::new(
            op::InspectBuildGrant::into_request(ployz_core::InspectBuildGrantRequest {
                id: ployz_core::BuildGrantId::parse("a".repeat(64)).unwrap(),
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    assert_eq!(code(response.body), RpcErrorCode::NotFound);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn listed_containers_redact_values_unless_requested() {
    use ployz_core::{ConfigSpec, EnvironmentValues, ListContainersRequest};
    use serde_json::json;
    use std::os::unix::fs::MetadataExt;
    let (data_dir, _store, service, fake) = fake_docker_service("ployzd-list-env").await;
    let owner = std::fs::metadata(&data_dir).unwrap();
    let created = create_service_container(&service, serde_json::from_value(json!({
                    "service_id": ServiceId::parse("a".repeat(32)).unwrap(), "name":"api",
                    "mode":{"mode":"replicated", "replicas":1},
                    "container":{
                        "image":"example.test/api", "pull_policy":"missing",
                        "environment":{"CADDY_ADMIN":"localhost:2019", "TOKEN":"secret"},
                        "config_mounts":[{
                            "config_name":"settings", "target":"/etc/api/settings",
                            "uid":owner.uid(), "gid":owner.gid()
                        }]
                    },
                    "pre_deploy":{"command":["migrate"], "environment":{"DATABASE_URL":"postgres://secret"}},
                    "configs":[{"name":"settings", "content":b"token=config-secret".to_vec()}]
                }))
                .unwrap()).await;
    *fake.existing_container.lock().unwrap() = Some(json!({"Id": created.container_id}));
    let list = |request: ListContainersRequest| {
        let service = &service;
        async move {
            let listed = service
                .list_containers(Request::new(
                    op::ListContainers::into_request(request).encode().unwrap(),
                ))
                .await
                .unwrap()
                .into_inner()
                .decode_response()
                .unwrap()
                .decode::<op::ListContainers>()
                .unwrap();
            let [observed] = listed.containers.try_into().unwrap();
            let spec = observed.resolved_spec.clone();
            let configs = spec.mount_graph.config_graph();
            let mounted = configs
                .mounts()
                .iter()
                .map(|mount| {
                    (
                        mount.config_name.clone(),
                        mount.target.clone().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                mounted,
                [("settings".to_owned(), "/etc/api/settings".to_owned())]
            );
            (
                spec.container.environment.clone(),
                spec.pre_deploy.clone().unwrap().environment,
                configs.configs().to_vec(),
            )
        }
    };

    assert_eq!(
        list(ListContainersRequest::default()).await,
        (
            BTreeMap::from([
                ("CADDY_ADMIN".into(), "<redacted>".into()),
                ("TOKEN".into(), "<redacted>".into()),
            ]),
            BTreeMap::from([("DATABASE_URL".into(), "<redacted>".into())]),
            vec![ConfigSpec {
                name: "settings".into(),
                content: Vec::new(),
            }],
        )
    );
    assert_eq!(
        list(ListContainersRequest {
            environment: EnvironmentValues::Included,
        })
        .await,
        (
            BTreeMap::from([
                ("CADDY_ADMIN".into(), "localhost:2019".into()),
                ("TOKEN".into(), "secret".into()),
            ]),
            BTreeMap::from([("DATABASE_URL".into(), "postgres://secret".into())]),
            vec![ConfigSpec {
                name: "settings".into(),
                content: b"token=config-secret".to_vec(),
            }],
        )
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A Machine service on an initialized store, backed by a fake Docker that keeps named Containers.
async fn fake_docker_service(
    prefix: &str,
) -> (
    std::path::PathBuf,
    RecordOwner,
    MachineService,
    crate::docker::test_support::FakeDocker,
) {
    use crate::docker::test_support::{FakeDocker, fake_runtime_with};
    use ployz_core::MachineName;
    let data_dir = std::env::temp_dir().join(format!("{prefix}-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
            },
            wireguard_mtu: None,
        })
        .unwrap();
    let store = RecordOwner::spawn(store).unwrap();
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        ..Default::default()
    })
    .await;
    let mut service =
        MachineService::with_cluster(store.clone(), None).with_optional_containers(Some(runtime));
    // `true` lists no pools, which reads as ZFS ready whatever the host has.
    service.local = service.local.with_zpool("true");
    (data_dir, store, service, fake)
}

mod set_volume_copy {
    //! The daemon half of SetVolumeCopy against a fake Docker and a fake plugin: the
    //! Container side of the cut and of a Writer start, and the refusals that stop
    //! before any ZFS effect.

    use super::*;
    use crate::{
        docker::test_support::{FakeDocker, provisioned_source, spec_with_sources},
        storage::test_support::FakePlugin,
        volume_copy::{CUT_ROUTE, INSPECT_ROUTE, WRITER_ROUTE},
    };
    use ployz_core::{RpcError, RpcResponse, SetVolumeCopyReply};
    use serde_json::{Value, json};

    const VOLUME: &str = "app_data";

    fn writer_root(epoch: u32, newest: Option<Value>) -> Value {
        json!({"Ok": {"kind":"root", "readonly":false,
            "newest":newest, "epoch":epoch, "seed":false}})
    }

    fn slot(newest: Option<Value>) -> Value {
        json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
            "newest":newest, "resume_token":null, "pull":null}})
    }

    fn snapshot(name: &str, guid: u64) -> Value {
        json!({"name":name, "guid":guid.to_string(), "created_unix_seconds":1_700_000_000})
    }

    fn spec() -> Value {
        serde_json::to_value(spec_with_sources(vec![provisioned_source(
            "data",
            1_073_741_824,
        )]))
        .unwrap()
    }

    async fn set_copy(service: &MachineService, request: Value) -> RpcResponse {
        service
            .set_volume_copy(Request::new(
                op::SetVolumeCopy::into_request(serde_json::from_value(request).unwrap())
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
    }

    fn reply(response: &RpcResponse) -> SetVolumeCopyReply {
        response
            .clone()
            .decode::<op::SetVolumeCopy>()
            .unwrap_or_else(|error| panic!("{error}: {response:?}"))
    }

    fn refusal(response: &RpcResponse) -> RpcError {
        let RpcResponseBody::Error(error) = &response.body else {
            panic!("expected a refusal, got {response:?}");
        };
        error.clone()
    }

    /// The lines the cut logged once every holder stopped, as the journal shows them.
    fn cut_lines(capture: &crate::diag::test_support::Capture) -> Vec<String> {
        capture
            .text()
            .lines()
            .filter(|line| line.contains("cut: holders stopped"))
            .map(str::to_owned)
            .collect()
    }

    fn cut(expect: Value) -> Value {
        json!({"name":VOLUME, "expect":expect,
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":0}})
    }

    /// A running Container that mounts the Volume, as Docker lists it for the cut,
    /// beside any holder already there.
    async fn running_holder(service: &MachineService, fake: &FakeDocker) -> ContainerId {
        let created = stopped_container(service).await;
        let id = created.to_string();
        for container in fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
            .filter(|container| container["Id"] == id.as_str())
        {
            container["State"] = json!({"Status":"running", "Running":true});
        }
        fake.volume_users
            .lock()
            .unwrap()
            .entry(VOLUME.into())
            .or_default()
            .push(json!({"Id": id}));
        created
    }

    async fn stopped_container(service: &MachineService) -> ContainerId {
        create_service_container(service, serde_json::from_value(spec()).unwrap())
            .await
            .container_id
    }

    fn running(fake: &FakeDocker) -> usize {
        holder_status(fake)
            .iter()
            .filter(|status| **status == json!("running"))
            .count()
    }

    /// A root at epoch 1 that the Writer route raises to epoch 2. With `stop`, every
    /// running holder exits while the route runs, and the `gone` Container is removed.
    fn raising_plugin(stop: Option<&FakeDocker>, gone: Option<ContainerId>) -> FakePlugin {
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, Some(snapshot("f-1", 7))));
        plugin.reply(WRITER_ROUTE, writer_root(2, Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        let holders = stop.and_then(|fake| fake.named_containers.clone());
        plugin.on(WRITER_ROUTE, move || {
            replies.lock().unwrap().insert(
                INSPECT_ROUTE.into(),
                writer_root(2, Some(snapshot("f-1", 7))),
            );
            if let Some(holders) = &holders {
                let mut holders = holders.lock().unwrap();
                for container in holders.values_mut() {
                    if container.pointer("/State/Running") == Some(&json!(true)) {
                        container["State"] = json!({"Status":"exited", "Running":false});
                    }
                }
                if let Some(gone) = gone {
                    holders.retain(|_, container| container["Id"] != gone.to_string().as_str());
                }
            }
        });
        plugin
    }

    #[tokio::test]
    async fn a_raise_whose_kept_start_another_holder_superseded_starts_nothing() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-kept-held").await;
        let plugin = raising_plugin(None, None);
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let kept = stopped_container(&service).await;
        running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2,
                "start":{"kind":"kept", "container_id":kept.to_string()}}});

        let raised = reply(&set_copy(&service, raise).await);

        assert_eq!(
            serde_json::to_value(&raised).unwrap(),
            json!({"copy": writer_root(2, Some(snapshot("f-1", 7))).get("Ok")})
        );
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, WRITER_ROUTE]);
        assert_eq!(running(&fake), 1, "{:?}", holder_status(&fake));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_raise_whose_create_start_another_holder_superseded_creates_nothing() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-create-held").await;
        let plugin = raising_plugin(None, None);
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec(),
                "creation_key":"move-1"}}});

        let raised = reply(&set_copy(&service, raise).await);

        assert_eq!(
            serde_json::to_value(&raised).unwrap(),
            json!({"copy": writer_root(2, Some(snapshot("f-1", 7))).get("Ok")})
        );
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, WRITER_ROUTE]);
        let creates = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| path.ends_with("/containers/create"))
            .count();
        assert_eq!(creates, 1, "the raise created the superseded Container");
        assert_eq!(running(&fake), 1, "{:?}", holder_status(&fake));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_raise_whose_holder_exited_during_the_raise_starts_the_kept_container() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-kept-exited").await;
        let plugin = raising_plugin(Some(&fake), None);
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let kept = stopped_container(&service).await;
        running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2,
                "start":{"kind":"kept", "container_id":kept.to_string()}}});

        reply(&set_copy(&service, raise).await);

        let kept_status = fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values()
            .find(|container| container["Id"] == kept.to_string().as_str())
            .and_then(|container| container.pointer("/State/Status").cloned());
        assert_eq!(
            kept_status,
            Some(json!("running")),
            "{:?}",
            holder_status(&fake)
        );
        assert_eq!(running(&fake), 1, "{:?}", holder_status(&fake));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_raise_whose_kept_container_went_during_the_raise_refuses_no_transition() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-kept-gone").await;
        let kept = stopped_container(&service).await;
        let plugin = raising_plugin(Some(&fake), Some(kept));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2,
                "start":{"kind":"kept", "container_id":kept.to_string()}}});

        let error = refusal(&set_copy(&service, raise).await);

        assert_eq!(error.details["reason"], json!("no_transition"), "{error:?}");
        assert_eq!(
            error.details["copy"],
            writer_root(2, Some(snapshot("f-1", 7)))["Ok"]
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_raise_whose_holder_exited_during_the_raise_creates_and_starts_its_container() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-create-exited").await;
        let plugin = raising_plugin(Some(&fake), None);
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec(),
                "creation_key":"move-1"}}});

        reply(&set_copy(&service, raise).await);

        let creates = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| path.ends_with("/containers/create"))
            .count();
        assert_eq!(creates, 2, "the raise created no Container");
        assert_eq!(running(&fake), 1, "{:?}", holder_status(&fake));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    fn holder_status(fake: &FakeDocker) -> Vec<Value> {
        fake.named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values()
            .map(|container| {
                container
                    .pointer("/State/Status")
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn the_cut_stops_the_holder_and_keeps_it_once_the_root_is_parked() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-cut").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        plugin.reply(CUT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        plugin.on(CUT_ROUTE, move || {
            replies
                .lock()
                .unwrap()
                .insert(INSPECT_ROUTE.into(), slot(Some(snapshot("f-1", 7))));
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let (capture, _logging) = crate::diag::test_support::capture();
        let first = reply(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(holder_status(&fake), vec![json!("exited")]);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, CUT_ROUTE, INSPECT_ROUTE]
        );

        let again = reply(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
        assert_eq!(again, first);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, CUT_ROUTE, INSPECT_ROUTE, INSPECT_ROUTE]
        );
        assert_eq!(
            cut_lines(&capture),
            vec![" INFO cut: holders stopped volume=app_data target=f-1"]
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn an_undo_sent_after_the_cut_caller_hung_up_waits_for_the_cut_then_refuses() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-hangup").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, Some(snapshot("w-1-1", 7))));
        plugin.reply(CUT_ROUTE, slot(Some(snapshot("f-1", 8))));
        plugin.reply(WRITER_ROUTE, writer_root(1, Some(snapshot("w-1-1", 7))));
        let replies = plugin.replies.clone();
        plugin.on(CUT_ROUTE, move || {
            replies
                .lock()
                .unwrap()
                .insert(INSPECT_ROUTE.into(), slot(Some(snapshot("f-1", 8))));
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let draining = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":400}});

        let hung_up = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            set_copy(&service, draining),
        )
        .await;
        assert!(
            hung_up.is_err(),
            "the cut finished inside its drain: {hung_up:?}"
        );
        let undo = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"writer", "at":"7", "epoch":1,
                "start":{"kind":"kept", "container_id":holder.to_string()}}});
        let error = refusal(&set_copy(&service, undo).await);

        assert_eq!(error.details["reason"], json!("mismatch"), "{error:?}");
        assert_eq!(error.details["copy"], slot(Some(snapshot("f-1", 8)))["Ok"]);
        assert_eq!(holder_status(&fake), vec![json!("exited")]);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, CUT_ROUTE, INSPECT_ROUTE, INSPECT_ROUTE]
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_holder_started_during_the_drain_refuses_busy_until_the_resend() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-late").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        plugin.reply(CUT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        plugin.on(CUT_ROUTE, move || {
            replies
                .lock()
                .unwrap()
                .insert(INSPECT_ROUTE.into(), slot(Some(snapshot("f-1", 7))));
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;
        let draining = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":300}});

        let (refused, _) = tokio::join!(set_copy(&service, draining.clone()), async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            running_holder(&service, &fake).await
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("busy"), "{error:?}");
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, INSPECT_ROUTE]);
        let mut status = holder_status(&fake);
        status.sort_by_key(ToString::to_string);
        assert_eq!(status, vec![json!("exited"), json!("running")]);

        let cut = reply(&set_copy(&service, draining).await);
        assert_eq!(
            serde_json::to_value(&cut).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(holder_status(&fake), vec![json!("exited"), json!("exited")]);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_cut_whose_stop_fails_leaves_the_writer_untouched() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-stop").await;
        fake.refuse_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let (capture, _logging) = crate::diag::test_support::capture();
        let error = refusal(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
        assert_eq!(error.code, RpcErrorCode::Internal, "{error:?}");
        assert!(
            error
                .cause
                .iter()
                .any(|cause| cause.contains("cannot stop container")),
            "{error:?}"
        );
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE]);
        assert_eq!(cut_lines(&capture), Vec::<String>::new());
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_holder_that_restarts_before_the_zfs_turn_refuses_busy() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-busy").await;
        fake.restart_after_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let (capture, _logging) = crate::diag::test_support::capture();
        let error = refusal(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert_eq!(error.details["reason"], json!("busy"));
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, INSPECT_ROUTE]);
        assert_eq!(cut_lines(&capture), Vec::<String>::new());
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_mismatched_expect_refuses_before_any_effect() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-mismatch").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let error = refusal(&set_copy(&service, cut(json!({"kind":"writer", "epoch":1}))).await);
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert_eq!(error.details["reason"], json!("mismatch"));
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE]);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_raised_epoch_refuses_a_cut_that_expected_the_old_one() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-epoch").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, Some(snapshot("f-1", 7))));
        plugin.reply(WRITER_ROUTE, writer_root(2, Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        plugin.on(WRITER_ROUTE, move || {
            replies.lock().unwrap().insert(
                INSPECT_ROUTE.into(),
                writer_root(2, Some(snapshot("f-1", 7))),
            );
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let raise = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":2,
                "start":{"kind":"kept", "container_id":holder.to_string()}}});

        let raised = reply(&set_copy(&service, raise.clone()).await);
        assert_eq!(
            serde_json::to_value(&raised).unwrap(),
            json!({"copy": writer_root(2, Some(snapshot("f-1", 7))).get("Ok")})
        );
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, WRITER_ROUTE]);
        assert_eq!(reply(&set_copy(&service, raise).await), raised);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, WRITER_ROUTE, INSPECT_ROUTE]
        );

        let error = refusal(&set_copy(&service, cut(json!({"kind":"writer", "epoch":1}))).await);
        assert_eq!(error.details["reason"], json!("mismatch"), "{error:?}");
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, WRITER_ROUTE, INSPECT_ROUTE, INSPECT_ROUTE]
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_late_writer_create_after_the_service_moved_on_starts_nothing() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-moved").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 900))));
        plugin.reply(WRITER_ROUTE, writer_root(1, Some(snapshot("f-1", 900))));
        let replies = plugin.replies.clone();
        plugin.on(WRITER_ROUTE, move || {
            replies.lock().unwrap().insert(
                INSPECT_ROUTE.into(),
                writer_root(1, Some(snapshot("f-1", 900))),
            );
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let step_five = json!({"name":VOLUME,
            "expect":{"kind":"mirror", "newest":"900", "slot":"1"},
            "want":{"kind":"writer", "at":"900", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec(),
                "creation_key":"move-1"}}});
        reply(&set_copy(&service, step_five.clone()).await);
        fake.named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .clear();
        running_holder(&service, &fake).await;

        let late = reply(&set_copy(&service, step_five).await);

        assert_eq!(
            serde_json::to_value(&late).unwrap(),
            json!({"copy": writer_root(1, Some(snapshot("f-1", 900)))["Ok"]})
        );
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        let creates = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| path.ends_with("/containers/create"))
            .count();
        assert_eq!(
            creates, 2,
            "the late step 5 created the old Container again"
        );
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, WRITER_ROUTE, INSPECT_ROUTE]
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_kept_start_docker_no_longer_has_refuses_no_transition() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-gone").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, Some(snapshot("f-1", 7))));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let gone = ContainerId::parse("c".repeat(64)).unwrap();
        let request = json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
            "want":{"kind":"writer", "at":"7", "epoch":1,
                "start":{"kind":"kept", "container_id":gone.to_string()}}});

        let error = refusal(&set_copy(&service, request.clone()).await);
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert_eq!(error.details["reason"], json!("no_transition"));
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE]);

        running_holder(&service, &fake).await;
        let held = reply(&set_copy(&service, request).await);
        assert_eq!(
            serde_json::to_value(&held).unwrap(),
            json!({"copy": writer_root(1, Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, INSPECT_ROUTE]);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_replayed_writer_create_finds_the_same_container_by_its_key() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-create").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 900))));
        plugin.reply(WRITER_ROUTE, writer_root(1, Some(snapshot("f-1", 900))));
        let replies = plugin.replies.clone();
        plugin.on(WRITER_ROUTE, move || {
            replies.lock().unwrap().insert(
                INSPECT_ROUTE.into(),
                writer_root(1, Some(snapshot("f-1", 900))),
            );
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let request = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"900", "slot":"1"},
            "want":{"kind":"writer", "at":"900", "epoch":1, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec(),
                "creation_key":"move-1"}}});

        let first = reply(&set_copy(&service, request.clone()).await);
        let again = reply(&set_copy(&service, request).await);
        assert_eq!(first, again);
        assert_eq!(
            plugin.routes_called(),
            vec![INSPECT_ROUTE, WRITER_ROUTE, INSPECT_ROUTE]
        );
        let containers = fake.named_containers.as_ref().unwrap().lock().unwrap();
        assert_eq!(containers.len(), 1, "{:?}", containers.keys());
        let container = containers.values().next().unwrap();
        assert_eq!(
            container.pointer("/Config/Labels/ployz.creation.key"),
            Some(&json!("move-1"))
        );
        assert_eq!(container["State"]["Status"], json!("running"));
        drop(containers);
        let creates = fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, path)| path.ends_with("/containers/create"))
            .count();
        assert_eq!(creates, 1);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// The fault point reads `PLOYZD_FAULT` from the process environment, which a test
    /// cannot set in-process without `unsafe`, so the child re-runs this binary with it.
    #[cfg(feature = "verify-faults")]
    mod fault_lines {
        use super::*;

        const CHILD: &str =
            "machine_api::local::tests::set_volume_copy::fault_lines::a_cut_under_a_switch_delay";

        #[test]
        fn a_switch_fault_line_names_the_volume_and_the_want() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD, "--ignored", "--nocapture"])
                .env("PLOYZD_FAULT", "delay-rpc:SetVolumeCopy.WriterToMirror:0")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{CHILD}:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.contains("fault:"))
                    .collect::<Vec<_>>(),
                vec![
                    r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToMirror" secs=0 volume=app_data target=f-1"#
                ]
            );
        }

        #[tokio::test]
        #[ignore = "a child of a_switch_fault_line_names_the_volume_and_the_want"]
        async fn a_cut_under_a_switch_delay() {
            let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-copy-fault").await;
            let plugin = FakePlugin::default();
            plugin.reply(INSPECT_ROUTE, writer_root(0, None));
            plugin.reply(CUT_ROUTE, slot(Some(snapshot("f-1", 7))));
            let (socket, server) = plugin.serve(&data_dir);
            let service = service.with_volume_plugin(socket);

            let (capture, _logging) = crate::diag::test_support::capture();
            reply(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
            print!("{}", capture.text());
            assert_eq!(
                plugin.routes_called(),
                vec![INSPECT_ROUTE, CUT_ROUTE, INSPECT_ROUTE]
            );
            server.abort();
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }
}

mod ensure_container {
    use std::time::Duration;

    use axum::http::Method;
    use ployz_core::{
        ContainerNow, ContainerRefusalReason, EnsuredContainer, ExecConfig, ExecOptions,
        ExecRequestFrame, MachineRpcClient,
    };
    use serde_json::{Value, json};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::Server;

    use super::*;
    use crate::docker::test_support::{FakeDocker, ordinary_source, spec_with_sources};

    fn spec(image: &str) -> Value {
        spec_with(image, vec![])
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn spec_with(image: &str, sources: Vec<ployz_core::VolumeSource>) -> Value {
        let mut spec = serde_json::to_value(spec_with_sources(sources)).unwrap();
        spec["container"]["image"] = json!(image);
        spec
    }

    fn slot(key: &str) -> Value {
        json!({"namespace":"app", "kind":"service_container", "creation_key":key})
    }

    fn run_new(key: &str, spec: Value) -> Value {
        json!({"want":{"state":"running", "container":{"new":{"slot":slot(key), "resolved_spec":spec}}}})
    }

    async fn ensure(service: &MachineService, request: Value) -> EnsuredContainer {
        service
            .ensure_container(Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(request).unwrap())
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::EnsureContainer>()
            .unwrap()
    }

    fn present(reply: &EnsuredContainer) -> &ContainerObservation {
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            ..
        } = reply
        else {
            panic!("expected an ensured present Container, got {reply:?}");
        };
        observed
    }

    fn requests(fake: &FakeDocker) -> Vec<(Method, String)> {
        fake.requests.lock().unwrap().clone()
    }

    fn requests_since(fake: &FakeDocker, seen: usize) -> Vec<(Method, String)> {
        requests(fake).split_off(seen)
    }

    fn ends_with<'a>(
        requests: &'a [(Method, String)],
        method: Method,
        suffix: &'a str,
    ) -> impl Iterator<Item = &'a (Method, String)> + 'a {
        requests
            .iter()
            .filter(move |(seen, path)| *seen == method && path.ends_with(suffix))
    }

    fn set_state(fake: &FakeDocker, id: &ContainerId, state: Value) {
        for container in fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
        {
            if container["Id"] == id.as_str() {
                container["State"] = state.clone();
            }
        }
    }

    #[tokio::test]
    async fn a_running_want_creates_in_the_slot_once_and_resends_change_nothing() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-run").await;
        let spec = spec("example.test/api:1");

        let first = ensure(&service, run_new("k1", spec.clone())).await;
        let observed = present(&first);
        assert!(matches!(
            observed.runtime,
            ContainerRuntimeObservation::Running { .. }
        ));
        assert_eq!(
            observed
                .labels
                .get("ployz.creation.key")
                .map(String::as_str),
            Some("k1")
        );
        let seen = requests(&fake);
        assert_eq!(
            ends_with(&seen, Method::POST, "/containers/create").count(),
            1
        );
        assert_eq!(ends_with(&seen, Method::POST, "/start").count(), 1);

        let again = ensure(&service, run_new("k1", spec)).await;
        assert_eq!(again, first);
        let later = requests_since(&fake, seen.len());
        assert!(
            later.iter().all(|(method, _)| *method == Method::GET),
            "a resend only observes: {later:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_slot_holding_another_spec_refuses_and_shows_the_occupant() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-occupied").await;
        let first = ensure(&service, run_new("k1", spec("example.test/api:1"))).await;
        let occupant = present(&first).container_id;
        let seen = requests(&fake).len();

        let reply = ensure(&service, run_new("k1", spec("example.test/api:2"))).await;
        let EnsuredContainer::Refused {
            now: ContainerNow::Present(observed),
            refusal,
        } = reply
        else {
            panic!("expected a refusal naming the occupant, got {reply:?}");
        };
        assert_eq!(refusal.reason, ContainerRefusalReason::SlotOccupied);
        assert_eq!(observed.container_id, occupant);
        assert!(
            requests_since(&fake, seen)
                .iter()
                .all(|(method, _)| *method == Method::GET),
            "a refusal has no effect"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_running_want_for_an_existing_container_starts_it() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-start").await;
        let id = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        set_state(&fake, &id, json!({"Status":"exited", "ExitCode":0}));
        let seen = requests(&fake).len();

        let reply = ensure(
            &service,
            json!({"want":{"state":"running", "container":{"existing":{"id":id}}}}),
        )
        .await;
        assert!(matches!(
            present(&reply).runtime,
            ContainerRuntimeObservation::Running { .. }
        ));
        assert_eq!(
            ends_with(&requests_since(&fake, seen), Method::POST, "/start").count(),
            1
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_stopped_want_stops_a_running_container_and_holds_a_stopped_one_without_a_drain() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-stop").await;
        let id = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        let seen = requests(&fake).len();

        let stopped = json!({"want":{"state":"stopped", "container":{"id":id}, "drain":0,
            "stop":{"signal":"SIGINT"}}});
        let reply = ensure(&service, stopped).await;
        assert!(matches!(
            present(&reply).runtime,
            ContainerRuntimeObservation::Exited { .. }
        ));
        let later = requests_since(&fake, seen);
        assert_eq!(ends_with(&later, Method::POST, "/stop").count(), 1);
        let seen = requests(&fake).len();

        let slow = json!({"want":{"state":"stopped", "container":{"id":id}, "drain":30000}});
        let again = tokio::time::timeout(Duration::from_secs(5), ensure(&service, slow))
            .await
            .expect("an already stopped Container waits no drain");
        assert_eq!(again, reply);
        let held = requests_since(&fake, seen);
        assert_eq!(ends_with(&held, Method::POST, "/stop").count(), 1);
        assert!(
            held.iter()
                .all(|(method, path)| *method == Method::GET || path.ends_with("/stop"))
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_gone_want_stops_then_removes_and_a_resend_reads_absent() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-gone").await;
        let spec = spec_with("example.test/api:1", vec![ordinary_source("data")]);
        fake.volumes.lock().unwrap().insert(
            "data".into(),
            json!({"Name":"data", "Driver":"example-driver", "Mountpoint":"/volumes/data"}),
        );
        let id = present(&ensure(&service, run_new("k1", spec)).await).container_id;
        let seen = requests(&fake).len();

        let gone = json!({"want":{"state":"gone", "container":{"key":slot("k1")}, "drain":0}});
        let reply = ensure(&service, gone.clone()).await;
        assert_eq!(
            reply,
            EnsuredContainer::Ensured {
                now: ContainerNow::Absent {
                    of: serde_json::from_value(json!({"key":slot("k1")})).unwrap(),
                },
                health: None,
            }
        );
        let later = requests_since(&fake, seen);
        assert_eq!(ends_with(&later, Method::POST, "/stop").count(), 1);
        assert_eq!(
            ends_with(&later, Method::DELETE, &format!("/containers/{id}")).count(),
            1
        );
        assert!(
            !later
                .iter()
                .any(|(method, path)| *method == Method::DELETE && path.contains("/volumes/")),
            "a remove never touches a named Volume: {later:?}"
        );
        assert!(fake.volumes.lock().unwrap().contains_key("data"));
        let seen = requests(&fake).len();

        let again = ensure(&service, gone).await;
        assert_eq!(again, reply);
        assert!(
            requests_since(&fake, seen)
                .iter()
                .all(|(method, _)| *method == Method::GET),
            "a resend of a Gone only observes"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_resent_hook_want_after_the_hook_exited_runs_it_once_and_returns_its_exit() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-hook").await;
        let hook = json!({"want":{"state":"running", "container":{"new":{
            "slot":{"namespace":"app", "kind":"pre_deploy_hook", "creation_key":"h1"},
            "resolved_spec":spec("example.test/api:1")}}}});
        let id = present(&ensure(&service, hook.clone()).await).container_id;
        // The hook ran and exited, and the reply was lost.
        set_state(&fake, &id, json!({"Status":"exited", "ExitCode":3}));
        let seen = requests(&fake).len();

        let reply = ensure(&service, hook).await;
        assert_eq!(present(&reply).container_id, id);
        assert_eq!(
            present(&reply).runtime,
            ContainerRuntimeObservation::Exited { code: 3 }
        );
        let later = requests_since(&fake, seen);
        assert_eq!(
            ends_with(&later, Method::POST, "/start").count(),
            0,
            "a hook runs once: {later:?}"
        );
        assert_eq!(
            ends_with(&requests(&fake), Method::POST, "/start").count(),
            1
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn replies_redact_env_and_config_values_even_on_a_refusal() {
        use std::os::unix::fs::MetadataExt;
        let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-ensure-redact").await;
        let owner = std::fs::metadata(&data_dir).unwrap();
        let secret_spec = |image: &str| {
            json!({
                "service_id": ServiceId::parse("a".repeat(32)).unwrap(), "name":"api",
                "mode":{"mode":"replicated", "replicas":1},
                "container":{
                    "image":image, "pull_policy":"missing",
                    "environment":{"TOKEN":"secret"},
                    "config_mounts":[{"config_name":"settings", "target":"/etc/api/settings",
                        "uid":owner.uid(), "gid":owner.gid()}]
                },
                "pre_deploy":{"command":["migrate"], "environment":{"DATABASE_URL":"postgres://secret"}},
                "configs":[{"name":"settings", "content":b"token=config-secret".to_vec()}]
            })
        };
        let redacted = |observed: &ContainerObservation| {
            let spec = &observed.resolved_spec;
            assert_eq!(
                spec.container.environment.get("TOKEN").map(String::as_str),
                Some("<redacted>")
            );
            assert_eq!(
                spec.pre_deploy
                    .as_ref()
                    .unwrap()
                    .environment
                    .get("DATABASE_URL")
                    .map(String::as_str),
                Some("<redacted>")
            );
            let graph = spec.mount_graph.config_graph();
            assert!(
                graph
                    .configs()
                    .iter()
                    .all(|config| config.content.is_empty())
            );
            assert!(!serde_json::to_string(observed).unwrap().contains("secret"));
        };

        let created = ensure(&service, run_new("k1", secret_spec("example.test/api:1"))).await;
        redacted(present(&created));
        let EnsuredContainer::Refused {
            now: ContainerNow::Present(occupant),
            ..
        } = ensure(&service, run_new("k1", secret_spec("example.test/api:2"))).await
        else {
            panic!("expected a refusal naming the occupant");
        };
        redacted(&occupant);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_gone_want_by_id_for_a_container_docker_lost_deletes_its_spec() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-ensure-gone-lost").await;
        let id = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        let (name, container) = {
            let mut containers = fake.named_containers.as_ref().unwrap().lock().unwrap();
            let name = containers
                .iter()
                .find(|(_, container)| container["Id"] == id.as_str())
                .map(|(name, _)| name.clone())
                .unwrap();
            let container = containers.remove(&name).unwrap();
            (name, container)
        };

        let reply = ensure(
            &service,
            json!({"want":{"state":"gone", "container":{"id":id}, "drain":0}}),
        )
        .await;
        assert_eq!(
            reply,
            EnsuredContainer::Ensured {
                now: ContainerNow::Absent {
                    of: serde_json::from_value(json!({"id":id})).unwrap(),
                },
                health: None,
            }
        );

        // Put the Docker object back: with its spec row gone it is no longer managed.
        fake.named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .insert(name, container);
        let local = service.local();
        let inspected = local
            .containers()
            .unwrap()
            .inspect_managed(&id, &local.record().id())
            .await;
        assert!(
            matches!(inspected, Err(crate::docker::Error::SpecNotFound(lost)) if lost == id),
            "the spec row outlived Gone: {inspected:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn exec_refuses_a_container_this_machine_does_not_manage() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-exec").await;
        let stray = ContainerId::parse("b".repeat(64)).unwrap();
        fake.named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .insert(
                "stray".into(),
                json!({"Id": stray, "Name":"/stray", "Config":{}, "State":{"Status":"running"}}),
            );
        let api = crate::machine_api::MachineApi::from_local(service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)),
        );
        let mut client = MachineRpcClient::connect(format!("http://{address}"))
            .await
            .unwrap();

        for (id, code) in [
            (ContainerId::parse("a".repeat(64)).unwrap(), Code::NotFound),
            (stray, Code::InvalidArgument),
        ] {
            let config = ExecRequestFrame::Config(ExecConfig {
                container_id: id,
                options: ExecOptions {
                    command: vec!["sh".into()],
                    attach_stdin: false,
                    attach_stdout: true,
                    attach_stderr: true,
                    tty: false,
                    detach: false,
                },
            })
            .encode()
            .unwrap();
            let status = client
                .exec(Request::new(tokio_stream::iter([config])))
                .await
                .expect_err("exec must refuse before any Docker exec is created");
            assert_eq!(status.code(), code, "{id}");
        }
        assert!(
            !requests(&fake)
                .iter()
                .any(|(_, path)| path.ends_with("/exec")),
            "no exec reached Docker"
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}
