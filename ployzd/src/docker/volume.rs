use std::collections::HashMap;

use bollard::{
    models::{Volume, VolumeCreateRequest},
    query_parameters::RemoveVolumeOptionsBuilder,
};
use futures_util::{StreamExt, stream};
use ployz_core::{
    CreateVolumeReport, CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, MachineId, VolumeInventory, VolumeObservationFailure,
};
use serde::Deserialize;
use serde_json::Value;

use super::{ContainerRuntime, Error, some_map};
use crate::VolumePluginStatus;

const VOLUME_INSPECTION_CONCURRENCY: usize = 8;

impl ContainerRuntime {
    /// Create a Docker Volume and verify its observable state when possible.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the create request. A successful create whose
    /// verification fails is returned as [`CreateVolumeReport::Unverified`].
    pub async fn create_volume(
        &self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<CreateVolumeReport, Error> {
        let docker_name = request.name.to_string();
        let id = DockerVolumeId {
            machine_id: *machine_id,
            name: request.name,
        };
        decode_volume(
            self.docker
                .client
                .create_volume(VolumeCreateRequest {
                    name: Some(docker_name),
                    driver: Some(request.driver),
                    driver_opts: some_map(request.options),
                    labels: some_map(request.labels),
                    ..Default::default()
                })
                .await,
        )?;
        Ok(match self.inspect_volume(machine_id, &id.name).await {
            Ok(volume) => CreateVolumeReport::Verified { volume },
            Err(error) => CreateVolumeReport::Unverified {
                id,
                error: (&error).into(),
            },
        })
    }

    /// List Docker Volumes, retaining individual detail failures alongside healthy observations.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker rejects the collection request or listed inventory is invalid.
    pub async fn list_volumes(&self, machine_id: &MachineId) -> Result<VolumeInventory, Error> {
        let listed = decode_volume_list(
            self.docker
                .client
                .list_volumes(None::<bollard::query_parameters::ListVolumesOptions>)
                .await,
        )?;
        let mut inventory = VolumeInventory::default();
        let mut observations = stream::iter(listed.into_iter().map(|volume| async move {
            let id = docker_volume_id(machine_id, &volume.name)?;
            let observation = if volume.driver == "ployz" {
                self.inspect_volume(machine_id, &id.name).await
            } else {
                docker_volume(machine_id, volume)
            };
            Ok::<_, Error>((id, observation))
        }))
        .buffered(VOLUME_INSPECTION_CONCURRENCY);
        while let Some(observation) = observations.next().await {
            let (id, observation) = observation?;
            match observation {
                Ok(volume) => inventory.volumes.push(volume),
                Err(error) => inventory.failures.push(VolumeObservationFailure {
                    id,
                    error: (&error).into(),
                }),
            }
        }
        Ok(inventory)
    }

    /// Observe one named Docker Volume directly.
    ///
    /// # Errors
    ///
    /// Returns an error when Docker inspection or decoding fails, or when Docker
    /// returns a different Volume name than requested.
    pub async fn inspect_volume(
        &self,
        machine_id: &MachineId,
        name: &DockerVolumeName,
    ) -> Result<DockerVolume, Error> {
        let volume = decode_volume(self.docker.client.inspect_volume(name.as_str()).await)?;
        if volume.name != name.as_str() {
            return Err(Error::UnexpectedVolumeName {
                requested: name.clone(),
                actual: volume.name,
            });
        }
        docker_volume(machine_id, volume)
    }

    pub async fn remove_volume(&self, name: &DockerVolumeName, force: bool) -> Result<(), Error> {
        self.docker
            .client
            .remove_volume(
                name.as_str(),
                Some(RemoveVolumeOptionsBuilder::default().force(force).build()),
            )
            .await
            .map_err(Into::into)
    }
}

// Bollard models Volume Status as key-only data, so valid plugin values use raw JSON recovery.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolume {
    name: String,
    driver: String,
    #[serde(default)]
    mountpoint: String,
    status: Option<Value>,
    options: Option<HashMap<String, String>>,
    labels: Option<HashMap<String, String>>,
}

impl From<Volume> for RawVolume {
    fn from(volume: Volume) -> Self {
        Self {
            name: volume.name,
            driver: volume.driver,
            mountpoint: volume.mountpoint,
            status: None,
            options: Some(volume.options),
            labels: Some(volume.labels),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawVolumeList {
    volumes: Option<Vec<RawVolume>>,
}

fn decode_volume(result: Result<Volume, bollard::errors::Error>) -> Result<RawVolume, Error> {
    match result {
        Ok(volume) => Ok(volume.into()),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str(&contents)?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Confirm that a Docker Volume inspection returned a decodable Volume.
///
/// # Errors
///
/// Returns an error when Docker rejects the inspection or its response cannot be decoded.
pub(super) fn ensure_volume_exists(
    result: Result<Volume, bollard::errors::Error>,
) -> Result<(), Error> {
    decode_volume(result).map(drop)
}

fn decode_volume_list(
    result: Result<bollard::models::VolumeListResponse, bollard::errors::Error>,
) -> Result<Vec<RawVolume>, Error> {
    match result {
        Ok(response) => Ok(response
            .volumes
            .unwrap_or_default()
            .into_iter()
            .map(Into::into)
            .collect()),
        Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
            Ok(serde_json::from_str::<RawVolumeList>(&contents)?
                .volumes
                .unwrap_or_default())
        }
        Err(error) => Err(error.into()),
    }
}

fn docker_volume(machine_id: &MachineId, volume: RawVolume) -> Result<DockerVolume, Error> {
    let storage = if volume.driver == "ployz" {
        let status = volume
            .status
            .ok_or_else(|| Error::InvalidVolumeStatus("Provisioned Volume status is missing"))?;
        let status = serde_json::from_value::<VolumePluginStatus>(status)
            .map_err(|_| Error::InvalidVolumeStatus("Provisioned Volume status is malformed"))?;
        let bound_bytes = std::num::NonZeroU64::new(status.bound_bytes).ok_or_else(|| {
            Error::InvalidVolumeStatus("Provisioned Volume status has no positive bound_bytes")
        })?;
        let mountpoint = ployz_core::MachinePath::parse(volume.mountpoint).map_err(|_| {
            Error::InvalidVolumeStatus("Provisioned Volume status has an invalid mountpoint")
        })?;
        DockerVolumeStorageObservation::Provisioned {
            mountpoint,
            bound_bytes,
            used_bytes: status.used_bytes,
        }
    } else {
        DockerVolumeStorageObservation::Plain {
            driver: volume.driver,
        }
    };
    Ok(DockerVolume {
        id: docker_volume_id(machine_id, &volume.name)?,
        options: volume.options.unwrap_or_default().into_iter().collect(),
        labels: volume.labels.unwrap_or_default().into_iter().collect(),
        storage,
    })
}

fn docker_volume_id(machine_id: &MachineId, name: &str) -> Result<DockerVolumeId, Error> {
    Ok(DockerVolumeId {
        machine_id: *machine_id,
        name: DockerVolumeName::parse(name).map_err(|source| Error::InvalidValue {
            field: "Docker Volume name",
            source,
        })?,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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
    use ployz_core::{MachineGateway, ProjectName, ResolvedServiceSpec};

    use super::*;
    use crate::docker::{LocalDocker, MachineSpecStore};

    #[derive(Clone, Default)]
    struct FakeDocker {
        requests: Arc<Mutex<Vec<(Method, String)>>>,
        reject_list: Arc<AtomicBool>,
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
        let response = if method == Method::GET
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
        } else if method == Method::GET && path.ends_with("/volumes/healthy") {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"healthy",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/healthy",
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else if method == Method::GET && path.ends_with("/volumes/cache") {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"cache",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/cache",
                    "Status":{"bound_bytes":536870912,"used_bytes":2048}
                }),
            )
        } else if method == Method::GET && path.ends_with("/volumes/malformed") {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"malformed",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/malformed",
                    "Status":{"bound_bytes":"not-a-number","used_bytes":4096}
                }),
            )
        } else if method == Method::GET && path.ends_with("/volumes/unavailable") {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"detail unavailable"}),
            )
        } else if method == Method::GET && path.ends_with("/volumes/mismatched") {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"other",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/other",
                    "Status":{"bound_bytes":1073741824,"used_bytes":4096}
                }),
            )
        } else if method == Method::POST
            && path.ends_with("/volumes/create")
            && String::from_utf8_lossy(&body).contains("rejected")
        {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({"message":"create rejected"}),
            )
        } else if method == Method::POST && path.ends_with("/volumes/create") {
            (
                StatusCode::CREATED,
                serde_json::json!({
                    "Name":"unavailable",
                    "Driver":"ployz",
                    "Mountpoint":"/var/lib/ployz-volumes/unavailable"
                }),
            )
        } else if method == Method::GET && path.ends_with("/networks/ployz") {
            (
                StatusCode::OK,
                serde_json::json!({
                    "Name":"ployz",
                    "IPAM":{"Config":[{"Subnet":"10.210.0.0/24","Gateway":"10.210.0.1"}]}
                }),
            )
        } else if method == Method::GET && path.ends_with("/containers/json") {
            (StatusCode::OK, serde_json::json!([]))
        } else if method == Method::POST && path.ends_with("/containers/create") {
            (
                StatusCode::CREATED,
                serde_json::json!({"Id":"1111111111111111111111111111111111111111111111111111111111111111","Warnings":[]}),
            )
        } else {
            (
                StatusCode::NOT_FOUND,
                serde_json::json!({"message":format!("unhandled {method} {path}")}),
            )
        };
        (response.0, Json(response.1)).into_response()
    }

    async fn fake_runtime() -> (ContainerRuntime, FakeDocker) {
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

    fn mounted_spec(names: &[&str]) -> ResolvedServiceSpec {
        let volumes = names
            .iter()
            .map(|name| {
                serde_json::json!({
                    "reference": name,
                    "source": {"kind":"named", "name":name, "external":true}
                })
            })
            .collect::<Vec<_>>();
        let mounts = names
            .iter()
            .map(|name| serde_json::json!({"volume":name, "target":format!("/{name}")}))
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({
            "service_id":"11111111111111111111111111111111",
            "name":"web",
            "mode":{"mode":"replicated", "replicas":1},
            "container":{"image":"unused", "pull_policy":"never"},
            "volumes":volumes,
            "mounts":mounts
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn container_creation_accepts_integer_status_for_multiple_named_volumes() {
        let (runtime, fake) = fake_runtime().await;

        runtime
            .create(
                &MachineId::random(),
                MachineGateway(std::net::Ipv4Addr::new(10, 210, 0, 1)),
                ployz_core::ContainerKind::ServiceContainer,
                &ProjectName::parse("app").unwrap(),
                &mounted_spec(&["healthy", "cache"]),
            )
            .await
            .unwrap();

        let requests = fake.requests.lock().unwrap();
        for suffix in ["/volumes/healthy", "/volumes/cache", "/containers/create"] {
            assert!(requests.iter().any(|(_, path)| path.ends_with(suffix)));
        }
    }

    #[tokio::test]
    async fn container_creation_rejects_a_missing_named_volume() {
        let (runtime, fake) = fake_runtime().await;

        let error = runtime
            .create(
                &MachineId::random(),
                MachineGateway(std::net::Ipv4Addr::new(10, 210, 0, 1)),
                ployz_core::ContainerKind::ServiceContainer,
                &ProjectName::parse("app").unwrap(),
                &mounted_spec(&["missing"]),
            )
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            Error::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            })
        ));
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| path.ends_with("/containers/create"))
        );
    }

    #[tokio::test]
    async fn inventory_reads_provisioned_details_and_keeps_healthy_siblings() {
        let (runtime, fake) = fake_runtime().await;
        let machine_id = MachineId::random();

        let inventory = runtime.list_volumes(&machine_id).await.unwrap();

        assert_eq!(
            inventory
                .volumes
                .iter()
                .map(|volume| volume.id.name.as_str())
                .collect::<Vec<_>>(),
            ["plain", "healthy"]
        );
        assert_eq!(
            inventory
                .failures
                .iter()
                .map(|failure| failure.id.name.as_str())
                .collect::<Vec<_>>(),
            ["malformed", "unavailable", "mismatched"]
        );
        let requests = fake.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(method, path)| method == Method::GET && path.ends_with("/volumes/healthy"))
        );
        assert!(
            !requests
                .iter()
                .any(|(method, path)| method == Method::GET && path.ends_with("/volumes/plain"))
        );
    }

    #[tokio::test]
    async fn direct_lookup_does_not_enumerate_unrelated_volumes() {
        let (runtime, fake) = fake_runtime().await;
        let machine_id = MachineId::random();

        let volume = runtime
            .inspect_volume(&machine_id, &DockerVolumeName::parse("healthy").unwrap())
            .await
            .unwrap();

        assert_eq!(volume.id.name.as_str(), "healthy");
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests.first().is_some_and(
            |(method, path)| method == Method::GET && path.ends_with("/volumes/healthy")
        ));
    }

    #[tokio::test]
    async fn inspect_rejects_a_mismatched_volume_identity() {
        let (runtime, _) = fake_runtime().await;
        let machine_id = MachineId::random();

        let error = runtime
            .inspect_volume(&machine_id, &DockerVolumeName::parse("mismatched").unwrap())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            Error::UnexpectedVolumeName { requested, actual }
                if requested.as_str() == "mismatched" && actual == "other"
        ));
    }

    #[tokio::test]
    async fn list_reports_a_mismatched_detail_under_the_requested_name() {
        let (runtime, _) = fake_runtime().await;
        let machine_id = MachineId::random();

        let inventory = runtime.list_volumes(&machine_id).await.unwrap();

        let failure = inventory
            .failures
            .iter()
            .find(|failure| failure.id.name.as_str() == "mismatched")
            .expect("mismatched detail is retained as a named failure");
        assert_eq!(failure.id.machine_id, machine_id);
        assert!(failure.error.message.contains("returned Volume 'other'"));
    }

    #[tokio::test]
    async fn list_returns_a_top_level_collection_error() {
        let (runtime, fake) = fake_runtime().await;
        fake.reject_list.store(true, Ordering::Relaxed);

        let error = runtime
            .list_volumes(&MachineId::random())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("collection unavailable"));
    }

    #[tokio::test]
    async fn create_reports_mutation_success_separately_from_failed_verification() {
        let (runtime, fake) = fake_runtime().await;
        let machine_id = MachineId::random();

        let report = runtime
            .create_volume(
                &machine_id,
                CreateVolumeRequest {
                    name: DockerVolumeName::parse("unavailable").unwrap(),
                    driver: "ployz".into(),
                    options: Default::default(),
                    labels: Default::default(),
                },
            )
            .await
            .unwrap();

        let CreateVolumeReport::Unverified { id, error } = report else {
            panic!("expected created-but-unverified report")
        };
        assert_eq!(id.machine_id, machine_id);
        assert_eq!(id.name.as_str(), "unavailable");
        assert!(error.message.contains("detail unavailable"), "{error}");
        let requests = fake.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(method, path)| method == Method::POST && path.ends_with("/volumes/create"))
        );
        assert!(
            requests
                .iter()
                .any(|(method, path)| method == Method::GET
                    && path.ends_with("/volumes/unavailable"))
        );
    }

    #[tokio::test]
    async fn create_returns_docker_rejection_as_an_error() {
        let (runtime, _) = fake_runtime().await;

        let error = runtime
            .create_volume(
                &MachineId::random(),
                CreateVolumeRequest {
                    name: DockerVolumeName::parse("rejected").unwrap(),
                    driver: "ployz".into(),
                    options: Default::default(),
                    labels: Default::default(),
                },
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("create rejected"));
    }

    #[test]
    fn docker_volume_preserves_provisioned_usage_at_alert_threshold() {
        let contents = serde_json::json!({"Volumes":[{
            "Name":"data",
            "Driver":"ployz",
            "Mountpoint":"/var/lib/ployz-volumes/data",
            "Status":{"bound_bytes":1073741824,"used_bytes":966367642},
            "Options":{"size":"1g"}
        }]})
        .to_string();
        let mut volumes = decode_volume_list(Err(bollard::errors::Error::JsonDataError {
            message: "generated Volume status cannot represent numeric values".into(),
            contents,
            column: 0,
        }))
        .unwrap();
        let observed = docker_volume(&MachineId::random(), volumes.remove(0)).unwrap();

        assert_eq!(
            observed.storage,
            DockerVolumeStorageObservation::Provisioned {
                mountpoint: ployz_core::MachinePath::parse("/var/lib/ployz-volumes/data").unwrap(),
                bound_bytes: std::num::NonZeroU64::new(1_073_741_824).unwrap(),
                used_bytes: 966_367_642,
            }
        );
    }

    #[test]
    fn provisioned_volume_without_complete_plugin_evidence_is_an_error() {
        let error = docker_volume(
            &MachineId::random(),
            serde_json::from_value(serde_json::json!({
                "Name":"data",
                "Driver":"ployz",
                "Mountpoint":"/var/lib/ployz-volumes/data"
            }))
            .unwrap(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("Provisioned Volume status"));
    }

    #[test]
    fn provisioned_volume_rejects_a_relative_mountpoint() {
        let error = docker_volume(
            &MachineId::random(),
            serde_json::from_value(serde_json::json!({
                "Name":"data",
                "Driver":"ployz",
                "Mountpoint":"relative/data",
                "Status":{"bound_bytes":1073741824,"used_bytes":0}
            }))
            .unwrap(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("invalid mountpoint"));
    }
}
