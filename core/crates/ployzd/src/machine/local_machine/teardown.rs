//! Teardowns under Device enforcement: an Operator's step that starts nothing, Volume
//! removal, `Leave` or `Forget` runs only under a person's teardown that names this Server
//! and what goes, or, for a preview's close, the rule that made it.
//!
//! A teardown is checked beside a deploy approval, never inside it: a want that carries a
//! deploy approval, a restart approval or a rule's proof goes their way, and one carrying
//! only a teardown comes here. Nothing is consumed, so a resend after a crash finishes
//! under the same statement while its window lasts.
//!
//! Admission checks a teardown before the request waits for its turn, and each admitted
//! turn checks it again, against the trusted keys and the enforcement switch as they stand
//! then: a key removed, or enforcement turned on, while the request waited stops it before
//! any effect.

use ployz_core::{
    ContainerNow, ContainerWant, DockerVolumeName, MachineId, Namespace,
    authority::{
        AuthorityRefusal, Purpose, RestartStep, TeardownApproval, TeardownFacts, TeardownStep,
        Timestamp, teardown,
    },
};

use super::{Error, LocalMachine, restart_approval::pinned};

impl LocalMachine {
    /// Admit an Operator's step on an existing Container under `approval`: one that starts
    /// nothing, in a Namespace the teardown names, on this Server. Returns the want pinned
    /// to the Container seen, and the teardown each of its turns checks again. A Machine not
    /// enforcing passes the want untouched.
    ///
    /// # Errors
    /// Returns [`Error::TeardownRefused`] for a creation, a start, or a step the teardown
    /// does not name, and [`Error::DockerUnavailable`] or [`Error::Docker`] when the
    /// Container cannot be read.
    pub(crate) async fn admit_teardown_step(
        &self,
        want: ContainerWant,
        approval: Box<TeardownApproval>,
    ) -> Result<(ContainerWant, HeldTeardown), Error> {
        if !self.record().device_enforcement() {
            // Held with no Namespace: a turn that finds enforcement on by then refuses it,
            // and a resend is admitted in full.
            return Ok((want, HeldTeardown::unsigned(HeldStep::Container(None))));
        }
        let Some((step, locator)) = RestartStep::of(&want) else {
            return Err(Error::TeardownRefused(AuthorityRefusal::Mismatch {
                field: "a creation",
            }));
        };
        let ContainerNow::Present(observed) = self.container_now(locator).await? else {
            return Err(Error::TeardownRefused(AuthorityRefusal::Mismatch {
                field: "a Container",
            }));
        };
        let identity = observed.identity();
        let held = HeldTeardown {
            approval: Some(approval),
            step: HeldStep::Container(Some((identity.namespace.clone(), step))),
        };
        held.admit(self)?;
        tracing::info!(?step, "teardown step on {identity} admitted");
        Ok((pinned(want, observed.container_id), held))
    }

    /// Under Device enforcement an Operator's `RemoveVolume` deletes `name` only under a
    /// teardown that names it on this Server, or the preview rule whose preview it held. A
    /// Machine not enforcing passes untouched.
    ///
    /// # Errors
    /// Returns [`Error::TeardownRefused`] naming what the teardown lacks.
    pub(crate) fn admit_operator_volume_removal(
        &self,
        name: &DockerVolumeName,
        approval: Option<Box<TeardownApproval>>,
    ) -> Result<HeldTeardown, Error> {
        let held = HeldTeardown {
            approval,
            step: HeldStep::Volume(name.clone()),
        };
        if self.record().device_enforcement() {
            held.admit(self)?;
            tracing::info!("teardown removal of Volume {name} admitted");
        }
        Ok(held)
    }

    /// Under Device enforcement an Operator's `Leave` runs only under a Server teardown
    /// naming this Server as the one leaving, and its `Forget` of `forgotten` only under one
    /// naming `forgotten` as leaving and this Server as one that forgets it. Either resets a
    /// Server, so neither is Cloud's to start. A Machine not enforcing passes untouched.
    ///
    /// # Errors
    /// Returns [`Error::DepartureRefused`] naming the route's capability and what the
    /// teardown lacks.
    pub(crate) fn admit_operator_departure(
        &self,
        forgotten: Option<&MachineId>,
        approval: Option<Box<TeardownApproval>>,
    ) -> Result<HeldTeardown, Error> {
        let held = HeldTeardown {
            approval,
            step: forgotten.map_or(HeldStep::Leave, |machine| HeldStep::Forget(*machine)),
        };
        if self.record().device_enforcement() {
            held.admit(self)?;
            tracing::info!(step = ?held.step, "teardown departure admitted");
        }
        Ok(held)
    }

    /// Check this call's held teardown again, under the admission turn its caller holds:
    /// against the trusted keys and the enforcement switch as they stand now. A call that
    /// holds none, a Full caller's or this Machine's own, passes untouched.
    ///
    /// # Errors
    /// As the admission of the held teardown's route.
    pub(super) fn recheck_teardown(&self) -> Result<(), Error> {
        match &self.teardown {
            Some(held) if self.record().device_enforcement() => held.check(self, false),
            _ => Ok(()),
        }
    }
}

/// An Operator's teardown, or its absence, with the step it was sent for, which each
/// admitted turn of the call checks again.
#[derive(Clone, Debug)]
pub(crate) struct HeldTeardown {
    approval: Option<Box<TeardownApproval>>,
    step: HeldStep,
}

/// The step a held teardown was sent for.
#[derive(Clone, Debug)]
pub(crate) enum HeldStep {
    /// A step on an existing Container, with its Namespace. None when admitted with no
    /// teardown while this Machine did not enforce: a turn under enforcement refuses it.
    Container(Option<(Namespace, RestartStep)>),
    Volume(DockerVolumeName),
    Leave,
    Forget(MachineId),
}

impl HeldTeardown {
    /// A step sent to a Machine not enforcing, held with no teardown it could check.
    pub(crate) fn unsigned(step: HeldStep) -> Self {
        Self {
            approval: None,
            step,
        }
    }

    fn admit(&self, machine: &LocalMachine) -> Result<(), Error> {
        self.check(machine, true)
    }

    /// Verify the teardown for its step against `machine`'s record now. `admitting` logs
    /// who approved it, once per request.
    fn check(&self, machine: &LocalMachine, admitting: bool) -> Result<(), Error> {
        let refused = |refusal: AuthorityRefusal| {
            tracing::warn!(step = ?self.step, "teardown refused: {refusal}");
            match self.step {
                HeldStep::Leave => Error::DepartureRefused {
                    capability: ployz_core::LEAVE_MACHINE_CAPABILITY,
                    refusal,
                },
                HeldStep::Forget(_) => Error::DepartureRefused {
                    capability: ployz_core::FORGET_MACHINE_CAPABILITY,
                    refusal,
                },
                HeldStep::Container(_) | HeldStep::Volume(_) => Error::TeardownRefused(refusal),
            }
        };
        let unsigned = AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        };
        let Some(approval) = &self.approval else {
            return Err(refused(unsigned));
        };
        let step = match &self.step {
            HeldStep::Container(Some((namespace, step))) => TeardownStep::Container {
                namespace,
                step: *step,
            },
            HeldStep::Container(None) => return Err(refused(unsigned)),
            HeldStep::Volume(name) => TeardownStep::Volume(name),
            HeldStep::Leave => TeardownStep::Leave,
            HeldStep::Forget(forgotten) => TeardownStep::Forget(forgotten),
        };
        let record = machine.record();
        let id = record.id();
        let approved = teardown::verify(
            approval,
            &TeardownFacts {
                cluster: machine.cluster_key(record.phase()),
                machine: &id,
                trusted: &record.trusted_keys(),
                step,
                now: Timestamp::now(),
            },
        )
        .map_err(refused)?;
        if admitting {
            match approved.preview_rule {
                Some(rule) => {
                    tracing::info!(by = %approved.by, %rule, "preview closed under its rule");
                }
                None => tracing::info!(by = %approved.by, "teardown approved"),
            }
        }
        Ok(())
    }
}
