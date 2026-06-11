use clap::Parser;
use ployz_core::ids::NodeId;
use ployz_core::install::{FirstNodeInstallSpec, KeeperFirstNodeInstall, NatsMachineMaterialPaths};
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};
use std::fs;
use std::io::Read;

use crate::commands::{PloyzctlCliError, clap_error, cli_error, invalid_value};
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
    pub fn keeper_install(keeper_install: KeeperFirstNodeInstall) -> Self {
        Self {
            mode: FirstNodeInitMode::EmitKeeperInstall(keeper_install),
        }
    }

    #[must_use]
    pub fn run_keeper_install(
        keeper_install: KeeperFirstNodeInstall,
        keeper_binary: String,
    ) -> Self {
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
            | FirstNodeInitMode::EmitKeeperInstall(KeeperFirstNodeInstall { node_id, .. })
            | FirstNodeInitMode::RunKeeperInstall {
                keeper_install: KeeperFirstNodeInstall { node_id, .. },
                ..
            } => node_id,
        }
    }

    #[must_use]
    pub fn gateway(&self) -> FirstNodeGateway {
        match &self.mode {
            FirstNodeInitMode::Summary { gateway, .. }
            | FirstNodeInitMode::EmitKeeperInstall(KeeperFirstNodeInstall { gateway, .. })
            | FirstNodeInitMode::RunKeeperInstall {
                keeper_install: KeeperFirstNodeInstall { gateway, .. },
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
    EmitKeeperInstall(KeeperFirstNodeInstall),
    RunKeeperInstall {
        keeper_install: KeeperFirstNodeInstall,
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
                &serde_json::to_string_pretty(&FirstNodeInstallSpec::from(keeper_install.clone()))
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

pub fn parse_init_command(args: &[String]) -> Result<FirstNodeInitCommand, PloyzctlCliError> {
    let parsed =
        InitCli::try_parse_from(std::iter::once("init".to_owned()).chain(args.iter().cloned()))
            .map_err(clap_error)?;
    let keeper_install_mode = match (parsed.emit_keeper_install, parsed.run_keeper_install) {
        (false, false) => ParsedKeeperInstallMode::None,
        (true, false) => ParsedKeeperInstallMode::Emit,
        (false, true) => ParsedKeeperInstallMode::Run,
        (true, true) => {
            return Err(cli_error(
                "--emit-keeper-install and --run-keeper-install cannot be used together",
            ));
        }
    };

    match keeper_install_mode {
        ParsedKeeperInstallMode::None => {
            if parsed.install_spec.is_some() || parsed.keeper_binary.is_some() {
                return Err(cli_error(
                    "--install-spec and --keeper-binary require --emit-keeper-install or --run-keeper-install",
                ));
            }
            let node_id =
                NodeId::try_new(parsed.node.ok_or_else(|| cli_error("--node is required"))?)
                    .map_err(|error| invalid_value("--node", error))?;
            Ok(FirstNodeInitCommand::summary(
                node_id,
                if parsed.gateway {
                    FirstNodeGateway::Install
                } else {
                    FirstNodeGateway::Skip
                },
            ))
        }
        ParsedKeeperInstallMode::Emit | ParsedKeeperInstallMode::Run => {
            if parsed.node.is_some() {
                return Err(cli_error(
                    "--node cannot be used with --emit-keeper-install or --run-keeper-install",
                ));
            }
            if parsed.gateway {
                return Err(cli_error(
                    "--gateway cannot be used with --emit-keeper-install or --run-keeper-install",
                ));
            }
            let keeper_install = KeeperFirstNodeInstall::from(read_first_node_install_spec(
                parsed
                    .install_spec
                    .ok_or_else(|| cli_error("--install-spec is required"))?,
            )?);
            match keeper_install_mode {
                ParsedKeeperInstallMode::None => unreachable!("handled above"),
                ParsedKeeperInstallMode::Emit => {
                    if parsed.keeper_binary.is_some() {
                        return Err(cli_error("--keeper-binary requires --run-keeper-install"));
                    }
                    Ok(FirstNodeInitCommand::keeper_install(keeper_install))
                }
                ParsedKeeperInstallMode::Run => {
                    let keeper_binary = parsed
                        .keeper_binary
                        .unwrap_or_else(|| "ployz-keeper".to_owned());
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
    }
}

#[derive(Debug, Parser)]
#[command(name = "init")]
struct InitCli {
    #[arg(long, conflicts_with_all = ["emit_keeper_install", "run_keeper_install"])]
    node: Option<String>,
    #[arg(long, conflicts_with_all = ["emit_keeper_install", "run_keeper_install"])]
    gateway: bool,
    #[arg(long, conflicts_with = "run_keeper_install")]
    emit_keeper_install: bool,
    #[arg(long)]
    run_keeper_install: bool,
    #[arg(long)]
    install_spec: Option<String>,
    #[arg(long, requires = "run_keeper_install")]
    keeper_binary: Option<String>,
}

pub fn parse_first_node_activate_command(
    args: &[String],
) -> Result<FirstNodeActivateCommand, PloyzctlCliError> {
    let parsed = FirstNodeActivateCli::try_parse_from(
        std::iter::once("init activate-first-node".to_owned()).chain(args.iter().cloned()),
    )
    .map_err(clap_error)?;
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

#[derive(Debug, Parser)]
#[command(name = "init activate-first-node")]
struct FirstNodeActivateCli {
    #[arg(long)]
    node: String,
    #[arg(long)]
    gateway: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedKeeperInstallMode {
    None,
    Emit,
    Run,
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
