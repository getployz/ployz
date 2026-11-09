//! Tests for the Machine RPC boundary.

use super::{MachineService, store_error, withhold_opened};
use crate::machine::{LocalMachineStore, RecordOwner, StoreError};
use ployz_core::labels::CREATION_KEY;
use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation, ForgetRequest,
    MachineId, MachineRelease, MachineRpc, MachineUpgradeAttemptId, MachineVersion, Namespace,
    RequestMachineUpgradeRequest, ResolvedServiceSpec, RpcErrorCode, RpcResponseBody,
    RuntimeWatchRequest, ServiceId, op,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use tonic::{Code, Request};

#[test]
fn an_operator_reads_every_env_key_but_the_opened_ones() {
    let opened = BTreeSet::from(["DATABASE_URL".to_owned()]);
    let mut environment = Some(BTreeMap::from([
        ("DATABASE_URL".to_owned(), "postgres://secret".to_owned()),
        ("PORT".to_owned(), "8080".to_owned()),
    ]));
    withhold_opened(&mut environment, &opened);
    assert_eq!(
        environment,
        Some(BTreeMap::from([("PORT".to_owned(), "8080".to_owned())]))
    );

    let mut unknown = None;
    withhold_opened(&mut unknown, &opened);
    assert_eq!(unknown, None);
}

#[tokio::test]
async fn retained_env_answers_no_opened_value_and_names_the_opened_keys() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = std::env::temp_dir().join(format!("ployzd-retained-env-{}", MachineId::random()));
    let store = RecordOwner::spawn(LocalMachineStore::open(&dir).unwrap()).unwrap();
    let socket = dir.join("observe.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let sealed = BTreeMap::from([(
        "DATABASE_URL".to_owned(),
        ployz_core::EnvValue::Sealed(cluster.seal(
            "DATABASE_URL",
            ployz_core::SealedScope::Shared,
            "postgres://opened",
        )),
    )]);
    let label = serde_json::to_string(&cluster.open(sealed).unwrap().record).unwrap();
    let environment = crate::observe::layout::RetainedEnv::from_inspected(
        &[
            "DATABASE_URL=postgres://opened".to_owned(),
            "PORT=8080".to_owned(),
        ],
        Some(&label),
    )
    .unwrap();
    let answer = serde_json::to_vec(&serde_json::json!({ "reply": environment })).unwrap();
    let observer = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut len = [0; 4];
            stream.read_exact(&mut len).await.unwrap();
            let mut request = vec![0; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut request).await.unwrap();
            let len = u32::try_from(answer.len()).unwrap();
            stream.write_all(&len.to_be_bytes()).await.unwrap();
            stream.write_all(&answer).await.unwrap();
        }
    });
    let service = MachineService::with_cluster(store, None).with_observe_socket(socket);
    let id = ContainerId::parse("a".repeat(64)).unwrap();

    for from_operator in [false, true] {
        let mut request = Request::new(
            op::RetainedEnv::into_request(ployz_core::RetainedEnvRequest { container_id: id })
                .encode()
                .unwrap(),
        );
        if from_operator {
            request
                .metadata_mut()
                .insert(ployz_core::OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
        }
        let reply = service
            .retained_env(request)
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::RetainedEnv>()
            .unwrap();
        assert_eq!(
            reply.environment,
            Some(BTreeMap::from([("PORT".to_owned(), "8080".to_owned())])),
            "operator: {from_operator}"
        );
    }
    assert_eq!(
        service.opened_keys(id).await.unwrap(),
        BTreeSet::from(["DATABASE_URL".to_owned()])
    );
    observer.abort();
    std::fs::remove_dir_all(dir).ok();
}

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

#[test]
fn an_off_channel_upgrade_refusal_names_its_capability() {
    let status = super::upgrade_error(crate::installer::upgrade::Error::OffChannel {
        target: MachineVersion::parse("0.3.17-beta.1").unwrap(),
    })
    .unwrap_err();

    assert_eq!(status.code(), Code::PermissionDenied);
    assert!(
        status.message().contains("needs Full access"),
        "{}",
        status.message()
    );
    let details = ployz_core::rpc::status_details(status.details()).unwrap();
    assert_eq!(
        details
            .get("capability")
            .and_then(serde_json::Value::as_str),
        Some(ployz_core::UPGRADE_EXACT_CAPABILITY),
        "{details}"
    );
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
async fn a_pulled_image_answers_only_after_docker_holds_it() {
    use axum::http::Method;
    let (data_dir, _store, service, fake) = fake_docker_service("ployzd-pull-image").await;
    let image = "example.test/api:1";
    service
        .pull_image(Request::new(
            op::PullImage::into_request(ployz_core::PullImageRequest {
                image: image.into(),
                policy: ployz_core::PullPolicy::Always,
                registry_auth: None,
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::PullImage>()
        .unwrap();
    let requests = fake.requests.lock().unwrap().clone();
    let pulled = requests
        .iter()
        .position(|(method, path)| *method == Method::POST && path.ends_with("/images/create"));
    let inspected = requests.iter().rposition(|(method, path)| {
        *method == Method::GET && path.ends_with(&format!("/images/{image}/json"))
    });
    assert!(
        matches!((pulled, inspected), (Some(pulled), Some(inspected)) if pulled < inspected),
        "{requests:?}"
    );
    std::fs::remove_dir_all(data_dir).unwrap();
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
    let (data_dir, store, service, fake, _) = fake_docker_service_keeping_runtime(prefix).await;
    (data_dir, store, service, fake)
}

/// The same, with the runtime whose spec store the service reads, so a service built again
/// over it finds the same specs.
async fn fake_docker_service_keeping_runtime(
    prefix: &str,
) -> (
    std::path::PathBuf,
    RecordOwner,
    MachineService,
    crate::docker::test_support::FakeDocker,
    crate::docker::ContainerRuntime,
) {
    let (data_dir, store, service, fake, runtime) = fake_docker_service_built(prefix, None).await;
    (data_dir, store, service, fake, runtime)
}

/// [`fake_docker_service_keeping_runtime`] on a Machine that holds a Cluster's Sealing
/// Key and resolves Machine paths under `<data_dir>/root`.
async fn fake_docker_service_sealing(
    prefix: &str,
) -> (
    std::path::PathBuf,
    RecordOwner,
    MachineService,
    crate::docker::test_support::FakeDocker,
    crate::docker::ContainerRuntime,
    crate::sealed_env::test_support::Cluster,
) {
    let cluster = crate::sealed_env::test_support::Cluster::new();
    let (data_dir, store, service, fake, runtime) =
        fake_docker_service_built(prefix, Some(cluster.dir.0.clone())).await;
    (data_dir, store, service, fake, runtime, cluster)
}

async fn fake_docker_service_built(
    prefix: &str,
    sealing_key_dir: Option<std::path::PathBuf>,
) -> (
    std::path::PathBuf,
    RecordOwner,
    MachineService,
    crate::docker::test_support::FakeDocker,
    crate::docker::ContainerRuntime,
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
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let store = RecordOwner::spawn(store).unwrap();
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        list_named: true,
        ..Default::default()
    })
    .await;
    let runtime = match sealing_key_dir {
        Some(dir) => runtime
            .with_sealing_key(dir)
            .with_machine_root(data_dir.join("root")),
        None => runtime,
    };
    let mut service = MachineService::with_cluster(store.clone(), None)
        .with_optional_containers(Some(runtime.clone()));
    // `true` lists no pools, which reads as ZFS ready whatever the host has.
    service.local = service.local.with_zpool("true");
    (data_dir, store, service, fake, runtime)
}

mod ingress_doors {
    //! The Ingress doors route and the legacy Caddy Deploy it translates, against the
    //! fake Docker: who may call it, and what each answers.

    use axum::http::Method;
    use ployz_core::{
        ContainerNow, ContainerRefusalReason, DoorChange, EnsuredContainer, IngressDoorsSet,
        OPERATOR_SCOPE_HEADER, QualifiedService, RESTART_APPROVAL_CAPABILITY, RpcError,
        SET_INGRESS_DOORS_CAPABILITY, rpc::status_details,
    };
    use serde_json::{Value, json};
    use tonic::Status;

    use super::*;
    use crate::docker::test_support::FakeDocker;
    use crate::images::DoorSpecs;

    fn proxy_id() -> ServiceId {
        ServiceId::parse("c".repeat(32)).unwrap()
    }

    fn doors_request() -> Value {
        json!({"doors": ["public"], "proxy_service": proxy_id()})
    }

    /// The route's answer: a `Status` fails the call, an `RpcError` answers it.
    async fn set_doors(
        service: &MachineService,
        body: Value,
        operator: bool,
    ) -> Result<Result<IngressDoorsSet, RpcError>, Status> {
        let mut request = Request::new(
            op::SetIngressDoors::into_request(serde_json::from_value(body).unwrap())
                .encode()
                .unwrap(),
        );
        if operator {
            request
                .metadata_mut()
                .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
        }
        let response = service
            .set_ingress_doors(request)
            .await?
            .into_inner()
            .decode_response()
            .unwrap();
        if let RpcResponseBody::Error(error) = response.body {
            return Ok(Err(error));
        }
        let shown = format!("{response:?}");
        Ok(Ok(response
            .decode::<op::SetIngressDoors>()
            .unwrap_or_else(|error| panic!("{error}: {shown}"))))
    }

    /// A legacy Deploy's want: the Caddy spec an older CLI builds, under its slot.
    fn legacy_caddy_want() -> Value {
        let spec = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(proxy_id(), Default::default())
            .unwrap();
        door_want(&spec)
    }

    fn door_want(spec: &ployz_core::ResolvedServiceSpec) -> Value {
        json!({"want":{"state":"running", "rotation":"pending", "container":{"new":{
            "slot": {"namespace":"ployz-system", "service": spec.name, "kind":"service_container",
                     "creation_key": ployz_core::global_creation_key(spec)},
            "resolved_spec": spec, "gen":1}}}})
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

    #[track_caller]
    fn refused_for_doors(reply: &EnsuredContainer, reason: ContainerRefusalReason) {
        let EnsuredContainer::Refused { refusal, .. } = reply else {
            panic!("expected a refusal, got {reply:?}");
        };
        assert_eq!(refusal.reason, reason, "{}", refusal.message);
        assert_eq!(
            refusal.capability.as_deref(),
            Some(SET_INGRESS_DOORS_CAPABILITY),
            "{}",
            refusal.message
        );
    }

    fn docker_calls(fake: &FakeDocker) -> usize {
        fake.requests.lock().unwrap().len()
    }

    async fn no_ingress(store: &RecordOwner) {
        store
            .mutate(|store| {
                store.update(
                    serde_json::from_value(json!({"accepts_ingress": false})).unwrap(),
                    &[],
                )
            })
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn the_route_places_both_doors_and_then_changes_nothing() {
        let (data_dir, _store, service, fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-place").await;
        let set = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        let holder = set.holder.unwrap();
        let proxy = set.proxy.unwrap();
        assert_eq!(holder.change, DoorChange::Created);
        assert_eq!(proxy.change, DoorChange::Created);
        assert_eq!(
            holder.now.identity(),
            QualifiedService::system_ingress_holder()
        );
        assert_eq!(proxy.now.identity(), QualifiedService::system_ingress());
        assert!(matches!(
            holder.now.runtime,
            ContainerRuntimeObservation::Running { .. }
        ));
        assert!(matches!(
            proxy.now.runtime,
            ContainerRuntimeObservation::Running { .. }
        ));
        let specs = DoorSpecs::public(proxy_id());
        assert_eq!(proxy.now.resolved_spec, specs.proxy);
        assert_eq!(holder.now.resolved_spec, specs.holder);
        let inventory = runtime
            .door_inventory(&service.local.record().id())
            .await
            .unwrap();
        assert_eq!(inventory.len(), 2);
        let caddy = inventory
            .iter()
            .find(|entry| entry.facts.door == crate::docker::Door::Proxy)
            .unwrap();
        assert_eq!(caddy.facts.joined_to, Some(holder.now.container_id));

        let seen = docker_calls(&fake);
        let again = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.failure, None, "{again:?}");
        assert_eq!(again.holder.unwrap().change, DoorChange::Unchanged);
        assert_eq!(again.proxy.unwrap().change, DoorChange::Unchanged);
        assert!(again.retired.is_empty());
        let since = fake.requests.lock().unwrap().split_off(seen);
        assert!(
            since.iter().all(|(method, _)| *method == Method::GET),
            "a converged Machine is only read: {since:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn an_unknown_door_is_refused_naming_the_doors_capability() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-doors-unknown").await;
        let seen = docker_calls(&fake);
        let error = set_doors(
            &service,
            json!({"doors": ["public", "window"], "proxy_service": proxy_id()}),
            false,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.code,
            RpcErrorCode::InvalidArgument,
            "{}",
            error.message
        );
        assert_eq!(
            error.details.get("capability"),
            Some(&json!(SET_INGRESS_DOORS_CAPABILITY))
        );
        assert_eq!(docker_calls(&fake), seen);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn the_tunnel_door_needs_a_token_sealed_for_this_cluster() {
        let (data_dir, _store, service, fake, _runtime, _cluster) =
            fake_docker_service_sealing("ployzd-doors-token").await;
        let seen = docker_calls(&fake);
        let missing = set_doors(
            &service,
            json!({"doors": ["tunnel"], "proxy_service": proxy_id()}),
            false,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            missing.code,
            RpcErrorCode::InvalidArgument,
            "{}",
            missing.message
        );
        assert_eq!(
            missing.details.get("capability"),
            Some(&json!(ployz_core::SET_INGRESS_TUNNEL_CAPABILITY))
        );

        let other = crate::sealed_env::test_support::Cluster::new();
        let foreign = other.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let unopenable = set_doors(
            &service,
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": foreign}),
            false,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            unopenable.code,
            RpcErrorCode::InvalidArgument,
            "{}",
            unopenable.message
        );
        assert_eq!(unopenable.details.get("capability"), None);
        assert_eq!(
            docker_calls(&fake),
            seen,
            "nothing reaches Docker without a token"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn the_tunnel_door_closes_the_ports_and_runs_the_connector_on_the_opened_token() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-tunnel").await;
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let tunnel =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let set = set_doors(&service, tunnel.clone(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        let specs = DoorSpecs::new(proxy_id(), ployz_core::IngressPorts::Closed, Some(&sealed));
        let holder = set.holder.unwrap();
        assert_eq!(holder.change, DoorChange::Created);
        assert_eq!(
            holder.now.resolved_spec, specs.holder,
            "the holder publishes no port"
        );
        assert_eq!(set.proxy.unwrap().now.resolved_spec, specs.proxy);
        let connector = set.tunnel.unwrap();
        assert_eq!(connector.change, DoorChange::Created);
        assert_eq!(connector.now.identity(), QualifiedService::system_tunnel());
        assert_eq!(connector.now.resolved_spec, specs.tunnel.unwrap());
        let token = data_dir.join("root/run/ployz/tunnel/token");
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        let inventory = runtime
            .door_inventory(&service.local.record().id())
            .await
            .unwrap();
        assert_eq!(inventory.len(), 3);
        assert!(inventory.iter().all(|entry| {
            entry.facts.door == crate::docker::Door::Holder
                || entry.facts.joined_to == Some(holder.now.container_id)
        }));

        let again = set_doors(&service, tunnel, false).await.unwrap().unwrap();
        assert_eq!(again.holder.unwrap().change, DoorChange::Unchanged);
        assert_eq!(again.tunnel.unwrap().change, DoorChange::Unchanged);

        let seen = docker_calls(&fake);
        let public = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(public.failure, None, "{public:?}");
        assert_eq!(
            public.tunnel, None,
            "the Public Door alone retires the connector"
        );
        assert_eq!(
            public.holder.unwrap().now.resolved_spec,
            DoorSpecs::public(proxy_id()).holder
        );
        assert!(docker_calls(&fake) > seen);
        assert!(
            !token.exists(),
            "the opened token leaves with the connector"
        );
        assert_eq!(
            runtime
                .door_inventory(&service.local.record().id())
                .await
                .unwrap()
                .len(),
            2
        );

        let both = set_doors(
            &service,
            json!({"doors": ["public", "tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed}),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(both.failure, None, "{both:?}");
        assert_eq!(
            both.holder.unwrap().now.resolved_spec,
            DoorSpecs::public(proxy_id()).holder,
            "both Doors keep the ports open beside the connector"
        );
        assert!(both.tunnel.is_some());
        runtime.remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_reboot_leaves_the_connector_at_rest_and_the_doors_held_place_it_again() {
        let (data_dir, _store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-reboot").await;
        assert!(
            service.local.restore_doors().await.unwrap().is_none(),
            "a Machine that held no doors places none"
        );
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let tunnel =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let placed = set_doors(&service, tunnel, false).await.unwrap().unwrap();
        assert_eq!(placed.failure, None, "{placed:?}");
        let connector = placed.tunnel.unwrap().now.container_id;

        let token = data_dir.join("root/run/ployz/tunnel/token");
        let running = |runtime: &crate::docker::ContainerRuntime| {
            let runtime = runtime.clone();
            let machine_id = service.local.record().id();
            async move {
                runtime
                    .door_inventory(&machine_id)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|entry| entry.facts.door == crate::docker::Door::Tunnel)
                    .all(|entry| entry.facts.running())
            }
        };
        let untouched = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(untouched.failure, None, "{untouched:?}");
        assert_eq!(
            untouched.tunnel.unwrap().change,
            DoorChange::Unchanged,
            "a daemon restart leaves the running doors alone"
        );

        runtime.stop(&connector, None, None).await.unwrap();
        runtime.remove_tunnel_token().unwrap();
        assert!(!running(&runtime).await && !token.exists());

        let restored = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        assert_eq!(restored.holder.unwrap().change, DoorChange::Unchanged);
        let started = restored.tunnel.unwrap();
        assert_eq!(started.change, DoorChange::Started);
        assert_eq!(started.now.container_id, connector);
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        assert!(running(&runtime).await);

        runtime.remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn an_operator_under_enforcement_is_refused_before_docker() {
        let (data_dir, store, service, fake) = fake_docker_service("ployzd-doors-enforced").await;
        store
            .mutate(|store| store.set_device_enforcement(true, Vec::new()).unwrap())
            .await
            .unwrap();
        let seen = docker_calls(&fake);
        let status = set_doors(&service, doors_request(), true)
            .await
            .unwrap_err();
        assert_eq!(
            status.code(),
            Code::PermissionDenied,
            "{}",
            status.message()
        );
        assert_eq!(
            status_details(status.details()).unwrap().get("capability"),
            Some(&json!(RESTART_APPROVAL_CAPABILITY))
        );
        assert_eq!(
            docker_calls(&fake),
            seen,
            "no Docker call before the refusal"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_machine_without_the_ingress_role_refuses_the_route() {
        let (data_dir, store, service, fake) = fake_docker_service("ployzd-doors-role").await;
        no_ingress(&store).await;
        let seen = docker_calls(&fake);
        let error = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Conflict, "{}", error.message);
        assert!(error.message.contains("role"), "{}", error.message);
        assert_eq!(docker_calls(&fake), seen);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_legacy_deploy_is_answered_with_the_joined_caddy() {
        let (data_dir, _store, service, fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-legacy").await;
        let first = ensure(&service, legacy_caddy_want()).await;
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(caddy),
            health: None,
        } = &first
        else {
            panic!("expected the joined Caddy, got {first:?}");
        };
        let specs = DoorSpecs::public(proxy_id());
        assert_eq!(
            caddy.resolved_spec, specs.proxy,
            "the CLI's spec never reaches Docker"
        );
        let bodies = fake.request_bodies.lock().unwrap().clone();
        let caddy_body = bodies
            .iter()
            .find(|(path, body)| {
                path.ends_with("/containers/create")
                    && body.pointer("/Labels/ployz.service.name") == Some(&json!("ingress"))
            })
            .map(|(_, body)| body.clone())
            .expect("the Caddy was created");
        let holder_id = runtime
            .door_inventory(&service.local.record().id())
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.facts.door == crate::docker::Door::Holder)
            .unwrap()
            .facts
            .id;
        assert_eq!(
            caddy_body.pointer("/HostConfig/NetworkMode"),
            Some(&json!(format!("container:{holder_id}")))
        );
        assert_eq!(caddy_body.pointer("/HostConfig/PortBindings"), None);

        let second = ensure(&service, legacy_caddy_want()).await;
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(replaced),
            ..
        } = &second
        else {
            panic!("expected the joined Caddy, got {second:?}");
        };
        assert_ne!(
            replaced.container_id, caddy.container_id,
            "a Deploy never answers a Caddy that existed before it"
        );
        assert!(matches!(
            runtime
                .inspect_managed(&caddy.container_id, &service.local.record().id())
                .await,
            Err(crate::docker::Error::ContainerNotFound(_))
        ));
        assert_eq!(
            runtime
                .door_inventory(&service.local.record().id())
                .await
                .unwrap()
                .len(),
            2,
            "one holder and one Caddy remain"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn an_older_clis_deploy_on_a_converged_machine_keeps_a_caddy_running() {
        let (data_dir, _store, service, _fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-older-cli").await;
        let machine = service.local.record().id();
        set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        let observed = runtime
            .door_inventory(&machine)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.facts.door == crate::docker::Door::Proxy)
            .unwrap()
            .facts
            .id;

        let stopped = ensure(
            &service,
            json!({"want":{"state":"stopped", "container":{"id":observed}, "drain":0}}),
        )
        .await;
        assert!(
            matches!(stopped, EnsuredContainer::Ensured { .. }),
            "{stopped:?}"
        );
        let created = ensure(&service, legacy_caddy_want()).await;
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(created),
            ..
        } = &created
        else {
            panic!("expected the joined Caddy, got {created:?}");
        };
        assert_ne!(created.container_id, observed);
        let retired = ensure(
            &service,
            json!({"want":{"state":"gone", "container":{"id":observed}, "drain":0}}),
        )
        .await;
        assert!(
            !matches!(retired, EnsuredContainer::Refused { .. }),
            "{retired:?}"
        );

        let doors = runtime.door_inventory(&machine).await.unwrap();
        assert_eq!(doors.len(), 2, "one holder and one Caddy remain");
        for entry in &doors {
            assert!(
                matches!(
                    entry.facts.runtime,
                    ployz_core::ContainerRuntimeObservation::Running { .. }
                ),
                "{:?} runs: {:?}",
                entry.facts.door,
                entry.facts.runtime
            );
        }
        assert!(
            doors
                .iter()
                .any(|entry| entry.facts.id == created.container_id),
            "the Caddy the older CLI was answered with is the one running"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    fn caddy_create_refused(fake: &FakeDocker) {
        fake.refuse_create_of
            .lock()
            .unwrap()
            .insert("ingress".into());
    }

    fn caddy_start_exits(fake: &FakeDocker) {
        fake.exit_on_start
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Each door Container on `machine`, holders first, with whether it is at rest.
    async fn at_rest_by_id(
        runtime: &crate::docker::ContainerRuntime,
        machine: &MachineId,
    ) -> Vec<(ployz_core::ContainerId, bool)> {
        runtime
            .door_inventory(machine)
            .await
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry.facts.id,
                    crate::ingress::at_rest(&entry.facts.runtime),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn an_older_clis_deploy_whose_caddy_fails_leaves_the_old_one_stopped() {
        type Fail = fn(&FakeDocker);
        let failures: [(&str, Fail); 2] = [
            ("refused", caddy_create_refused),
            ("exited", caddy_start_exits),
        ];
        for (failure, fail) in failures {
            let (data_dir, _store, service, fake, runtime) =
                fake_docker_service_keeping_runtime("ployzd-doors-failed-caddy").await;
            let machine = service.local.record().id();
            set_doors(&service, doors_request(), false)
                .await
                .unwrap()
                .unwrap();
            let doors = runtime.door_inventory(&machine).await.unwrap();
            let caddy_of = |door| {
                doors
                    .iter()
                    .find(|entry| entry.facts.door == door)
                    .unwrap()
                    .facts
                    .id
            };
            let (holder, observed) = (
                caddy_of(crate::docker::Door::Holder),
                caddy_of(crate::docker::Door::Proxy),
            );
            ensure(
                &service,
                json!({"want":{"state":"stopped", "container":{"id":observed}, "drain":0}}),
            )
            .await;

            fail(&fake);
            let reply = ensure(&service, legacy_caddy_want()).await;
            refused_for_doors(&reply, ContainerRefusalReason::InvalidIngress);
            assert_eq!(
                at_rest_by_id(&runtime, &machine).await,
                vec![(holder, false), (observed, true)],
                "{failure}: the holder runs and the old Caddy is left stopped, alone"
            );

            fake.exit_on_start
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let restarted = ensure(
                &service,
                json!({"want":{"state":"running", "rotation":"pending",
                               "container":{"existing":{"id":observed}}}}),
            )
            .await;
            assert!(
                matches!(restarted, EnsuredContainer::Ensured { .. }),
                "{failure}: the older CLI starts the Caddy it stopped again: {restarted:?}"
            );
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    /// A port-publishing Caddy from before the holder, as an older daemon placed it.
    async fn legacy_caddy(
        service: &MachineService,
        runtime: &crate::docker::ContainerRuntime,
    ) -> ployz_core::ContainerId {
        let record = service.local.record();
        let machine = record.machine().unwrap();
        let spec = crate::images::legacy_caddy_for_tests("caddy:2")
            .to_resolved(proxy_id(), Default::default())
            .unwrap();
        let created = runtime
            .create_for_test(
                &machine.id,
                machine.subnet.gateway(),
                ployz_core::ContainerKind::ServiceContainer,
                &ployz_core::Namespace::system(),
                &spec,
            )
            .await
            .unwrap();
        runtime.start(&created.container_id).await.unwrap();
        created.container_id
    }

    #[tokio::test]
    async fn a_cutover_whose_caddy_fails_brings_the_legacy_caddy_back() {
        let (data_dir, _store, service, fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-cutover").await;
        let machine = service.local.record().id();
        let legacy = legacy_caddy(&service, &runtime).await;
        caddy_create_refused(&fake);
        let set = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            set.failure.as_ref().map(|failure| failure.stage),
            Some(ployz_core::IngressStage::Proxy),
            "{set:?}"
        );
        assert_eq!(set.holder, None, "the new holder is gone: {set:?}");
        assert_eq!(
            at_rest_by_id(&runtime, &machine).await,
            vec![(legacy, false)],
            "the holder made for the cutover is removed, and the legacy Caddy the route \
             stopped runs again"
        );

        ensure(
            &service,
            json!({"want":{"state":"stopped", "container":{"id":legacy}, "drain":0}}),
        )
        .await;
        let reply = ensure(&service, legacy_caddy_want()).await;
        refused_for_doors(&reply, ContainerRefusalReason::InvalidIngress);
        assert_eq!(
            at_rest_by_id(&runtime, &machine).await,
            vec![(legacy, true)],
            "a legacy Caddy the older CLI stopped stays stopped for its compensation"
        );

        fake.refuse_create_of.lock().unwrap().clear();
        let set = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        assert_eq!(set.retired, vec![legacy]);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    fn inspect_fails_after_start(fake: &FakeDocker, service: &str) {
        fake.fail_inspect_after_start
            .lock()
            .unwrap()
            .insert(service.into(), 1);
    }

    #[tokio::test]
    async fn a_cutover_whose_inspect_fails_after_a_start_brings_the_legacy_caddy_back() {
        let stages = [
            ("ingress-holder", ployz_core::IngressStage::Holder),
            ("ingress", ployz_core::IngressStage::Proxy),
        ];
        for (service, stage) in stages {
            let (data_dir, _store, service_api, fake, runtime) =
                fake_docker_service_keeping_runtime("ployzd-doors-cutover-inspect").await;
            let machine = service_api.local.record().id();
            let legacy = legacy_caddy(&service_api, &runtime).await;
            inspect_fails_after_start(&fake, service);
            let set = set_doors(&service_api, doors_request(), false)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                set.failure.as_ref().map(|failure| failure.stage),
                Some(stage),
                "{service}: {set:?}"
            );
            assert_eq!(
                set.holder, None,
                "{service}: the new holder is gone: {set:?}"
            );
            assert_eq!(
                at_rest_by_id(&runtime, &machine).await,
                vec![(legacy, false)],
                "{service}: every door the cutover created is removed, and the legacy Caddy \
                 runs again alone"
            );
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    #[tokio::test]
    async fn a_fresh_caddy_whose_inspect_fails_after_its_start_leaves_one_caddy() {
        let (data_dir, _store, service, fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-fresh-inspect").await;
        let machine = service.local.record().id();
        set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        let before = at_rest_by_id(&runtime, &machine).await;
        assert_eq!(before.len(), 2, "{before:?}");

        inspect_fails_after_start(&fake, "ingress");
        let reply = ensure(&service, legacy_caddy_want()).await;
        refused_for_doors(&reply, ContainerRefusalReason::InvalidIngress);
        assert_eq!(
            at_rest_by_id(&runtime, &machine).await,
            before,
            "the new Caddy is removed and the old one runs again, never beside it"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_cutover_that_started_the_holder_stops_it_before_the_legacy_caddy_returns() {
        let (data_dir, _store, service, fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-started-holder").await;
        let machine = service.local.record().id();
        set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        let mut holder = None;
        for entry in runtime.door_inventory(&machine).await.unwrap() {
            runtime.stop(&entry.facts.id, None, None).await.unwrap();
            match entry.facts.door {
                crate::docker::Door::Holder => holder = Some(entry.facts.id),
                crate::docker::Door::Proxy | crate::docker::Door::Tunnel => {
                    runtime.remove(&entry.facts.id, true, true).await.unwrap();
                }
            }
        }
        let holder = holder.unwrap();
        let legacy = legacy_caddy(&service, &runtime).await;

        caddy_create_refused(&fake);
        let set = set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            set.failure.as_ref().map(|failure| failure.stage),
            Some(ployz_core::IngressStage::Proxy),
            "{set:?}"
        );
        assert_eq!(
            set.holder, None,
            "the holder the call started is stopped: {set:?}"
        );
        assert_eq!(
            at_rest_by_id(&runtime, &machine).await,
            vec![(holder, true), (legacy, false)],
            "the holder lets go of the ports before the legacy Caddy runs again"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_door_stop_whose_caller_left_holds_the_doors_until_it_lands() {
        let (data_dir, _store, service, _fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-detached-stop").await;
        let machine = service.local.record().id();
        set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        let holder = runtime
            .door_inventory(&machine)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.facts.door == crate::docker::Door::Holder)
            .unwrap()
            .facts
            .id;

        let mut stopping = runtime.subscribe_stopping_for_test();
        let stop = tokio::spawn({
            let service = service.clone();
            async move {
                ensure(
                    &service,
                    json!({"want":{"state":"stopped", "container":{"id":holder}, "drain":2000}}),
                )
                .await
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            stopping.wait_for(|stopping| stopping.contains_key(&holder)),
        )
        .await
        .expect("the holder's stop began its drain")
        .unwrap();
        stop.abort();
        assert!(stop.await.unwrap_err().is_cancelled());
        assert!(
            futures_util::poll!(std::pin::pin!(runtime.door_lifecycle())).is_pending(),
            "the stop its caller left still holds the doors through its drain"
        );

        let placing = tokio::spawn({
            let service = service.clone();
            async move { set_doors(&service, doors_request(), false).await }
        });
        let set = placing.await.unwrap().unwrap().unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        assert_eq!(
            set.holder.map(|holder| holder.change),
            Some(DoorChange::Started),
            "the route sees the holder the stop left at rest"
        );
        assert!(
            at_rest_by_id(&runtime, &machine)
                .await
                .iter()
                .all(|(_, at_rest)| !at_rest),
            "both doors run once the route answers"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_legacy_deploy_without_the_role_is_refused() {
        let (data_dir, store, service, fake) =
            fake_docker_service("ployzd-doors-legacy-role").await;
        no_ingress(&store).await;
        let seen = docker_calls(&fake);
        let reply = ensure(&service, legacy_caddy_want()).await;
        let EnsuredContainer::Refused { refusal, .. } = &reply else {
            panic!("expected a refusal, got {reply:?}");
        };
        assert_eq!(refusal.reason, ContainerRefusalReason::NotEligible);
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .skip(seen)
                .all(|(method, _)| *method == Method::GET),
            "a refusal only reads"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn door_specs_sent_through_ensure_container_are_refused() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-doors-ensure").await;
        let specs = DoorSpecs::public(proxy_id());
        let seen = docker_calls(&fake);
        refused_for_doors(
            &ensure(&service, door_want(&specs.holder)).await,
            ContainerRefusalReason::InvalidIngress,
        );
        refused_for_doors(
            &ensure(&service, door_want(&specs.proxy)).await,
            ContainerRefusalReason::InvalidIngress,
        );
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .skip(seen)
                .all(|(method, _)| *method == Method::GET),
            "a refusal only reads"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }
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
            "newest":newest, "resume_token":null, "pull":null, "role":"old_copy"}})
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

    async fn inspect(service: &MachineService) -> ployz_core::VolumeCopyView {
        let request = ployz_core::InspectVolumeCopyRequest {
            name: VOLUME.parse().unwrap(),
        };
        let response = service
            .inspect_volume_copy(Request::new(
                op::InspectVolumeCopy::into_request(request)
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        response
            .clone()
            .decode::<op::InspectVolumeCopy>()
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
        let created = kept_holder(service, fake).await;
        start_holder(fake, &created);
        created
    }

    /// A stopped Container that mounts the Volume: Docker lists it for the cut too.
    async fn kept_holder(service: &MachineService, fake: &FakeDocker) -> ContainerId {
        let created = stopped_container(service).await;
        fake.volume_users
            .lock()
            .unwrap()
            .entry(VOLUME.into())
            .or_default()
            .push(json!({"Id": created.to_string()}));
        created
    }

    /// The cut withdraws a holder from ingress, then drains it. Awaiting that withdrawal
    /// rather than a fixed time keeps a slow test host from fencing the cut before its drain.
    async fn drain_started(service: &MachineService, holder: &ContainerId) {
        let mut stopping = service
            .local()
            .containers()
            .unwrap()
            .subscribe_stopping_for_test();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            stopping.wait_for(|stopping| stopping.contains_key(holder)),
        )
        .await
        .expect("the cut began draining the holder")
        .unwrap();
    }

    /// Asserts the cut holds the floor lock, so a floor want polled now queues behind it.
    async fn floor_lock_held(service: &MachineService) {
        let local = service.local();
        let turn = std::pin::pin!(local.containers().unwrap().floor_turn());
        assert!(
            futures_util::poll!(turn).is_pending(),
            "the cut holds the floor lock"
        );
    }

    fn hold_route(released: &std::sync::Mutex<std::sync::mpsc::Receiver<()>>) {
        tokio::task::block_in_place(|| {
            released
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the test released the route");
        });
    }

    async fn requested(fake: &FakeDocker, method: http::Method, suffix: &str) {
        let arrived = || {
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .any(|(seen, path)| *seen == method && path.ends_with(suffix))
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !arrived() {
                // real-wait: polls the fake Docker's request log, which emits no arrival event
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the request reached the fake Docker");
    }

    fn start_holder(fake: &FakeDocker, id: &ContainerId) {
        let id = id.to_string();
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
                "creation_key":"move-1", "gen":1, "rotation":"pending"}}});

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
                "creation_key":"move-1", "gen":1, "rotation":"pending"}}});

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
            vec![" INFO cut: holders stopped volume=app_data target=f-1 gen=0"]
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

        // Hang up once the cut is draining, not after a fixed wait: on a loaded runner the
        // cut may not have begun by then, and the undo would run before it.
        tokio::select! {
            finished = set_copy(&service, draining) => {
                panic!("the cut finished inside its drain: {finished:?}")
            }
            () = drain_started(&service, &holder) => {}
        }
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
        let first = running_holder(&service, &fake).await;
        let draining = |drain: u32| {
            json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
                "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":drain}})
        };

        // The fake holder never reads idle, so the drain lasts its whole cap, and the
        // second holder must start inside it. A loaded runner took over 300 ms to start one.
        let (refused, _) = tokio::join!(set_copy(&service, draining(3_000)), async {
            drain_started(&service, &first).await;
            running_holder(&service, &fake).await
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("busy"), "{error:?}");
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, INSPECT_ROUTE]);
        let mut status = holder_status(&fake);
        status.sort_by_key(ToString::to_string);
        assert_eq!(status, vec![json!("exited"), json!("running")]);

        let cut = reply(&set_copy(&service, draining(300)).await);
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

    fn at_gen(mut request: Value, r#gen: u64) -> Value {
        request
            .as_object_mut()
            .unwrap()
            .insert("gen".into(), json!(r#gen));
        request
    }

    fn floor(r#gen: u64) -> Value {
        at_gen(
            json!({"name":VOLUME, "expect":{"kind":"none"}, "want":{"kind":"floor"}}),
            r#gen,
        )
    }

    fn forget(expect: Value) -> Value {
        json!({"name":VOLUME, "expect":expect, "want":{"kind":"forget"}})
    }

    fn draining_cut(drain: u64) -> Value {
        json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
            "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":drain}})
    }

    /// A Writer root whose cut parks it in a slot holding `f-1`.
    fn cutting_plugin() -> FakePlugin {
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
        plugin
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_request_below_the_floor_refuses_stale_before_any_effect() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-stale").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let raised = reply(&set_copy(&service, floor(5)).await);
        assert_eq!(
            serde_json::to_value(&raised).unwrap(),
            json!({"copy": writer_root(0, None)["Ok"]})
        );
        let error = refusal(
            &set_copy(
                &service,
                at_gen(cut(json!({"kind":"writer", "epoch":0})), 4),
            )
            .await,
        );
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert_eq!(error.details["reason"], json!("stale"));
        assert_eq!(error.details["floor"], json!(5));
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(plugin.routes_called(), vec![INSPECT_ROUTE, INSPECT_ROUTE]);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_gen_at_or_above_the_floor_raises_it_and_a_lower_one_is_stale() {
        let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-copy-gen").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let request = forget(json!({"kind":"writer", "epoch":0}));

        let first = reply(&set_copy(&service, at_gen(request.clone(), 3)).await);
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            json!({"copy": writer_root(0, None)["Ok"]})
        );
        assert_eq!(
            reply(&set_copy(&service, at_gen(request.clone(), 3)).await),
            first
        );
        let error = refusal(&set_copy(&service, at_gen(request.clone(), 2)).await);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(3));
        let unnumbered = refusal(&set_copy(&service, request).await);
        assert_eq!(
            unnumbered.details["reason"],
            json!("stale"),
            "{unnumbered:?}"
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    const SEED_ROUTE: &str = "Volume.Seed";

    async fn seed(service: &MachineService, request: Value) -> RpcResponse {
        service
            .seed_volume(Request::new(
                op::SeedVolume::into_request(serde_json::from_value(request).unwrap())
                    .encode()
                    .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
    }

    /// A plugin holding a Mirror of `into` that a pull received, which its Seed route
    /// would `zfs destroy -r` and copy over (seed.rs `seed_volume`).
    fn pulled_mirror_plugin() -> FakePlugin {
        let plugin = FakePlugin::default();
        let mirror = json!({"kind":"slot", "slot":"1", "readonly":true,
            "newest":snapshot("w-9-1", 900), "resume_token":null, "pull":null, "role":"mirror"});
        plugin.reply(INSPECT_ROUTE, json!({"Ok": mirror}));
        plugin.reply(SEED_ROUTE, json!({"Ok": {"copy": mirror}}));
        plugin
    }

    /// A Seed request still in transit from a seed run that ended, landing after a later
    /// run raised `into`'s floor and pulled a Mirror of it back here.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_seed_below_the_floor_of_into_refuses_stale_before_any_effect() {
        let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-seed-stale").await;
        let plugin = pulled_mirror_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        reply(&set_copy(&service, floor(5)).await);

        let response = seed(
            &service,
            json!({"name":"source", "into":VOLUME, "guid":"77", "gen":4}),
        )
        .await;
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == SEED_ROUTE),
            "a Seed below into's floor reached the plugin, which replaces a pulled Mirror: {response:?}"
        );
        let error = refusal(&response);
        assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        assert_eq!(error.details["reason"], json!("stale"));
        assert_eq!(error.details["floor"], json!(5));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// The floor refuses only a gen below it, and a Seed raises no floor: a later
    /// request on `into` with a lower gen than the Seed's is still admitted. A Seed with
    /// no gen, from a Cloud that predates it, is not checked.
    #[tokio::test]
    async fn a_seed_at_the_floor_or_without_a_gen_reaches_the_plugin_and_raises_no_floor() {
        let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-seed-admit").await;
        let plugin = pulled_mirror_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        reply(&set_copy(&service, floor(5)).await);

        for request in [
            json!({"name":"source", "into":VOLUME, "guid":"77", "gen":5}),
            json!({"name":"source", "into":VOLUME, "guid":"77", "gen":9}),
            json!({"name":"source", "into":VOLUME, "guid":"77"}),
        ] {
            let response = seed(&service, request.clone()).await;
            response
                .clone()
                .decode::<op::SeedVolume>()
                .unwrap_or_else(|error| panic!("{request}: {error}: {response:?}"));
        }
        let seeds = plugin
            .routes_called()
            .into_iter()
            .filter(|route| route == SEED_ROUTE)
            .count();
        assert_eq!(seeds, 3);
        let floor = service
            .containers()
            .unwrap()
            .floor_turn()
            .await
            .admit(&VOLUME.parse().unwrap(), ployz_core::VolumeGen::new(5))
            .await
            .unwrap();
        assert_eq!(floor, Ok(()), "the Seed at gen 9 raised into's floor");
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_machine_outside_the_cluster_refuses_a_cut_forget_or_floor_and_raises_no_floor() {
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-copy-outside-{}", MachineId::random()));
        let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
        let (runtime, _fake) =
            crate::docker::test_support::fake_runtime_with(FakeDocker::default()).await;
        let service =
            MachineService::with_cluster(store, None).with_optional_containers(Some(runtime));

        for request in [
            at_gen(draining_cut(0), 5),
            at_gen(forget(json!({"kind":"writer", "epoch":0})), 5),
            floor(5),
        ] {
            let error = refusal(&set_copy(&service, request).await);
            assert_eq!(error.message, "Machine is not participating", "{error:?}");
        }
        let lower = service
            .containers()
            .unwrap()
            .floor_turn()
            .await
            .admit(&VOLUME.parse().unwrap(), ployz_core::VolumeGen::new(0))
            .await
            .unwrap();
        assert_eq!(lower, Ok(()), "a refused request left a floor behind");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_machine_outside_the_cluster_without_docker_says_it_is_not_participating() {
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-copy-bare-{}", MachineId::random()));
        let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
        let service = MachineService::with_cluster(store, None);

        for request in [
            at_gen(draining_cut(0), 5),
            at_gen(forget(json!({"kind":"writer", "epoch":0})), 5),
            floor(5),
        ] {
            let error = refusal(&set_copy(&service, request).await);
            assert_eq!(error.message, "Machine is not participating", "{error:?}");
        }
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_floor_queued_on_the_floor_lock_across_a_reset_refuses_and_raises_no_floor() {
        let (data_dir, store, service, _fake) = fake_docker_service("ployzd-copy-reset").await;
        let turn = service.containers().unwrap().floor_turn().await;
        let mut queued = std::pin::pin!(set_copy(&service, floor(5)));
        assert!(
            futures_util::poll!(queued.as_mut()).is_pending(),
            "the Floor did not queue on the floor lock"
        );
        store
            .mutate(LocalMachineStore::begin_reset)
            .await
            .unwrap()
            .unwrap();
        drop(turn);

        let response = queued.await;
        let lower = service
            .containers()
            .unwrap()
            .floor_turn()
            .await
            .admit(&VOLUME.parse().unwrap(), ployz_core::VolumeGen::new(0))
            .await
            .unwrap();
        assert_eq!(lower, Ok(()), "the Floor raised a floor after the reset");
        let error = refusal(&response);
        assert_eq!(error.message, "Machine is not participating", "{error:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_want_answers_the_root_while_a_cut_drains_then_the_slot() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-floor").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;

        let (cut, during) =
            tokio::join!(set_copy(&service, at_gen(draining_cut(3000), 1)), async {
                drain_started(&service, &holder).await;
                tokio::time::timeout(
                    std::time::Duration::from_millis(2000),
                    set_copy(&service, floor(1)),
                )
                .await
                .expect("the floor want waited on the draining cut")
            });
        assert_eq!(
            serde_json::to_value(reply(&during)).unwrap(),
            json!({"copy": writer_root(0, None)["Ok"]})
        );
        assert_eq!(
            serde_json::to_value(reply(&cut)).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(
            serde_json::to_value(reply(&set_copy(&service, floor(1)).await)).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Deploy that floors the Volume during the drain sees the holder running and
    /// keeps it, so the cut must neither stop it nor leave it out of ingress.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_raised_during_the_drain_refuses_the_cut_stale_and_leaves_its_holder_serving() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-fenced").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;

        let (refused, _) = tokio::join!(set_copy(&service, at_gen(draining_cut(300), 1)), async {
            drain_started(&service, &holder).await;
            set_copy(&service, floor(2)).await
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(2));
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        let local = service.local();
        let observed = local
            .containers()
            .unwrap()
            .inspect_managed(&holder, &local.record().id())
            .await
            .unwrap();
        assert!(observed.runtime.may_serve(), "{:?}", observed.runtime);
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == CUT_ROUTE)
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A sole holder is out of Internal DNS while withdrawn, so a cut the floor already
    /// refuses must return it before queueing for a turn other work may hold long.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_raised_during_the_drain_returns_the_holder_without_waiting_for_a_turn() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-fenced-busy").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let local = service.local();
        let containers = local.containers().unwrap();

        let turn = local.owner().admission_lock().write_owned().await;
        let cut = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            set_copy(&service, at_gen(draining_cut(3000), 1)),
        );
        let (refused, ()) = tokio::join!(cut, async {
            drain_started(&service, &holder).await;
            reply(&set_copy(&service, floor(2)).await);
        });
        let refused = refused.expect("the refused cut waited for the turn");
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert!(containers.stopping_ids_for_test().is_empty());
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        drop(turn);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Deploy that floors the Volume while the cut stops its holder reads the holder
    /// next and keeps a running one, so the floor must wait for the stop to land.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_raised_during_the_holder_stop_waits_for_it_and_sees_the_holder_stopped() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-fenced-stop").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let stop = format!("/containers/{holder}/stop");
        let held = fake.stop_gate.write().await;

        let (refused, seen_by_floor) =
            tokio::join!(set_copy(&service, at_gen(draining_cut(0), 1)), async {
                requested(&fake, http::Method::POST, &stop).await;
                floor_lock_held(&service).await;
                let mut floored = std::pin::pin!(async {
                    reply(&set_copy(&service, floor(2)).await);
                    holder_status(&fake)
                });
                assert!(futures_util::poll!(floored.as_mut()).is_pending());
                drop(held);
                floored.await
            });
        assert_eq!(seen_by_floor, vec![json!("exited")]);
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(2));
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_read_failing_after_the_drain_stops_nothing_and_leaves_its_holder_serving() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-fence-fails").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let local = service.local();
        let containers = local.containers().unwrap();
        let specs = containers.spec_store_path_for_test().to_owned();

        let (failed, ()) = tokio::join!(set_copy(&service, at_gen(draining_cut(300), 1)), async {
            drain_started(&service, &holder).await;
            rusqlite::Connection::open(&specs)
                .unwrap()
                .execute("DROP TABLE volume_floors", [])
                .unwrap();
        });
        let error = refusal(&failed);
        assert_ne!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        let stop = format!("/containers/{holder}/stop");
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| *path == stop)
        );
        assert!(containers.stopping_ids_for_test().is_empty());
        let observed = containers
            .inspect_managed(&holder, &local.record().id())
            .await
            .unwrap();
        assert!(observed.runtime.may_serve(), "{:?}", observed.runtime);
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == CUT_ROUTE)
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Deploy floors the writer and then starts its Container. A cut that was already
    /// draining an older holder must not stop that Container on its way to Stale.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_cut_fenced_during_its_drain_leaves_a_holder_started_after_the_floor_running() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-fenced-late").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let older = running_holder(&service, &fake).await;
        let deployed = kept_holder(&service, &fake).await;

        let (refused, _) = tokio::join!(set_copy(&service, at_gen(draining_cut(300), 1)), async {
            drain_started(&service, &older).await;
            let floored = set_copy(&service, floor(2)).await;
            start_holder(&fake, &deployed);
            floored
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(2));
        assert_eq!(running(&fake), 2, "{:?}", holder_status(&fake));
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == CUT_ROUTE)
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Deploy that floors the Volume while a Writer raise runs its route starts its
    /// own Container next, so the raise must not start an older one beside it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_raised_during_a_raise_refuses_its_start_stale() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-raise-fenced").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, Some(snapshot("f-1", 7))));
        plugin.reply(WRITER_ROUTE, writer_root(2, Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        let raising = std::sync::Arc::new(tokio::sync::Notify::new());
        let raised = raising.clone();
        let (release, released) = std::sync::mpsc::channel();
        let released = std::sync::Mutex::new(released);
        plugin.on(WRITER_ROUTE, move || {
            replies.lock().unwrap().insert(
                INSPECT_ROUTE.into(),
                writer_root(2, Some(snapshot("f-1", 7))),
            );
            raised.notify_one();
            hold_route(&released);
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let kept = stopped_container(&service).await;
        let raise = at_gen(
            json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":1},
                "want":{"kind":"writer", "at":"7", "epoch":2,
                    "start":{"kind":"kept", "container_id":kept.to_string()}}}),
            1,
        );

        let (refused, _) = tokio::join!(set_copy(&service, raise), async {
            raising.notified().await;
            let floored = set_copy(&service, floor(2)).await;
            release.send(()).unwrap();
            floored
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(2));
        assert_eq!(running(&fake), 0, "{:?}", holder_status(&fake));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_request_queued_behind_a_cut_refuses_stale_once_a_floor_fenced_it() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-queued").await;
        let plugin = cutting_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;
        let request = at_gen(forget(json!({"kind":"writer", "epoch":0})), 1);

        let (_, queued) = tokio::join!(set_copy(&service, at_gen(draining_cut(300), 1)), async {
            drain_started(&service, &holder).await;
            // Its first poll takes the floor lock to admit its gen, or queues on it, ahead
            // of the floor below: the lock is FIFO, so the request passes before the raise.
            let mut queued = std::pin::pin!(set_copy(&service, request));
            assert!(futures_util::poll!(queued.as_mut()).is_pending());
            tokio::join!(queued, set_copy(&service, floor(2))).0
        });
        let error = refusal(&queued);
        assert_eq!(error.details["reason"], json!("stale"), "{error:?}");
        assert_eq!(error.details["floor"], json!(2));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_floor_want_sent_during_the_flip_waits_for_it_and_answers_the_slot() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-copy-flip").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(0, None));
        plugin.reply(CUT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let replies = plugin.replies.clone();
        let flipping = std::sync::Arc::new(tokio::sync::Notify::new());
        let flip = flipping.clone();
        let (release, released) = std::sync::mpsc::channel();
        let released = std::sync::Mutex::new(released);
        plugin.on(CUT_ROUTE, move || {
            replies
                .lock()
                .unwrap()
                .insert(INSPECT_ROUTE.into(), json!({"Ok": null}));
            flip.notify_one();
            hold_route(&released);
            replies
                .lock()
                .unwrap()
                .insert(INSPECT_ROUTE.into(), slot(Some(snapshot("f-1", 7))));
        });
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        running_holder(&service, &fake).await;

        let (cut, during) = tokio::join!(set_copy(&service, at_gen(draining_cut(0), 1)), async {
            flipping.notified().await;
            floor_lock_held(&service).await;
            let mut floored = std::pin::pin!(set_copy(&service, floor(1)));
            assert!(futures_util::poll!(floored.as_mut()).is_pending());
            release.send(()).unwrap();
            floored.await
        });
        assert_eq!(
            serde_json::to_value(reply(&during)).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(reply(&cut), reply(&during));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Deploy, a release and a requeued move observe the Volume first, so an inspect
    /// that waited on a cut's drain would time out and fail them.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn an_inspect_answers_the_root_while_a_cut_drains() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-copy-inspect-drain").await;
        let plugin = cutting_plugin();
        plugin.reply(
            "Volume.Inspect",
            json!({"Ok": {"copy": writer_root(0, None)["Ok"]}}),
        );
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let holder = running_holder(&service, &fake).await;

        let (cut, during) = tokio::join!(set_copy(&service, at_gen(draining_cut(400), 1)), async {
            drain_started(&service, &holder).await;
            tokio::time::timeout(std::time::Duration::from_millis(200), inspect(&service))
                .await
                .expect("the inspect waited on the draining cut")
        });
        assert_eq!(
            serde_json::to_value(during).unwrap(),
            json!({"copy": writer_root(0, None)["Ok"]})
        );
        assert_eq!(
            serde_json::to_value(reply(&cut)).unwrap(),
            json!({"copy": slot(Some(snapshot("f-1", 7)))["Ok"]})
        );
        assert_eq!(holder_status(&fake), vec![json!("exited")]);
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
                "creation_key":"move-1", "gen":1, "rotation":"pending"}}});
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
                "creation_key":"move-1", "gen":1, "rotation":"pending"}}});

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
        assert_eq!(container["Config"]["Labels"][CREATION_KEY], json!("move-1"));
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

    /// Each child re-runs this binary armed through its own fault file, since a kill aborts
    /// the whole process.
    #[cfg(feature = "verify-faults")]
    mod fault_lines {
        use std::os::unix::process::ExitStatusExt;

        use super::*;

        const SIGABRT: i32 = 6;

        /// Runs the ignored test `child` of this module alone, under `fault`.
        fn run(child: &str, fault: &str) -> (std::process::Output, String) {
            let child = format!("machine_api::local::tests::set_volume_copy::fault_lines::{child}");
            let armed = tempfile::tempdir().unwrap();
            let path = armed.path().join("fault");
            std::fs::write(&path, fault).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &child, "--ignored", "--nocapture"])
                .env(crate::faults::PATH_ENV, &path)
                .env_remove(crate::diag::ENV)
                .output()
                .unwrap();
            (output, child)
        }

        fn fault_lines(text: &str) -> Vec<&str> {
            text.lines()
                .filter(|line| line.contains("fault:"))
                .collect()
        }

        #[test]
        fn a_switch_fault_line_names_the_volume_and_the_want() {
            let (output, child) = run(
                "a_cut_under_a_switch_delay",
                "delay-rpc:SetVolumeCopy.WriterToMirror:0",
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{child}:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fault_lines(&stdout),
                vec![
                    r#" WARN fault: delaying switch verb verb="SetVolumeCopy.WriterToMirror" secs=0 volume=app_data target=f-1 gen=6"#,
                    r#" WARN fault: releasing switch verb verb="SetVolumeCopy.WriterToMirror" by=limit volume=app_data target=f-1 gen=6"#,
                ]
            );
        }

        #[test]
        fn an_admission_fault_line_names_the_volume_and_the_want() {
            let (output, child) = run("a_cut_under_a_switch_delay", "delay-rpc:SetVolumeCopy:0");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{child}:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fault_lines(&stdout),
                vec![
                    r#" WARN fault: delaying switch verb verb="SetVolumeCopy" secs=0 volume=app_data target=f-1 gen=6"#,
                    r#" WARN fault: releasing switch verb verb="SetVolumeCopy" by=limit volume=app_data target=f-1 gen=6"#,
                ]
            );
        }

        /// A Floor only raises the gen floor, outside the switch lock, so an armed switch
        /// fault never holds it.
        #[test]
        fn a_floor_passes_an_armed_switch_fault_without_a_line() {
            let (output, child) = run("a_floor_under_a_switch_delay", "delay-rpc:SetVolumeCopy:30");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{child}:\n{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fault_lines(&stdout), Vec::<&str>::new());
        }

        /// The kill aborts the child, so its line is read from the child's stderr.
        #[test]
        fn a_promote_kill_line_names_the_volume_and_the_want() {
            let (output, child) = run(
                "a_promote_under_a_kill",
                "kill-daemon:SetVolumeCopy.MirrorToWriter",
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.signal(),
                Some(SIGABRT),
                "{child} was not aborted:\n{}\n{stderr}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert_eq!(
                fault_lines(&stderr),
                vec![
                    r#" WARN fault: killing daemon inside the effect verb="SetVolumeCopy.MirrorToWriter" volume=app_data at=900 epoch=1 gen=6"#
                ]
            );
        }

        #[tokio::test]
        #[ignore = "a child of a_promote_kill_line_names_the_volume_and_the_want"]
        async fn a_promote_under_a_kill() {
            crate::diag::init(None).unwrap();
            let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-copy-kill").await;
            let plugin = FakePlugin::default();
            plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 900))));
            plugin.reply(WRITER_ROUTE, writer_root(1, Some(snapshot("f-1", 900))));
            let (socket, _server) = plugin.serve(&data_dir);
            let service = service.with_volume_plugin(socket);
            let promote = json!({"name":VOLUME,
                "expect":{"kind":"mirror", "newest":"900", "slot":"1"},
                "want":{"kind":"writer", "at":"900", "epoch":1, "start":null}, "gen":6});

            let answered = set_copy(&service, promote).await;
            panic!("the promote outlived its kill: {answered:?}");
        }

        #[tokio::test]
        #[ignore = "a child of a_floor_passes_an_armed_switch_fault_without_a_line"]
        async fn a_floor_under_a_switch_delay() {
            let (data_dir, _store, service, _fake) =
                fake_docker_service("ployzd-floor-fault").await;
            let plugin = FakePlugin::default();
            plugin.reply(INSPECT_ROUTE, writer_root(0, None));
            let (socket, server) = plugin.serve(&data_dir);
            let service = service.with_volume_plugin(socket);

            let (capture, _logging) = crate::diag::test_support::capture();
            reply(&set_copy(&service, floor(6)).await);
            print!("{}", capture.text());
            server.abort();
            std::fs::remove_dir_all(data_dir).unwrap();
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

            let request = at_gen(cut(json!({"kind":"writer", "epoch":0})), 6);
            let (capture, _logging) = crate::diag::test_support::capture();
            reply(&set_copy(&service, request).await);
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
        json!({"namespace":"app", "service":"api", "kind":"service_container", "creation_key":key})
    }

    fn run_new(key: &str, spec: Value) -> Value {
        json!({"want":{"state":"running", "rotation":"pending", "container":{"new":{"slot":slot(key), "resolved_spec":spec, "gen":1}}}})
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
            observed.labels.get(CREATION_KEY).map(String::as_str),
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
    #[expect(clippy::indexing_slicing, reason = "JSON fixture edits")]
    async fn a_prepared_image_is_never_pulled_again_at_creation() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-prepared").await;
        let mut spec = spec("example.test/api:1");
        spec["container"]["pull_policy"] = json!("always");
        let pulls =
            |fake: &FakeDocker| ends_with(&requests(fake), Method::POST, "/images/create").count();

        let mut prepared = run_new("k1", spec.clone());
        prepared["want"]["container"]["new"]["image_prepared"] = json!(true);
        present(&ensure(&service, prepared).await);
        assert_eq!(pulls(&fake), 0);

        present(&ensure(&service, run_new("k2", spec)).await);
        assert_eq!(
            pulls(&fake),
            1,
            "an unprepared image pulls under its policy"
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
            json!({"want":{"state":"running", "rotation":"pending", "container":{"existing":{"id":id}}}}),
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

    /// The floor of one admission turn: a Stopped want on an exited Container takes the
    /// exclusive turn through `finish_mutation` for a Docker stop that changes nothing.
    #[tokio::test]
    #[ignore = "perf probe: prints the p95 of 200 admitted stops"]
    async fn admission_turn_floor() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-turn-floor").await;
        let id = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        set_state(&fake, &id, json!({"Status":"exited", "ExitCode":0}));
        let stopped = json!({"want":{"state":"stopped", "container":{"id":id}, "drain":0}});
        let mut took = Vec::with_capacity(200);
        for _ in 0..200 {
            let started = std::time::Instant::now();
            present(&ensure(&service, stopped.clone()).await);
            took.push(started.elapsed());
        }
        took.sort_unstable();
        let p95 = *took.get(took.len() * 95 / 100 - 1).unwrap();
        println!("admission_turn_floor p95 {:.3} ms", p95.as_secs_f64() * 1e3);
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
        let hook = json!({"want":{"state":"running", "rotation":"pending", "container":{"new":{
            "slot":{"namespace":"app", "service":"api", "kind":"pre_deploy_hook", "creation_key":"h1"},
            "resolved_spec":spec("example.test/api:1"), "gen":1}}}});
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
                spec.container
                    .environment
                    .get("TOKEN")
                    .and_then(ployz_core::EnvValue::text),
                Some("<redacted>")
            );
            assert_eq!(
                spec.pre_deploy
                    .as_ref()
                    .unwrap()
                    .environment
                    .get("DATABASE_URL")
                    .and_then(ployz_core::EnvValue::text),
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
    #[expect(clippy::indexing_slicing, reason = "JSON fixture edits")]
    async fn a_request_past_tonic_s_default_4_mib_still_creates_the_container() {
        use std::os::unix::fs::MetadataExt;
        let (data_dir, _store, service, _fake) = fake_docker_service("ployzd-ensure-large").await;
        let owner = std::fs::metadata(&data_dir).unwrap();
        let mut spec = spec("example.test/api:1");
        spec["configs"] = json!([{"name":"bulk/a", "content":vec![b'x'; 2 * 1024 * 1024]}]);
        spec["container"]["config_mounts"] = json!([{
            "config_name":"bulk/a", "target":"/etc/bulk/a", "uid":owner.uid(), "gid":owner.gid()
        }]);
        let request =
            op::EnsureContainer::into_request(serde_json::from_value(run_new("k1", spec)).unwrap())
                .encode()
                .unwrap();
        assert!(request.json.len() > 4 * 1024 * 1024);
        let api = crate::machine_api::MachineApi::from_local(service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            Server::builder().serve_with_incoming(api, TcpListenerStream::new(listener)),
        );
        let mut client = MachineRpcClient::connect(format!("http://{address}"))
            .await
            .unwrap()
            .max_decoding_message_size(ployz_core::RUNTIME_WATCH_MESSAGE_SIZE_LIMIT);

        let reply = client
            .ensure_container(Request::new(request))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::EnsureContainer>()
            .unwrap();

        present(&reply);
        server.abort();
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

        let unknown = ContainerId::parse("a".repeat(64)).unwrap();
        for (id, from_operator, code) in [
            (unknown, false, Code::NotFound),
            (stray, false, Code::InvalidArgument),
            (unknown, true, Code::PermissionDenied),
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
            let mut request = Request::new(tokio_stream::iter([config]));
            if from_operator {
                request
                    .metadata_mut()
                    .insert(ployz_core::OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
            }
            let status = client
                .exec(request)
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

    mod device_enforcement {
        //! The Operator restart gate under Device enforcement: what an approved Device's
        //! approval admits, what it refuses, and that a used one stays used across a
        //! daemon restart.

        use ployz_core::{
            DEPLOY_APPROVAL_CAPABILITY, DeviceKey, Namespace, OPERATOR_SCOPE_HEADER, OperationId,
            RESTART_APPROVAL_CAPABILITY, SealingPublicKey, ServiceName,
            SetDeviceEnforcementRequest,
            authority::{
                ApprovedService, DeployApproval, PayloadDigest, ProposalApproval, ProposalRef,
                ProposalStep, RestartApproval, Signed, Signer, StartingSet, StepDigest, Timestamp,
                WorkflowKind, canonical,
            },
            rpc::status_details,
        };
        use tonic::Status;

        use super::*;

        struct Enforced {
            data_dir: std::path::PathBuf,
            store: RecordOwner,
            service: MachineService,
            fake: FakeDocker,
            runtime: crate::docker::ContainerRuntime,
            signer: Signer,
            cluster: SealingPublicKey,
            /// The key the runtime opens Sealed values with, when the Machine holds one.
            sealed: Option<crate::sealed_env::test_support::Cluster>,
        }

        async fn enforced(prefix: &str) -> Enforced {
            enforce(fake_docker_service_keeping_runtime(prefix).await, None).await
        }

        /// An enforcing Machine whose runtime opens what its test Cluster seals.
        async fn enforced_sealing(prefix: &str) -> Enforced {
            let (data_dir, store, service, fake, runtime, cluster) =
                fake_docker_service_sealing(prefix).await;
            enforce((data_dir, store, service, fake, runtime), Some(cluster)).await
        }

        async fn enforce(
            (data_dir, store, service, fake, runtime): (
                std::path::PathBuf,
                RecordOwner,
                MachineService,
                FakeDocker,
                crate::docker::ContainerRuntime,
            ),
            sealed: Option<crate::sealed_env::test_support::Cluster>,
        ) -> Enforced {
            let signer = Signer::from_seed(&[7; 32]);
            let device = DeviceKey::from(signer.public());
            store
                .mutate(move |store| {
                    store.approve_device(device).unwrap();
                    store.set_device_enforcement(true, Vec::new()).unwrap();
                })
                .await
                .unwrap();
            let cluster = crate::sealing_key::SealingKey::load(&data_dir)
                .unwrap()
                .unwrap()
                .public_key();
            Enforced {
                data_dir,
                store,
                service,
                fake,
                runtime,
                signer,
                cluster,
                sealed,
            }
        }

        impl Enforced {
            fn approval(&self, container: &ContainerId) -> RestartApproval {
                RestartApproval {
                    cluster: self.cluster,
                    operation: OperationId::random(),
                    kind: WorkflowKind::Restart,
                    namespace: Namespace::parse("app").unwrap(),
                    service: ServiceName::parse("api").unwrap(),
                    machines: vec![self.service.local.record().id()],
                    container: *container,
                    issued_at: Timestamp::now(),
                    expires_at: Timestamp::now().plus(60_000),
                    signer: ployz_core::authority::SignerKey::Device(self.signer.public().into()),
                    proposal: None,
                }
            }

            fn signed(&self, approval: &RestartApproval) -> Signed<RestartApproval> {
                Signed::sign(approval, &self.signer).unwrap()
            }

            /// The service a restarted daemon builds over the same record and Docker.
            fn restarted(&self) -> MachineService {
                let mut service = MachineService::with_cluster(self.store.clone(), None)
                    .with_optional_containers(Some(self.runtime.clone()));
                service.local = service.local.with_zpool("true");
                service
            }
        }

        fn stopped(id: &ContainerId) -> Value {
            json!({"state":"stopped", "container":{"id":id}, "drain":0})
        }

        fn started(id: &ContainerId) -> Value {
            json!({"state":"running", "rotation":"pending", "container":{"existing":{"id":id}}})
        }

        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn operator_ensure(
            service: &MachineService,
            want: Value,
            approval: Option<&Signed<RestartApproval>>,
        ) -> Result<EnsuredContainer, Status> {
            let mut body = json!({"want": want});
            if let Some(approval) = approval {
                body["approval"] = serde_json::to_value(approval).unwrap();
            }
            let mut request = Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(body).unwrap())
                    .encode()
                    .unwrap(),
            );
            request
                .metadata_mut()
                .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
            let response = service
                .ensure_container(request)
                .await?
                .into_inner()
                .decode_response()
                .unwrap();
            let shown = format!("{response:?}");
            Ok(response
                .decode::<op::EnsureContainer>()
                .unwrap_or_else(|error| panic!("{error}: {shown}")))
        }

        #[track_caller]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
        fn refused(result: Result<EnsuredContainer, Status>, reason: &str) {
            let status = result.expect_err("the restart step must be refused");
            assert_eq!(
                status.code(),
                Code::PermissionDenied,
                "{}",
                status.message()
            );
            let shown = format!(
                "{} {}",
                status.message(),
                String::from_utf8_lossy(status.details())
            );
            assert!(shown.contains(reason), "{shown}");
            assert_eq!(
                status_details(status.details()).unwrap()["capability"],
                json!(RESTART_APPROVAL_CAPABILITY)
            );
        }

        /// Connects to a Machine's local socket, the way a Full caller on the Machine does.
        #[derive(Clone)]
        struct LocalSocket(std::path::PathBuf);

        impl tower_service::Service<tonic::transport::Uri> for LocalSocket {
            type Response = hyper_util::rt::TokioIo<tokio::net::UnixStream>;
            type Error = std::io::Error;
            type Future = std::pin::Pin<
                Box<dyn std::future::Future<Output = std::io::Result<Self::Response>> + Send>,
            >;

            fn poll_ready(
                &mut self,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, _: tonic::transport::Uri) -> Self::Future {
                let path = self.0.clone();
                Box::pin(async move {
                    tokio::net::UnixStream::connect(path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                })
            }
        }

        /// A socket path of its own under `dir`, short enough for a Unix socket.
        fn socket_in(dir: &std::path::Path) -> std::path::PathBuf {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            dir.join(format!("m{next}.sock"))
        }

        /// A Machine's gRPC front over its local socket. A request with no Operator header
        /// reaches it as a Full caller, even under enforcement, and one with it as Cloud does.
        async fn served(
            service: &MachineService,
            socket: &std::path::Path,
        ) -> (
            MachineRpcClient<tonic::transport::Channel>,
            tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
        ) {
            let api = crate::machine_api::MachineApi::from_local(service.clone());
            let listener = tokio::net::UnixListener::bind(socket).unwrap();
            let server = tokio::spawn(Server::builder().serve_with_incoming(
                api,
                tokio_stream::wrappers::UnixListenerStream::new(listener),
            ));
            let channel = tonic::transport::Endpoint::try_from("http://machine.invalid")
                .unwrap()
                .connect_with_connector(LocalSocket(socket.to_owned()))
                .await
                .unwrap();
            (MachineRpcClient::new(channel), server)
        }

        fn as_operator<T>(mut request: Request<T>, operator: bool) -> Request<T> {
            if operator {
                request
                    .metadata_mut()
                    .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
            }
            request
        }

        /// What a Build whose start frame is not one says, when it was admitted at all.
        async fn build_answer(
            service: &MachineService,
            dir: &std::path::Path,
            operator: bool,
        ) -> Result<String, Status> {
            let (mut client, server) = served(service, &socket_in(dir)).await;
            let garbage = ployz_core::OpaquePayload::new(b"{}".to_vec());
            let request = as_operator(Request::new(tokio_stream::iter([garbage])), operator);
            let answer = async {
                let mut events = client.build(request).await?.into_inner();
                let payload = events.message().await?.expect("a terminal event");
                match ployz_build::remote::decode::<ployz_build::remote::Event>(&payload).unwrap() {
                    ployz_build::remote::Event::Finished(
                        ployz_build::remote::Outcome::Failed { message, .. },
                    ) => Ok(message),
                    other @ (ployz_build::remote::Event::Admitted { .. }
                    | ployz_build::remote::Event::Progress(_)
                    | ployz_build::remote::Event::Finished(_)) => {
                        panic!("expected a failed start, got {other:?}")
                    }
                }
            }
            .await;
            server.abort();
            answer
        }

        /// A Sealed record this Machine's Sealing Key cannot open: any attempt to open it
        /// answers a Conflict, so an open shows as a different error than a refusal.
        fn foreign_sealed() -> ployz_core::SealedRecord {
            let other = crate::sealed_env::test_support::Cluster::new();
            let env = BTreeMap::from([(
                "NPM_TOKEN".to_owned(),
                ployz_core::EnvValue::Sealed(other.seal(
                    "NPM_TOKEN",
                    ployz_core::SealedScope::Shared,
                    "marker-sealed-0123456789",
                )),
            )]);
            other.open(env).unwrap().record.unwrap()
        }

        async fn mint(
            service: &MachineService,
            operator: bool,
        ) -> Result<ployz_core::RpcResponse, Status> {
            let holder = hex::encode([3u8; 32]);
            let request = op::MintBuildGrant::into_request(ployz_core::MintBuildGrantRequest {
                repository: ployz_core::BuildGrantRepository::parse("ployz-build/web").unwrap(),
                holder: Some(ployz_core::BuildGrantId::parse(holder).unwrap()),
                sealed: Some(foreign_sealed()),
            })
            .encode()
            .unwrap();
            Ok(service
                .mint_build_grant(as_operator(Request::new(request), operator))
                .await?
                .into_inner()
                .decode_response()
                .unwrap())
        }

        /// The Conflict `Mint::check` answers once it tries to open the record.
        #[track_caller]
        fn opened_and_failed(response: &ployz_core::RpcResponse) {
            let RpcResponseBody::Error(error) = &response.body else {
                panic!("the open must fail: {response:?}");
            };
            assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
        }

        #[track_caller]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
        fn refused_build(status: &Status, capability: &str) {
            assert_eq!(
                status.code(),
                Code::PermissionDenied,
                "{}",
                status.message()
            );
            assert_eq!(
                status.message(),
                "This server runs only builds an approved device starts."
            );
            let details = status_details(status.details()).unwrap();
            assert_eq!(details["capability"], json!(capability));
        }

        #[tokio::test]
        async fn an_operator_mint_is_refused_under_enforcement_before_the_sealed_record_opens() {
            let fixture = enforced_sealing("ployzd-enforce-mint").await;

            let status = mint(&fixture.service, true)
                .await
                .expect_err("an Operator's mint must be refused");
            refused_build(&status, ployz_core::MINT_BUILD_GRANT_CAPABILITY);

            // The same request from a Full caller does reach the record and fails to open it,
            // so the refusal above came from the gate and not from the open.
            opened_and_failed(&mint(&fixture.service, false).await.unwrap());
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_mint_is_admitted_with_enforcement_off() {
            let (data_dir, _store, service, _fake, _runtime, _cluster) =
                fake_docker_service_sealing("ployzd-enforce-off-mint").await;

            opened_and_failed(&mint(&service, true).await.unwrap());
            opened_and_failed(&mint(&service, false).await.unwrap());
            std::fs::remove_dir_all(data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_build_is_refused_under_enforcement_before_its_stream_is_read() {
            let fixture = enforced("ployzd-enforce-build").await;
            let (mut client, server) =
                served(&fixture.service, &socket_in(&fixture.data_dir)).await;

            // A request stream that never yields: only a refusal that does not read it answers.
            let silent = tokio_stream::pending::<ployz_core::OpaquePayload>();
            let status = tokio::time::timeout(
                Duration::from_secs(5),
                client.build(as_operator(Request::new(silent), true)),
            )
            .await
            .expect("the refusal does not wait for a start frame")
            .expect_err("an Operator's build must be refused");
            refused_build(&status, ployz_core::BUILD_CAPABILITY);

            assert_eq!(
                build_answer(&fixture.service, &fixture.data_dir, true)
                    .await
                    .expect_err("an Operator's build is refused whatever it sends")
                    .code(),
                Code::PermissionDenied
            );
            server.abort();
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_full_build_is_admitted_under_enforcement() {
            let fixture = enforced("ployzd-enforce-build-full").await;

            assert_eq!(
                build_answer(&fixture.service, &fixture.data_dir, false)
                    .await
                    .unwrap(),
                "expected a valid Build start frame"
            );
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn builds_are_admitted_from_both_callers_with_enforcement_off() {
            let (data_dir, _store, service, _fake) =
                fake_docker_service("ployzd-enforce-off-build").await;

            for operator in [true, false] {
                assert_eq!(
                    build_answer(&service, &data_dir, operator).await.unwrap(),
                    "expected a valid Build start frame",
                    "operator: {operator}"
                );
            }
            std::fs::remove_dir_all(data_dir).unwrap();
        }

        #[tokio::test]
        async fn enforcement_off_leaves_an_unsigned_operator_restart_as_it_was() {
            let (data_dir, _store, service, fake) = fake_docker_service("ployzd-enforce-off").await;
            let id = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let seen = requests(&fake).len();

            let reply = operator_ensure(&service, stopped(&id), None).await.unwrap();

            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            assert_eq!(
                ends_with(&requests_since(&fake, seen), Method::POST, "/stop").count(),
                1
            );
            std::fs::remove_dir_all(data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_cannot_route_out_then_remove_a_serving_container() {
            let fixture = enforced("ployzd-enforce-out-gone").await;
            let service = &fixture.service;
            let created = ensure(service, run_new("k1", spec("example.test/api:1"))).await;
            let id = present(&created).container_id;
            fixture
                .runtime
                .rotation_turn(&id)
                .await
                .unwrap()
                .write(&ployz_core::CopyRotation::In)
                .await
                .unwrap();
            let seen = requests(&fixture.fake).len();
            for want in [
                json!({"state":"out", "container":{"id":id}, "allow_empty":true, "expect":null}),
                json!({"state":"gone", "container":{"id":id}, "drain":0}),
            ] {
                refused(
                    operator_ensure(service, want, None).await,
                    "carries no signature",
                );
            }
            let observed = fixture
                .runtime
                .inspect_managed(&id, &service.local.record().id())
                .await
                .unwrap();
            assert_eq!(observed.rotation(), &ployz_core::CopyRotation::In);
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/stop").count(),
                0
            );
            assert_eq!(
                requests_since(&fixture.fake, seen)
                    .iter()
                    .filter(|(method, _)| *method == Method::DELETE)
                    .count(),
                0
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_restart_approval_cannot_route_out_then_remove_a_serving_container() {
            let fixture = enforced("ployzd-enforce-signed-out-gone").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            fixture
                .runtime
                .rotation_turn(&id)
                .await
                .unwrap()
                .write(&ployz_core::CopyRotation::In)
                .await
                .unwrap();
            let signed = fixture.signed(&fixture.approval(&id));
            for want in [
                json!({"state":"out", "container":{"id":id}, "allow_empty":true, "expect":null}),
                json!({"state":"gone", "container":{"id":id}, "drain":0}),
            ] {
                refused(
                    operator_ensure(service, want, Some(&signed)).await,
                    "names a workflow",
                );
            }
            let observed = fixture
                .runtime
                .inspect_managed(&id, &service.local.record().id())
                .await
                .unwrap();
            assert_eq!(observed.rotation(), &ployz_core::CopyRotation::In);
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_start_or_restart_approval_naming_a_completed_hook_never_starts_it() {
            let fixture = enforced("ployzd-enforce-hook").await;
            let service = &fixture.service;
            let hook = json!({"want":{"state":"running", "rotation":"pending", "container":{"new":{
                "slot":{"namespace":"app", "service":"api", "kind":"pre_deploy_hook", "creation_key":"h1"},
                "resolved_spec":spec("example.test/api:1"), "gen":1}}}});
            let id = present(&ensure(service, hook).await).container_id;
            set_state(&fixture.fake, &id, json!({"Status":"exited", "ExitCode":0}));
            let seen = requests(&fixture.fake).len();
            for kind in [WorkflowKind::Start, WorkflowKind::Restart] {
                let signed = fixture.signed(&RestartApproval {
                    kind,
                    ..fixture.approval(&id)
                });
                refused(
                    operator_ensure(service, started(&id), Some(&signed)).await,
                    "names a workflow",
                );
            }
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0,
                "the hook stays exited"
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn admission_pins_the_verified_container_when_its_slot_is_replaced() {
            let fixture = enforced("ployzd-enforce-pinned-slot").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let signed = fixture.signed(&fixture.approval(&id));
            let want = serde_json::from_value(
                json!({"state":"stopped", "container":{"key":slot("k1")}, "drain":0}),
            )
            .unwrap();
            let admitted = service
                .local
                .admit_operator_restart(want, Some(&signed), None)
                .await
                .unwrap();
            ensure(
                service,
                json!({"want":{"state":"gone", "container":{"id":id}, "drain":0}}),
            )
            .await;
            let replacement =
                present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                    .container_id;
            assert_ne!(id, replacement);
            let seen = requests(&fixture.fake).len();
            let _ = service.local.ensure_container(admitted).await;
            refused(
                operator_ensure(
                    service,
                    json!({"state":"stopped", "container":{"key":slot("k1")}, "drain":0}),
                    Some(&signed),
                )
                .await,
                "names a Container",
            );
            let observed = fixture
                .runtime
                .inspect_managed(&replacement, &service.local.record().id())
                .await
                .unwrap();
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/stop").count(),
                0
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_empty_slot_at_admission_cannot_authorize_a_later_container() {
            let fixture = enforced("ployzd-enforce-empty-slot").await;
            let service = &fixture.service;
            let signed =
                fixture.signed(&fixture.approval(&ContainerId::parse("a".repeat(64)).unwrap()));
            for want in [
                json!({"state":"stopped", "container":{"key":slot("k1")}, "drain":0}),
                json!({"state":"gone", "container":{"key":slot("k1")}, "drain":0}),
            ] {
                refused(
                    operator_ensure(service, want, Some(&signed)).await,
                    "names a Container",
                );
            }
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let observed = fixture
                .runtime
                .inspect_managed(&id, &service.local.record().id())
                .await
                .unwrap();
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_cannot_adopt_an_exited_container_with_a_new_creation() {
            let fixture = enforced("ployzd-enforce-adopt").await;
            let service = &fixture.service;
            let new = run_new("k1", spec("example.test/api:1"));
            let id = present(&ensure(service, new.clone()).await).container_id;
            ensure(service, json!({"want": stopped(&id)})).await;
            let seen = requests(&fixture.fake).len();
            deploy_refused(
                operator_ensure(service, new.get("want").unwrap().clone(), None).await,
                "runs only deploys an approved device signed",
            );
            let signed = fixture.signed(&fixture.approval(&id));
            deploy_refused(
                operator_ensure(service, new.get("want").unwrap().clone(), Some(&signed)).await,
                "runs only deploys an approved device signed",
            );
            let observed = fixture
                .runtime
                .inspect_managed(&id, &service.local.record().id())
                .await
                .unwrap();
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_restart_passes_only_with_an_approved_devices_approval_of_it() {
            let fixture = enforced("ployzd-enforce-on").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let seen = requests(&fixture.fake).len();
            let good = fixture.approval(&id);

            refused(
                operator_ensure(service, stopped(&id), None).await,
                "carries no signature",
            );
            let stranger = Signer::from_seed(&[9; 32]);
            let forged = Signed::sign(
                &RestartApproval {
                    signer: ployz_core::authority::SignerKey::Device(stranger.public().into()),
                    ..good.clone()
                },
                &stranger,
            )
            .unwrap();
            refused(
                operator_ensure(service, stopped(&id), Some(&forged)).await,
                "is not approved on this Machine",
            );
            let mut tampered = fixture.signed(&good);
            tampered.body = tampered.body.replace("\"api\"", "\"web\"");
            refused(
                operator_ensure(service, stopped(&id), Some(&tampered)).await,
                "does not verify",
            );
            let other_service = fixture.signed(&RestartApproval {
                service: ServiceName::parse("web").unwrap(),
                ..good.clone()
            });
            refused(
                operator_ensure(service, stopped(&id), Some(&other_service)).await,
                "names a Service",
            );
            let elsewhere = fixture.signed(&RestartApproval {
                machines: vec![MachineId::random()],
                ..good.clone()
            });
            refused(
                operator_ensure(service, stopped(&id), Some(&elsewhere)).await,
                "names Machines",
            );
            let expired = fixture.signed(&RestartApproval {
                issued_at: Timestamp::from_millis(0).unwrap(),
                expires_at: Timestamp::from_millis(1).unwrap(),
                ..good.clone()
            });
            refused(
                operator_ensure(service, stopped(&id), Some(&expired)).await,
                "expired",
            );
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/stop").count(),
                0,
                "no refused step reached Docker"
            );

            let reply = operator_ensure(service, stopped(&id), Some(&fixture.signed(&good)))
                .await
                .unwrap();

            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/stop").count(),
                1
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_used_approval_runs_each_step_once_even_after_the_daemon_restarts() {
            let fixture = enforced("ployzd-enforce-replay").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let approval = fixture.signed(&fixture.approval(&id));

            operator_ensure(service, stopped(&id), Some(&approval))
                .await
                .unwrap();
            refused(
                operator_ensure(service, stopped(&id), Some(&approval)).await,
                "already ran this step",
            );
            let reply = operator_ensure(service, started(&id), Some(&approval))
                .await
                .unwrap();
            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            refused(
                operator_ensure(service, started(&id), Some(&approval)).await,
                "already ran this step",
            );

            let restarted = fixture.restarted();
            refused(
                operator_ensure(&restarted, stopped(&id), Some(&approval)).await,
                "already ran this step",
            );
            refused(
                operator_ensure(&restarted, started(&id), Some(&approval)).await,
                "already ran this step",
            );
            let fresh = fixture.signed(&fixture.approval(&id));
            let reply = operator_ensure(&restarted, stopped(&id), Some(&fresh))
                .await
                .unwrap();
            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        fn browser() -> Signer {
            Signer::from_seed(&[8; 32])
        }

        /// A browser key's restart approval under a session grant from a passkey this
        /// Machine trusts, as the dashboard sends it.
        fn browser_approval(fixture: &Enforced, id: &ContainerId) -> Signed<RestartApproval> {
            use ployz_core::authority::{ActionFamily, GrantBody, SignerKey, session::Delegation};

            let approval = RestartApproval {
                signer: SignerKey::Browser {
                    browser: browser().public(),
                },
                ..fixture.approval(id)
            };
            let grant = session_grant(&GrantBody {
                browser_key: browser().public(),
                cluster: fixture.cluster,
                environment: approval.namespace.clone(),
                environment_id: uuid::Uuid::nil(),
                machines: approval.machines.clone(),
                families: vec![ActionFamily::Restart],
                issued_at: approval.issued_at,
                expires_at: approval.issued_at.plus(60 * 60_000),
                delegation: Delegation::None,
            });
            Signed::sign_under(&approval, &browser(), grant).unwrap()
        }

        /// The trusted passkey's answer to a session prompt for `grant`.
        fn session_grant(
            grant: &ployz_core::authority::GrantBody,
        ) -> ployz_core::authority::SessionGrant {
            use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
            use ed25519_dalek::Signer as _;
            use ployz_core::authority::{
                GrantBody, PasskeyAssertion, SessionGrant, trusted_key::Base64Url,
            };
            use sha2::{Digest, Sha256};

            let authenticator = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
            let credential = Base64Url::new(vec![5, 1, 2, 3]);
            let body = ployz_core::authority::canonical::to_canonical(grant).unwrap();
            let client_data = json!({
                "type": "webauthn.get",
                "challenge": URL_SAFE_NO_PAD.encode(GrantBody::challenge(&body)),
                "origin": "https://ployz.cloud",
            })
            .to_string()
            .into_bytes();
            let mut authenticator_data = Sha256::digest(b"ployz.cloud").to_vec();
            authenticator_data.extend_from_slice(&[0x05, 0, 0, 0, 1]);
            let mut answered = authenticator_data.clone();
            answered.extend_from_slice(&Sha256::digest(&client_data));
            SessionGrant {
                body,
                assertion: PasskeyAssertion {
                    credential_id: credential.try_into().unwrap(),
                    authenticator_data: Base64Url::new(authenticator_data),
                    client_data_json: Base64Url::new(client_data),
                    signature: Base64Url::new(authenticator.sign(&answered).to_bytes().to_vec()),
                },
            }
        }

        async fn trust_passkey(fixture: &Enforced) {
            use ployz_core::authority::{
                Passkey, PasskeyAccount, PasskeyApprover, PasskeyPublicKey, TrustedPasskey,
                trusted_key::{Base64Url, PasskeyAlgorithm, RelyingParty},
            };
            let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]).verifying_key();
            let trusted = TrustedPasskey {
                passkey: Passkey {
                    credential_id: Base64Url::new(vec![5, 1, 2, 3]).try_into().unwrap(),
                    public_key: PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes())
                        .unwrap(),
                    account: PasskeyAccount::parse("ada@example.com").unwrap(),
                    relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap())
                        .unwrap(),
                },
                approved_by: PasskeyApprover::Server,
            };
            fixture
                .store
                .mutate(move |store| {
                    store.approve_passkey(trusted).unwrap();
                })
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn a_browser_approval_under_a_passkey_grant_restarts_once_and_its_replay_is_refused()
        {
            let fixture = enforced("ployzd-enforce-browser-replay").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let approval = browser_approval(&fixture, &id);
            refused(
                operator_ensure(service, stopped(&id), Some(&approval)).await,
                "passkey is not approved",
            );

            trust_passkey(&fixture).await;
            operator_ensure(service, stopped(&id), Some(&approval))
                .await
                .unwrap();
            refused(
                operator_ensure(service, stopped(&id), Some(&approval)).await,
                "already ran this step",
            );
            let reply = operator_ensure(service, started(&id), Some(&approval))
                .await
                .unwrap();
            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            refused(
                operator_ensure(&fixture.restarted(), started(&id), Some(&approval)).await,
                "already ran this step",
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_proposal_step_runs_once_in_its_bundle_and_a_swapped_step_is_refused() {
            let fixture = enforced("ployzd-enforce-proposal").await;
            let service = &fixture.service;
            let first = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let second = present(&ensure(service, run_new("k2", spec("example.test/api:1"))).await)
                .container_id;
            let bundle_operation = OperationId::random();
            let steps = [
                RestartApproval {
                    kind: WorkflowKind::Stop,
                    proposal: Some(ProposalRef {
                        operation: bundle_operation,
                        index: 0,
                    }),
                    ..fixture.approval(&first)
                },
                RestartApproval {
                    kind: WorkflowKind::Stop,
                    proposal: Some(ProposalRef {
                        operation: bundle_operation,
                        index: 1,
                    }),
                    ..fixture.approval(&second)
                },
            ];
            let plan = ProposalApproval {
                cluster: fixture.cluster,
                environment: Namespace::parse("app").unwrap(),
                operation: bundle_operation,
                steps: steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| ProposalStep {
                        index: u32::try_from(index).unwrap(),
                        kind: step.kind.into(),
                        digest: StepDigest::of(&canonical::to_canonical(step).unwrap()),
                    })
                    .collect(),
                issued_at: steps.first().unwrap().issued_at,
                expires_at: steps.first().unwrap().expires_at,
                signer: ployz_core::authority::SignerKey::Device(fixture.signer.public().into()),
            };
            let bundle = Signed::sign(&plan, &fixture.signer).unwrap();
            let [stop_first, stop_second] = steps;
            let step = |body: &RestartApproval| Signed::proposed(body, bundle.clone()).unwrap();

            refused(
                operator_ensure(service, stopped(&first), Some(&step(&stop_second))).await,
                "names a Container",
            );
            let swapped = RestartApproval {
                proposal: Some(ProposalRef {
                    operation: bundle_operation,
                    index: 1,
                }),
                ..stop_first.clone()
            };
            refused(
                operator_ensure(service, stopped(&first), Some(&step(&swapped))).await,
                "names a step",
            );
            operator_ensure(service, stopped(&first), Some(&step(&stop_first)))
                .await
                .unwrap();
            refused(
                operator_ensure(service, stopped(&first), Some(&step(&stop_first))).await,
                "already ran this step",
            );
            operator_ensure(service, stopped(&second), Some(&step(&stop_second)))
                .await
                .unwrap();
            refused(
                operator_ensure(
                    &fixture.restarted(),
                    stopped(&second),
                    Some(&step(&stop_second)),
                )
                .await,
                "already ran this step",
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_captured_approval_cannot_stop_another_copy() {
            let fixture = enforced("ployzd-enforce-bound-copy").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec("example.test/api:1"))).await)
                .container_id;
            let other = present(&ensure(service, run_new("k2", spec("example.test/api:1"))).await)
                .container_id;
            let signed = fixture.signed(&fixture.approval(&id));
            refused(
                operator_ensure(service, stopped(&other), Some(&signed)).await,
                "names a Container",
            );
            let observed = fixture
                .runtime
                .inspect_managed(&other, &service.local.record().id())
                .await
                .unwrap();
            assert!(matches!(
                observed.runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            operator_ensure(service, stopped(&id), Some(&signed))
                .await
                .unwrap();
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn an_operator_cannot_change_device_enforcement_on_the_local_machine() {
            let fixture = enforced("ployzd-enforce-operator-switch").await;
            fixture
                .store
                .mutate(|store| {
                    store
                        .stage_management_client(
                            ployz_core::ManagementClientLabel::parse("cloud").unwrap(),
                            [19; 32],
                        )
                        .unwrap();
                    store.activate_management_client([19; 32]).unwrap();
                })
                .await
                .unwrap();
            let operator = fixture
                .service
                .local
                .clone()
                .with_management_client([19; 32]);
            assert!(matches!(
                operator
                    .set_device_enforcement(SetDeviceEnforcementRequest {
                        enforced: false,
                        starting_set: None,
                    })
                    .await,
                Err(crate::machine::LocalMachineError::NeedsFullScope)
            ));
            assert!(fixture.service.local.record().device_enforcement());
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_full_caller_restarts_without_an_approval() {
            let fixture = enforced("ployzd-enforce-full").await;
            let id =
                present(&ensure(&fixture.service, run_new("k1", spec("example.test/api:1"))).await)
                    .container_id;

            let reply = ensure(&fixture.service, json!({"want": stopped(&id)})).await;

            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn the_switch_answers_its_new_state_and_the_record_keeps_it() {
            let (data_dir, store, service, _fake) =
                fake_docker_service("ployzd-enforce-switch").await;
            let signer = Signer::from_seed(&[7; 32]);
            let device = DeviceKey::from(signer.public());
            store
                .mutate(move |store| store.approve_device(device).unwrap())
                .await
                .unwrap();
            let issued_at = Timestamp::now();
            let set = StartingSet {
                operation: OperationId::random(),
                members: vec![service.local.record().machine().unwrap().into()],
                issued_at,
                expires_at: issued_at.plus(60_000),
                signer: device,
            };
            let starting_set = Signed::sign(&set, &signer).unwrap();
            for enforced in [true, false] {
                let reply = service
                    .set_device_enforcement(Request::new(
                        op::SetDeviceEnforcement::into_request(SetDeviceEnforcementRequest {
                            enforced,
                            starting_set: enforced.then(|| starting_set.clone()),
                        })
                        .encode()
                        .unwrap(),
                    ))
                    .await
                    .unwrap()
                    .into_inner()
                    .decode_response()
                    .unwrap()
                    .decode::<op::SetDeviceEnforcement>()
                    .unwrap();
                assert_eq!(reply.enforced, enforced);
                assert_eq!(service.local.record().device_enforcement(), enforced);
            }
            std::fs::remove_dir_all(data_dir).unwrap();
        }

        const PINNED: &str = "example.test/api@sha256:00000000000000000000000000000000000000000000000000000000000000aa";

        impl Enforced {
            /// A deploy approval of `spec` for Service `api` in Namespace `app` on this Machine.
            fn deploy(&self, spec: &Value) -> Signed<DeployApproval> {
                let approval = self.deploy_body(
                    &[spec],
                    ployz_core::authority::SignerKey::Device(self.signer.public().into()),
                );
                Signed::sign(&approval, &self.signer).unwrap()
            }

            /// A deploy approval of every Service in `specs`, each named by its spec, signed
            /// by the browser key under a grant for Environment `environment_id`.
            fn browser_deploy(
                &self,
                environment_id: uuid::Uuid,
                specs: &[&Value],
            ) -> Signed<DeployApproval> {
                use ployz_core::authority::{
                    ActionFamily, GrantBody, SignerKey, session::Delegation,
                };
                let approval = self.deploy_body(
                    specs,
                    SignerKey::Browser {
                        browser: browser().public(),
                    },
                );
                let grant = session_grant(&GrantBody {
                    browser_key: browser().public(),
                    cluster: self.cluster,
                    environment: approval.namespace.clone(),
                    environment_id,
                    machines: approval.machines.clone(),
                    families: vec![ActionFamily::Deploy],
                    issued_at: approval.issued_at,
                    expires_at: approval.issued_at.plus(60 * 60_000),
                    delegation: Delegation::None,
                });
                Signed::sign_under(&approval, &browser(), grant).unwrap()
            }

            fn deploy_body(
                &self,
                specs: &[&Value],
                signer: ployz_core::authority::SignerKey,
            ) -> DeployApproval {
                let namespace = Namespace::parse("app").unwrap();
                let mut services: Vec<ApprovedService> = specs
                    .iter()
                    .map(|spec| {
                        let resolved: ployz_core::ResolvedServiceSpec =
                            serde_json::from_value((*spec).clone()).unwrap();
                        ApprovedService {
                            digest: PayloadDigest::of(&resolved.to_requested(), &namespace)
                                .unwrap(),
                            name: resolved.name,
                            image: None,
                        }
                    })
                    .collect();
                services.sort_by(|left, right| left.name.cmp(&right.name));
                let issued_at = Timestamp::now();
                DeployApproval {
                    cluster: self.cluster,
                    operation: OperationId::random(),
                    namespace,
                    machines: vec![self.service.local.record().id()],
                    services,
                    issued_at,
                    expires_at: issued_at.plus(60_000),
                    signer,
                    proposal: None,
                }
            }
        }

        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn deploy_ensure(
            service: &MachineService,
            want: Value,
            deploy: Option<&Signed<DeployApproval>>,
        ) -> Result<EnsuredContainer, Status> {
            let mut body = json!({"want": want});
            if let Some(deploy) = deploy {
                body["deploy"] = serde_json::to_value(deploy).unwrap();
            }
            let mut request = Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(body).unwrap())
                    .encode()
                    .unwrap(),
            );
            request
                .metadata_mut()
                .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
            let response = service
                .ensure_container(request)
                .await?
                .into_inner()
                .decode_response()
                .unwrap();
            Ok(response.decode::<op::EnsureContainer>().unwrap())
        }

        #[track_caller]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
        fn deploy_refused(result: Result<EnsuredContainer, Status>, reason: &str) {
            let status = result.expect_err("the creation must be refused");
            assert_eq!(
                status.code(),
                Code::PermissionDenied,
                "{}",
                status.message()
            );
            assert!(status.message().contains(reason), "{}", status.message());
            assert_eq!(
                status_details(status.details()).unwrap()["capability"],
                json!(DEPLOY_APPROVAL_CAPABILITY)
            );
        }

        fn new_want(kind: &str, key: &str, spec: &Value) -> Value {
            json!({"state":"running", "rotation":"pending", "container":{"new":{
                "slot":{"namespace":"app", "service":spec["name"], "kind":kind, "creation_key":key},
                "resolved_spec":spec, "gen":1}}})
        }

        fn creates(fake: &FakeDocker, since: usize) -> usize {
            ends_with(
                &requests_since(fake, since),
                Method::POST,
                "/containers/create",
            )
            .count()
        }

        #[tokio::test]
        async fn an_operator_creation_runs_only_the_approved_spec() {
            let fixture = enforced("ployzd-deploy-approval").await;
            let service = &fixture.service;
            let approved = spec(PINNED);
            let signed = fixture.deploy(&approved);
            let seen = requests(&fixture.fake).len();

            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &approved),
                    None,
                )
                .await,
                "runs only deploys an approved device signed",
            );
            let mut privileged = approved.clone();
            *privileged
                .pointer_mut("/container/privileged")
                .expect("the fixture names privileged") = json!(true);
            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &privileged),
                    Some(&signed),
                )
                .await,
                "settings differ from what the device approved",
            );
            let tagged = spec("example.test/api:1");
            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &tagged),
                    Some(&fixture.deploy(&tagged)),
                )
                .await,
                "runs only images pinned by digest",
            );
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing reached Docker");

            for key in ["k1", "k2"] {
                deploy_ensure(
                    service,
                    new_want("service_container", key, &approved),
                    Some(&signed),
                )
                .await
                .unwrap();
            }
            assert_eq!(
                creates(&fixture.fake, seen),
                2,
                "replicas repeat a Service Container"
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_hook_runs_once_per_approval_even_after_a_daemon_restart() {
            let fixture = enforced("ployzd-deploy-hook-once").await;
            let approved = spec(PINNED);
            let signed = fixture.deploy(&approved);
            deploy_ensure(
                &fixture.service,
                new_want("pre_deploy_hook", "h1", &approved),
                Some(&signed),
            )
            .await
            .unwrap();
            let seen = requests(&fixture.fake).len();
            for service in [&fixture.service, &fixture.restarted()] {
                deploy_refused(
                    deploy_ensure(
                        service,
                        new_want("pre_deploy_hook", "h2", &approved),
                        Some(&signed),
                    )
                    .await,
                    "already ran its pre-deploy command",
                );
            }
            assert_eq!(creates(&fixture.fake, seen), 0);
            let mut hookless = approved.clone();
            hookless.as_object_mut().unwrap().remove("pre_deploy");
            deploy_refused(
                deploy_ensure(
                    &fixture.service,
                    new_want("pre_deploy_hook", "h3", &hookless),
                    Some(&fixture.deploy(&hookless)),
                )
                .await,
                "approved no pre-deploy command",
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_deploy_approval_starts_only_a_copy_whose_spec_it_approved() {
            let fixture = enforced("ployzd-deploy-start").await;
            let service = &fixture.service;
            let mut planted = spec(PINNED);
            *planted
                .pointer_mut("/container/privileged")
                .expect("the fixture names privileged") = json!(true);
            let old = present(&ensure(service, run_new("k0", planted)).await).container_id;
            let approved =
                present(&ensure(service, run_new("k1", spec(PINNED))).await).container_id;
            for id in [&old, &approved] {
                ensure(service, json!({"want": stopped(id)})).await;
            }
            let signed = fixture.deploy(&spec(PINNED));
            let seen = requests(&fixture.fake).len();
            deploy_refused(
                deploy_ensure(service, started(&old), Some(&signed)).await,
                "settings differ from what the device approved",
            );
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0
            );
            // The old copy can still be retired under the same approval.
            deploy_ensure(service, stopped(&old), Some(&signed))
                .await
                .unwrap();
            let reply = deploy_ensure(service, started(&approved), Some(&signed))
                .await
                .unwrap();
            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Running { .. }
            ));
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_deploy_approval_never_starts_a_hook_again_but_adopts_a_resent_one() {
            let fixture = enforced("ployzd-deploy-hook-start").await;
            let approved = spec(PINNED);
            let signed = fixture.deploy(&approved);
            let hook = deploy_ensure(
                &fixture.service,
                new_want("pre_deploy_hook", "h1", &approved),
                Some(&signed),
            )
            .await
            .unwrap();
            let hook = present(&hook).container_id;
            let seen = requests(&fixture.fake).len();
            // A reply lost on the way back: the same slot again adopts the hook that ran.
            for service in [&fixture.service, &fixture.restarted()] {
                let again = deploy_ensure(
                    service,
                    new_want("pre_deploy_hook", "h1", &approved),
                    Some(&signed),
                )
                .await
                .unwrap();
                assert_eq!(present(&again).container_id, hook);
            }
            assert_eq!(creates(&fixture.fake, seen), 0);
            deploy_refused(
                deploy_ensure(&fixture.service, started(&hook), Some(&signed)).await,
                "already ran its pre-deploy command",
            );
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_removed_hook_is_never_made_again_from_its_slot() {
            let fixture = enforced("ployzd-deploy-hook-gone").await;
            let approved = spec(PINNED);
            let signed = fixture.deploy(&approved);
            let hook = deploy_ensure(
                &fixture.service,
                new_want("pre_deploy_hook", "h1", &approved),
                Some(&signed),
            )
            .await
            .unwrap();
            let hook = present(&hook).container_id;
            deploy_ensure(
                &fixture.service,
                json!({"state":"gone", "container":{"id":hook}, "drain":0}),
                Some(&signed),
            )
            .await
            .unwrap();
            let seen = requests(&fixture.fake).len();
            for service in [&fixture.service, &fixture.restarted()] {
                deploy_refused(
                    deploy_ensure(
                        service,
                        new_want("pre_deploy_hook", "h1", &approved),
                        Some(&signed),
                    )
                    .await,
                    "already ran its pre-deploy command",
                );
            }
            assert_eq!(creates(&fixture.fake, seen), 0);
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_hook_admitted_but_never_made_is_made_on_retry() {
            let fixture = enforced("ployzd-deploy-hook-retry").await;
            let approved = spec(PINNED);
            let signed = fixture.deploy(&approved);
            // Admission took the hook's place, and the create never happened.
            fixture
                .service
                .local
                .admit_operator_restart(
                    serde_json::from_value(new_want("pre_deploy_hook", "h1", &approved)).unwrap(),
                    None,
                    Some(&signed),
                )
                .await
                .unwrap();
            let seen = requests(&fixture.fake).len();
            deploy_ensure(
                &fixture.service,
                new_want("pre_deploy_hook", "h1", &approved),
                Some(&signed),
            )
            .await
            .unwrap();
            assert_eq!(creates(&fixture.fake, seen), 1);
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        /// The Environment the test Cluster's Sealed values were sealed for.
        const PRODUCTION: uuid::Uuid = uuid::Uuid::from_u128(1);
        /// Another Environment a session may be open in.
        const FIX_API: uuid::Uuid = uuid::Uuid::from_u128(9);
        const SECRET: &str = "hunter2-production";

        /// Service `name` at the pinned image. With `sealed`, its environment, or only its
        /// hook's, holds a value sealed for the test Cluster's production Environment.
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        fn sealed_spec(fixture: &Enforced, name: &str, sealed: Option<Sealed>) -> Value {
            let mut spec = spec(PINNED);
            spec["name"] = json!(name);
            spec["pre_deploy"] = json!({"command": ["migrate"]});
            let Some(sealed) = sealed else {
                return spec;
            };
            let cluster = fixture.sealed.as_ref().expect("a sealing fixture");
            let value = cluster.seal("TOKEN", ployz_core::SealedScope::Shared, SECRET);
            let environment = match sealed {
                Sealed::Container => &mut spec["container"]["environment"],
                Sealed::Hook => &mut spec["pre_deploy"]["environment"],
            };
            *environment = json!({"TOKEN": value});
            spec["container"]["sealed_identity"] = serde_json::to_value(&cluster.identity).unwrap();
            spec
        }

        #[derive(Clone, Copy)]
        enum Sealed {
            Container,
            Hook,
        }

        #[tokio::test]
        async fn a_browser_approval_opens_only_its_own_environments_sealed_values() {
            let fixture = enforced_sealing("ployzd-deploy-sealed-scope").await;
            trust_passkey(&fixture).await;
            let service = &fixture.service;
            let sealed = sealed_spec(&fixture, "api", Some(Sealed::Container));
            let seen = requests(&fixture.fake).len();
            let (log, guard) = crate::diag::test_support::capture();
            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &sealed),
                    Some(&fixture.browser_deploy(FIX_API, &[&sealed])),
                )
                .await,
                "sealed values belong to another environment",
            );
            drop(guard);
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing reached Docker");
            let log = log.text();
            assert!(
                log.contains("Sealed values belong to another Environment")
                    && log.contains("refused deploy")
                    && log.contains("service=api"),
                "{log}"
            );
            assert!(!log.contains(SECRET), "the refusal prints no value: {log}");

            deploy_ensure(
                service,
                new_want("service_container", "k1", &sealed),
                Some(&fixture.deploy(&sealed)),
            )
            .await
            .expect("a Device opens any Environment's Sealed values");
            deploy_ensure(
                service,
                new_want("service_container", "k2", &sealed),
                Some(&fixture.browser_deploy(PRODUCTION, &[&sealed])),
            )
            .await
            .expect("a session opens its own Environment's Sealed values");
            assert_eq!(creates(&fixture.fake, seen), 2);
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_browser_approval_of_a_spec_without_sealed_values_runs_in_any_session() {
            let fixture = enforced_sealing("ployzd-deploy-sealed-plain").await;
            trust_passkey(&fixture).await;
            let plain = sealed_spec(&fixture, "api", None);
            let seen = requests(&fixture.fake).len();
            deploy_ensure(
                &fixture.service,
                new_want("service_container", "k1", &plain),
                Some(&fixture.browser_deploy(FIX_API, &[&plain])),
            )
            .await
            .unwrap();
            assert_eq!(creates(&fixture.fake, seen), 1);
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_hooks_sealed_values_for_another_environment_are_refused() {
            let fixture = enforced_sealing("ployzd-deploy-sealed-hook").await;
            trust_passkey(&fixture).await;
            let hooked = sealed_spec(&fixture, "api", Some(Sealed::Hook));
            let signed = fixture.browser_deploy(FIX_API, &[&hooked]);
            let seen = requests(&fixture.fake).len();
            for (kind, key) in [("pre_deploy_hook", "h1"), ("service_container", "k1")] {
                deploy_refused(
                    deploy_ensure(
                        &fixture.service,
                        new_want(kind, key, &hooked),
                        Some(&signed),
                    )
                    .await,
                    "sealed values belong to another environment",
                );
            }
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing reached Docker");
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn one_approval_of_two_services_refuses_only_the_cross_environment_one() {
            let fixture = enforced_sealing("ployzd-deploy-sealed-mixed").await;
            trust_passkey(&fixture).await;
            let api = sealed_spec(&fixture, "api", None);
            let worker = sealed_spec(&fixture, "worker", Some(Sealed::Container));
            let signed = fixture.browser_deploy(FIX_API, &[&api, &worker]);
            let seen = requests(&fixture.fake).len();
            deploy_refused(
                deploy_ensure(
                    &fixture.service,
                    new_want("service_container", "w1", &worker),
                    Some(&signed),
                )
                .await,
                "sealed values belong to another environment",
            );
            deploy_ensure(
                &fixture.service,
                new_want("service_container", "a1", &api),
                Some(&signed),
            )
            .await
            .unwrap();
            assert_eq!(creates(&fixture.fake, seen), 1, "only api reached Docker");
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_deploy_approval_admits_steps_in_its_namespace_only() {
            let fixture = enforced("ployzd-deploy-steps").await;
            let service = &fixture.service;
            let id = present(&ensure(service, run_new("k1", spec(PINNED))).await).container_id;
            let signed = fixture.deploy(&spec(PINNED));
            let mut elsewhere = signed.open().unwrap();
            elsewhere.namespace = Namespace::parse("other").unwrap();
            let elsewhere = Signed::sign(&elsewhere, &fixture.signer).unwrap();
            deploy_refused(
                deploy_ensure(service, stopped(&id), Some(&elsewhere)).await,
                "a deploy to another environment",
            );
            let reply = deploy_ensure(service, stopped(&id), Some(&signed))
                .await
                .unwrap();
            assert!(matches!(
                present(&reply).runtime,
                ContainerRuntimeObservation::Exited { .. }
            ));
            deploy_ensure(
                service,
                json!({"state":"gone", "container":{"id":id}, "drain":0}),
                Some(&signed),
            )
            .await
            .unwrap();
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }
    }
}

mod read_routes {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use futures_util::{SinkExt, StreamExt};
    use ployz_core::{
        ContainerId, Machine, QualifiedService, ReadRoutesRequest, RoutesRead, RpcError,
    };
    use serde_json::{Value, json};
    use tokio::net::UnixListener;
    use tokio_util::codec::LengthDelimitedCodec;

    use super::*;
    use crate::corrosion::{
        AdminClient, IngressRouteAck, IngressRouteChange, ReplicatedStore,
        store_tests::{corrosion_db, identity_store},
    };

    fn admin_socket(dir: &Path, states: Vec<Value>) -> PathBuf {
        let path = dir.join(format!("admin-{}.sock", MachineId::random()));
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut framed = LengthDelimitedCodec::builder()
                    .big_endian()
                    .length_field_length(4)
                    .new_framed(stream);
                framed.next().await;
                for state in &states {
                    let frame = serde_json::to_vec(&json!({ "Json": state })).unwrap();
                    framed.send(frame.into()).await.unwrap();
                }
                framed
                    .send(serde_json::to_vec(&json!("Success")).unwrap().into())
                    .await
                    .unwrap();
            }
        });
        path
    }

    fn row(id: char, subnet: u8) -> Machine {
        serde_json::from_value(json!({
            "id": id.to_string().repeat(32),
            "name": format!("machine-{subnet}"),
            "subnet": format!("10.210.{subnet}.0/24"),
            "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
            "public_key": crate::network::WireGuardPrivateKey::from_bytes([subnet; 32]).public_key(),
            "advertised_endpoints": [format!("192.0.2.{subnet}:51820")],
        }))
        .unwrap()
    }

    fn enrolled(dir: &Path) -> LocalMachineStore {
        let mut local = LocalMachineStore::open(dir.join(MachineId::random().as_str())).unwrap();
        local
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: ployz_core::MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                    grant: None,
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        local
    }

    pub(super) async fn serve(
        dir: &Path,
        rows: &[Machine],
    ) -> (MachineService, tokio::task::JoinHandle<()>) {
        let (replicated, task): (ReplicatedStore, _) = identity_store(corrosion_db()).await;
        for row in rows {
            replicated
                .machine_publication()
                .await
                .publish(row)
                .await
                .unwrap();
        }
        let service = MachineService::with_cluster(
            RecordOwner::spawn(enrolled(dir)).unwrap(),
            Some((replicated, AdminClient::new(admin_socket(dir, Vec::new())))),
        );
        (service, task)
    }

    fn api() -> QualifiedService {
        QualifiedService::parse("app/api").unwrap()
    }

    async fn read(service: &MachineService) -> Result<RoutesRead, RpcError> {
        let request = op::ReadRoutes::into_request(ReadRoutesRequest { service: api() });
        let response = service
            .read_routes(Request::new(request.encode().unwrap()))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        if let ployz_core::RpcResponseBody::Error(error) = &response.body {
            return Err(error.clone());
        }
        Ok(response.decode::<op::ReadRoutes>().unwrap())
    }

    #[tokio::test]
    async fn read_names_the_copies_each_machine_routes_for_the_service() {
        let dir = std::env::temp_dir().join(format!("ployzd-routes-{}", MachineId::random()));
        std::fs::create_dir_all(&dir).unwrap();
        let (first, second, third) = (row('1', 11), row('2', 12), row('3', 13));
        let (service, task) = serve(&dir, &[first.clone(), second.clone(), third.clone()]).await;
        let local = service.local();
        let replicated = local.replicated().unwrap();
        let web = QualifiedService::parse("app/web").unwrap();
        let (routed, other) = (
            ContainerId::parse("a".repeat(64)).unwrap(),
            ContainerId::parse("b".repeat(64)).unwrap(),
        );
        let ack = |containers: &[ContainerId]| {
            IngressRouteChange::Upsert(IngressRouteAck {
                containers: containers.iter().copied().collect(),
            })
        };
        replicated
            .publish_ingress_routes(
                &first.id,
                &BTreeMap::from([(api(), ack(&[routed])), (web.clone(), ack(&[other]))]),
            )
            .await
            .unwrap();
        replicated
            .publish_ingress_routes(&second.id, &BTreeMap::from([(api(), ack(&[]))]))
            .await
            .unwrap();

        let read = read(&service).await.unwrap();

        assert_eq!(
            read.routes.get(&first.id),
            Some(&Some(BTreeSet::from([routed]))),
            "{read:?}"
        );
        assert_eq!(
            read.routes.get(&second.id),
            Some(&Some(BTreeSet::new())),
            "{read:?}"
        );
        assert_eq!(
            read.routes.get(&third.id),
            Some(&None),
            "a Machine with no row is unknown, not empty: {read:?}"
        );
        task.abort();
        std::fs::remove_dir_all(dir).ok();
    }
}

async fn select_endpoint(
    service: &MachineService,
    peer: MachineId,
) -> Result<ployz_core::EndpointSelected, ployz_core::RpcError> {
    let response = service
        .select_endpoint(Request::new(
            op::SelectEndpoint::into_request(ployz_core::SelectEndpointRequest {
                peer,
                endpoint: ployz_core::SelectedEndpoint("198.51.100.7:51820".parse().unwrap()),
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();
    if let RpcResponseBody::EndpointSelected(selected) = response.body {
        return Ok(selected);
    }
    let RpcResponseBody::Error(error) = response.body else {
        panic!("unexpected response {:?}", response.body);
    };
    Err(error)
}

#[tokio::test]
async fn an_operator_endpoint_reaches_this_machines_wireguard_and_never_names_itself() {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-select-endpoint-{}", MachineId::random()));
    let store = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let own_id = store.record().id();
    let without_mesh = MachineService::with_cluster(store.clone(), None);
    assert_eq!(
        select_endpoint(&without_mesh, MachineId::random())
            .await
            .unwrap_err()
            .code,
        RpcErrorCode::Unavailable
    );

    let (chooser, mut choices) = tokio::sync::mpsc::channel(1);
    let mut service = MachineService::with_cluster(store, None);
    service.endpoint_chooser = Some(chooser);
    let known = MachineId::random();
    let plane = tokio::spawn(async move {
        let mut taken = Vec::new();
        while let Some(choice) = choices.recv().await {
            let crate::network::EndpointChoice {
                peer,
                endpoint,
                applied,
            } = choice;
            let outcome = if peer == known {
                taken.push((peer, endpoint));
                Ok(())
            } else {
                Err(crate::network::ChoiceRefused::UnknownPeer(peer))
            };
            applied.send(outcome).unwrap();
        }
        taken
    });

    assert_eq!(
        select_endpoint(&service, own_id).await.unwrap_err().code,
        RpcErrorCode::InvalidArgument
    );
    let selected = select_endpoint(&service, known).await.unwrap();
    assert_eq!(selected.peer, known);
    assert_eq!(
        select_endpoint(&service, MachineId::random())
            .await
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    drop(service);
    assert_eq!(plane.await.unwrap(), [(known, selected.endpoint)]);
    std::fs::remove_dir_all(data_dir).ok();
}

mod read_routed_addresses {
    use ployz_core::{
        ContainerAddress, QualifiedService, ReadRoutedAddressesRequest, RoutedAddresses,
    };
    use tokio::sync::watch;

    use super::*;
    use crate::ingress::PublishedRoutes;

    fn service() -> MachineService {
        let dir = std::env::temp_dir().join(format!("ployzd-routed-{}", MachineId::random()));
        let store = RecordOwner::spawn(LocalMachineStore::open(&dir).unwrap()).unwrap();
        MachineService::with_cluster(store, None)
    }

    async fn read(service: &MachineService) -> RoutedAddresses {
        let request = op::ReadRoutedAddresses::into_request(ReadRoutedAddressesRequest {});
        service
            .read_routed_addresses(Request::new(request.encode().unwrap()))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::ReadRoutedAddresses>()
            .unwrap()
    }

    #[tokio::test]
    async fn a_proxy_that_never_published_claims_no_address() {
        assert_eq!(read(&service()).await, RoutedAddresses::default());
    }

    #[tokio::test]
    async fn read_answers_the_addresses_the_proxy_last_applied() {
        let web = QualifiedService::parse("app/web").unwrap();
        let copy = ContainerId::parse("a".repeat(64)).unwrap();
        let (publishes, published) = watch::channel(PublishedRoutes::default());
        let service = MachineService {
            published_routes: published,
            ..service()
        };
        let routed = |octet: u8| {
            BTreeMap::from([(
                web.clone(),
                BTreeMap::from([(copy, ContainerAddress([10, 210, 2, octet].into()))]),
            )])
        };
        for octet in [2, 3] {
            publishes.send_modify(|published| published.addresses = Some(routed(octet)));
        }

        assert_eq!(read(&service).await.routes, Some(routed(3)));
    }
}

mod cluster_name {
    use ployz_core::{
        ClusterName, ClusterNamed, OpaquePayload, ReadClusterNameRequest, Rpc, RpcResponse,
        SetClusterNameRequest,
    };
    use tonic::Response;

    use super::*;

    fn named<R: Rpc<Response = ClusterNamed>>(response: Response<OpaquePayload>) -> ClusterNamed {
        response
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<R>()
            .unwrap()
    }

    async fn read(service: &MachineService) -> ClusterNamed {
        let request = op::ReadClusterName::into_request(ReadClusterNameRequest {});
        named::<op::ReadClusterName>(
            service
                .read_cluster_name(Request::new(request.encode().unwrap()))
                .await
                .unwrap(),
        )
    }

    async fn set(service: &MachineService, name: &str) -> ClusterNamed {
        let request = op::SetClusterName::into_request(SetClusterNameRequest {
            name: ClusterName::parse(name).unwrap(),
        });
        named::<op::SetClusterName>(
            service
                .set_cluster_name(Request::new(request.encode().unwrap()))
                .await
                .unwrap(),
        )
    }

    #[tokio::test]
    async fn a_set_name_is_read_back_and_a_rename_replaces_it() {
        let dir = std::env::temp_dir().join(format!("ployzd-name-{}", MachineId::random()));
        std::fs::create_dir_all(&dir).unwrap();
        let (service, task) = super::read_routes::serve(&dir, &[]).await;

        assert_eq!(read(&service).await.name, None);
        for name in ["acme", "beta"] {
            let name = Some(ClusterName::parse(name).unwrap());
            assert_eq!(
                set(&service, name.as_ref().unwrap().as_str()).await.name,
                name
            );
            assert_eq!(read(&service).await.name, name);
        }
        task.abort();
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_machine_outside_a_cluster_refuses_the_name() {
        let dir = std::env::temp_dir().join(format!("ployzd-name-{}", MachineId::random()));
        let store = RecordOwner::spawn(LocalMachineStore::open(&dir).unwrap()).unwrap();
        let service = MachineService::with_cluster(store, None);
        let request = op::SetClusterName::into_request(SetClusterNameRequest {
            name: ClusterName::parse("acme").unwrap(),
        });
        let response: RpcResponse = service
            .set_cluster_name(Request::new(request.encode().unwrap()))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        let ployz_core::RpcResponseBody::Error(error) = response.body else {
            panic!("expected a refusal, got {response:?}");
        };
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        std::fs::remove_dir_all(dir).ok();
    }
}
