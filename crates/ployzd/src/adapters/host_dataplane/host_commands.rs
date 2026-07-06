use ployz_core::dataplane::{
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, WireGuardEbpfPrepareError,
    WireGuardReadyEvidence,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_WIREGUARD_KEY_DIR: &str = "/etc/ployz";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HostDataplaneEvidence {
    WireGuard(WireGuardReadyEvidence),
    EbpfForwarding(EbpfForwardingReadyEvidence),
}

/// One planned host action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HostCommandPlan {
    pub(super) action: HostCommandAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HostCommandAction {
    ExistingPath {
        component: PloyzNativeMeshComponent,
        path: PathBuf,
    },
    CommandSucceeds {
        component: PloyzNativeMeshComponent,
        program: String,
        args: Vec<String>,
    },
    PloyzTcBytecode {
        path: PathBuf,
    },
}

impl HostCommandPlan {
    #[must_use]
    pub(super) fn readiness_path(
        component: PloyzNativeMeshComponent,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            action: HostCommandAction::ExistingPath {
                component,
                path: path.into(),
            },
        }
    }

    #[must_use]
    pub(super) fn readiness_command(
        component: PloyzNativeMeshComponent,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            action: command_action(component, program, args),
        }
    }

    #[must_use]
    pub(super) fn provisioning_command(
        component: PloyzNativeMeshComponent,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            action: command_action(component, program, args),
        }
    }

    #[must_use]
    pub(super) fn readiness_ployz_tc_bytecode(path: impl Into<PathBuf>) -> Self {
        Self {
            action: HostCommandAction::PloyzTcBytecode { path: path.into() },
        }
    }

    pub(super) async fn run(
        &self,
        machine_id: &MachineId,
        command_timeout: Duration,
    ) -> Result<HostDataplaneEvidence, WireGuardEbpfPrepareError> {
        let Self { action } = self;
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
    component: PloyzNativeMeshComponent,
    program: impl Into<String>,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> HostCommandAction {
    HostCommandAction::CommandSucceeds {
        component,
        program: program.into(),
        args: args.into_iter().map(Into::into).collect(),
    }
}

pub(super) enum HostCommandOutcome {
    Success(std::process::Output),
    Failed(std::process::Output),
    TimedOut,
    CouldNotStart(std::io::Error),
}

pub(super) async fn run_host_command(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> HostCommandOutcome {
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => HostCommandOutcome::TimedOut,
        Ok(Err(source)) => HostCommandOutcome::CouldNotStart(source),
        Ok(Ok(output)) if output.status.success() => HostCommandOutcome::Success(output),
        Ok(Ok(output)) => HostCommandOutcome::Failed(output),
    }
}

pub(super) fn default_command_plans(
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
        HostCommandPlan::readiness_path(PloyzNativeMeshComponent::EbpfForwarding, "/sys/fs/bpf"),
        HostCommandPlan::readiness_command(PloyzNativeMeshComponent::EbpfForwarding, "tc", ["-V"]),
        HostCommandPlan::readiness_path(PloyzNativeMeshComponent::EbpfForwarding, ebpf_ctl_path),
        HostCommandPlan::readiness_command(
            PloyzNativeMeshComponent::EbpfForwarding,
            ebpf_ctl_program.clone(),
            ["validate".to_owned(), ebpf_bytecode_arg.clone()],
        ),
        HostCommandPlan::readiness_ployz_tc_bytecode(ebpf_bytecode_path),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::EbpfForwarding,
            ebpf_ctl_program,
            ensure_attached_args,
        ),
    ]);
    plans
}

pub(super) fn ebpf_ctl_args(
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
pub(super) fn wireguard_interface_plans(
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
        HostCommandPlan::readiness_path(PloyzNativeMeshComponent::WireGuard, "/dev/net/tun"),
        HostCommandPlan::readiness_command(PloyzNativeMeshComponent::WireGuard, "wg", ["--version"]),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "install",
            [
                "-d".to_owned(),
                "-m".to_owned(),
                "0700".to_owned(),
                private_key_dir,
            ],
        ),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "test -s \"$1\" || (umask 077 && wg genkey > \"$1\")".to_owned(),
                "--".to_owned(),
                private_key_arg.clone(),
            ],
        ),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "sh",
            [
                "-c".to_owned(),
                "if [ -f /etc/apparmor.d/wg ] && command -v apparmor_parser >/dev/null 2>&1; then install -d -m 0755 /etc/apparmor.d/local; touch /etc/apparmor.d/local/wg; if ! grep -qxF \"  $1 r,\" /etc/apparmor.d/local/wg; then printf '\\n  %s r,\\n' \"$1\" >> /etc/apparmor.d/local/wg; fi; apparmor_parser -r /etc/apparmor.d/wg; fi".to_owned(),
                "--".to_owned(),
                private_key_arg.clone(),
            ],
        ),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
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
            PloyzNativeMeshComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "private-key".to_owned(),
                private_key_arg,
            ],
        ),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "wg",
            [
                "set".to_owned(),
                wg_ifname.clone(),
                "listen-port".to_owned(),
                listen_port.to_string(),
            ],
        ),
        HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
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
            PloyzNativeMeshComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode could not be read: {}: {source}",
                path.display()
            ),
        )
    })?;
    ployz_ebpf_common::validate_ployz_tc_bytecode(bytes.as_slice()).map_err(|source| {
        unavailable(
            machine_id,
            PloyzNativeMeshComponent::EbpfForwarding,
            format!(
                "required eBPF bytecode is not valid Ployz TC bytecode: {}: {source:?}",
                path.display()
            ),
        )
    })
}

fn component_ready_path(
    component: PloyzNativeMeshComponent,
    path: String,
) -> HostDataplaneEvidence {
    match component {
        PloyzNativeMeshComponent::WireGuard => {
            HostDataplaneEvidence::WireGuard(WireGuardReadyEvidence::HostPath { path })
        }
        PloyzNativeMeshComponent::EbpfForwarding => {
            HostDataplaneEvidence::EbpfForwarding(EbpfForwardingReadyEvidence::HostPath { path })
        }
    }
}

fn component_ready_command(
    component: PloyzNativeMeshComponent,
    program: String,
    args: Vec<String>,
) -> HostDataplaneEvidence {
    match component {
        PloyzNativeMeshComponent::WireGuard => {
            HostDataplaneEvidence::WireGuard(WireGuardReadyEvidence::Command { program, args })
        }
        PloyzNativeMeshComponent::EbpfForwarding => {
            HostDataplaneEvidence::EbpfForwarding(EbpfForwardingReadyEvidence::Command {
                program,
                args,
            })
        }
    }
}

pub(super) fn unavailable(
    machine_id: &MachineId,
    component: PloyzNativeMeshComponent,
    message: String,
) -> WireGuardEbpfPrepareError {
    WireGuardEbpfPrepareError::Unavailable {
        machine_id: machine_id.clone(),
        component,
        message: FailureMessage::try_new(message)
            .expect("generated dataplane failure message is non-empty"),
    }
}
