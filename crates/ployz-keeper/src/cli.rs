//! Minimal keeper command-line contract.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ployz_core::install::{
    AbsoluteInstallPath, FirstNodeInstallArtifacts, FirstNodeInstallSpec, InstallArtifactSource,
    InstallArtifactSpec, InstallArtifactVersion, InstallSha256Digest, NatsServerInstallSpec,
};

use crate::artifacts::{
    ArtifactSource, ArtifactTargetError, ArtifactVersion, DataplaneArtifactTargets,
    EbpfBytecodeArtifactTarget, EbpfCtlArtifactTarget, NatsServerArtifactTarget,
    PloyzdArtifactTarget, Sha256Digest,
};
use crate::join::{JoinTokenFileError, read_join_token_file};
use crate::nats_identity::{
    NatsIdentityError, ServerCertificateSans, generate_cluster_nats_identity,
};
use crate::steps::{FirstNodeInstallTarget, JoinToken};
use crate::systemd::{NatsServerUnitTarget, SupervisorUnitFileError};

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
    let parsed =
        KeeperCli::try_parse_from(std::iter::once(OsString::from("ployz-keeper")).chain(args))
            .map_err(KeeperCliError::Clap)?;
    match parsed.command {
        None => Ok(KeeperCommand::Start(load_startup_from_path(
            parsed.join_token_file,
        )?)),
        Some(KeeperSubcommand::FirstNodeInstall { spec }) => {
            let spec = read_first_node_install_spec(spec)?;
            Ok(KeeperCommand::FirstNodeInstall(Box::new(
                first_node_install_target(spec)?,
            )))
        }
    }
}

fn load_startup_from_path(
    join_token_file: Option<PathBuf>,
) -> Result<KeeperStartup, KeeperCliError> {
    let join = match join_token_file {
        Some(path) => Some(StartupJoinToken {
            token: read_join_token_file(&path)?,
            file: path,
        }),
        None => None,
    };

    Ok(KeeperStartup { join })
}

#[derive(Debug, Parser)]
#[command(
    name = "ployz-keeper",
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true
)]
struct KeeperCli {
    #[arg(long)]
    join_token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<KeeperSubcommand>,
}

#[derive(Debug, Subcommand)]
enum KeeperSubcommand {
    FirstNodeInstall {
        #[arg(long, value_name = "path|-")]
        spec: SpecSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecSource {
    Path(PathBuf),
    Stdin,
}

impl std::str::FromStr for SpecSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "-" {
            return Ok(Self::Stdin);
        }
        if value.is_empty() {
            return Err("spec path is empty".to_owned());
        }
        Ok(Self::Path(PathBuf::from(value)))
    }
}

fn read_first_node_install_spec(
    source: SpecSource,
) -> Result<FirstNodeInstallSpec, KeeperCliError> {
    let mut bytes = String::new();
    match &source {
        SpecSource::Path(path) => {
            bytes = std::fs::read_to_string(path).map_err(|error| KeeperCliError::ReadSpec {
                source: source.clone(),
                error,
            })?;
        }
        SpecSource::Stdin => {
            std::io::stdin()
                .read_to_string(&mut bytes)
                .map_err(|error| KeeperCliError::ReadSpec {
                    source: source.clone(),
                    error,
                })?;
        }
    }
    serde_json::from_str(&bytes).map_err(|error| KeeperCliError::ParseSpec { source, error })
}

/// The machine hostname covered by the server certificate SANs. A host
/// without a UTF-8 hostname simply gets no hostname SAN.
fn machine_hostname() -> Option<String> {
    gethostname::gethostname().into_string().ok()
}

fn first_node_install_target(
    install: FirstNodeInstallSpec,
) -> Result<FirstNodeInstallTarget, KeeperCliError> {
    let FirstNodeInstallSpec {
        node_id,
        gateway,
        node_public_ip,
        machine_bootstrap_url,
        machine_join_template_file,
        artifacts:
            FirstNodeInstallArtifacts {
                ployzd,
                ebpf_bytecode,
                ebpf_ctl,
                nats_server,
            },
    } = install;
    let InstallArtifactSpec {
        version: ployzd_version,
        source: ployzd_source,
        sha256: ployzd_sha256,
        install_path: ployzd_install_path,
    } = ployzd;
    let InstallArtifactSpec {
        version: ebpf_bytecode_version,
        source: ebpf_bytecode_source,
        sha256: ebpf_bytecode_sha256,
        install_path: ebpf_bytecode_install_path,
    } = ebpf_bytecode;
    let InstallArtifactSpec {
        version: ebpf_ctl_version,
        source: ebpf_ctl_source,
        sha256: ebpf_ctl_sha256,
        install_path: ebpf_ctl_install_path,
    } = ebpf_ctl;
    let NatsServerInstallSpec {
        version: nats_version,
        source: nats_source,
        sha256: nats_sha256,
        binary: nats_binary,
        config: nats_config,
    } = nats_server;
    let ployzd_artifact = PloyzdArtifactTarget::new(
        artifact_version(&ployzd_version)?,
        artifact_source(&ployzd_source)?,
        sha256_digest(&ployzd_sha256)?,
        install_path(&ployzd_install_path),
    )?;
    let ebpf_bytecode_artifact = EbpfBytecodeArtifactTarget::new(
        artifact_version(&ebpf_bytecode_version)?,
        artifact_source(&ebpf_bytecode_source)?,
        sha256_digest(&ebpf_bytecode_sha256)?,
        install_path(&ebpf_bytecode_install_path),
    )?;
    let ebpf_ctl_artifact = EbpfCtlArtifactTarget::new(
        artifact_version(&ebpf_ctl_version)?,
        artifact_source(&ebpf_ctl_source)?,
        sha256_digest(&ebpf_ctl_sha256)?,
        install_path(&ebpf_ctl_install_path),
    )?;
    let nats_server_artifact = NatsServerArtifactTarget::new(
        artifact_version(&nats_version)?,
        artifact_source(&nats_source)?,
        sha256_digest(&nats_sha256)?,
        install_path(&nats_binary),
    )?;
    let nats_server_unit = NatsServerUnitTarget::new(
        nats_server_artifact.install_path().to_path_buf(),
        install_path(&nats_config),
    )?;
    let certificate_sans = ServerCertificateSans::try_new(node_public_ip, machine_hostname())?;
    let nats_identity = generate_cluster_nats_identity(&certificate_sans)?;

    let mut target = FirstNodeInstallTarget::new(
        node_id,
        ployzd_artifact,
        DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
        nats_server_artifact,
        gateway,
        nats_identity,
    )
    .with_nats_server_unit(nats_server_unit);
    if let Some(url) = machine_bootstrap_url {
        target = target.with_machine_bootstrap_url(url);
    }
    if let Some(path) = machine_join_template_file {
        target = target.with_machine_join_template_file(path);
    }
    if let Some(public_ip) = node_public_ip {
        target = target.with_node_public_ip(public_ip);
    }
    Ok(target)
}

fn artifact_version(
    value: &InstallArtifactVersion,
) -> Result<ArtifactVersion, ArtifactTargetError> {
    ArtifactVersion::try_new(value.as_str().to_owned())
}

fn artifact_source(value: &InstallArtifactSource) -> Result<ArtifactSource, ArtifactTargetError> {
    ArtifactSource::try_new(value.as_str().to_owned())
}

fn sha256_digest(value: &InstallSha256Digest) -> Result<Sha256Digest, ArtifactTargetError> {
    Sha256Digest::try_new(value.as_str().to_owned())
}

fn install_path(value: &AbsoluteInstallPath) -> PathBuf {
    PathBuf::from(value.as_str())
}

#[derive(Debug)]
pub enum KeeperCliError {
    Clap(clap::Error),
    ReadSpec {
        source: SpecSource,
        error: std::io::Error,
    },
    ParseSpec {
        source: SpecSource,
        error: serde_json::Error,
    },
    JoinTokenFile(JoinTokenFileError),
    ArtifactTarget(ArtifactTargetError),
    SupervisorUnit(SupervisorUnitFileError),
    NatsIdentity(NatsIdentityError),
}

impl KeeperCliError {
    #[must_use]
    pub fn is_help_requested(&self) -> bool {
        matches!(
            self,
            Self::Clap(error) if error.kind() == clap::error::ErrorKind::DisplayHelp
        )
    }
}

impl From<JoinTokenFileError> for KeeperCliError {
    fn from(value: JoinTokenFileError) -> Self {
        Self::JoinTokenFile(value)
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

impl From<NatsIdentityError> for KeeperCliError {
    fn from(value: NatsIdentityError) -> Self {
        Self::NatsIdentity(value)
    }
}

impl fmt::Display for KeeperCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clap(error) => write!(formatter, "{error}"),
            Self::ReadSpec { source, error } => {
                write!(
                    formatter,
                    "failed to read first-node install spec from {source}: {error}"
                )
            }
            Self::ParseSpec { source, error } => {
                write!(
                    formatter,
                    "failed to parse first-node install spec from {source}: {error}"
                )
            }
            Self::JoinTokenFile(error) => write!(formatter, "{error}"),
            Self::ArtifactTarget(error) => write!(formatter, "{error}"),
            Self::SupervisorUnit(error) => write!(formatter, "{error}"),
            Self::NatsIdentity(error) => write!(formatter, "{error}"),
        }
    }
}

impl fmt::Display for SpecSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(formatter, "{}", path.display()),
            Self::Stdin => formatter.write_str("stdin"),
        }
    }
}

impl std::error::Error for KeeperCliError {}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use clap::Parser;

    use super::{KeeperCliError, KeeperCommand, KeeperStartup, SpecSource, load_command};

    fn load_startup(
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<KeeperStartup, KeeperCliError> {
        let parsed = KeeperStartupCli::try_parse_from(
            std::iter::once(OsString::from("ployz-keeper")).chain(args),
        )
        .map_err(KeeperCliError::Clap)?;
        super::load_startup_from_path(parsed.join_token_file)
    }

    #[derive(Debug, Parser)]
    #[command(name = "ployz-keeper", disable_help_subcommand = true)]
    struct KeeperStartupCli {
        #[arg(long)]
        join_token_file: Option<PathBuf>,
    }

    #[test]
    fn parser_accepts_no_args() {
        let command = load_command([]).expect("no args are valid");

        assert_eq!(
            command,
            KeeperCommand::Start(super::KeeperStartup { join: None })
        );
    }

    #[test]
    fn parser_rejects_missing_join_token_file() {
        assert!(matches!(
            load_startup(["--join-token-file".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::InvalidValue
                    || error.kind() == clap::error::ErrorKind::MissingRequiredArgument
        ));
    }

    #[test]
    fn parser_rejects_extra_args() {
        assert!(matches!(
            load_startup(["--join-token-file".into(), "/tmp/join".into(), "extra".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::UnknownArgument
        ));
    }

    #[test]
    fn parser_loads_first_node_install_spec_command() {
        let path = write_temp_spec();
        let command = load_command(["first-node-install".into(), "--spec".into(), path.into()])
            .expect("first-node install command loads");

        let KeeperCommand::FirstNodeInstall(target) = command else {
            panic!("expected first-node install command");
        };
        assert_eq!(target.node_id.as_str(), "node_1");
        assert_eq!(target.gateway, ployz_core::roles::FirstNodeGateway::Install);
    }

    #[test]
    fn parser_rejects_missing_first_node_install_spec() {
        assert!(matches!(
            load_command(["first-node-install".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::MissingRequiredArgument
        ));
    }

    #[test]
    fn startup_parser_rejects_first_node_install_without_subcommand_validation() {
        assert!(matches!(
            load_startup(["first-node-install".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::UnknownArgument
        ));
    }

    #[test]
    fn spec_source_accepts_stdin_marker() {
        assert_eq!("-".parse::<SpecSource>(), Ok(SpecSource::Stdin));
    }

    fn write_temp_spec() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ployz-first-node-install-{}.json",
            std::process::id()
        ));
        fs::write(&path, FIRST_NODE_INSTALL_SPEC).expect("write spec");
        path
    }

    const FIRST_NODE_INSTALL_SPEC: &str = r#"{
        "node_id": "node_1",
        "gateway": "install",
        "node_public_ip": null,
        "machine_bootstrap_url": null,
        "machine_join_template_file": null,
        "artifacts": {
            "ployzd": {
                "version": "0.1.0",
                "source": "/tmp/ployzd",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "install_path": "/usr/local/bin/ployzd"
            },
            "ebpf_bytecode": {
                "version": "0.1.0",
                "source": "/tmp/ployz-ebpf-tc",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "install_path": "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"
            },
            "ebpf_ctl": {
                "version": "0.1.0",
                "source": "/tmp/ployz-ebpf-ctl",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "install_path": "/usr/local/bin/ployz-ebpf-ctl"
            },
            "nats_server": {
                "version": "2.12.0",
                "source": "/tmp/nats-server",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "binary": "/usr/local/bin/nats-server",
                "config": "/etc/nats/nats-server.conf"
            }
        }
    }"#;
}
