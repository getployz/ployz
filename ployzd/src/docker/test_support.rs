//! Stateful fake-Docker support for Volume tests.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use bollard::Docker;

use super::*;
use ployz_core::{Machine, MachineGateway, MachineId, ResolvedServiceSpec, VolumeSource};

#[derive(Clone, Default)]
pub(super) struct FakeDocker {
    pub(super) requests: Arc<Mutex<Vec<(Method, String)>>>,
    pub(super) request_bodies: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    pub(super) volumes: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    pub(super) fail_after_create: Arc<Mutex<BTreeSet<String>>>,
    pub(super) fail_inspect_once: Arc<Mutex<BTreeSet<String>>>,
    pub(super) existing_container: Arc<Mutex<Option<serde_json::Value>>>,
    pub(super) reject_list: Arc<AtomicBool>,
}

async fn fake_docker(
    State(fake): State<FakeDocker>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    fake.requests
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    if !body.is_empty() {
        fake.request_bodies.lock().unwrap().push((
            path.clone(),
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        ));
    }
    let response = if method == Method::GET && path.ends_with("/containers/json") {
        let listed = fake
                .existing_container
                .lock()
                .unwrap()
                .as_ref()
                .map(|container| {
                    serde_json::json!([{"Id":container.get("Id").expect("fixture has ID")}])
                })
                .unwrap_or_else(|| serde_json::json!([]));
        (StatusCode::OK, listed)
    } else if method == Method::GET && path.contains("/containers/") && path.ends_with("/json") {
        fake.existing_container.lock().unwrap().clone().map_or_else(
            || {
                (
                    StatusCode::NOT_FOUND,
                    serde_json::json!({"message":"no such container"}),
                )
            },
            |container| (StatusCode::OK, container),
        )
    } else if method == Method::GET && path.contains("/images/") && path.ends_with("/json") {
        (StatusCode::OK, serde_json::json!({}))
    } else if method == Method::GET
        && path.ends_with("/volumes")
        && fake.reject_list.load(Ordering::Relaxed)
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"message":"collection unavailable"}),
        )
    } else if method == Method::GET && path.ends_with("/volumes") {
        (
            StatusCode::OK,
            serde_json::json!({"Volumes":[
                {"Name":"plain","Driver":"local","Mountpoint":"/var/lib/docker/volumes/plain/_data"},
                {"Name":"healthy","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/healthy"},
                {"Name":"malformed","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/malformed"},
                {"Name":"unavailable","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/unavailable"},
                {"Name":"mismatched","Driver":"ployz","Mountpoint":"/var/lib/ployz-volumes/mismatched"}
            ]}),
        )
    } else if method == Method::GET && path.contains("/volumes/") {
        let name = path.rsplit('/').next().unwrap();
        if fake.fail_inspect_once.lock().unwrap().remove(name) {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"detail temporarily unavailable"}),
            )
        } else if let Some(volume) = fake.volumes.lock().unwrap().get(name).cloned() {
            (StatusCode::OK, volume)
        } else if ["healthy", "data", "cache"].contains(&name) {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":name,
                    "Driver":"ployz",
                    "Mountpoint":format!("/var/lib/ployz-volumes/{name}"),
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else if name == "malformed" {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"malformed",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/malformed",
                    "Status":{"bound_bytes":"not-a-number","used_bytes":4096}
                }),
            )
        } else if name == "unavailable" {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"detail unavailable"}),
            )
        } else if name == "mismatched" {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"other",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/other",
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else {
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"message":format!("no such volume: {name}")}),
            )
        }
    } else if method == Method::POST
        && path.ends_with("/volumes/create")
        && String::from_utf8_lossy(&body).contains("rejected")
    {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"message":"create rejected"}),
        )
    } else if method == Method::POST && path.ends_with("/volumes/create") {
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let name = request
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_owned();
        if name == "unavailable" {
            (
                StatusCode::CREATED,
                serde_json::json!({
                    "Name":"unavailable",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/unavailable"
                }),
            )
        } else {
            let driver = request
                .get("Driver")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("local");
            let mut observed = serde_json::json!({
                "Name":name,
                "Driver":driver,
                "Mountpoint":format!("/volumes/{name}"),
                "Options":request.get("DriverOpts").cloned().unwrap_or_default(),
                "Labels":request.get("Labels").cloned().unwrap_or_default(),
            });
            if driver == ployz_core::PROVISIONED_VOLUME_DRIVER {
                let bound = request
                    .get("DriverOpts")
                    .and_then(|options| options.get("size"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .trim_end_matches('b')
                    .parse::<u64>()
                    .unwrap();
                observed.as_object_mut().unwrap().insert(
                    "Status".into(),
                    serde_json::json!({"bound_bytes":bound,"used_bytes":0}),
                );
            }
            fake.volumes
                .lock()
                .unwrap()
                .insert(name.clone(), observed.clone());
            if fake.fail_after_create.lock().unwrap().contains(&name) {
                fake.fail_inspect_once.lock().unwrap().insert(name.clone());
            }
            (StatusCode::CREATED, observed)
        }
    } else if method == Method::POST && (path.ends_with("/start") || path.ends_with("/stop")) {
        (StatusCode::NO_CONTENT, serde_json::Value::Null)
    } else if method == Method::DELETE && path.contains("/containers/") {
        fake.existing_container.lock().unwrap().take();
        (StatusCode::NO_CONTENT, serde_json::Value::Null)
    } else {
        (
            StatusCode::NOT_FOUND,
            serde_json::json!({"message":format!("unhandled {method} {path}")}),
        )
    };
    (response.0, Json(response.1)).into_response()
}

pub(super) async fn fake_runtime() -> (ContainerRuntime, FakeDocker) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fake = FakeDocker::default();
    tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/{*path}", any(fake_docker))
                .with_state(fake.clone()),
        )
        .into_future(),
    );
    let docker = Docker::connect_with_http(
        &format!("http://{address}"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap();
    let specs = MachineSpecStore::open(std::env::temp_dir().join(format!(
        "ployzd-volume-observation-{}.db",
        MachineId::random()
    )))
    .await
    .unwrap();
    (
        ContainerRuntime::new(LocalDocker::from_client(docker), specs),
        fake,
    )
}

pub(super) fn machine() -> Machine {
    crate::docker::lifecycle::test_machine(
        MachineId::random(),
        MachineGateway("10.210.0.1".parse().unwrap()),
    )
}

pub(super) fn container_request<'spec, Storage>(
    kind: ContainerKind,
    project_name: &'spec ProjectName,
    spec: &'spec ResolvedServiceSpec,
    storage: Storage,
) -> ContainerRequest<'spec, Storage, std::future::Ready<Result<NetworkAttachment, Error>>> {
    ContainerRequest {
        kind,
        project_name,
        spec,
        network: std::future::ready(Ok(NetworkAttachment::Host)),
        storage,
    }
}

pub(super) fn global_slot_request<'spec, Storage>(
    project_name: &'spec ProjectName,
    spec: &'spec ResolvedServiceSpec,
    storage: Storage,
) -> GlobalSlotRequest<'spec, Storage, std::future::Ready<Result<NetworkAttachment, Error>>> {
    GlobalSlotRequest {
        project_name,
        spec,
        network: std::future::ready(Ok(NetworkAttachment::Host)),
        storage,
    }
}

pub(super) fn provisioned_source(name: &str, maximum_bytes: u64) -> VolumeSource {
    let mut source = ployz_core::RawVolumeSource::Provisioned {
        name: DockerVolumeName::parse(name).unwrap(),
        maximum_bytes: ployz_core::ProvisionedVolumeMaximumBytes::new(
            std::num::NonZeroU64::new(maximum_bytes).unwrap(),
        ),
        labels: BTreeMap::from([("backup".into(), "daily".into())]),
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_project(&ProjectName::parse("app").unwrap());
    source
}

pub(super) fn ordinary_source(name: &str) -> VolumeSource {
    let mut source = ployz_core::RawVolumeSource::Ordinary {
        name: DockerVolumeName::parse(name).unwrap(),
        driver: ployz_core::VolumeDriver::parse(
            "example-driver",
            BTreeMap::from([("mode".into(), "safe".into())]),
        )
        .unwrap(),
        labels: BTreeMap::from([("backup".into(), "daily".into())]),
    }
    .admit()
    .expect("valid volume declaration");
    source.scope_to_project(&ProjectName::parse("app").unwrap());
    source
}

pub(super) fn spec_with_sources(sources: Vec<VolumeSource>) -> ResolvedServiceSpec {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ployz_core::ServiceId::random(),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/api", "pull_policy": "missing" },
        "pre_deploy": { "command": ["true"] }
    }))
    .unwrap();
    let volumes = sources
        .into_iter()
        .enumerate()
        .map(|(index, source)| ployz_core::ServiceVolume {
            reference: ployz_core::ServiceVolumeReference::parse(format!("v{index}")).unwrap(),
            source,
        })
        .collect::<Vec<_>>();
    let mounts = volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| ployz_core::ServiceMount {
            volume: volume.reference.clone(),
            target: ployz_core::ContainerPath::parse(format!("/data/{index}")).unwrap(),
            read_only: false,
            no_copy: false,
            subpath: None,
        })
        .collect();
    spec.set_volume_graph(
        ployz_core::ServiceVolumeGraph::parse(volumes, mounts)
            .unwrap()
            .scope_to_project(&ProjectName::parse("app").unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    spec
}
