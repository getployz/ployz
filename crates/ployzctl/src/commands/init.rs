use clap::Args;
use ployz_core::ids::MachineId;
use ployz_core::install::{FirstMachineInstallSpec, NatsMachineMaterialPaths};
pub use ployz_core::roles::{GatewayRole, InstallRolePolicy, plan_first_machine_process_set};
use std::fs;
use std::io::Read;

use super::role_policy::RolePolicyCli;
use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use ployz_sdk_types::InitFirstMachineActivateRequest;

pub mod join_template;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstMachineInitCommand {
    pub mode: FirstMachineInitMode,
}

impl FirstMachineInitCommand {
    #[must_use]
    pub fn summary(machine_id: MachineId, roles: InstallRolePolicy) -> Self {
        Self {
            mode: FirstMachineInitMode::Summary { machine_id, roles },
        }
    }

    #[must_use]
    pub fn keeper_install(keeper_install: FirstMachineInstallSpec) -> Self {
        Self {
            mode: FirstMachineInitMode::EmitKeeperInstall(keeper_install),
        }
    }

    #[must_use]
    pub fn run_keeper_install(
        keeper_install: FirstMachineInstallSpec,
        keeper_binary: String,
    ) -> Self {
        Self {
            mode: FirstMachineInitMode::RunKeeperInstall {
                keeper_install,
                keeper_binary,
            },
        }
    }

    #[must_use]
    pub fn machine_id(&self) -> &MachineId {
        match &self.mode {
            FirstMachineInitMode::Summary { machine_id, .. }
            | FirstMachineInitMode::EmitKeeperInstall(FirstMachineInstallSpec {
                machine_id, ..
            })
            | FirstMachineInitMode::RunKeeperInstall {
                keeper_install: FirstMachineInstallSpec { machine_id, .. },
                ..
            } => machine_id,
        }
    }

    #[must_use]
    pub fn roles(&self) -> InstallRolePolicy {
        match &self.mode {
            FirstMachineInitMode::Summary { roles, .. } => *roles,
            FirstMachineInitMode::EmitKeeperInstall(spec)
            | FirstMachineInitMode::RunKeeperInstall {
                keeper_install: spec,
                ..
            } => spec.role_policy(),
        }
    }

    #[must_use]
    pub fn output(&self) -> FirstMachineInitOutput {
        FirstMachineInitOutput {
            mode: self.mode.clone(),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.output().render()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstMachineInitMode {
    Summary {
        machine_id: MachineId,
        roles: InstallRolePolicy,
    },
    EmitKeeperInstall(FirstMachineInstallSpec),
    RunKeeperInstall {
        keeper_install: FirstMachineInstallSpec,
        keeper_binary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstMachineActivateCommand {
    pub machine_id: MachineId,
    pub roles: InstallRolePolicy,
}

impl FirstMachineActivateCommand {
    #[must_use]
    pub const fn new(machine_id: MachineId, roles: InstallRolePolicy) -> Self {
        Self { machine_id, roles }
    }

    #[must_use]
    pub fn into_request(self) -> InitFirstMachineActivateRequest {
        InitFirstMachineActivateRequest {
            machine_id: self.machine_id,
            roles: self.roles,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstMachineActivationOutput {
    pub operation_id: ployz_core::ids::OperationId,
    pub machine_id: MachineId,
}

impl FirstMachineActivationOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nfirst-machine {} active\n",
            self.operation_id.as_str(),
            self.machine_id.as_str()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstMachineInitOutput {
    pub mode: FirstMachineInitMode,
}

impl FirstMachineInitOutput {
    #[must_use]
    pub fn summary(machine_id: MachineId, roles: InstallRolePolicy) -> Self {
        Self {
            mode: FirstMachineInitMode::Summary { machine_id, roles },
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let (machine_id, roles, keeper_install) = match &self.mode {
            FirstMachineInitMode::Summary { machine_id, roles } => (machine_id, *roles, None),
            FirstMachineInitMode::EmitKeeperInstall(keeper_install) => (
                &keeper_install.machine_id,
                keeper_install.role_policy(),
                Some(keeper_install),
            ),
            FirstMachineInitMode::RunKeeperInstall { keeper_install, .. } => (
                &keeper_install.machine_id,
                keeper_install.role_policy(),
                None,
            ),
        };
        let process_set = plan_first_machine_process_set(machine_id, roles);
        let roles = process_set
            .roles()
            .iter()
            .map(|role| role.process_name())
            .collect::<Vec<_>>()
            .join(" ");

        let mut output = format!(
            "init first machine {}\nsupervise {}\nsupervise roles {}\n",
            machine_id.as_str(),
            process_set.nats_server.process_name(),
            roles
        );

        if let Some(keeper_install) = keeper_install {
            output.push_str("install ployz-keeper first-machine-install --spec -\n");
            output.push_str(&render_first_machine_credential_paths());
            output.push_str(
                &serde_json::to_string_pretty(keeper_install)
                    .expect("first-machine install spec serializes"),
            );
            output.push('\n');
        }

        output
    }
}

/// Where the keeper install leaves the operator credential and cluster CA
/// that `ployzctl` connects with.
#[must_use]
pub fn render_first_machine_credential_paths() -> String {
    let paths = NatsMachineMaterialPaths::in_default_state_dir();
    format!(
        "operator seed {}\ncluster ca {}\n",
        paths.operator_seed_file().display(),
        paths.ca_file().display()
    )
}

pub(crate) fn init_command(parsed: InitCli) -> Result<FirstMachineInitCommand, PloyzctlCliError> {
    let InitCli {
        machine,
        roles,
        emit_keeper_install,
        run_keeper_install,
        install_spec,
        keeper_binary,
    } = parsed;
    // clap enforces the flag combinations: the keeper-install modes conflict
    // with each other and with --machine, both modes require
    // --install-spec, and --keeper-binary requires --run-keeper-install.
    match (emit_keeper_install, run_keeper_install) {
        (false, false) => {
            let machine_id =
                MachineId::try_new(machine.ok_or_else(|| cli_error("--machine is required"))?)
                    .map_err(|error| invalid_value("--machine", error))?;
            Ok(FirstMachineInitCommand::summary(
                machine_id,
                roles.into_policy(),
            ))
        }
        (true, _) => {
            reject_keeper_mode_role_flags(roles)?;
            Ok(FirstMachineInitCommand::keeper_install(
                read_first_machine_install_spec(
                    install_spec.ok_or_else(|| cli_error("--install-spec is required"))?,
                )?,
            ))
        }
        (false, true) => {
            reject_keeper_mode_role_flags(roles)?;
            let keeper_install = read_first_machine_install_spec(
                install_spec.ok_or_else(|| cli_error("--install-spec is required"))?,
            )?;
            let keeper_binary = keeper_binary.unwrap_or_else(|| "ployz-keeper".to_owned());
            if keeper_binary.is_empty() {
                return Err(PloyzctlCliError::InvalidValue {
                    flag: "--keeper-binary",
                    message: "keeper binary is empty".to_owned(),
                });
            }
            Ok(FirstMachineInitCommand::run_keeper_install(
                keeper_install,
                keeper_binary,
            ))
        }
    }
}

#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("keeper_install_mode")
    .args(["emit_keeper_install", "run_keeper_install"])
    .multiple(false))]
pub(crate) struct InitCli {
    #[arg(long, conflicts_with = "keeper_install_mode")]
    machine: Option<String>,
    #[command(flatten)]
    roles: RolePolicyCli,
    #[arg(long, requires = "install_spec")]
    emit_keeper_install: bool,
    #[arg(long, requires = "install_spec")]
    run_keeper_install: bool,
    #[arg(long, requires = "keeper_install_mode")]
    install_spec: Option<String>,
    // `requires` must point at the group: a bare flag's false default would
    // satisfy `requires = "run_keeper_install"`, while group presence needs
    // explicit usage. The conflict pins it to the run mode.
    #[arg(
        long,
        requires = "keeper_install_mode",
        conflicts_with = "emit_keeper_install"
    )]
    keeper_binary: Option<String>,
}

pub(crate) fn first_machine_activate_command(
    parsed: FirstMachineActivateCli,
) -> Result<FirstMachineActivateCommand, PloyzctlCliError> {
    let machine_id =
        MachineId::try_new(parsed.machine).map_err(|error| invalid_value("--machine", error))?;
    Ok(FirstMachineActivateCommand::new(
        machine_id,
        parsed.roles.into_policy(),
    ))
}

#[derive(Debug, Args)]
pub(crate) struct FirstMachineActivateCli {
    #[arg(long)]
    machine: String,
    #[command(flatten)]
    roles: RolePolicyCli,
}

fn reject_keeper_mode_role_flags(roles: RolePolicyCli) -> Result<(), PloyzctlCliError> {
    if roles.has_explicit_flags() {
        return Err(PloyzctlCliError::InvalidValue {
            flag: "--no-gateway/--no-dns",
            message:
                "role opt-outs belong in the first-machine install spec for keeper install modes"
                    .to_owned(),
        });
    }
    Ok(())
}

fn read_first_machine_install_spec(
    path: String,
) -> Result<FirstMachineInstallSpec, PloyzctlCliError> {
    let mut contents = String::new();
    if path == "-" {
        std::io::stdin()
            .read_to_string(&mut contents)
            .map_err(|error| PloyzctlCliError::InvalidValue {
                flag: "--install-spec",
                message: format!("cannot read stdin: {error}"),
            })?;
    } else {
        contents = fs::read_to_string(&path).map_err(|error| PloyzctlCliError::InvalidValue {
            flag: "--install-spec",
            message: format!("cannot read {path}: {error}"),
        })?;
    }
    serde_json::from_str(&contents).map_err(|error| PloyzctlCliError::InvalidValue {
        flag: "--install-spec",
        message: format!("invalid first-machine install spec json: {error}"),
    })
}
