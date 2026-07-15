pub(super) const fn network_repair_state(
    state: &ployz_sdk_types::NetworkRepairOperationState,
) -> &'static str {
    match state {
        ployz_sdk_types::NetworkRepairOperationState::Accepted => "accepted",
        ployz_sdk_types::NetworkRepairOperationState::Running { stage } => match stage {
            ployz_sdk_types::NetworkRepairRunningStage::AwaitingDataplane => {
                "running:awaiting-dataplane"
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
        ployz_sdk_types::NetworkRepairOperationState::Interrupted { .. } => "interrupted",
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
        ployz_sdk_types::NetworkRepairFailure::ProjectionMemberMissing {
            machine_id,
            revision,
        } => format!(
            "projection-member-missing machine={} revision={}",
            machine_id.as_str(),
            revision.as_str()
        ),
        ployz_sdk_types::NetworkRepairFailure::IntentReadFailed { message } => {
            format!("intent-read-failed message={}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairFailure::DataplaneUnavailable { machine_id, reason } => {
            format!(
                "dataplane-unavailable machine={} reason={reason}",
                machine_id.as_str()
            )
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
                phase.as_str(),
                message.as_str()
            )
        }
    }
}

fn render_machine_facts_outcome(
    outcome: &ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome,
) -> String {
    match outcome {
        ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome::Refreshed { refresh } => format!(
            "machine={}:refreshed@{}",
            refresh.machine_id.as_str(),
            refresh.observed_at_unix_ms,
        ),
        ployz_sdk_types::NetworkRepairMachineFactsRefreshOutcome::Unavailable {
            machine_id,
            failure,
        } => format!(
            "machine={}:unavailable:{}",
            machine_id.as_str(),
            render_request_failure(failure)
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
            failure,
        } => format!(
            "machine={}:unavailable:{}",
            machine_id.as_str(),
            render_request_failure(failure)
        ),
        ployz_sdk_types::NetworkRepairDnsRefreshProblem::ResolverNotServing { machine_id } => {
            format!("machine={}:resolver-not-serving", machine_id.as_str())
        }
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

fn render_request_failure(failure: &ployz_sdk_types::NetworkRepairRequestFailure) -> String {
    match failure {
        ployz_sdk_types::NetworkRepairRequestFailure::NoAnswer => "no-answer".to_owned(),
        ployz_sdk_types::NetworkRepairRequestFailure::TimedOut => "timed-out".to_owned(),
        ployz_sdk_types::NetworkRepairRequestFailure::RequestFailed { message } => {
            format!("request-failed:{}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairRequestFailure::ProtocolFailed { message } => {
            format!("protocol-failed:{}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairRequestFailure::DecodeFailed { message } => {
            format!("decode-failed:{}", message.as_str())
        }
        ployz_sdk_types::NetworkRepairRequestFailure::WrongResponder { actual_machine_id } => {
            format!("wrong-responder:{}", actual_machine_id.as_str())
        }
    }
}
