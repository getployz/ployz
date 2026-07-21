//! Host Runner plan progress events and human-readable reporting.

use std::io::Write;

use ployz_core::operation::FailureMessage;

use super::execution::{HostRunnerStepEvent, HostRunnerStepRecorder};
use super::steps::{
    ContainerRuntime, HostPrerequisite, HostRunnerStepFailureReason, HostRunnerStepLabel,
};
use crate::execution::{ArtifactKind, ArtifactTarget};

pub struct HostRunnerTextRecorder<W> {
    writer: W,
}

impl<W> HostRunnerTextRecorder<W> {
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write> HostRunnerStepRecorder for HostRunnerTextRecorder<W> {
    fn record_step_event(&mut self, event: &HostRunnerStepEvent) -> Result<(), FailureMessage> {
        writeln!(self.writer, "{}", render_step_event(event)).map_err(|error| {
            failure_message(format!("failed to write Host Runner progress: {error}"))
        })
    }
}

#[must_use]
pub fn render_step_event(event: &HostRunnerStepEvent) -> String {
    match event {
        HostRunnerStepEvent::Started { step } => format!("started {}", render_step_label(step)),
        HostRunnerStepEvent::Succeeded { step } => format!("succeeded {}", render_step_label(step)),
        HostRunnerStepEvent::Failed(failure) => format!(
            "failed {} {}: {}",
            render_step_label(&failure.step),
            render_failure_reason(failure.reason),
            failure.message.as_str()
        ),
    }
}

fn render_step_label(step: &HostRunnerStepLabel) -> String {
    match step {
        HostRunnerStepLabel::VerifyHost(HostPrerequisite::LinuxRoot) => {
            "verify-host linux-root".to_owned()
        }
        HostRunnerStepLabel::PreflightHostPorts(ports) => {
            format!("preflight-host-ports {}", render_host_ports(ports))
        }
        HostRunnerStepLabel::AssureHostPorts(ports) => {
            format!("assure-host-ports {}", render_host_ports(ports))
        }
        HostRunnerStepLabel::StoreAssignedSubstrate(ports) => {
            format!("store-assigned-substrate {}", render_host_ports(ports))
        }
        HostRunnerStepLabel::PrepareDataplaneHost => "prepare-dataplane-host".to_owned(),
        HostRunnerStepLabel::PrepareContainerRuntime(ContainerRuntime::Docker) => {
            "prepare-container-runtime docker".to_owned()
        }
        HostRunnerStepLabel::VerifyContainerRuntime(ContainerRuntime::Docker) => {
            "verify-container-runtime docker".to_owned()
        }
        HostRunnerStepLabel::InstallArtifact(target) => {
            format!("install-artifact {}", render_artifact_target(target))
        }
        HostRunnerStepLabel::WriteNatsServerConfig(target) => {
            format!("write-nats-config {}", target.display_path().display())
        }
        HostRunnerStepLabel::WriteMachineJoinTemplate { path } => {
            format!("write-machine-join-template {}", path.display())
        }
        HostRunnerStepLabel::WriteNatsTlsMaterial { state_dir } => {
            format!("write-nats-tls-material {}", state_dir.display())
        }
        HostRunnerStepLabel::WriteNatsAuthorizedUsers { path } => {
            format!("write-nats-authorized-users {}", path.display())
        }
        HostRunnerStepLabel::WriteNatsClientCredentials { state_dir } => {
            format!("write-nats-client-credentials {}", state_dir.display())
        }
        HostRunnerStepLabel::WritePloyzdRoleEnvironment(step) => {
            format!("write-role-env {}", step.file().path().display())
        }
        HostRunnerStepLabel::WriteSupervisorUnit(target) => {
            format!("write-unit {}", target.unit_name())
        }
        HostRunnerStepLabel::StartSupervisorUnit(target) => {
            format!("start-unit {}", target.unit_name())
        }
        HostRunnerStepLabel::RedeemJoinToken => "redeem-join-token".to_owned(),
        HostRunnerStepLabel::ResolveJoinTarget => "resolve-join-target".to_owned(),
        HostRunnerStepLabel::ReportJoinResult => "report-join-result".to_owned(),
        HostRunnerStepLabel::ConsumeJoinTokenFile => "consume-join-token-file".to_owned(),
        HostRunnerStepLabel::StoreJoinMaterial(material) => {
            format!("store-join-material {}", material.machine_id.as_str())
        }
        HostRunnerStepLabel::StoreInstalledSubstrateRelease(release) => format!(
            "store-installed-substrate-release {}",
            release.release.version.as_str()
        ),
    }
}

fn render_host_ports(ports: &[crate::lifecycle::assigned_substrate::AssignedHostPort]) -> String {
    ports
        .iter()
        .map(|port| {
            let protocol = match port.protocol {
                crate::lifecycle::assigned_substrate::HostPortProtocol::Tcp => "tcp",
                crate::lifecycle::assigned_substrate::HostPortProtocol::Udp => "udp",
            };
            format!("{}/{protocol}", port.port)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn render_artifact_target(target: &ArtifactTarget) -> String {
    let kind = match target.kind {
        ArtifactKind::EbpfBytecode => "ebpf-bytecode",
        ArtifactKind::EbpfCtl => "ebpf-ctl",
        ArtifactKind::NatsServer => "nats-server",
        ArtifactKind::Ployzd => "ployzd",
        ArtifactKind::Railpack => "railpack",
    };
    format!("{kind} {}", target.install_path().display())
}

fn render_failure_reason(reason: HostRunnerStepFailureReason) -> &'static str {
    match reason {
        HostRunnerStepFailureReason::HostPrerequisiteFailed => "host-prerequisite-failed",
        HostRunnerStepFailureReason::HostPortsPreflightFailed => "host-ports-preflight-failed",
        HostRunnerStepFailureReason::HostPortsAssuranceFailed => "host-ports-assurance-failed",
        HostRunnerStepFailureReason::AssignedSubstrateStoreFailed => {
            "assigned-substrate-store-failed"
        }
        HostRunnerStepFailureReason::ArtifactDownloadFailed => "artifact-download-failed",
        HostRunnerStepFailureReason::ArtifactVerificationFailed => "artifact-verification-failed",
        HostRunnerStepFailureReason::ArtifactInstallFailed => "artifact-install-failed",
        HostRunnerStepFailureReason::NatsConfigWriteFailed => "nats-config-write-failed",
        HostRunnerStepFailureReason::NatsTlsMaterialWriteFailed => "nats-tls-material-write-failed",
        HostRunnerStepFailureReason::NatsAuthorizedUsersWriteFailed => {
            "nats-authorized-users-write-failed"
        }
        HostRunnerStepFailureReason::NatsClientCredentialsWriteFailed => {
            "nats-client-credentials-write-failed"
        }
        HostRunnerStepFailureReason::MachineJoinTemplateWriteFailed => {
            "machine-join-template-write-failed"
        }
        HostRunnerStepFailureReason::RoleEnvironmentWriteFailed => "role-environment-write-failed",
        HostRunnerStepFailureReason::SupervisorWriteFailed => "supervisor-write-failed",
        HostRunnerStepFailureReason::SupervisorStartFailed => "supervisor-start-failed",
        HostRunnerStepFailureReason::JoinTokenRedeemFailed => "join-token-redeem-failed",
        HostRunnerStepFailureReason::JoinTargetResolutionFailed => "join-target-resolution-failed",
        HostRunnerStepFailureReason::JoinReportFailed => "join-report-failed",
        HostRunnerStepFailureReason::JoinTokenConsumeFailed => "join-token-consume-failed",
        HostRunnerStepFailureReason::JoinMaterialStoreFailed => "join-material-store-failed",
        HostRunnerStepFailureReason::InstalledSubstrateReleaseStoreFailed => {
            "installed-substrate-release-store-failed"
        }
        HostRunnerStepFailureReason::ContainerRuntimePrepareFailed => {
            "container-runtime-prepare-failed"
        }
        HostRunnerStepFailureReason::DataplaneHostPrepareFailed => "dataplane-host-prepare-failed",
        HostRunnerStepFailureReason::ContainerRuntimeVerifyFailed => {
            "container-runtime-verify-failed"
        }
        HostRunnerStepFailureReason::ContainerRuntimeClassicStoreUnsupported => {
            "container-runtime-classic-store-unsupported"
        }
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("Host Runner generated a non-empty failure message")
}

#[cfg(test)]
mod tests {
    use super::super::execution::HostRunnerStepEvent;
    use super::super::steps::HostRunnerStepLabel;
    use crate::lifecycle::assigned_substrate::{AssignedHostPort, HostPortProtocol};

    use super::render_step_event;

    #[test]
    fn host_port_progress_names_exact_ports() {
        let event = HostRunnerStepEvent::Started {
            step: HostRunnerStepLabel::PreflightHostPorts(vec![
                AssignedHostPort {
                    port: 80,
                    protocol: HostPortProtocol::Tcp,
                },
                AssignedHostPort {
                    port: 51820,
                    protocol: HostPortProtocol::Udp,
                },
            ]),
        };

        assert_eq!(
            render_step_event(&event),
            "started preflight-host-ports 80/tcp,51820/udp"
        );
    }
}
