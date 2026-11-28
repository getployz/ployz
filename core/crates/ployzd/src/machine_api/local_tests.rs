//! Tests for the Machine RPC boundary.

use super::{MachineService, store_error, withhold_opened};
use crate::machine::local_machine::names_content;
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

/// An Operator's peer copy may write only the retention name its own content certifies: a
/// drain's user tag, a tag reference, or a retention name of other content would leave a
/// name a Full creation runs.
#[test]
fn an_operator_copies_only_under_the_retention_name_of_its_content() {
    use ployz_core::{ImageDigestReference, PeerImagePull};
    let hex = "a".repeat(64);
    let pinned = ImageDigestReference::parse(format!("app@sha256:{hex}")).unwrap();
    let publish = |tag: String| PeerImagePull::Publish {
        image: pinned.clone(),
        tag,
    };
    let reference = |image: String| PeerImagePull::Reference { image };
    for allowed in [
        reference(format!("app@sha256:{hex}")),
        reference(format!("app:v1@sha256:{hex}")),
        publish(format!("app:ployz-sha256-{hex}")),
    ] {
        assert!(names_content(&allowed), "{allowed:?}");
    }
    for refused in [
        reference("app:v1".into()),
        reference("app".into()),
        publish("app:v1".into()),
        publish(format!("app:ployz-sha256-{}", "b".repeat(64))),
        publish("app:ployz-sha256-staging".into()),
    ] {
        assert!(!names_content(&refused), "{refused:?}");
    }
}

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
                registry_login: None,
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

/// A Full creation runs the tag by name, so a Full caller's pull, such as a direct
/// Device's before its Deploy creates anything, must leave the tag naming what the
/// registry serves: on a Server that lacks it, and over an older one when pulled `always`.
#[tokio::test]
async fn a_full_callers_pull_leaves_the_tag_its_creation_runs() {
    let image = "example.test/api:1";
    let served = format!("sha256:{}", "e".repeat(64));
    for (held, policy) in [
        (None, ployz_core::PullPolicy::Missing),
        (
            Some(format!("sha256:{}", "b".repeat(64))),
            ployz_core::PullPolicy::Always,
        ),
    ] {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-full-pull").await;
        fake.images
            .lock()
            .unwrap()
            .insert(image.into(), held.clone());
        fake.registry
            .lock()
            .unwrap()
            .insert(image.into(), served.clone());
        service
            .pull_free_image(Request::new(
                op::PullFreeImage::into_request(ployz_core::PullFreeImageRequest {
                    image: image.into(),
                    policy,
                    registry_auth: None,
                    registry_login: None,
                })
                .encode()
                .unwrap(),
            ))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::PullFreeImage>()
            .unwrap();
        assert_eq!(
            fake.images.lock().unwrap().get(image),
            Some(&Some(served.clone())),
            "held {held:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }
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
        .remove_container(
            first.container_id,
            crate::docker::VolumeDisposal::Keep,
            false,
        )
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
    let inspect = op::InspectVolumeCopy::into_request(ployz_core::InspectVolumeCopyRequest {
        name,
        prove: false,
    });
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
    let (data_dir, store, service, fake, runtime) =
        fake_docker_service_built(prefix, None, None).await;
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
        fake_docker_service_built(prefix, Some(cluster.dir.0.clone()), None).await;
    (data_dir, store, service, fake, runtime, cluster)
}

/// With `start_barrier`, every start waits there twice: once to announce it began, once
/// to be released.
async fn fake_docker_service_built(
    prefix: &str,
    sealing_key_dir: Option<std::path::PathBuf>,
    start_barrier: Option<Arc<tokio::sync::Barrier>>,
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
        start_barrier,
        ..Default::default()
    })
    .await;
    let runtime = match sealing_key_dir {
        Some(dir) => runtime
            .with_sealing_key(dir)
            .with_machine_root(data_dir.join("root")),
        None => runtime,
    };
    // A Legacy root under every name, so a create reads the origin its spec expects.
    let plugin = crate::storage::test_support::FakePlugin::default();
    plugin.reply(
        crate::volume_copy::INSPECT_ROUTE,
        serde_json::json!({"Ok": {"kind":"root", "readonly":false, "newest":null, "epoch":1,
            "seed":false}}),
    );
    let default_plugin = data_dir.join("default-plugin");
    std::fs::create_dir_all(&default_plugin).unwrap();
    let (plugin, _server) = plugin.serve(&default_plugin);
    let mut service = MachineService::with_cluster(store.clone(), None)
        .with_optional_containers(Some(runtime.clone()))
        .with_volume_plugin(plugin);
    // `true` lists no pools, which reads as ZFS ready whatever the host has.
    service.local =
        service
            .local
            .with_zpool("true")
            .with_pacing(crate::machine::local_machine::Pacing {
                router_settle: std::time::Duration::from_millis(300),
                router_poll: std::time::Duration::from_millis(200),
            });
    (data_dir, store, service, fake, runtime)
}

mod ingress_doors {
    //! The Ingress doors route and the legacy Caddy Deploy it translates, against the
    //! fake Docker: who may call it, and what each answers.

    use axum::http::Method;
    use ployz_core::{
        ContainerNow, ContainerRefusalReason, DoorChange, EnsuredContainer, IngressDoorsSet,
        OPERATOR_SCOPE_HEADER, QualifiedService, RpcError, SET_INGRESS_DOORS_CAPABILITY,
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
        let response = service
            .ensure_container(Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(request).unwrap())
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
            .decode::<op::EnsureContainer>()
            .unwrap_or_else(|error| panic!("{error}: {response:?}"))
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

    fn docker_calls(fake: &crate::docker::test_support::FakeDocker) -> usize {
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
            .doors()
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
    async fn no_door_places_a_holder_with_no_port_and_no_connector() {
        let (data_dir, _store, service, _fake, runtime) =
            fake_docker_service_keeping_runtime("ployzd-doors-closed").await;
        let set = set_doors(
            &service,
            json!({"doors": [], "proxy_service": proxy_id()}),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        let holder = set.holder.unwrap();
        assert!(
            holder.now.resolved_spec.ports.is_empty(),
            "a closed holder publishes no port: {:?}",
            holder.now.resolved_spec.ports
        );
        assert_eq!(
            holder.now.resolved_spec,
            DoorSpecs::new(proxy_id(), ployz_core::IngressPorts::Closed, None).holder
        );
        assert_eq!(set.proxy.unwrap().change, DoorChange::Created);
        assert!(set.tunnel.is_none(), "{:?}", set.tunnel);
        let inventory = runtime
            .doors()
            .door_inventory(&service.local.record().id())
            .await
            .unwrap();
        assert!(
            inventory
                .iter()
                .all(|entry| entry.facts.door != crate::docker::Door::Tunnel),
            "no connector runs on a closed Machine"
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

    /// A Device's Tunnel Door, sent from its own connection, whose Device goes while the
    /// doors wait for their turn: the opened token never reaches the connector's file.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tunnel_door_queued_while_its_device_goes_writes_no_token() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake, _runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-removed").await;
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
            })
            .await
            .unwrap();
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let caller = service.clone().with_management_client(device.into());
        let turn = store.admission_lock().write_owned().await;
        let tunnel =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let call = set_doors(&caller, tunnel, false);
        tokio::pin!(call);
        if let std::task::Poll::Ready(answered) = futures_util::poll!(&mut call) {
            panic!("answered before its turn: {answered:?}");
        }
        store
            .mutate(move |store| {
                assert!(
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device.code())
                        .unwrap()
                )
            })
            .await
            .unwrap();
        drop(turn);
        let answer = call.await;
        assert!(
            !data_dir.join("root/run/ployz/tunnel/token").exists(),
            "{answer:?}"
        );
        assert_eq!(answer.unwrap_err().code(), Code::Unauthenticated);
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
        let pair = specs.tunnel.unwrap();
        assert_eq!(connector.now.resolved_spec, pair.connector);
        let token = data_dir.join("root/run/ployz/tunnel/token");
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        let inventory = runtime
            .doors()
            .door_inventory(&service.local.record().id())
            .await
            .unwrap();
        assert_eq!(inventory.len(), 4);
        let origin = inventory
            .iter()
            .find(|entry| entry.facts.door == crate::docker::Door::TunnelOrigin)
            .unwrap();
        assert_eq!(origin.observation.resolved_spec, pair.origin);
        for entry in &inventory {
            let joined = match entry.facts.door {
                crate::docker::Door::Proxy => Some(holder.now.container_id),
                crate::docker::Door::Tunnel => Some(origin.facts.id),
                crate::docker::Door::Holder | crate::docker::Door::TunnelOrigin => None,
            };
            assert_eq!(entry.facts.joined_to, joined, "{:?}", entry.facts.door);
        }
        let network_modes = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/containers/create"))
            .filter_map(|(_, body)| {
                body.pointer("/HostConfig/NetworkMode")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            network_modes
                .iter()
                .filter(|mode| *mode == "ployz-tunnel")
                .count(),
            1,
            "the Tunnel Origin alone is created on its own bridge: {network_modes:?}"
        );

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
                .doors()
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
        runtime.machine_files().remove_tunnel_token().unwrap();
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
                    .doors()
                    .door_inventory(&machine_id)
                    .await
                    .unwrap()
                    .iter()
                    .filter(|entry| entry.facts.door == crate::docker::Door::Tunnel)
                    .all(|entry| entry.facts.runtime.is_running())
            }
        };
        let untouched = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(untouched.failure, None, "{untouched:?}");
        assert_eq!(
            untouched.tunnel.unwrap().change,
            DoorChange::Unchanged,
            "a daemon restart leaves the running doors alone"
        );

        runtime
            .lifecycle()
            .stop(&connector, None, None)
            .await
            .unwrap();
        runtime.machine_files().remove_tunnel_token().unwrap();
        assert!(!running(&runtime).await && !token.exists());

        let restored = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        assert_eq!(restored.holder.unwrap().change, DoorChange::Unchanged);
        let started = restored.tunnel.unwrap();
        assert_eq!(started.change, DoorChange::Started);
        assert_eq!(started.now.container_id, connector);
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        assert!(running(&runtime).await);

        runtime.machine_files().remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_reboot_places_a_held_door_only_while_the_cluster_declares_it() {
        use crate::corrosion::{
            AdminClient,
            fake_cluster::{corrosion_db, identity_store},
        };
        let (data_dir, store, _without_cluster, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-declared").await;
        let (replicated, task) = identity_store(corrosion_db()).await;
        let mut service = MachineService::with_cluster(
            store,
            Some((
                replicated.clone(),
                AdminClient::new(data_dir.join("admin.sock")),
            )),
        )
        .with_optional_containers(Some(runtime.clone()));
        service.local = service.local.with_zpool("true");
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let tunnel =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let placed = set_doors(&service, tunnel, false).await.unwrap().unwrap();
        assert_eq!(placed.failure, None, "{placed:?}");
        let connector = placed.tunnel.unwrap().now.container_id;
        let token = data_dir.join("root/run/ployz/tunnel/token");
        let reboot = || async {
            runtime
                .lifecycle()
                .stop(&connector, None, None)
                .await
                .unwrap();
            runtime.machine_files().remove_tunnel_token().unwrap();
        };

        // A declaration without the Tunnel Door leaves the connector at rest and writes no token.
        replicated
            .publish_cluster_doors(&ployz_core::IngressDoors::public())
            .await
            .unwrap();
        reboot().await;
        assert!(service.local.restore_doors().await.unwrap().is_none());
        assert!(!token.exists(), "the connector's token is not written back");
        let machine_id = service.local.record().id();
        let connector_running = || async {
            runtime
                .doors()
                .door_inventory(&machine_id)
                .await
                .unwrap()
                .iter()
                .filter(|entry| entry.facts.door == crate::docker::Door::Tunnel)
                .any(|entry| entry.facts.runtime.is_running())
        };
        assert!(!connector_running().await);

        // Declaring it places the connector as a Machine with no declaration does.
        replicated
            .publish_cluster_doors(&ployz_core::IngressDoors::tunnel())
            .await
            .unwrap();
        let restored = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        assert_eq!(restored.tunnel.unwrap().change, DoorChange::Started);
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        assert!(connector_running().await);

        // Part way through a switch to tunnel the Machine still holds both Doors. Docker
        // brings the holder back, so the declared connector must come back too.
        let both = json!({"doors": ["public", "tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let switching = set_doors(&service, both, false).await.unwrap().unwrap();
        assert_eq!(switching.failure, None, "{switching:?}");
        let connector = switching.tunnel.unwrap().now.container_id;
        runtime
            .lifecycle()
            .stop(&connector, None, None)
            .await
            .unwrap();
        runtime.machine_files().remove_tunnel_token().unwrap();
        assert!(!connector_running().await);
        let restored = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        assert_eq!(restored.tunnel.unwrap().change, DoorChange::Started);
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        assert!(connector_running().await);

        task.abort();
        runtime.machine_files().remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_connector_closing_could_not_remove_stays_stopped_across_a_reboot() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-closed-stays").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let open =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let placed = set_doors(&service, open.clone(), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(placed.failure, None, "{placed:?}");
        let connector = placed.tunnel.unwrap().now.container_id;
        let token = data_dir.join("root/run/ployz/tunnel/token");
        let connectors = || async {
            runtime
                .doors()
                .door_inventory(&machine)
                .await
                .unwrap()
                .into_iter()
                .filter(|entry| entry.facts.door == crate::docker::Door::Tunnel)
                .collect::<Vec<_>>()
        };

        let service_name = QualifiedService::system_tunnel().name.to_string();
        fake.refuse_remove_of
            .lock()
            .unwrap()
            .insert(service_name.clone());
        let closed = set_doors(
            &service,
            json!({"doors": ["public"], "proxy_service": proxy_id()}),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(closed.failure, None, "{closed:?}");
        let [stayed] = connectors().await.try_into().unwrap_or_else(|left| {
            panic!("the connector Docker refused to remove stays: {left:?}")
        });
        assert_eq!(stayed.facts.id, connector);
        assert!(!stayed.facts.runtime.is_running(), "it is stopped");
        assert!(!token.exists(), "the token goes with the Tunnel Door");

        let restored = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        assert!(
            restored.tunnel.is_none(),
            "a reboot places no connector for a Tunnel Door that was closed"
        );
        let [stayed] = connectors().await.try_into().unwrap_or_else(|left| {
            panic!("the connector still stays while Docker refuses: {left:?}")
        });
        assert!(
            !stayed.facts.runtime.is_running(),
            "a reboot never starts it"
        );
        assert!(!token.exists(), "a reboot writes no token for it");

        fake.refuse_remove_of.lock().unwrap().remove(&service_name);
        let finished = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(finished.failure, None, "{finished:?}");
        assert_eq!(finished.retired, vec![connector], "{finished:?}");
        assert!(connectors().await.is_empty(), "the next restore removes it");

        let reopened = set_doors(&service, open, false).await.unwrap().unwrap();
        assert_eq!(reopened.failure, None, "{reopened:?}");
        assert_eq!(std::fs::read(&token).unwrap(), b"cf-token");
        let restarted = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(
            restarted.tunnel.unwrap().change,
            DoorChange::Unchanged,
            "opening the Tunnel Door again holds it across a restart"
        );

        runtime.machine_files().remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_failed_tunnel_door_never_starts_a_connector_in_the_holders_network_again() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-old-connector").await;
        let machine = service.local.record().id();
        set_doors(&service, doors_request(), false)
            .await
            .unwrap()
            .unwrap();
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let record = service.local.record();
        let placed = record.machine().unwrap();
        let mut spec = ployz_core::tunnel_service_spec("cloudflared:1", &sealed);
        spec.container.network = Some(ployz_core::ServiceNetwork::IngressHolder);
        let spec = spec
            .to_resolved(ployz_core::ServiceId::random(), Default::default())
            .unwrap();
        let old = runtime
            .lifecycle()
            .create_for_test(
                &placed.id,
                placed.subnet.gateway(),
                ployz_core::ContainerKind::ServiceContainer,
                &ployz_core::Namespace::system(),
                &spec,
            )
            .await
            .unwrap()
            .container_id;
        runtime.lifecycle().start(&old).await.unwrap();

        fake.refuse_create_of
            .lock()
            .unwrap()
            .insert(QualifiedService::system_tunnel_origin().name.to_string());
        let open =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let set = set_doors(&service, open, false).await.unwrap().unwrap();
        assert_eq!(
            set.failure.as_ref().map(|failure| failure.stage),
            Some(ployz_core::IngressStage::Tunnel),
            "{set:?}"
        );
        let doors = at_rest_by_id(&runtime, &machine).await;
        assert!(
            doors.contains(&(old, true)),
            "the undo started the old doors again but this connector: {doors:?}"
        );
        assert!(
            doors.iter().any(|(id, at_rest)| *id != old && !at_rest),
            "the previous holder and Caddy run again: {doors:?}"
        );
        runtime.machine_files().remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn reopening_the_tunnel_door_over_a_connector_left_stopped_holds_it_across_a_reboot() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-doors-reopen-stays").await;
        let sealed = cluster.seal_platform(ployz_core::PlatformPurpose::IngressTunnel, b"cf-token");
        let open =
            json!({"doors": ["tunnel"], "proxy_service": proxy_id(), "sealed_token": sealed});
        let placed = set_doors(&service, open.clone(), false)
            .await
            .unwrap()
            .unwrap();
        let connector = placed.tunnel.unwrap().now.container_id;
        let service_name = QualifiedService::system_tunnel().name.to_string();
        fake.refuse_remove_of
            .lock()
            .unwrap()
            .insert(service_name.clone());
        set_doors(
            &service,
            json!({"doors": ["public"], "proxy_service": proxy_id()}),
            false,
        )
        .await
        .unwrap()
        .unwrap();

        let reopened = set_doors(&service, open, false).await.unwrap().unwrap();
        assert_eq!(reopened.failure, None, "{reopened:?}");
        let created = reopened.tunnel.unwrap();
        assert_eq!(created.change, DoorChange::Created);
        assert_ne!(created.now.container_id, connector);

        let restarted = service.local.restore_doors().await.unwrap().unwrap();
        assert_eq!(restarted.failure, None, "{restarted:?}");
        assert_eq!(
            restarted.tunnel.unwrap().change,
            DoorChange::Unchanged,
            "the reopened connector is held, not retired"
        );

        runtime.machine_files().remove_tunnel_token().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn an_operator_under_enforcement_places_the_pinned_doors() {
        let (data_dir, store, service, fake) = fake_docker_service("ployzd-doors-enforced").await;
        store
            .mutate(|store| store.trust_members(Vec::new()).unwrap())
            .await
            .unwrap();
        let set = set_doors(&service, doors_request(), true)
            .await
            .unwrap()
            .unwrap();
        assert!(set.failure.is_none(), "{set:?}");
        let specs = DoorSpecs::public(proxy_id());
        assert_eq!(set.proxy.unwrap().now.resolved_spec, specs.proxy);
        let images: Vec<String> = fake
            .request_bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.ends_with("/containers/create"))
            .map(|(_, body)| body["Image"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(
            images,
            [crate::images::PAUSE, crate::images::CADDY],
            "the doors run the daemon's pins and nothing else"
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
            ..
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
            .doors()
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
                .observation()
                .inspect_managed(&caddy.container_id, &service.local.record().id())
                .await,
            Err(crate::docker::Error::ContainerNotFound(_))
        ));
        assert_eq!(
            runtime
                .doors()
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
            .doors()
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

        let doors = runtime.doors().door_inventory(&machine).await.unwrap();
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

    fn caddy_create_refused(fake: &crate::docker::test_support::FakeDocker) {
        fake.refuse_create_of
            .lock()
            .unwrap()
            .insert("ingress".into());
    }

    fn caddy_start_exits(fake: &crate::docker::test_support::FakeDocker) {
        fake.exit_on_start
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Each door Container on `machine`, holders first, with whether it is at rest.
    async fn at_rest_by_id(
        runtime: &crate::docker::ContainerRuntime,
        machine: &MachineId,
    ) -> Vec<(ployz_core::ContainerId, bool)> {
        runtime
            .doors()
            .door_inventory(machine)
            .await
            .unwrap()
            .iter()
            .map(|entry| (entry.facts.id, entry.facts.runtime.at_rest()))
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
            let doors = runtime.doors().door_inventory(&machine).await.unwrap();
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
            .lifecycle()
            .create_for_test(
                &machine.id,
                machine.subnet.gateway(),
                ployz_core::ContainerKind::ServiceContainer,
                &ployz_core::Namespace::system(),
                &spec,
            )
            .await
            .unwrap();
        runtime
            .lifecycle()
            .start(&created.container_id)
            .await
            .unwrap();
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

    fn inspect_fails_after_start(fake: &crate::docker::test_support::FakeDocker, service: &str) {
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
        for entry in runtime.doors().door_inventory(&machine).await.unwrap() {
            runtime
                .lifecycle()
                .stop(&entry.facts.id, None, None)
                .await
                .unwrap();
            match entry.facts.door {
                crate::docker::Door::Holder => holder = Some(entry.facts.id),
                crate::docker::Door::Proxy
                | crate::docker::Door::TunnelOrigin
                | crate::docker::Door::Tunnel => {
                    runtime
                        .lifecycle()
                        .remove(&entry.facts.id, crate::docker::VolumeDisposal::Keep, true)
                        .await
                        .unwrap();
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
            .doors()
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
            futures_util::poll!(std::pin::pin!(runtime.doors().hold())).is_pending(),
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

mod private_access {
    //! The Private Access route against the fake Docker: who it answers, what it
    //! places, and what it leaves on the Machine.

    use ployz_core::{
        DoorChange, PlatformPurpose, PrivateAccessSet, QualifiedService, RpcError,
        SET_PRIVATE_ACCESS_CAPABILITY,
    };
    use serde_json::{Value, json};
    use tonic::Status;

    use super::*;
    use crate::images::private_access_router;

    async fn set_access(
        service: &MachineService,
        body: Value,
    ) -> Result<Result<PrivateAccessSet, RpcError>, Status> {
        let request = Request::new(
            op::SetPrivateAccess::into_request(serde_json::from_value(body).unwrap())
                .encode()
                .unwrap(),
        );
        let response = service
            .set_private_access(request)
            .await?
            .into_inner()
            .decode_response()
            .unwrap();
        if let RpcResponseBody::Error(error) = response.body {
            return Ok(Err(error));
        }
        let shown = format!("{response:?}");
        Ok(Ok(response
            .decode::<op::SetPrivateAccess>()
            .unwrap_or_else(|error| panic!("{error}: {shown}"))))
    }

    fn key_file(data_dir: &std::path::Path) -> std::path::PathBuf {
        data_dir.join("root/run/ployz/tailscale/auth-key")
    }

    fn node_state(data_dir: &std::path::Path) -> std::path::PathBuf {
        data_dir.join("root/var/lib/ployz/tailscale")
    }

    #[tokio::test]
    async fn on_places_the_router_on_the_opened_key_and_off_removes_both() {
        let (data_dir, _store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-on-off").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let on = json!({"enabled": true, "sealed_auth_key": sealed});

        let set = set_access(&service, on.clone()).await.unwrap().unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        let router = set.router.unwrap();
        assert_eq!(router.change, DoorChange::Created);
        assert_eq!(router.now.identity(), QualifiedService::system_tailscale());
        let network = service.local.cluster_network().await.unwrap();
        assert_eq!(
            router.now.resolved_spec,
            private_access_router(
                network,
                &sealed,
                runtime.machine_files().keep_private_access_state().unwrap()
            )
        );
        assert_eq!(
            std::fs::read(key_file(&data_dir)).unwrap(),
            b"tskey-auth-test"
        );
        assert!(
            node_state(&data_dir).is_dir(),
            "the node state directory is there before the router starts"
        );

        let again = set_access(&service, on).await.unwrap().unwrap();
        assert_eq!(again.router.unwrap().change, DoorChange::Unchanged);
        assert_eq!(runtime.routers(&machine).await.unwrap().len(), 1);

        let off = set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(off.router, None);
        assert_eq!(off.retired.len(), 1, "{off:?}");
        assert!(runtime.routers(&machine).await.unwrap().is_empty());
        assert!(
            !key_file(&data_dir).exists(),
            "the key leaves with the router"
        );
        assert!(
            !node_state(&data_dir).exists(),
            "off forgets the node, so a later on registers a new one"
        );

        let off_again = set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(off_again, PrivateAccessSet::default());
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Device's turn on, sent at Full access with no approval, whose Device slot goes
    /// while it waits for its turn. The key keeps Cloud's slot, an Operator's, so the turn
    /// on needs the approval an Operator's does: no router is placed and the unapproved key
    /// never reaches the router's file.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_on_queued_while_its_device_slot_goes_needs_an_operators_approval() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-downgraded").await;
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
                let cloud = ployz_core::ManagementClientLabel::cloud();
                store
                    .stage_management_client(
                        &crate::machine::KeyChangeHeld::for_test(),
                        cloud,
                        device.into(),
                    )
                    .unwrap();
                store
                    .activate_management_client(
                        &crate::machine::KeyChangeHeld::for_test(),
                        device.into(),
                    )
                    .unwrap();
            })
            .await
            .unwrap();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-planted");
        let caller = service.clone().with_management_client(device.into());
        let turn = store.admission_lock().write_owned().await;
        let call = set_access(&caller, json!({"enabled": true, "sealed_auth_key": sealed}));
        tokio::pin!(call);
        if let std::task::Poll::Ready(answered) = futures_util::poll!(&mut call) {
            panic!("answered before its turn: {answered:?}");
        }
        store
            .mutate(move |store| {
                assert!(
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device.code())
                        .unwrap()
                )
            })
            .await
            .unwrap();
        assert!(store.record().accepts_management_client(&device.into()));
        drop(turn);
        let answer = call.await;
        let machine = service.local.record().id();
        assert!(
            runtime.routers(&machine).await.unwrap().is_empty(),
            "{answer:?}"
        );
        assert!(!key_file(&data_dir).exists(), "{answer:?}");
        assert!(!node_state(&data_dir).exists(), "{answer:?}");
        let refused = answer.unwrap_err();
        assert_eq!(refused.code(), Code::PermissionDenied, "{refused:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_reboot_leaves_the_router_at_rest_and_the_router_held_starts_again() {
        let (data_dir, _store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-reboot").await;
        let machine = service.local.record().id();
        let running = || async {
            runtime
                .routers(&machine)
                .await
                .unwrap()
                .iter()
                .all(|router| matches!(router.runtime, ContainerRuntimeObservation::Running { .. }))
        };
        assert!(
            service
                .local
                .restore_private_access()
                .await
                .unwrap()
                .is_none(),
            "a Machine with Private Access off places no router"
        );
        assert!(runtime.routers(&machine).await.unwrap().is_empty());
        assert!(!key_file(&data_dir).exists());

        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let on = json!({"enabled": true, "sealed_auth_key": sealed});
        let placed = set_access(&service, on).await.unwrap().unwrap();
        let router = placed.router.unwrap().now.container_id;

        let untouched = service
            .local
            .restore_private_access()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.failure, None, "{untouched:?}");
        assert_eq!(
            untouched.router.unwrap().change,
            DoorChange::Unchanged,
            "a daemon restart leaves the running router alone"
        );

        runtime.lifecycle().stop(&router, None, None).await.unwrap();
        runtime.machine_files().remove_private_access_key().unwrap();
        assert!(!running().await && !key_file(&data_dir).exists());
        std::fs::write(node_state(&data_dir).join("tailscaled.state"), b"node").unwrap();

        let restored = service
            .local
            .restore_private_access()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored.failure, None, "{restored:?}");
        let started = restored.router.unwrap();
        assert_eq!(started.change, DoorChange::Started);
        assert_eq!(started.now.container_id, router);
        assert!(restored.retired.is_empty());
        assert_eq!(
            std::fs::read(key_file(&data_dir)).unwrap(),
            b"tskey-auth-test"
        );
        assert_eq!(
            std::fs::read(node_state(&data_dir).join("tailscaled.state")).unwrap(),
            b"node",
            "the node the router registered comes back with it"
        );
        assert!(running().await);

        set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap();
        assert!(
            service
                .local
                .restore_private_access()
                .await
                .unwrap()
                .is_none(),
            "a Machine turned off stays off after a reboot"
        );
        assert!(runtime.routers(&machine).await.unwrap().is_empty());
        assert!(!key_file(&data_dir).exists());
        assert!(!node_state(&data_dir).exists());
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches `off` answering success while Docker kept the stopped router, which the
    // next boot restore would start again, with the key its label carries.
    #[tokio::test]
    async fn off_that_cannot_remove_the_router_fails_and_a_reboot_finishes_it() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-off-stays").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let router = set_access(
            &service,
            json!({"enabled": true, "sealed_auth_key": sealed}),
        )
        .await
        .unwrap()
        .unwrap()
        .router
        .unwrap()
        .now
        .container_id;
        let service_name = QualifiedService::system_tailscale().name.to_string();
        fake.refuse_remove_of
            .lock()
            .unwrap()
            .insert(service_name.clone());

        let refused = set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Internal, "{refused:?}");
        assert!(
            refused.message.contains("not off yet"),
            "the answer says the Server is not off: {}",
            refused.message
        );
        let stays = || async {
            let routers = runtime.routers(&machine).await.unwrap();
            let [stayed] = routers.as_slice() else {
                panic!("the router Docker refused to remove stays: {routers:?}");
            };
            assert_eq!(stayed.container_id, router);
            assert!(
                !matches!(stayed.runtime, ContainerRuntimeObservation::Running { .. }),
                "the router is stopped, never started: {:?}",
                stayed.runtime
            );
        };
        stays().await;
        assert!(
            !key_file(&data_dir).exists(),
            "the key is gone before the router"
        );
        assert!(
            !node_state(&data_dir).exists(),
            "the node is forgotten before the router"
        );

        service.local.restore_private_access().await.unwrap_err();
        stays().await;
        assert!(
            !key_file(&data_dir).exists(),
            "a reboot writes no key for a router turned off"
        );

        fake.refuse_remove_of.lock().unwrap().remove(&service_name);
        let finished = service
            .local
            .restore_private_access()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.router, None);
        assert_eq!(finished.retired, vec![router]);
        assert!(runtime.routers(&machine).await.unwrap().is_empty());
        assert!(
            service
                .local
                .restore_private_access()
                .await
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches an `off` cut short before it forgets the node, which a reboot read as
    // `on` and so started the router again.
    #[tokio::test]
    async fn off_cut_short_before_the_router_stops_is_still_off_after_a_reboot() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-off-cut").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let router = set_access(
            &service,
            json!({"enabled": true, "sealed_auth_key": sealed}),
        )
        .await
        .unwrap()
        .unwrap()
        .router
        .unwrap()
        .now
        .container_id;
        fake.refuse_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);

        set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            !node_state(&data_dir).exists(),
            "the node is forgotten before the router is touched"
        );

        fake.refuse_stop
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let finished = service
            .local
            .restore_private_access()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.router, None, "a reboot never starts the router");
        assert_eq!(finished.retired, vec![router]);
        assert!(runtime.routers(&machine).await.unwrap().is_empty());
        assert!(!key_file(&data_dir).exists());
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    fn refuse_stop(fake: &crate::docker::test_support::FakeDocker, refused: bool) {
        fake.refuse_stop
            .store(refused, std::sync::atomic::Ordering::SeqCst);
    }

    /// A router an `off` could not stop, and an `on` that could not stop it either, so
    /// the router still mounts the node state `off` deleted while a new one is kept.
    async fn strand_router(
        service: &MachineService,
        fake: &crate::docker::test_support::FakeDocker,
        on: &Value,
    ) -> ContainerId {
        let stranded = set_access(service, on.clone())
            .await
            .unwrap()
            .unwrap()
            .router
            .unwrap()
            .now
            .container_id;
        refuse_stop(fake, true);
        set_access(service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap_err();
        set_access(service, on.clone()).await.unwrap().unwrap_err();
        refuse_stop(fake, false);
        stranded
    }

    // Catches `on` keeping a router an `off` could not stop, once an `on` that could
    // not stop it either had kept new node state. That router still mounts the state
    // `off` deleted, so its next restart would register a new node.
    #[tokio::test]
    async fn on_after_an_off_that_could_not_stop_the_router_replaces_it() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-off-then-on").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let on = json!({"enabled": true, "sealed_auth_key": sealed});
        let stranded = strand_router(&service, &fake, &on).await;

        let again = set_access(&service, on).await.unwrap().unwrap();
        let placed = again.router.unwrap();
        assert_eq!(placed.change, DoorChange::Created, "{placed:?}");
        assert_eq!(
            ployz_core::private_access_state(&placed.now.resolved_spec.container.labels),
            Some(runtime.machine_files().keep_private_access_state().unwrap()),
            "the new router mounts the node state on disk now"
        );
        assert_eq!(again.retired, vec![stranded]);
        let routers = runtime.routers(&machine).await.unwrap();
        assert_eq!(
            routers
                .iter()
                .map(|router| router.container_id)
                .collect::<Vec<_>>(),
            vec![placed.now.container_id]
        );
        set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a reboot keeping or starting a router that mounts node state `off`
    // deleted, after an `on` that could not stop it kept new state.
    #[tokio::test]
    async fn a_reboot_after_a_stranded_router_replaces_it() {
        let (data_dir, _store, service, fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-stranded-reboot").await;
        let machine = service.local.record().id();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let on = json!({"enabled": true, "sealed_auth_key": sealed});
        let stranded = strand_router(&service, &fake, &on).await;

        let restored = service
            .local
            .restore_private_access()
            .await
            .unwrap()
            .unwrap();
        let placed = restored.router.unwrap();
        assert_eq!(placed.change, DoorChange::Created, "{placed:?}");
        assert_eq!(
            ployz_core::private_access_state(&placed.now.resolved_spec.container.labels),
            Some(runtime.machine_files().keep_private_access_state().unwrap()),
            "the new router mounts the node state on disk now"
        );
        assert_eq!(restored.retired, vec![stranded]);
        let routers = runtime.routers(&machine).await.unwrap();
        assert_eq!(
            routers
                .iter()
                .map(|router| router.container_id)
                .collect::<Vec<_>>(),
            vec![placed.now.container_id]
        );
        assert_eq!(
            std::fs::read(key_file(&data_dir)).unwrap(),
            b"tskey-auth-test"
        );
        set_access(&service, json!({"enabled": false}))
            .await
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_new_key_replaces_the_router() {
        let (data_dir, _store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-rekey").await;
        let machine = service.local.record().id();
        let first = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"first");
        let second = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"second");
        let before = set_access(&service, json!({"enabled": true, "sealed_auth_key": first}))
            .await
            .unwrap()
            .unwrap()
            .router
            .unwrap();
        let after = set_access(
            &service,
            json!({"enabled": true, "sealed_auth_key": second}),
        )
        .await
        .unwrap()
        .unwrap();
        let router = after.router.unwrap();
        assert_eq!(router.change, DoorChange::Created);
        assert_ne!(router.now.container_id, before.now.container_id);
        assert_eq!(after.retired, vec![before.now.container_id]);
        assert_eq!(runtime.routers(&machine).await.unwrap().len(), 1);
        assert_eq!(std::fs::read(key_file(&data_dir)).unwrap(), b"second");
        runtime.machine_files().remove_private_access_key().unwrap();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn on_needs_a_key_this_cluster_opens_and_reaches_no_docker_without_one() {
        let (data_dir, _store, service, fake, _runtime, _cluster) =
            fake_docker_service_sealing("ployzd-access-key").await;
        let seen = fake.requests.lock().unwrap().len();
        let missing = set_access(&service, json!({"enabled": true}))
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
            Some(&json!(SET_PRIVATE_ACCESS_CAPABILITY))
        );

        let other = crate::sealed_env::test_support::Cluster::new();
        let foreign = other.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let unopenable = set_access(
            &service,
            json!({"enabled": true, "sealed_auth_key": foreign}),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(unopenable.code, RpcErrorCode::InvalidArgument);
        assert_eq!(unopenable.details.get("capability"), None);

        let tunnel = other.seal_platform(PlatformPurpose::IngressTunnel, b"cf-token");
        assert!(
            set_access(
                &service,
                json!({"enabled": true, "sealed_auth_key": tunnel})
            )
            .await
            .unwrap()
            .is_err(),
            "a tunnel token's purpose does not open as an auth key"
        );
        assert_eq!(
            fake.requests.lock().unwrap().len(),
            seen,
            "nothing reaches Docker without a key"
        );
        assert!(!key_file(&data_dir).exists());
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn the_router_runs_on_a_machine_that_accepts_only_ingress() {
        let (data_dir, store, service, _fake, runtime, cluster) =
            fake_docker_service_sealing("ployzd-access-ingress-only").await;
        store
            .mutate(|store| {
                store.update(
                    serde_json::from_value(json!({"accepts_services": false})).unwrap(),
                    &[],
                )
            })
            .await
            .unwrap()
            .unwrap();
        let sealed = cluster.seal_platform(PlatformPurpose::PrivateAccess, b"tskey-auth-test");
        let set = set_access(
            &service,
            json!({"enabled": true, "sealed_auth_key": sealed}),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(set.failure, None, "{set:?}");
        assert_eq!(
            runtime
                .routers(&service.local.record().id())
                .await
                .unwrap()
                .len(),
            1
        );
        runtime.machine_files().remove_private_access_key().unwrap();
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
    /// The plugin routes that changed a copy, in order. Reads are left out: how often the
    /// daemon reads a copy follows the code, and what a test checks is what changed.
    fn effects(plugin: &FakePlugin) -> Vec<String> {
        plugin
            .routes_called()
            .into_iter()
            .filter(|route| route != INSPECT_ROUTE)
            .collect()
    }

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

    /// The reply without the image its promote recorded, which every Create start places.
    fn unplaced(reply: &SetVolumeCopyReply) -> Value {
        let mut reply = serde_json::to_value(reply).unwrap();
        if let Some(copy) = reply.get_mut("copy").and_then(Value::as_object_mut) {
            copy.remove("placed");
        }
        reply
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
            prove: false,
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
    async fn running_holder(
        service: &MachineService,
        fake: &crate::docker::test_support::FakeDocker,
    ) -> ContainerId {
        let created = kept_holder(service, fake).await;
        start_holder(fake, &created);
        created
    }

    /// A stopped Container that mounts the Volume: Docker lists it for the cut too.
    async fn kept_holder(
        service: &MachineService,
        fake: &crate::docker::test_support::FakeDocker,
    ) -> ContainerId {
        let created = stopped_container(service).await;
        fake.volume_users
            .lock()
            .unwrap()
            .entry(VOLUME.into())
            .or_default()
            .push(json!({"Id": created.to_string()}));
        created
    }

    /// The holders of the Volume in the order Docker lists them, which is the order the cut
    /// visits them in.
    async fn listed_holders(service: &MachineService) -> Vec<ContainerId> {
        service
            .local()
            .containers()
            .unwrap()
            .volumes()
            .volume_holder_ids(&VOLUME.parse().unwrap())
            .await
            .unwrap()
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
        let turn = std::pin::pin!(local.containers().unwrap().lifecycle().floor_turn());
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

    pub(super) async fn requested(
        fake: &crate::docker::test_support::FakeDocker,
        method: http::Method,
        suffix: &str,
    ) {
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

    fn start_holder(fake: &crate::docker::test_support::FakeDocker, id: &ContainerId) {
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

    #[tokio::test]
    async fn an_operator_start_of_a_holder_keeps_the_switch_lock_after_its_caller_hangs_up() {
        let started = Arc::new(tokio::sync::Barrier::new(2));
        let (data_dir, _store, service, fake, _) =
            fake_docker_service_built("ployzd-holder-start-dropped", None, Some(started.clone()))
                .await;
        let kept = kept_holder(&service, &fake).await;
        for container in fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
        {
            container["State"] = json!({"Status":"exited", "Running":false});
        }
        let mut request = Request::new(
            op::EnsureContainer::into_request(
                serde_json::from_value(json!({"want": {"state":"running", "rotation":"pending",
                    "container":{"existing":{"id":kept}}}}))
                .unwrap(),
            )
            .encode()
            .unwrap(),
        );
        request
            .metadata_mut()
            .insert(ployz_core::OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
        let ensuring = tokio::spawn({
            let service = service.clone();
            async move { service.ensure_container(request).await }
        });

        started.wait().await;
        ensuring.abort();
        assert!(ensuring.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::ZERO, service.local().lock_switch())
                .await
                .is_err(),
            "the caller hung up and freed the switch lock while the holder's start still ran"
        );

        started.wait().await;
        let _freed = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            service.local().lock_switch(),
        )
        .await
        .expect("the start ended and freed the switch lock");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    fn running(fake: &crate::docker::test_support::FakeDocker) -> usize {
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

    /// A spec mounting `data` born in `namespace`.
    fn born_spec(namespace: &str) -> (ResolvedServiceSpec, ployz_core::VolumeOrigin) {
        let birth: ployz_core::VolumeBirth = serde_json::from_value(json!({
            "namespace": namespace, "name": "data", "made_at": 1_762_178_640_000_u64,
            "nonce": "00112233445566778899aabbccddeeff"}))
        .unwrap();
        let origin = ployz_core::VolumeOrigin::of(&birth).unwrap();
        let mut source = ployz_core::RawVolumeSource::Provisioned {
            name: "data".parse().unwrap(),
            maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(1_073_741_824).unwrap(),
            ),
            labels: BTreeMap::new(),
            birth: Some(birth),
        }
        .admit()
        .unwrap();
        source.scope_to_namespace(&Namespace::parse("app").unwrap());
        (spec_with_sources(vec![source]), origin)
    }

    fn root_carrying(origin: Option<ployz_core::VolumeOrigin>) -> Value {
        json!({"Ok": {"kind":"root", "readonly":false, "newest":null, "epoch":1,
            "seed":false, "origin":origin}})
    }

    /// A stale copy promoted under the name, or a sibling's data swapped under it, is
    /// refused at create: Docker never makes a Container that would open it.
    #[tokio::test]
    async fn a_holder_is_created_only_over_the_data_its_spec_names() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-origin-create").await;
        let plugin = FakePlugin::default();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let (spec, born) = born_spec("app");
        let (other, _) = born_spec("other-env");
        let create = |spec: ResolvedServiceSpec| {
            let service = service.clone();
            async move {
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
            }
        };
        let made = || {
            fake.named_containers
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .len()
        };

        for carried in [
            None,
            Some(
                ployz_core::VolumeOrigin::of(
                    &serde_json::from_value(json!({
            "namespace": "app", "name": "data", "made_at": 1_762_178_640_000_u64,
            "nonce": "ffeeddccbbaa99887766554433221100"}))
                    .unwrap(),
                )
                .unwrap(),
            ),
        ] {
            plugin.reply(INSPECT_ROUTE, root_carrying(carried));
            let refused = create(spec.clone()).await.unwrap_err();
            assert!(
                matches!(
                    refused,
                    crate::machine::LocalMachineError::Docker(
                        crate::docker::Error::ForeignVolume { .. }
                    )
                ),
                "{refused:?}"
            );
            assert_eq!(made(), 0, "Docker made no Container over {carried:?}");
        }
        plugin.reply(INSPECT_ROUTE, root_carrying(Some(born)));
        create(other).await.unwrap_err();
        assert_eq!(
            made(),
            0,
            "a birth from another Environment names no data here"
        );

        create(spec).await.unwrap();
        assert_eq!(made(), 1);
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// Creates the `api` Service Container of `spec` in Namespace `app`.
    async fn create_api(
        service: &MachineService,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), crate::machine::LocalMachineError> {
        service
            .local
            .create_container(
                ContainerKind::ServiceContainer,
                &Namespace::parse("app").unwrap(),
                spec,
                None,
                None,
                None,
            )
            .await
            .map(drop)
    }

    /// An Ordinary Volume's origin is its Docker label. One left by another Volume of the
    /// name holds other data, and the refusal says so rather than that the Volume is
    /// unprepared.
    #[tokio::test]
    async fn an_ordinary_volume_labelled_for_other_data_is_refused_as_foreign() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-origin-ordinary").await;
        let birth: ployz_core::VolumeBirth = serde_json::from_value(json!({
            "namespace": "app", "name": "data", "made_at": 1_762_178_640_000_u64,
            "nonce": "00112233445566778899aabbccddeeff"}))
        .unwrap();
        let mut source = ployz_core::RawVolumeSource::Ordinary {
            name: "data".parse().unwrap(),
            driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
            labels: BTreeMap::new(),
            birth: Some(birth),
        }
        .admit()
        .unwrap();
        source.scope_to_namespace(&Namespace::parse("app").unwrap());
        let mut labels = source.creation_labels();
        labels.insert(ployz_core::labels::VOLUME_ORIGIN.into(), "c".repeat(64));
        let name = source.docker_volume_name().unwrap().to_string();
        fake.volumes.lock().unwrap().insert(
            name.clone(),
            json!({"Name":name, "Driver":"local", "Mountpoint":format!("/volumes/{name}"),
                "Options":{}, "Labels":labels}),
        );

        let refused = create_api(&service, &spec_with_sources(vec![source]))
            .await
            .unwrap_err();

        assert!(
            matches!(
                refused,
                crate::machine::LocalMachineError::Docker(
                    crate::docker::Error::ForeignVolume { .. }
                )
            ),
            "{refused:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// Docker can lose a Provisioned root's labels and find it again through the plugin. An
    /// External mount of its name still never opens it, and a genuine External Volume of the
    /// name opens only through the driver Docker has for it now.
    #[tokio::test]
    async fn an_external_mount_never_opens_a_provisioned_root_docker_lost_the_labels_of() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-origin-external").await;
        let spec = spec_with_sources(vec![
            ployz_core::RawVolumeSource::External {
                name: "shared".parse().unwrap(),
            }
            .admit()
            .unwrap(),
        ]);
        let made = || {
            fake.named_containers
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .len()
        };
        fake.volumes.lock().unwrap().insert(
            "shared".into(),
            json!({"Name":"shared", "Driver":"ployz", "Mountpoint":"/var/lib/ployz-volumes/shared",
                "Status":{"bound_bytes":1_073_741_824_u64, "used_bytes":0}}),
        );

        let refused = create_api(&service, &spec).await.unwrap_err();

        assert!(
            matches!(
                refused,
                crate::machine::LocalMachineError::Docker(
                    crate::docker::Error::ForeignVolume { .. }
                )
            ),
            "{refused:?}"
        );
        assert_eq!(
            made(),
            0,
            "Docker made no Container over the Provisioned root"
        );

        fake.volumes.lock().unwrap().insert(
            "shared".into(),
            json!({"Name":"shared", "Driver":"local", "Mountpoint":"/volumes/shared"}),
        );
        create_api(&service, &spec).await.unwrap();
        assert_eq!(made(), 1);
        let bodies = fake.request_bodies.lock().unwrap();
        let (_, body) = bodies
            .iter()
            .rfind(|(path, _)| path.ends_with("/containers/create"))
            .unwrap();
        assert_eq!(
            body.pointer("/HostConfig/Mounts/0/VolumeOptions/DriverConfig/Name"),
            Some(&json!("local")),
            "Docker resolves the name only through the driver it had: {body}"
        );
        drop(bodies);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Provisioned Volume with no root here, absent or held only in a slot, is not
    /// prepared on this Machine. It holds no other data, so it is not refused as foreign.
    #[tokio::test]
    async fn a_provisioned_volume_with_no_writable_copy_is_refused_as_unprepared() {
        let (data_dir, _store, service, _fake) =
            fake_docker_service("ployzd-origin-unwritable").await;
        let plugin = FakePlugin::default();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let (spec, _) = born_spec("app");

        for copy in [json!({"Ok": null}), slot(None)] {
            plugin.reply(INSPECT_ROUTE, copy.clone());
            let refused = create_api(&service, &spec).await.unwrap_err();
            assert!(
                matches!(
                    refused,
                    crate::machine::LocalMachineError::Docker(
                        crate::docker::Error::VolumeNotWritable(_)
                    )
                ),
                "{copy}: {refused:?}"
            );
        }
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// The volume plugin refuses a fresh root under a name a kept Container mounts for
    /// other data, and Docker relays that as text. The create still reads as foreign.
    #[tokio::test]
    async fn a_fresh_volume_the_plugin_holds_for_other_data_is_refused_as_foreign() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-origin-held").await;
        *fake.refuse_volume_create.lock().unwrap() = Some(format!(
            "create fresh: VolumeDriver.Create: Volume fresh {}, so no new data is made under its name until that Container is removed",
            crate::volume_copy::HELD_FOR_OTHER_DATA
        ));

        let refused = create_api(
            &service,
            &spec_with_sources(vec![provisioned_source("fresh", 1_073_741_824)]),
        )
        .await
        .unwrap_err();

        assert!(
            matches!(
                refused,
                crate::machine::LocalMachineError::Docker(
                    crate::docker::Error::ForeignVolume { .. }
                )
            ),
            "{refused:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A fresh Provisioned Volume waits while Docker does not list its holders, and the
    /// create says so as a refusal to retry, as a promote's or seed's does.
    #[tokio::test]
    async fn a_fresh_volume_docker_did_not_list_the_holders_of_is_unavailable() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-origin-unlisted").await;
        *fake.refuse_volume_create.lock().unwrap() = Some(format!(
            "create fresh: VolumeDriver.Create: {}, so no new data is made under Volume fresh until it does: no list within 3 s",
            crate::volume_copy::DOCKER_NOT_ANSWERING
        ));

        let refused = create_api(
            &service,
            &spec_with_sources(vec![provisioned_source("fresh", 1_073_741_824)]),
        )
        .await
        .unwrap_err();

        let crate::machine::LocalMachineError::Docker(refused) = refused else {
            panic!("{refused:?}");
        };
        assert_eq!(
            refused.rpc_code(),
            ployz_core::RpcErrorCode::Unavailable,
            "{refused}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
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
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
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
            unplaced(&raised),
            json!({"copy": writer_root(2, Some(snapshot("f-1", 7))).get("Ok")})
        );
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
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

    /// A check refuses a slot holding other data, as the Create after the promote would,
    /// so the Move refuses before its cut rather than after it.
    #[tokio::test]
    async fn a_checked_start_refuses_a_copy_of_other_data() {
        let (data_dir, _store, mut service, _fake) =
            fake_docker_service("ployzd-copy-check-origin").await;
        service.local = service.local.with_zpool("true");
        let plugin = FakePlugin::default();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let (spec, born) = born_spec("app");
        let (_, other) = born_spec("other-env");
        let check = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":2, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec,
                "creation_key":"move-1", "gen":1, "rotation":"pending", "check":true}}});
        let slot_of = |origin: &ployz_core::VolumeOrigin| {
            json!({"Ok": {"kind":"slot", "slot":"1", "readonly":true,
                "newest":snapshot("f-1", 7), "resume_token":null, "pull":null,
                "role":"mirror", "origin":origin}})
        };

        plugin.reply(INSPECT_ROUTE, slot_of(&other));
        let refused = format!("{:?}", set_copy(&service, check.clone()).await);
        assert!(refused.contains("another Volume's data"), "{refused}");

        plugin.reply(INSPECT_ROUTE, slot_of(&born));
        reply(&set_copy(&service, check).await);
        assert_eq!(effects(&plugin), Vec::<String>::new());
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// Cloud asks the destination before the cut whether it would start the Move. The
    /// answer promotes no copy and makes no Container.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_checked_start_answers_the_copy_and_changes_nothing() {
        let (data_dir, _store, mut service, fake) = fake_docker_service("ployzd-copy-check").await;
        service.local = service.local.with_zpool("true");
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let check = json!({"name":VOLUME, "expect":{"kind":"mirror", "newest":"7", "slot":"1"},
            "want":{"kind":"writer", "at":"7", "epoch":2, "start":{
                "kind":"create", "namespace":"app", "resolved_spec":spec(),
                "creation_key":"move-1", "gen":1, "rotation":"pending", "check":true}}});

        let answered = reply(&set_copy(&service, check).await);

        assert_eq!(
            serde_json::to_value(answered.copy).unwrap(),
            slot(Some(snapshot("f-1", 7)))["Ok"]
        );
        assert_eq!(effects(&plugin), Vec::<String>::new());
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| path.ends_with("/containers/create"))
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Create that reaches a Server late, while the Writer still runs there, makes no
    /// second Container on the root, and a kept holder doesn't start beside it either.
    /// Once the Writer stops, the kept holder starts.
    #[tokio::test]
    async fn no_holder_is_created_or_started_beside_a_running_one() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-one-holder").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let kept = kept_holder(&service, &fake).await;
        let writer = running_holder(&service, &fake).await;
        // A stopped helper Ployz didn't make, left on the root, never matters.
        helper_on_root(&fake, "exited");
        let held = |error: crate::machine::LocalMachineError| {
            let crate::machine::LocalMachineError::Docker(
                held @ crate::docker::Error::VolumeHeld { .. },
            ) = error
            else {
                panic!("expected the running holder named, got {error:?}");
            };
            assert_eq!(
                held.to_string(),
                "api still runs on the Volume at /data/0 on this Server; deploy again, or stop api first"
            );
        };

        // A new Service mounting the Volume elsewhere hears where the holder mounts it.
        let elsewhere =
            serde_json::from_str(&spec().to_string().replace("/data/0", "/elsewhere")).unwrap();
        held(
            service
                .local
                .create_container(
                    ContainerKind::ServiceContainer,
                    &Namespace::parse("app").unwrap(),
                    &elsewhere,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap_err(),
        );
        assert_eq!(
            holder_status(&fake).len(),
            3,
            "Docker made nothing beside the two holders and the helper"
        );
        held(service.local.start_container(kept, None).await.unwrap_err());
        assert_eq!(running(&fake), 1, "{:?}", holder_status(&fake));

        set_exited(&fake, &writer);
        service.local.start_container(kept, None).await.unwrap();
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Container whose Deploy shares the Volume is created and started beside the
    /// running Writer, and carries the sharing on its labels for a start with no spec,
    /// as bridge recovery makes. One that doesn't share it stays refused beside both.
    #[tokio::test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    async fn a_holder_that_shares_the_volume_runs_beside_the_writer() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-shared-holder").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, writer_root(1, None));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let kept = kept_holder(&service, &fake).await;
        running_holder(&service, &fake).await;
        let mut sharing = spec();
        sharing["container"]["shared_writes"] = json!(["v0"]);
        let shared = create_service_container(&service, serde_json::from_value(sharing).unwrap())
            .await
            .container_id;
        fake.volume_users
            .lock()
            .unwrap()
            .entry(VOLUME.into())
            .or_default()
            .push(json!({"Id": shared.to_string()}));
        let labels = fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values()
            .find(|container| container["Id"] == shared.to_string().as_str())
            .and_then(|container| container.pointer("/Config/Labels").cloned())
            .unwrap();
        assert_eq!(
            labels[ployz_core::labels::SHARED_WRITES],
            VOLUME,
            "{labels}"
        );

        service.local.start_container(shared, None).await.unwrap();
        start_holder(&fake, &shared);
        assert_eq!(running(&fake), 2, "{:?}", holder_status(&fake));
        let refused = service.local.start_container(kept, None).await.unwrap_err();
        assert!(
            matches!(
                refused,
                crate::machine::LocalMachineError::Docker(crate::docker::Error::VolumeHeld { .. })
            ),
            "{refused:?}"
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Container Ployz didn't make, in `status`, that mounts the Volume.
    fn helper_on_root(fake: &crate::docker::test_support::FakeDocker, status: &str) {
        let id = format!("{}{}", MachineId::random(), MachineId::random());
        fake.named_containers.as_ref().unwrap().lock().unwrap().insert(
            "helper".into(),
            json!({"Id": id, "Name": "/helper", "Config": {"Labels": {}}, "State": {"Status": status}}),
        );
        fake.volume_users
            .lock()
            .unwrap()
            .entry(VOLUME.into())
            .or_default()
            .push(json!({"Id": id}));
    }

    fn set_exited(fake: &crate::docker::test_support::FakeDocker, id: &ContainerId) {
        for container in fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
            .filter(|container| container["Id"] == id.to_string().as_str())
        {
            container["State"] = json!({"Status":"exited", "Running":false, "ExitCode":0});
        }
    }

    fn holder_status(fake: &crate::docker::test_support::FakeDocker) -> Vec<Value> {
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
        assert_eq!(effects(&plugin), vec![CUT_ROUTE]);

        let again = reply(&set_copy(&service, cut(json!({"kind":"writer", "epoch":0}))).await);
        assert_eq!(again, first);
        assert_eq!(effects(&plugin), vec![CUT_ROUTE]);
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
        assert_eq!(effects(&plugin), vec![CUT_ROUTE]);
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
        // Both are made stopped: no Container is made beside a running holder. The cut
        // visits holders in Docker's list order and passes a stopped one by, so only a
        // holder listed before the one it drains is started too late to be drained itself.
        kept_holder(&service, &fake).await;
        kept_holder(&service, &fake).await;
        let [second, first] = listed_holders(&service).await.try_into().unwrap();
        start_holder(&fake, &first);
        let draining = |drain: u32| {
            json!({"name":VOLUME, "expect":{"kind":"writer", "epoch":0},
                "want":{"kind":"mirror", "from":null, "target":"f-1", "drain":drain}})
        };

        // The fake holder never reads idle, so the drain lasts its whole cap, and the
        // second holder starts once it has begun.
        let (refused, ()) = tokio::join!(set_copy(&service, draining(3_000)), async {
            drain_started(&service, &first).await;
            start_holder(&fake, &second);
        });
        let error = refusal(&refused);
        assert_eq!(error.details["reason"], json!("busy"), "{error:?}");
        assert_eq!(error.details["copy"], writer_root(0, None)["Ok"]);
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
        assert_eq!(reply(&set_copy(&service, raise).await), raised);
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);

        let error = refusal(&set_copy(&service, cut(json!({"kind":"writer", "epoch":1}))).await);
        assert_eq!(error.details["reason"], json!("mismatch"), "{error:?}");
        assert_eq!(holder_status(&fake), vec![json!("running")]);
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
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
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
            .lifecycle()
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
            .lifecycle()
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
        let turn = service.containers().unwrap().lifecycle().floor_turn().await;
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
            .lifecycle()
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
            .observation()
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
            .observation()
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
        // Made first: no Container is made beside a running holder.
        let deployed = kept_holder(&service, &fake).await;
        let older = running_holder(&service, &fake).await;

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
            unplaced(&late),
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
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
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
        assert_eq!(effects(&plugin), Vec::<&str>::new());
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
        assert_eq!(effects(&plugin), vec![WRITER_ROUTE]);
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

    #[tokio::test]
    async fn a_forget_queued_on_the_switch_while_its_device_goes_destroys_nothing() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake) = fake_docker_service("ployzd-copy-revoked").await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 7))));
        plugin.reply(crate::volume_copy::DESTROY_ROUTE, json!({"Ok": null}));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
            })
            .await
            .unwrap();
        let caller = service.clone().with_management_client(device.into());
        let switch = service.local().lock_switch().await.unwrap();
        let forgetting = tokio::spawn(async move {
            let mirror = json!({"kind":"mirror", "newest":"7", "slot":"1"});
            let request = at_gen(forget(mirror), 3);
            caller
                .set_volume_copy(Request::new(
                    op::SetVolumeCopy::into_request(serde_json::from_value(request).unwrap())
                        .encode()
                        .unwrap(),
                ))
                .await
                .map(|reply| reply.into_inner().decode_response().unwrap())
        });
        // The forget raises the floor to its gen just before it waits for the switch.
        let name = VOLUME.parse().unwrap();
        let containers = service.local().docker().unwrap().clone();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let mut floor = containers.lifecycle().floor_turn().await;
                if floor
                    .check(&name, ployz_core::VolumeGen::new(2))
                    .await
                    .unwrap()
                    .is_err()
                {
                    break;
                }
                drop(floor);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the forget queued on the switch");
        store
            .mutate(move |store| {
                assert!(
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device.code())
                        .unwrap()
                )
            })
            .await
            .unwrap();
        drop(switch);
        let answer = forgetting.await.unwrap();
        assert_eq!(effects(&plugin), Vec::<String>::new(), "{answer:?}");
        assert_eq!(
            answer.map(drop).unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// Until `plugin` has heard a call on `route`.
    async fn until_called(plugin: &FakePlugin, route: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !plugin.routes_called().iter().any(|called| called == route) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the plugin never heard {route}"));
    }

    fn destroys(plugin: &FakePlugin) -> usize {
        plugin
            .routes_called()
            .iter()
            .filter(|route| *route == crate::volume_copy::DESTROY_ROUTE)
            .count()
    }

    /// A Device's forget holds the switch, its read waiting on the plugin, when `ployz
    /// device rm` removes that Device. The removal waits for the switch, so the forget's
    /// destroy lands while the Device is still trusted, and none lands after the removal
    /// returns.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removal_waits_out_the_forget_that_holds_the_switch() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake) =
            fake_docker_service("ployzd-copy-removal-waits").await;
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
            })
            .await
            .unwrap();
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 7))));
        plugin.reply(crate::volume_copy::DESTROY_ROUTE, json!({"Ok": null}));
        // Whether the Device was still trusted at each destroy.
        let trusted = Arc::new(std::sync::Mutex::new(Vec::new()));
        plugin.on(crate::volume_copy::DESTROY_ROUTE, {
            let (store, trusted) = (store.clone(), Arc::clone(&trusted));
            move || {
                let held = store.record().accepts_management_client(&device.into());
                trusted.lock().unwrap().push(held);
            }
        });
        let reading = plugin.hold(INSPECT_ROUTE).await;
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let mut asked = service.local().switch_asked();
        let caller = service.clone().with_management_client(device.into());
        let mirror = json!({"kind":"mirror", "newest":"7", "slot":"1"});
        let forgetting =
            tokio::spawn(async move { set_copy(&caller, at_gen(forget(mirror), 3)).await });
        until_called(&plugin, INSPECT_ROUTE).await;
        let mut removing = tokio::spawn({
            let service = service.clone();
            async move {
                let remove = ployz_core::SetDeviceRequest::Remove {
                    code: device.code(),
                };
                service
                    .set_device(Request::new(
                        op::SetDevice::into_request(remove).encode().unwrap(),
                    ))
                    .await
                    .map(|reply| reply.into_inner().decode_response().unwrap())
            }
        });
        let early = tokio::select! {
            removed = &mut removing => Some(removed.unwrap()),
            queued = asked.wait_for(|asked| *asked >= 2) => {
                queued.unwrap();
                None
            }
        };
        drop(reading);
        let removed = match early {
            Some(removed) => removed,
            None => removing.await.unwrap(),
        };
        let at_removal = destroys(&plugin);
        let forgot = forgetting.await.unwrap();
        assert_eq!(
            destroys(&plugin),
            at_removal,
            "a destroy landed after the removal returned: {forgot:?}"
        );
        assert_eq!(*trusted.lock().unwrap(), [true], "{removed:?}");
        reply(&forgot);
        removed
            .unwrap()
            .decode::<op::SetDevice>()
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!store.record().accepts_management_client(&device.into()));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Device's floor raise queues on the floor lock behind another caller's, whose read
    /// waits on the plugin, when `ployz device rm` removes that Device. A floor raise takes
    /// no switch, so the removal waits for the floor lock instead: the raise lands before
    /// the removal returns, or not at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removal_waits_out_its_floor_raise_queued_on_the_floor_lock() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake) =
            fake_docker_service("ployzd-floor-removal-waits").await;
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
            })
            .await
            .unwrap();
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let reading = plugin.hold(INSPECT_ROUTE).await;
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let specs = service
            .local()
            .containers()
            .unwrap()
            .spec_store_path_for_test()
            .to_owned();
        let first = tokio::spawn({
            let service = service.clone();
            async move { set_copy(&service, floor(2)).await }
        });
        until_called(&plugin, INSPECT_ROUTE).await;
        let mut asked = service.local().floor_asked();
        let caller = service.clone().with_management_client(device.into());
        let raising = tokio::spawn(async move {
            caller
                .set_volume_copy(Request::new(
                    op::SetVolumeCopy::into_request(serde_json::from_value(floor(5)).unwrap())
                        .encode()
                        .unwrap(),
                ))
                .await
                .map(|reply| reply.into_inner().decode_response().unwrap())
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            asked.wait_for(|asked| *asked >= 2),
        )
        .await
        .expect("the Device's raise queues on the floor lock")
        .unwrap();
        let mut removing = tokio::spawn({
            let (service, specs) = (service.clone(), specs.clone());
            async move {
                let remove = ployz_core::SetDeviceRequest::Remove {
                    code: device.code(),
                };
                let removed = service
                    .set_device(Request::new(
                        op::SetDevice::into_request(remove).encode().unwrap(),
                    ))
                    .await
                    .map(|reply| reply.into_inner().decode_response().unwrap());
                (removed, floor_row(&specs))
            }
        });
        let early = tokio::select! {
            removed = &mut removing => Some(removed.unwrap()),
            queued = asked.wait_for(|asked| *asked >= 3) => {
                queued.unwrap();
                None
            }
        };
        drop(reading);
        let (removed, at_removal) = match early {
            Some(removed) => removed,
            None => removing.await.unwrap(),
        };
        reply(&first.await.unwrap());
        let raised = raising.await.unwrap();
        assert_eq!(
            floor_row(&specs),
            at_removal,
            "a floor raise landed after the removal returned: {raised:?}"
        );
        removed
            .unwrap()
            .decode::<op::SetDevice>()
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!store.record().accepts_management_client(&device.into()));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// The floor of the Volume as the spec store holds it, read without the floor lock.
    fn floor_row(specs: &std::path::Path) -> i64 {
        rusqlite::Connection::open(specs)
            .unwrap()
            .query_row(
                "SELECT coalesce(max(floor), 0) FROM volume_floors WHERE name = ?1",
                [VOLUME],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// Approve `device` and a second Device, so removing `device` leaves a key that may
    /// change keys.
    async fn approve_with_another(store: &RecordOwner, device: ployz_core::DeviceKey) {
        let kept = ployz_core::DeviceKey::from(
            ployz_core::authority::Signer::from_seed(&[9; 32]).public(),
        );
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
            })
            .await
            .unwrap();
    }

    /// `ployz device rm` of `device`, answered with the floor of the Volume read the moment
    /// it returns.
    fn remove_device(
        service: &MachineService,
        device: ployz_core::DeviceKey,
        specs: &std::path::Path,
    ) -> tokio::task::JoinHandle<(Result<RpcResponse, tonic::Status>, i64)> {
        let (service, specs) = (service.clone(), specs.to_owned());
        tokio::spawn(async move {
            let remove = ployz_core::SetDeviceRequest::Remove {
                code: device.code(),
            };
            let removed = service
                .set_device(Request::new(
                    op::SetDevice::into_request(remove).encode().unwrap(),
                ))
                .await
                .map(|reply| reply.into_inner().decode_response().unwrap());
            (removed, floor_row(&specs))
        })
    }

    /// A Device's floor raise, its RPC cancelled once its write began, when `ployz device
    /// rm` removes that Device. Another connection holds the database, so the write waits.
    /// Nothing takes the floor lock until the write lands, so the removal returns after it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removal_waits_out_a_cancelled_floor_raise_still_writing() {
        use futures_util::FutureExt;
        let (data_dir, store, service, _fake) = fake_docker_service("ployzd-floor-cancelled").await;
        let device = ployz_core::DeviceKey::from(
            ployz_core::authority::Signer::from_seed(&[7; 32]).public(),
        );
        approve_with_another(&store, device).await;
        let plugin = FakePlugin::default();
        plugin.reply(INSPECT_ROUTE, slot(Some(snapshot("f-1", 7))));
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let containers = service.local().containers().unwrap().clone();
        let specs = containers.spec_store_path_for_test().to_owned();
        let blocker = rusqlite::Connection::open(&specs).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut begun = containers.floor_writes_for_test();
        let caller = service.clone().with_management_client(device.into());
        let raising = tokio::spawn(async move { set_copy(&caller, floor(5)).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            begun.wait_for(|begun| *begun >= 1),
        )
        .await
        .expect("the raise's write begins")
        .unwrap();
        raising.abort();
        assert!(raising.await.unwrap_err().is_cancelled());
        assert!(
            containers.lifecycle().floor_turn().now_or_never().is_none(),
            "the floor lock came free while the cancelled raise's write still waited"
        );
        let mut asked = service.local().floor_asked();
        let before = *asked.borrow();
        let mut removing = remove_device(&service, device, &specs);
        let early = tokio::select! {
            removed = &mut removing => Some(removed.unwrap()),
            queued = asked.wait_for(|asked| *asked > before) => {
                queued.unwrap();
                None
            }
        };
        drop(blocker);
        let (removed, at_removal) = match early {
            Some(removed) => removed,
            None => removing.await.unwrap(),
        };
        assert_eq!(
            at_removal, 5,
            "the removal returned before the cancelled raise's write landed"
        );
        removed
            .unwrap()
            .decode::<op::SetDevice>()
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!store.record().accepts_management_client(&device.into()));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Device's SetVolumeSize, its RPC cancelled while the plugin resizes, when `ployz
    /// device rm` removes that Device. The resize keeps the switch until the plugin
    /// answers, so the removal returns only after the bound changed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_device_removal_waits_out_a_cancelled_resize_the_plugin_still_runs() {
        use futures_util::FutureExt;
        use std::sync::atomic::{AtomicBool, Ordering};
        let (data_dir, store, service, _fake) =
            fake_docker_service("ployzd-resize-cancelled").await;
        let device = ployz_core::DeviceKey::from(
            ployz_core::authority::Signer::from_seed(&[7; 32]).public(),
        );
        approve_with_another(&store, device).await;
        let plugin = FakePlugin::default();
        let route = crate::volume_copy::VOLUME_SIZE_SET_ROUTE;
        plugin.reply(route, json!({"Ok": {"bound_bytes": 2_147_483_648_u64}}));
        let resized = Arc::new(AtomicBool::new(false));
        plugin.on(route, {
            let resized = Arc::clone(&resized);
            move || resized.store(true, Ordering::SeqCst)
        });
        let resizing = plugin.hold(route).await;
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let specs = service
            .local()
            .containers()
            .unwrap()
            .spec_store_path_for_test()
            .to_owned();
        let caller = service.clone().with_management_client(device.into());
        let sizing = tokio::spawn(async move {
            let request = json!({"name":VOLUME, "maximum_bytes":2_147_483_648_u64});
            caller
                .set_volume_size(Request::new(
                    op::SetVolumeSize::into_request(serde_json::from_value(request).unwrap())
                        .encode()
                        .unwrap(),
                ))
                .await
        });
        until_called(&plugin, route).await;
        sizing.abort();
        assert!(sizing.await.unwrap_err().is_cancelled());
        let mut asked = service.local().switch_asked();
        let before = *asked.borrow();
        assert!(
            service.local().lock_switch().now_or_never().is_none(),
            "the switch came free while the plugin still resized"
        );
        let mut removing = tokio::spawn({
            let removing = remove_device(&service, device, &specs);
            let resized = Arc::clone(&resized);
            async move {
                let (removed, _) = removing.await.unwrap();
                (removed, resized.load(Ordering::SeqCst))
            }
        });
        let early = tokio::select! {
            removed = &mut removing => Some(removed.unwrap()),
            queued = asked.wait_for(|asked| *asked >= before + 2) => {
                queued.unwrap();
                None
            }
        };
        drop(resizing);
        let (removed, resized_at_removal) = match early {
            Some(removed) => removed,
            None => removing.await.unwrap(),
        };
        assert!(
            resized_at_removal,
            "the removal returned before the cancelled resize finished"
        );
        removed
            .unwrap()
            .decode::<op::SetDevice>()
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!store.record().accepts_management_client(&device.into()));
        server.abort();
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A Seed, which only Full access sends, queued on the switch while its Device slot
    /// goes. The key keeps Cloud's slot, an Operator's, and the Seed is refused there
    /// before the plugin hears it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_seed_queued_while_its_device_slot_goes_is_refused_before_the_plugin_hears_it() {
        use ployz_core::authority::Signer;
        let (data_dir, store, service, _fake) = fake_docker_service("ployzd-seed-downgraded").await;
        let plugin = pulled_mirror_plugin();
        let (socket, server) = plugin.serve(&data_dir);
        let service = service.with_volume_plugin(socket);
        let device = ployz_core::DeviceKey::from(Signer::from_seed(&[7; 32]).public());
        let kept = ployz_core::DeviceKey::from(Signer::from_seed(&[9; 32]).public());
        store
            .mutate(move |store| {
                store.approve_device(device).unwrap();
                store.approve_device(kept).unwrap();
                let cloud = ployz_core::ManagementClientLabel::cloud();
                store
                    .stage_management_client(
                        &crate::machine::KeyChangeHeld::for_test(),
                        cloud,
                        device.into(),
                    )
                    .unwrap();
                store
                    .activate_management_client(
                        &crate::machine::KeyChangeHeld::for_test(),
                        device.into(),
                    )
                    .unwrap();
            })
            .await
            .unwrap();
        let mut asked = service.local().switch_asked();
        let switch = service.local().lock_switch().await.unwrap();
        let caller = service.clone().with_management_client(device.into());
        let seeding = tokio::spawn(async move {
            let request = json!({"name":"source", "into":VOLUME, "guid":"77", "gen":4});
            caller
                .seed_volume(Request::new(
                    op::SeedVolume::into_request(serde_json::from_value(request).unwrap())
                        .encode()
                        .unwrap(),
                ))
                .await
                .map(|reply| reply.into_inner().decode_response().unwrap())
        });
        asked.wait_for(|asked| *asked >= 2).await.unwrap();
        store
            .mutate(move |store| {
                assert!(
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device.code())
                        .unwrap()
                )
            })
            .await
            .unwrap();
        assert!(store.record().accepts_management_client(&device.into()));
        drop(switch);
        let answer = seeding.await.unwrap();
        assert!(
            !plugin
                .routes_called()
                .iter()
                .any(|route| route == SEED_ROUTE),
            "{answer:?}"
        );
        assert_eq!(
            answer.map(drop).unwrap_err().code(),
            tonic::Code::PermissionDenied
        );
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
        let response = service
            .ensure_container(Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(request).unwrap())
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
            .decode::<op::EnsureContainer>()
            .unwrap_or_else(|error| panic!("{error}: {response:?}"))
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

    fn requests(fake: &crate::docker::test_support::FakeDocker) -> Vec<(Method, String)> {
        fake.requests.lock().unwrap().clone()
    }

    fn requests_since(
        fake: &crate::docker::test_support::FakeDocker,
        seen: usize,
    ) -> Vec<(Method, String)> {
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

    fn set_state(fake: &crate::docker::test_support::FakeDocker, id: &ContainerId, state: Value) {
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
        let pulls = |fake: &crate::docker::test_support::FakeDocker| {
            ends_with(&requests(fake), Method::POST, "/images/create").count()
        };

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
                verified: None,
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

    /// A Deploy retires its old copies together, so each stop's grace must run beside
    /// the others, not after them.
    #[tokio::test]
    async fn stops_of_two_containers_on_one_machine_overlap() {
        let (data_dir, _store, service, fake) = fake_docker_service("ployzd-ensure-overlap").await;
        let first = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        let second = present(&ensure(&service, run_new("k2", spec("example.test/api:1"))).await)
            .container_id;
        let gone = |id| json!({"want":{"state":"gone", "container":{"id":id}, "drain":0}});
        let held = fake.stop_gate.write().await;

        let (one, two, overlapped) = tokio::join!(
            ensure(&service, gone(first)),
            ensure(&service, gone(second)),
            async {
                let both = tokio::time::timeout(Duration::from_secs(3), async {
                    for id in [first, second] {
                        super::set_volume_copy::requested(
                            &fake,
                            Method::POST,
                            &format!("/containers/{id}/stop"),
                        )
                        .await;
                    }
                })
                .await;
                drop(held);
                both.is_ok()
            }
        );
        assert!(overlapped, "one stop waited for the other to end");
        for reply in [one, two] {
            assert!(
                matches!(
                    reply,
                    EnsuredContainer::Ensured {
                        now: ContainerNow::Absent { .. },
                        ..
                    }
                ),
                "{reply:?}"
            );
        }
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A stop that is not a Volume cut still keeps a start out until its Container has
    /// exited, so nothing new runs beside a copy that may still write.
    #[tokio::test]
    async fn a_start_waits_for_a_stop_in_flight_to_end() {
        let (data_dir, _store, service, fake) =
            fake_docker_service("ployzd-ensure-start-after").await;
        let kept = present(&ensure(&service, run_new("k2", spec("example.test/api:1"))).await)
            .container_id;
        ensure(
            &service,
            json!({"want":{"state":"stopped", "container":{"id":kept}, "drain":0}}),
        )
        .await;
        let leaving = present(&ensure(&service, run_new("k1", spec("example.test/api:1"))).await)
            .container_id;
        let start = json!({"want":{"state":"running", "rotation":"pending",
            "container":{"existing":{"id":kept}}}});
        let held = fake.stop_gate.write().await;

        let start_path = format!("/containers/{kept}/start");
        let starts = || ends_with(&requests(&fake), Method::POST, &start_path).count();
        let before = starts();
        let (stopped, started) = tokio::join!(
            ensure(
                &service,
                json!({"want":{"state":"stopped", "container":{"id":leaving}, "drain":0}})
            ),
            async {
                super::set_volume_copy::requested(
                    &fake,
                    Method::POST,
                    &format!("/containers/{leaving}/stop"),
                )
                .await;
                let mut starting = std::pin::pin!(ensure(&service, start));
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), starting.as_mut())
                        .await
                        .is_err()
                );
                assert_eq!(
                    starts(),
                    before,
                    "the start reached Docker while the stop was in flight"
                );
                drop(held);
                starting.await
            }
        );
        assert!(matches!(
            present(&stopped).runtime,
            ContainerRuntimeObservation::Exited { .. }
        ));
        assert_eq!(starts(), before + 1);
        present(&started);
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
                verified: None,
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
            .observation()
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
        let (client, server) = crate::machine_api::test_socket::served(
            crate::machine_api::MachineApi::from_local(service),
            &crate::machine_api::test_socket::socket_in(&data_dir),
        )
        .await;

        let reply = client
            .max_decoding_message_size(ployz_core::RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
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
        let (mut client, server) = crate::machine_api::test_socket::served(
            crate::machine_api::MachineApi::from_local(service),
            &crate::machine_api::test_socket::socket_in(&data_dir),
        )
        .await;

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
        //! The Operator gate: a build an Operator may not start, and a new Container that
        //! runs only the spec an approved Device signed.

        use ployz_core::{
            DeviceKey, Namespace, OPERATOR_SCOPE_HEADER, SIGNED_VERSION_CAPABILITY,
            SealingPublicKey,
            authority::{PayloadDigest, ServiceVersion, Signed, Signer, Timestamp},
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
        }

        async fn enforced(prefix: &str) -> Enforced {
            enforce(fake_docker_service_keeping_runtime(prefix).await).await
        }

        /// An enforcing Machine whose runtime opens what its test Cluster seals.
        async fn enforced_sealing(prefix: &str) -> Enforced {
            let (data_dir, store, service, fake, runtime, _) =
                fake_docker_service_sealing(prefix).await;
            enforce((data_dir, store, service, fake, runtime)).await
        }

        async fn enforce(
            (data_dir, store, service, fake, runtime): (
                std::path::PathBuf,
                RecordOwner,
                MachineService,
                FakeDocker,
                crate::docker::ContainerRuntime,
            ),
        ) -> Enforced {
            let signer = Signer::from_seed(&[7; 32]);
            let device = DeviceKey::from(signer.public());
            store
                .mutate(move |store| {
                    store.approve_device(device).unwrap();
                    // A second key, so removing the signer never leaves the Machine keyless.
                    store
                        .approve_device(DeviceKey::from(Signer::from_seed(&[8; 32]).public()))
                        .unwrap();
                    store.trust_members(Vec::new()).unwrap();
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
            }
        }

        impl Enforced {
            /// Give the Machine a replica of its own, where an Operator's unsigned create
            /// looks up the names kept signed. Keep the returned server running.
            async fn with_replica(&mut self) -> tokio::task::JoinHandle<()> {
                let (replicated, server) = crate::corrosion::fake_cluster::store().await;
                let admin = crate::corrosion::AdminClient::new("/no/such/admin.sock");
                self.service.local = self
                    .service
                    .local
                    .clone()
                    .with_cluster(Some((replicated, admin)));
                server
            }

            /// The service a restarted daemon builds over the same record and Docker.
            fn restarted(&self) -> MachineService {
                let mut service = MachineService::with_cluster(self.store.clone(), None)
                    .with_optional_containers(Some(self.runtime.clone()));
                service.local = service.local.with_zpool("true");
                service
            }
        }

        fn started(id: &ContainerId) -> Value {
            json!({"state":"running", "rotation":"pending", "container":{"existing":{"id":id}}})
        }

        use crate::machine_api::test_socket::socket_in;

        /// A Machine's gRPC front over its local socket. A request with no Operator header
        /// reaches it as a Full caller, even under enforcement, and one with it as Cloud does.
        async fn served(
            service: &MachineService,
            socket: &std::path::Path,
        ) -> (
            MachineRpcClient<tonic::transport::Channel>,
            tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
        ) {
            crate::machine_api::test_socket::served(
                crate::machine_api::MachineApi::from_local(service.clone()),
                socket,
            )
            .await
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
                rule: None,
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

        /// An Operator's mint under a rule: the rule's checks run before the record opens.
        async fn mint_under_rule(
            fixture: &Enforced,
            record: &ployz_core::SealedRecord,
            jti: &str,
        ) -> Result<ployz_core::RpcResponse, Status> {
            use crate::machine::local_machine::rule_grant::tests as rule;
            let machine = fixture.service.local.record().id();
            let signed = rule::rule_for(fixture.cluster, machine, &fixture.signer, record);
            let token = rule::token_for(&rule::claim_for(fixture.cluster, &signed), jti);
            let request = op::MintBuildGrant::into_request(ployz_core::MintBuildGrantRequest {
                repository: ployz_core::BuildGrantRepository::parse("ployz-build/web").unwrap(),
                holder: Some(rule::holder()),
                sealed: Some(record.clone()),
                rule: Some(rule::grant_for(&signed, &fixture.signer, token)),
            })
            .encode()
            .unwrap();
            Ok(fixture
                .service
                .mint_build_grant(as_operator(Request::new(request), true))
                .await?
                .into_inner()
                .decode_response()
                .unwrap())
        }

        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
        async fn a_rule_mint_refuses_a_moved_head_before_the_sealed_record_opens() {
            use crate::machine::local_machine::rule_grant::tests as rule;
            let mut fixture = enforced_sealing("ployzd-enforce-rule-mint").await;
            let record = foreign_sealed();

            fixture.service.github = Some(Arc::new(rule::github(
                ployz_core::authority::CommitSha::from_bytes([9; 20]),
            )));
            let status = mint_under_rule(&fixture, &record, "moved")
                .await
                .expect_err("a head that moved must be refused");
            assert_eq!(
                status.code(),
                Code::PermissionDenied,
                "{}",
                status.message()
            );
            assert!(
                status.message().contains("a commit"),
                "{}",
                status.message()
            );
            let details = status_details(status.details()).unwrap();
            assert_eq!(
                details["capability"],
                json!(ployz_core::BUILD_GRANT_RULE_CAPABILITY)
            );

            // With the head where the token says, every check passes, the Operator's mint
            // gets through enforcement, and only then does the record fail to open.
            fixture.service.github = Some(Arc::new(rule::github(rule::commit())));
            opened_and_failed(&mint_under_rule(&fixture, &record, "at-head").await.unwrap());
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
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

        const PINNED: &str = "example.test/api@sha256:00000000000000000000000000000000000000000000000000000000000000aa";

        impl Enforced {
            /// A version of `spec` for its Service in Namespace `app`, signed by this
            /// Machine's Device.
            fn version(&self, spec: &Value) -> Signed<ServiceVersion> {
                self.version_at(spec, Timestamp::now(), false)
            }

            /// [`Self::version`] signed at `signed_at`, marked to forget older ones or not.
            fn version_at(
                &self,
                spec: &Value,
                signed_at: Timestamp,
                forget_older: bool,
            ) -> Signed<ServiceVersion> {
                let namespace = Namespace::parse("app").unwrap();
                let resolved: ployz_core::ResolvedServiceSpec =
                    serde_json::from_value(spec.clone()).unwrap();
                let body = ServiceVersion {
                    forget_older,
                    cluster: self.cluster,
                    digest: PayloadDigest::of(&resolved.to_requested(), &namespace).unwrap(),
                    namespace,
                    service: resolved.name,
                    signed_at,
                    signer: ployz_core::authority::SignerKey::Device(self.signer.public().into()),
                    proposal: None,
                    parent: None,
                };
                Signed::sign(&body, &self.signer).unwrap()
            }

            /// What this Machine answers an Operator's forget of `version`.
            async fn forget(
                &self,
                version: &Signed<ServiceVersion>,
            ) -> Vec<ployz_core::ForgetOutcome> {
                let mut request = Request::new(
                    op::ForgetVersions::into_request(ployz_core::ForgetVersionsRequest {
                        versions: vec![version.clone()],
                    })
                    .encode()
                    .unwrap(),
                );
                request
                    .metadata_mut()
                    .insert(OPERATOR_SCOPE_HEADER, "1".parse().unwrap());
                let response = self
                    .service
                    .forget_versions(request)
                    .await
                    .unwrap()
                    .into_inner()
                    .decode_response()
                    .unwrap();
                response.decode::<op::ForgetVersions>().unwrap().outcomes
            }
        }

        fn minutes_from_now(minutes: i64) -> Timestamp {
            let millis = Timestamp::now().as_millis().cast_signed() + minutes * 60_000;
            Timestamp::from_millis(millis.cast_unsigned()).unwrap()
        }

        #[tokio::test]
        async fn a_version_signed_before_a_forget_is_refused_and_the_forget_itself_runs() {
            let fixture = enforced("ployzd-forgotten-version").await;
            let approved = reaching(PINNED);
            let older = fixture.version_at(&approved, minutes_from_now(-1), false);
            let forget = fixture.version_at(&approved, Timestamp::now(), true);
            let seen = requests(&fixture.fake).len();

            deploy_ensure(
                &fixture.service,
                new_want("service_container", "k1", &approved),
                Some(&older),
            )
            .await
            .unwrap();
            assert!(matches!(
                fixture.forget(&forget).await.as_slice(),
                [ployz_core::ForgetOutcome::Raised { .. }]
            ));
            // The floor outlives the daemon, and holds on a Server the older one never ran on.
            let restarted = fixture.restarted();
            for (service, key) in [(&fixture.service, "k2"), (&restarted, "k3")] {
                deploy_refused(
                    deploy_ensure(
                        service,
                        new_want("service_container", key, &approved),
                        Some(&older),
                    )
                    .await,
                    "older versions were forgotten",
                );
            }
            deploy_ensure(
                &restarted,
                new_want("service_container", "k4", &approved),
                Some(&forget),
            )
            .await
            .unwrap();
            assert_eq!(creates(&fixture.fake, seen), 2);
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_deploy_marked_to_forget_raises_the_floor_before_the_forget_call_arrives() {
            let fixture = enforced("ployzd-forget-on-deploy").await;
            let approved = reaching(PINNED);
            let older = fixture.version_at(&approved, minutes_from_now(-1), false);
            let forget = fixture.version_at(&approved, Timestamp::now(), true);

            deploy_ensure(
                &fixture.service,
                new_want("service_container", "k1", &approved),
                Some(&forget),
            )
            .await
            .unwrap();
            deploy_refused(
                deploy_ensure(
                    &fixture.service,
                    new_want("service_container", "k2", &approved),
                    Some(&older),
                )
                .await,
                "older versions were forgotten",
            );
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        /// A creation at Full access would hide that the forget it carried was refused.
        #[tokio::test]
        async fn a_full_creation_whose_forget_is_refused_creates_nothing() {
            let fixture = enforced("ployzd-full-forget-refused").await;
            let approved = reaching(PINNED);
            let ahead = fixture.version_at(&approved, minutes_from_now(11), true);
            let seen = requests(&fixture.fake).len();

            deploy_refused(
                deploy_versions(
                    &fixture.service,
                    new_want("service_container", "k1", &approved),
                    &[ahead],
                    false,
                )
                .await,
                "ahead of this server's clock",
            );
            assert_eq!(creates(&fixture.fake, seen), 0);
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        /// A removed Device's old forget, still in the Config Store, never blocks a Full
        /// creation.
        #[tokio::test]
        async fn a_full_creation_beside_a_forget_no_trusted_key_signed_creates() {
            let fixture = enforced("ployzd-full-stranger-forget").await;
            let approved = reaching(PINNED);
            let stranger = Signer::from_seed(&[42; 32]);
            let mut body = fixture
                .version_at(&approved, Timestamp::now(), true)
                .open()
                .unwrap();
            body.signer = ployz_core::authority::SignerKey::Device(stranger.public().into());
            let forget = Signed::sign(&body, &stranger).unwrap();
            let seen = requests(&fixture.fake).len();

            deploy_versions(
                &fixture.service,
                new_want("service_container", "k1", &approved),
                &[forget],
                false,
            )
            .await
            .unwrap();
            assert_eq!(creates(&fixture.fake, seen), 1);
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_version_signed_ahead_never_hides_one_the_same_key_signed_on_time() {
            let fixture = enforced("ployzd-ahead-beside-honest").await;
            let approved = reaching(PINNED);
            let honest = fixture.version_at(&approved, Timestamp::now(), false);
            let ahead = fixture.version_at(&approved, minutes_from_now(11), false);

            deploy_versions(
                &fixture.service,
                new_want("service_container", "k1", &approved),
                &[honest, ahead],
                true,
            )
            .await
            .unwrap();
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        /// Floors are kept by Cluster, so a Server that can't read its Sealing Key file
        /// refuses an Operator's start rather than read no floor.
        #[tokio::test]
        async fn an_operator_start_is_refused_while_the_sealing_key_file_is_unreadable() {
            let fixture = enforced("ployzd-floor-without-key").await;
            let service = &fixture.service;
            let approved = reaching(PINNED);
            let older = fixture.version_at(&approved, minutes_from_now(-1), false);
            let created = deploy_ensure(
                service,
                new_want("service_container", "k1", &approved),
                Some(&older),
            )
            .await
            .unwrap();
            let id = present(&created).container_id;
            let stopped = json!({"state":"stopped", "container":{"id":id}, "drain":0});
            deploy_ensure(service, stopped, None).await.unwrap();
            let forget = fixture.version_at(&approved, Timestamp::now(), true);
            assert!(matches!(
                fixture.forget(&forget).await.as_slice(),
                [ployz_core::ForgetOutcome::Raised { .. }]
            ));

            std::fs::write(fixture.data_dir.join("sealing.key"), b"bad").unwrap();
            deploy_refused(
                deploy_ensure(service, started(&id), None).await,
                "can't check the deploy's approval",
            );
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_forget_signed_eleven_minutes_ahead_is_refused_and_leaves_the_floor() {
            let fixture = enforced("ployzd-forget-ahead").await;
            let approved = reaching(PINNED);
            let ahead = fixture.version_at(&approved, minutes_from_now(11), true);

            match fixture.forget(&ahead).await.as_slice() {
                [ployz_core::ForgetOutcome::Refused { reason }] => {
                    assert!(reason.contains("ahead of this server's clock"), "{reason}");
                }
                other => panic!("a forget past the guard was taken: {other:?}"),
            }
            deploy_ensure(
                &fixture.service,
                new_want("service_container", "k1", &approved),
                Some(&fixture.version(&approved)),
            )
            .await
            .unwrap();
            std::fs::remove_dir_all(&fixture.data_dir).unwrap();
        }

        async fn deploy_ensure(
            service: &MachineService,
            want: Value,
            version: Option<&Signed<ServiceVersion>>,
        ) -> Result<EnsuredContainer, Status> {
            let versions = version.map_or(&[][..], std::slice::from_ref);
            deploy_versions(service, want, versions, true).await
        }

        /// Ensure `want` with `versions`, as an Operator or as a Full caller.
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn deploy_versions(
            service: &MachineService,
            want: Value,
            versions: &[Signed<ServiceVersion>],
            operator: bool,
        ) -> Result<EnsuredContainer, Status> {
            let mut body = json!({"want": want});
            if !versions.is_empty() {
                body["versions"] = json!(versions);
            }
            let request = Request::new(
                op::EnsureContainer::into_request(serde_json::from_value(body).unwrap())
                    .encode()
                    .unwrap(),
            );
            let response = service
                .ensure_container(as_operator(request, operator))
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
                json!(SIGNED_VERSION_CAPABILITY)
            );
        }

        fn new_want(kind: &str, key: &str, spec: &Value) -> Value {
            json!({"state":"running", "rotation":"pending", "container":{"new":{
                "slot":{"namespace":"app", "service":spec["name"], "kind":kind, "creation_key":key},
                "resolved_spec":spec, "gen":1}}})
        }

        fn creates(fake: &crate::docker::test_support::FakeDocker, since: usize) -> usize {
            ends_with(
                &requests_since(fake, since),
                Method::POST,
                "/containers/create",
            )
            .count()
        }

        /// A pinned image on a registry at a public address. A Machine pulls the image of
        /// a creation nothing signed itself, and only from such an address.
        const PUBLIC: &str =
            "8.8.8.8/api@sha256:00000000000000000000000000000000000000000000000000000000000000bb";

        fn pulls(fake: &crate::docker::test_support::FakeDocker, since: usize) -> usize {
            ends_with(&requests_since(fake, since), Method::POST, "/images/create").count()
        }

        /// `image`'s spec, privileged, so it reaches its Machine.
        fn reaching(image: &str) -> Value {
            let mut spec = spec(image);
            *spec
                .pointer_mut("/container/privileged")
                .expect("the fixture names privileged") = json!(true);
            spec
        }

        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn an_operator_runs_a_spec_that_reaches_no_data_with_no_signature() {
            let mut fixture = enforced("ployzd-free-spec").await;
            let _replica = fixture.with_replica().await;
            let service = &fixture.service;
            let free = spec(PUBLIC);
            let seen = requests(&fixture.fake).len();

            // The caller's word that the image is already here is not taken.
            let mut first = new_want("service_container", "k1", &free);
            first["container"]["new"]["image_prepared"] = json!(true);
            present(&deploy_ensure(service, first, None).await.unwrap());
            let second = new_want("service_container", "k2", &free);
            present(&deploy_ensure(service, second, None).await.unwrap());

            assert_eq!(creates(&fixture.fake, seen), 2);
            assert_eq!(
                pulls(&fixture.fake, seen),
                1,
                "this Machine pulls the image itself, once"
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn an_operator_spec_that_reaches_data_is_refused_with_no_signature() {
            let fixture = enforced("ployzd-data-spec").await;
            let service = &fixture.service;
            let cluster = crate::sealed_env::test_support::Cluster::new();
            let mut sealed = spec(PUBLIC);
            sealed["container"]["environment"] = json!({
                "TOKEN": ployz_core::EnvValue::Sealed(cluster.seal(
                    "TOKEN",
                    ployz_core::SealedScope::Shared,
                    "t0ken",
                )),
            });
            let volume = spec_with(
                PUBLIC,
                vec![crate::docker::test_support::provisioned_source(
                    "data",
                    1_073_741_824,
                )],
            );
            let seen = requests(&fixture.fake).len();

            for (spec, reach) in [
                (sealed, "opens a secret"),
                (volume, "mounts a volume"),
                (reaching(PUBLIC), "reaches its server"),
            ] {
                deploy_refused(
                    deploy_ensure(service, new_want("service_container", "k1", &spec), None).await,
                    reach,
                );
            }
            let mut login = new_want("service_container", "k1", &spec(PUBLIC));
            login["container"]["new"]["registry_auth"] = json!({"password": "p"});
            deploy_refused(
                deploy_ensure(service, login, None).await,
                "pulls its image with a login",
            );
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing reached Docker");
            assert_eq!(pulls(&fixture.fake, seen), 0, "nothing reached Docker");
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_signed_spec_runs_for_an_operator_only_with_its_version_until_the_device_is_removed()
         {
            let fixture = enforced("ployzd-signed-spec").await;
            let service = &fixture.service;
            let approved = reaching(PINNED);
            let signed = fixture.version(&approved);
            let seen = requests(&fixture.fake).len();

            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &reaching(PUBLIC)),
                    Some(&signed),
                )
                .await,
                "settings differ from what the device approved",
            );
            let tagged = reaching("example.test/api:1");
            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k1", &tagged),
                    Some(&fixture.version(&tagged)),
                )
                .await,
                "runs only images pinned by digest",
            );
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing reached Docker");

            // A scale and a restarted daemon each carry the version again.
            let restarted = fixture.restarted();
            for (service, key) in [(service, "k1"), (service, "k2"), (&restarted, "k3")] {
                deploy_ensure(
                    service,
                    new_want("service_container", key, &approved),
                    Some(&signed),
                )
                .await
                .unwrap();
            }
            assert_eq!(creates(&fixture.fake, seen), 3);
            // What it admitted before admits nothing more: no Server keeps a version.
            for service in [service, &restarted] {
                deploy_refused(
                    deploy_ensure(
                        service,
                        new_want("service_container", "k4", &approved),
                        None,
                    )
                    .await,
                    "reaches its server",
                );
            }

            let code = DeviceKey::from(fixture.signer.public()).code();
            fixture
                .store
                .mutate(move |store| {
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), code)
                        .unwrap();
                })
                .await
                .unwrap();
            deploy_refused(
                deploy_ensure(
                    service,
                    new_want("service_container", "k5", &approved),
                    Some(&signed),
                )
                .await,
                "doesn't trust the key that approved",
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        /// A Full caller, such as `ployz deploy` over SSH, that carries a Device's version
        /// keeps that Device behind the Container, so an Operator start stops once it is
        /// removed, as for a Container an Operator created.
        #[tokio::test]
        async fn a_full_creation_that_carries_a_version_starts_for_an_operator_only_while_its_signer_is_trusted()
         {
            let fixture = enforced("ployzd-full-signed-spec").await;
            let service = &fixture.service;
            let approved = reaching(PINNED);
            let created = ensure(
                service,
                json!({
                    "want": new_want("service_container", "k1", &approved),
                    "versions": [fixture.version(&approved)],
                }),
            )
            .await;
            let id = present(&created).container_id;
            let stopped = json!({"state":"stopped", "container":{"id":id}, "drain":0});
            deploy_ensure(service, stopped.clone(), None).await.unwrap();
            deploy_ensure(service, started(&id), None).await.unwrap();

            let code = DeviceKey::from(fixture.signer.public()).code();
            fixture
                .store
                .mutate(move |store| {
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), code)
                        .unwrap();
                })
                .await
                .unwrap();
            deploy_ensure(service, stopped, None).await.unwrap();
            deploy_refused(
                deploy_ensure(service, started(&id), None).await,
                "was rotated or removed",
            );
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_device_removed_after_its_call_arrived_creates_nothing_on_its_own_connection() {
            let fixture = enforced("ployzd-device-connection-spec").await;
            let service = &fixture.service;
            let pinned = reaching(PINNED);
            let device = DeviceKey::from(fixture.signer.public());
            let direct = service
                .clone()
                .with_management_client(ployz_core::ClientKey::from(device));

            ensure(&direct, run_new("k1", pinned.clone())).await;
            fixture
                .store
                .mutate(move |store| {
                    store
                        .remove_by_code(&crate::machine::KeyChangeHeld::for_test(), device.code())
                        .unwrap();
                })
                .await
                .unwrap();
            // A call the Device sent before it was removed is answered as a removed key's.
            let late = direct
                .ensure_container(Request::new(
                    op::EnsureContainer::into_request(
                        serde_json::from_value(run_new("k3", pinned.clone())).unwrap(),
                    )
                    .encode()
                    .unwrap(),
                ))
                .await;
            assert_eq!(late.unwrap_err().code(), Code::Unauthenticated);
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn full_access_runs_a_spec_no_key_can_sign() {
            let fixture = enforced("ployzd-unsignable-spec").await;
            // A negative number, Docker's "no limit", has no place in a signed body.
            let mut unlimited = spec(PINNED);
            unlimited["container"]["resources"]["ulimits"] =
                json!({"nofile": {"soft": -1, "hard": -1}});

            present(&ensure(&fixture.service, run_new("k1", unlimited)).await);
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn an_unsigned_creation_follows_its_pull_policy() {
            let mut fixture = enforced("ployzd-free-pull-policy").await;
            let _replica = fixture.with_replica().await;
            let service = &fixture.service;
            let with_policy = |image: &str, policy: &str| {
                let mut spec = spec(image);
                spec["container"]["pull_policy"] = json!(policy);
                spec
            };
            let seen = requests(&fixture.fake).len();

            // `always` asks the registry each time: a tag may have moved.
            for key in ["k1", "k2"] {
                let always = with_policy(PUBLIC, "always");
                deploy_ensure(service, new_want("service_container", key, &always), None)
                    .await
                    .unwrap();
            }
            assert_eq!(pulls(&fixture.fake, seen), 2);

            // `never` runs only an image this Machine already pulled itself.
            let here = with_policy(PUBLIC, "never");
            deploy_ensure(service, new_want("service_container", "k3", &here), None)
                .await
                .unwrap();
            let absent = "8.8.8.8/api@sha256:00000000000000000000000000000000000000000000000000000000000000cc";
            fixture
                .fake
                .images
                .lock()
                .unwrap()
                .insert(absent.into(), None);
            let elsewhere = with_policy(absent, "never");
            let refused = deploy_ensure(
                service,
                new_want("service_container", "k4", &elsewhere),
                None,
            )
            .await
            .unwrap();
            assert!(
                format!("{refused:?}").contains("its pull policy is never"),
                "{refused:?}"
            );
            assert_eq!(pulls(&fixture.fake, seen), 2);
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        /// The image a Free Build loaded runs unsigned only by a receipt this Cluster
        /// made for that content, from what this Server already holds.
        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn an_unsigned_creation_runs_a_free_builds_image_only_by_its_clusters_receipt() {
            let (data_dir, store, service, fake, runtime, cluster) =
                fake_docker_service_sealing("ployzd-free-receipt").await;
            let mut fixture = enforce((data_dir, store, service, fake, runtime)).await;
            let _replica = fixture.with_replica().await;
            let service = &fixture.service;
            let built =
                ployz_core::ImageDigest::parse(format!("sha256:{}", "d".repeat(64))).unwrap();
            let other =
                ployz_core::ImageDigest::parse(format!("sha256:{}", "e".repeat(64))).unwrap();
            let receipt = cluster.store().free_receipt(&built).unwrap();
            let carrying = |key: &str, receipt: &ployz_core::FreeReceipt| {
                let mut want = new_want("service_container", key, &spec(PUBLIC));
                want["container"]["new"]["free_receipt"] = json!(receipt);
                want
            };
            let created_images = |since: usize| -> Vec<Value> {
                fixture.fake.request_bodies.lock().unwrap()[since..]
                    .iter()
                    .filter(|(path, _)| path.ends_with("/containers/create"))
                    .map(|(_, body)| body["Image"].clone())
                    .collect()
            };
            let refused = |reply: EnsuredContainer, why: &str| {
                assert!(format!("{reply:?}").contains(why), "{reply:?}");
            };
            let seen = requests(&fixture.fake).len();
            let bodies = fixture.fake.request_bodies.lock().unwrap().len();

            // Another Cluster's receipt, or this one's moved onto other content, runs nothing.
            let elsewhere = crate::sealed_env::test_support::Cluster::new()
                .store()
                .free_receipt(&built)
                .unwrap();
            let moved = ployz_core::FreeReceipt {
                image: other.clone(),
                mac: receipt.mac,
            };
            for (key, forged) in [("k1", &elsewhere), ("k2", &moved)] {
                refused(
                    deploy_ensure(service, carrying(key, forged), None)
                        .await
                        .unwrap(),
                    "no Server of this Cluster made the receipt",
                );
            }
            assert_eq!(creates(&fixture.fake, seen), 0);

            // The receipt runs exactly its image, never pulled, and again for the same
            // content under another slot.
            for key in ["k3", "k4"] {
                present(
                    &deploy_ensure(service, carrying(key, &receipt), None)
                        .await
                        .unwrap(),
                );
            }
            assert_eq!(
                created_images(bodies),
                [json!(built.as_str()), json!(built.as_str())]
            );
            assert_eq!(pulls(&fixture.fake, seen), 0);

            // A receipt for content this Server doesn't hold, as after a copy that broke off,
            // makes nothing and pulls nothing.
            fixture
                .fake
                .images
                .lock()
                .unwrap()
                .insert(other.as_str().into(), None);
            let absent = cluster.store().free_receipt(&other).unwrap();
            refused(
                deploy_ensure(service, carrying("k5", &absent), None)
                    .await
                    .unwrap(),
                "isn't on this Server",
            );
            assert_eq!(creates(&fixture.fake, seen), 2);
            assert_eq!(pulls(&fixture.fake, seen), 0);
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        /// A slot that already holds a Free Build's image answers a receipt only when it
        /// names that image: never as already running, and never by starting what is there.
        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn a_reused_slot_answers_a_receipt_only_for_the_image_it_holds() {
            let (data_dir, store, service, fake, runtime, cluster) =
                fake_docker_service_sealing("ployzd-free-reuse").await;
            let mut fixture = enforce((data_dir, store, service, fake, runtime)).await;
            let _replica = fixture.with_replica().await;
            let service = &fixture.service;
            let built =
                ployz_core::ImageDigest::parse(format!("sha256:{}", "d".repeat(64))).unwrap();
            let other =
                ployz_core::ImageDigest::parse(format!("sha256:{}", "e".repeat(64))).unwrap();
            let receipt = cluster.store().free_receipt(&built).unwrap();
            let elsewhere = cluster.store().free_receipt(&other).unwrap();
            let forged = ployz_core::FreeReceipt {
                image: built.clone(),
                mac: elsewhere.mac,
            };
            // One spec, so each request names the same creation in the slot.
            let public = spec(PUBLIC);
            let carrying = |key: &str, receipt: &ployz_core::FreeReceipt| {
                let mut want = new_want("service_container", key, &public);
                want["container"]["new"]["free_receipt"] = json!(receipt);
                want
            };
            for state in ["running", "exited"] {
                let key = format!("k-{state}");
                let first = present(
                    &deploy_ensure(service, carrying(&key, &receipt), None)
                        .await
                        .unwrap(),
                )
                .container_id;
                for container in fixture
                    .fake
                    .named_containers
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .values_mut()
                    .filter(|container| container["Id"] == json!(first.as_str()))
                {
                    container["State"] = json!({"Status": state});
                }
                let seen = requests(&fixture.fake).len();

                for (wrong, why) in [
                    (&elsewhere, "not the pinned"),
                    (&forged, "no Server of this Cluster made the receipt"),
                ] {
                    let reply = deploy_ensure(service, carrying(&key, wrong), None)
                        .await
                        .unwrap();
                    assert!(format!("{reply:?}").contains(why), "{state}: {reply:?}");
                }
                assert_eq!(
                    ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                    0,
                    "{state}: nothing started"
                );
                assert_eq!(creates(&fixture.fake, seen), 0, "{state}: nothing made");
            }
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        /// A pin and a Free receipt that name different images settle nothing: an empty
        /// slot makes neither, and a slot holding the pinned image neither answers it as
        /// running nor starts it.
        #[tokio::test]
        #[expect(clippy::indexing_slicing, reason = "JSON fixture")]
        async fn a_pin_and_a_receipt_for_different_images_run_neither() {
            let (data_dir, store, service, fake, runtime, cluster) =
                fake_docker_service_sealing("ployzd-free-pin-conflict").await;
            let mut fixture = enforce((data_dir, store, service, fake, runtime)).await;
            let _replica = fixture.with_replica().await;
            let service = &fixture.service;
            let a = ployz_core::ImageDigest::parse(format!("sha256:{}", "d".repeat(64))).unwrap();
            let b = ployz_core::ImageDigest::parse(format!("sha256:{}", "e".repeat(64))).unwrap();
            let receipt_a = cluster.store().free_receipt(&a).unwrap();
            let receipt_b = cluster.store().free_receipt(&b).unwrap();
            // A tag, as the pinned route takes.
            let tagged = spec("8.8.8.8/api:1");
            let pinned = |key: &str| {
                let mut want = new_want("service_container", key, &tagged);
                let new = &mut want["container"]["new"];
                new["image_id"] = json!(a);
                new["free_receipt"] = json!(receipt_b);
                new["image_prepared"] = json!(true);
                new["answer"] = json!("settled");
                want
            };
            let pinned_ensure = |want: Value| async move {
                let request = Request::new(
                    op::EnsurePinnedContainer::into_request(
                        serde_json::from_value(json!({"want": want})).unwrap(),
                    )
                    .encode()
                    .unwrap(),
                );
                service
                    .ensure_pinned_container(as_operator(request, true))
                    .await
                    .unwrap()
                    .into_inner()
                    .decode_response()
                    .unwrap()
                    .decode::<op::EnsurePinnedContainer>()
                    .unwrap()
            };
            let differ = "differ; send one";

            let seen = requests(&fixture.fake).len();
            let empty = pinned_ensure(pinned("k-empty")).await;
            assert!(format!("{empty:?}").contains(differ), "{empty:?}");
            assert_eq!(
                creates(&fixture.fake, seen),
                0,
                "an empty slot makes nothing"
            );

            // The slot already holds A, made from A's receipt.
            let mut held = new_want("service_container", "k-held", &tagged);
            held["container"]["new"]["free_receipt"] = json!(receipt_a);
            present(&deploy_ensure(service, held, None).await.unwrap());
            let seen = requests(&fixture.fake).len();
            let reused = pinned_ensure(pinned("k-held")).await;
            assert!(format!("{reused:?}").contains(differ), "{reused:?}");
            assert_eq!(
                ends_with(&requests_since(&fixture.fake, seen), Method::POST, "/start").count(),
                0,
                "nothing started"
            );
            assert_eq!(creates(&fixture.fake, seen), 0, "nothing made");
            std::fs::remove_dir_all(fixture.data_dir).unwrap();
        }

        #[tokio::test]
        async fn a_signed_hook_never_starts_again_but_adopts_a_resent_one() {
            let fixture = enforced("ployzd-deploy-hook-start").await;
            let approved = spec(PINNED);
            let signed = fixture.version(&approved);
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
        fake_cluster::{corrosion_db, identity_store},
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
        let mut local = enrolled(dir);
        local
            .trust_members(rows.iter().map(Into::into).collect())
            .unwrap();
        let service = MachineService::with_cluster(
            RecordOwner::spawn(local).unwrap(),
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

mod cluster_doors {
    use ployz_core::{
        ClusterDoorsDeclared, IngressDoors, RpcResponse, RpcResponseBody, SetClusterDoorsRequest,
    };

    use super::*;

    async fn declare(service: &MachineService, doors: IngressDoors) -> RpcResponse {
        let request = op::SetClusterDoors::into_request(SetClusterDoorsRequest { doors });
        service
            .set_cluster_doors(Request::new(request.encode().unwrap()))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
    }

    #[tokio::test]
    async fn a_known_set_is_declared_and_answered_back() {
        let dir = std::env::temp_dir().join(format!("ployzd-doors-{}", MachineId::random()));
        std::fs::create_dir_all(&dir).unwrap();
        let (service, task) = super::read_routes::serve(&dir, &[]).await;

        for doors in [IngressDoors::tunnel(), IngressDoors::closed()] {
            let response = declare(&service, doors.clone()).await;
            let shown = format!("{response:?}");
            let declared = response
                .decode::<op::SetClusterDoors>()
                .unwrap_or_else(|error| panic!("{error:?}: {shown}"));
            assert_eq!(
                service.local.declared_doors().await,
                Some(doors.clone()),
                "the key holds what was declared"
            );
            assert_eq!(declared, ClusterDoorsDeclared { doors });
        }
        task.abort();
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_door_this_release_does_not_know_is_refused_and_nothing_is_declared() {
        let dir = std::env::temp_dir().join(format!("ployzd-doors-{}", MachineId::random()));
        std::fs::create_dir_all(&dir).unwrap();
        let (service, task) = super::read_routes::serve(&dir, &[]).await;
        let unknown: IngressDoors = serde_json::from_str(r#"["tunnel","warp"]"#).unwrap();

        let response = declare(&service, unknown).await;

        let RpcResponseBody::Error(error) = response.body else {
            panic!("expected a refusal, got {response:?}");
        };
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert_eq!(
            error.details.get("capability"),
            Some(&serde_json::json!(ployz_core::SET_CLUSTER_DOORS_CAPABILITY))
        );
        assert_eq!(service.local.declared_doors().await, None);
        task.abort();
        std::fs::remove_dir_all(dir).ok();
    }
}
