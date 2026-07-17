//! Control-owned mutating operation execution.

use ployz_core::ids::OperationId;

use crate::tasks::TaskAdmissionError;

pub mod build;
pub mod credential_grant;
pub mod dataplane_projection_admission;
pub mod deploy;
pub mod ingress_configure;
pub(crate) mod local_execution_admission;
pub mod machine_lifecycle;
pub mod machine_storage_prepare;
pub mod machine_update;
pub mod namespace_remove;
pub mod network_repair;
pub mod service_restart;
pub mod volume_create;
pub mod volume_remove;

pub(super) async fn finish_rejected_task_admission(
    controllers: &crate::control::sequencer::OperationControllers,
    operation_id: &OperationId,
    admission: Result<(), TaskAdmissionError>,
) {
    if let Err(error) = admission
        && let Err(record_error) = controllers
            .repository()
            .record_interrupted_operation(
                operation_id,
                ployz_core::operation::OperationInterruptionCause::CoreShutdown,
            )
            .await
    {
        eprintln!(
            "operation {} task admission failed ({error}) and interruption evidence could not be recorded: {record_error}",
            operation_id.as_str()
        );
    }
}
