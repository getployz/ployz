//! A peer image pull: open a Machine's image ingest, then have another Machine pull from it.

use ployz_core::{
    EnsureImageIngestRequest, ImageIngestDestination, MachineId, MachineTarget, PeerImagePull,
    PullImageFromMachineRequest, op,
};

use crate::{
    cluster::Call,
    connect::{Client, ConnectError},
};

/// A Machine whose image ingest is open, so another Machine can pull from it.
#[derive(Debug)]
pub(super) struct PeerSource {
    pub(super) machine: MachineId,
    pub(super) destination: ImageIngestDestination,
}

impl PeerSource {
    /// Open `machine`'s image ingest.
    ///
    /// # Errors
    /// The call's failure, for the caller to name its Machine and step.
    pub(super) async fn open(
        client: &mut Client,
        machine: MachineId,
    ) -> Result<Self, ConnectError> {
        let opened = client
            .call::<op::EnsureImageIngest>(
                EnsureImageIngestRequest {},
                Call::keyed().to(&MachineTarget::from(&machine)),
            )
            .await?;
        Ok(Self {
            machine,
            destination: opened.destination,
        })
    }

    /// Have `dest` pull `pull` from this Machine, taking `platform`.
    ///
    /// # Errors
    /// The call's failure, for the caller to name its Machine and step.
    pub(super) async fn pull_into(
        &self,
        client: &mut Client,
        dest: &MachineId,
        pull: PeerImagePull,
        platform: &str,
    ) -> Result<(), ConnectError> {
        client
            .call::<op::PullImageFromMachine>(
                PullImageFromMachineRequest {
                    pull,
                    source: self.destination,
                    platform: platform.to_owned(),
                },
                Call::keyed().to(&MachineTarget::from(dest)),
            )
            .await
            .map(drop)
    }
}
