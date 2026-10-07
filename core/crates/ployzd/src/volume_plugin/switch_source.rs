//! Source switch admission, freezing, and the irreversible handover of a Volume.

use std::sync::{Arc, Mutex};

use axum::{Json, extract::State};
use ployz_core::{
    ContainerId, Cycle, HandOverRequest, LeaseRecord, MirrorRequest, RpcError,
    SourceContainerRequest, SwitchError, SwitchReply, WriterMarker,
};

use super::{
    Dataset, DockerVolumeName, VolumeStorage, checked_command,
    lease::{MIRROR_PROPERTY, WRITER_PROPERTY, internal, slot_parent},
    mirror::{Leased, name},
};

pub(super) const HOLDER_PROPERTY: &str = "ployz:source-container";

#[derive(Clone)]
pub(super) struct MountGrant {
    pub(super) name: String,
    pub(super) record: LeaseRecord,
    pub(super) container: ContainerId,
    pub(super) mountpoint: String,
}

struct Granted(Arc<Mutex<Option<MountGrant>>>);

impl Drop for Granted {
    fn drop(&mut self) {
        *self.0.lock().expect("mount grant is never poisoned") = None;
    }
}

pub(super) fn precondition(message: impl Into<String>) -> RpcError {
    SwitchError::Precondition.rpc_error(message)
}

impl VolumeStorage {
    pub(super) fn require_no_mount_grant(&self, name: &DockerVolumeName) -> super::Result<()> {
        if self
            .mount_grant
            .lock()
            .expect("mount grant is never poisoned")
            .as_ref()
            .is_some_and(|grant| grant.name == name.0)
        {
            return Err(format!(
                "VolumeSwitching: Volume {name} is restarting its source Container"
            )
            .into());
        }
        Ok(())
    }

    pub(super) async fn docker(&self, arguments: &[&str]) -> super::Result<String> {
        self.mutation.raise_docker_bound();
        checked_command(&self.docker, arguments).await
    }

    pub(super) async fn holders(&self, name: &DockerVolumeName) -> super::Result<Vec<String>> {
        Ok(self
            .docker(&[
                "ps",
                "--all",
                "--quiet",
                "--no-trunc",
                "--filter",
                &format!("volume={name}"),
            ])
            .await?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    async fn require_holder(
        &self,
        name: &DockerVolumeName,
        container: &ContainerId,
    ) -> Result<(), RpcError> {
        let holders = self.holders(name).await.map_err(internal)?;
        if holders != [container.as_str()] {
            return Err(precondition(format!(
                "Volume {name} must be held only by Container {container}"
            )));
        }
        let managed = self
            .docker(&[
                "ps",
                "--all",
                "--quiet",
                "--no-trunc",
                "--filter",
                &format!("id={container}"),
                "--filter",
                "label=ployz.managed",
            ])
            .await
            .map_err(internal)?;
        if managed.trim() != container.as_str() {
            return Err(precondition(format!(
                "Container {container} is not managed by Ployz"
            )));
        }
        Ok(())
    }

    fn source_root<'scope>(
        scope: &'scope Leased,
        name: &DockerVolumeName,
    ) -> Result<&'scope Dataset, RpcError> {
        let root = Self::dataset(&scope.datasets, &scope.pool, name)
            .map_err(internal)?
            .ok_or_else(|| precondition(format!("Volume {name} has no source root")))?;
        root.require_provisioned(name).map_err(internal)?;
        Ok(root)
    }

    async fn set_writer(&self, root: &str, writer: WriterMarker) -> Result<(), RpcError> {
        self.zfs(&["set", &format!("{WRITER_PROPERTY}={writer}"), root])
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn require_source_container(
        &self,
        root: &Dataset,
        request: &SourceContainerRequest,
    ) -> Result<(), RpcError> {
        if let Some(holder) = self
            .property(&root.name, HOLDER_PROPERTY)
            .await
            .map_err(internal)?
            && holder != request.container_id.as_str()
        {
            return Err(precondition(
                "the source Container differs from the withdrawn Container",
            ));
        }
        self.require_holder(&name(&request.name)?, &request.container_id)
            .await
    }

    async fn withdraw_source(
        &self,
        request: &SourceContainerRequest,
        verb: &'static str,
    ) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = Self::source_root(&scope, &name)?;
        let writer = self.writer_marker(root).await.map_err(internal)?;
        if !matches!(writer, WriterMarker::Idle | WriterMarker::Stopping) || root.readonly {
            return Err(precondition(format!(
                "Volume {name} cannot withdraw from {writer}"
            )));
        }
        self.require_source_container(root, request).await?;
        let root = root.name.clone();
        scope.admitted.lease.cycle = Cycle::Open;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record(verb);
        self.zfs(&[
            "set",
            &format!("{HOLDER_PROPERTY}={}", request.container_id),
            &root,
        ])
        .await
        .map_err(internal)?;
        self.set_writer(&root, WriterMarker::Stopping).await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn freeze_source(
        &self,
        request: &SourceContainerRequest,
    ) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = Self::source_root(&scope, &name)?;
        let writer = self.writer_marker(root).await.map_err(internal)?;
        if !matches!(writer, WriterMarker::Stopping | WriterMarker::Frozen { .. }) {
            return Err(precondition(format!(
                "Volume {name} cannot freeze from {writer}"
            )));
        }
        self.require_source_container(root, request).await?;
        let root_name = root.name.clone();
        let mounted = root.mounted;
        scope.admitted.lease.cycle = Cycle::Open;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("Freeze");
        self.docker(&["stop", request.container_id.as_str()])
            .await
            .map_err(internal)?;
        self.zfs(&["set", "readonly=on", &root_name])
            .await
            .map_err(internal)?;
        if mounted && let Err(error) = self.zfs(&["unmount", &root_name]).await {
            if writer == WriterMarker::Stopping {
                self.zfs(&["set", "readonly=off", &root_name])
                    .await
                    .map_err(internal)?;
            }
            return Err(
                SwitchError::Busy.rpc_error(format!("Volume {name} could not unmount: {error}"))
            );
        }
        if self
            .property(&root_name, "readonly")
            .await
            .map_err(internal)?
            .as_deref()
            != Some("on")
        {
            return Err(precondition(format!(
                "Volume {name} did not become read-only"
            )));
        }
        let snapshots = self.snapshots(&root_name).await.map_err(internal)?;
        let final_name = format!("f-{}", request.switch.lease);
        let final_snapshot = match writer {
            WriterMarker::Frozen { guid } => {
                snapshots.iter().find(|snapshot| snapshot.guid == guid)
            }
            WriterMarker::Idle
            | WriterMarker::Stopping
            | WriterMarker::Thawing
            | WriterMarker::Handed { .. } => snapshots
                .iter()
                .find(|snapshot| snapshot.name.as_str() == final_name),
        };
        let guid = if let Some(snapshot) = final_snapshot {
            snapshot.guid
        } else {
            if matches!(writer, WriterMarker::Frozen { .. }) {
                return Err(precondition("the frozen snapshot is missing"));
            }
            self.zfs(&["snapshot", &format!("{root_name}@{final_name}")])
                .await
                .map_err(internal)?;
            self.snapshots(&root_name)
                .await
                .map_err(internal)?
                .into_iter()
                .find(|snapshot| snapshot.name.as_str() == final_name)
                .ok_or_else(|| precondition("ZFS did not report the final snapshot"))?
                .guid
        };
        self.set_writer(&root_name, WriterMarker::Frozen { guid })
            .await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn hand_over_source(&self, request: &HandOverRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = Self::source_root(&scope, &name)?;
        let writer = self.writer_marker(root).await.map_err(internal)?;
        if !matches!(writer, WriterMarker::Frozen { guid } | WriterMarker::Handed { guid } if guid == request.guid)
            || !root.readonly
            || !self
                .snapshots(&root.name)
                .await
                .map_err(internal)?
                .iter()
                .any(|snapshot| {
                    snapshot.guid == request.guid && snapshot.name.as_str().starts_with("f-")
                })
        {
            return Err(precondition(
                "HandOver requires the source's read-only final snapshot",
            ));
        }
        let root = root.name.clone();
        scope.admitted.lease.cycle = Cycle::Open;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("HandOver");
        self.set_writer(&root, WriterMarker::Handed { guid: request.guid })
            .await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn thaw_source(&self, request: &SourceContainerRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = Self::source_root(&scope, &name)?;
        let writer = self.writer_marker(root).await.map_err(internal)?;
        if matches!(writer, WriterMarker::Handed { .. }) {
            return Err(precondition(
                "Volume is handed over; continue the move on the target",
            ));
        }
        if scope.replayed()
            && scope.admitted.lease.cycle == Cycle::Closed
            && writer == WriterMarker::Idle
        {
            return self.reply(&scope.pool, &name, scope.admitted).await;
        }
        self.require_source_container(root, request).await?;
        let root_name = root.name.clone();
        scope.admitted.lease.cycle = Cycle::Open;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("Thaw");
        self.set_writer(&root_name, WriterMarker::Thawing).await?;
        self.zfs(&["set", "readonly=off", &root_name])
            .await
            .map_err(internal)?;
        let datasets = self.datasets(&scope.pool).await.map_err(internal)?;
        let root = Self::dataset(&datasets, &scope.pool, &name)
            .map_err(internal)?
            .ok_or_else(|| precondition("the source root disappeared"))?;
        root.require_not_switching(&name).map_err(internal)?;
        if !root.mounted {
            self.zfs(&["mount", &root_name]).await.map_err(internal)?;
        }
        self.start_granted(MountGrant {
            name: name.to_string(),
            record: scope.admitted.lease,
            container: request.container_id,
            mountpoint: name.mountpoint(),
        })
        .await?;
        self.set_writer(&root_name, WriterMarker::Idle).await?;
        self.zfs(&["inherit", HOLDER_PROPERTY, &root_name])
            .await
            .map_err(internal)?;
        scope.admitted.lease.cycle = Cycle::Closed;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// Starts the grant's Container while its Mount may use the root the open record holds.
    pub(super) async fn start_granted(&self, grant: MountGrant) -> Result<(), RpcError> {
        let container = grant.container;
        *self
            .mount_grant
            .lock()
            .expect("mount grant is never poisoned") = Some(grant);
        let granted = Granted(Arc::clone(&self.mount_grant));
        self.docker(&["start", container.as_str()])
            .await
            .map_err(internal)?;
        drop(granted);
        Ok(())
    }

    pub(super) async fn granted_mountpoint(
        &self,
        name: &DockerVolumeName,
    ) -> super::Result<Option<String>> {
        let grant = self
            .mount_grant
            .lock()
            .expect("mount grant is never poisoned")
            .clone();
        let Some(grant) = grant.filter(|grant| grant.name == name.0) else {
            return Ok(None);
        };
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        if self.lease_record(&datasets, &pool, name).await? != Some(grant.record)
            || self.holders(name).await? != [grant.container.as_str()]
        {
            return Err(
                format!("VolumeSwitching: Volume {name} has no grant for this Container").into(),
            );
        }
        let root = Self::dataset(&datasets, &pool, name)?.ok_or("the granted root disappeared")?;
        root.require_provisioned(name)?;
        root.require_not_switching(name)?;
        if !root.mounted {
            return Err("the granted root is not mounted".into());
        }
        Ok(Some(grant.mountpoint))
    }

    async fn close_source(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = Self::dataset(&scope.datasets, &scope.pool, &name).map_err(internal)?;
        if let Some(root) = root {
            root.require_provisioned(&name).map_err(internal)?;
            if !matches!(
                self.writer_marker(root).await.map_err(internal)?,
                WriterMarker::Handed { .. }
            ) || !root.readonly
            {
                return Err(precondition("Close requires a handed read-only source"));
            }
            if scope.fs(&name).is_some() {
                return Err(precondition("the source already has a mirror copy"));
            }
        } else if scope.fs(&name).is_none() {
            return Err(precondition(
                "Close requires the source or its renamed mirror",
            ));
        }
        let root = root.map(|root| (root.name.clone(), root.refquota, root.mounted));
        scope.admitted.lease.cycle = Cycle::Open;
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("Close");
        for holder in self.holders(&name).await.map_err(internal)? {
            self.docker(&["rm", &holder]).await.map_err(internal)?;
        }
        let parent = slot_parent(&scope.pool, &name);
        let fs = format!("{parent}/fs");
        if let Some((root, bound, mounted)) = root {
            if mounted {
                self.zfs(&["unmount", &root]).await.map_err(internal)?;
            }
            self.ensure_mirror_root(&scope.pool, &scope.datasets)
                .await
                .map_err(internal)?;
            if scope.slot(&name).is_none() {
                self.create_slot_parent(&parent, bound)
                    .await
                    .map_err(internal)?;
            }
            self.zfs(&["rename", &root, &fs]).await.map_err(internal)?;
        }
        self.zfs(&["set", "readonly=on", &fs])
            .await
            .map_err(internal)?;
        self.zfs(&["inherit", WRITER_PROPERTY, &fs])
            .await
            .map_err(internal)?;
        self.zfs(&["inherit", HOLDER_PROPERTY, &fs])
            .await
            .map_err(internal)?;
        self.zfs(&["set", &format!("{MIRROR_PROPERTY}=idle"), &parent])
            .await
            .map_err(internal)?;
        ployzd::faults::kill_inside("Close");
        self.docker(&["volume", "rm", "--force", &name.0])
            .await
            .map_err(internal)?;
        scope.admitted.lease.cycle = Cycle::Closed;
        self.reply(&scope.pool, &name, scope.admitted).await
    }
}

pub(super) async fn withdraw(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SourceContainerRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.withdraw_source(&request, "Withdraw").await)
}

pub(super) async fn mark_stopping(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SourceContainerRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(
        storage
            .withdraw_source(&request, "MarkContainerStopping")
            .await,
    )
}

pub(super) async fn freeze(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SourceContainerRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.freeze_source(&request).await)
}

pub(super) async fn hand_over(
    State(storage): State<VolumeStorage>,
    Json(request): Json<HandOverRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.hand_over_source(&request).await)
}

pub(super) async fn thaw(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SourceContainerRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.thaw_source(&request).await)
}

pub(super) async fn close(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.close_source(&request).await)
}
