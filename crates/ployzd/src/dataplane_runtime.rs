//! Host WireGuard/eBPF readiness for node-local dataplane preparation.

use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfComponent,
    WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardEbpfReady, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::ids::NodeId;
use ployz_core::ops::FailureMessage;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

use crate::node_service_runtime::NodeWireGuardEbpfPreparer;

const HOST_DATAPLANE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WIREGUARD_KEY_DIR: &str = "/etc/ployz";
const DEFAULT_WIREGUARD_PRIVATE_KEY: &str = "/etc/ployz/wireguard.key";

#[derive(Debug, Clone)]
pub struct HostWireGuardEbpfPreparer {
    node_id: NodeId,
    requirements: Vec<HostDataplaneRequirement>,
    route_programming: Option<HostDataplaneRouteProgramming>,
    command_timeout: Duration,
}

impl HostWireGuardEbpfPreparer {
    #[must_use]
    pub fn new(
        node_id: NodeId,
        ebpf_bytecode_path: PathBuf,
        ebpf_ctl_path: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
    ) -> Self {
        let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
        Self {
            node_id,
            requirements: default_requirements(
                ebpf_bytecode_path,
                ebpf_ctl_path,
                bridge_ifname,
                wg_ifname.clone(),
            ),
            route_programming: Some(HostDataplaneRouteProgramming {
                ebpf_ctl_program,
                wg_ifname,
            }),
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
            route_programming: None,
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
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
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
        if let Some(route_programming) = &self.route_programming {
            for requirement in route_programming.requirements_for(&self.node_id, endpoint_routes) {
                match requirement
                    .check(&self.node_id, self.command_timeout)
                    .await?
                {
                    HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                    HostDataplaneEvidence::EbpfForwarding(evidence) => {
                        ebpf_forwarding.push(evidence);
                    }
                }
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
struct HostDataplaneRouteProgramming {
    ebpf_ctl_program: String,
    wg_ifname: String,
}

impl HostDataplaneRouteProgramming {
    fn requirements_for(
        &self,
        node_id: &NodeId,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
    ) -> Vec<HostDataplaneRequirement> {
        endpoint_routes
            .iter()
            .filter(|route| route.node_id != *node_id)
            .map(|route| {
                HostDataplaneRequirement::command_succeeds(
                    WireGuardEbpfComponent::EbpfForwarding,
                    self.ebpf_ctl_program.clone(),
                    [
                        "route".to_owned(),
                        "add-ifname".to_owned(),
                        route.endpoint_subnet.clone(),
                        self.wg_ifname.clone(),
                    ],
                )
            })
            .collect()
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

fn default_requirements(
    ebpf_bytecode_path: PathBuf,
    ebpf_ctl_path: PathBuf,
    bridge_ifname: String,
    wg_ifname: String,
) -> Vec<HostDataplaneRequirement> {
    let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
    let ebpf_bytecode_arg = ebpf_bytecode_path.display().to_string();
    vec![
        HostDataplaneRequirement::existing_path(WireGuardEbpfComponent::WireGuard, "/dev/net/tun"),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            ["--version"],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "install",
            ["-d", "-m", "0700", DEFAULT_WIREGUARD_KEY_DIR],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "test -s /etc/ployz/wireguard.key || (umask 077 && wg genkey > /etc/ployz/wireguard.key)",
            ],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "ip link show \"$1\" >/dev/null 2>&1 || ip link add dev \"$1\" type wireguard"
                    .to_owned(),
                "--".to_owned(),
                wg_ifname.clone(),
            ],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "private-key".to_owned(),
                DEFAULT_WIREGUARD_PRIVATE_KEY.to_owned(),
            ],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "ip",
            [
                "link".to_owned(),
                "set".to_owned(),
                "up".to_owned(),
                "dev".to_owned(),
                wg_ifname.clone(),
            ],
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
            ebpf_ctl_path,
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::EbpfForwarding,
            ebpf_ctl_program.clone(),
            ["validate".to_owned(), ebpf_bytecode_arg.clone()],
        ),
        HostDataplaneRequirement::ployz_tc_bytecode(ebpf_bytecode_path),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::EbpfForwarding,
            ebpf_ctl_program,
            [
                "ensure-attached".to_owned(),
                ebpf_bytecode_arg,
                bridge_ifname,
                wg_ifname,
            ],
        ),
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
    ployz_ebpf_common::validate_ployz_tc_bytecode(bytes.as_slice()).map_err(|source| {
        unavailable(
            node_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode is not valid Ployz TC bytecode: {}: {source:?}",
                path.display()
            ),
        )
    })
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
            .prepare_wireguard_ebpf(&[])
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

    #[test]
    fn default_requirements_ensure_ployz_tc_is_attached() {
        let requirements = default_requirements(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
        );

        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::EbpfForwarding,
                    program,
                    args,
                } if program == "/usr/local/bin/ployz-ebpf-ctl"
                    && args == &[
                        "ensure-attached",
                        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                        "docker0",
                        "ployz-wg0"
                    ]
            )
        }));
    }

    #[test]
    fn default_requirements_ensure_wireguard_interface_and_key() {
        let requirements = default_requirements(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
        );

        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::WireGuard,
                    program,
                    args,
                } if program == "install" && args == &["-d", "-m", "0700", "/etc/ployz"]
            )
        }));
        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::WireGuard,
                    program,
                    args,
                } if program == "sh"
                    && args == &[
                        "-c",
                        "test -s /etc/ployz/wireguard.key || (umask 077 && wg genkey > /etc/ployz/wireguard.key)"
                    ]
            )
        }));
        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::WireGuard,
                    program,
                    args,
                } if program == "sh"
                    && args == &[
                        "-c",
                        "ip link show \"$1\" >/dev/null 2>&1 || ip link add dev \"$1\" type wireguard",
                        "--",
                        "ployz-wg0"
                    ]
            )
        }));
        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::WireGuard,
                    program,
                    args,
                } if program == "wg"
                    && args == &["set", "ployz-wg0", "private-key", "/etc/ployz/wireguard.key"]
            )
        }));
        assert!(requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::WireGuard,
                    program,
                    args,
                } if program == "ip" && args == &["link", "set", "up", "dev", "ployz-wg0"]
            )
        }));
    }

    #[test]
    fn route_programming_adds_only_peer_endpoint_subnets() {
        let route_programming = HostDataplaneRouteProgramming {
            ebpf_ctl_program: "/usr/local/bin/ployz-ebpf-ctl".to_owned(),
            wg_ifname: "ployz-wg0".to_owned(),
        };
        let requirements = route_programming.requirements_for(
            &node_id("node_a"),
            &[
                WireGuardEbpfEndpointRoute::default_for_node(&node_id("node_a")),
                WireGuardEbpfEndpointRoute {
                    node_id: node_id("node_b"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                },
            ],
        );

        assert_eq!(
            requirements,
            vec![HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::EbpfForwarding,
                "/usr/local/bin/ployz-ebpf-ctl",
                ["route", "add-ifname", "10.42.2.0/24", "ployz-wg0"]
            )]
        );
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
            .prepare_wireguard_ebpf(&[])
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
            .prepare_wireguard_ebpf(&[])
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
            .prepare_wireguard_ebpf(&[])
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
            .prepare_wireguard_ebpf(&[])
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
