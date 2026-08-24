//! Docker Volume plugin removal and lookup endpoints.

use axum::{Json, extract::State};
use ployzd::VolumePluginStatus;
use serde::Serialize;

use super::{
    DATASET_ROOT, DockerVolumeName, ErrorResponse, Result, VolumeRequest, VolumeStorage,
    error_response,
};

impl VolumeStorage {
    async fn remove(&self, name: &DockerVolumeName) -> Result<()> {
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(());
        };
        let datasets = self.datasets(&pool).await?;
        let requested = format!("{}/{DATASET_ROOT}/{name}", pool.name());
        if !datasets.iter().any(|dataset| dataset.name == requested) {
            return Ok(());
        }
        let dataset = Self::dataset(&datasets, &pool, name)?
            .expect("the requested dataset was just observed");
        dataset.require_provisioned(name)?;
        self.zfs(&["destroy", &dataset.name]).await?;
        Ok(())
    }

    async fn inspect(&self, name: &DockerVolumeName) -> Result<PluginVolume> {
        let _guard = self.mutation.lock().await;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_provisioned(name)?;
        Ok(PluginVolume::new(name, dataset))
    }

    async fn list(&self) -> Result<Vec<PluginVolume>> {
        let _guard = self.mutation.lock().await;
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(Vec::new());
        };
        let datasets = self.datasets(&pool).await?;
        let root_name = format!("{}/{DATASET_ROOT}", pool.name());
        let root = datasets.iter().find(|dataset| dataset.name == root_name);
        if let Some(root) = root {
            root.require_mountpoint(super::MOUNT_ROOT)?;
            root.require_writable()?;
        }
        let prefix = format!("{}/{DATASET_ROOT}/", pool.name());
        let mut volumes = Vec::new();
        for dataset in &datasets {
            let Some(name) = dataset.name.strip_prefix(&prefix) else {
                continue;
            };
            if let Some((name, _)) = name.split_once('/') {
                return Err(format!(
                    "ZFS dataset {} is a descendant of Provisioned Volume {name}; remove it before retrying",
                    dataset.name
                )
                .into());
            }
            if root.is_none() {
                return Err(format!("ZFS did not report managed root dataset {root_name}").into());
            }
            let name = name.parse::<DockerVolumeName>()?;
            dataset.require_provisioned(&name)?;
            volumes.push(PluginVolume::new(&name, dataset));
        }
        Ok(volumes)
    }
}

pub(super) async fn remove(
    State(storage): State<VolumeStorage>,
    Json(request): Json<VolumeRequest>,
) -> Json<ErrorResponse> {
    let result = match request.name.parse::<DockerVolumeName>() {
        Ok(name) => storage.remove(&name).await,
        Err(error) => Err(error),
    };
    error_response(result)
}

pub(super) async fn get(
    State(storage): State<VolumeStorage>,
    Json(request): Json<VolumeRequest>,
) -> Json<GetResponse> {
    let result = match request.name.parse::<DockerVolumeName>() {
        Ok(name) => storage.inspect(&name).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(volume) => Json(GetResponse {
            volume: Some(volume),
            error: String::new(),
        }),
        Err(error) => Json(GetResponse {
            volume: None,
            error: error.to_string(),
        }),
    }
}

pub(super) async fn list(State(storage): State<VolumeStorage>) -> Json<ListResponse> {
    match storage.list().await {
        Ok(volumes) => Json(ListResponse {
            volumes,
            error: String::new(),
        }),
        Err(error) => Json(ListResponse {
            volumes: Vec::new(),
            error: error.to_string(),
        }),
    }
}

#[derive(Serialize)]
pub(super) struct GetResponse {
    #[serde(rename = "Volume", skip_serializing_if = "Option::is_none")]
    volume: Option<PluginVolume>,
    #[serde(rename = "Err")]
    error: String,
}

#[derive(Serialize)]
pub(super) struct ListResponse {
    #[serde(rename = "Volumes")]
    volumes: Vec<PluginVolume>,
    #[serde(rename = "Err")]
    error: String,
}

#[derive(Serialize)]
struct PluginVolume {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Mountpoint")]
    mountpoint: String,
    #[serde(rename = "Status")]
    status: VolumePluginStatus,
}

impl PluginVolume {
    fn new(name: &DockerVolumeName, dataset: &super::Dataset) -> Self {
        Self {
            name: name.to_string(),
            mountpoint: dataset.mountpoint.clone(),
            status: VolumePluginStatus {
                bound_bytes: dataset.refquota,
                used_bytes: dataset.used_bytes,
            },
        }
    }
}
