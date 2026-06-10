//! Host WireGuard/eBPF readiness for node-local dataplane preparation.

use ployz_core::dataplane::{
    DEFAULT_WIREGUARD_LISTEN_PORT, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError,
    WireGuardEbpfReady, WireGuardPeer, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::NodeId;
use ployz_core::ops::FailureMessage;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

use crate::node_service_runtime::NodeWireGuardEbpfPreparer;

#[path = "dataplane_runtime/host_routes.rs"]
mod host_routes;

use host_routes::HostDataplaneRouteProgramming;

const HOST_DATAPLANE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WIREGUARD_KEY_DIR: &str = "/etc/ployz";
const DEFAULT_WIREGUARD_PRIVATE_KEY: &str = "/etc/ployz/wireguard.key";

#[derive(Debug, Clone)]
pub struct HostWireGuardEbpfPreparer {
    node_id: NodeId,
    requirements: Vec<HostDataplaneRequirement>,
    route_programming: Option<HostDataplaneRouteProgramming>,
    peer_programming: Option<HostDataplanePeerProgramming>,
    public_key: HostWireGuardPublicKey,
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
        Self::new_with_private_key_path(
            node_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            PathBuf::from(DEFAULT_WIREGUARD_PRIVATE_KEY),
            DEFAULT_WIREGUARD_LISTEN_PORT,
        )
    }

    #[must_use]
    pub fn new_with_private_key_path(
        node_id: NodeId,
        ebpf_bytecode_path: PathBuf,
        ebpf_ctl_path: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
        private_key_path: PathBuf,
        listen_port: u16,
    ) -> Self {
        Self::new_with_host_overrides(
            node_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            private_key_path,
            listen_port,
            None,
        )
    }

    #[must_use]
    pub fn new_with_host_overrides(
        node_id: NodeId,
        ebpf_bytecode_path: PathBuf,
        ebpf_ctl_path: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
        private_key_path: PathBuf,
        listen_port: u16,
        ebpf_pin_path: Option<PathBuf>,
    ) -> Self {
        let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
        Self {
            node_id,
            requirements: default_requirements(
                ebpf_bytecode_path,
                ebpf_ctl_path,
                bridge_ifname.clone(),
                wg_ifname.clone(),
                private_key_path.clone(),
                listen_port,
                ebpf_pin_path.clone(),
            ),
            route_programming: Some(HostDataplaneRouteProgramming {
                ebpf_ctl_program,
                bridge_ifname,
                wg_ifname: wg_ifname.clone(),
                ebpf_pin_path,
            }),
            peer_programming: Some(HostDataplanePeerProgramming {
                wg_ifname: wg_ifname.clone(),
            }),
            public_key: HostWireGuardPublicKey::Command {
                wg_ifname,
                private_key_path,
                listen_port,
            },
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
            peer_programming: None,
            public_key: HostWireGuardPublicKey::Static(
                WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"),
            ),
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
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        self.public_key
            .read(&self.node_id, self.command_timeout)
            .await
    }

    async fn prepare_wireguard_ebpf(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
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
            for requirement in route_programming.requirements_for(&self.node_id, endpoint_routes)? {
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
        if let Some(peer_programming) = &self.peer_programming {
            for requirement in peer_programming.requirements_for(&self.node_id, peers) {
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
        let public_key = self
            .public_key
            .read(&self.node_id, self.command_timeout)
            .await?;
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
                public_key,
                evidence: wireguard,
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: ebpf_forwarding,
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostWireGuardPublicKey {
    Static(WireGuardPublicKey),
    Command {
        wg_ifname: String,
        private_key_path: PathBuf,
        listen_port: u16,
    },
}

impl HostWireGuardPublicKey {
    async fn read(
        &self,
        node_id: &NodeId,
        command_timeout: Duration,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        match self {
            Self::Static(public_key) => Ok(public_key.clone()),
            Self::Command {
                wg_ifname,
                private_key_path,
                listen_port,
            } => {
                for requirement in wireguard_public_key_requirements(
                    wg_ifname.clone(),
                    private_key_path.clone(),
                    *listen_port,
                ) {
                    let _ = requirement.check(node_id, command_timeout).await?;
                }
                read_wireguard_public_key(node_id, wg_ifname, command_timeout).await
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostDataplanePeerProgramming {
    wg_ifname: String,
}

impl HostDataplanePeerProgramming {
    fn requirements_for(
        &self,
        node_id: &NodeId,
        peers: &[WireGuardPeer],
    ) -> Vec<HostDataplaneRequirement> {
        peers
            .iter()
            .filter(|peer| peer.node_id != *node_id)
            .map(|peer| {
                HostDataplaneRequirement::command_succeeds(
                    WireGuardEbpfComponent::WireGuard,
                    "wg",
                    [
                        "set".to_owned(),
                        self.wg_ifname.clone(),
                        "peer".to_owned(),
                        peer.public_key.as_str().to_owned(),
                        "endpoint".to_owned(),
                        peer.public_endpoint.to_string(),
                        "allowed-ips".to_owned(),
                        peer.endpoint_subnet.clone(),
                        "persistent-keepalive".to_owned(),
                        "25".to_owned(),
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
    private_key_path: PathBuf,
    listen_port: u16,
    ebpf_pin_path: Option<PathBuf>,
) -> Vec<HostDataplaneRequirement> {
    let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
    let ebpf_bytecode_arg = ebpf_bytecode_path.display().to_string();
    let ensure_attached_args = ebpf_ctl_args(
        &ebpf_pin_path,
        [
            "ensure-attached".to_owned(),
            ebpf_bytecode_arg.clone(),
            bridge_ifname.clone(),
            wg_ifname.clone(),
        ],
    );
    let mut requirements =
        wireguard_public_key_requirements(wg_ifname.clone(), private_key_path, listen_port);
    requirements.extend([
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
            ensure_attached_args,
        ),
    ]);
    requirements
}

fn ebpf_ctl_args(
    ebpf_pin_path: &Option<PathBuf>,
    args: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut command_args = Vec::new();
    if let Some(pin_path) = ebpf_pin_path {
        command_args.push("--pin-path".to_owned());
        command_args.push(pin_path.display().to_string());
    }
    command_args.extend(args);
    command_args
}

fn wireguard_public_key_requirements(
    wg_ifname: String,
    private_key_path: PathBuf,
    listen_port: u16,
) -> Vec<HostDataplaneRequirement> {
    let private_key_dir = private_key_path
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| DEFAULT_WIREGUARD_KEY_DIR.to_owned());
    let private_key_arg = private_key_path.display().to_string();
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
            [
                "-d".to_owned(),
                "-m".to_owned(),
                "0700".to_owned(),
                private_key_dir,
            ],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "test -s \"$1\" || (umask 077 && wg genkey > \"$1\")".to_owned(),
                "--".to_owned(),
                private_key_arg.clone(),
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
                private_key_arg,
            ],
        ),
        HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "listen-port".to_owned(),
                listen_port.to_string(),
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

async fn read_wireguard_public_key(
    node_id: &NodeId,
    wg_ifname: &str,
    command_timeout: Duration,
) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
    let mut command = Command::new("wg");
    command
        .args(["show", wg_ifname, "public-key"])
        .kill_on_drop(true);
    let output = match tokio::time::timeout(command_timeout, command.output()).await {
        Ok(output) => output,
        Err(_) => {
            return Err(unavailable(
                node_id,
                WireGuardEbpfComponent::WireGuard,
                format!(
                    "wireguard public key command timed out after {}s: wg show {} public-key",
                    command_timeout.as_secs(),
                    wg_ifname,
                ),
            ));
        }
    };
    let output = output.map_err(|source| {
        unavailable(
            node_id,
            WireGuardEbpfComponent::WireGuard,
            format!("wireguard public key command could not start: {source}"),
        )
    })?;
    if !output.status.success() {
        return Err(unavailable(
            node_id,
            WireGuardEbpfComponent::WireGuard,
            format!(
                "wireguard public key command failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    WireGuardPublicKey::try_new(public_key).map_err(|source| {
        unavailable(
            node_id,
            WireGuardEbpfComponent::WireGuard,
            format!("wireguard public key is invalid: {source}"),
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
            .prepare_wireguard_ebpf(&[], &[])
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
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
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
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
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
                        "test -s \"$1\" || (umask 077 && wg genkey > \"$1\")",
                        "--",
                        "/etc/ployz/wireguard.key"
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
                } if program == "wg"
                    && args == &["set", "ployz-wg0", "listen-port", "51820"]
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
    fn peer_programming_adds_only_peer_wireguard_peers() {
        let peer_programming = HostDataplanePeerProgramming {
            wg_ifname: "ployz-wg0".to_owned(),
        };
        let requirements = peer_programming.requirements_for(
            &node_id("node_a"),
            &[
                WireGuardPeer {
                    node_id: node_id("node_a"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                    public_endpoint: "203.0.113.1:51820".parse().expect("valid endpoint"),
                    public_key: wireguard_public_key("public-node_a"),
                },
                WireGuardPeer {
                    node_id: node_id("node_b"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                    public_endpoint: "203.0.113.2:51820".parse().expect("valid endpoint"),
                    public_key: wireguard_public_key("public-node_b"),
                },
            ],
        );

        assert_eq!(
            requirements,
            vec![HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "wg",
                [
                    "set",
                    "ployz-wg0",
                    "peer",
                    "public-node_b",
                    "endpoint",
                    "203.0.113.2:51820",
                    "allowed-ips",
                    "10.42.2.0/24",
                    "persistent-keepalive",
                    "25"
                ]
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
            .prepare_wireguard_ebpf(&[], &[])
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
            .prepare_wireguard_ebpf(&[], &[])
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
            .prepare_wireguard_ebpf(&[], &[])
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
            .prepare_wireguard_ebpf(&[], &[])
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

    fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
    }
}
