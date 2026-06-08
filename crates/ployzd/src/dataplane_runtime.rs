//! Host WireGuard/eBPF readiness for node-local dataplane preparation.

use ployz_core::dataplane::{WireGuardEbpfComponent, WireGuardEbpfPrepareError};
use ployz_core::ids::NodeId;
use ployz_core::ops::FailureMessage;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

use crate::node_service_runtime::NodeWireGuardEbpfPreparer;

const HOST_DATAPLANE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct HostWireGuardEbpfPreparer {
    node_id: NodeId,
    requirements: Vec<HostDataplaneRequirement>,
    command_timeout: Duration,
}

impl HostWireGuardEbpfPreparer {
    #[must_use]
    pub fn new(node_id: NodeId, ebpf_bytecode_path: PathBuf) -> Self {
        Self {
            node_id,
            requirements: default_requirements(ebpf_bytecode_path),
            command_timeout: HOST_DATAPLANE_COMMAND_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_requirements(
        node_id: NodeId,
        requirements: impl IntoIterator<Item = HostDataplaneRequirement>,
    ) -> Self {
        Self {
            node_id,
            requirements: requirements.into_iter().collect(),
            command_timeout: HOST_DATAPLANE_COMMAND_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_command_timeout(mut self, command_timeout: Duration) -> Self {
        self.command_timeout = command_timeout;
        self
    }
}

impl NodeWireGuardEbpfPreparer for HostWireGuardEbpfPreparer {
    async fn prepare_wireguard_ebpf(&self) -> Result<(), WireGuardEbpfPrepareError> {
        for requirement in &self.requirements {
            requirement
                .check(&self.node_id, self.command_timeout)
                .await?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDataplaneRequirement {
    ExistingPath {
        component: WireGuardEbpfComponent,
        path: PathBuf,
    },
    CommandSucceeds {
        component: WireGuardEbpfComponent,
        program: String,
        args: Vec<String>,
    },
}

impl HostDataplaneRequirement {
    #[must_use]
    pub fn existing_path(component: WireGuardEbpfComponent, path: impl Into<PathBuf>) -> Self {
        Self::ExistingPath {
            component,
            path: path.into(),
        }
    }

    #[must_use]
    pub fn command_succeeds(
        component: WireGuardEbpfComponent,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::CommandSucceeds {
            component,
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    async fn check(
        &self,
        node_id: &NodeId,
        command_timeout: Duration,
    ) -> Result<(), WireGuardEbpfPrepareError> {
        match self {
            Self::ExistingPath { component, path } => {
                if path.exists() {
                    return Ok(());
                }
                Err(unavailable(
                    node_id,
                    *component,
                    format!("required dataplane path is missing: {}", path.display()),
                ))
            }
            Self::CommandSucceeds {
                component,
                program,
                args,
            } => {
                let mut command = Command::new(program);
                command.args(args).kill_on_drop(true);
                let output = match tokio::time::timeout(command_timeout, command.output()).await {
                    Ok(output) => output,
                    Err(_) => {
                        return Err(unavailable(
                            node_id,
                            *component,
                            format!(
                                "required dataplane command timed out after {}s: {} {}",
                                command_timeout.as_secs(),
                                program,
                                args.join(" ")
                            ),
                        ));
                    }
                };
                match output {
                    Ok(output) if output.status.success() => Ok(()),
                    Ok(output) => Err(unavailable(
                        node_id,
                        *component,
                        format!(
                            "required dataplane command failed: {} {}: {}",
                            program,
                            args.join(" "),
                            String::from_utf8_lossy(&output.stderr).trim()
                        ),
                    )),
                    Err(source) => Err(unavailable(
                        node_id,
                        *component,
                        format!(
                            "required dataplane command could not start: {} {}: {}",
                            program,
                            args.join(" "),
                            source
                        ),
                    )),
                }
            }
        }
    }
}

fn default_requirements(ebpf_bytecode_path: PathBuf) -> Vec<HostDataplaneRequirement> {
    vec![
        HostDataplaneRequirement::existing_path(WireGuardEbpfComponent::WireGuard, "/dev/net/tun"),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            ["--version"],
        ),
        HostDataplaneRequirement::existing_path(
            WireGuardEbpfComponent::EbpfForwarding,
            "/sys/fs/bpf",
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::EbpfForwarding,
            "tc",
            ["-V"],
        ),
        HostDataplaneRequirement::existing_path(
            WireGuardEbpfComponent::EbpfForwarding,
            ebpf_bytecode_path,
        ),
    ]
}

fn unavailable(
    node_id: &NodeId,
    component: WireGuardEbpfComponent,
    message: String,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        node_id: node_id.clone(),
        component,
        message: FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_preparer_accepts_empty_requirement_set() {
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            Vec::<HostDataplaneRequirement>::new(),
        );

        preparer
            .prepare_wireguard_ebpf()
            .await
            .expect("empty requirements pass");
    }

    #[tokio::test]
    async fn host_preparer_reports_missing_required_path() {
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            [HostDataplaneRequirement::existing_path(
                WireGuardEbpfComponent::EbpfForwarding,
                "/definitely/missing",
            )],
        );

        let error = preparer
            .prepare_wireguard_ebpf()
            .await
            .expect_err("missing path fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component: WireGuardEbpfComponent::EbpfForwarding,
                ..
            } if node_id == self::node_id("node_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_times_out_hung_commands() {
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            [HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sh",
                ["-c", "sleep 5"],
            )],
        )
        .with_command_timeout(Duration::from_millis(1));

        let error = preparer
            .prepare_wireguard_ebpf()
            .await
            .expect_err("hung command fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component: WireGuardEbpfComponent::WireGuard,
                ..
            } if node_id == self::node_id("node_a")
        ));
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
