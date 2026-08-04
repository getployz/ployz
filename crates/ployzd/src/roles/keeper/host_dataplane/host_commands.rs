//! Bounded privileged command plans used by Keeper convergence.

use ployz_core::ids::MachineId;
use ployz_core::network::{
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, WireGuardEbpfPrepareError,
    WireGuardReadyEvidence,
};
use ployz_core::operation::FailureMessage;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

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
    SysctlSet {
        component: PloyzNativeMeshComponent,
        name: String,
        value: String,
    },
    PloyzTcBytecode {
        path: PathBuf,
    },
    /// Ensure `path` is a live bpffs mount, mounting it if absent, so eBPF pins
    /// can be created. A bare directory that is not bpffs fails loudly here
    /// instead of silently reporting detached after pinning fails.
    EnsureBpfFs {
        path: PathBuf,
    },
}

impl HostCommandPlan {
    #[must_use]
    pub(super) const fn component(&self) -> PloyzNativeMeshComponent {
        match &self.action {
            HostCommandAction::ExistingPath { component, .. }
            | HostCommandAction::CommandSucceeds { component, .. }
            | HostCommandAction::SysctlSet { component, .. } => *component,
            HostCommandAction::PloyzTcBytecode { .. } | HostCommandAction::EnsureBpfFs { .. } => {
                PloyzNativeMeshComponent::EbpfForwarding
            }
        }
    }

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
    pub(super) fn provisioning_sysctl(
        component: PloyzNativeMeshComponent,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            action: HostCommandAction::SysctlSet {
                component,
                name: name.into(),
                value: value.into(),
            },
        }
    }

    #[must_use]
    pub(super) fn readiness_ployz_tc_bytecode(path: impl Into<PathBuf>) -> Self {
        Self {
            action: HostCommandAction::PloyzTcBytecode { path: path.into() },
        }
    }

    #[must_use]
    pub(super) fn ensure_bpffs(path: impl Into<PathBuf>) -> Self {
        Self {
            action: HostCommandAction::EnsureBpfFs { path: path.into() },
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
                HostCommandOutcome::Success => Ok(component_ready_command(
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
            HostCommandAction::SysctlSet {
                component,
                name,
                value,
            } => {
                use sysctl::Sysctl;

                let ctl = sysctl::Ctl::new(name.as_str()).map_err(|source| {
                    unavailable(
                        machine_id,
                        *component,
                        format!("required sysctl could not be opened: {name}: {source}"),
                    )
                })?;
                ctl.set_value(sysctl::CtlValue::String(value.clone()))
                    .map_err(|source| {
                        unavailable(
                            machine_id,
                            *component,
                            format!("required sysctl could not be set: {name}={value}: {source}"),
                        )
                    })?;
                Ok(component_ready_command(
                    *component,
                    "sysctl".to_owned(),
                    vec!["-w".to_owned(), format!("{name}={value}")],
                ))
            }
            HostCommandAction::PloyzTcBytecode { path } => {
                let symbols = validate_ployz_tc_bytecode(machine_id, path)?;
                Ok(HostDataplaneEvidence::EbpfForwarding(
                    EbpfForwardingReadyEvidence::PloyzTcBytecode {
                        path: path.display().to_string(),
                        symbols,
                    },
                ))
            }
            HostCommandAction::EnsureBpfFs { path } => {
                ensure_bpffs_mounted(machine_id, path, command_timeout).await
            }
        }
    }
}

/// `BPF_FS_MAGIC` from `linux/magic.h` — the `statfs.f_type` of a bpffs mount.
const BPF_FS_MAGIC: u32 = 0xcafe_4a11;

fn is_bpffs(path: &Path) -> std::io::Result<bool> {
    let stat = rustix::fs::statfs(path)?;
    Ok(stat.f_type as u32 == BPF_FS_MAGIC)
}

/// Make `path` a live bpffs mount so eBPF pins can be created, mounting it if
/// absent (ployzd runs as root). Distinguishes "bpffs not mounted" from a
/// failed mount so the failure states WHY instead of a later silent detach.
async fn ensure_bpffs_mounted(
    machine_id: &MachineId,
    path: &Path,
    command_timeout: Duration,
) -> Result<HostDataplaneEvidence, WireGuardEbpfPrepareError> {
    let component = PloyzNativeMeshComponent::EbpfForwarding;
    match is_bpffs(path) {
        Ok(true) => return Ok(component_ready_path(component, path.display().to_string())),
        Ok(false) => {}
        Err(source) => {
            return Err(unavailable(
                machine_id,
                component,
                format!(
                    "bpffs mount at {} could not be inspected: {source}",
                    path.display()
                ),
            ));
        }
    }

    let mount_args = vec![
        "-t".to_owned(),
        "bpf".to_owned(),
        "bpf".to_owned(),
        path.display().to_string(),
    ];
    match run_host_command("mount", &mount_args, command_timeout).await {
        HostCommandOutcome::Success => {}
        HostCommandOutcome::TimedOut => {
            return Err(unavailable(
                machine_id,
                component,
                format!(
                    "bpffs is not mounted at {} and mounting it timed out after {}s",
                    path.display(),
                    command_timeout.as_secs()
                ),
            ));
        }
        HostCommandOutcome::Failed(output) => {
            return Err(unavailable(
                machine_id,
                component,
                format!(
                    "bpffs is not mounted at {} and mounting it failed: mount {}: {}",
                    path.display(),
                    mount_args.join(" "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        HostCommandOutcome::CouldNotStart(source) => {
            return Err(unavailable(
                machine_id,
                component,
                format!(
                    "bpffs is not mounted at {} and the mount command could not start: {source}",
                    path.display()
                ),
            ));
        }
    }

    match is_bpffs(path) {
        Ok(true) => Ok(component_ready_command(
            component,
            "mount".to_owned(),
            mount_args,
        )),
        Ok(false) => Err(unavailable(
            machine_id,
            component,
            format!(
                "bpffs still not present at {} after mounting; eBPF pins cannot be created",
                path.display()
            ),
        )),
        Err(source) => Err(unavailable(
            machine_id,
            component,
            format!(
                "bpffs mount at {} could not be verified after mounting: {source}",
                path.display()
            ),
        )),
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
    Success,
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
        Ok(Ok(output)) if output.status.success() => HostCommandOutcome::Success,
        Ok(Ok(output)) => HostCommandOutcome::Failed(output),
    }
}

pub(super) fn default_command_plans(
    ebpf_bytecode_path: PathBuf,
    ebpf_ctl_path: PathBuf,
    bridge_ifname: String,
    wg_ifname: String,
    _private_key_path: PathBuf,
    _listen_port: u16,
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
    vec![
        HostCommandPlan::ensure_bpffs("/sys/fs/bpf"),
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
    ]
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
