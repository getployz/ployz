//! SeedVolume: a new Volume holding one restore point of another, received on this Machine
//! from the writer's own snapshot. The copy grows in a slot of `into` marked with the point
//! it pulls, and lands as a root the next call finds complete. Every call answers the copy
//! of `into` as it stands, so the same request resent converges.

use std::process::Stdio;

use axum::{Json, extract::State};
use ployz_core::{RpcError, SeedVolumeRequest, SwitchError, VolumeCopyView};
use tokio::process::Command;

use super::{
    VolumeStorage,
    lease::internal,
    mirror,
    storage::{PROMOTE_PROPERTY, PULL_PROPERTY, root_dataset, slot_fs, slot_parent},
};

/// On a seeded root: the guid of the restore point it holds.
pub(super) const SEED_PROPERTY: &str = "ployz:seed";

impl VolumeStorage {
    async fn seed_volume(&self, request: &SeedVolumeRequest) -> Result<VolumeCopyView, RpcError> {
        let name = mirror::name(&request.name)?;
        let into = mirror::name(&request.into)?;
        let guid = request.guid;
        let _held = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let root = root_dataset(&pool, &into);
        let parent = slot_parent(&pool, &into);
        let fs = slot_fs(&pool, &into);
        let slot = Self::slot(&datasets, &pool, &into).is_some();
        if let Some(seeded) = Self::dataset(&datasets, &pool, &into).map_err(internal)? {
            let seed = self
                .property(&seeded.name, SEED_PROPERTY)
                .await
                .map_err(internal)?;
            if seed != Some(guid.to_string()) {
                return Err(SwitchError::Precondition
                    .rpc_error(format!("Volume {into} already exists on this Machine")));
            }
            let snapshots = self.snapshots(&seeded.name).await.map_err(internal)?;
            if let Some(received) = snapshots.iter().find(|snapshot| snapshot.guid == guid) {
                let parent = slot.then_some(parent.as_str());
                self.finish_landing(&root, parent, &received.name.to_string())
                    .await?;
            }
            return self.seeded(&pool, &into).await;
        }
        if self.receives.running(&into) {
            return self.seeded(&pool, &into).await;
        }
        if let Some(failure) = self.receives.failure(&into) {
            // Answer the failure once; the next call starts the copy over.
            let view = self.seeded(&pool, &into).await;
            self.receives.forget(&into);
            tracing::warn!(%into, failure, "Volume seed copy failed");
            return view;
        }
        if Self::slot_fs(&datasets, &pool, &into).is_some() {
            let landed = self.snapshots(&fs).await.map_err(internal)?;
            if let Some(newest) = landed.first().filter(|newest| newest.guid == guid) {
                self.land(&fs, &parent, &root, &newest.name.to_string(), request)
                    .await?;
                return self.seeded(&pool, &into).await;
            }
        }
        let source = root_dataset(&pool, &name);
        let Some(bound) = datasets
            .iter()
            .find(|dataset| dataset.name == source)
            .map(|dataset| dataset.refquota)
        else {
            return Err(SwitchError::Precondition
                .rpc_error(format!("Volume {name} has no writer on this Machine")));
        };
        let point = self
            .snapshots(&source)
            .await
            .map_err(internal)?
            .into_iter()
            .find(|snapshot| snapshot.guid == guid)
            .ok_or_else(|| {
                SwitchError::RestorePointGone.rpc_error(format!(
                    "restore point {guid} is no longer on Volume {name}"
                ))
            })?;
        if bound == 0 {
            return Err(SwitchError::Precondition
                .rpc_error(format!("Volume {name} has no bound to seed {into} with")));
        }
        if slot {
            // Another point, or a copy that never finished: it goes and the receive starts over.
            self.zfs(&["destroy", "-r", &parent])
                .await
                .map_err(internal)?;
        } else {
            self.ensure_commitment(&pool, &datasets, bound)
                .await
                .map_err(internal)?;
            self.ensure_mirror_root(&pool, &datasets)
                .await
                .map_err(internal)?;
        }
        self.create_bounded_slot(&parent, bound).await?;
        self.zfs(&["set", &format!("{PULL_PROPERTY}={}", point.name), &parent])
            .await
            .map_err(internal)?;
        let outcome = self.receives.start(&into);
        let storage = self.clone();
        let snapshot = format!("{source}@{}", point.name);
        tokio::spawn(async move {
            let result = storage.copy_snapshot(&snapshot, &fs, bound).await;
            *outcome.lock().expect("outcome lock") = Some(result);
        });
        self.seeded(&pool, &into).await
    }

    /// The copy of `into` as the datasets stand now: a slot while it copies, a root once landed.
    async fn seeded(
        &self,
        pool: &ployzd::machine_pool::MachinePool,
        into: &super::DockerVolumeName,
    ) -> Result<VolumeCopyView, RpcError> {
        let datasets = self.datasets(pool).await.map_err(internal)?;
        let copy = self.copy(&datasets, pool, into).await.map_err(internal)?;
        Ok(VolumeCopyView { copy })
    }

    /// Makes the complete copy the root of `into`, marked with its point and unregistered
    /// until Docker's Create adopts it.
    async fn land(
        &self,
        fs: &str,
        parent: &str,
        root: &str,
        snapshot: &str,
        request: &SeedVolumeRequest,
    ) -> Result<(), RpcError> {
        self.zfs(&["set", &format!("{SEED_PROPERTY}={}", request.guid), fs])
            .await
            .map_err(internal)?;
        self.zfs(&["set", &format!("{PROMOTE_PROPERTY}=on"), fs])
            .await
            .map_err(internal)?;
        self.zfs(&["rename", fs, root]).await.map_err(internal)?;
        self.finish_landing(root, Some(parent), snapshot).await
    }

    /// The landing after the rename, repeatable. The received snapshot goes last, so a root
    /// still holding it has not finished landing. Its name belongs to the source's runs,
    /// and the new Volume's own runs name theirs from one.
    async fn finish_landing(
        &self,
        root: &str,
        parent: Option<&str>,
        snapshot: &str,
    ) -> Result<(), RpcError> {
        self.zfs(&["inherit", "mountpoint", root])
            .await
            .map_err(internal)?;
        self.zfs(&["set", "readonly=off", root])
            .await
            .map_err(internal)?;
        if let Some(parent) = parent {
            self.zfs(&["destroy", parent]).await.map_err(internal)?;
        }
        self.zfs(&["destroy", &format!("{root}@{snapshot}")])
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// `zfs send` of one local snapshot piped into a bounded, read-only receive of `fs`.
    async fn copy_snapshot(&self, snapshot: &str, fs: &str, bound: u64) -> Result<(), String> {
        let mut send = Command::new(&self.zfs)
            .args(["send", snapshot])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not run zfs send: {error}"))?;
        let stream = send
            .stdout
            .take()
            .expect("zfs send stdout is piped")
            .into_owned_fd()
            .map_err(|error| format!("could not hand the send stream over: {error}"))?;
        let refquota = format!("refquota={bound}");
        let receive = Command::new(&self.zfs)
            .args([
                "receive",
                "-u",
                "-s",
                "-o",
                "readonly=on",
                "-o",
                &refquota,
                fs,
            ])
            .stdin(Stdio::from(stream))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not run zfs receive: {error}"))?;
        let (sent, received) = tokio::join!(send.wait_with_output(), receive.wait_with_output());
        let received = received.map_err(|error| format!("zfs receive did not finish: {error}"))?;
        let sent = sent.map_err(|error| format!("zfs send did not finish: {error}"))?;
        if !received.status.success() {
            return Err(format!(
                "zfs receive into {fs} failed: {}",
                String::from_utf8_lossy(&received.stderr).trim()
            ));
        }
        if !sent.status.success() {
            return Err(format!(
                "zfs send of {snapshot} failed: {}",
                String::from_utf8_lossy(&sent.stderr).trim()
            ));
        }
        self.finish_copy(fs, bound).await
    }
}

pub(super) async fn seed_volume(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SeedVolumeRequest>,
) -> Json<Result<VolumeCopyView, RpcError>> {
    Json(storage.seed_volume(&request).await)
}
