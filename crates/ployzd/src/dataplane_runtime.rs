//! Host WireGuard/eBPF readiness for machine-local dataplane preparation.

use ployz_core::dataplane::{
    DEFAULT_WIREGUARD_LISTEN_PORT, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError,
    WireGuardEbpfReady, WireGuardPeer, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

use crate::machine_runtime::service::MachineWireGuardEbpfPreparer;

#[path = "dataplane_runtime/host_routes.rs"]
mod host_routes;

use host_routes::HostDataplaneRouteProgramming;

const HOST_DATAPLANE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_WIREGUARD_KEY_DIR: &str = "/etc/ployz";
pub const DEFAULT_WIREGUARD_PRIVATE_KEY: &str = "/etc/ployz/wireguard.key";

/// Everything the host preparer needs to provision and verify the local
/// WireGuard/eBPF dataplane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDataplaneConfig {
    pub machine_id: MachineId,
    pub ebpf_bytecode_path: PathBuf,
    pub ebpf_ctl_path: PathBuf,
    pub bridge_ifname: String,
    pub wg_ifname: String,
    pub private_key_path: PathBuf,
    pub listen_port: u16,
    pub ebpf_pin_path: Option<PathBuf>,
}

impl HostDataplaneConfig {
    /// Production defaults for key material: the canonical on-host private
    /// key path and the default WireGuard listen port, with no pin override.
    #[must_use]
    pub fn with_default_key_material(
        machine_id: MachineId,
        ebpf_bytecode_path: PathBuf,
        ebpf_ctl_path: PathBuf,
        bridge_ifname: String,
        wg_ifname: String,
    ) -> Self {
        Self {
            machine_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            private_key_path: PathBuf::from(DEFAULT_WIREGUARD_PRIVATE_KEY),
            listen_port: DEFAULT_WIREGUARD_LISTEN_PORT,
            ebpf_pin_path: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostWireGuardEbpfPreparer {
    machine_id: MachineId,
    plans: Vec<HostCommandPlan>,
    route_programming: Option<HostDataplaneRouteProgramming>,
    peer_programming: Option<HostDataplanePeerProgramming>,
    public_key: HostWireGuardPublicKey,
    command_timeout: Duration,
}

impl HostWireGuardEbpfPreparer {
    #[must_use]
    pub fn new(config: HostDataplaneConfig) -> Self {
        let HostDataplaneConfig {
            machine_id,
            ebpf_bytecode_path,
            ebpf_ctl_path,
            bridge_ifname,
            wg_ifname,
            private_key_path,
            listen_port,
            ebpf_pin_path,
        } = config;
        let ebpf_ctl_program = ebpf_ctl_path.display().to_string();
        Self {
            machine_id,
            plans: default_command_plans(
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

    #[cfg(test)]
    fn with_command_plans(
        machine_id: MachineId,
        plans: impl IntoIterator<Item = HostCommandPlan>,
    ) -> Self {
        Self {
            machine_id,
            plans: plans.into_iter().collect(),
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

impl MachineWireGuardEbpfPreparer for HostWireGuardEbpfPreparer {
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        // Standalone reads happen before any prepare (e.g. the join
        // report), so the WireGuard interface must be provisioned first.
        self.public_key
            .provision_and_read(&self.machine_id, self.command_timeout)
            .await
    }

    async fn prepare_wireguard_ebpf(
        &self,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
        peers: &[WireGuardPeer],
    ) -> Result<WireGuardEbpfReady, WireGuardEbpfPrepareError> {
        let mut wireguard = Vec::new();
        let mut ebpf_forwarding = Vec::new();
        for plan in &self.plans {
            match plan.run(&self.machine_id, self.command_timeout).await? {
                HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                HostDataplaneEvidence::EbpfForwarding(evidence) => ebpf_forwarding.push(evidence),
            }
        }
        if let Some(route_programming) = &self.route_programming {
            for plan in route_programming.plans_for(&self.machine_id, endpoint_routes)? {
                match plan.run(&self.machine_id, self.command_timeout).await? {
                    HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                    HostDataplaneEvidence::EbpfForwarding(evidence) => {
                        ebpf_forwarding.push(evidence);
                    }
                }
            }
        }
        if let Some(peer_programming) = &self.peer_programming {
            for plan in peer_programming.plans_for(&self.machine_id, peers) {
                match plan.run(&self.machine_id, self.command_timeout).await? {
                    HostDataplaneEvidence::WireGuard(evidence) => wireguard.push(evidence),
                    HostDataplaneEvidence::EbpfForwarding(evidence) => {
                        ebpf_forwarding.push(evidence);
                    }
                }
            }
        }
        // The plans above already provisioned the WireGuard interface, so
        // the public key only needs to be read here.
        let public_key = self
            .public_key
            .read_provisioned(&self.machine_id, self.command_timeout)
            .await?;
        if wireguard.is_empty() {
            return Err(unavailable(
                &self.machine_id,
                WireGuardEbpfComponent::WireGuard,
                "wireguard readiness has no evidence".to_owned(),
            ));
        }
        if ebpf_forwarding.is_empty() {
            return Err(unavailable(
                &self.machine_id,
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
    #[cfg(test)]
    Static(WireGuardPublicKey),
    Command {
        wg_ifname: String,
        private_key_path: PathBuf,
        listen_port: u16,
    },
}

impl HostWireGuardPublicKey {
    /// Provisions the WireGuard interface, then reads its public key. Used
    /// where no prepare has run yet.
    async fn provision_and_read(
        &self,
        machine_id: &MachineId,
        command_timeout: Duration,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        match self {
            #[cfg(test)]
            Self::Static(public_key) => Ok(public_key.clone()),
            Self::Command {
                wg_ifname,
                private_key_path,
                listen_port,
            } => {
                for plan in wireguard_interface_plans(
                    wg_ifname.clone(),
                    private_key_path.clone(),
                    *listen_port,
                ) {
                    let _ = plan.run(machine_id, command_timeout).await?;
                }
                read_wireguard_public_key(machine_id, wg_ifname, command_timeout).await
            }
        }
    }

    /// Reads the public key from an interface the prepare plans already
    /// provisioned.
    async fn read_provisioned(
        &self,
        machine_id: &MachineId,
        command_timeout: Duration,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        match self {
            #[cfg(test)]
            Self::Static(public_key) => Ok(public_key.clone()),
            Self::Command { wg_ifname, .. } => {
                read_wireguard_public_key(machine_id, wg_ifname, command_timeout).await
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostDataplanePeerProgramming {
    wg_ifname: String,
}

impl HostDataplanePeerProgramming {
    fn plans_for(&self, machine_id: &MachineId, peers: &[WireGuardPeer]) -> Vec<HostCommandPlan> {
        peers
            .iter()
            .filter(|peer| peer.machine_id != *machine_id)
            .map(|peer| {
                HostCommandPlan::provisioning_command(
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

/// Why a host command runs: to mutate the host toward the required
/// dataplane shape (idempotently), or to observe it without changing
/// anything. Both produce readiness evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCommandPurpose {
    ProvisioningStep,
    ReadinessCheck,
}

/// One planned host action with an explicit purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostCommandPlan {
    purpose: HostCommandPurpose,
    action: HostCommandAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostCommandAction {
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

impl HostCommandPlan {
    #[must_use]
    fn readiness_path(component: WireGuardEbpfComponent, path: impl Into<PathBuf>) -> Self {
        Self {
            purpose: HostCommandPurpose::ReadinessCheck,
            action: HostCommandAction::ExistingPath {
                component,
                path: path.into(),
            },
        }
    }

    #[must_use]
    fn readiness_command(
        component: WireGuardEbpfComponent,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            purpose: HostCommandPurpose::ReadinessCheck,
            action: command_action(component, program, args),
        }
    }

    #[must_use]
    fn provisioning_command(
        component: WireGuardEbpfComponent,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            purpose: HostCommandPurpose::ProvisioningStep,
            action: command_action(component, program, args),
        }
    }

    #[must_use]
    fn readiness_ployz_tc_bytecode(path: impl Into<PathBuf>) -> Self {
        Self {
            purpose: HostCommandPurpose::ReadinessCheck,
            action: HostCommandAction::PloyzTcBytecode { path: path.into() },
        }
    }

    async fn run(
        &self,
        machine_id: &MachineId,
        command_timeout: Duration,
    ) -> Result<HostDataplaneEvidence, WireGuardEbpfPrepareError> {
        let Self {
            purpose: HostCommandPurpose::ProvisioningStep | HostCommandPurpose::ReadinessCheck,
            action,
        } = self;
        match action {
            HostCommandAction::ExistingPath { component, path } => {
                if !path.exists() {
                    return Err(unavailable(
                        machine_id,
                        *component,
                        format!("required dataplane path is missing: {}", path.display()),
                    ));
                }

                Ok(component_ready_path(*component, path.display().to_string()))
            }
            HostCommandAction::CommandSucceeds {
                component,
                program,
                args,
            } => match run_host_command(program, args, command_timeout).await {
                HostCommandOutcome::Success(_) => Ok(component_ready_command(
                    *component,
                    program.clone(),
                    args.clone(),
                )),
                HostCommandOutcome::TimedOut => Err(unavailable(
                    machine_id,
                    *component,
                    format!(
                        "required dataplane command timed out after {}s: {} {}",
                        command_timeout.as_secs(),
                        program,
                        args.join(" ")
                    ),
                )),
                HostCommandOutcome::Failed(output) => Err(unavailable(
                    machine_id,
                    *component,
                    format!(
                        "required dataplane command failed: {} {}: {}",
                        program,
                        args.join(" "),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                )),
                HostCommandOutcome::CouldNotStart(source) => Err(unavailable(
                    machine_id,
                    *component,
                    format!(
                        "required dataplane command could not start: {} {}: {}",
                        program,
                        args.join(" "),
                        source
                    ),
                )),
            },
            HostCommandAction::PloyzTcBytecode { path } => {
                let symbols = validate_ployz_tc_bytecode(machine_id, path)?;
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

fn command_action(
    component: WireGuardEbpfComponent,
    program: impl Into<String>,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> HostCommandAction {
    HostCommandAction::CommandSucceeds {
        component,
        program: program.into(),
        args: args.into_iter().map(Into::into).collect(),
    }
}

enum HostCommandOutcome {
    Success(std::process::Output),
    Failed(std::process::Output),
    TimedOut,
    CouldNotStart(std::io::Error),
}

async fn run_host_command(program: &str, args: &[String], timeout: Duration) -> HostCommandOutcome {
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => HostCommandOutcome::TimedOut,
        Ok(Err(source)) => HostCommandOutcome::CouldNotStart(source),
        Ok(Ok(output)) if output.status.success() => HostCommandOutcome::Success(output),
        Ok(Ok(output)) => HostCommandOutcome::Failed(output),
    }
}

fn default_command_plans(
    ebpf_bytecode_path: PathBuf,
    ebpf_ctl_path: PathBuf,
    bridge_ifname: String,
    wg_ifname: String,
    private_key_path: PathBuf,
    listen_port: u16,
    ebpf_pin_path: Option<PathBuf>,
) -> Vec<HostCommandPlan> {
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
    let mut plans = wireguard_interface_plans(wg_ifname.clone(), private_key_path, listen_port);
    plans.extend([
        HostCommandPlan::readiness_path(WireGuardEbpfComponent::EbpfForwarding, "/sys/fs/bpf"),
        HostCommandPlan::readiness_command(WireGuardEbpfComponent::EbpfForwarding, "tc", ["-V"]),
        HostCommandPlan::readiness_path(WireGuardEbpfComponent::EbpfForwarding, ebpf_ctl_path),
        HostCommandPlan::readiness_command(
            WireGuardEbpfComponent::EbpfForwarding,
            ebpf_ctl_program.clone(),
            ["validate".to_owned(), ebpf_bytecode_arg.clone()],
        ),
        HostCommandPlan::readiness_ployz_tc_bytecode(ebpf_bytecode_path),
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::EbpfForwarding,
            ebpf_ctl_program,
            ensure_attached_args,
        ),
    ]);
    plans
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

/// The steps that make the local WireGuard interface exist with its key and
/// listen port, plus the readiness checks they depend on.
fn wireguard_interface_plans(
    wg_ifname: String,
    private_key_path: PathBuf,
    listen_port: u16,
) -> Vec<HostCommandPlan> {
    let private_key_dir = private_key_path
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| DEFAULT_WIREGUARD_KEY_DIR.to_owned());
    let private_key_arg = private_key_path.display().to_string();
    vec![
        HostCommandPlan::readiness_path(WireGuardEbpfComponent::WireGuard, "/dev/net/tun"),
        HostCommandPlan::readiness_command(WireGuardEbpfComponent::WireGuard, "wg", ["--version"]),
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "install",
            [
                "-d".to_owned(),
                "-m".to_owned(),
                "0700".to_owned(),
                private_key_dir,
            ],
        ),
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "test -s \"$1\" || (umask 077 && wg genkey > \"$1\")".to_owned(),
                "--".to_owned(),
                private_key_arg.clone(),
            ],
        ),
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "if [ -f /etc/apparmor.d/wg ] && command -v apparmor_parser >/dev/null 2>&1; then install -d -m 0755 /etc/apparmor.d/local; touch /etc/apparmor.d/local/wg; if ! grep -qxF \"  $1 r,\" /etc/apparmor.d/local/wg; then printf '\\n  %s r,\\n' \"$1\" >> /etc/apparmor.d/local/wg; fi; apparmor_parser -r /etc/apparmor.d/wg; fi".to_owned(),
                "--".to_owned(),
                private_key_arg.clone(),
            ],
        ),
        HostCommandPlan::provisioning_command(
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
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "private-key".to_owned(),
                private_key_arg,
            ],
        ),
        HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "listen-port".to_owned(),
                listen_port.to_string(),
            ],
        ),
        HostCommandPlan::provisioning_command(
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
    machine_id: &MachineId,
    path: &PathBuf,
) -> Result<Vec<String>, WireGuardEbpfPrepareError> {
    let bytes = std::fs::read(path).map_err(|source| {
        unavailable(
            machine_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode could not be read: {}: {source}",
                path.display()
            ),
        )
    })?;
    ployz_ebpf_common::validate_ployz_tc_bytecode(bytes.as_slice()).map_err(|source| {
        unavailable(
            machine_id,
            WireGuardEbpfComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode is not valid Ployz TC bytecode: {}: {source:?}",
                path.display()
            ),
        )
    })
}

async fn read_wireguard_public_key(
    machine_id: &MachineId,
    wg_ifname: &str,
    command_timeout: Duration,
) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
    let args = vec![
        "show".to_owned(),
        wg_ifname.to_owned(),
        "public-key".to_owned(),
    ];
    let output = match run_host_command("wg", &args, command_timeout).await {
        HostCommandOutcome::Success(output) => output,
        HostCommandOutcome::TimedOut => {
            return Err(unavailable(
                machine_id,
                WireGuardEbpfComponent::WireGuard,
                format!(
                    "wireguard public key command timed out after {}s: wg show {} public-key",
                    command_timeout.as_secs(),
                    wg_ifname,
                ),
            ));
        }
        HostCommandOutcome::CouldNotStart(source) => {
            return Err(unavailable(
                machine_id,
                WireGuardEbpfComponent::WireGuard,
                format!("wireguard public key command could not start: {source}"),
            ));
        }
        HostCommandOutcome::Failed(output) => {
            return Err(unavailable(
                machine_id,
                WireGuardEbpfComponent::WireGuard,
                format!(
                    "wireguard public key command failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
    };
    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    WireGuardPublicKey::try_new(public_key).map_err(|source| {
        unavailable(
            machine_id,
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
    machine_id: &MachineId,
    component: WireGuardEbpfComponent,
    message: String,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        machine_id: machine_id.clone(),
        component,
        message: FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn host_preparer_rejects_empty_command_plan_set() {
        let preparer = HostWireGuardEbpfPreparer::with_command_plans(
            machine_id("machine_a"),
            Vec::<HostCommandPlan>::new(),
        );

        let error = preparer
            .prepare_wireguard_ebpf(&[], &[])
            .await
            .expect_err("empty command plans fail");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: WireGuardEbpfComponent::WireGuard,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[test]
    fn default_command_plans_ensure_ployz_tc_is_attached() {
        let plans = default_command_plans(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
        );

        assert!(plans.iter().any(|plan| {
            matches!(
                &plan.action,
                HostCommandAction::CommandSucceeds {
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
    fn default_command_plans_ensure_wireguard_interface_and_key() {
        let plans = default_command_plans(
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "docker0".to_owned(),
            "ployz-wg0".to_owned(),
            "/etc/ployz/wireguard.key".into(),
            51820,
            None,
        );

        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "install",
            ["-d", "-m", "0700", "/etc/ployz"]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "test -s \"$1\" || (umask 077 && wg genkey > \"$1\")",
                "--",
                "/etc/ployz/wireguard.key"
            ]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "if [ -f /etc/apparmor.d/wg ] && command -v apparmor_parser >/dev/null 2>&1; then install -d -m 0755 /etc/apparmor.d/local; touch /etc/apparmor.d/local/wg; if ! grep -qxF \"  $1 r,\" /etc/apparmor.d/local/wg; then printf '\\n  %s r,\\n' \"$1\" >> /etc/apparmor.d/local/wg; fi; apparmor_parser -r /etc/apparmor.d/wg; fi",
                "--",
                "/etc/ployz/wireguard.key"
            ]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "ip link show \"$1\" >/dev/null 2>&1 || ip link add dev \"$1\" type wireguard",
                "--",
                "ployz-wg0"
            ]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            [
                "set",
                "ployz-wg0",
                "private-key",
                "/etc/ployz/wireguard.key"
            ]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            ["set", "ployz-wg0", "listen-port", "51820"]
        )));
        assert!(plans.contains(&HostCommandPlan::provisioning_command(
            WireGuardEbpfComponent::WireGuard,
            "ip",
            ["link", "set", "up", "dev", "ployz-wg0"]
        )));
    }

    #[test]
    fn command_plans_distinguish_provisioning_from_readiness() {
        let plans = wireguard_interface_plans(
            "ployz-wg0".to_owned(),
            "/etc/ployz/wireguard.key".into(),
            51820,
        );

        assert!(plans.contains(&HostCommandPlan::readiness_command(
            WireGuardEbpfComponent::WireGuard,
            "wg",
            ["--version"]
        )));
        let key_generation = plans
            .iter()
            .find(|plan| {
                matches!(
                    &plan.action,
                    HostCommandAction::CommandSucceeds { program, .. } if program == "sh"
                )
            })
            .expect("key generation plan exists");
        assert_eq!(key_generation.purpose, HostCommandPurpose::ProvisioningStep);
    }

    #[test]
    fn peer_programming_adds_only_peer_wireguard_peers() {
        let peer_programming = HostDataplanePeerProgramming {
            wg_ifname: "ployz-wg0".to_owned(),
        };
        let plans = peer_programming.plans_for(
            &machine_id("machine_a"),
            &[
                WireGuardPeer {
                    machine_id: machine_id("machine_a"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                    public_endpoint: "203.0.113.1:51820".parse().expect("valid endpoint"),
                    public_key: wireguard_public_key("public-machine_a"),
                },
                WireGuardPeer {
                    machine_id: machine_id("machine_b"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                    public_endpoint: "203.0.113.2:51820".parse().expect("valid endpoint"),
                    public_key: wireguard_public_key("public-machine_b"),
                },
            ],
        );

        assert_eq!(
            plans,
            vec![HostCommandPlan::provisioning_command(
                WireGuardEbpfComponent::WireGuard,
                "wg",
                [
                    "set",
                    "ployz-wg0",
                    "peer",
                    "public-machine_b",
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
        let preparer = HostWireGuardEbpfPreparer::with_command_plans(
            machine_id("machine_a"),
            [HostCommandPlan::readiness_path(
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
                machine_id,
                component: WireGuardEbpfComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_times_out_hung_commands() {
        let preparer = HostWireGuardEbpfPreparer::with_command_plans(
            machine_id("machine_a"),
            [HostCommandPlan::readiness_command(
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
                machine_id,
                component: WireGuardEbpfComponent::WireGuard,
                ..
            } if machine_id == self::machine_id("machine_a")
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
        let preparer = HostWireGuardEbpfPreparer::with_command_plans(
            machine_id("machine_a"),
            [HostCommandPlan::readiness_ployz_tc_bytecode(&path)],
        );

        let error = preparer
            .prepare_wireguard_ebpf(&[], &[])
            .await
            .expect_err("text with symbols is not a BPF object");

        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: WireGuardEbpfComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    #[tokio::test]
    async fn host_preparer_rejects_non_ployz_tc_bytecode() {
        let path = std::env::temp_dir().join(format!(
            "ployz-ebpf-bad-bytecode-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"not the required bpf object").expect("write test bytecode");
        let preparer = HostWireGuardEbpfPreparer::with_command_plans(
            machine_id("machine_a"),
            [HostCommandPlan::readiness_ployz_tc_bytecode(&path)],
        );

        let error = preparer
            .prepare_wireguard_ebpf(&[], &[])
            .await
            .expect_err("missing bytecode symbols fail");

        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: WireGuardEbpfComponent::EbpfForwarding,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn wireguard_public_key(value: &str) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
    }
}
