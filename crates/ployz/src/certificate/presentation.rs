use ployz_sdk_types::{
    CertInterruptionStage, CertificateInterruptionNextAction, CertificateProvisionFailure,
    OperationInterruptionCause,
};

pub(crate) fn provision_failure_detail(
    failure: &CertificateProvisionFailure,
    scope: Option<&str>,
) -> String {
    let scope = scope.map_or_else(String::new, |scope| format!(" {scope}"));
    match failure {
        CertificateProvisionFailure::OperationEvidenceWrite { message } => format!(
            "certificate operation evidence write failed{scope}: {}",
            message.as_str()
        ),
        CertificateProvisionFailure::DnsPreflight { message } => {
            format!(
                "certificate DNS preflight failed{scope}: {}",
                message.as_str()
            )
        }
        CertificateProvisionFailure::ChallengePublish { message } => format!(
            "certificate HTTP-01 challenge publish failed{scope}: {}",
            message.as_str()
        ),
        CertificateProvisionFailure::ChallengeReadiness {
            missing_machine_ids,
        } => {
            let machines = missing_machine_ids
                .iter()
                .map(|machine_id| machine_id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "certificate HTTP-01 challenge readiness failed{scope}: missing gateway acknowledgements from {machines}"
            )
        }
        CertificateProvisionFailure::AcmeValidation { message } => {
            format!(
                "certificate ACME validation failed{scope}: {}",
                message.as_str()
            )
        }
        CertificateProvisionFailure::CoreInterrupted {
            cause,
            last_durable_stage,
            next_action,
        } => format!(
            "certificate work interrupted by {} at {}{scope}; {}",
            interruption_cause(*cause),
            cert_stage(*last_durable_stage),
            interruption_action(*next_action),
        ),
        CertificateProvisionFailure::GatewayArtifactPush {
            machine_id,
            message,
        } => format!(
            "certificate gateway artifact push failed on {}{scope}: {}",
            machine_id.as_str(),
            message.as_str()
        ),
        CertificateProvisionFailure::ActiveCertCommit {
            attempted_active_cert,
            message,
        } => format!(
            "active certificate commit failed for {}{scope}: {}",
            attempted_active_cert.cert_id.as_str(),
            message.as_str()
        ),
    }
}

const fn interruption_cause(cause: OperationInterruptionCause) -> &'static str {
    match cause {
        OperationInterruptionCause::CoreShutdown => "core shutdown",
        OperationInterruptionCause::PriorCoreProcessLoss => "prior core process loss",
    }
}

const fn cert_stage(stage: CertInterruptionStage) -> &'static str {
    match stage {
        CertInterruptionStage::Accepted => "accepted",
        CertInterruptionStage::Running {
            stage: ployz_sdk_types::CertRunningStage::ChallengePublished,
        } => "challenge published",
        CertInterruptionStage::Running {
            stage: ployz_sdk_types::CertRunningStage::ValidationStarted,
        } => "validation started",
    }
}

const fn interruption_action(action: CertificateInterruptionNextAction) -> &'static str {
    match action {
        CertificateInterruptionNextAction::RetryFromCurrentIntent => "retry from current intent",
    }
}
