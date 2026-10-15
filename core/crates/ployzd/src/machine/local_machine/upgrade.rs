//! Local ownership of Machine upgrade replay and admission.

use ployz_core::{MachineUpgradeAttempt, RequestMachineUpgradeRequest};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Resolve, durably accept, and launch one retry-safe Machine upgrade.
    ///
    /// A replay is read before claiming exclusive mutation ownership so a lost reply can be
    /// recovered while the independent worker owns the installation lock.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] for a conflicting retry identity, active mutation, release
    /// failure, receipt failure, or worker launch uncertainty.
    pub(crate) async fn request_upgrade(
        &self,
        request: RequestMachineUpgradeRequest,
    ) -> Result<MachineUpgradeAttempt, Error> {
        let data_dir = self.owner.data_dir().to_owned();
        let run_dir = self.owner.run_dir().to_owned();
        if let Some(attempt) =
            crate::installer::upgrade::replay(&request, &data_dir, &run_dir).await?
        {
            return Ok(attempt);
        }
        let _local = self
            .owner
            .admission_lock()
            .try_write_owned()
            .map_err(|_| crate::mutation::Error::Busy)?;
        self.require_management_access()?;
        let installation = self.owner.mutation_gate().try_installation()?;
        crate::installer::require_standard_machine_paths(&data_dir, &run_dir.join("ployz.sock"))
            .map_err(crate::installer::upgrade::Error::NonstandardPaths)?;
        let downgrade = if self.management_client.is_some() {
            crate::installer::upgrade::Downgrade::Refused
        } else {
            crate::installer::upgrade::Downgrade::Allowed
        };
        crate::installer::upgrade::request(request, downgrade, data_dir, run_dir, installation)
            .await
            .map_err(Into::into)
    }
}
