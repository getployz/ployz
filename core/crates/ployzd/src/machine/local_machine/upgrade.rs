//! Local ownership of Machine upgrade replay and admission.

use ployz_core::{MachineUpgradeAttempt, RequestMachineUpgradeRequest};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Resolve, durably accept, and launch one retry-safe Machine upgrade.
    ///
    /// A replay is read before claiming exclusive mutation ownership so a lost reply can be
    /// recovered while the independent worker owns the installation lock.
    ///
    /// `cloud_routed` marks a request that entered the mesh through Ployz Cloud on any
    /// Machine; such a request, like one on this Machine's own tunnel, cannot downgrade.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] for a conflicting retry identity, active mutation, release
    /// failure, receipt failure, or worker launch uncertainty.
    pub(crate) async fn request_upgrade(
        &self,
        request: RequestMachineUpgradeRequest,
        cloud_routed: bool,
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
        crate::installer::upgrade::request(
            request,
            self.downgrade(cloud_routed),
            data_dir,
            run_dir,
            installation,
        )
        .await
        .map_err(Into::into)
    }

    fn downgrade(&self, cloud_routed: bool) -> crate::installer::upgrade::Downgrade {
        if cloud_routed || self.management_client.is_some() {
            crate::installer::upgrade::Downgrade::Refused
        } else {
            crate::installer::upgrade::Downgrade::Allowed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        installer::upgrade::Downgrade,
        machine::{LocalMachineStore, RecordOwner},
    };

    #[test]
    fn only_a_request_that_never_touched_cloud_may_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        let tunnel = local.clone().with_management_client([7; 32]);

        assert_eq!(local.downgrade(false), Downgrade::Allowed);
        assert_eq!(local.downgrade(true), Downgrade::Refused);
        assert_eq!(tunnel.downgrade(false), Downgrade::Refused);
        assert_eq!(tunnel.downgrade(true), Downgrade::Refused);
    }
}
