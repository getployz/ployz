use futures_util::future::join_all;
use ployz_core::dataplane::MachineDataplaneStatus;
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;

use super::client::NatsMachineFactsReader;

pub(crate) async fn gather_dataplane_statuses<'a>(
    reader: &NatsMachineFactsReader,
    machine_ids: impl IntoIterator<Item = &'a MachineId>,
) -> Vec<(MachineId, Result<MachineDataplaneStatus, FailureMessage>)> {
    join_all(machine_ids.into_iter().map(|machine_id| async move {
        (
            machine_id.clone(),
            reader.read_dataplane_status(machine_id).await,
        )
    }))
    .await
}
