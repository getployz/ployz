//! A preview's Close on its home Server. Every copy of the preview's Docker Volumes leaves
//! this Pool: destroyed when it carries exactly the origin the preview's own birth derives,
//! so no data from anywhere else is ever destroyed, and moved to the Trash otherwise. A
//! receive into one is stopped first, so nothing lands after the sweep.
//!
//! Each step reads the Pool afresh, so a Close stopped halfway is run again from the top.

use std::collections::BTreeSet;

use axum::{Json, extract::State};
use ployz_core::{
    ClosePreviewRequest, DockerVolumeName, PreviewClosed, PreviewNamespace, PreviewTrashed,
    RpcError, TrashPlace, VolumeOrigin,
};

use super::{PluginError, Result, VolumeName, VolumeStorage, marks::Mark, storage::Holds};
use crate::dataset_layout::DatasetLayout;

impl VolumeStorage {
    async fn close_preview(&self, preview: &PreviewNamespace) -> Result<PreviewClosed> {
        let (_held, _installation) = self.admit_mutation().await?;
        let mut closed = PreviewClosed::default();
        let Some(pool) = self.pool.one_usable().await? else {
            return Ok(closed);
        };
        let layout = DatasetLayout::of(&pool);
        let prefix = format!("{}_", preview.namespace());
        let datasets = self.datasets(&pool).await?;
        let names = datasets
            .iter()
            .filter_map(|dataset| layout.place(&dataset.name).volume())
            .filter(|name| name.starts_with(&prefix) && !name.contains('/'))
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
            let own = preview.own_origin(name.as_str());
            let root = layout.root(&name);
            if datasets.iter().any(|dataset| dataset.name == root) {
                let origin = self.local_origin(&root, Mark::Origin).await;
                if made_here(origin, own.as_ref()) {
                    self.destroy(&root, Holds::ClosedPreview).await?;
                    closed.destroyed.push(docker.clone());
                } else {
                    let id = self.discard(&root, TrashPlace::Root, &name).await?;
                    closed.trashed.push(PreviewTrashed {
                        name: docker.clone(),
                        id,
                    });
                }
            }
            let parent = layout.slot_parent(&name);
            if datasets.iter().any(|dataset| dataset.name == parent) {
                let fs = layout.slot_fs(&name);
                let fs = datasets
                    .iter()
                    .any(|dataset| dataset.name == fs)
                    .then_some(fs.as_str());
                let origin = self.origin_of(Some(&parent), fs.map(|fs| (fs, None))).await;
                match fs {
                    None => self.destroy(&parent, Holds::Nothing).await?,
                    Some(_) if made_here(origin, own.as_ref()) => {
                        self.destroy(&parent, Holds::ClosedPreview).await?;
                        if closed.destroyed.last() != Some(&docker) {
                            closed.destroyed.push(docker.clone());
                        }
                    }
                    Some(fs) if self.newest_guid(fs).await?.is_none() => {
                        self.destroy(&parent, Holds::Unreceived).await?;
                    }
                    Some(_) => {
                        let id = self.discard(&parent, TrashPlace::Slot, &name).await?;
                        closed.trashed.push(PreviewTrashed { name: docker, id });
                    }
                }
            }
            self.receives.forget(&name);
        }
        Ok(closed)
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
