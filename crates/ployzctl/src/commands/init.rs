use ployz_core::ids::NodeId;
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    KeeperFirstNodeInstall, MachineBootstrapUrl,
};
pub use ployz_core::roles::{FirstNodeGateway, first_node_process_set};

use crate::commands::{ArgCursor, PloyzctlCliError, invalid_value, required, set_once};

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
            output.push_str("install ");
            output.push_str(&keeper_install.render_command());
            output.push('\n');
        }

        output
    }
}

pub fn parse_init_command(args: &[String]) -> Result<FirstNodeInitCommand, PloyzctlCliError> {
    let mut parsed = ParsedInitArgs::default();
    let mut args = ArgCursor::new(args);

    while !args.is_empty() {
        if args.take_flag("--emit-keeper-install") {
            parsed.set_keeper_install_mode(ParsedKeeperInstallMode::Emit)?;
            continue;
        }
        if args.take_flag("--run-keeper-install") {
            parsed.set_keeper_install_mode(ParsedKeeperInstallMode::Run)?;
            continue;
        }
        if args.take_flag("--gateway") {
            if parsed.gateway == FirstNodeGateway::Install {
                return Err(PloyzctlCliError::DuplicateArgument { flag: "--gateway" });
            }
            parsed.gateway = FirstNodeGateway::Install;
            continue;
        }
        if let Some(value) = args.take_value("--node")? {
            set_once(&mut parsed.node_id, value, "--node")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-version")? {
            set_once(&mut parsed.ployzd_version, value, "--ployzd-version")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-source")? {
            set_once(&mut parsed.ployzd_source, value, "--ployzd-source")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-sha256")? {
            set_once(&mut parsed.ployzd_sha256, value, "--ployzd-sha256")?;
            continue;
        }
        if let Some(value) = args.take_value("--ployzd-install-path")? {
            set_once(
                &mut parsed.ployzd_install_path,
                value,
                "--ployzd-install-path",
            )?;
            continue;
        }
        if let Some(value) = args.take_value("--nats-version")? {
            set_once(&mut parsed.nats_version, value, "--nats-version")?;
            continue;
        }
        if let Some(value) = args.take_value("--nats-source")? {
            set_once(&mut parsed.nats_source, value, "--nats-source")?;
            continue;
        }
        if let Some(value) = args.take_value("--nats-sha256")? {
            set_once(&mut parsed.nats_sha256, value, "--nats-sha256")?;
            continue;
        }
        if let Some(value) = args.take_value("--nats-binary")? {
            set_once(&mut parsed.nats_binary, value, "--nats-binary")?;
            continue;
        }
        if let Some(value) = args.take_value("--nats-config")? {
            set_once(&mut parsed.nats_config, value, "--nats-config")?;
            continue;
        }
        if let Some(value) = args.take_value("--machine-bootstrap-url")? {
            set_once(
                &mut parsed.machine_bootstrap_url,
                value,
                "--machine-bootstrap-url",
            )?;
            continue;
        }
        if let Some(value) = args.take_value("--keeper-binary")? {
            set_once(&mut parsed.keeper_binary, value, "--keeper-binary")?;
            continue;
        }
        return Err(args.unexpected());
    }

    parsed.into_command()
}

struct ParsedInitArgs {
    keeper_install_mode: ParsedKeeperInstallMode,
    node_id: Option<String>,
    gateway: FirstNodeGateway,
    ployzd_version: Option<String>,
    ployzd_source: Option<String>,
    ployzd_sha256: Option<String>,
    ployzd_install_path: Option<String>,
    nats_version: Option<String>,
    nats_source: Option<String>,
    nats_sha256: Option<String>,
    nats_binary: Option<String>,
    nats_config: Option<String>,
    machine_bootstrap_url: Option<String>,
    keeper_binary: Option<String>,
}

impl Default for ParsedInitArgs {
    fn default() -> Self {
        Self {
            keeper_install_mode: ParsedKeeperInstallMode::None,
            node_id: None,
            gateway: FirstNodeGateway::Skip,
            ployzd_version: None,
            ployzd_source: None,
            ployzd_sha256: None,
            ployzd_install_path: None,
            nats_version: None,
            nats_source: None,
            nats_sha256: None,
            nats_binary: None,
            nats_config: None,
            machine_bootstrap_url: None,
            keeper_binary: None,
        }
    }
}

impl ParsedInitArgs {
    fn set_keeper_install_mode(
        &mut self,
        mode: ParsedKeeperInstallMode,
    ) -> Result<(), PloyzctlCliError> {
        match mode {
            ParsedKeeperInstallMode::None => unreachable!("install mode is set from a flag"),
            ParsedKeeperInstallMode::Emit => match &self.keeper_install_mode {
                ParsedKeeperInstallMode::None => {
                    self.keeper_install_mode = ParsedKeeperInstallMode::Emit;
                    Ok(())
                }
                ParsedKeeperInstallMode::Emit => Err(PloyzctlCliError::DuplicateArgument {
                    flag: "--emit-keeper-install",
                }),
                ParsedKeeperInstallMode::Run => Err(PloyzctlCliError::ConflictingArguments {
                    first: "--run-keeper-install",
                    second: "--emit-keeper-install",
                }),
            },
            ParsedKeeperInstallMode::Run => match &self.keeper_install_mode {
                ParsedKeeperInstallMode::None => {
                    self.keeper_install_mode = ParsedKeeperInstallMode::Run;
                    Ok(())
                }
                ParsedKeeperInstallMode::Emit => Err(PloyzctlCliError::ConflictingArguments {
                    first: "--emit-keeper-install",
                    second: "--run-keeper-install",
                }),
                ParsedKeeperInstallMode::Run => Err(PloyzctlCliError::DuplicateArgument {
                    flag: "--run-keeper-install",
                }),
            },
        }
    }

    fn into_command(self) -> Result<FirstNodeInitCommand, PloyzctlCliError> {
        let Self {
            keeper_install_mode,
            node_id,
            gateway,
            ployzd_version,
            ployzd_source,
            ployzd_sha256,
            ployzd_install_path,
            nats_version,
            nats_source,
            nats_sha256,
            nats_binary,
            nats_config,
            machine_bootstrap_url,
            keeper_binary,
        } = self;
        let keeper_install = ParsedKeeperInstallArgs {
            ployzd_version,
            ployzd_source,
            ployzd_sha256,
            ployzd_install_path,
            nats_version,
            nats_source,
            nats_sha256,
            nats_binary,
            nats_config,
            machine_bootstrap_url,
        };
        let has_keeper_install_value = keeper_install.has_any_value();
        let node_id = NodeId::try_new(required(node_id, "--node")?)
            .map_err(|error| invalid_value("--node", error))?;

        if keeper_install_mode == ParsedKeeperInstallMode::None {
            if has_keeper_install_value || keeper_binary.is_some() {
                return Err(PloyzctlCliError::MissingRequiredArgument {
                    flag: "--emit-keeper-install or --run-keeper-install",
                });
            }
            return Ok(FirstNodeInitCommand::summary(node_id, gateway));
        }

        let keeper_install = keeper_install.into_keeper_install(node_id, gateway)?;
        if keeper_install_mode == ParsedKeeperInstallMode::Run {
            let keeper_binary = keeper_binary.unwrap_or_else(|| "ployz-keeper".to_owned());
            if keeper_binary.is_empty() {
                return Err(PloyzctlCliError::InvalidValue {
                    flag: "--keeper-binary",
                    message: "keeper binary is empty".to_owned(),
                });
            }
            return Ok(FirstNodeInitCommand::run_keeper_install(
                keeper_install,
                keeper_binary,
            ));
        }
        if keeper_binary.is_some() {
            return Err(PloyzctlCliError::MissingRequiredArgument {
                flag: "--run-keeper-install",
            });
        }

        Ok(FirstNodeInitCommand::keeper_install(keeper_install))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedKeeperInstallMode {
    None,
    Emit,
    Run,
}

struct ParsedKeeperInstallArgs {
    ployzd_version: Option<String>,
    ployzd_source: Option<String>,
    ployzd_sha256: Option<String>,
    ployzd_install_path: Option<String>,
    nats_version: Option<String>,
    nats_source: Option<String>,
    nats_sha256: Option<String>,
    nats_binary: Option<String>,
    nats_config: Option<String>,
    machine_bootstrap_url: Option<String>,
}

impl ParsedKeeperInstallArgs {
    fn into_keeper_install(
        self,
        node_id: NodeId,
        gateway: FirstNodeGateway,
    ) -> Result<KeeperFirstNodeInstall, PloyzctlCliError> {
        Ok(KeeperFirstNodeInstall {
            node_id,
            gateway,
            machine_bootstrap_url: self
                .machine_bootstrap_url
                .map(MachineBootstrapUrl::try_new)
                .transpose()
                .map_err(|error| invalid_value("--machine-bootstrap-url", error))?,
            ployzd_version: InstallArtifactVersion::try_new(required(
                self.ployzd_version,
                "--ployzd-version",
            )?)
            .map_err(|error| invalid_value("--ployzd-version", error))?,
            ployzd_source: InstallArtifactSource::try_new(required(
                self.ployzd_source,
                "--ployzd-source",
            )?)
            .map_err(|error| invalid_value("--ployzd-source", error))?,
            ployzd_sha256: InstallSha256Digest::try_new(required(
                self.ployzd_sha256,
                "--ployzd-sha256",
            )?)
            .map_err(|error| invalid_value("--ployzd-sha256", error))?,
            ployzd_install_path: AbsoluteInstallPath::try_new(required(
                self.ployzd_install_path,
                "--ployzd-install-path",
            )?)
            .map_err(|error| invalid_value("--ployzd-install-path", error))?,
            nats_version: InstallArtifactVersion::try_new(required(
                self.nats_version,
                "--nats-version",
            )?)
            .map_err(|error| invalid_value("--nats-version", error))?,
            nats_source: InstallArtifactSource::try_new(required(
                self.nats_source,
                "--nats-source",
            )?)
            .map_err(|error| invalid_value("--nats-source", error))?,
            nats_sha256: InstallSha256Digest::try_new(required(self.nats_sha256, "--nats-sha256")?)
                .map_err(|error| invalid_value("--nats-sha256", error))?,
            nats_binary: AbsoluteInstallPath::try_new(required(self.nats_binary, "--nats-binary")?)
                .map_err(|error| invalid_value("--nats-binary", error))?,
            nats_config: AbsoluteInstallPath::try_new(required(self.nats_config, "--nats-config")?)
                .map_err(|error| invalid_value("--nats-config", error))?,
        })
    }

    fn has_any_value(&self) -> bool {
        self.ployzd_version.is_some()
            || self.ployzd_source.is_some()
            || self.ployzd_sha256.is_some()
            || self.ployzd_install_path.is_some()
            || self.nats_version.is_some()
            || self.nats_source.is_some()
            || self.nats_sha256.is_some()
            || self.nats_binary.is_some()
            || self.nats_config.is_some()
            || self.machine_bootstrap_url.is_some()
    }
}
