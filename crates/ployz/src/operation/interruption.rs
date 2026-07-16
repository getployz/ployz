use ployz_core::operation::{
    BuildInterruptionStage, DeployInterruptionStage, DeployRunningStage,
    NamespaceRemoveRunningStage, NetworkRepairRunningStage, OperationInterruptionCause,
    OperationInterruptionEvidence, OperationInterruptionNextAction, OperationInterruptionStage,
    OperationInterruptionUncertainWork, ServiceRestartRunningStage, VolumeCreateRunningStage,
    VolumeRemoveRunningStage,
};

pub(crate) fn render_interruption(evidence: &OperationInterruptionEvidence) -> String {
    format!(
        "core interruption: cause {}; last durable stage {}; uncertain work {}; next action {}",
        cause(evidence.cause()),
        stage(evidence.last_durable_stage()),
        uncertain_work(evidence.uncertain_work()),
        next_action(evidence.next_action()),
    )
}

const fn cause(cause: OperationInterruptionCause) -> &'static str {
    match cause {
        OperationInterruptionCause::CoreShutdown => "core shutdown",
        OperationInterruptionCause::PriorCoreProcessLoss => "prior core process loss",
    }
}

fn stage(stage: OperationInterruptionStage) -> String {
    match stage {
        OperationInterruptionStage::Build { stage } => match stage {
            BuildInterruptionStage::Accepted => "build accepted".to_owned(),
            BuildInterruptionStage::Placing => "build placing".to_owned(),
            BuildInterruptionStage::Building => "build running".to_owned(),
        },
        OperationInterruptionStage::Deploy { stage } => match stage {
            DeployInterruptionStage::Accepted => "deploy accepted".to_owned(),
            DeployInterruptionStage::Planning => "deploy planning".to_owned(),
            DeployInterruptionStage::Running { stage } => {
                format!("deploy running {}", deploy_stage(stage))
            }
        },
        OperationInterruptionStage::CredentialGrantAccepted => {
            "credential grant accepted".to_owned()
        }
        OperationInterruptionStage::IngressConfigureAccepted => {
            "ingress configure accepted".to_owned()
        }
        OperationInterruptionStage::MachineUpdateAccepted => "machine update accepted".to_owned(),
        OperationInterruptionStage::MachineUpdateRunning => "machine update running".to_owned(),
        OperationInterruptionStage::MachineStoragePrepareAccepted => {
            "machine storage prepare accepted".to_owned()
        }
        OperationInterruptionStage::MachineStoragePreparePreparing => {
            "machine storage prepare preparing".to_owned()
        }
        OperationInterruptionStage::MachineLifecycleAccepted => {
            "machine lifecycle accepted".to_owned()
        }
        OperationInterruptionStage::NetworkRepairAccepted => "network repair accepted".to_owned(),
        OperationInterruptionStage::NetworkRepairRunning { stage } => {
            format!("network repair running {}", network_repair_stage(stage))
        }
        OperationInterruptionStage::ServiceRestartAccepted => "service restart accepted".to_owned(),
        OperationInterruptionStage::ServiceRestartRunning { stage } => {
            format!("service restart running {}", service_restart_stage(stage))
        }
        OperationInterruptionStage::NamespaceRemoveAccepted => {
            "namespace remove accepted".to_owned()
        }
        OperationInterruptionStage::NamespaceRemoveRunning { stage } => {
            format!("namespace remove running {}", namespace_remove_stage(stage))
        }
        OperationInterruptionStage::VolumeRemoveAccepted => "volume remove accepted".to_owned(),
        OperationInterruptionStage::VolumeRemoveRunning { stage } => {
            format!("volume remove running {}", volume_remove_stage(stage))
        }
        OperationInterruptionStage::VolumeCreateAccepted => "volume create accepted".to_owned(),
        OperationInterruptionStage::VolumeCreatePlanning => "volume create planning".to_owned(),
        OperationInterruptionStage::VolumeCreateRunning { stage } => {
            format!("volume create running {}", volume_create_stage(stage))
        }
    }
}

const fn deploy_stage(stage: DeployRunningStage) -> &'static str {
    match stage {
        DeployRunningStage::EnsuringImages => "ensuring images",
        DeployRunningStage::EnsuringVolumes => "ensuring volumes",
        DeployRunningStage::StartingContainers => "starting containers",
        DeployRunningStage::WaitingForHealth => "waiting for health",
        DeployRunningStage::EnsuringCertificates => "ensuring certificates",
        DeployRunningStage::RouteCutover => "route cutover",
        DeployRunningStage::ServingTargetCommit => "serving target commit",
        DeployRunningStage::RemovingSupersededContainers => "removing superseded containers",
    }
}

const fn volume_create_stage(stage: VolumeCreateRunningStage) -> &'static str {
    match stage {
        VolumeCreateRunningStage::CommittingPin => "committing pin",
        VolumeCreateRunningStage::EnsuringVolume => "ensuring volume",
    }
}

const fn network_repair_stage(stage: NetworkRepairRunningStage) -> &'static str {
    match stage {
        NetworkRepairRunningStage::AwaitingDataplane => "awaiting dataplane",
        NetworkRepairRunningStage::RefreshingMachineFacts => "refreshing machine facts",
        NetworkRepairRunningStage::ConfirmingDnsRefresh => "confirming DNS refresh",
    }
}

const fn service_restart_stage(stage: ServiceRestartRunningStage) -> &'static str {
    match stage {
        ServiceRestartRunningStage::RestartingContainers => "restarting containers",
        ServiceRestartRunningStage::WaitingForHealth => "waiting for health",
    }
}

const fn namespace_remove_stage(stage: NamespaceRemoveRunningStage) -> &'static str {
    match stage {
        NamespaceRemoveRunningStage::RemovingRouteBindings => "removing route bindings",
        NamespaceRemoveRunningStage::RemovingServingTargets => "removing serving targets",
        NamespaceRemoveRunningStage::RemovingContainers => "removing containers",
    }
}

const fn volume_remove_stage(stage: VolumeRemoveRunningStage) -> &'static str {
    match stage {
        VolumeRemoveRunningStage::RemovingVolumeData => "removing volume data",
        VolumeRemoveRunningStage::RemovingDataset => "removing dataset",
    }
}

const fn uncertain_work(work: OperationInterruptionUncertainWork) -> &'static str {
    match work {
        OperationInterruptionUncertainWork::Intent => "intent",
        OperationInterruptionUncertainWork::Runtime => "runtime",
        OperationInterruptionUncertainWork::IntentAndRuntime => "intent and runtime",
    }
}

const fn next_action(action: OperationInterruptionNextAction) -> &'static str {
    match action {
        OperationInterruptionNextAction::RetryFromObservedReality => "retry from observed reality",
        OperationInterruptionNextAction::InspectThenResubmit => "inspect then resubmit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
    use ployz_core::operation::{DeployOperationState, EventSequence, OperationStatus};

    #[test]
    fn renders_typed_evidence_as_stable_human_text() {
        let status = OperationStatus::Deploy {
            id: OperationId::try_new("op-render").expect("operation id"),
            namespace_id: NamespaceId::try_new("production").expect("namespace id"),
            service_id: ServiceId::try_new("api").expect("service id"),
            origin: None,
            state: DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
            last_event_sequence: EventSequence::first(),
        };
        let evidence = status
            .interruption_evidence(OperationInterruptionCause::PriorCoreProcessLoss)
            .expect("running deploy interruption evidence");
        let rendered = render_interruption(&evidence);

        assert_eq!(
            rendered,
            "core interruption: cause prior core process loss; last durable stage deploy running starting containers; uncertain work intent and runtime; next action retry from observed reality"
        );
    }
}
