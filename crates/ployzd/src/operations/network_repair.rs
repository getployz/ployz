//! Operation-owned cluster dataplane repair.

use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::deploy::DataplanePreparer;
use crate::operations::log::{AcceptedNetworkRepairSubmission, RecordOperationEventError};
use crate::roles::machine::client::NatsMachineDataplanePreparer;
use crate::tasks::TaskRegistry;
use ployz_core::dataplane::{
    DataplaneMember, DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    FailureMessage, NetworkRepairEvidence, NetworkRepairFailure, NetworkRepairRunningStage,
    NetworkRepairTransition,
};

#[derive(Debug, Clone)]
pub struct NetworkRepairOperation {
    controllers: OperationControllers,
    intent_reader: NatsIntentReader,
    dataplane: NatsMachineDataplanePreparer,
    task_registry: TaskRegistry,
}

impl NetworkRepairOperation {
    #[must_use]
    pub const fn new(
        controllers: OperationControllers,
        intent_reader: NatsIntentReader,
        dataplane: NatsMachineDataplanePreparer,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            controllers,
            intent_reader,
            dataplane,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedNetworkRepairSubmission) {
        if !accepted.should_start_execution {
            return;
        }
        let runtime = self.clone();
        self.task_registry.spawn(async move {
            runtime.run(accepted).await;
        });
    }

    pub async fn run(mut self, accepted: AcceptedNetworkRepairSubmission) {
        let operation_id = accepted.operation_id;
        if let Err(error) = self
            .controllers
            .repository()
            .record_network_repair_transition(
                &operation_id,
                NetworkRepairTransition::Running {
                    stage: NetworkRepairRunningStage::PreparingDataplane,
                },
            )
            .await
        {
            record_warning(&operation_id, "record-running", &error);
            return;
        }
        let intent = match self.intent_reader.intent().await {
            Ok(intent) => intent,
            Err(error) => {
                self.record_terminal(
                    &operation_id,
                    NetworkRepairTransition::Failed {
                        failure: NetworkRepairFailure::IntentReadFailed {
                            message: failure_message(error.to_string()),
                        },
                    },
                )
                .await;
                return;
            }
        };
        let membership = intent
            .active_machines
            .into_iter()
            .map(|machine| DataplaneMember {
                machine_id: machine.machine_id,
                endpoint_subnet: machine.endpoint_subnet,
            })
            .collect::<Vec<_>>();
        if membership.is_empty() {
            self.record_terminal(
                &operation_id,
                NetworkRepairTransition::Failed {
                    failure: NetworkRepairFailure::NoActiveMachines,
                },
            )
            .await;
            return;
        }
        let request = DataplanePrepareRequest {
            operation_id: operation_id.clone(),
            membership,
        };
        let transition = match self.dataplane.prepare_dataplane(request).await {
            Ok(report) => {
                if let Err(error) = self
                    .controllers
                    .repository()
                    .record_network_repair_evidence(
                        &operation_id,
                        NetworkRepairEvidence::DataplanePrepared { report },
                    )
                    .await
                {
                    record_warning(&operation_id, "record-dataplane-prepared", &error);
                    return;
                }
                NetworkRepairTransition::Completed
            }
            Err(error) => NetworkRepairTransition::Failed {
                failure: network_repair_failure(error),
            },
        };
        self.record_terminal(&operation_id, transition).await;
    }

    async fn record_terminal(
        &self,
        operation_id: &OperationId,
        transition: NetworkRepairTransition,
    ) {
        if let Err(error) = self
            .controllers
            .repository()
            .record_network_repair_transition(operation_id, transition)
            .await
        {
            record_warning(operation_id, "record-terminal", &error);
        }
    }
}

fn network_repair_failure(error: DataplanePrepareError) -> NetworkRepairFailure {
    match error {
        DataplanePrepareError::Unavailable {
            machine_id,
            provider: DataplaneProviderFailure::PloyzNativeMesh { component },
            message,
        } => NetworkRepairFailure::DataplaneConvergenceFailed {
            machine_id,
            component,
            message,
        },
        DataplanePrepareError::InvalidReport { message } => {
            NetworkRepairFailure::DataplaneReportInvalid { message }
        }
    }
}

fn failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("rendered operation failure is non-empty")
}

fn record_warning(operation_id: &OperationId, phase: &str, error: &RecordOperationEventError) {
    eprintln!(
        "ployzd network repair warning: phase={phase} operation_id={} error={error}",
        operation_id.as_str()
    );
}
