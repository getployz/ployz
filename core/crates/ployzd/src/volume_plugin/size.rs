//! SetVolumeSize: a Provisioned Volume's bound raised in place. The bound is the
//! `refquota` of the dataset this Machine holds for the Volume, its root or its slot's
//! parent, so a grow commits the difference to the Pool, writes the one property and
//! reads it back, with the Container on the Volume still running. A slot's landed copy
//! carries the same bound, and a promote renames that copy into the writer root, so a
//! grow raises the copy too, with nothing more committed.

use axum::{Json, extract::State};
use ployz_core::{
    ProvisionedVolumeMaximumBytes, RpcError, SetVolumeSizeRefused, SetVolumeSizeReply,
    SetVolumeSizeRequest, SizeRefusal,
};

use super::{
    DockerVolumeName, VolumeError, VolumeStorage, inspect::internal, mirror, storage::Held,
};

impl VolumeStorage {
    async fn set_size(
        &self,
        request: &SetVolumeSizeRequest,
    ) -> Result<SetVolumeSizeReply, RpcError> {
        let name = mirror::name(&request.name)?;
        let wanted = request.maximum_bytes;
        let admission = self.admit_mutation().await.map_err(internal)?;
        let storage = self.clone();
        tokio::spawn(async move {
            let _admission = admission;
            storage.set_size_admitted(&name, wanted).await
        })
        .await
        .unwrap_or_else(|error| Err(internal(VolumeError::from(error.to_string()))))
    }

    async fn set_size_admitted(
        &self,
        name: &DockerVolumeName,
        wanted: ProvisionedVolumeMaximumBytes,
    ) -> Result<SetVolumeSizeReply, RpcError> {
        let _pool_guard = self.pool.lock_mutation().await.map_err(internal)?;
        let not_found = || {
            refuse(
                SizeRefusal::NotFound,
                None,
                format!("Volume {name} does not exist on this Machine"),
            )
        };
        let Some(pool) = self
            .pool
            .admit(super::pool::UnlabeledBacking::Preserve)
            .await
            .map_err(internal)?
        else {
            return Err(not_found());
        };
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let Some(held) = self.held(&datasets, &pool, name).map_err(internal)? else {
            return Err(not_found());
        };
        if let Held::Root(root) = &held {
            root.require_mountpoint(&name.mountpoint())
                .map_err(internal)?;
        }
        let Some(bound) = held.bound() else {
            return Err(refuse(
                SizeRefusal::NotProvisioned,
                None,
                format!("Volume {name} has no Provisioned Volume bound to raise"),
            ));
        };
        let bound = ProvisionedVolumeMaximumBytes::new(bound);
        let copy_behind = match &held {
            Held::Slot { fs: Some(fs), .. } if fs.refquota < wanted.get() => Some(fs.name.as_str()),
            Held::Root(_) | Held::Slot { .. } => None,
        };
        if wanted < bound {
            return Err(refuse(
                SizeRefusal::Shrink,
                Some(bound),
                format!(
                    "Volume {name} holds a {bound} bound; a Volume does not shrink to {wanted}"
                ),
            ));
        }
        if wanted == bound && copy_behind.is_none() {
            return Ok(SetVolumeSizeReply { bound_bytes: bound });
        }
        if held.switching() {
            if wanted == bound {
                return Ok(SetVolumeSizeReply { bound_bytes: bound });
            }
            let message = match &held {
                Held::Root(root) => format!(
                    "Volume {name} is mid-run on this Machine (ZFS dataset {} is read-only); wait for the run to finish",
                    root.name
                ),
                Held::Slot { .. } => format!(
                    "Volume {name} is receiving a copy on this Machine; wait for the receive to land"
                ),
            };
            return Err(refuse(SizeRefusal::Switching, Some(bound), message));
        }
        if wanted > bound {
            match self
                .grow_bound(
                    &pool,
                    &datasets,
                    &held.dataset().name,
                    bound.get(),
                    wanted.get(),
                )
                .await
            {
                Ok(()) => {}
                Err(VolumeError::Capacity(capacity)) => {
                    return Err(SetVolumeSizeRefused {
                        reason: SizeRefusal::NoCapacity,
                        bound_bytes: Some(bound),
                        capacity: Some(capacity.clone()),
                    }
                    .rpc_error(format!(
                        "this Machine has no room to grow Volume {name} to {wanted}: {capacity}"
                    )));
                }
                Err(error) => return Err(internal(error)),
            }
        }
        if let Some(fs) = copy_behind {
            self.set_verified(fs, "refquota", &wanted.get().to_string())
                .await
                .map_err(internal)?;
        }
        Ok(SetVolumeSizeReply {
            bound_bytes: wanted,
        })
    }
}

fn refuse(
    reason: SizeRefusal,
    bound_bytes: Option<ProvisionedVolumeMaximumBytes>,
    message: String,
) -> RpcError {
    SetVolumeSizeRefused {
        reason,
        bound_bytes,
        capacity: None,
    }
    .rpc_error(message)
}

pub(super) async fn set_volume_size(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SetVolumeSizeRequest>,
) -> Json<Result<SetVolumeSizeReply, RpcError>> {
    Json(storage.set_size(&request).await)
}
