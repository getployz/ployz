use ployz_model::{MachineLifecycle, MachineMembership};
use ployz_store_api::MachineMembershipStore;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(in crate::daemon::handlers::volume::zfs) async fn find_machine(
        &self,
        machine: &str,
    ) -> Option<MachineMembership> {
        let active = self.active.as_ref()?;
        let machines = active.mesh.store.list_machines().await.ok()?;
        machines
            .into_iter()
            .find(|record| record.id.as_str() == machine)
    }

    pub(in crate::daemon::handlers::volume::zfs) async fn find_active_machine(
        &self,
        machine: &str,
    ) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if record.lifecycle != MachineLifecycle::Active {
            return Err(format!(
                "machine '{}' is {}, expected active",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }

    pub(in crate::daemon::handlers::volume::zfs) async fn find_volume_move_source_machine(
        &self,
        machine: &str,
    ) -> Result<MachineMembership, String> {
        let record = self
            .find_machine(machine)
            .await
            .ok_or_else(|| format!("machine '{machine}' not found"))?;
        if !matches!(
            record.lifecycle,
            MachineLifecycle::Active | MachineLifecycle::Draining
        ) {
            return Err(format!(
                "machine '{}' is {}, expected active or draining",
                record.id, record.lifecycle
            ));
        }
        Ok(record)
    }
}
