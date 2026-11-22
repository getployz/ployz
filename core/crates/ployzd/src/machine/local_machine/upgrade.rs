//! Local ownership of Machine upgrade replay and admission.

use ployz_core::{MachineUpgradeAttempt, RequestMachineUpgradeRequest};

use super::{Caller, Error, LocalMachine, Scope};
use crate::installer::upgrade::ReleaseChoice;

impl LocalMachine {
    /// Resolve, durably accept, and launch one retry-safe Machine upgrade.
    ///
    /// A replay is read before claiming exclusive mutation ownership so a lost reply can be
    /// recovered while the independent worker owns the installation lock.
    ///
    /// A request that entered the mesh through Ployz Cloud on any Machine, like one on this
    /// Machine's own tunnel, cannot downgrade.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] for a conflicting retry identity, active mutation, release
    /// failure, an Operator's release off both channels, receipt failure, or worker launch
    /// uncertainty.
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
        let choice = self.release_choice();
        let installation = self.owner.mutation_gate().try_installation()?;
        crate::installer::require_standard_machine_paths(&data_dir, &run_dir.join("ployz.sock"))
            .map_err(crate::installer::upgrade::Error::NonstandardPaths)?;
        crate::installer::upgrade::request(
            request,
            self.downgrade(),
            choice,
            data_dir,
            run_dir,
            installation,
        )
        .await
        .map_err(Into::into)
    }

    fn downgrade(&self) -> crate::installer::upgrade::Downgrade {
        if self.call.cloud_routed() || matches!(self.call.caller(), Caller::Management(_)) {
            crate::installer::upgrade::Downgrade::Refused
        } else {
            crate::installer::upgrade::Downgrade::Allowed
        }
    }

    /// The releases this call may name: a Full caller any published release, an Operator
    /// a Release Channel or what one resolves to now.
    fn release_choice(&self) -> ReleaseChoice {
        match self.scope() {
            Scope::Full(_) => ReleaseChoice::AnyRelease,
            Scope::Operator => ReleaseChoice::ChannelResolved,
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::ManagementScope;

    use super::*;
    use crate::{
        installer::upgrade::Downgrade,
        machine::{Call, LocalMachineStore, RecordOwner},
    };

    #[test]
    fn only_a_request_that_never_touched_cloud_may_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        let tunnel = Caller::Management(ployz_core::ClientKey::from_bytes([7; 32]));
        let at = |caller, cloud_routed| {
            local
                .clone()
                .at(Call::new(caller, ManagementScope::Full, cloud_routed))
                .downgrade()
        };

        assert_eq!(at(Caller::LocalSocket, false), Downgrade::Allowed);
        assert_eq!(at(Caller::LocalSocket, true), Downgrade::Refused);
        assert_eq!(at(tunnel, false), Downgrade::Refused);
        assert_eq!(at(tunnel, true), Downgrade::Refused);
    }
}
