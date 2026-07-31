//! Control-owned mutating operation execution.

use std::time::Duration;

use ployz_core::ids::OperationId;
use ployz_nats::subjects::INTENT_CHANGED;

use crate::tasks::TaskAdmissionError;

const INTENT_CHANGED_PUBLISH_TIMEOUT: Duration = Duration::from_secs(2);

/// Best-effort intent-changed poke after a committed intent write. Fanout
/// only — a missed poke is repaired by the drumbeat rebroadcast, so a
/// failure never fails the operation — but it is still evidence worth a
/// warning, and the publish is bounded so no commit path can hang on a
/// slow client. `after` names the commit the poke follows.
pub(crate) async fn publish_intent_changed(client: &async_nats::Client, after: &'static str) {
    let publish = client.publish(INTENT_CHANGED, Vec::new().into());
    match tokio::time::timeout(INTENT_CHANGED_PUBLISH_TIMEOUT, publish).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::warn!(after, error = %error, "intent-changed publish failed");
        }
        Err(_) => {
            tracing::warn!(after, "intent-changed publish timed out");
        }
    }
}

pub mod build;
pub mod credential_grant;
pub mod dataplane_projection_admission;
pub mod deploy;
pub mod ingress_configure;
pub(crate) mod local_execution_admission;
pub mod machine_build_cache_prune;
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
        tracing::error!(
            operation_id = operation_id.as_str(),
            admission_error = %error,
            error = %record_error,
            "task admission failed and interruption evidence could not be recorded"
        );
    }
}
