use clap::Args;
use ployz_core::ids::NodeId;
use ployz_core::install::{FirstNodeInstallSpec, NatsMachineMaterialPaths};
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};
use std::fs;
use std::io::Read;

use crate::commands::{PloyzctlCliError, cli_error, invalid_value};
use ployz_sdk_types::{InitFirstNodeActivateRequest, MachineAddGateway};

pub mod join_template;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInitCommand {
    pub mode: FirstNodeInitMode,
}

impl FirstNodeInitCommand {
    #[must_use]
    pub fn summary(node_id: NodeId, gateway: FirstNodeGateway) -> Self {
        Self {
            mode: FirstNodeInitMode::Summary { node_id, gateway },
        }
    }

    #[must_use]
    pub fn keeper_install(keeper_install: FirstNodeInstallSpec) -> Self {
        Self {
            mode: FirstNodeInitMode::EmitKeeperInstall(keeper_install),
        }
    }

    #[must_use]
    pub fn run_keeper_install(keeper_install: FirstNodeInstallSpec, keeper_binary: String) -> Self {
        Self {
            mode: FirstNodeInitMode::RunKeeperInstall {
                keeper_install,
                keeper_binary,
            },
        }
    }

    #[must_use]
    pub fn node_id(&self) -> &NodeId {
        match &self.mode {
            FirstNodeInitMode::Summary { node_id, .. }
            | FirstNodeInitMode::EmitKeeperInstall(FirstNodeInstallSpec { node_id, .. })
            | FirstNodeInitMode::RunKeeperInstall {
                keeper_install: FirstNodeInstallSpec { node_id, .. },
                ..
            } => node_id,
        }
    }

    #[must_use]
    pub fn gateway(&self) -> FirstNodeGateway {
        match &self.mode {
            FirstNodeInitMode::Summary { gateway, .. }
            | FirstNodeInitMode::EmitKeeperInstall(FirstNodeInstallSpec { gateway, .. })
            | FirstNodeInitMode::RunKeeperInstall {
                keeper_install: FirstNodeInstallSpec { gateway, .. },
                ..
            } => *gateway,
        }
    }

    #[must_use]
    pub fn output(&self) -> FirstNodeInitOutput {
        FirstNodeInitOutput {
            mode: self.mode.clone(),
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.output().render()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstNodeInitMode {
    Summary {
        node_id: NodeId,
        gateway: FirstNodeGateway,
    },
    EmitKeeperInstall(FirstNodeInstallSpec),
    RunKeeperInstall {
        keeper_install: FirstNodeInstallSpec,
        keeper_binary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeActivateCommand {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
}

impl FirstNodeActivateCommand {
    #[must_use]
    pub const fn new(node_id: NodeId, gateway: FirstNodeGateway) -> Self {
        Self { node_id, gateway }
    }

    #[must_use]
    pub fn into_request(self) -> InitFirstNodeActivateRequest {
        InitFirstNodeActivateRequest {
            node_id: self.node_id,
            gateway: match self.gateway {
                FirstNodeGateway::Install => MachineAddGateway::Install,
                FirstNodeGateway::Skip => MachineAddGateway::Skip,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeActivationOutput {
    pub operation_id: ployz_core::ids::OperationId,
    pub node_id: NodeId,
}

impl FirstNodeActivationOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nfirst-node {} active\n",
            self.operation_id.as_str(),
            self.node_id.as_str()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeInitOutput {
    pub mode: FirstNodeInitMode,
}

impl FirstNodeInitOutput {
    #[must_use]
    pub fn summary(node_id: NodeId, gateway: FirstNodeGateway) -> Self {
        Self {
            mode: FirstNodeInitMode::Summary { node_id, gateway },
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let (node_id, gateway, keeper_install) = match &self.mode {
            FirstNodeInitMode::Summary { node_id, gateway } => (node_id, *gateway, None),
            FirstNodeInitMode::EmitKeeperInstall(keeper_install) => (
                &keeper_install.node_id,
                keeper_install.gateway,
                Some(keeper_install),
            ),
            FirstNodeInitMode::RunKeeperInstall { keeper_install, .. } => {
                (&keeper_install.node_id, keeper_install.gateway, None)
            }
        };
        let process_set = first_node_process_set(node_id, gateway);
        let roles = process_set
            .roles()
            .iter()
            .map(|role| role.process_name())
            .collect::<Vec<_>>()
            .join(" ");

        let mut output = format!(
            "init first node {}\nsupervise {}\nsupervise roles {}\n",
            node_id.as_str(),
            process_set.nats_server.process_name(),
            roles
        );

        if let Some(keeper_install) = keeper_install {
            output.push_str("install ployz-keeper first-node-install --spec -\n");
            output.push_str(&render_first_node_credential_paths());
            output.push_str(
                &serde_json::to_string_pretty(keeper_install)
                    .expect("first-node install spec serializes"),
            );
            output.push('\n');
        }

        output
    }
}

/// Where the keeper install leaves the operator credential and cluster CA
/// that `ployzctl` connects with.
#[must_use]
pub fn render_first_node_credential_paths() -> String {
    let paths = NatsMachineMaterialPaths::in_default_state_dir();
    format!(
        "operator seed {}\ncluster ca {}\n",
        paths.operator_seed_file().display(),
        paths.ca_file().display()
    )
}

pub(crate) fn init_command(parsed: InitCli) -> Result<FirstNodeInitCommand, PloyzctlCliError> {
    let InitCli {
        node,
        gateway,
        emit_keeper_install,
        run_keeper_install,
        install_spec,
        keeper_binary,
    } = parsed;
    // clap enforces the flag combinations: the keeper-install modes conflict
    // with each other and with --node/--gateway, both modes require
    // --install-spec, and --keeper-binary requires --run-keeper-install.
    match (emit_keeper_install, run_keeper_install) {
        (false, false) => {
            let node_id = NodeId::try_new(node.ok_or_else(|| cli_error("--node is required"))?)
                .map_err(|error| invalid_value("--node", error))?;
            Ok(FirstNodeInitCommand::summary(
                node_id,
                if gateway {
                    FirstNodeGateway::Install
                } else {
                    FirstNodeGateway::Skip
                },
            ))
        }
        (true, _) => Ok(FirstNodeInitCommand::keeper_install(
            read_first_node_install_spec(
                install_spec.ok_or_else(|| cli_error("--install-spec is required"))?,
            )?,
        )),
        (false, true) => {
            let keeper_install = read_first_node_install_spec(
                install_spec.ok_or_else(|| cli_error("--install-spec is required"))?,
            )?;
            let keeper_binary = keeper_binary.unwrap_or_else(|| "ployz-keeper".to_owned());
            if keeper_binary.is_empty() {
                return Err(PloyzctlCliError::InvalidValue {
                    flag: "--keeper-binary",
                    message: "keeper binary is empty".to_owned(),
                });
            }
            Ok(FirstNodeInitCommand::run_keeper_install(
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
    node: Option<String>,
    #[arg(long, conflicts_with = "keeper_install_mode")]
    gateway: bool,
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

pub(crate) fn first_node_activate_command(
    parsed: FirstNodeActivateCli,
) -> Result<FirstNodeActivateCommand, PloyzctlCliError> {
    let node_id = NodeId::try_new(parsed.node).map_err(|error| invalid_value("--node", error))?;
    Ok(FirstNodeActivateCommand::new(
        node_id,
        if parsed.gateway {
            FirstNodeGateway::Install
        } else {
            FirstNodeGateway::Skip
        },
    ))
}

#[derive(Debug, Args)]
pub(crate) struct FirstNodeActivateCli {
    #[arg(long)]
    node: String,
    #[arg(long)]
    gateway: bool,
}

fn read_first_node_install_spec(path: String) -> Result<FirstNodeInstallSpec, PloyzctlCliError> {
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
        message: format!("invalid first-node install spec json: {error}"),
    })
}
