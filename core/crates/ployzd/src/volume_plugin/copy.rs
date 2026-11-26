//! The ZFS half of SetVolumeCopy: one internal route per effect cell of the transition
//! table. The daemon owns the table and the Container half; each route here is
//! check-then-act over the datasets it finds, so the same request resent after a crash
//! at any sub-step converges. Every route answers the copy as it is afterwards.

use crate::{
    dataset_layout::DatasetLayout,
    faults::{Identity, Point, Want},
    machine_pool::MachinePool,
    volume_copy::{
        CopyRequest, CutRequest, DestroyRequest, ForgetRequest, PullRequest, SnapshotRequest,
        WriterRequest,
    },
    volume_send::protocol,
};
use axum::{Json, extract::State};
use ployz_core::{
    CopyPlan, CopyRefusal, CopyTransition, DockerVolumeName as WireName, RpcError, SendSource,
    SendStream, SetVolumeCopyRefused, SlotRole, SnapshotName, TrashPlace, VolumeCopy, VolumeGen,
    WantedCopy, error_chain::inline,
};

use super::{
    Dataset, PluginError, VolumeName, VolumeStorage,
    holders::{Unheld, copy_held_for_other_data, copy_unlisted},
    inspect::answering,
    marks::{Mark, Marks},
    pool::UnlabeledBacking,
    storage::Holds,
    transfer::{Busy, Proof, ReceiveFailure, Stop, pulling},
};

type Copy = Option<VolumeCopy>;

fn forgetting(volume: &WireName, r#gen: VolumeGen) -> Identity<'_> {
    Identity {
        volume,
        want: Want::Forget,
        r#gen,
    }
}
type Reply = Json<Result<Copy, RpcError>>;

pub(super) async fn inspect(
    State(storage): State<VolumeStorage>,
    Json(request): Json<CopyRequest>,
) -> Reply {
    Json(storage.copy_inspect(&request.name).await)
}

pub(super) async fn cut(
    State(storage): State<VolumeStorage>,
    Json(request): Json<CutRequest>,
) -> Reply {
    Json(storage.copy_cut(&request).await)
}

pub(super) async fn snapshot(
    State(storage): State<VolumeStorage>,
    Json(request): Json<SnapshotRequest>,
) -> Reply {
    Json(storage.copy_run_snapshot(&request).await)
}

pub(super) async fn forget(
    State(storage): State<VolumeStorage>,
    Json(request): Json<ForgetRequest>,
) -> Reply {
    Json(storage.copy_forget(&request).await)
}

pub(super) async fn writer(
    State(storage): State<VolumeStorage>,
    Json(request): Json<WriterRequest>,
) -> Reply {
    Json(storage.copy_writer(&request).await)
}

pub(super) async fn pull(
    State(storage): State<VolumeStorage>,
    Json(request): Json<PullRequest>,
) -> Reply {
    Json(storage.copy_pull(&request).await)
}

pub(super) async fn destroy(
    State(storage): State<VolumeStorage>,
    Json(request): Json<DestroyRequest>,
) -> Reply {
    Json(storage.copy_destroy(&request).await)
}

/// The plugin's mutation and the daemon's installation, held before the route looks
/// for the Pool.
struct Admitted {
    held: super::storage::HeldMutation,
    installation: crate::mutation::MutationGuard,
    name: VolumeName,
}

/// What a route sees once it holds the plugin's mutation and the daemon's installation.
struct Scope {
    held: super::storage::HeldMutation,
    _installation: crate::mutation::MutationGuard,
    pool: MachinePool,
    datasets: Vec<Dataset>,
    name: VolumeName,
}

impl Scope {
    fn root(&self) -> Result<Option<&Dataset>, RpcError> {
        VolumeStorage::dataset(&self.datasets, &self.pool, &self.name).map_err(RpcError::from)
    }

    fn slot(&self) -> Option<&Dataset> {
        VolumeStorage::slot(&self.datasets, &self.pool, &self.name)
    }

    fn fs(&self) -> Option<&Dataset> {
        VolumeStorage::slot_fs(&self.datasets, &self.pool, &self.name)
    }
}

fn refuse(reason: CopyRefusal, copy: Copy, message: String) -> RpcError {
    SetVolumeCopyRefused {
        reason,
        copy,
        floor: None,
        holder: None,
    }
    .rpc_error(message)
}

fn no_pool() -> RpcError {
    RpcError::internal("no usable existing Machine Pool")
}

fn no_room(bound: u64, name: &VolumeName, error: &PluginError) -> RpcError {
    refuse(
        CopyRefusal::NoCapacity,
        None,
        format!(
            "this Machine has no room for a {bound} byte copy of {name}{}",
            error.trash_note()
        ),
    )
}

impl VolumeStorage {
    async fn admit(&self, wire: &WireName) -> Result<Admitted, RpcError> {
        let name = VolumeName::try_from(wire)?;
        let (held, installation) = self.admit_mutation().await?;
        Ok(Admitted {
            held,
            installation,
            name,
        })
    }

    async fn enter(&self, admitted: Admitted, pool: MachinePool) -> Result<Scope, RpcError> {
        let datasets = self.datasets(&pool).await?;
        Ok(Scope {
            held: admitted.held,
            _installation: admitted.installation,
            pool,
            datasets,
            name: admitted.name,
        })
    }

    async fn scope(&self, wire: &WireName) -> Result<Option<Scope>, RpcError> {
        let admitted = self.admit(wire).await?;
        match self.pool.one_usable().await? {
            Some(pool) => self.enter(admitted, pool).await.map(Some),
            None => Ok(None),
        }
    }

    async fn first_pool(&self, bound: u64, name: &VolumeName) -> Result<MachinePool, RpcError> {
        let _pool_guard = self.pool.lock_mutation().await?;
        if let Some(pool) = self.pool.one_usable().await? {
            return Ok(pool);
        }
        if let Some(pool) = self.pool.recover(UnlabeledBacking::Remove).await? {
            return Ok(pool);
        }
        match self.pool.create(bound).await {
            Ok(created) => {
                self.trash_new_pool(created.machine_pool()).await;
                self.one_pool().await.map_err(RpcError::from)
            }
            Err(error @ PluginError::Capacity(..)) => Err(no_room(bound, name, &error)),
            Err(error) => Err(RpcError::from(error)),
        }
    }

    async fn z(&self, args: &[&str]) -> Result<(), RpcError> {
        self.zfs(args).await.map(drop).map_err(RpcError::from)
    }

    /// The copy as the datasets stand now.
    async fn copy_now(&self, scope: &Scope) -> Result<Copy, RpcError> {
        self.current_copy(&scope.pool, &scope.name)
            .await
            .map_err(RpcError::from)
    }

    async fn receiving_another(&self, scope: &Scope) -> RpcError {
        let name = &scope.name;
        self.refuse_found(
            scope,
            CopyRefusal::Busy,
            format!("Volume {name} is still receiving another pull"),
        )
        .await
    }

    async fn refuse_found(&self, scope: &Scope, reason: CopyRefusal, message: String) -> RpcError {
        match self.copy_now(scope).await {
            Ok(copy) => refuse(reason, copy, message),
            Err(error) => error,
        }
    }

    async fn written_since(
        &self,
        dataset: &str,
        snapshot: &SnapshotName,
    ) -> Result<bool, RpcError> {
        let written = self
            .zfs(&[
                "get",
                "-Hp",
                "-o",
                "value",
                &format!("written@{snapshot}"),
                dataset,
            ])
            .await?;
        Ok(written.trim() != "0")
    }

    async fn copy_inspect(&self, name: &WireName) -> Result<Copy, RpcError> {
        let Some(scope) = self.scope(name).await? else {
            return Ok(None);
        };
        self.copy_now(&scope).await
    }

    /// Writer to Mirror. The daemon stopped the Container and holds its admission turn;
    /// this moves the root into the slot at `target`.
    async fn copy_cut(&self, request: &CutRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?.ok_or_else(no_pool)?;
        let name = &scope.name;
        let Some(root) = scope.root()? else {
            let copy = self.copy_now(&scope).await?;
            return match &copy {
                Some(VolumeCopy::Slot {
                    newest: Some(newest),
                    ..
                }) if newest.name == request.target => Ok(copy),
                _ => Err(refuse(
                    CopyRefusal::NoTransition,
                    copy,
                    format!("Volume {name} has no writer root on this Machine to cut"),
                )),
            };
        };
        let (root_name, readonly, mounted) = (root.name.clone(), root.readonly, root.mounted);
        let taken = self
            .snapshots(&root_name)
            .await?
            .iter()
            .any(|snapshot| snapshot.name == request.target);
        if taken && self.written_since(&root_name, &request.target).await? {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::Written,
                    format!(
                        "Volume {name} was written after snapshot {}; cut at a newer one",
                        request.target
                    ),
                )
                .await);
        }
        if !readonly {
            self.z(&["set", "readonly=on", &root_name]).await?;
        }
        // Docker on this Machine keeps its record for the Volume; plugin Get answers
        // from the slot while the root is away.
        self.marks(&root_name).set_cut().await?;
        if !taken {
            self.z(&["snapshot", &format!("{root_name}@{}", request.target)])
                .await?;
        }
        crate::faults::kill_inside(
            Point::Copy(CopyTransition::WriterToMirror),
            Some(Identity {
                volume: &request.name,
                want: Want::Mirror {
                    target: &request.target,
                },
                r#gen: request.r#gen,
            }),
        );
        if mounted {
            self.z(&["unmount", &root_name]).await?;
        }
        self.park_root_in_slot(&scope.held, &scope.pool, &scope.datasets, name, root)
            .await?;
        self.copy_now(&scope).await
    }

    /// Writer to Snapshot: take `snapshot` when anything was written since the newest
    /// run snapshot, then keep the newest two.
    async fn copy_run_snapshot(&self, request: &SnapshotRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?.ok_or_else(no_pool)?;
        let root = self.require_root(&scope).await?;
        let snapshots = self.snapshots(&root).await?;
        if !snapshots
            .iter()
            .any(|snapshot| snapshot.name == request.snapshot)
        {
            let take = match snapshots.first() {
                None => true,
                Some(newest) => self.written_since(&root, &newest.name).await?,
            };
            if take {
                self.z(&["snapshot", &format!("{root}@{}", request.snapshot)])
                    .await?;
            }
        }
        crate::faults::kill_inside(
            Point::Copy(CopyTransition::WriterToSnapshot),
            Some(Identity {
                volume: &request.name,
                want: Want::Snapshot {
                    snapshot: &request.snapshot,
                },
                r#gen: request.r#gen,
            }),
        );
        let snapshots = self.snapshots(&root).await?;
        self.destroy_snapshots(&root, snapshots.get(2..).unwrap_or_default())
            .await?;
        self.copy_now(&scope).await
    }

    /// Writer to Forget: every run snapshot dropped from the root.
    async fn copy_forget(&self, request: &ForgetRequest) -> Result<Copy, RpcError> {
        let name = &request.name;
        let Some(scope) = self.scope(name).await? else {
            return Ok(None);
        };
        let root = self.require_root(&scope).await?;
        let snapshots = self.snapshots(&root).await?;
        for (index, snapshot) in snapshots.iter().enumerate() {
            self.z(&["destroy", &format!("{root}@{}", snapshot.name)])
                .await?;
            if index == 0 {
                crate::faults::kill_inside(
                    Point::Copy(CopyTransition::WriterToForget),
                    Some(forgetting(name, request.r#gen)),
                );
            }
        }
        self.copy_now(&scope).await
    }

    async fn require_root(&self, scope: &Scope) -> Result<String, RpcError> {
        match scope.root()? {
            Some(root) => Ok(root.name.clone()),
            None => Err(self
                .refuse_found(
                    scope,
                    CopyRefusal::NoTransition,
                    format!("Volume {} has no writer root on this Machine", scope.name),
                )
                .await),
        }
    }

    /// Writer at `at` numbered `epoch`. A root takes the higher epoch and becomes writable
    /// again; a slot whose newest snapshot is `at` is promoted to the root first.
    ///
    /// A promoted slot drops every mark that says another step made its copy, before the
    /// rename, and takes a writer's:
    /// - `ployz:pulled` goes, so the departure stops taking it for a received copy from a
    ///   crash on ([`Self::written_here`]).
    /// - `ployz:seed` goes whenever the slot carries it. A departure parks a seed's root
    ///   with the mark still on it, whether Docker adopted it or not, since Docker's
    ///   Create inherits only `ployz:promote`. Beside the `ployz:promote` set below, it
    ///   would read as a seed Docker never adopted ([`RootCopy::parse`]), which Forget
    ///   destroys. So `land` is the one step that leaves both on a root.
    /// - `ployz:image` becomes the image the start placed before this promote, or goes,
    ///   then `ployz:epoch` becomes `epoch`. A root already at `epoch` takes the image
    ///   again, so a resend's record is the one its start reads back.
    /// - `ployz:promote` is set unless the slot was cut. A pulled copy has no Docker record
    ///   yet, so the root hides from Get and List until Docker's own Create registers it
    ///   with labels. A cut copy keeps its record, and hiding it would make Docker drop
    ///   that record.
    /// - `ployz:cut` goes after the rename, in [`Self::make_writable`].
    ///
    /// [`RootCopy::parse`]: ployz_decide::volume_root::RootCopy::parse
    async fn copy_writer(&self, request: &WriterRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?.ok_or_else(no_pool)?;
        let name = &scope.name;
        let root_name = DatasetLayout::of(&scope.pool).root(name);
        if let Some(root) = scope.root()? {
            let (readonly, mountpoint) = (root.readonly, root.mountpoint.clone());
            let epoch = self.marks(&root_name).epoch().await?;
            if epoch > request.epoch {
                return Err(self
                    .refuse_found(
                        &scope,
                        CopyRefusal::NoTransition,
                        format!(
                            "Volume {name} is writer epoch {epoch}, past {}",
                            request.epoch
                        ),
                    )
                    .await);
            }
            self.require_at(&scope, &root_name, request).await?;
            let marks = self.marks(&root_name);
            marks.record_image(request.image.as_ref()).await?;
            if epoch < request.epoch {
                marks.set_epoch(request.epoch).await?;
            }
            if scope.slot().is_some() && scope.fs().is_none() {
                let parent = DatasetLayout::of(&scope.pool).slot_parent(name);
                self.destroy(&parent, Holds::Nothing).await?;
            }
            self.make_writable(&root_name, &mountpoint, readonly, name)
                .await?;
            return self.copy_now(&scope).await;
        }
        let Some(fs) = scope.fs() else {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::NoTransition,
                    format!("Volume {name} has no copy on this Machine to make the writer"),
                )
                .await);
        };
        let fs_name = fs.name.clone();
        self.require_at(&scope, &fs_name, request).await?;
        if self.receives.status(name).busy() {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::Busy,
                    format!("Volume {name} is still receiving into its slot"),
                )
                .await);
        }
        let marks = self.marks(&fs_name);
        if marks.is_set(Mark::ResumeToken).await? {
            self.z(&["receive", "-A", &fs_name]).await?;
        }
        let parent = DatasetLayout::of(&scope.pool).slot_parent(name);
        // No copy leaves its slot while the slot still holds a record its bytes lack.
        self.complete(&parent, &fs_name).await?;
        let newest = self.newest_snapshot(&fs_name).await?;
        let carried = self
            .origin_of(
                Some(&parent),
                Some((&fs_name, newest.as_ref().map(|newest| &newest.name))),
            )
            .await?;
        match self.admit_holders(&request.name, carried).await {
            Ok(()) => {}
            Err(Unheld::Foreign) => {
                return Err(self
                    .refuse_found(
                        &scope,
                        CopyRefusal::Foreign,
                        copy_held_for_other_data(&request.name),
                    )
                    .await);
            }
            Err(Unheld::Unlisted(cause)) => {
                return Err(self
                    .refuse_found(
                        &scope,
                        CopyRefusal::DockerUnavailable,
                        copy_unlisted(&request.name, &cause),
                    )
                    .await);
            }
        }
        marks
            .promote_slot(request.epoch, request.image.as_ref())
            .await?;
        // A Machine that only ever mirrored has no managed parent for the root yet.
        let parent = DatasetLayout::of(&scope.pool).root_parent();
        self.ensure_root(&scope.datasets, &parent).await?;
        self.z(&["rename", &fs_name, &root_name]).await?;
        let parent = DatasetLayout::of(&scope.pool).slot_parent(name);
        self.destroy(&parent, Holds::Nothing).await?;
        self.make_writable(&root_name, "", true, name).await?;
        self.copy_now(&scope).await
    }

    async fn require_at(
        &self,
        scope: &Scope,
        dataset: &str,
        request: &WriterRequest,
    ) -> Result<(), RpcError> {
        let newest = self.snapshots(dataset).await?.into_iter().next();
        if newest.is_some_and(|newest| newest.guid == request.at) {
            return Ok(());
        }
        Err(self
            .refuse_found(
                scope,
                CopyRefusal::Behind,
                format!(
                    "Volume {} is not at snapshot {} on this Machine",
                    scope.name, request.at
                ),
            )
            .await)
    }

    async fn make_writable(
        &self,
        root: &str,
        mountpoint: &str,
        readonly: bool,
        name: &VolumeName,
    ) -> Result<(), RpcError> {
        if mountpoint != name.mountpoint() {
            self.z(&["inherit", "mountpoint", root]).await?;
        }
        if readonly {
            self.z(&["set", "readonly=off", root]).await?;
        }
        self.marks(root).clear_cut().await?;
        Ok(())
    }

    /// Mirror to Mirror and None to Mirror: the slot pulls `target` from `from`. A
    /// receive in flight is left alone; a landed target is bounded and pruned; anything
    /// else starts or resumes the receive.
    async fn copy_pull(&self, request: &PullRequest) -> Result<Copy, RpcError> {
        let admitted = self.admit(&request.name).await?;
        let (scope, fetched) = match self.pool.one_usable().await? {
            Some(pool) => (self.enter(admitted, pool).await?, None),
            None => {
                let bound = self.fetch_bound(request).await?;
                let pool = self.first_pool(bound, &admitted.name).await?;
                (self.enter(admitted, pool).await?, Some(bound))
            }
        };
        let name = &scope.name;
        if scope.root()?.is_some() {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::NoTransition,
                    format!("Volume {name} is the writer on this Machine; it does not pull"),
                )
                .await);
        }
        let parent = DatasetLayout::of(&scope.pool).slot_parent(name);
        let target = request.target.to_string();
        let pulling = match scope.slot() {
            Some(_) => self.marks(&parent).get(Mark::Pull).await?,
            None => None,
        };
        if pulling.as_deref() == Some(target.as_str()) {
            match self.receives.take_failure(name, Some(&request.target)) {
                Err(Busy) => return self.copy_now(&scope).await,
                // A resend after the receive failed answers that failure instead of hiding
                // it under a fresh receive. The answer clears it, so the next want for this
                // target retries, and a want for another target replaces it.
                Ok(Some(failure)) => {
                    return self
                        .copy_now(&scope)
                        .await
                        .map(|copy| answering(copy, failure));
                }
                Ok(None) => {}
            }
        } else if self.receives.status(name).busy() {
            return Err(self.receiving_another(&scope).await);
        }
        let (fresh, bound) = match scope.slot() {
            Some(slot) => (false, slot.refquota),
            None => {
                let bound = match fetched {
                    Some(bound) => bound,
                    None => self.fetch_bound(request).await?,
                };
                if let Err(error) = self
                    .ensure_commitment(&scope.pool, &scope.datasets, bound)
                    .await
                {
                    return Err(if error.is_capacity() {
                        no_room(bound, name, &error)
                    } else {
                        RpcError::from(error)
                    });
                }
                self.ensure_mirror_root(&scope.pool, &scope.datasets)
                    .await?;
                self.create_slot(&parent, bound).await?;
                crate::faults::kill_inside(
                    Point::Copy(CopyTransition::NoneToMirror),
                    Some(Identity {
                        volume: &request.name,
                        want: Want::Mirror {
                            target: &request.target,
                        },
                        r#gen: request.r#gen,
                    }),
                );
                (true, bound)
            }
        };
        if bound == 0 {
            return Err(RpcError::internal(format!(
                "slot {parent} carries no bound"
            )));
        }
        let fs = DatasetLayout::of(&scope.pool).slot_fs(name);
        let newest = match scope.fs() {
            Some(_) => self.snapshots(&fs).await?.into_iter().next(),
            None => None,
        };
        if let Some(newest) = &newest
            && newest.name == request.target
        {
            if pulling.as_deref() == Some(target.as_str())
                && self.written_here(&fs).await?
                && let Some(failure) = self
                    .mark_proven_pull(request, (&parent, &fs), newest.guid)
                    .await?
            {
                // Nothing proved or refuted the landing, so `ployz:pull` stays for the next
                // resend to prove.
                return self
                    .copy_now(&scope)
                    .await
                    .map(|copy| answering(copy, failure));
            }
            // An old copy already holding the target receives nothing, and is the mirror now.
            if self.marks(&parent).role().await? != SlotRole::Mirror {
                self.marks(&parent).set_role(SlotRole::Mirror).await?;
            }
            self.finish_copy(&fs, bound).await?;
            let snapshots = self.snapshots(&fs).await?;
            self.destroy_snapshots(&fs, snapshots.get(1..).unwrap_or_default())
                .await?;
            self.complete(&parent, &fs).await?;
            if pulling.is_some() {
                self.marks(&parent).clear_pull().await?;
            }
            self.receives.forget(name);
            return self.copy_now(&scope).await;
        }
        let retarget = pulling.as_deref() != Some(target.as_str());
        let bound = if fresh {
            bound
        } else {
            self.raise_slot_bound(&scope, &parent, bound, request)
                .await?
        };
        let token = match scope.fs() {
            Some(_) => self.marks(&fs).get(Mark::ResumeToken).await?,
            None => None,
        };
        let source = match (token, newest) {
            (Some(token), _) => SendSource::Resume {
                token,
                target: request.target.clone(),
            },
            (None, Some(base)) => SendSource::Incremental {
                base: base.guid,
                target: request.target.clone(),
            },
            (None, None) => SendSource::Full {
                target: request.target.clone(),
            },
        };
        let stream = SendStream {
            name: request.name.clone(),
            source,
        };
        // Set last before the spawn, so a step above that fails leaves `ployz:pull` naming
        // the target it named before.
        if retarget {
            self.marks(&parent).set_pull(&target).await?;
        }
        let storage = self.clone();
        let (from, r#gen, pulled) = (request.from, request.r#gen, name.clone());
        let spawned = self
            .receives
            .spawn(&scope.held, name, &request.target, |stop| async move {
                let result = match storage
                    .pull_stream(from, stream, (&parent, &fs, &pulled), bound, r#gen, &stop)
                    .await
                {
                    Ok(()) => match storage.mark_pulled(&parent, &fs).await {
                        Ok(()) => storage
                            .complete(&parent, &fs)
                            .await
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                };
                if let Err(error) = &result {
                    tracing::warn!(%fs, %error, "Volume pull failed");
                }
                result
            });
        if spawned.is_err() {
            return Err(self.receiving_another(&scope).await);
        }
        self.copy_now(&scope).await
    }

    /// One receive of `stream`. When the writer no longer has the base or the token the
    /// stream names, the slot's copy starts over from a full send. The copy is dropped
    /// only once the writer answers the full send, so a writer without the target leaves
    /// the slot's newest snapshot in place. A copy this Machine may alone hold
    /// ([`Self::written_here`]), such as one a departure parked, is never dropped: the
    /// pull fails and the copy stays.
    async fn pull_stream(
        &self,
        from: ployz_core::ManagementAddress,
        stream: SendStream,
        (parent, fs, name): (&str, &str, &VolumeName),
        bound: u64,
        r#gen: VolumeGen,
        stop: &Stop,
    ) -> Result<(), String> {
        match self
            .receive(from, &stream, (parent, fs), bound, r#gen, stop)
            .await
        {
            Ok(()) => return Ok(()),
            Err(failure @ (ReceiveFailure::Failed(_) | ReceiveFailure::Storage(_))) => {
                return Err(failure.to_string());
            }
            Err(ReceiveFailure::NoStream(_)) => {}
        }
        let (target, resumed) = match &stream.source {
            SendSource::Full { .. } => {
                return Err(format!(
                    "{} has no snapshot {} to send",
                    from.0, stream.name
                ));
            }
            SendSource::Resume { target, .. } => (target.clone(), true),
            SendSource::Incremental { target, .. } => (target.clone(), false),
        };
        if self
            .written_here(fs)
            .await
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "this Machine's copy of {} may hold writes {} doesn't have, so the pull keeps it rather than replace it with a full copy; delete it with `ployz volume mirror rm` to take a fresh one",
                stream.name, from.0
            ));
        }
        let full = SendStream {
            name: stream.name,
            source: SendSource::Full { target },
        };
        let opened = self
            .open_stream(from, &full, stop)
            .await
            .map_err(|failure| failure.to_string())?;
        // The full stream replaces this slot's copy only once its origin matches it.
        self.admit_origin(parent, fs, &opened)
            .await
            .map_err(|failure| failure.to_string())?;
        if resumed {
            self.zfs(&["receive", "-A", fs])
                .await
                .map_err(|error| error.to_string())?;
        }
        // The old copy moves to the Trash; an interrupted first receive goes outright.
        if self.exists(fs).await.map_err(|error| error.to_string())? {
            self.discard_copy(parent, fs, name, Some(stop))
                .await
                .map_err(|error| error.to_string())?;
        }
        self.receive_opened(opened, fs, bound, pulling(&full, r#gen), stop)
            .await
            .map_err(|failure| failure.to_string())
    }

    /// A resend of a pull whose target landed, on a slot not marked pulled: a daemon that
    /// stopped between the receive and [`Self::mark_pulled`]. A slot a released version
    /// wrote can carry the same marks with no receive, so the writer must send the target
    /// as the slot's newest guid before the slot is marked. A writer that says it can't
    /// leaves the slot as it is, kept by a departure. A writer that gives no answer, such
    /// as one that can't be reached, leaves why, and the slot unmarked.
    async fn mark_proven_pull(
        &self,
        request: &PullRequest,
        (parent, fs): (&str, &str),
        guid: ployz_core::SnapshotGuid,
    ) -> Result<Option<String>, RpcError> {
        let stream = SendStream {
            name: request.name.clone(),
            source: SendSource::Full {
                target: request.target.clone(),
            },
        };
        match self.sends_as(request.from, &stream, guid).await {
            Ok(Proof::Sent) => {
                self.mark_pulled(parent, fs)
                    .await
                    .map_err(RpcError::internal)?;
                Ok(None)
            }
            Ok(Proof::Refuted(failure)) => {
                tracing::warn!(%fs, %failure, "left a landed pull unmarked: its writer did not send its snapshot");
                Ok(None)
            }
            Err(failure) => Ok(Some(format!(
                "could not confirm with its writer that {} landed: {failure}",
                request.target
            ))),
        }
    }

    /// Marks the slot received from a writer elsewhere, and that writer's Mirror. A
    /// receive that exits 0 made the stream's snapshot, and a pull of the slot's own newest
    /// never starts one, so the newest snapshot is the one a writer elsewhere sent, with
    /// that writer's guid. Any cut, restore, promote or seed mark describes a point that
    /// writer has since built on, so each goes first: a crash between leaves a slot with
    /// neither, which the departure keeps. The role goes last, so an old copy a refused
    /// pull leaves stays an old copy.
    async fn mark_pulled(&self, parent: &str, fs: &str) -> Result<(), String> {
        let failed = |error: super::PluginError| error.to_string();
        let Some(newest) = self.newest_guid(fs).await.map_err(failed)? else {
            return Ok(());
        };
        self.marks(fs)
            .apply(&Marks::received(newest))
            .await
            .map_err(failed)?;
        if self.marks(parent).role().await.map_err(failed)? != SlotRole::Mirror {
            self.marks(parent)
                .set_role(SlotRole::Mirror)
                .await
                .map_err(failed)?;
        }
        Ok(())
    }

    /// The slot's bound raised to the writer's, when the writer's Volume grew since the
    /// slot was made, so the receive fits whatever the writer holds now.
    async fn raise_slot_bound(
        &self,
        scope: &Scope,
        parent: &str,
        bound: u64,
        request: &PullRequest,
    ) -> Result<u64, RpcError> {
        let writer = self.fetch_bound(request).await?;
        if writer <= bound {
            return Ok(bound);
        }
        match self
            .grow_bound(&scope.pool, &scope.datasets, parent, bound, writer)
            .await
        {
            Ok(()) => Ok(writer),
            Err(error @ PluginError::Capacity(..)) => Err(self
                .refuse_found(
                    scope,
                    CopyRefusal::NoCapacity,
                    format!(
                        "this Machine has no room for a {writer} byte copy of {}{}",
                        scope.name,
                        error.trash_note()
                    ),
                )
                .await),
            Err(error) => Err(RpcError::from(error)),
        }
    }

    /// The writer's bound for the Volume, from its send port.
    async fn fetch_bound(&self, request: &PullRequest) -> Result<u64, RpcError> {
        let url = protocol::bound_url(request.from, self.send_port, &request.name);
        let client = reqwest::Client::builder()
            .connect_timeout(self.receive_stall)
            .timeout(self.receive_stall)
            .build()
            .map_err(|error| {
                RpcError::internal(format!(
                    "could not build the bound client: {}",
                    inline(&error)
                ))
            })?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| RpcError::internal(format!("GET {url}: {}", inline(&error))))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            let reason = response.text().await.unwrap_or_default();
            return Err(refuse(
                CopyRefusal::NoSource,
                None,
                format!(
                    "Volume {} has no copy on {}: {reason}",
                    request.name, request.from.0
                ),
            ));
        }
        let body = response
            .error_for_status()
            .map_err(|error| RpcError::internal(format!("GET {url}: {}", inline(&error))))?
            .text()
            .await
            .map_err(|error| RpcError::internal(format!("GET {url}: {}", inline(&error))))?;
        protocol::parse_bound(&body)
            .map_err(|error| RpcError::internal(format!("GET {url} answered {body:?}: {error}")))
    }

    /// The slot, or an unadopted seed's root, destroyed, once the Forget the daemon planned
    /// still deletes the copy here. No receive changes the slot past the busy check: its task
    /// has ended, and a new one starts only under the mutation this route holds.
    async fn copy_destroy(&self, request: &DestroyRequest) -> Result<Copy, RpcError> {
        let wire = &request.name;
        let Some(scope) = self.scope(wire).await? else {
            return Ok(None);
        };
        let name = &scope.name;
        let receiving = self.receives.status(name).busy();
        if scope.root()?.is_none() && scope.slot().is_some() && receiving {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::Busy,
                    format!("Volume {name} is still receiving into its slot"),
                )
                .await);
        }
        if let Some(expect) = &request.expect {
            let copy = self.copy_now(&scope).await?;
            let want = WantedCopy::Forget {
                discard: request.discard,
            };
            match ployz_core::plan(copy.as_ref(), false, expect, &want) {
                CopyPlan::Transition(
                    CopyTransition::MirrorToForget | CopyTransition::SeedToForget,
                ) => {}
                CopyPlan::AtWant => return Ok(copy),
                CopyPlan::Refuse(reason) => {
                    let message = format!(
                        "Volume {name} changed after this Machine planned its delete, so it was kept"
                    );
                    return Err(refuse(reason, copy, message));
                }
                CopyPlan::Transition(_) => {
                    let message = format!("Volume {name} has no copy here to delete");
                    return Err(refuse(CopyRefusal::NoTransition, copy, message));
                }
            }
        }
        if let Some(root) = scope.root()? {
            if self.marks(&root.name).unadopted_seed().await? {
                crate::faults::kill_inside(
                    Point::Copy(CopyTransition::SeedToForget),
                    Some(forgetting(wire, request.r#gen)),
                );
                self.discard(&root.name, TrashPlace::Root, name).await?;
                // A slot beside the seed is the parent its landing emptied; one holding a
                // copy that may be the only one stays.
                let kept = match scope.fs() {
                    Some(fs) => self.written_here(&fs.name).await?,
                    None => false,
                };
                if scope.slot().is_some() && !receiving && !kept {
                    self.discard_slot(&DatasetLayout::of(&scope.pool).slot_parent(name), name)
                        .await?;
                    self.receives.forget(name);
                }
                return self.copy_now(&scope).await;
            }
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::NoTransition,
                    format!(
                        "Volume {name} is the writer on this Machine; forget its snapshots instead"
                    ),
                )
                .await);
        }
        if scope.slot().is_none() {
            return Ok(None);
        }
        crate::faults::kill_inside(
            Point::Copy(CopyTransition::MirrorToForget),
            Some(forgetting(wire, request.r#gen)),
        );
        self.discard_slot(&DatasetLayout::of(&scope.pool).slot_parent(name), name)
            .await?;
        self.receives.forget(name);
        self.copy_now(&scope).await
    }
}
