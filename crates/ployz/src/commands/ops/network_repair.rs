pub(super) const fn network_repair_state(
    state: &ployz_sdk_types::NetworkRepairOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::NetworkRepairOperationState::Accepted => "accepted",
        ployz_sdk_types::NetworkRepairOperationState::Running { stage } => match stage {
            ployz_sdk_types::NetworkRepairRunningStage::PreparingDataplane => {
                "running:preparing-dataplane"
            }
        },
        ployz_sdk_types::NetworkRepairOperationState::Completed => "completed",
        ployz_sdk_types::NetworkRepairOperationState::Failed { .. } => "failed",
        ployz_sdk_types::NetworkRepairOperationState::Cancelled { .. } => "cancelled",
    }
}

pub(super) fn render_network_repair_failure(
    failure: &ployz_sdk_types::NetworkRepairFailure,
) -> String {
    match failure {
        ployz_sdk_types::NetworkRepairFailure::NoActiveMachines => "no-active-machines".to_owned(),
        ployz_sdk_types::NetworkRepairFailure::IntentReadFailed { message } => {
            format!("intent-read-failed message={}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairFailure::DataplaneConvergenceFailed {
            machine_id,
            component,
            message,
        } => format!(
            "dataplane-convergence-failed machine={} component={} message={}",
            machine_id.as_str(),
            match component {
                ployz_sdk_types::PloyzNativeMeshComponent::WireGuard => "wireguard",
                ployz_sdk_types::PloyzNativeMeshComponent::EbpfForwarding => "ebpf-forwarding",
            },
            message.as_str(),
        ),
        ployz_sdk_types::NetworkRepairFailure::DataplaneReportInvalid { message } => {
            format!("dataplane-report-invalid message={}", message.as_str())
        }
    }
}
