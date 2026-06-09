//! Minimal keeper command-line contract.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use ployz_core::ids::SubjectTokenError;
use ployz_core::install::InstallContractError;

use crate::artifacts::ArtifactTargetError;
use crate::first_node_install_cli::parse_first_node_install_args;
use crate::join::{JoinTokenFileError, read_join_token_file};
use crate::steps::{FirstNodeInstallTarget, JoinToken};
use crate::systemd::SupervisorUnitFileError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperCommand {
    Start(KeeperStartup),
    FirstNodeInstall(Box<FirstNodeInstallTarget>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStartup {
    pub join: Option<StartupJoinToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupJoinToken {
    pub token: JoinToken,
    pub file: PathBuf,
}

pub fn load_command(
    args: impl IntoIterator<Item = OsString>,
) -> Result<KeeperCommand, KeeperCliError> {
    let parsed = parse_keeper_args(args)?;
    match parsed {
        ParsedKeeperCommand::Start { join_token_file } => {
            let join = match join_token_file {
                Some(path) => Some(StartupJoinToken {
                    token: read_join_token_file(&path)?,
                    file: path,
                }),
                None => None,
            };
            Ok(KeeperCommand::Start(KeeperStartup { join }))
        }
        ParsedKeeperCommand::FirstNodeInstall(target) => {
            Ok(KeeperCommand::FirstNodeInstall(target))
        }
    }
}

pub fn load_startup(
    args: impl IntoIterator<Item = OsString>,
) -> Result<KeeperStartup, KeeperCliError> {
    let join_token_file = parse_startup_args(args)?;
    let join = match join_token_file {
        Some(path) => Some(StartupJoinToken {
            token: read_join_token_file(&path)?,
            file: path,
        }),
        None => None,
    };

    Ok(KeeperStartup { join })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedKeeperCommand {
    Start { join_token_file: Option<PathBuf> },
    FirstNodeInstall(Box<FirstNodeInstallTarget>),
}

fn parse_keeper_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<ParsedKeeperCommand, KeeperCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(ParsedKeeperCommand::Start {
            join_token_file: None,
        }),
        [flag, ..] if flag == "--join-token-file" => parse_startup_args(args)
            .map(|join_token_file| ParsedKeeperCommand::Start { join_token_file }),
        [flag] if flag == "--help" || flag == "-h" => Err(KeeperCliError::HelpRequested),
        [command, rest @ ..] if command == "first-node-install" => {
            parse_first_node_install_args(rest)
                .map(Box::new)
                .map(ParsedKeeperCommand::FirstNodeInstall)
        }
        [unknown, ..] => Err(KeeperCliError::UnexpectedArgument {
            value: unknown.to_string_lossy().into_owned(),
        }),
    }
}

fn parse_startup_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<PathBuf>, KeeperCliError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(None),
        [flag, path] if flag == "--join-token-file" => Ok(Some(PathBuf::from(path))),
        [flag, _path, extra, ..] if flag == "--join-token-file" => {
            Err(KeeperCliError::UnexpectedArgument {
                value: extra.to_string_lossy().into_owned(),
            })
        }
        [flag] if flag == "--join-token-file" => Err(KeeperCliError::MissingJoinTokenFile),
        [flag] if flag == "--help" || flag == "-h" => Err(KeeperCliError::HelpRequested),
        [unknown, ..] => Err(KeeperCliError::UnexpectedArgument {
            value: unknown.to_string_lossy().into_owned(),
        }),
    }
}

#[derive(Debug)]
pub enum KeeperCliError {
    HelpRequested,
    MissingJoinTokenFile,
    MissingRequiredArgument { flag: &'static str },
    MissingValue { flag: String },
    DuplicateArgument { flag: &'static str },
    InvalidUtf8Argument { flag: &'static str },
    UnexpectedArgument { value: String },
    JoinTokenFile(JoinTokenFileError),
    NodeId(SubjectTokenError),
    InstallContract(InstallContractError),
    ArtifactTarget(ArtifactTargetError),
    SupervisorUnit(SupervisorUnitFileError),
}

impl From<JoinTokenFileError> for KeeperCliError {
    fn from(value: JoinTokenFileError) -> Self {
        Self::JoinTokenFile(value)
    }
}

impl From<SubjectTokenError> for KeeperCliError {
    fn from(value: SubjectTokenError) -> Self {
        Self::NodeId(value)
    }
}

impl From<InstallContractError> for KeeperCliError {
    fn from(value: InstallContractError) -> Self {
        Self::InstallContract(value)
    }
}

impl From<ArtifactTargetError> for KeeperCliError {
    fn from(value: ArtifactTargetError) -> Self {
        Self::ArtifactTarget(value)
    }
}

impl From<SupervisorUnitFileError> for KeeperCliError {
    fn from(value: SupervisorUnitFileError) -> Self {
        Self::SupervisorUnit(value)
    }
}

impl fmt::Display for KeeperCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HelpRequested => formatter.write_str(KEEPER_USAGE),
            Self::MissingJoinTokenFile => {
                formatter.write_str("--join-token-file requires a file path")
            }
            Self::MissingRequiredArgument { flag } => write!(formatter, "{flag} is required"),
            Self::MissingValue { flag } => write!(formatter, "{flag} requires a value"),
            Self::DuplicateArgument { flag } => write!(formatter, "{flag} was provided twice"),
            Self::InvalidUtf8Argument { flag } => {
                write!(formatter, "{flag} must be valid UTF-8")
            }
            Self::UnexpectedArgument { value } => {
                write!(formatter, "unexpected keeper argument: {value}")
            }
            Self::JoinTokenFile(error) => write!(formatter, "{error}"),
            Self::NodeId(error) => write!(formatter, "{error}"),
            Self::InstallContract(error) => write!(formatter, "{error}"),
            Self::ArtifactTarget(error) => write!(formatter, "{error}"),
            Self::SupervisorUnit(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for KeeperCliError {}

pub const KEEPER_USAGE: &str = "usage: ployz-keeper [--join-token-file <path>]\n       ployz-keeper first-node-install --node <id> --ployzd-version <version> --ployzd-source <path> --ployzd-sha256 <sha256> --ployzd-install-path <path> --nats-version <version> --nats-source <path> --nats-sha256 <sha256> --nats-binary <path> --nats-config <path> [--machine-bootstrap-url <url>] [--machine-join-template-file <path>] [--gateway]";

#[cfg(test)]
mod tests {
    use super::{
        KeeperCliError, ParsedKeeperCommand, load_command, load_startup, parse_keeper_args,
    };

    #[test]
    fn parser_accepts_no_args() {
        let command = parse_keeper_args([]).expect("no args are valid");

        assert_eq!(
            command,
            ParsedKeeperCommand::Start {
                join_token_file: None
            }
        );
    }

    #[test]
    fn parser_accepts_join_token_file() {
        let command = parse_keeper_args(["--join-token-file".into(), "/tmp/join".into()])
            .expect("join token file is valid");

        assert_eq!(
            command,
            ParsedKeeperCommand::Start {
                join_token_file: Some("/tmp/join".into())
            }
        );
    }

    #[test]
    fn parser_rejects_missing_join_token_file() {
        assert!(matches!(
            parse_keeper_args(["--join-token-file".into()]),
            Err(KeeperCliError::MissingJoinTokenFile)
        ));
    }

    #[test]
    fn parser_rejects_extra_args() {
        assert!(matches!(
            parse_keeper_args(["--join-token-file".into(), "/tmp/join".into(), "extra".into()]),
            Err(KeeperCliError::UnexpectedArgument { value }) if value == "extra"
        ));
    }

    #[test]
    fn parser_loads_first_node_install_command() {
        let command = load_command([
            "first-node-install".into(),
            "--node".into(),
            "node_1".into(),
            "--ployzd-version".into(),
            "0.1.0".into(),
            "--ployzd-source".into(),
            "/tmp/ployzd".into(),
            "--ployzd-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--ployzd-install-path".into(),
            "/usr/local/bin/ployzd".into(),
            "--ebpf-bytecode-version".into(),
            "0.1.0".into(),
            "--ebpf-bytecode-source".into(),
            "/tmp/ployz-ebpf-tc".into(),
            "--ebpf-bytecode-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--ebpf-bytecode-install-path".into(),
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".into(),
            "--ebpf-ctl-version".into(),
            "0.1.0".into(),
            "--ebpf-ctl-source".into(),
            "/tmp/ployz-ebpf-ctl".into(),
            "--ebpf-ctl-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--ebpf-ctl-install-path".into(),
            "/usr/local/bin/ployz-ebpf-ctl".into(),
            "--nats-version".into(),
            "2.12.0".into(),
            "--nats-source".into(),
            "/tmp/nats-server".into(),
            "--nats-sha256".into(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            "--nats-binary".into(),
            "/usr/local/bin/nats-server".into(),
            "--nats-config".into(),
            "/etc/nats/nats-server.conf".into(),
            "--gateway".into(),
        ])
        .expect("first-node install command loads");

        let super::KeeperCommand::FirstNodeInstall(target) = command else {
            panic!("expected first-node install command");
        };
        assert_eq!(target.node_id.as_str(), "node_1");
        assert_eq!(target.gateway, ployz_core::roles::FirstNodeGateway::Install);
    }

    #[test]
    fn parser_rejects_missing_first_node_install_required_flag() {
        assert!(matches!(
            load_command(["first-node-install".into()]),
            Err(KeeperCliError::MissingRequiredArgument { flag }) if flag == "--node"
        ));
    }

    #[test]
    fn startup_parser_rejects_first_node_install_without_subcommand_validation() {
        assert!(matches!(
            load_startup(["first-node-install".into()]),
            Err(KeeperCliError::UnexpectedArgument { value }) if value == "first-node-install"
        ));
    }

    #[test]
    fn parser_rejects_duplicate_first_node_install_flags() {
        assert!(matches!(
            load_command([
                "first-node-install".into(),
                "--node".into(),
                "node_1".into(),
                "--node".into(),
                "node_2".into(),
            ]),
            Err(KeeperCliError::DuplicateArgument { flag }) if flag == "--node"
        ));
    }
}
