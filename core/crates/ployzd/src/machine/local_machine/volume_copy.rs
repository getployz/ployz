//! SetVolumeCopy, Volume.Seed and SetVolumeSize: the transition table and its Container
//! half, under the switch lock each takes. ZFS runs on the plugin through the routes in
//! [`crate::volume_copy`].

use ployz_core::{
    ContainerKind, ContainerLocator, ContainerNow, ContainerObservation, ContainerRefusalReason,
    ContainerWant, CopyPlan, CopyRefusal, CopyRotation, CopyTransition, DockerVolumeName,
    EnsuredContainer, HealthVerdict, QualifiedService, RpcError, RunRotation, RunTarget,
    SeedCopyRequest, SeedVolumeRequest, SetVolumeCopyRefused, SetVolumeCopyRequest,
    SetVolumeSizeReply, SetVolumeSizeRequest, Start, VolumeCopy, VolumeCopyView, VolumeGen,
    WantedCopy,
};

use ployz_core::authority::{
    AuthorityRefusal, CopyFacts, CopyVerified, CopyWindow, Purpose, Timestamp, copy,
};

use super::{AdmittedSpec, CutRefusal, Error, LocalMachine, SwitchGuard};
use crate::{
    docker::FloorTurn,
    faults::{Identity, Point, Want},
    machine::has_cluster_identity,
    volume_copy::{
        CUT_ROUTE, CopyBinding, CopyRequest, CutRequest, DESTROY_ROUTE, FORGET_ROUTE,
        ForgetRequest, INSPECT_ROUTE, PULL_ROUTE, PullRequest, SNAPSHOT_ROUTE, SeedCopy,
        SnapshotRequest, StartWindow, VOLUME_SEED_COPY_ROUTE, VOLUME_SEED_ROUTE,
        VOLUME_SIZE_SET_ROUTE, WRITER_ROUTE, WriterRequest,
    },
};

type Copy = Option<VolumeCopy>;

/// What Docker shows of a Writer's start before the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartState {
    Running,
    Stopped,
    Gone,
}

impl LocalMachine {
    /// SetVolumeCopy: move the copy of `request.name` to its want, under the switch lock
    /// from the floor check through the transition.
    ///
    /// # Errors
    ///
    /// Returns the refusal the request answers, or the failure of a step.
    pub(crate) async fn set_volume_copy(
        &self,
        request: SetVolumeCopyRequest,
    ) -> Result<Copy, Error> {
        // The spec of the Container this request's start creates, when it creates one,
        // which the create checks again under its turn.
        let admitted = match &request.want {
            WantedCopy::Writer {
                start: Some(start), ..
            } => self.admit_volume_start(&request.name, start).await?,
            WantedCopy::Writer { start: None, .. }
            | WantedCopy::Mirror { .. }
            | WantedCopy::Snapshot { .. }
            | WantedCopy::Forget
            | WantedCopy::Floor => None,
        };
        if let WantedCopy::Writer {
            start:
                Some(Start::Create {
                    check: true,
                    resolved_spec,
                    ..
                }),
            ..
        } = &request.want
        {
            // What the Create after the promote refuses, a check refuses before the cut.
            self.docker()?
                .lifecycle()
                .check_start_volumes(resolved_spec, self.plugin(), &request.name)
                .await?;
            return self.inspect_copy(&request.name).await;
        }
        if request.want == WantedCopy::Floor {
            return self.floor_copy(&request.name, request.r#gen).await;
        }
        self.admit_gen(&request.name, request.r#gen).await?;
        let switch = self.lock_switch().await?;
        crate::faults::apply(
            Point::Verb("SetVolumeCopy"),
            Some(Identity::new(&request.name, &request.want, request.r#gen)),
        )
        .await;
        // Detached with the switch guard, so a caller that hangs up mid-drain does not
        // free the next request to plan against a cut still running. The task runs this
        // one transition and ends when ZFS or Docker answers; it never retries or starts
        // other work.
        tokio::spawn(async move { switch.set_copy_planned(request, admitted).await }).await?
    }

    /// Volume.Seed, under the switch lock.
    ///
    /// # Errors
    ///
    /// Returns a Stale refusal for a gen below the floor of `into`, or the plugin's error.
    pub(crate) async fn seed_volume(
        &self,
        request: &SeedVolumeRequest,
    ) -> Result<VolumeCopyView, Error> {
        let switch = self.lock_switch().await?;
        // Only a Full caller seeds, checked under the switch: the Machine API refused an
        // Operator on arrival, and a key that lost its Device slot since is one now.
        switch.full_caller()?;
        // Checked under the switch lock, which a mirror pull of `into` takes too: a check
        // before it would let a Seed admitted at the old floor run after a later run
        // raised the floor and pulled a Mirror of `into` here, and replace that Mirror.
        if let Some(r#gen) = request.r#gen {
            switch.check_gen(&request.into, r#gen).await?;
        }
        switch
            .volume_verb("SeedVolume", VOLUME_SEED_ROUTE, request)
            .await
    }

    /// SeedCopy: a Volume.Seed an Operator sends under a person's Copy step, under the
    /// switch lock. The step must open here for exactly this seed, and is taken in the
    /// approvals record only while its window holds. A step taken before is taken again
    /// past its window, and the plugin then hears the window closed, so a resend only
    /// finishes a copy underway, starting its receive over in the slot the step made if a
    /// restart killed it. Otherwise the plugin makes a new copy's slot only while the
    /// window still holds on its own clock. It leaves the step's tombstone
    /// whenever it destroys what the step made, so a resend never makes a deleted new
    /// Volume again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CopyRefused`] for a step that does not open here, a Stale refusal
    /// for a gen below the floor of `into`, or the plugin's error.
    pub(crate) async fn seed_copy(
        &self,
        request: SeedCopyRequest,
    ) -> Result<VolumeCopyView, Error> {
        let switch = self.lock_switch().await?;
        if let Some(r#gen) = request.seed.r#gen {
            switch.check_gen(&request.seed.into, r#gen).await?;
        }
        let record = switch.record();
        if !record.has_cluster_identity() {
            return Err(Error::NotParticipating);
        }
        let trusted = record.trusted_keys();
        let read = Timestamp::now();
        let now = self.approvals.observed_now(read)?;
        let facts = CopyFacts {
            trusted: &trusted,
            cluster: self.cluster_key(record.phase()),
            machine: record.id(),
            now,
        };
        let verified =
            copy::verify(&request.step, &request.seed, &facts).map_err(Error::CopyRefused)?;
        let birth = verified.origin().map_err(|error| {
            Error::CopyRefused(AuthorityRefusal::Malformed {
                purpose: Purpose::Workflow,
                problem: error.to_string(),
            })
        })?;
        self.approvals.take_copy(&verified)?;
        let step = &verified.step;
        let seed = SeedCopy {
            seed: request.seed,
            copy: CopyBinding {
                birth,
                source: step.origin,
                taken_at: step.taken_at,
                step: verified.digest,
                window: match verified.window {
                    CopyWindow::Open => StartWindow::Open {
                        closes: plugin_closes(&verified, (read, now)),
                    },
                    CopyWindow::Closed => StartWindow::Closed,
                },
            },
        };
        switch
            .volume_verb::<VolumeCopyView>("SeedCopy", VOLUME_SEED_COPY_ROUTE, &seed)
            .await
    }

    /// SetVolumeSize, under the switch lock.
    ///
    /// # Errors
    ///
    /// Returns the plugin's error.
    pub(crate) async fn set_volume_size(
        &self,
        request: &SetVolumeSizeRequest,
    ) -> Result<SetVolumeSizeReply, Error> {
        let switch = self.lock_switch().await?;
        switch
            .volume_verb("SetVolumeSize", VOLUME_SIZE_SET_ROUTE, request)
            .await
    }

    /// Call the plugin's `route` for the volume verb `verb` on a joined Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotParticipating`] off a Cluster, or the plugin's error.
    pub(crate) async fn volume_verb<T: serde::de::DeserializeOwned>(
        &self,
        verb: &'static str,
        route: &str,
        request: &impl serde::Serialize,
    ) -> Result<T, Error> {
        self.require_joined()?;
        crate::faults::apply(Point::Verb(verb), None).await;
        self.plugin
            .call::<T>(route, request)
            .await
            .map_err(Error::StoragePreparation)
    }

    fn require_joined(&self) -> Result<(), Error> {
        if has_cluster_identity(&self.record().phase()) {
            Ok(())
        } else {
            Err(Error::NotParticipating)
        }
    }
}

impl SwitchGuard {
    async fn set_copy_planned(
        &self,
        request: SetVolumeCopyRequest,
        admitted: Option<AdmittedSpec>,
    ) -> Result<Copy, Error> {
        let SetVolumeCopyRequest {
            name,
            expect,
            want,
            r#gen,
        } = request;
        // Checked again at the scope the caller holds under the switch, before any effect:
        // a caller that lost its Device slot while this queued promotes nothing for a start
        // an Operator may not make.
        if let WantedCopy::Writer {
            start: Some(start), ..
        } = &want
        {
            self.recheck_volume_start(&name, start, admitted.as_ref())
                .await?;
        }
        // Checked again under the switch lock: a newer lane may have fenced this Volume
        // while the request queued behind a cut.
        self.admit_gen(&name, r#gen).await?;
        let copy = self.inspect_copy(&name).await?;
        let start = match &want {
            WantedCopy::Writer {
                start: Some(start), ..
            } => Some(self.start_state(start, &name).await?),
            WantedCopy::Writer { start: None, .. }
            | WantedCopy::Mirror { .. }
            | WantedCopy::Snapshot { .. }
            | WantedCopy::Forget
            | WantedCopy::Floor => None,
        };
        let start_running = start == Some(StartState::Running);
        let transition = match ployz_core::plan(copy.as_ref(), start_running, &expect, &want) {
            CopyPlan::AtWant => None,
            CopyPlan::Refuse(reason) => return Err(refused(reason, copy, &name)),
            CopyPlan::Transition(_) if start == Some(StartState::Gone) => {
                return Err(refused(CopyRefusal::NoTransition, copy, &name));
            }
            CopyPlan::Transition(transition) => Some(transition),
        };
        let Some(transition) = transition else {
            if let WantedCopy::Writer {
                start: Some(start), ..
            } = &want
            {
                self.route_created(start, &name).await?;
            }
            return Ok(copy);
        };
        crate::faults::apply(
            Point::Copy(transition),
            Some(Identity::new(&name, &want, r#gen)),
        )
        .await;
        match (transition, want) {
            (CopyTransition::WriterToWriter, WantedCopy::Writer { at, epoch, start }) => {
                let copy = match copy {
                    Some(VolumeCopy::Root {
                        readonly,
                        epoch: held,
                        ..
                    }) if readonly || held != epoch => {
                        let request = WriterRequest {
                            name: name.clone(),
                            at,
                            epoch,
                            r#gen,
                        };
                        self.plugin_copy(WRITER_ROUTE, &request).await?
                    }
                    copy => copy,
                };
                // Another holder already runs: starting this one too would run two. Read
                // again here, since a holder can exit or go while the Writer route runs.
                if let Some(start) = start {
                    match self.start_state(&start, &name).await? {
                        StartState::Running => {}
                        StartState::Gone => {
                            return Err(refused(CopyRefusal::NoTransition, copy, &name));
                        }
                        StartState::Stopped => {
                            self.run_start(start.clone(), &name, r#gen, admitted)
                                .await?;
                        }
                    }
                    self.route_created(&start, &name).await?;
                }
                Ok(copy)
            }
            (CopyTransition::MirrorToWriter, WantedCopy::Writer { at, epoch, start }) => {
                let request = WriterRequest {
                    name: name.clone(),
                    at,
                    epoch,
                    r#gen,
                };
                let copy = self.plugin_copy(WRITER_ROUTE, &request).await?;
                crate::faults::kill_inside(
                    Point::Copy(CopyTransition::MirrorToWriter),
                    Some(Identity {
                        volume: &name,
                        want: Want::Writer { at, epoch },
                        r#gen,
                    }),
                );
                if let Some(start) = start {
                    self.run_start(start.clone(), &name, r#gen, admitted)
                        .await?;
                    self.route_created(&start, &name).await?;
                }
                Ok(copy)
            }
            (
                CopyTransition::WriterToMirror,
                WantedCopy::Mirror {
                    target,
                    drain,
                    holder,
                    running,
                    ..
                },
            ) => {
                let plugin = self.plugin().clone();
                let request = CutRequest {
                    name: name.clone(),
                    target,
                    r#gen,
                };
                let cut = self
                    .cut_volume_copy(
                        name.clone(),
                        r#gen,
                        drain,
                        holder.map(|holder| (holder, running)),
                        move || async move {
                            // This closure runs only past the Busy re-check, so the line means every
                            // holder is stopped, whoever signalled it. The verify harness credits that
                            // moment by this line's Volume, target and gen, after its restart cursor.
                            tracing::info!(
                                volume = %request.name,
                                target = %request.target,
                                r#gen = %request.r#gen,
                                "cut: holders stopped"
                            );
                            plugin
                                .call::<Copy>(CUT_ROUTE, &request)
                                .await
                                .map(drop)
                                .map_err(Error::StoragePreparation)
                        },
                    )
                    .await?;
                match cut {
                    Ok(()) => self.inspect_copy(&name).await,
                    Err(CutRefusal::Busy) => Err(refused(
                        CopyRefusal::Busy,
                        self.inspect_copy(&name).await?,
                        &name,
                    )),
                    Err(CutRefusal::Stale(floor)) => {
                        Err(stale(floor, self.inspect_copy(&name).await?, &name))
                    }
                    Err(CutRefusal::Stopped) => Err(refused(
                        CopyRefusal::Stopped,
                        self.inspect_copy(&name).await?,
                        &name,
                    )),
                    Err(CutRefusal::Held(holder)) => Err(refusal(
                        SetVolumeCopyRefused {
                            reason: CopyRefusal::Held,
                            copy: self.inspect_copy(&name).await?,
                            floor: None,
                            holder: Some(holder),
                        },
                        &name,
                    )),
                }
            }
            (CopyTransition::WriterToSnapshot, WantedCopy::Snapshot { name: snapshot }) => {
                self.plugin_copy(
                    SNAPSHOT_ROUTE,
                    &SnapshotRequest {
                        name,
                        snapshot,
                        r#gen,
                    },
                )
                .await
            }
            (CopyTransition::WriterToForget, WantedCopy::Forget) => {
                self.plugin_copy(FORGET_ROUTE, &ForgetRequest { name, r#gen })
                    .await
            }
            (
                CopyTransition::MirrorToMirror | CopyTransition::NoneToMirror,
                WantedCopy::Mirror {
                    from: Some(from),
                    target,
                    ..
                },
            ) => {
                self.admit_pull_source(&from).await?;
                self.plugin_copy(
                    PULL_ROUTE,
                    &PullRequest {
                        name,
                        from,
                        target,
                        r#gen,
                    },
                )
                .await
            }
            (CopyTransition::MirrorToForget | CopyTransition::SeedToForget, WantedCopy::Forget) => {
                // Recorded before the destroy, so no restart after it loses the record.
                if let Some(VolumeCopy::Slot { origin, .. } | VolumeCopy::Root { origin, .. }) =
                    &copy
                {
                    let origin = origin.unwrap_or_else(|| ployz_core::VolumeOrigin::legacy(&name));
                    self.docker()?.lifecycle().forgot(&name, &origin).await?;
                }
                self.plugin_copy(DESTROY_ROUTE, &ForgetRequest { name, r#gen })
                    .await
            }
            (transition, want) => {
                unreachable!("the planner paired {transition:?} with {want:?}")
            }
        }
    }
}

impl LocalMachine {
    async fn admit_gen(&self, name: &DockerVolumeName, r#gen: VolumeGen) -> Result<(), Error> {
        let admitted = self.joined_floor_turn().await?.admit(name, r#gen).await?;
        match admitted {
            Ok(()) => Ok(()),
            Err(floor) => Err(stale(floor, self.inspect_copy(name).await?, name)),
        }
    }

    /// Refuse `gen` Stale below the floor of `name`, without raising it. A Seed fences on
    /// `into` this way: raising it would refuse every later mirror sync of `into` that
    /// carries a gen below the seed run's.
    async fn check_gen(&self, name: &DockerVolumeName, r#gen: VolumeGen) -> Result<(), Error> {
        let checked = self.joined_floor_turn().await?.check(name, r#gen).await?;
        match checked {
            Ok(()) => Ok(()),
            Err(floor) => Err(stale(floor, self.inspect_copy(name).await?, name)),
        }
    }

    /// The Floor want: raise the floor and read the copy under the floor lock, which a
    /// cut holds through its flip, so the copy is the root or the slot, never between.
    /// Never refuses on `gen`; a lower one leaves the floor where it is.
    async fn floor_copy(&self, name: &DockerVolumeName, r#gen: VolumeGen) -> Result<Copy, Error> {
        let mut floor = self.joined_floor_turn().await?;
        floor.admit(name, r#gen).await?.ok();
        self.inspect_copy(name).await
    }

    /// The floor lock, with the caller's access checked under it. A floor raise takes no
    /// turn and no switch, so it lands during a cut's drain and fences it, and every key
    /// change takes this lock last, so a caller removed while this queued raises nothing.
    ///
    /// # Errors
    /// Returns [`Error::ManagementRevoked`] when this Machine no longer accepts the
    /// caller's key.
    async fn joined_floor_turn(&self) -> Result<FloorTurn<'_>, Error> {
        self.require_joined()?;
        let lifecycle = self.docker()?.lifecycle();
        #[cfg(test)]
        self.floor_asked.send_modify(|asked| *asked += 1);
        let turn = lifecycle.floor_turn().await;
        // Checked again under the floor lock: a reset may have begun while this queued.
        self.require_joined()?;
        self.clone().checked_under(&turn)?;
        Ok(turn)
    }

    async fn inspect_copy(&self, name: &DockerVolumeName) -> Result<Copy, Error> {
        self.plugin_copy(INSPECT_ROUTE, &CopyRequest { name: name.clone() })
            .await
    }

    async fn plugin_copy(
        &self,
        route: &str,
        request: &impl serde::Serialize,
    ) -> Result<Copy, Error> {
        self.plugin
            .call::<Copy>(route, request)
            .await
            .map_err(Error::StoragePreparation)
    }

    /// Whether the Writer's start already runs. It runs when Docker runs the Container
    /// `start` names, or a [`Self::replacement_running`]. Another holder of the Volume
    /// is not the Service, so its run is no start. A Kept Container Docker no longer
    /// has is gone, and nothing can start it again.
    async fn start_state(
        &self,
        start: &Start,
        name: &DockerVolumeName,
    ) -> Result<StartState, Error> {
        let observed = self.start_observed(start).await?;
        if observed
            .as_ref()
            .is_some_and(|observed| observed.runtime.is_running())
            || self
                .replacement_running(start, observed.as_ref(), name)
                .await?
        {
            return Ok(StartState::Running);
        }
        Ok(match (start, observed) {
            (Start::Kept { .. }, None) => StartState::Gone,
            _ => StartState::Stopped,
        })
    }

    /// Whether Docker runs another Service Container of the Service `start` starts on
    /// the Volume `name`: a Service that moved on to a new Container after the start
    /// landed has the start's effect already. A helper Ployz didn't make, a hook, or
    /// another Service is not that Service. A Kept Container Docker no longer has names
    /// no Service, so any Service Container on the Volume counts for it.
    async fn replacement_running(
        &self,
        start: &Start,
        observed: Option<&ContainerObservation>,
        name: &DockerVolumeName,
    ) -> Result<bool, Error> {
        let service = match (start, observed) {
            (
                Start::Create {
                    namespace,
                    resolved_spec,
                    ..
                },
                _,
            ) => Some(QualifiedService::new(
                namespace.clone(),
                resolved_spec.name.clone(),
            )),
            (Start::Kept { .. }, Some(observed)) => Some(QualifiedService::new(
                observed.namespace.clone(),
                observed.resolved_spec.name.clone(),
            )),
            (Start::Kept { .. }, None) => None,
        };
        Ok(self
            .docker()?
            .volumes()
            .service_beside(
                name,
                observed.map(|observed| &observed.container_id),
                service.as_ref(),
            )
            .await?)
    }

    async fn start_observed(&self, start: &Start) -> Result<Option<ContainerObservation>, Error> {
        let containers = self.docker()?;
        let machine_id = self.record().id();
        let container_id = match start {
            Start::Kept { container_id } => Some(*container_id),
            Start::Create {
                namespace,
                resolved_spec,
                creation_key,
                ..
            } => {
                let record = self.record();
                let machine = record.machine().ok_or(Error::NotParticipating)?;
                containers
                    .lifecycle()
                    .find_creation(
                        machine,
                        namespace,
                        ContainerKind::ServiceContainer,
                        resolved_spec,
                        creation_key.as_str(),
                    )
                    .await?
                    .map(|created| created.container_id)
            }
        };
        let Some(container_id) = container_id else {
            return Ok(None);
        };
        match containers
            .observation()
            .inspect_managed(&container_id, &machine_id)
            .await
        {
            Ok(observed) => Ok(Some(observed)),
            Err(crate::docker::Error::ContainerNotFound(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// A Move's Create that asks for In routes the copy it made once healthy, as an In
    /// want does. A copy left unrouted fails the request, so the Move keeps its source
    /// Container and a resend tries again. A newer copy already In serves the Service,
    /// so that refusal is no failure.
    async fn route_created(&self, start: &Start, name: &DockerVolumeName) -> Result<(), Error> {
        let Start::Create {
            rotation: rotation @ RunRotation::In { .. },
            ..
        } = start
        else {
            return Ok(());
        };
        let created = self.start_observed(start).await?;
        let running = created
            .as_ref()
            .is_some_and(|created| created.runtime.is_running());
        // A resend lands here too when a replacement of the Service runs; only this
        // Create's own copy is routed. A copy that exited is started again by its In want,
        // which reports health.
        if !running
            && self
                .replacement_running(start, created.as_ref(), name)
                .await?
        {
            return Ok(());
        }
        let Some(created) = created else {
            return Err(Error::StoragePreparation(not_routed(
                "the moved Service is not routed: its Container is gone".to_owned(),
            )));
        };
        // Boxed: the In future is large, and inline it overflows the SetVolumeCopy task's stack.
        let ensured = Box::pin(self.ensure_container(
            ContainerWant::Running {
                container: RunTarget::Existing(ContainerLocator::Id(created.container_id)),
                rotation: rotation.clone(),
            },
            None,
        ))
        .await?;
        let error = match ensured {
            EnsuredContainer::Ensured {
                now: ContainerNow::Present(observed),
                ..
            } if *observed.rotation() == CopyRotation::In => return Ok(()),
            EnsuredContainer::Refused { refusal, .. }
                if refusal.reason == ContainerRefusalReason::OlderThanIn =>
            {
                return Ok(());
            }
            EnsuredContainer::Refused { refusal, .. } => refusal.into_error(),
            EnsuredContainer::Ensured { health, .. } => {
                not_routed(match health.and_then(HealthVerdict::into_failure) {
                    Some(failure) => format!("the moved Service is not routed: {failure}"),
                    None => "the moved Service stopped before it was routed".to_owned(),
                })
            }
        };
        Err(Error::StoragePreparation(error))
    }

    /// Create the Container when `start` asks for one and it is absent, then start it.
    /// Both halves are idempotent, so a resend after either converges. `Stale` when a
    /// newer lane raised the floor after this request was admitted.
    async fn run_start(
        &self,
        start: Start,
        name: &DockerVolumeName,
        r#gen: VolumeGen,
        admitted: Option<AdmittedSpec>,
    ) -> Result<(), Error> {
        match self
            .start_volume_holder(start, name.clone(), r#gen, admitted)
            .await?
        {
            Ok(()) => Ok(()),
            Err(floor) => Err(stale(floor, self.inspect_copy(name).await?, name)),
        }
    }
}

/// A delay names one cell, so a run can slow the promote without slowing every pull poll.
fn refused(reason: CopyRefusal, copy: Copy, name: &DockerVolumeName) -> Error {
    refusal(
        SetVolumeCopyRefused {
            reason,
            copy,
            floor: None,
            holder: None,
        },
        name,
    )
}

fn stale(floor: VolumeGen, copy: Copy, name: &DockerVolumeName) -> Error {
    refusal(
        SetVolumeCopyRefused {
            reason: CopyRefusal::Stale,
            copy,
            floor: Some(floor),
            holder: None,
        },
        name,
    )
}

fn refusal(refused: SetVolumeCopyRefused, name: &DockerVolumeName) -> Error {
    let message = match refused.reason {
        CopyRefusal::Mismatch => format!("Volume {name} is not the copy the request expected"),
        CopyRefusal::NoTransition => format!("Volume {name} has no transition to the want"),
        CopyRefusal::Busy => format!("Volume {name} is still held by a running Container"),
        CopyRefusal::NoCapacity => format!("this Machine has no room for a copy of {name}"),
        CopyRefusal::Behind => format!("Volume {name} is not at the wanted snapshot"),
        CopyRefusal::Written => format!("Volume {name} was written after the wanted snapshot"),
        CopyRefusal::NoSource => format!("Volume {name} has no copy to pull from"),
        CopyRefusal::Foreign => {
            format!("Volume {name} {}", crate::volume_copy::HELD_FOR_OTHER_DATA)
        }
        CopyRefusal::DockerUnavailable => {
            format!("Docker is not answering on this Machine, so Volume {name} stays as it is")
        }
        CopyRefusal::Held => format!(
            "{} also runs on Volume {name}, so the cut stopped nothing",
            refused.holder.as_deref().unwrap_or("another Container")
        ),
        CopyRefusal::Stopped => {
            format!(
                "the Container the cut would stop no longer runs on Volume {name}, so the cut stopped nothing"
            )
        }
        CopyRefusal::Unrecognized => format!("Volume {name} was refused for an unknown reason"),
        CopyRefusal::Stale => format!(
            "a newer move or release owns Volume {name}; this Machine is at lane {}",
            refused.floor.unwrap_or_default()
        ),
    };
    Error::StoragePreparation(refused.rpc_error(message))
}

fn not_routed(message: String) -> RpcError {
    RpcError::conflict(message)
}

/// When `verified`'s window closes on the plugin's clock, which reads the wall clock
/// unfloored: earlier by as much as the floored `now` runs ahead of the wall clock `read`.
fn plugin_closes(verified: &CopyVerified, (read, now): (Timestamp, Timestamp)) -> Timestamp {
    let ahead = now.as_millis().saturating_sub(read.as_millis());
    Timestamp::from_millis(verified.closes.as_millis().saturating_sub(ahead))
        .unwrap_or(verified.closes)
}
