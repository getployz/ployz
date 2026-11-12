//! An enforcing Machine runs an Operator's `SetVolumeSize` or `SeedVolume` only under a
//! Device's Resize or Seed approval that names this Machine and exactly what the request
//! asks: the size and whether it may shrink, or the source Volume, its snapshot and the new
//! Volume. The operation stays open until the step lands here, so a resend while it copies
//! or after a crash still runs. Once it lands the operation ends, so the approval never runs
//! again, and a resend of that same approval is answered without running anything.

use ployz_core::{
    DockerVolumeName, OperationId, SeedVolumeRequest, SetVolumeSizeRequest,
    authority::{AuthorityRefusal, Purpose, Signed, StepDigest, VolumeApproval, VolumeStep},
};

use super::{Error, LocalMachine};

/// What an admitted resize or seed does on this Machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Landing {
    /// Run the step. Once it lands, end `operation`. `None` when nothing was admitted: a
    /// Full caller, or a Machine that doesn't enforce.
    Run(Option<OperationId>),
    /// The step already landed here under this approval, and its operation ended. Answer
    /// what the step left without running it again.
    Landed,
}

impl LocalMachine {
    /// Under Device enforcement, admit an Operator's `SetVolumeSize` against the Resize
    /// approval it carries.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] with no approval, or one that names another
    /// Machine, Volume, size or direction, and [`Error::ApprovalRecord`] when the record
    /// cannot be read or written.
    pub(crate) fn admit_operator_resize(
        &self,
        request: &SetVolumeSizeRequest,
    ) -> Result<Landing, Error> {
        self.admit_landing(
            request.approval.as_ref(),
            &request.name,
            &VolumeStep::Resize {
                maximum_bytes: request.maximum_bytes,
                allow_shrink: request.allow_shrink,
            },
        )
    }

    /// Under Device enforcement, admit an Operator's `SeedVolume` against the Seed approval
    /// it carries. The approval's Volume is the one the seed creates, `into`.
    ///
    /// # Errors
    /// Returns [`Error::VolumeStepRefused`] with no approval, or one that names another
    /// Machine, source, snapshot or new Volume, and [`Error::ApprovalRecord`] when the record
    /// cannot be read or written.
    pub(crate) fn admit_operator_seed(
        &self,
        request: &SeedVolumeRequest,
    ) -> Result<Landing, Error> {
        self.admit_landing(
            request.approval.as_ref(),
            &request.into,
            &VolumeStep::Seed {
                source: request.name.clone(),
                at: request.guid,
            },
        )
    }

    fn admit_landing(
        &self,
        approval: Option<&Signed<VolumeApproval>>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
    ) -> Result<Landing, Error> {
        if !self.record().device_enforcement() {
            return Ok(Landing::Run(None));
        }
        let Some(approval) = approval else {
            return Err(Error::VolumeStepRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Volume,
            }));
        };
        match self.admit_volume_step(approval, volume, step) {
            Ok(operation) => Ok(Landing::Run(Some(operation))),
            Err(Error::VolumeStepRefused(AuthorityRefusal::Replayed { operation }))
                if self.landed(approval, volume, step)? =>
            {
                tracing::info!(%operation, %volume, ?step, "Volume step already landed");
                Ok(Landing::Landed)
            }
            Err(error) => Err(error),
        }
    }

    /// Whether `approval` is the very body this Machine admitted and ended once its step
    /// landed, and it permits `step` on `volume` here. Its signature checked out when it was
    /// admitted, so an identical body needs no second check to answer a read.
    fn landed(
        &self,
        approval: &Signed<VolumeApproval>,
        volume: &DockerVolumeName,
        step: &VolumeStep,
    ) -> Result<bool, Error> {
        let body = approval.open().map_err(Error::VolumeStepRefused)?;
        if body.volume != *volume || !body.workflow.permits(&self.record().id(), step) {
            return Ok(false);
        }
        let digest = StepDigest::of(&approval.body);
        self.approvals
            .read_volumes(|records| records.ended_as(body.operation, digest))
    }
}
