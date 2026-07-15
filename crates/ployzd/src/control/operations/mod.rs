//! Control-owned mutating operation execution.

use ployz_core::ids::OperationId;

use crate::tasks::TaskAdmissionError;

pub mod credential_grant;
pub mod dataplane_projection_admission;
pub mod deploy;
pub mod ingress_configure;
pub mod machine_lifecycle;
pub mod machine_update;
pub mod namespace_remove;
pub mod network_repair;
pub mod service_restart;
pub mod volume_remove;

pub(super) fn report_task_admission(
    operation_id: &OperationId,
    admission: Result<(), TaskAdmissionError>,
) {
    if let Err(error) = admission {
        eprintln!(
            "operation {} was accepted while control tasks were quiescing: {error}",
            operation_id.as_str()
        );
    }
}
