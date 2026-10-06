//! Fresh capacity and bounded batch preparation on the plugin's existing mutation lock.

use axum::{Json, extract::State};
use ployz_core::{
    CopyRole, ProvisionedCopy, ProvisionedVolumeMaximumBytes, StorageCapacity, StorageCapacityError,
};
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

struct ManagedStorage {
    capacity: StorageCapacity,
    slot_bound_bytes: u64,
}

impl VolumeStorage {
    async fn capacity(&self) -> super::Result<ManagedStorage> {
        let pool = match self.pool.one_usable().await? {
            Some(pool) => Some(pool),
            None => {
                self.pool
                    .recover(super::pool::UnlabeledBacking::Preserve)
                    .await?
            }
        };
        let mut volumes = BTreeMap::new();
        let mut copies: BTreeMap<ployz_core::DockerVolumeName, ProvisionedCopy> = BTreeMap::new();
        let mut managed_used_bytes = 0u64;
        let mut slot_bound_bytes = 0u64;
        if let Some(pool) = &pool {
            let datasets = self.datasets(pool).await?;
            for dataset in &datasets {
                let place = super::Place::of(&dataset.name, pool.name());
                if !place.commits() {
                    continue;
                }
                managed_used_bytes = managed_used_bytes
                    .checked_add(dataset.active_used_bytes)
                    .ok_or("Dataset occupancy overflows u64")?;
                let (super::Place::Root(name) | super::Place::Slot(name)) = place else {
                    continue;
                };
                let name =
                    ployz_core::DockerVolumeName::parse(name).map_err(|error| error.to_string())?;
                let (role, maximum_bytes) = match place {
                    super::Place::Root(_) => {
                        let plugin_name = name
                            .as_str()
                            .parse::<DockerVolumeName>()
                            .map_err(|error| error.to_string())?;
                        (
                            self.root_role(&datasets, pool, dataset, &plugin_name)
                                .await?,
                            dataset.refquota,
                        )
                    }
                    super::Place::Slot(_) => {
                        let bound = super::storage::committed_bytes(dataset, pool.name());
                        if bound == 0 {
                            if let Some(copy) = copies.get_mut(&name) {
                                copy.used_bytes =
                                    copy.used_bytes.saturating_add(dataset.active_used_bytes);
                            }
                            continue;
                        }
                        slot_bound_bytes = slot_bound_bytes
                            .checked_add(bound)
                            .ok_or("Volume commitments overflow u64")?;
                        (CopyRole::Slot, bound)
                    }
                    super::Place::Outside => continue,
                };
                let maximum = ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(maximum_bytes).ok_or("Volume has no finite bound")?,
                );
                if role != CopyRole::Slot {
                    volumes.insert(name.clone(), maximum);
                }
                copies.insert(
                    name,
                    ProvisionedCopy {
                        role,
                        maximum_bytes: maximum,
                        used_bytes: dataset.active_used_bytes,
                    },
                );
            }
        }
        Ok(ManagedStorage {
            capacity: StorageCapacity {
                backing: self.pool.capacity_backing(pool.as_ref()).await?,
                unmanaged_used_bytes: pool.as_ref().map_or(0, |pool| {
                    pool.used_bytes().saturating_sub(managed_used_bytes)
                }),
                volumes,
                copies,
            },
            slot_bound_bytes,
        })
    }

    /// Writer while idle, closed and writable; Switching while a run holds the root.
    async fn root_role(
        &self,
        datasets: &[super::Dataset],
        pool: &ployzd::machine_pool::MachinePool,
        root: &super::Dataset,
        name: &DockerVolumeName,
    ) -> super::Result<CopyRole> {
        let open = self
            .lease_record(datasets, pool, name)
            .await?
            .is_some_and(|record| record.cycle == ployz_core::Cycle::Open);
        let idle = self.writer_marker(root).await? == ployz_core::WriterMarker::Idle;
        Ok(if idle && !root.readonly && !open {
            CopyRole::Writer
        } else {
            CopyRole::Switching
        })
    }

    async fn inspect_capacity(&self) -> Result<StorageCapacity, ployz_core::RpcError> {
        let admission = self.admit_mutation().await.map_err(unknown)?;
        let storage = self.clone();
        tokio::spawn(async move {
            let _admission = admission;
            let _pool_guard = storage.pool.lock_mutation().await.map_err(unknown)?;
            storage
                .capacity()
                .await
                .map(|managed| managed.capacity)
                .map_err(unknown)
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
        let managed = self.capacity().await.map_err(unknown)?;
        let budget = managed
            .capacity
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
        let commitment = managed
            .capacity
            .volumes
            .values()
            .map(|maximum| maximum.get())
            .try_fold(budget.additional_commitment_bytes, u64::checked_add)
            .and_then(|roots| roots.checked_add(managed.slot_bound_bytes))
            .ok_or_else(|| unknown("Volume commitments overflow u64"))?;
        match existing_pool {
            Some(pool) => {
                self.pool
                    .ensure_capacity(&pool, commitment, managed.capacity.unmanaged_used_bytes)
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
