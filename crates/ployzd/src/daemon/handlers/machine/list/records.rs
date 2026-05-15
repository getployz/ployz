use ployz_model::{MachineId, MachineMembership};
use ployz_store_api::{MachineMembershipStore, StoreDriver};

pub(in crate::daemon::handlers::machine) async fn find_machine_record(
    store: &StoreDriver,
    machine_id: &MachineId,
) -> Result<Option<MachineMembership>, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("{err}"))?;
    Ok(machines
        .into_iter()
        .find(|machine| machine.id == *machine_id))
}
