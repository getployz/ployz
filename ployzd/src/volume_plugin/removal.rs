//! Docker Volume plugin removal and lookup endpoints.

use axum::{Json, extract::State};
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

    async fn inspect(&self, name: &DockerVolumeName) -> Result<String> {
        let _guard = self.mutation.lock().await;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_provisioned(name)?;
        Ok(dataset.mountpoint.clone())
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
        Ok(name) => storage.inspect(&name).await.map(|mountpoint| PluginVolume {
            name: name.to_string(),
            mountpoint,
        }),
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

#[derive(Serialize)]
pub(super) struct GetResponse {
    #[serde(rename = "Volume", skip_serializing_if = "Option::is_none")]
    volume: Option<PluginVolume>,
    #[serde(rename = "Err")]
    error: String,
}

#[derive(Serialize)]
struct PluginVolume {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Mountpoint")]
    mountpoint: String,
}
