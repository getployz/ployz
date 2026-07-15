//! Operation-owned machine substrate updates.

use crate::control::operation_evidence::{
    AcceptedMachineUpdateSubmission, RecordOperationEventError,
};
use crate::operation_api::admission::OperationControllers;
use crate::roles::machine::client::NatsMachineSubstrateUpdater;
use crate::roles::machine::protocol::MachineSubstrateUpdateRpcRequest;
use crate::tasks::TaskRegistry;
use ployz_core::ids::MachineId;
use ployz_core::install::InstallArtifactVersion;
use ployz_core::ops::MachineSubstrateVersions;
use ployz_core::ops::{FailureMessage, MachineUpdateFailure, MachineUpdateTransition};
use std::time::{Duration, Instant};

const UPDATE_REPORT_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_REPORT_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct MachineUpdateOperation {
    controllers: OperationControllers,
    updater: NatsMachineSubstrateUpdater,
    task_registry: TaskRegistry,
}

impl MachineUpdateOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        updater: NatsMachineSubstrateUpdater,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            updater,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedMachineUpdateSubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(self, accepted: AcceptedMachineUpdateSubmission) {
        let operation_id = accepted.operation_id;
        let machine_id = accepted.machine_id;
        let target_version = accepted.target_version;

        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_update_transition(
                &operation_id,
                &machine_id,
                MachineUpdateTransition::Running,
            )
            .await
        {
            self.record_failed(
                &operation_id,
                &machine_id,
                MachineUpdateFailure::StateCommitFailed {
                    machine_id: machine_id.clone(),
                    message: machine_update_event_failure(error),
                },
            )
            .await;
            return;
        }
        let result = self
            .updater
            .update_substrate(
                &machine_id,
                MachineSubstrateUpdateRpcRequest {
                    operation_id: operation_id.clone(),
                    target_version: target_version.clone(),
                },
            )
            .await;

        match result {
            Ok(()) => {
                let reported = match self
                    .wait_for_target_report(&operation_id, &machine_id, &target_version)
                    .await
                {
                    Ok(reported) => reported,
                    Err(failure) => {
                        self.record_failed(&operation_id, &machine_id, failure)
                            .await;
                        return;
                    }
                };

                if let Err(error) = self
                    .controllers
                    .repository()
                    .record_machine_update_transition(
                        &operation_id,
                        &machine_id,
                        MachineUpdateTransition::Completed {
                            reported: reported.clone(),
                        },
                    )
                    .await
                {
                    self.record_failed(
                        &operation_id,
                        &machine_id,
                        MachineUpdateFailure::StateCommitFailed {
                            machine_id: machine_id.clone(),
                            message: machine_update_event_failure(error),
                        },
                    )
                    .await;
                }
            }
            Err(error) => {
                self.record_failed(&operation_id, &machine_id, error.into_operation_failure())
                    .await;
            }
        }
    }

    async fn record_failed(
        &self,
        operation_id: &ployz_core::ids::OperationId,
        machine_id: &MachineId,
        failure: MachineUpdateFailure,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_machine_update_transition(
                operation_id,
                machine_id,
                MachineUpdateTransition::Failed { failure },
            )
            .await
        {
            eprintln!(
                "ployzd machine update warning: phase=record-failed operation_id={} machine_id={} error={error}",
                operation_id.as_str(),
                machine_id.as_str()
            );
        }
    }

    async fn wait_for_target_report(
        &self,
        operation_id: &ployz_core::ids::OperationId,
        machine_id: &MachineId,
        target_version: &InstallArtifactVersion,
    ) -> Result<MachineSubstrateVersions, MachineUpdateFailure> {
        let deadline = Instant::now() + UPDATE_REPORT_TIMEOUT;
        let mut last_reported = MachineSubstrateVersions::default();
        loop {
            match self
                .updater
                .report_substrate_versions(machine_id, operation_id)
                .await
            {
                Ok(reported) if reported.ployzd.as_ref() == Some(target_version) => {
                    return Ok(reported);
                }
                Ok(reported) => {
                    last_reported = reported;
                }
                Err(_error) if Instant::now() < deadline => {}
                Err(error) => return Err(error.into_operation_failure()),
            }
            if Instant::now() >= deadline {
                return Err(MachineUpdateFailure::VersionNotReported {
                    machine_id: machine_id.clone(),
                    target_version: target_version.clone(),
                    reported: last_reported,
                });
            }
            tokio::time::sleep(UPDATE_REPORT_POLL_INTERVAL).await;
        }
    }
}

fn machine_update_event_failure(error: RecordOperationEventError) -> FailureMessage {
    FailureMessage::try_new(format!(
        "failed to record machine-update completed event: {error}"
    ))
    .expect("event failure message is non-empty")
}
