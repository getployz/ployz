//! SeedVolume: a new Volume holding one restore point of another, received on this Machine
//! from the writer's own snapshot. The copy grows in a slot of `into` marked with the point
//! it pulls, and lands as a root the next call finds complete. Every call answers the copy
//! of `into` as it stands, so the same request resent converges.
//!
//! A seed a Copy step admitted is bound to the step's birth: its slot records the birth's
//! origin, and the root lands carrying it. Only a slot or root of `into` that carries that
//! origin is this copy's; any other refuses, so the seed never adopts another copy of
//! `into`. It starts a copy, or starts one over, only when the daemon says the step may.

use std::process::Stdio;

use crate::{
    dataset_layout::DatasetLayout,
    machine_pool::MachinePool,
    volume_copy::{CopyBinding, SeedCopy},
    volume_send::begin::{self, READ_BOUND},
};
use axum::{Json, extract::State};
use ployz_core::{
    DockerVolumeName, RpcError, SeedVolumeRequest, Snapshot, SnapshotGuid, SwitchError,
    VolumeCopyView, VolumeOrigin,
};
use tokio::{io::AsyncWriteExt as _, process::Command};

use super::{
    Dataset, PluginError, VolumeName, VolumeStorage,
    holders::{Unheld, copy_held_for_other_data, copy_unlisted},
    inspect::answering,
    marks::{Mark, Marks},
    origin::Origin,
    storage::HeldMutation,
    transfer::{Busy, Stop, ZfsChild, exited},
};

/// Where a Seed of `into` at one guid stands on this Machine, as
/// [`VolumeStorage::seed_progress`] reads it under the mutation. Nothing records it: each
/// call reads it again from `into`'s root and slot, their marks and snapshots, and this
/// plugin process's one receive registry entry for `into`. That entry is keyed by Volume
/// name, and a SetVolumeCopy pull of `into` writes it too. The root is read before the
/// registry, and only [`Self::Stale`] and [`Self::Absent`] go on to read the source.
enum SeedProgress {
    /// A root of `into` whose `ployz:seed` is not the guid. Under a Copy step, also a root
    /// or a slot of `into` that does not carry the step's birth, and a receive of `into`
    /// whose slot does not.
    Taken,
    /// A root whose `ployz:seed` is the guid and which still holds a run snapshot of that
    /// guid, the one [`Renamed::finish_landing`] destroys last.
    Renamed(Renamed),
    /// A root whose `ployz:seed` is the guid and which holds no run snapshot of that guid.
    Landed,
    /// No root, and the registry's receive of `into` is alive, or a child it dropped is,
    /// whatever its target and whether or not a slot of `into` exists:
    /// [`super::transfer::Receives::take_failure`] answers [`Busy`] before it compares
    /// targets.
    Copying,
    /// No root, and the registry's receive of `into` ended failed, with no child it dropped
    /// left alive, for the target the slot's `ployz:pull` names. So a failed SetVolumeCopy
    /// pull of `into` whose target has the point's name selects it too. Reading it clears
    /// it.
    Failed(String),
    /// No root, no receive alive and no failure answered, and the slot's `fs` has the guid
    /// as its newest run snapshot. That includes an `fs` a stopped [`Copied::land`] left
    /// carrying `ployz:seed` and `ployz:promote` before its rename.
    Copied(Copied),
    /// No root, no receive alive and no failure answered, and a slot of `into` whose `fs`
    /// is absent or has a newest run snapshot other than the guid, or none.
    Stale(Stale),
    /// No root, no slot, and no receive alive.
    Absent,
}

/// The root [`Copied::land`] renamed, still holding the received snapshot.
struct Renamed {
    root: String,
    /// The slot parent, when one is left.
    parent: Option<String>,
    received: String,
}

impl Renamed {
    /// The landing after the rename, repeatable. The received snapshot goes last, so a root
    /// still holding it has not finished landing. Its name belongs to the source's runs,
    /// and the new Volume's own runs name theirs from one.
    ///
    /// A parent still holding a record lost its copy before completion stamped it, so the
    /// root may not carry the origin the record names. That root stays read-only and the
    /// parent stays with its record.
    async fn finish_landing(self, storage: &VolumeStorage) -> Result<(), RpcError> {
        let root = &self.root;
        if let Some(parent) = &self.parent
            && storage.record(parent).await?.is_some()
        {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "{root} left slot {parent} before its origin was stamped; forget it and seed again"
            )));
        }
        storage.zfs(&["inherit", "mountpoint", root]).await?;
        storage.zfs(&["set", "readonly=off", root]).await?;
        if let Some(parent) = &self.parent {
            storage.zfs(&["destroy", parent]).await?;
        }
        storage
            .zfs(&["destroy", &format!("{root}@{}", self.received)])
            .await?;
        Ok(())
    }
}

/// The slot's `fs`, whose newest run snapshot is the one the Seed asks for.
struct Copied {
    fs: String,
    parent: String,
    root: String,
    snapshot: String,
}

impl Copied {
    /// Makes the complete copy the root of `into`, marked with its point and unregistered
    /// until Docker's Create adopts it. Every Container that already mounts `into` must
    /// expect the origin the copy carries.
    async fn land(
        self,
        storage: &VolumeStorage,
        into: &DockerVolumeName,
        guid: SnapshotGuid,
    ) -> Result<(), RpcError> {
        let fs = &self.fs;
        storage.complete(&self.parent, fs).await?;
        let carried = storage.point(fs, &self.snapshot).await?.origin;
        storage
            .admit_holders(into, carried)
            .await
            .map_err(|unheld| match unheld {
                Unheld::Foreign => SwitchError::Foreign.rpc_error(copy_held_for_other_data(into)),
                Unheld::Unlisted(cause) => {
                    SwitchError::DockerUnavailable.rpc_error(copy_unlisted(into, &cause))
                }
            })?;
        // The slot may be a mirror that pulled this point; landed, it is no received copy.
        storage.marks(fs).apply(&Marks::landed(guid)).await?;
        storage.zfs(&["rename", fs, &self.root]).await?;
        Renamed {
            root: self.root,
            parent: Some(self.parent),
            received: self.snapshot,
        }
        .finish_landing(storage)
        .await
    }
}

/// A slot of `into` whose `fs`, when it has one, does not have the point as its newest run
/// snapshot.
struct Stale {
    parent: String,
    /// The slot's `fs`, when a receive began one.
    fs: Option<String>,
}

impl Stale {
    /// Destroys the slot so the copy starts over, unless its `fs` holds a point this Machine
    /// may alone hold.
    async fn clear(self, storage: &VolumeStorage, into: &VolumeName) -> Result<(), RpcError> {
        if let Some(fs) = &self.fs
            && storage.written_here(fs).await?
        {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "Volume {into} is held in slot {}; promote or restore it before seeding it",
                self.parent
            )));
        }
        storage.zfs(&["destroy", "-r", &self.parent]).await?;
        Ok(())
    }
}

/// The source's restore point the Seed copies, the origin the copy records, and the bound
/// the copy takes from the source.
struct Source {
    root: String,
    point: Snapshot,
    /// The point's own origin, or under a Copy step the birth's.
    record: Origin,
    bound: u64,
}

impl CopyBinding {
    /// Refuse to start the copy of `into` over once the step's window has closed.
    fn may_restart(&self, into: &VolumeName) -> Result<(), RpcError> {
        if self.open {
            return Ok(());
        }
        Err(SwitchError::Expired.rpc_error(format!(
            "The approval to copy into Volume {into} expired before its copy started; approve the restore again"
        )))
    }

    /// Refuse to begin a copy into `into` that is gone after a copy began under the step,
    /// since it was deleted, or once the step's window has closed.
    fn may_begin(&self, into: &VolumeName) -> Result<(), RpcError> {
        if self.began {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "Volume {into} was deleted after its copy began; approve the restore again"
            )));
        }
        self.may_restart(into)
    }
}

impl VolumeStorage {
    /// The seed `request` asks for, bound to `copy` when a Copy step admitted it.
    async fn seed_volume(
        &self,
        request: &SeedVolumeRequest,
        copy: Option<&CopyBinding>,
    ) -> Result<VolumeCopyView, RpcError> {
        let name = VolumeName::try_from(&request.name)?;
        let into = VolumeName::try_from(&request.into)?;
        let guid = request.guid;
        let (held, _installation) = self.admit_mutation().await?;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let birth = copy.map(|copy| copy.birth);
        match self
            .seed_progress(&held, &pool, &datasets, &into, (guid, birth))
            .await?
        {
            SeedProgress::Taken => Err(SwitchError::Precondition
                .rpc_error(format!("Volume {into} already exists on this Machine"))),
            SeedProgress::Renamed(renamed) => {
                renamed.finish_landing(self).await?;
                self.seeded(&pool, &into).await
            }
            SeedProgress::Landed | SeedProgress::Copying => self.seeded(&pool, &into).await,
            // Answered once. The next call finds the slot Copied if the receive completed
            // before the copy failed, and Stale otherwise.
            SeedProgress::Failed(failure) => {
                tracing::warn!(%into, failure, "Volume seed copy failed");
                self.seeded(&pool, &into).await.map(|view| VolumeCopyView {
                    copy: answering(view.copy, failure),
                })
            }
            SeedProgress::Copied(copied) => {
                copied.land(self, &request.into, guid).await?;
                self.seeded(&pool, &into).await
            }
            SeedProgress::Stale(stale) => {
                if let Some(copy) = copy {
                    copy.may_restart(&into)?;
                }
                let source = self
                    .seed_source(&pool, &datasets, (request, &name, &into), copy)
                    .await?;
                stale.clear(self, &into).await?;
                self.copy_point(&held, &pool, &into, source).await
            }
            SeedProgress::Absent => {
                if let Some(copy) = copy {
                    copy.may_begin(&into)?;
                }
                let source = self
                    .seed_source(&pool, &datasets, (request, &name, &into), copy)
                    .await?;
                self.ensure_commitment(&pool, &datasets, source.bound)
                    .await?;
                self.ensure_mirror_root(&pool, &datasets).await?;
                self.copy_point(&held, &pool, &into, source).await
            }
        }
    }

    /// The one read of [`SeedProgress`], under the mutation `_held`. Its only registry read
    /// is [`super::transfer::Receives::take_failure`], which answers and clears in one
    /// step: a receive task ends outside the mutation, so two reads could each see a
    /// different side of that end. `birth` is a Copy step's, which every root, slot and
    /// receive of `into` the seed goes on with must carry; a slot that holds nothing yet
    /// carries none and may be started over.
    async fn seed_progress(
        &self,
        _held: &HeldMutation,
        pool: &MachinePool,
        datasets: &[Dataset],
        into: &VolumeName,
        (guid, birth): (SnapshotGuid, Option<VolumeOrigin>),
    ) -> Result<SeedProgress, RpcError> {
        let root = DatasetLayout::of(pool).root(into);
        let parent = DatasetLayout::of(pool).slot_parent(into);
        let slot = Self::slot(datasets, pool, into).is_some();
        if Self::dataset(datasets, pool, into)?.is_some() {
            let seed = self.marks(&root).get(Mark::Seed).await?;
            if seed != Some(guid.to_string()) {
                return Ok(SeedProgress::Taken);
            }
            let snapshots = self.snapshots(&root).await?;
            if let Some(birth) = birth {
                let newest = snapshots.first().map(|snapshot| &snapshot.name);
                if self.origin_of(None, Some((&root, newest))).await? != Some(birth) {
                    return Ok(SeedProgress::Taken);
                }
            }
            return Ok(
                match snapshots.into_iter().find(|snapshot| snapshot.guid == guid) {
                    Some(received) => SeedProgress::Renamed(Renamed {
                        root,
                        parent: slot.then_some(parent),
                        received: received.name.to_string(),
                    }),
                    None => SeedProgress::Landed,
                },
            );
        }
        let fs = Self::slot_fs(datasets, pool, into).map(|fs| fs.name.clone());
        let bound = match birth {
            Some(birth) if slot => {
                let newest = match &fs {
                    Some(fs) => self.newest_snapshot(fs).await?.map(|newest| newest.name),
                    None => None,
                };
                let carried = self
                    .origin_of(Some(&parent), fs.as_deref().map(|fs| (fs, newest.as_ref())))
                    .await?;
                if carried.is_some_and(|carried| carried != birth)
                    || (carried.is_none() && fs.is_some())
                {
                    return Ok(SeedProgress::Taken);
                }
                carried.is_some()
            }
            Some(_) => false,
            None => true,
        };
        let pulling = if slot {
            self.marks(&parent).pull_target().await?
        } else {
            None
        };
        match self.receives.take_failure(into, pulling.as_ref()) {
            Err(Busy) if bound => return Ok(SeedProgress::Copying),
            Ok(Some(failure)) if bound => return Ok(SeedProgress::Failed(failure)),
            Err(Busy) | Ok(Some(_)) => return Ok(SeedProgress::Taken),
            Ok(None) => {}
        }
        if let Some(fs) = &fs {
            let newest = self.snapshots(fs).await?.into_iter().next();
            if let Some(newest) = newest.filter(|newest| newest.guid == guid) {
                return Ok(SeedProgress::Copied(Copied {
                    fs: fs.clone(),
                    parent,
                    root,
                    snapshot: newest.name.to_string(),
                }));
            }
        }
        Ok(if slot {
            SeedProgress::Stale(Stale { parent, fs })
        } else {
            SeedProgress::Absent
        })
    }

    /// The source's snapshot of `guid` and its origin, refused when `name` has no writer
    /// here, no longer holds the point, carries an origin this Machine did not stamp, or
    /// has no bound for the copy to take. It reads nothing it changes, so a refusal leaves
    /// any slot of `into` as it was.
    ///
    /// Under a Copy step it also refuses a point whose origin or time is not the one the
    /// step approved, and the copy records the step's birth in place of the point's origin.
    async fn seed_source(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        (request, name, into): (&SeedVolumeRequest, &VolumeName, &VolumeName),
        copy: Option<&CopyBinding>,
    ) -> Result<Source, RpcError> {
        let guid = request.guid;
        let root = DatasetLayout::of(pool).root(name);
        let Some(bound) = datasets
            .iter()
            .find(|dataset| dataset.name == root)
            .map(|dataset| dataset.refquota)
        else {
            return Err(SwitchError::Precondition
                .rpc_error(format!("Volume {name} has no writer on this Machine")));
        };
        let point = self
            .snapshots(&root)
            .await?
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
        let read = self
            .point(&root, point.name.as_str())
            .await
            .map_err(|error| {
                if let PluginError::Unstamped(message) = error {
                    SwitchError::Precondition.rpc_error(message)
                } else {
                    RpcError::from(error)
                }
            })?;
        if read.guid != guid {
            return Err(SwitchError::RestorePointGone.rpc_error(format!(
                "restore point {guid} is no longer on Volume {name}"
            )));
        }
        let Some(copy) = copy else {
            return Ok(Source {
                root,
                point,
                record: read.origin,
                bound,
            });
        };
        let carried = read
            .origin
            .unwrap_or_else(|| VolumeOrigin::legacy(&request.name));
        let taken = i64::try_from(copy.taken_at.as_millis()).ok();
        if carried != copy.source || point.created_unix_seconds.checked_mul(1000) != taken {
            return Err(SwitchError::Precondition.rpc_error(format!(
                "restore point {guid} of Volume {name} is not the point the copy approved"
            )));
        }
        Ok(Source {
            root,
            point,
            record: Some(copy.birth),
            bound,
        })
    }

    /// A new slot of `into` marked with the point it pulls, and the copy started into it.
    async fn copy_point(
        &self,
        held: &HeldMutation,
        pool: &MachinePool,
        into: &VolumeName,
        source: Source,
    ) -> Result<VolumeCopyView, RpcError> {
        let parent = DatasetLayout::of(pool).slot_parent(into);
        let fs = DatasetLayout::of(pool).slot_fs(into);
        let Source {
            root,
            point,
            record,
            bound,
        } = source;
        self.create_slot(&parent, bound).await?;
        self.marks(&parent).set_pull(point.name.as_str()).await?;
        self.write_record(&parent, record).await?;
        let storage = self.clone();
        let snapshot = format!("{root}@{}", point.name);
        // A receive still alive refuses the spawn, and the copy as it stands answers then too.
        let _ = self
            .receives
            .spawn(held, into, &point.name, |stop| async move {
                storage
                    .copy_snapshot(&snapshot, point.guid, &fs, bound, &stop)
                    .await
            });
        self.seeded(pool, into).await
    }

    /// The copy of `into` as the datasets stand now: a slot while it copies, a root once landed.
    async fn seeded(
        &self,
        pool: &MachinePool,
        into: &VolumeName,
    ) -> Result<VolumeCopyView, RpcError> {
        let copy = self.current_copy(pool, into).await?;
        Ok(VolumeCopyView { copy })
    }

    /// `zfs send` of one local snapshot piped into a bounded, read-only receive of `fs`,
    /// checked read-only at its bound once the receive has it all. The send's BEGIN record
    /// must build `guid` before the receive starts. It sets no `pulled` mark, so a
    /// departure keeps a seed that has not landed. Once `stop` fires, both are killed and
    /// reaped before this returns.
    async fn copy_snapshot(
        &self,
        snapshot: &str,
        guid: SnapshotGuid,
        fs: &str,
        bound: u64,
        stop: &Stop,
    ) -> Result<(), String> {
        let mut send = ZfsChild::spawn(
            Command::new(&self.zfs)
                .args(["send", snapshot])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
            stop,
        )
        .map_err(|error| format!("could not run zfs send: {error}"))?;
        let mut stream = send
            .child()
            .stdout
            .take()
            .expect("zfs send stdout is piped");
        let record = tokio::select! {
            biased;
            () = stop.cancelled() => None,
            record = begin::read(&mut stream, guid.get(), READ_BOUND) => Some(record),
        };
        let record = match record {
            Some(Ok(record)) => record,
            Some(Err(error)) => {
                send.reap().await;
                return Err(format!("send of {snapshot} refused: {error}"));
            }
            None => {
                send.reap().await;
                return Err(format!("the seed of {fs} was stopped"));
            }
        };
        let mut receive = self
            .spawn_receive(fs, bound, Stdio::piped(), stop)
            .map_err(|error| format!("could not run zfs receive: {error}"))?;
        let mut stdin = receive
            .child()
            .stdin
            .take()
            .expect("zfs receive stdin is piped");
        let exits = {
            let pipe = async move {
                stdin.write_all(&record).await?;
                tokio::io::copy(&mut stream, &mut stdin).await.map(drop)
            };
            let all = async { tokio::join!(pipe, exited(send.child()), exited(receive.child())) };
            tokio::select! {
                biased;
                () = stop.cancelled() => None,
                exits = all => Some(exits),
            }
        };
        send.reap().await;
        receive.reap().await;
        let Some((piped, sent, received)) = exits else {
            return Err(format!("the seed of {fs} was stopped"));
        };
        let (received, stderr) =
            received.map_err(|error| format!("zfs receive did not finish: {error}"))?;
        if !received.success() {
            return Err(format!(
                "zfs receive into {fs} failed: {}",
                super::transfer::without_resume_token(stderr.trim())
            ));
        }
        let (sent, stderr) = sent.map_err(|error| format!("zfs send did not finish: {error}"))?;
        if !sent.success() {
            return Err(format!("zfs send of {snapshot} failed: {}", stderr.trim()));
        }
        piped.map_err(|error| format!("the seed of {fs} broke: {error}"))?;
        self.finish_copy(fs, bound)
            .await
            .map_err(|error| error.to_string())
    }
}

pub(super) async fn seed_volume(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SeedVolumeRequest>,
) -> Json<Result<VolumeCopyView, RpcError>> {
    Json(storage.seed_volume(&request, None).await)
}

pub(super) async fn seed_copy(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SeedCopy>,
) -> Json<Result<VolumeCopyView, RpcError>> {
    Json(
        storage
            .seed_volume(&request.seed, Some(&request.copy))
            .await,
    )
}
