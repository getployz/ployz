//! Teardowns under Device enforcement: an Operator's step that starts nothing, Volume
//! removal, `Leave` or `Forget` runs only under a person's teardown that names this Server
//! and what goes, or, for a preview's close, the rule that made it.
//!
//! A teardown is checked beside a deploy approval, never inside it: a want that carries a
//! deploy approval, a restart approval or a rule's proof goes their way, and one carrying
//! only a teardown comes here. Nothing is consumed, so a resend after a crash finishes
//! under the same statement while its window lasts.

use ployz_core::{
    ContainerNow, ContainerWant, DockerVolumeName, MachineId,
    authority::{
        AuthorityRefusal, Purpose, RestartStep, TeardownApproval, TeardownFacts, TeardownStep,
        Timestamp, teardown,
    },
};

use super::{Error, LocalMachine, restart_approval::pinned};

impl LocalMachine {
    /// Admit an Operator's step on an existing Container under `approval`: one that starts
    /// nothing, in a Namespace the teardown names, on this Server. Returns the want pinned
    /// to the Container seen. A Machine not enforcing passes untouched.
    ///
    /// # Errors
    /// Returns [`Error::TeardownRefused`] for a creation, a start, or a step the teardown
    /// does not name, and [`Error::DockerUnavailable`] or [`Error::Docker`] when the
    /// Container cannot be read.
    pub(crate) async fn admit_teardown_step(
        &self,
        want: ContainerWant,
        approval: &TeardownApproval,
    ) -> Result<ContainerWant, Error> {
        if !self.record().device_enforcement() {
            return Ok(want);
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
        self.admit_teardown(
            approval,
            TeardownStep::Container {
                namespace: &identity.namespace,
                step,
            },
        )
        .map_err(Error::TeardownRefused)?;
        tracing::info!(?step, "teardown step on {identity} admitted");
        Ok(pinned(want, observed.container_id))
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
        approval: Option<&TeardownApproval>,
    ) -> Result<(), Error> {
        if !self.record().device_enforcement() {
            return Ok(());
        }
        let approval = approval.ok_or(Error::TeardownRefused(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        }))?;
        self.admit_teardown(approval, TeardownStep::Volume(name))
            .map_err(Error::TeardownRefused)?;
        tracing::info!("teardown removal of Volume {name} admitted");
        Ok(())
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
        approval: Option<&TeardownApproval>,
    ) -> Result<(), Error> {
        if !self.record().device_enforcement() {
            return Ok(());
        }
        let (capability, step) = match forgotten {
            None => (ployz_core::LEAVE_MACHINE_CAPABILITY, TeardownStep::Leave),
            Some(machine) => (
                ployz_core::FORGET_MACHINE_CAPABILITY,
                TeardownStep::Forget(machine),
            ),
        };
        let refused = |refusal| Error::DepartureRefused {
            capability,
            refusal,
        };
        let approval = approval.ok_or_else(|| {
            refused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Workflow,
            })
        })?;
        self.admit_teardown(approval, step).map_err(refused)?;
        tracing::info!(?step, "teardown departure admitted");
        Ok(())
    }

    fn admit_teardown(
        &self,
        approval: &TeardownApproval,
        step: TeardownStep<'_>,
    ) -> Result<(), AuthorityRefusal> {
        let record = self.record();
        let machine = record.id();
        let approved = teardown::verify(
            approval,
            &TeardownFacts {
                cluster: self.cluster_key(record.phase()),
                machine: &machine,
                trusted: &record.trusted_keys(),
                step,
                now: Timestamp::now(),
            },
        )
        .inspect_err(|refusal| tracing::warn!(?step, "teardown refused: {refusal}"))?;
        match approved.preview_rule {
            Some(rule) => tracing::info!(by = %approved.by, %rule, "preview closed under its rule"),
            None => tracing::info!(by = %approved.by, "teardown approved"),
        }
        Ok(())
    }
}
