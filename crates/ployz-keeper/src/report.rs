//! Human-readable keeper progress.

use std::io::Write;

use ployz_core::ops::FailureMessage;

use crate::artifacts::{ArtifactKind, ArtifactTarget};
use crate::executor::{KeeperStepEvent, KeeperStepRecorder};
use crate::steps::{ContainerRuntime, HostPrerequisite, KeeperStepFailureReason, KeeperStepLabel};

pub struct KeeperTextRecorder<W> {
    writer: W,
}

impl<W> KeeperTextRecorder<W> {
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write> KeeperStepRecorder for KeeperTextRecorder<W> {
    fn record_step_event(&mut self, event: &KeeperStepEvent) -> Result<(), FailureMessage> {
        writeln!(self.writer, "{}", render_step_event(event))
            .map_err(|error| failure_message(format!("failed to write keeper progress: {error}")))
    }
}

#[must_use]
pub fn render_step_event(event: &KeeperStepEvent) -> String {
    match event {
        KeeperStepEvent::Started { step } => format!("started {}", render_step_label(step)),
        KeeperStepEvent::Succeeded { step } => format!("succeeded {}", render_step_label(step)),
        KeeperStepEvent::Failed(failure) => format!(
            "failed {} {}: {}",
            render_step_label(&failure.step),
            render_failure_reason(failure.reason),
            failure.message.as_str()
        ),
    }
}

fn render_step_label(step: &KeeperStepLabel) -> String {
    match step {
        KeeperStepLabel::VerifyHost(HostPrerequisite::LinuxRootSystemd) => {
            "verify-host linux-root-systemd".to_owned()
        }
        KeeperStepLabel::PrepareDataplaneHost => "prepare-dataplane-host".to_owned(),
        KeeperStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker) => {
            "prepare-container-runtime docker".to_owned()
        }
        KeeperStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker) => {
            "verify-container-runtime docker".to_owned()
        }
        KeeperStepLabel::InstallArtifact(target) => {
            format!("install-artifact {}", render_artifact_target(target))
        }
        KeeperStepLabel::WriteNatsServerConfig(target) => {
            format!("write-nats-config {}", target.display_path().display())
        }
        KeeperStepLabel::WriteMachineJoinTemplate { path } => {
            format!("write-machine-join-template {}", path.display())
        }
        KeeperStepLabel::WriteNatsTlsMaterial { state_dir } => {
            format!("write-nats-tls-material {}", state_dir.display())
        }
        KeeperStepLabel::WriteNatsAuthorizedUsers { path } => {
            format!("write-nats-authorized-users {}", path.display())
        }
        KeeperStepLabel::WriteNatsClientCredentials { state_dir } => {
            format!("write-nats-client-credentials {}", state_dir.display())
        }
        KeeperStepLabel::WritePloyzdRoleEnvironment(step) => {
            format!("write-role-env {}", step.file().path().display())
        }
        KeeperStepLabel::WriteSupervisorUnit(target) => {
            format!("write-unit {}", target.unit_name())
        }
        KeeperStepLabel::StartSupervisorUnit(target) => {
            format!("start-unit {}", target.unit_name())
        }
        KeeperStepLabel::RestartSupervisorUnit(target) => {
            format!("restart-unit {}", target.unit_name())
        }
        KeeperStepLabel::RedeemJoinToken => "redeem-join-token".to_owned(),
        KeeperStepLabel::ReportJoinResult => "report-join-result".to_owned(),
        KeeperStepLabel::ConsumeJoinTokenFile => "consume-join-token-file".to_owned(),
        KeeperStepLabel::StoreJoinMaterial(material) => {
            format!("store-join-material {}", material.machine_id.as_str())
        }
    }
}

fn render_artifact_target(target: &ArtifactTarget) -> String {
    let kind = match target.kind {
        ArtifactKind::EbpfBytecode => "ebpf-bytecode",
        ArtifactKind::EbpfCtl => "ebpf-ctl",
        ArtifactKind::NatsServer => "nats-server",
        ArtifactKind::Ployzd => "ployzd",
    };
    format!("{kind} {}", target.install_path().display())
}

fn render_failure_reason(reason: KeeperStepFailureReason) -> &'static str {
    match reason {
        KeeperStepFailureReason::HostPrerequisiteFailed => "host-prerequisite-failed",
        KeeperStepFailureReason::ArtifactDownloadFailed => "artifact-download-failed",
        KeeperStepFailureReason::ArtifactVerificationFailed => "artifact-verification-failed",
        KeeperStepFailureReason::ArtifactInstallFailed => "artifact-install-failed",
        KeeperStepFailureReason::NatsConfigWriteFailed => "nats-config-write-failed",
        KeeperStepFailureReason::NatsTlsMaterialWriteFailed => "nats-tls-material-write-failed",
        KeeperStepFailureReason::NatsAuthorizedUsersWriteFailed => {
            "nats-authorized-users-write-failed"
        }
        KeeperStepFailureReason::NatsClientCredentialsWriteFailed => {
            "nats-client-credentials-write-failed"
        }
        KeeperStepFailureReason::MachineJoinTemplateWriteFailed => {
            "machine-join-template-write-failed"
        }
        KeeperStepFailureReason::RoleEnvironmentWriteFailed => "role-environment-write-failed",
        KeeperStepFailureReason::SupervisorWriteFailed => "supervisor-write-failed",
        KeeperStepFailureReason::SupervisorStartFailed => "supervisor-start-failed",
        KeeperStepFailureReason::SupervisorRestartFailed => "supervisor-restart-failed",
        KeeperStepFailureReason::JoinTokenRedeemFailed => "join-token-redeem-failed",
        KeeperStepFailureReason::JoinReportFailed => "join-report-failed",
        KeeperStepFailureReason::JoinTokenConsumeFailed => "join-token-consume-failed",
        KeeperStepFailureReason::JoinMaterialStoreFailed => "join-material-store-failed",
        KeeperStepFailureReason::ContainerRuntimePrepareFailed => {
            "container-runtime-prepare-failed"
        }
        KeeperStepFailureReason::DataplaneHostPrepareFailed => "dataplane-host-prepare-failed",
        KeeperStepFailureReason::ContainerRuntimeVerifyFailed => "container-runtime-verify-failed",
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("keeper generated a non-empty failure message")
}
