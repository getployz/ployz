//! The ZFS half of SetVolumeCopy: one internal route per effect cell of the transition
//! table. The daemon owns the table and the Container half; each route here is
//! check-then-act over the datasets it finds, so the same request resent after a crash
//! at any sub-step converges. Every route answers the copy as it is afterwards.

use axum::{Json, extract::State};
use ployz_core::{
    CopyRefusal, DockerVolumeName as WireName, RpcError, SendSource, SendStream,
    SetVolumeCopyRefused, SnapshotName, VolumeCopy,
};
use ployzd::{
    machine_pool::MachinePool,
    volume_copy::{
        BOUND_PATH_PREFIX, CopyRequest, CutRequest, PullRequest, SnapshotRequest, WriterRequest,
    },
};

use super::{
    Dataset, DockerVolumeName, VolumeError, VolumeStorage,
    lease::internal,
    mirror,
    storage::{
        CUT_PROPERTY, EPOCH_PROPERTY, PROMOTE_PROPERTY, PULL_PROPERTY, RESUME_TOKEN_PROPERTY,
        slot_fs, slot_parent,
    },
    transfer::ReceiveFailure,
};

type Copy = Option<VolumeCopy>;
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
    Json(request): Json<CopyRequest>,
) -> Reply {
    Json(storage.copy_forget(&request.name).await)
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
    Json(request): Json<CopyRequest>,
) -> Reply {
    Json(storage.copy_destroy(&request.name).await)
}

/// What a route sees once it holds the plugin's mutation and the daemon's installation.
struct Scope {
    _held: super::storage::HeldMutation,
    _installation: ployzd::mutation::MutationGuard,
    pool: MachinePool,
    datasets: Vec<Dataset>,
    name: DockerVolumeName,
}

impl Scope {
    fn root(&self) -> Result<Option<&Dataset>, RpcError> {
        VolumeStorage::dataset(&self.datasets, &self.pool, &self.name).map_err(internal)
    }

    fn slot(&self) -> Option<&Dataset> {
        VolumeStorage::slot(&self.datasets, &self.pool, &self.name)
    }

    fn fs(&self) -> Option<&Dataset> {
        VolumeStorage::slot_fs(&self.datasets, &self.pool, &self.name)
    }
}

fn refuse(reason: CopyRefusal, copy: Copy, message: String) -> RpcError {
    SetVolumeCopyRefused { reason, copy }.rpc_error(message)
}

impl VolumeStorage {
    async fn scope(&self, wire: &WireName) -> Result<Scope, RpcError> {
        let name = mirror::name(wire)?;
        let (held, installation) = self.admit_mutation().await.map_err(internal)?;
        let pool = self.one_pool().await.map_err(internal)?;
        let datasets = self.datasets(&pool).await.map_err(internal)?;
        Ok(Scope {
            _held: held,
            _installation: installation,
            pool,
            datasets,
            name,
        })
    }

    async fn z(&self, args: &[&str]) -> Result<(), RpcError> {
        self.zfs(args).await.map(drop).map_err(internal)
    }

    /// The copy as the datasets stand now.
    async fn copy_now(&self, scope: &Scope) -> Result<Copy, RpcError> {
        let datasets = self.datasets(&scope.pool).await.map_err(internal)?;
        self.copy(&datasets, &scope.pool, &scope.name)
            .await
            .map_err(internal)
    }

    /// The copy the route found, for a refusal that changed nothing.
    async fn copy_found(&self, scope: &Scope) -> Result<Copy, RpcError> {
        self.copy(&scope.datasets, &scope.pool, &scope.name)
            .await
            .map_err(internal)
    }

    async fn refuse_found(&self, scope: &Scope, reason: CopyRefusal, message: String) -> RpcError {
        match self.copy_found(scope).await {
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
            .await
            .map_err(internal)?;
        Ok(written.trim() != "0")
    }

    async fn copy_inspect(&self, name: &WireName) -> Result<Copy, RpcError> {
        let scope = self.scope(name).await?;
        self.copy_found(&scope).await
    }

    /// Writer to Mirror. The daemon stopped the Container and holds its admission turn;
    /// this moves the root into the slot at `target`.
    async fn copy_cut(&self, request: &CutRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?;
        let name = &scope.name;
        let Some(root) = scope.root()? else {
            let copy = self.copy_found(&scope).await?;
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
        let (root_name, readonly, mounted, refquota) = (
            root.name.clone(),
            root.readonly,
            root.mounted,
            root.refquota,
        );
        let taken = self
            .snapshots(&root_name)
            .await
            .map_err(internal)?
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
        self.z(&["set", &format!("{CUT_PROPERTY}=on"), &root_name])
            .await?;
        if !taken {
            self.z(&["snapshot", &format!("{root_name}@{}", request.target)])
                .await?;
        }
        ployzd::faults::kill_inside("SetVolumeCopy.WriterToMirror");
        if mounted {
            self.z(&["unmount", &root_name]).await?;
        }
        self.ensure_mirror_root(&scope.pool, &scope.datasets)
            .await
            .map_err(internal)?;
        if scope.slot().is_none() {
            self.create_bounded_slot(&slot_parent(&scope.pool, name), refquota)
                .await?;
        }
        self.z(&["rename", &root_name, &slot_fs(&scope.pool, name)])
            .await?;
        self.copy_now(&scope).await
    }

    /// A slot parent bounded like the copy it will hold. Unlike the old mirror slot it
    /// carries no lease marker.
    async fn create_bounded_slot(&self, parent: &str, bound: u64) -> Result<(), RpcError> {
        self.z(&[
            "create",
            "-o",
            "canmount=off",
            "-o",
            "readonly=on",
            "-o",
            &format!("refquota={bound}"),
            parent,
        ])
        .await
    }

    /// Writer to Snapshot: take `snapshot` when anything was written since the newest
    /// run snapshot, then keep the newest two.
    async fn copy_run_snapshot(&self, request: &SnapshotRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?;
        let root = self.require_root(&scope).await?;
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
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
        ployzd::faults::kill_inside("SetVolumeCopy.WriterToSnapshot");
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
        self.destroy_snapshots(&root, snapshots.get(2..).unwrap_or_default())
            .await?;
        self.copy_now(&scope).await
    }

    /// Writer to Forget: every run snapshot dropped from the root.
    async fn copy_forget(&self, name: &WireName) -> Result<Copy, RpcError> {
        let scope = self.scope(name).await?;
        let root = self.require_root(&scope).await?;
        let snapshots = self.snapshots(&root).await.map_err(internal)?;
        for (index, snapshot) in snapshots.iter().enumerate() {
            self.z(&["destroy", &format!("{root}@{}", snapshot.name)])
                .await?;
            if index == 0 {
                ployzd::faults::kill_inside("SetVolumeCopy.WriterToForget");
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

    /// Writer at `at` numbered `epoch`. A read-only root becomes writable again; a slot
    /// whose newest snapshot is `at` is promoted to the root first.
    async fn copy_writer(&self, request: &WriterRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?;
        let name = &scope.name;
        let root_name = super::storage::root_dataset(&scope.pool, name);
        if let Some(root) = scope.root()? {
            let (readonly, mountpoint) = (root.readonly, root.mountpoint.clone());
            let epoch = self.epoch(&root_name).await.map_err(internal)?;
            if epoch != request.epoch {
                return Err(self
                    .refuse_found(
                        &scope,
                        CopyRefusal::NoTransition,
                        format!(
                            "Volume {name} is writer epoch {epoch}, not {}",
                            request.epoch
                        ),
                    )
                    .await);
            }
            self.require_at(&scope, &root_name, request).await?;
            if scope.slot().is_some() && scope.fs().is_none() {
                self.z(&["destroy", &slot_parent(&scope.pool, name)])
                    .await?;
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
        if self.receives.running(name) {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::Busy,
                    format!("Volume {name} is still receiving into its slot"),
                )
                .await);
        }
        if self
            .property(&fs_name, RESUME_TOKEN_PROPERTY)
            .await
            .map_err(internal)?
            .is_some()
        {
            self.z(&["receive", "-A", &fs_name]).await?;
        }
        self.z(&[
            "set",
            &format!("{EPOCH_PROPERTY}={}", request.epoch),
            &fs_name,
        ])
        .await?;
        // A pulled copy has no Docker record yet, so the root hides from Get and List
        // until Docker's own Create registers it with labels. A cut copy keeps its
        // record, and hiding it would make Docker drop that record.
        let cut = self
            .property(&fs_name, CUT_PROPERTY)
            .await
            .map_err(internal)?
            .is_some();
        if !cut {
            self.z(&["set", &format!("{PROMOTE_PROPERTY}=on"), &fs_name])
                .await?;
        }
        self.z(&["rename", &fs_name, &root_name]).await?;
        self.z(&["destroy", &slot_parent(&scope.pool, name)])
            .await?;
        self.make_writable(&root_name, "", true, name).await?;
        self.copy_now(&scope).await
    }

    async fn require_at(
        &self,
        scope: &Scope,
        dataset: &str,
        request: &WriterRequest,
    ) -> Result<(), RpcError> {
        let newest = self
            .snapshots(dataset)
            .await
            .map_err(internal)?
            .into_iter()
            .next();
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
        name: &DockerVolumeName,
    ) -> Result<(), RpcError> {
        if mountpoint != name.mountpoint() {
            self.z(&["inherit", "mountpoint", root]).await?;
        }
        if readonly {
            self.z(&["set", "readonly=off", root]).await?;
        }
        if self
            .property(root, CUT_PROPERTY)
            .await
            .map_err(internal)?
            .is_some()
        {
            self.z(&["inherit", CUT_PROPERTY, root]).await?;
        }
        Ok(())
    }

    /// Mirror to Mirror and None to Mirror: the slot pulls `target` from `from`. A
    /// receive in flight is left alone; a landed target is bounded and pruned; anything
    /// else starts or resumes the receive.
    async fn copy_pull(&self, request: &PullRequest) -> Result<Copy, RpcError> {
        let scope = self.scope(&request.name).await?;
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
        let parent = slot_parent(&scope.pool, name);
        let bound = match scope.slot() {
            Some(slot) => slot.refquota,
            None => {
                let bound = self.fetch_bound(request).await?;
                if let Err(error) = self
                    .ensure_commitment(&scope.pool, &scope.datasets, bound)
                    .await
                {
                    return Err(match error {
                        VolumeError::Capacity(_) => refuse(
                            CopyRefusal::NoCapacity,
                            None,
                            format!("this Machine has no room for a {bound} byte copy of {name}"),
                        ),
                        error => internal(error),
                    });
                }
                self.ensure_mirror_root(&scope.pool, &scope.datasets)
                    .await
                    .map_err(internal)?;
                self.create_bounded_slot(&parent, bound).await?;
                ployzd::faults::kill_inside("SetVolumeCopy.NoneToMirror");
                bound
            }
        };
        if bound == 0 {
            return Err(internal(VolumeError::from(format!(
                "slot {parent} carries no bound"
            ))));
        }
        let target = request.target.to_string();
        if self
            .property(&parent, PULL_PROPERTY)
            .await
            .map_err(internal)?
            .as_deref()
            != Some(target.as_str())
        {
            self.z(&["set", &format!("{PULL_PROPERTY}={target}"), &parent])
                .await?;
        }
        let fs = slot_fs(&scope.pool, name);
        let newest = match scope.fs() {
            Some(_) => self
                .snapshots(&fs)
                .await
                .map_err(internal)?
                .into_iter()
                .next(),
            None => None,
        };
        if self.receives.running(name) {
            return self.copy_now(&scope).await;
        }
        if newest
            .as_ref()
            .is_some_and(|newest| newest.name == request.target)
        {
            self.finish_copy(&fs, bound)
                .await
                .map_err(|error| internal(error.into()))?;
            let snapshots = self.snapshots(&fs).await.map_err(internal)?;
            self.destroy_snapshots(&fs, snapshots.get(1..).unwrap_or_default())
                .await?;
            self.z(&["inherit", PULL_PROPERTY, &parent]).await?;
            self.receives.forget(name);
            return self.copy_now(&scope).await;
        }
        let token = match scope.fs() {
            Some(_) => self
                .property(&fs, RESUME_TOKEN_PROPERTY)
                .await
                .map_err(internal)?,
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
        let outcome = self.receives.start(name);
        let storage = self.clone();
        let from = request.from;
        tokio::spawn(async move {
            let result = storage.pull_stream(from, stream, &fs, bound).await;
            if let Err(error) = &result {
                tracing::warn!(%fs, %error, "Volume pull failed");
            }
            *outcome.lock().expect("outcome lock") = Some(result);
        });
        self.copy_now(&scope).await
    }

    /// One receive of `stream`. When the writer no longer has the base or the token the
    /// stream names, the slot's copy starts over from a full send.
    async fn pull_stream(
        &self,
        from: ployz_core::ManagementAddress,
        stream: SendStream,
        fs: &str,
        bound: u64,
    ) -> Result<(), String> {
        let fault = "SetVolumeCopy.MirrorToMirror";
        match self.receive(from, &stream, fs, bound, fault).await {
            Ok(()) => return Ok(()),
            Err(ReceiveFailure::Failed(error)) => return Err(error),
            Err(ReceiveFailure::NoStream(_)) => {}
        }
        let target = match &stream.source {
            SendSource::Full { .. } => {
                return Err(format!(
                    "{} has no snapshot {} to send",
                    from.0, stream.name
                ));
            }
            SendSource::Resume { target, .. } => {
                self.zfs(&["receive", "-A", fs])
                    .await
                    .map_err(|error| error.to_string())?;
                target.clone()
            }
            SendSource::Incremental { target, .. } => target.clone(),
        };
        self.zfs(&["destroy", "-r", fs])
            .await
            .map_err(|error| error.to_string())?;
        let full = SendStream {
            name: stream.name,
            source: SendSource::Full { target },
        };
        self.receive(from, &full, fs, bound, fault)
            .await
            .map_err(|failure| failure.to_string())
    }

    /// The writer's bound for the Volume, from its send port.
    async fn fetch_bound(&self, request: &PullRequest) -> Result<u64, RpcError> {
        let url = format!(
            "http://[{}]:{}{BOUND_PATH_PREFIX}{}",
            request.from.0, self.send_port, request.name
        );
        let client = reqwest::Client::builder()
            .connect_timeout(self.receive_stall)
            .timeout(self.receive_stall)
            .build()
            .map_err(|error| {
                internal(format!("could not build the bound client: {error}").into())
            })?;
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|error| internal(format!("GET {url}: {error}").into()))?;
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
            .map_err(|error| internal(format!("GET {url}: {error}").into()))?
            .text()
            .await
            .map_err(|error| internal(format!("GET {url}: {error}").into()))?;
        body.trim()
            .parse()
            .map_err(|error| internal(format!("GET {url} answered {body:?}: {error}").into()))
    }

    /// Mirror to Forget: the slot and its copy destroyed.
    async fn copy_destroy(&self, name: &WireName) -> Result<Copy, RpcError> {
        let scope = self.scope(name).await?;
        let name = &scope.name;
        if scope.root()?.is_some() {
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
        if self.receives.running(name) {
            return Err(self
                .refuse_found(
                    &scope,
                    CopyRefusal::Busy,
                    format!("Volume {name} is still receiving into its slot"),
                )
                .await);
        }
        ployzd::faults::kill_inside("SetVolumeCopy.MirrorToForget");
        self.z(&["destroy", "-r", &slot_parent(&scope.pool, name)])
            .await?;
        self.receives.forget(name);
        self.copy_now(&scope).await
    }
}
