//! Let go of the copies a Drain's move left Out on the drained Server.

use ployz_core::error_chain;
use ployz_core::{AnonymousVolumes, MachineId};
use tokio_util::sync::CancellationToken;

use super::placement::{MachineRef, OldCopyEnd, Unconfirmed};
use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::{self, Copy};

pub(crate) async fn let_go<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> OldCopyEnd {
    let unlisted = cleanup::wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(cleanup::Waited::failure);
    let ended = match unlisted {
        Ok(Ok(())) => cleanup::remove(client, copy, AnonymousVolumes::Remove)
            .await
            .map(cleanup::Removal::failure),
        unlisted => unlisted,
    };
    let unconfirmed = match ended {
        Ok(Ok(())) => return OldCopyEnd::Stopped,
        Ok(Err(failure)) => Unconfirmed::of(failure, name),
        Err(error) => Unconfirmed::Failed {
            detail: error_chain::row(&error),
        },
    };
    OldCopyEnd::Running { unconfirmed }
}

#[cfg(test)]
#[path = "retirement_tests.rs"]
mod tests;
