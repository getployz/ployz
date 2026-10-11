//! SeedVolume: a new Volume holding one restore point of another, received on this Machine
//! from the writer's own snapshot. The copy grows under the mirror parent, where only
//! InspectReceive sees it, and lands as a root the next call finds complete.

use std::process::Stdio;

use axum::{Json, extract::State};
use ployz_core::{RpcError, SeedVolumeRequest, SwitchError, SwitchReply};
use tokio::process::Command;

use super::{
    VolumeStorage,
    lease::{internal, root_dataset, slot_fs, slot_parent},
    mirror,
    transfer::{RECEIVE_PROPERTY, ReceiveRecord},
};

/// On a seeded root: the guid of the restore point it holds.
pub(super) const SEED_PROPERTY: &str = "ployz:seed";

impl VolumeStorage {
    async fn seed_volume(&self, request: &SeedVolumeRequest) -> Result<SwitchReply, RpcError> {
        let name = mirror::name(&request.name)?;
        let into = mirror::name(&request.into)?;
        let guid = request.guid;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = root_dataset(&scope.pool, &into);
        if let Some(seeded) =
            Self::dataset(&scope.datasets, &scope.pool, &into).map_err(internal)?
        {
            let seed = self
                .property(&seeded.name, SEED_PROPERTY)
                .await
                .map_err(internal)?;
            if seed == Some(guid.to_string()) {
                let snapshots = self.snapshots(&seeded.name).await.map_err(internal)?;
                if let Some(received) = snapshots.iter().find(|snapshot| snapshot.guid == guid) {
                    let parent = scope
                        .slot(&into)
                        .is_some()
                        .then(|| slot_parent(&scope.pool, &into));
                    self.finish_landing(&root, parent.as_deref(), &received.name.to_string())
                        .await?;
                }
                return self.reply(&scope.pool, &name, scope.admitted).await;
            }
            return Err(SwitchError::Precondition
                .rpc_error(format!("Volume {into} already exists on this Machine")));
        }
        if self.receives.running(&into) {
            return Err(SwitchError::Busy.rpc_error(format!("the copy into {into} is running")));
        }
        let parent = slot_parent(&scope.pool, &into);
        let fs = slot_fs(&scope.pool, &into);
        if let Some(copy) = scope.fs(&into) {
            let landed = self.snapshots(&copy.name).await.map_err(internal)?;
            if let Some(newest) = landed.first().filter(|newest| newest.guid == guid) {
                self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
                    .await?;
                ployzd::faults::kill_after_record("SeedVolume");
                self.land(&fs, &parent, &root, &newest.name.to_string(), request)
                    .await?;
                return self.reply(&scope.pool, &name, scope.admitted).await;
            }
        }
        let source = scope.require_root(&name)?;
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
        let bound = scope
            .datasets
            .iter()
            .find(|dataset| dataset.name == source)
            .map_or(0, |dataset| dataset.refquota);
        if bound == 0 {
            return Err(SwitchError::Precondition
                .rpc_error(format!("Volume {name} has no bound to seed {into} with")));
        }
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("SeedVolume");
        let stale = scope.fs(&into).is_some();
        let unplaced = scope.slot(&into).is_none();
        if stale {
            // Another point, or a copy that never finished: it goes and the receive starts over.
            self.zfs(&["destroy", "-r", &parent])
                .await
                .map_err(internal)?;
        } else if unplaced {
            self.ensure_commitment(&scope.pool, &scope.datasets, bound)
                .await
                .map_err(internal)?;
            self.ensure_mirror_root(&scope.pool, &scope.datasets)
                .await
                .map_err(internal)?;
        }
        if stale || unplaced {
            self.create_slot_parent(&parent, bound)
                .await
                .map_err(internal)?;
        }
        let record = ReceiveRecord {
            lease: request.switch.lease,
            pos: request.switch.pos,
            target: point.name.clone(),
        };
        self.zfs(&["set", &format!("{RECEIVE_PROPERTY}={record}"), &parent])
            .await
            .map_err(internal)?;
        let outcome = self.receives.start(&into);
        let storage = self.clone();
        let snapshot = format!("{source}@{}", point.name);
        tokio::spawn(async move {
            let result = storage.copy_snapshot(&snapshot, &fs, bound).await;
            if let Err(error) = &result {
                tracing::warn!(%fs, error, "Volume seed copy failed");
            }
            *outcome.lock().expect("outcome lock") = Some(result);
        });
        self.reply(&scope.pool, &name, scope.admitted).await
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
        self.rename_unregistered(fs, root, request.switch.lease)
            .await?;
        self.finish_landing(root, Some(parent), snapshot).await
    }

    /// The landing after the rename, repeatable. The received snapshot goes last, so a root
    /// still holding it has not finished landing. Its name belongs to the source's lease,
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
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.seed_volume(&request).await)
}
