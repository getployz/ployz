//! Local ownership of Machine upgrade replay, admission, and durable inspection.

use ployz_core::{
    InspectMachineUpgradeRequest, MachineUpgradeAttempt, RequestMachineUpgradeRequest,
};

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
    /// failure, receipt failure, or worker launch uncertainty. Returns [`Error::LockPoisoned`]
    /// when the Local Machine store cannot be read.
    pub(crate) async fn request_upgrade(
        &self,
        request: RequestMachineUpgradeRequest,
    ) -> Result<MachineUpgradeAttempt, Error> {
        let (data_dir, run_dir, local, gate) = {
            let store = self.lock_store()?;
            (
                store.data_dir.clone(),
                store.run_dir.clone(),
                store.admission_lock.clone(),
                store.mutation_gate.clone(),
            )
        };
        if let Some(attempt) = crate::installer::upgrade::existing_request(&request, &data_dir)? {
            return Ok(attempt);
        }
        let _local = local
            .try_lock_owned()
            .map_err(|_| crate::mutation::Error::Busy)?;
        let installation = gate.try_installation()?;
        crate::installer::require_standard_machine_paths(&data_dir, &run_dir.join("ployz.sock"))
            .map_err(crate::installer::upgrade::Error::NonstandardPaths)?;
        crate::installer::upgrade::request(request, data_dir, run_dir, installation)
            .await
            .map_err(Into::into)
    }

    /// Read one durable Machine upgrade attempt and reconcile positively stopped work.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] when no matching receipt exists or worker and ownership
    /// evidence cannot be read. Returns [`Error::LockPoisoned`] when the Local Machine store
    /// cannot be read.
    pub(crate) async fn inspect_upgrade(
        &self,
        request: InspectMachineUpgradeRequest,
    ) -> Result<MachineUpgradeAttempt, Error> {
        let (data_dir, run_dir) = {
            let store = self.lock_store()?;
            (store.data_dir.clone(), store.run_dir.clone())
        };
        crate::installer::upgrade::inspect(request.attempt_id, &data_dir, &run_dir)
            .await
            .map_err(Into::into)
    }
}
