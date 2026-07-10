pub(super) const fn network_repair_state(
    state: &ployz_sdk_types::NetworkRepairOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::NetworkRepairOperationState::Accepted => "accepted",
        ployz_sdk_types::NetworkRepairOperationState::Running { stage } => match stage {
            ployz_sdk_types::NetworkRepairRunningStage::PreparingDataplane => {
                "running:preparing-dataplane"
            }
            ployz_sdk_types::NetworkRepairRunningStage::RefreshingMachineFacts => {
                "running:refreshing-machine-facts"
            }
            ployz_sdk_types::NetworkRepairRunningStage::ConfirmingDnsRefresh => {
                "running:confirming-dns-refresh"
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
        ployz_sdk_types::NetworkRepairFailure::TargetMachineNotFound { machine_id } => {
            format!("target-machine-not-found machine={}", machine_id.as_str())
        }
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
        ployz_sdk_types::NetworkRepairFailure::MachineFactsRefreshFailed { outcomes } => format!(
            "machine-facts-refresh-failed outcomes={}",
            outcomes
                .iter()
                .map(render_machine_facts_outcome)
                .collect::<Vec<_>>()
                .join(";")
        ),
        ployz_sdk_types::NetworkRepairFailure::DnsRefreshFailed {
            confirmed_machine_ids,
            problems,
        } => format!(
            "dns-refresh-failed confirmed={} problems={}",
            confirmed_machine_ids
                .iter()
                .map(|machine_id| machine_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            problems
                .iter()
                .map(render_dns_refresh_problem)
                .collect::<Vec<_>>()
                .join(";")
        ),
        ployz_sdk_types::NetworkRepairFailure::ProgressRecordFailed { phase, message } => {
            format!(
                "progress-record-failed phase={} message={}",
                render_progress_phase(*phase),
                message.as_str()
            )
        }
    }
}

fn render_machine_facts_outcome(
    outcome: &ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome,
) -> String {
    match outcome {
        ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome::Refreshed {
            machine_id,
            observed_at_unix_ms,
        } => format!(
            "machine={}:refreshed@{observed_at_unix_ms}",
            machine_id.as_str()
        ),
        ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome::Unavailable {
            machine_id,
            message,
        } => format!(
            "machine={}:unavailable:{}",
            machine_id.as_str(),
            message.as_str()
        ),
        ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome::Failed {
            machine_id,
            message,
        } => format!(
            "machine={}:failed:{}",
            machine_id.as_str(),
            message.as_str()
        ),
    }
}

fn render_dns_refresh_problem(problem: &ployz_sdk_types::NetworkRepairDnsRefreshProblem) -> String {
    match problem {
        ployz_sdk_types::NetworkRepairDnsRefreshProblem::Unavailable {
            machine_id,
            message,
        } => format!(
            "machine={}:unavailable:{}",
            machine_id.as_str(),
            message.as_str()
        ),
        ployz_sdk_types::NetworkRepairDnsRefreshProblem::Stale {
            machine_id,
            stale_machine_ids,
        } => format!(
            "machine={}:stale:{}",
            machine_id.as_str(),
            stale_machine_ids
                .iter()
                .map(|machine_id| machine_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

const fn render_progress_phase(phase: ployz_sdk_types::NetworkRepairProgressPhase) -> &'static str {
    match phase {
        ployz_sdk_types::NetworkRepairProgressPhase::Starting => "starting",
        ployz_sdk_types::NetworkRepairProgressPhase::RecordingDataplaneEvidence => {
            "recording-dataplane-evidence"
        }
        ployz_sdk_types::NetworkRepairProgressPhase::AdvancingMachineFacts => {
            "advancing-machine-facts"
        }
        ployz_sdk_types::NetworkRepairProgressPhase::RecordingMachineFactsEvidence => {
            "recording-machine-facts-evidence"
        }
        ployz_sdk_types::NetworkRepairProgressPhase::AdvancingDnsRefresh => "advancing-dns-refresh",
        ployz_sdk_types::NetworkRepairProgressPhase::RecordingDnsRefreshEvidence => {
            "recording-dns-refresh-evidence"
        }
        ployz_sdk_types::NetworkRepairProgressPhase::Completing => "completing",
        ployz_sdk_types::NetworkRepairProgressPhase::RecordingTerminal => "recording-terminal",
    }
}
