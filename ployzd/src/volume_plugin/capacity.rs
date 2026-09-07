//! Fresh capacity and bounded batch preparation on the plugin's existing mutation lock.

use axum::{Json, extract::State};
use ployz_core::{ProvisionedVolumeMaximumBytes, StorageCapacity, StorageCapacityError};
use std::collections::BTreeMap;

use super::{CapacityAdmission, DockerVolumeName, VolumeStorage};

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

    async fn prepare(
        &self,
        requested: &Volumes,
    ) -> Result<Vec<ployz_core::DockerVolumeName>, ployz_core::RpcError> {
        let _guard = self.mutation.lock().await;
        let _pool_guard = self.pool.lock_mutation().await.map_err(storage_error)?;
        let capacity = self.capacity().await.map_err(unknown)?;
        let budget = capacity
            .budget(requested)
            .map_err(StorageCapacityError::into_rpc_error)?;
        if requested.is_empty() {
            return Ok(Vec::new());
        }
        let commitment = capacity
            .volumes
            .values()
            .map(|maximum| maximum.get())
            .try_fold(budget.additional_commitment_bytes, u64::checked_add)
            .ok_or_else(|| unknown("Volume commitments overflow u64"))?;
        let pool = match self.pool.one_usable().await.map_err(storage_error)? {
            Some(pool) => {
                self.pool
                    .ensure_capacity(&pool, commitment, capacity.unmanaged_used_bytes)
                    .await
                    .map_err(storage_error)?;
                pool
            }
            None => {
                self.pool.create(commitment).await.map_err(storage_error)?;
                self.one_pool().await.map_err(storage_error)?
            }
        };
        let mut prepared = Vec::new();
        for (name, maximum) in requested {
            let plugin_name = name
                .as_str()
                .parse::<DockerVolumeName>()
                .map_err(storage_error)?;
            // The whole batch has physical backing; each dataset records its durable commitment.
            if let Err(error) = self
                .create_volume(
                    &pool,
                    &plugin_name,
                    maximum.get(),
                    CapacityAdmission::Ensured,
                )
                .await
            {
                let mut error = storage_error(error);
                error
                    .details
                    .as_object_mut()
                    .expect("storage errors have object details")
                    .insert("prepared_volumes".into(), serde_json::json!(prepared));
                return Err(error);
            }
            prepared.push(name.clone());
        }
        Ok(prepared)
    }
}

pub(super) async fn inspect(
    State(storage): State<VolumeStorage>,
) -> Json<Result<StorageCapacity, ployz_core::RpcError>> {
    // Finish import recovery under the locks even if the observer disconnects.
    Json(
        tokio::spawn(async move {
            let _guard = storage.mutation.lock().await;
            let _pool_guard = storage.pool.lock_mutation().await.map_err(unknown)?;
            storage.capacity().await.map_err(unknown)
        })
        .await
        .unwrap_or_else(|error| Err(unknown(error))),
    )
}

pub(super) async fn prepare(
    State(storage): State<VolumeStorage>,
    Json(requested): Json<Volumes>,
) -> Json<Result<Vec<ployz_core::DockerVolumeName>, ployz_core::RpcError>> {
    // Finish admitted allocation even if the requesting connection disappears.
    Json(
        tokio::spawn(async move { storage.prepare(&requested).await })
            .await
            .unwrap_or_else(|error| Err(unknown(error))),
    )
}
