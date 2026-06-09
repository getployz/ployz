//! Host WireGuard/eBPF readiness for node-local dataplane preparation.

use object::{Object, ObjectSymbol};
use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfComponent,
    WireGuardEbpfPrepareError, WireGuardEbpfReady, WireGuardReady, WireGuardReadyEvidence,
};
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
    async fn prepare_wireguard_ebpf(
        &self,
    ) -> Result<WireGuardEbpfReady, WireGuardEbpfPrepareError> {
        let mut wireguard = Vec::new();
        let mut ebpf_forwarding = Vec::new();
        for requirement in &self.requirements {
            match requirement
                .check(&self.node_id, self.command_timeout)
                .await?
            {
                HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                HostDataplaneEvidence::EbpfForwarding(evidence) => ebpf_forwarding.push(evidence),
            }
        }
        if wireguard.is_empty() {
            return Err(unavailable(
                &self.node_id,
                WireGuardEbpfComponent::WireGuard,
                "wireguard readiness has no evidence".to_owned(),
            ));
        }
        if ebpf_forwarding.is_empty() {
            return Err(unavailable(
                &self.node_id,
                WireGuardEbpfComponent::EbpfForwarding,
                "eBPF forwarding readiness has no evidence".to_owned(),
            ));
        }

        Ok(WireGuardEbpfReady {
            wireguard: WireGuardReady {
                evidence: wireguard,
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: ebpf_forwarding,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostDataplaneEvidence {
    WireGuard(WireGuardReadyEvidence),
    EbpfForwarding(EbpfForwardingReadyEvidence),
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
    PloyzTcBytecode {
        path: PathBuf,
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

    #[must_use]
    pub fn ployz_tc_bytecode(path: impl Into<PathBuf>) -> Self {
        Self::PloyzTcBytecode { path: path.into() }
    }

    async fn check(
        &self,
        node_id: &NodeId,
        command_timeout: Duration,
    ) -> Result<HostDataplaneEvidence, WireGuardEbpfPrepareError> {
        match self {
            Self::ExistingPath { component, path } => {
                if !path.exists() {
                    return Err(unavailable(
                        node_id,
                        *component,
                        format!("required dataplane path is missing: {}", path.display()),
                    ));
                }

                Ok(component_ready_path(*component, path.display().to_string()))
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
                    Ok(output) if output.status.success() => Ok(component_ready_command(
                        *component,
                        program.clone(),
                        args.clone(),
                    )),
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
            Self::PloyzTcBytecode { path } => {
                let symbols = validate_ployz_tc_bytecode(node_id, path)?;
                Ok(HostDataplaneEvidence::EbpfForwarding(
                    EbpfForwardingReadyEvidence::PloyzTcBytecode {
                        path: path.display().to_string(),
                        symbols,
                    },
                ))
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
        HostDataplaneRequirement::ployz_tc_bytecode(ebpf_bytecode_path),
    ]
}

fn validate_ployz_tc_bytecode(
    node_id: &NodeId,
    path: &PathBuf,
) -> Result<Vec<String>, WireGuardEbpfPrepareError> {
    let bytes = std::fs::read(path).map_err(|source| {
        unavailable(
            node_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode could not be read: {}: {source}",
                path.display()
            ),
        )
    })?;
    let object = object::File::parse(bytes.as_slice()).map_err(|source| {
        unavailable(
            node_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode is not a readable object file: {}: {source}",
                path.display()
            ),
        )
    })?;
    if object.architecture() != object::Architecture::Bpf {
        return Err(unavailable(
            node_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode is not a BPF object: {}",
                path.display()
            ),
        ));
    }

    let symbols = object
        .symbols()
        .filter_map(|symbol| symbol.name().ok())
        .collect::<std::collections::BTreeSet<_>>();
    let required = [
        "ployz_egress",
        "ployz_ingress",
        "ROUTES",
        "WG_IFINDEX",
        "OBSERVE_FLAG",
        "EVENTS",
    ];
    let mut found = Vec::new();
    for symbol in required {
        if !symbols.contains(symbol) {
            return Err(unavailable(
                node_id,
                WireGuardEbpfComponent::EbpfForwarding,
                format!(
                    "required eBPF bytecode symbol is missing from {}: {}",
                    path.display(),
                    symbol
                ),
            ));
        }
        found.push(symbol.to_owned());
    }

    Ok(found)
}

fn component_ready_path(component: WireGuardEbpfComponent, path: String) -> HostDataplaneEvidence {
    match component {
        WireGuardEbpfComponent::WireGuard => {
            HostDataplaneEvidence::WireGuard(WireGuardReadyEvidence::HostPath { path })
        }
        WireGuardEbpfComponent::EbpfForwarding => {
            HostDataplaneEvidence::EbpfForwarding(EbpfForwardingReadyEvidence::HostPath { path })
        }
    }
}

fn component_ready_command(
    component: WireGuardEbpfComponent,
    program: String,
    args: Vec<String>,
) -> HostDataplaneEvidence {
    match component {
        WireGuardEbpfComponent::WireGuard => {
            HostDataplaneEvidence::WireGuard(WireGuardReadyEvidence::Command { program, args })
        }
        WireGuardEbpfComponent::EbpfForwarding => {
            HostDataplaneEvidence::EbpfForwarding(EbpfForwardingReadyEvidence::Command {
                program,
                args,
            })
        }
    }
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
    async fn host_preparer_rejects_empty_requirement_set() {
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            Vec::<HostDataplaneRequirement>::new(),
        );

        let error = preparer
            .prepare_wireguard_ebpf()
            .await
            .expect_err("empty requirements fail");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component: WireGuardEbpfComponent::WireGuard,
                ..
            } if node_id == self::node_id("node_a")
        ));
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

    #[tokio::test]
    async fn host_preparer_rejects_text_with_ployz_tc_symbol_names() {
        let path =
            std::env::temp_dir().join(format!("ployz-ebpf-bytecode-test-{}", std::process::id()));
        std::fs::write(
            &path,
            b"ployz_egress\0ployz_ingress\0ROUTES\0WG_IFINDEX\0OBSERVE_FLAG\0EVENTS\0",
        )
        .expect("write test bytecode");
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            [HostDataplaneRequirement::ployz_tc_bytecode(&path)],
        );

        let error = preparer
            .prepare_wireguard_ebpf()
            .await
            .expect_err("text with symbols is not a BPF object");

        let _ = std::fs::remove_file(&path);
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
    async fn host_preparer_rejects_non_ployz_tc_bytecode() {
        let path = std::env::temp_dir().join(format!(
            "ployz-ebpf-bad-bytecode-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"not the required bpf object").expect("write test bytecode");
        let preparer = HostWireGuardEbpfPreparer::with_requirements(
            node_id("node_a"),
            [HostDataplaneRequirement::ployz_tc_bytecode(&path)],
        );

        let error = preparer
            .prepare_wireguard_ebpf()
            .await
            .expect_err("missing bytecode symbols fail");

        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component: WireGuardEbpfComponent::EbpfForwarding,
                ..
            } if node_id == self::node_id("node_a")
        ));
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
