//! Admission for operations that exclusively mutate one machine's substrate.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use ployz_core::deploy::ZfsPoolName;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use tokio::sync::Mutex;

use super::{OperationControllers, SubmitCommandError};
use crate::control::operation_evidence::{
    AcceptedMachineStoragePrepareSubmission, AcceptedMachineUpdateSubmission,
    MachineStoragePrepareOperationSubmission, MachineUpdateOperationSubmission,
    SubmitOperationError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateSubmitCommand {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineStoragePrepareSubmitCommand {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub requested_pool: Option<ZfsPoolName>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct MachineSubstrateLocks {
    owners: Arc<Mutex<BTreeMap<MachineId, OperationId>>>,
}

impl OperationControllers {
    pub async fn submit_machine_update(
        &self,
        command: MachineUpdateSubmitCommand,
    ) -> Result<AcceptedMachineUpdateSubmission, SubmitCommandError> {
        let machine_id = command.machine_id.clone();
        let operation_id = command.operation_id.clone();
        let submitted = self
            .repository
            .submit_machine_update(MachineUpdateOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                target_version: command.target_version,
            });
        self.submit_with_machine_substrate_lock(machine_id, operation_id, submitted, |accepted| {
            accepted.should_start_execution
        })
        .await
    }

    pub async fn submit_machine_storage_prepare(
        &self,
        command: MachineStoragePrepareSubmitCommand,
    ) -> Result<AcceptedMachineStoragePrepareSubmission, SubmitCommandError> {
        let machine_id = command.machine_id.clone();
        let operation_id = command.operation_id.clone();
        let submitted = self.repository.submit_machine_storage_prepare(
            MachineStoragePrepareOperationSubmission {
                operation_id: command.operation_id,
                machine_id: command.machine_id,
                requested_pool: command.requested_pool,
            },
        );
        self.submit_with_machine_substrate_lock(machine_id, operation_id, submitted, |accepted| {
            accepted.should_start_execution
        })
        .await
    }

    async fn submit_with_machine_substrate_lock<A, F>(
        &self,
        machine_id: MachineId,
        operation_id: OperationId,
        submitted: F,
        should_start_execution: impl FnOnce(&A) -> bool,
    ) -> Result<A, SubmitCommandError>
    where
        F: Future<Output = Result<A, SubmitOperationError>>,
    {
        let claim = self
            .claim_machine_substrate(&machine_id, &operation_id)
            .await;
        if let MachineSubstrateClaim::Busy { owner } = claim {
            return Err(SubmitCommandError::MachineSubstrateBusy { machine_id, owner });
        }

        let result = submitted.await.map_err(SubmitCommandError::Submit);
        let release = match &result {
            Ok(accepted) => !should_start_execution(accepted),
            Err(_) => true,
        };
        if release && matches!(claim, MachineSubstrateClaim::Acquired) {
            self.release_machine(&machine_id, &operation_id).await;
        }
        result
    }

    async fn claim_machine_substrate(
        &self,
        machine_id: &MachineId,
        operation_id: &OperationId,
    ) -> MachineSubstrateClaim {
        let mut owners = self.machine_substrate_locks.owners.lock().await;
        match owners.get(machine_id) {
            Some(owner) if owner == operation_id => MachineSubstrateClaim::AlreadyOwned,
            Some(owner) => MachineSubstrateClaim::Busy {
                owner: owner.clone(),
            },
            None => {
                owners.insert(machine_id.clone(), operation_id.clone());
                MachineSubstrateClaim::Acquired
            }
        }
    }

    pub async fn release_machine(&self, machine_id: &MachineId, operation_id: &OperationId) {
        let mut owners = self.machine_substrate_locks.owners.lock().await;
        if owners.get(machine_id) == Some(operation_id) {
            owners.remove(machine_id);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineSubstrateClaim {
    Acquired,
    AlreadyOwned,
    Busy { owner: OperationId },
}
