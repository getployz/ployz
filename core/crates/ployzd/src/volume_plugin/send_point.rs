//! `Volume.SendPoint`: which dataset a send reads, and the GUID and origin of the snapshot
//! it sends, in one answer under the plugin's mutation. No receive starts under it
//! ([`super::transfer::Receives::spawn`]), so the copy cannot be replaced between choosing
//! the dataset and reading the snapshot. The send checks the stream's own BEGIN record
//! against the GUID, which covers a replacement after this answer.

use crate::volume_copy::{SendPoint, SendPointRequest};
use axum::{Json, extract::State};
use ployz_core::{RpcError, RpcErrorCode, SwitchError};

use super::{VolumeName, VolumeStorage, marks::Mark};

pub(super) async fn send_point(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SendPointRequest>,
) -> Json<Result<SendPoint, RpcError>> {
    Json(storage.send_point(&request).await)
}

fn unknown(message: String) -> RpcError {
    RpcError::new(RpcErrorCode::NotFound, message)
}

impl VolumeStorage {
    async fn send_point(&self, request: &SendPointRequest) -> Result<SendPoint, RpcError> {
        let name = VolumeName::try_from(&request.name)?;
        let (_held, _installation) = self.admit_mutation().await?;
        let pool = self
            .pool
            .one_usable()
            .await?
            .ok_or_else(|| unknown("no Machine Pool is imported on this Machine".to_owned()))?;
        // Every 404 below is an absence, judged only from listings ZFS finished.
        let datasets = self.complete_datasets(&pool).await?;
        let pulled = || {
            SwitchError::Busy.rpc_error(format!(
                "Volume {name} is still being pulled on this Server"
            ))
        };
        if self.receives.status(&name).busy() {
            return Err(pulled());
        }
        // A slot parent's marks hold whatever sits beside it: a root parked or landed next
        // to a stale parent sends only once that parent is clean.
        if let Some(parent) = Self::slot(&datasets, &pool, &name) {
            let marks = self.marks(&parent.name);
            if marks.is_set(Mark::Pull).await? || marks.is_set(Mark::PullOrigin).await? {
                return Err(pulled());
            }
        }
        let dataset = match Self::dataset(&datasets, &pool, &name)? {
            Some(root) => root.name.clone(),
            None => Self::slot_fs(&datasets, &pool, &name)
                .ok_or_else(|| unknown(format!("Volume {name} has no writer on this Machine")))?
                .name
                .clone(),
        };
        let at = format!("{dataset}@{}", request.target);
        if !self.snapshot_names(&dataset).await?.contains(&at) {
            return Err(unknown(format!("snapshot {at} does not exist")));
        }
        let point = self.point(&dataset, request.target.as_str()).await?;
        Ok(SendPoint {
            dataset,
            guid: point.guid,
            origin: point.origin,
        })
    }
}
