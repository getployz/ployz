//! Target switch effects: accepting the handed-over mirror, promoting it to the writer,
//! starting its Service Container, and restoring a copy as the writer.

use std::{fmt, str::FromStr};

use axum::{Json, extract::State};
use ployz_core::{
    Cycle, FenceDecision, HandOverRequest, Lease, LeaseRecord, MirrorMarker, MirrorRequest,
    RpcError, SnapshotGuid, SourceContainerRequest, SwitchError, SwitchReply, WriterMarker,
};
use ployzd::machine_pool::MachinePool;

use super::{
    Dataset, DockerVolumeName, VolumeStorage,
    lease::{Admitted, MIRROR_PROPERTY, PROMOTE_PROPERTY, WRITER_PROPERTY, internal, root_dataset},
    mirror::{Leased, name},
    switch_source::{HOLDER_PROPERTY, MountGrant, precondition},
};

/// `<lease>:<admitted unix seconds>` on the dataset a Promote or Start task works on. The
/// time is kept for the format; nothing decides by it.
const TASK_PROPERTY: &str = "ployz:task";
/// The guid of the handed-over final snapshot, kept on the promoted root.
const HANDOFF_PROPERTY: &str = "ployz:handoff";
const RESTORE_PREFIX: &str = "restore-";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Task {
    lease: Lease,
    admitted_unix_seconds: i64,
}

impl fmt::Display for Task {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.lease, self.admitted_unix_seconds)
    }
}

impl FromStr for Task {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid {TASK_PROPERTY} marker {value:?}");
        let (lease, admitted) = value.split_once(':').ok_or_else(invalid)?;
        Ok(Self {
            lease: Lease::new(lease.parse().map_err(|_| invalid())?),
            admitted_unix_seconds: admitted.parse().map_err(|_| invalid())?,
        })
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl VolumeStorage {
    async fn task(&self, dataset: &str) -> Result<Option<Task>, RpcError> {
        self.property(dataset, TASK_PROPERTY)
            .await
            .map_err(internal)?
            .map(|value| {
                value
                    .parse()
                    .map_err(|error: String| internal(error.into()))
            })
            .transpose()
    }

    /// Admits the task for `lease` on `dataset`; a crashed task of the same lease is kept.
    async fn admit_task(&self, dataset: &str, lease: Lease) -> Result<(), RpcError> {
        match self.task(dataset).await? {
            Some(task) if task.lease == lease => Ok(()),
            Some(_) | None => {
                let task = Task {
                    lease,
                    admitted_unix_seconds: now(),
                };
                self.zfs(&["set", &format!("{TASK_PROPERTY}={task}"), dataset])
                    .await
                    .map_err(internal)?;
                Ok(())
            }
        }
    }

    /// This Machine's record of `name` still names `lease`, and answers it.
    async fn require_recorded(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &DockerVolumeName,
        lease: Lease,
    ) -> Result<LeaseRecord, RpcError> {
        let record = self
            .lease_record(datasets, pool, name)
            .await
            .map_err(internal)?;
        match record {
            Some(record) if record.lease == lease => Ok(record),
            _ => Err(SwitchError::StaleLease.rpc_error(format!(
                "the record of Volume {name} no longer names lease {lease}"
            ))),
        }
    }

    /// The guard a task passes, under the lock, before its irreversible effect: its marker
    /// and this Machine's record still name `lease`, and the record's cycle is still open.
    /// Departure closes the record at the same lease, so a task it outlived stops here.
    async fn require_task(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        name: &DockerVolumeName,
        dataset: &str,
        lease: Lease,
    ) -> Result<(), RpcError> {
        if !self
            .task(dataset)
            .await?
            .is_some_and(|task| task.lease == lease)
        {
            return Err(precondition(format!(
                "{dataset} carries no task of lease {lease}"
            )));
        }
        let record = self.require_recorded(pool, datasets, name, lease).await?;
        if record.cycle != Cycle::Open {
            return Err(SwitchError::StaleStep.rpc_error(format!(
                "the record of Volume {name} closed at {record}; the task of lease {lease} on {dataset} stops"
            )));
        }
        Ok(())
    }

    fn open(scope: &mut Leased) {
        scope.admitted.lease.cycle = Cycle::Open;
    }

    async fn accept_hand_off(&self, request: &HandOverRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let slot = scope.require_slot(&name)?.name.clone();
        self.require_no_receive(&name)?;
        let newest = match scope.fs(&name) {
            Some(fs) => self
                .snapshots(&fs.name)
                .await
                .map_err(internal)?
                .first()
                .map(|snapshot| snapshot.guid),
            None => None,
        };
        if newest != Some(request.guid) {
            return Err(precondition(format!(
                "the mirror of Volume {name} does not hold the handed-over snapshot {}",
                request.guid
            )));
        }
        let marker = self
            .mirror_marker(scope.require_slot(&name)?)
            .await
            .map_err(internal)?;
        match marker {
            MirrorMarker::Idle => {}
            MirrorMarker::Final { guid } | MirrorMarker::HandedIn { guid }
                if guid == request.guid => {}
            MirrorMarker::Final { .. }
            | MirrorMarker::HandedIn { .. }
            | MirrorMarker::Promoting => {
                return Err(precondition(format!(
                    "the mirror of Volume {name} cannot accept the hand-off from {marker}"
                )));
            }
        }
        Self::open(&mut scope);
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("AcceptHandOff");
        self.zfs(&[
            "set",
            &format!(
                "{MIRROR_PROPERTY}={}",
                MirrorMarker::HandedIn { guid: request.guid }
            ),
            &slot,
        ])
        .await
        .map_err(internal)?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn clear_final(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let slot = scope.require_slot(&name)?;
        match self.mirror_marker(slot).await.map_err(internal)? {
            MirrorMarker::Idle => {}
            MirrorMarker::Final { .. } => {
                let slot = slot.name.clone();
                self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
                    .await?;
                ployzd::faults::kill_after_record("ClearFinal");
                self.zfs(&[
                    "set",
                    &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Idle),
                    &slot,
                ])
                .await
                .map_err(internal)?;
            }
            marker @ (MirrorMarker::HandedIn { .. } | MirrorMarker::Promoting) => {
                return Err(precondition(format!(
                    "the mirror of Volume {name} is {marker}; it is no longer a final copy"
                )));
            }
        }
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    async fn promote(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        let root = root_dataset(&scope.pool, &name);
        let rooted = scope.datasets.iter().any(|dataset| dataset.name == root);
        let target = match (scope.slot(&name), scope.fs(&name)) {
            (Some(slot), fs) => {
                let marker = self.mirror_marker(slot).await.map_err(internal)?;
                if !matches!(
                    marker,
                    MirrorMarker::HandedIn { .. } | MirrorMarker::Promoting
                ) {
                    return Err(precondition(format!(
                        "Promote requires a handed-in mirror of Volume {name}, not {marker}"
                    )));
                }
                match fs {
                    Some(fs) => fs.name.clone(),
                    None if rooted => root,
                    None => return Err(precondition(format!("the mirror of {name} has no copy"))),
                }
            }
            (None, _) if rooted => {
                if self.task(&root).await?.is_none() {
                    if scope.replayed() {
                        return self.reply(&scope.pool, &name, scope.admitted).await;
                    }
                    return Err(precondition(format!(
                        "Volume {name} has no handed-in mirror to promote"
                    )));
                }
                root
            }
            (None, _) => {
                return Err(precondition(format!(
                    "no mirror of Volume {name} is declared on this Machine"
                )));
            }
        };
        Self::open(&mut scope);
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("Promote");
        self.admit_task(&target, request.switch.lease).await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// Renames the handed-in copy over the root, unregistered, and makes it writable. Every
    /// step checks ZFS first, so a task restarted after a crash finishes what the last began.
    pub(super) async fn finish_promote(&self, request: &MirrorRequest) -> Result<(), RpcError> {
        let name = &name(&request.name)?;
        let lease = request.switch.lease;
        let _guard = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let root = root_dataset(&pool, name);
        let rooted = datasets.iter().any(|dataset| dataset.name == root);
        if rooted
            && Self::slot(&datasets, &pool, name).is_none()
            && self.task(&root).await?.is_none()
        {
            return Ok(());
        }
        let marker = match Self::slot(&datasets, &pool, name) {
            Some(slot) => Some(self.mirror_marker(slot).await.map_err(internal)?),
            None => None,
        };
        let slot = Self::slot(&datasets, &pool, name).map(|slot| slot.name.clone());
        let fs = Self::slot_fs(&datasets, &pool, name).map(|fs| fs.name.clone());
        let target = fs.clone().unwrap_or_else(|| root.clone());
        self.require_task(&pool, &datasets, name, &target, lease)
            .await?;
        if let (Some(slot), Some(fs)) = (&slot, &fs) {
            if let Some(MirrorMarker::HandedIn { guid }) = marker {
                self.zfs(&["set", &format!("{HANDOFF_PROPERTY}={guid}"), fs])
                    .await
                    .map_err(internal)?;
            }
            self.zfs(&[
                "set",
                &format!("{MIRROR_PROPERTY}={}", MirrorMarker::Promoting),
                slot,
            ])
            .await
            .map_err(internal)?;
            self.rename_unregistered(fs, &root, lease).await?;
        }
        if let Some(slot) = &slot {
            self.zfs(&["destroy", slot]).await.map_err(internal)?;
        }
        ployzd::faults::kill_inside("Promote");
        self.zfs(&["inherit", "mountpoint", &root])
            .await
            .map_err(internal)?;
        self.zfs(&["set", "readonly=off", &root])
            .await
            .map_err(internal)?;
        self.zfs(&["inherit", TASK_PROPERTY, &root])
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// Admits StartHandedContainer: a reply with a closed cycle means the Container already
    /// started; an open one carries the handed-over guid as the root's newest snapshot.
    async fn admit_handed_start(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        if scope.slot(&name).is_some() {
            return Err(precondition(format!(
                "the mirror of Volume {name} is not promoted yet"
            )));
        }
        let root = Self::dataset(&scope.datasets, &scope.pool, &name)
            .map_err(internal)?
            .ok_or_else(|| precondition(format!("Volume {name} has no promoted root")))?;
        root.require_provisioned(&name).map_err(internal)?;
        let root_name = root.name.clone();
        let task = self.task(&root_name).await?;
        if scope.replayed() && scope.admitted.lease.cycle == Cycle::Closed && task.is_none() {
            return self.reply(&scope.pool, &name, scope.admitted).await;
        }
        if root.readonly || self.writer_marker(root).await.map_err(internal)? != WriterMarker::Idle
        {
            return Err(precondition(format!(
                "Volume {name} is not a writable promoted root"
            )));
        }
        let handoff = self
            .property(&root_name, HANDOFF_PROPERTY)
            .await
            .map_err(internal)?
            .and_then(|guid| guid.parse().ok())
            .map(SnapshotGuid::new);
        let newest = self
            .snapshots(&root_name)
            .await
            .map_err(internal)?
            .first()
            .map(|snapshot| snapshot.guid);
        if handoff.is_none() || handoff != newest {
            return Err(precondition(format!(
                "Volume {name}'s newest snapshot is not the handed-over one"
            )));
        }
        Self::open(&mut scope);
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("StartHandedContainer");
        self.admit_task(&root_name, request.switch.lease).await?;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// Starts the created handed Container. Runs without the fence: the task's own guard
    /// decides, so a Start that outlived its lease or its open cycle stops before docker start.
    async fn start_handed_container(
        &self,
        request: &SourceContainerRequest,
    ) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let lease = request.switch.lease;
        let (held, _installation) = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let root = Self::dataset(&datasets, &pool, &name)
            .map_err(internal)?
            .ok_or_else(|| precondition(format!("Volume {name} has no promoted root")))?;
        let (root_name, mounted) = (root.name.clone(), root.mounted);
        self.require_task(&pool, &datasets, &name, &root_name, lease)
            .await?;
        let record = self
            .lease_record(&datasets, &pool, &name)
            .await
            .map_err(internal)?
            .ok_or_else(|| precondition(format!("Volume {name} has no record")))?;
        if self.holders(&name).await.map_err(internal)? != [request.container_id.as_str()] {
            return Err(precondition(format!(
                "Volume {name} must be held only by Container {}",
                request.container_id
            )));
        }
        if !mounted {
            self.zfs(&["mount", &root_name]).await.map_err(internal)?;
        }
        self.start_granted(
            &held,
            MountGrant {
                name: name.to_string(),
                record,
                container: request.container_id,
                mountpoint: name.mountpoint(),
            },
        )
        .await?;
        ployzd::faults::kill_inside("StartHandedContainer.started");
        self.zfs(&["inherit", TASK_PROPERTY, &root_name])
            .await
            .map_err(internal)?;
        self.reply(
            &pool,
            &name,
            Admitted {
                decision: FenceDecision::Replay,
                lease: LeaseRecord {
                    cycle: Cycle::Closed,
                    ..record
                },
                recorded: Some(record),
            },
        )
        .await
    }

    /// Makes this Machine's copy the writer: the slot's copy is renamed over the root, or the
    /// root is reopened, after one bounded `@restore-<unix>` snapshot keeps its data.
    async fn restore(&self, request: &MirrorRequest) -> Result<SwitchReply, RpcError> {
        let name = name(&request.name)?;
        let mut scope = self.leased(&name, &request.switch).await?;
        self.require_no_receive(&name)?;
        let root = root_dataset(&scope.pool, &name);
        let rooted = scope.datasets.iter().any(|dataset| dataset.name == root);
        let fs = scope.fs(&name).map(|fs| fs.name.clone());
        let slot = scope.slot(&name).map(|slot| slot.name.clone());
        if scope.replayed() && scope.admitted.lease.cycle == Cycle::Closed && slot.is_none() {
            return self.reply(&scope.pool, &name, scope.admitted).await;
        }
        let copy = match (&fs, rooted) {
            (_, true) => root.clone(),
            (Some(fs), false) => fs.clone(),
            (None, false) => {
                return Err(precondition(format!(
                    "Volume {name} has no copy on this Machine to restore"
                )));
            }
        };
        if rooted && fs.is_some() {
            return Err(precondition(format!(
                "Volume {name} has both a root and a mirror copy here"
            )));
        }
        Self::open(&mut scope);
        self.record(&scope.pool, &scope.datasets, &name, &mut scope.admitted)
            .await?;
        ployzd::faults::kill_after_record("Restore");
        self.restore_snapshot(&copy).await?;
        if let (Some(fs), false) = (&fs, rooted) {
            self.rename_unregistered(fs, &root, request.switch.lease)
                .await?;
        }
        if let Some(slot) = &slot {
            self.zfs(&["destroy", slot]).await.map_err(internal)?;
        }
        self.zfs(&["inherit", "mountpoint", &root])
            .await
            .map_err(internal)?;
        self.zfs(&["set", "readonly=off", &root])
            .await
            .map_err(internal)?;
        for property in [WRITER_PROPERTY, HOLDER_PROPERTY, TASK_PROPERTY] {
            self.zfs(&["inherit", property, &root])
                .await
                .map_err(internal)?;
        }
        scope.admitted.lease.cycle = Cycle::Closed;
        self.reply(&scope.pool, &name, scope.admitted).await
    }

    /// Renames a copy over the root marked unregistered: Docker sees it only once its Create
    /// registers it with the spec's labels.
    async fn rename_unregistered(
        &self,
        fs: &str,
        root: &str,
        lease: Lease,
    ) -> Result<(), RpcError> {
        self.zfs(&["set", &format!("{PROMOTE_PROPERTY}={lease}"), fs])
            .await
            .map_err(internal)?;
        self.zfs(&["rename", fs, root]).await.map_err(internal)?;
        Ok(())
    }

    /// Hides a root Docker recorded without its labels, so Docker forgets it and the next
    /// Create registers it again.
    async fn unregister(&self, request: &MirrorRequest) -> Result<(), RpcError> {
        let name = name(&request.name)?;
        let lease = request.switch.lease;
        let _guard = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        let root = Self::dataset(&datasets, &pool, &name)
            .map_err(internal)?
            .ok_or_else(|| precondition(format!("Volume {name} has no root here")))?;
        root.require_provisioned(&name).map_err(internal)?;
        self.require_recorded(&pool, &datasets, &name, lease)
            .await?;
        if !self.holders(&name).await.map_err(internal)?.is_empty() {
            return Err(precondition(format!(
                "Volume {name} is held by a Container; it cannot be registered again"
            )));
        }
        self.zfs(&["set", &format!("{PROMOTE_PROPERTY}={lease}"), &root.name])
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// Keeps one restore snapshot per copy: older `@restore-*` go before the new one.
    async fn restore_snapshot(&self, dataset: &str) -> Result<(), RpcError> {
        for snapshot in self.snapshot_names(dataset).await.map_err(internal)? {
            if snapshot
                .split_once('@')
                .is_some_and(|(_, name)| name.starts_with(RESTORE_PREFIX))
            {
                self.zfs(&["destroy", &snapshot]).await.map_err(internal)?;
            }
        }
        self.zfs(&["snapshot", &format!("{dataset}@{RESTORE_PREFIX}{}", now())])
            .await
            .map_err(internal)?;
        Ok(())
    }
}

pub(super) async fn accept_hand_off(
    State(storage): State<VolumeStorage>,
    Json(request): Json<HandOverRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.accept_hand_off(&request).await)
}

pub(super) async fn clear_final(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.clear_final(&request).await)
}

pub(super) async fn promote(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.promote(&request).await)
}

pub(super) async fn finish_promote(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<(), RpcError>> {
    Json(storage.finish_promote(&request).await)
}

pub(super) async fn unregister(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<(), RpcError>> {
    Json(storage.unregister(&request).await)
}

pub(super) async fn admit_handed_start(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    if let Err(error) =
        storage.refuse_while_running(&request.name.to_string(), &request.switch, "starting")
    {
        return Json(Err(error));
    }
    Json(storage.admit_handed_start(&request).await)
}

pub(super) async fn start_handed_container(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SourceContainerRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    let (name, switch) = (request.name.to_string(), request.switch);
    let step = storage.clone();
    let start = async move { step.start_handed_container(&request).await };
    Json(
        storage
            .container_step(&name, &switch, "starting", start)
            .await,
    )
}

pub(super) async fn restore(
    State(storage): State<VolumeStorage>,
    Json(request): Json<MirrorRequest>,
) -> Json<Result<SwitchReply, RpcError>> {
    Json(storage.restore(&request).await)
}
