//! Fresh capacity and bounded batch preparation on the plugin's existing mutation lock.

use axum::{Json, extract::State};
use ployz_core::{ProvisionedVolumeMaximumBytes, StorageCapacity, StorageCapacityError};
use std::collections::BTreeMap;

use super::{DockerVolumeName, VolumeStorage};

type Volumes = BTreeMap<ployz_core::DockerVolumeName, ProvisionedVolumeMaximumBytes>;

fn unknown(error: impl std::fmt::Display) -> ployz_core::RpcError {
    StorageCapacityError::StorageCapacityUnknown {
        message: error.to_string(),
    }
    .into_rpc_error()
}

fn storage_error(error: super::VolumeError) -> ployz_core::RpcError {
    match error {
        super::VolumeError::Capacity(error) => error.into_rpc_error(),
        error => ployz_core::RpcError {
            code: ployz_core::RpcErrorCode::Internal,
            message: format!("Storage preparation failed: {error}"),
            details: serde_json::json!({"code": "storage_preparation_failed"}),
        },
    }
}

impl VolumeStorage {
    async fn capacity(&self) -> super::Result<StorageCapacity> {
        let pool = match self.pool.one_usable().await? {
            Some(pool) => Some(pool),
            None => {
                self.pool
                    .recover(super::pool::UnlabeledBacking::Preserve)
                    .await?
            }
        };
        let mut volumes = BTreeMap::new();
        let mut managed_used_bytes = 0u64;
        if let Some(pool) = &pool {
            let prefix = format!("{}/ployz/", pool.name());
            for dataset in self.datasets(pool).await? {
                let Some(name) = dataset.name.strip_prefix(&prefix) else {
                    continue;
                };
                let name =
                    ployz_core::DockerVolumeName::parse(name).map_err(|error| error.to_string())?;
                let maximum = ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(dataset.refquota)
                        .ok_or("Volume has no finite bound")?,
                );
                managed_used_bytes = managed_used_bytes
                    .checked_add(dataset.active_used_bytes)
                    .ok_or("Dataset occupancy overflows u64")?;
                volumes.insert(name, maximum);
            }
        }
        Ok(StorageCapacity {
            backing: self.pool.capacity_backing(pool.as_ref()).await?,
            unmanaged_used_bytes: pool.as_ref().map_or(0, |pool| {
                pool.used_bytes().saturating_sub(managed_used_bytes)
            }),
            volumes,
        })
    }

    async fn inspect_capacity(&self) -> Result<StorageCapacity, ployz_core::RpcError> {
        let admission = self.admit_mutation().await.map_err(unknown)?;
        let storage = self.clone();
        tokio::spawn(async move {
            let _admission = admission;
            let _pool_guard = storage.pool.lock_mutation().await.map_err(unknown)?;
            storage.capacity().await.map_err(unknown)
        })
        .await
        .unwrap_or_else(|error| Err(unknown(error)))
    }

    async fn prepare(
        &self,
        requested: &Volumes,
    ) -> Result<Vec<ployz_core::DockerVolumeName>, ployz_core::RpcError> {
        let admission = self.admit_mutation().await.map_err(storage_error)?;
        let storage = self.clone();
        let requested = requested.clone();
        tokio::spawn(async move {
            let _admission = admission;
            storage.prepare_admitted(&requested).await
        })
        .await
        .unwrap_or_else(|error| Err(unknown(error)))
    }

    async fn prepare_admitted(
        &self,
        requested: &Volumes,
    ) -> Result<Vec<ployz_core::DockerVolumeName>, ployz_core::RpcError> {
        let _pool_guard = self.pool.lock_mutation().await.map_err(storage_error)?;
        let capacity = self.capacity().await.map_err(unknown)?;
        let budget = capacity
            .budget(requested)
            .map_err(StorageCapacityError::into_rpc_error)?;
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let existing_pool = self.pool.one_usable().await.map_err(storage_error)?;
        if let Some(pool) = &existing_pool {
            let datasets = self.datasets(pool).await.map_err(storage_error)?;
            for (name, maximum) in requested {
                let plugin_name = name
                    .as_str()
                    .parse::<DockerVolumeName>()
                    .map_err(storage_error)?;
                if let Some(existing) =
                    Self::dataset(&datasets, pool, &plugin_name).map_err(storage_error)?
                {
                    existing
                        .require_requested(&plugin_name, maximum.get())
                        .map_err(storage_error)?;
                }
            }
        }
        let commitment = capacity
            .volumes
            .values()
            .map(|maximum| maximum.get())
            .try_fold(budget.additional_commitment_bytes, u64::checked_add)
            .ok_or_else(|| unknown("Volume commitments overflow u64"))?;
        match existing_pool {
            Some(pool) => {
                self.pool
                    .ensure_capacity(&pool, commitment, capacity.unmanaged_used_bytes)
                    .await
                    .map_err(storage_error)?;
            }
            None => {
                self.pool.create(commitment).await.map_err(storage_error)?;
            }
        }
        Ok(requested.keys().cloned().collect())
    }
}

pub(super) async fn inspect(
    State(storage): State<VolumeStorage>,
) -> Json<Result<StorageCapacity, ployz_core::RpcError>> {
    Json(storage.inspect_capacity().await)
}

pub(super) async fn prepare(
    State(storage): State<VolumeStorage>,
    Json(requested): Json<Volumes>,
) -> Json<Result<Vec<ployz_core::DockerVolumeName>, ployz_core::RpcError>> {
    Json(storage.prepare(&requested).await)
}
