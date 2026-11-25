//! A preview's Close on its home Server. Every copy of the preview's Docker Volumes leaves
//! this Pool: destroyed when it carries exactly the origin the preview's own birth derives,
//! so no data from anywhere else is ever destroyed, and moved to the Trash otherwise. A
//! receive into one is stopped first, so nothing lands after the sweep.
//!
//! The sweep refuses, and leaves the Close to run again, whenever it can't be sure what the
//! Pool holds: a Pool this Machine held is not imported, ZFS stopped its listing early, or a
//! dataset sits below a copy. It destroys with no `-r`, so ZFS refuses one that appears
//! below a copy after the read.
//!
//! Each step reads the Pool afresh, so a Close stopped halfway is run again from the top.

use std::collections::BTreeSet;

use axum::{Json, extract::State};
use ployz_core::{
    ClosePreviewRequest, DockerVolumeName, PreviewClosed, PreviewNamespace, PreviewTrashed,
    RpcError, TrashPlace, VolumeOrigin,
};

use super::{Dataset, PluginError, Result, VolumeName, VolumeStorage, marks::Mark, storage::Holds};
use crate::dataset_layout::DatasetLayout;

impl VolumeStorage {
    async fn close_preview(&self, preview: &PreviewNamespace) -> Result<PreviewClosed> {
        let (_held, _installation) = self.admit_mutation().await?;
        let (_pool_guard, pool) = self.imported_pool().await?;
        let mut closed = PreviewClosed::default();
        let Some(pool) = pool else {
            return Ok(closed);
        };
        let layout = DatasetLayout::of(&pool);
        let prefix = format!("{}_", preview.namespace());
        let listed = self.complete_datasets(&pool).await?;
        let names = listed
            .iter()
            .filter_map(|dataset| layout.place(&dataset.name).volume())
            .filter_map(|name| name.split('/').next())
            .filter(|name| name.starts_with(&prefix))
            .collect::<BTreeSet<&str>>();
        for name in names {
            let docker = DockerVolumeName::parse(name)
                .map_err(|error| ployz_core::error_chain::inline(&error))?;
            let name = name.parse::<VolumeName>()?;
            if !self.receives.stop(&name, self.stop_deadline).await {
                return Err(PluginError::from(format!(
                    "Volume {name} is still receiving, so the preview's Close stops until it is run again"
                )));
            }
            // Read again once its receive stopped, which may have made the slot's copy since.
            let datasets = self.complete_datasets(&pool).await?;
            let root = Self::dataset(&datasets, &pool, &name)?;
            let slot = Self::slot(&datasets, &pool, &name);
            if slot.is_some() {
                only_its_copy_below(&datasets, &layout, &name)?;
            }
            let own = preview.own_origin(name.as_str());
            if let Some(root) = root {
                let origin = self.local_origin(&root.name, Mark::Origin).await;
                if made_here(origin, own.as_ref()) {
                    self.destroy_exactly(&root.name, None).await?;
                    closed.destroyed.push(docker.clone());
                } else {
                    let id = self.discard(&root.name, TrashPlace::Root, &name).await?;
                    closed.trashed.push(PreviewTrashed {
                        name: docker.clone(),
                        id,
                    });
                }
            }
            if let Some(parent) = slot {
                let parent = parent.name.as_str();
                let fs = Self::slot_fs(&datasets, &pool, &name).map(|fs| fs.name.as_str());
                let origin = self.origin_of(Some(parent), fs.map(|fs| (fs, None))).await;
                match fs {
                    None => self.destroy_exactly(parent, None).await?,
                    Some(fs) if made_here(origin, own.as_ref()) => {
                        self.destroy_exactly(parent, Some(fs)).await?;
                        if closed.destroyed.last() != Some(&docker) {
                            closed.destroyed.push(docker.clone());
                        }
                    }
                    Some(fs) if self.newest_guid(fs).await?.is_none() => {
                        self.destroy_exactly(parent, Some(fs)).await?;
                    }
                    Some(_) => {
                        let id = self.discard(parent, TrashPlace::Slot, &name).await?;
                        closed.trashed.push(PreviewTrashed { name: docker, id });
                    }
                }
            }
            self.receives.forget(&name);
        }
        Ok(closed)
    }

    /// Destroys `top`, and the copy `below` it first when it has one, with no `-r`: each
    /// one's snapshots, then the dataset. ZFS itself then refuses a dataset that gained a
    /// child or a snapshot since the Pool was read, so nothing that appears under a closing
    /// preview's copy is destroyed with it.
    async fn destroy_exactly(&self, top: &str, below: Option<&str>) -> Result<()> {
        self.bury(top, true).await?;
        let below = match below {
            Some(fs) if self.marks(fs).is_set(Mark::ResumeToken).await? => {
                self.zfs(&["receive", "-A", fs]).await?;
                // Aborting a first receive destroys the copy it began.
                self.exists(fs).await?.then_some(fs)
            }
            below => below,
        };
        for dataset in below.into_iter().chain([top]) {
            for snapshot in self.snapshot_names(dataset).await? {
                self.zfs(&["destroy", &snapshot]).await?;
            }
            self.destroy(dataset, Holds::Nothing).await?;
        }
        Ok(())
    }
}

/// Refuses while `name`'s slot holds any dataset besides its copy, which a recursive
/// destroy of the slot would take with it, whatever that dataset holds.
fn only_its_copy_below(
    datasets: &[Dataset],
    layout: &DatasetLayout,
    name: &VolumeName,
) -> Result<()> {
    let below = format!("{}/", layout.slot_parent(name));
    let fs = layout.slot_fs(name);
    match datasets
        .iter()
        .find(|dataset| dataset.name.starts_with(&below) && dataset.name != fs)
    {
        Some(dataset) => Err(format!(
            "ZFS dataset {} is not part of Volume {name}'s slot; remove it before retrying",
            dataset.name
        )
        .into()),
        None => Ok(()),
    }
}

/// Whether a copy's origin is exactly the preview's own. An origin unset, unreadable, or
/// set by anything but this Machine is not.
fn made_here(origin: Result<Option<VolumeOrigin>>, own: Option<&VolumeOrigin>) -> bool {
    matches!((origin, own), (Ok(Some(origin)), Some(own)) if origin == *own)
}

pub(super) async fn close(
    State(storage): State<VolumeStorage>,
    Json(request): Json<ClosePreviewRequest>,
) -> Json<std::result::Result<PreviewClosed, RpcError>> {
    let Some(preview) = PreviewNamespace::of(&request.namespace) else {
        return Json(Err(RpcError::invalid_argument(format!(
            "{} is not a preview Namespace",
            request.namespace
        ))));
    };
    Json(
        storage
            .close_preview(&preview)
            .await
            .map_err(RpcError::from),
    )
}
