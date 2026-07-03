//! Minimal keeper command-line contract.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use crate::artifacts::{
    ArtifactKind, ArtifactTargetError, DataplaneArtifactTargets, artifact_target,
};
use crate::join::{JoinTokenFileError, read_join_token_file};
use crate::nats_identity::{
    NatsIdentityError, ServerCertificateSans, generate_cluster_nats_identity,
};
use crate::release_manifest::{ExactPloyzVersion, ExactPloyzVersionError};
use crate::steps::{FirstMachineInstallTarget, JoinToken};
use crate::systemd::{NatsServerUnitTarget, SupervisorUnitFileError};
use clap::{Parser, Subcommand};
use ployz_core::ids::OperationId;
use ployz_core::install::{
    FirstMachineInstallArtifacts, FirstMachineInstallSpec, InstallArtifactSpec,
    NatsServerInstallSpec,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperCommand {
    Start(KeeperStartup),
    Bootstrap(KeeperBootstrap),
    FirstMachineInstall(Box<FirstMachineInstallTarget>),
    SubstrateUpdate(KeeperSubstrateUpdate),
}

pub const DEFAULT_CLOUD_HOST: &str = "https://ployz.dev";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperBootstrap {
    pub mode: KeeperBootstrapMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperBootstrapMode {
    Interactive { cloud_host: Option<CloudHost> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudHost(String);

impl CloudHost {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudHostError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CloudHostError::Empty);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(CloudHostError::Invalid { value });
        }
        if value.starts_with("http://") {
            return Err(CloudHostError::Insecure { value });
        }

        let normalized = if value.starts_with("https://") {
            value
        } else {
            format!("https://{value}")
        };
        let authority_and_path =
            normalized
                .strip_prefix("https://")
                .ok_or_else(|| CloudHostError::Invalid {
                    value: normalized.clone(),
                })?;
        let Some((authority, path)) = authority_and_path
            .split_once('/')
            .or(Some((authority_and_path, "")))
        else {
            return Err(CloudHostError::Invalid { value: normalized });
        };
        if authority.is_empty()
            || authority.contains('?')
            || authority.contains('#')
            || !path.is_empty()
        {
            return Err(CloudHostError::Invalid { value: normalized });
        }

        Ok(Self(format!("https://{authority}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for CloudHost {
    type Err = CloudHostError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl Default for CloudHost {
    fn default() -> Self {
        Self(DEFAULT_CLOUD_HOST.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CloudHostError {
    #[error("cloud host is empty")]
    Empty,
    #[error("cloud host must use https, got {value:?}")]
    Insecure { value: String },
    #[error("cloud host is invalid: {value:?}")]
    Invalid { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperStartup {
    pub join: Option<StartupJoinToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperSubstrateUpdate {
    pub operation_id: Option<OperationId>,
    pub version: ExactPloyzVersion,
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
        Some(KeeperSubcommand::Bootstrap { cloud_host }) => {
            Ok(KeeperCommand::Bootstrap(load_bootstrap(cloud_host)))
        }
        Some(KeeperSubcommand::FirstMachineInstall { spec }) => {
            let spec = read_first_machine_install_spec(spec)?;
            Ok(KeeperCommand::FirstMachineInstall(Box::new(
                first_machine_install_target_from_spec(spec)?,
            )))
        }
        Some(KeeperSubcommand::SubstrateUpdate {
            operation_id,
            version,
        }) => Ok(KeeperCommand::SubstrateUpdate(KeeperSubstrateUpdate {
            operation_id,
            version,
        })),
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
    Bootstrap {
        #[arg(long, value_name = "host-or-https-url")]
        cloud_host: Option<CloudHost>,
    },
    FirstMachineInstall {
        #[arg(long, value_name = "path|-")]
        spec: SpecSource,
    },
    SubstrateUpdate {
        #[arg(long, value_name = "operation-id", value_parser = parse_operation_id)]
        operation_id: Option<OperationId>,
        #[arg(long, value_name = "version")]
        version: ExactPloyzVersion,
    },
}

fn parse_operation_id(value: &str) -> Result<OperationId, String> {
    OperationId::try_new(value).map_err(|error| error.to_string())
}

fn load_bootstrap(cloud_host: Option<CloudHost>) -> KeeperBootstrap {
    let mode = KeeperBootstrapMode::Interactive { cloud_host };
    KeeperBootstrap { mode }
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

fn read_first_machine_install_spec(
    source: SpecSource,
) -> Result<FirstMachineInstallSpec, KeeperCliError> {
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

pub fn first_machine_install_target_from_spec(
    install: FirstMachineInstallSpec,
) -> Result<FirstMachineInstallTarget, KeeperCliError> {
    let roles = install.role_policy();
    let FirstMachineInstallSpec {
        machine_id,
        gateway: _,
        dns: _,
        machine_public_ip,
        machine_bootstrap_url,
        machine_join_template_file,
        artifacts:
            FirstMachineInstallArtifacts {
                ployzd,
                ebpf_bytecode,
                ebpf_ctl,
                nats_server,
            },
        machine_join_cluster_name,
        machine_join_runtime_nats_url,
    } = install;
    let ployzd_artifact = artifact_target(ArtifactKind::Ployzd, &ployzd)?;
    let ebpf_bytecode_artifact = artifact_target(ArtifactKind::EbpfBytecode, &ebpf_bytecode)?;
    let ebpf_ctl_artifact = artifact_target(ArtifactKind::EbpfCtl, &ebpf_ctl)?;
    let NatsServerInstallSpec {
        version: nats_version,
        source: nats_source,
        sha256: nats_sha256,
        binary: nats_binary,
        config: nats_config,
    } = nats_server;
    let nats_server_artifact = artifact_target(
        ArtifactKind::NatsServer,
        &InstallArtifactSpec {
            version: nats_version,
            source: nats_source,
            sha256: nats_sha256,
            install_path: nats_binary,
        },
    )?;
    let nats_server_unit = NatsServerUnitTarget::new(
        nats_server_artifact.install_path().to_path_buf(),
        PathBuf::from(nats_config.as_str()),
    )?;
    let certificate_sans = ServerCertificateSans::try_new(machine_public_ip, machine_hostname())?;
    let nats_identity = generate_cluster_nats_identity(&certificate_sans)?;

    let mut target = FirstMachineInstallTarget::new(
        machine_id,
        ployzd_artifact,
        DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
        nats_server_artifact,
        roles,
        nats_identity,
    )
    .with_nats_server_unit(nats_server_unit);
    if let Some(url) = machine_bootstrap_url {
        target = target.with_machine_bootstrap_url(url);
    }
    if let Some(path) = machine_join_template_file {
        target = target.with_machine_join_template_file(path);
    }
    target = target
        .with_machine_join_cluster_name(machine_join_cluster_name)
        .with_machine_join_runtime_nats_url(machine_join_runtime_nats_url);
    if let Some(public_ip) = machine_public_ip {
        target = target.with_machine_public_ip(public_ip);
    }
    Ok(target)
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
    CloudHost(CloudHostError),
    ExactPloyzVersion(ExactPloyzVersionError),
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

impl From<ExactPloyzVersionError> for KeeperCliError {
    fn from(value: ExactPloyzVersionError) -> Self {
        Self::ExactPloyzVersion(value)
    }
}

impl fmt::Display for KeeperCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clap(error) => write!(formatter, "{error}"),
            Self::ReadSpec { source, error } => {
                write!(
                    formatter,
                    "failed to read first-machine install spec from {source}: {error}"
                )
            }
            Self::ParseSpec { source, error } => {
                write!(
                    formatter,
                    "failed to parse first-machine install spec from {source}: {error}"
                )
            }
            Self::JoinTokenFile(error) => write!(formatter, "{error}"),
            Self::CloudHost(error) => write!(formatter, "{error}"),
            Self::ExactPloyzVersion(error) => write!(formatter, "{error}"),
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
    use ployz_core::ids::OperationId;

    use super::{
        CloudHost, KeeperBootstrap, KeeperBootstrapMode, KeeperCliError, KeeperCommand,
        KeeperStartup, KeeperSubstrateUpdate, SpecSource, load_command,
    };

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
    fn parser_accepts_interactive_bootstrap() {
        let command = load_command(["bootstrap".into()]).expect("bootstrap command is valid");

        assert_eq!(
            command,
            KeeperCommand::Bootstrap(KeeperBootstrap {
                mode: KeeperBootstrapMode::Interactive { cloud_host: None },
            })
        );
    }

    #[test]
    fn parser_accepts_interactive_bootstrap_with_custom_cloud_host() {
        let command = load_command([
            "bootstrap".into(),
            "--cloud-host".into(),
            "cloud.example.com".into(),
        ])
        .expect("bootstrap command is valid");

        assert_eq!(
            command,
            KeeperCommand::Bootstrap(KeeperBootstrap {
                mode: KeeperBootstrapMode::Interactive {
                    cloud_host: Some(CloudHost::try_new("https://cloud.example.com").unwrap()),
                },
            })
        );
    }

    #[test]
    fn parser_rejects_insecure_cloud_host() {
        assert!(matches!(
            load_command([
                "bootstrap".into(),
                "--cloud-host".into(),
                "http://cloud.example.com".into(),
            ]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::ValueValidation
        ));
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
    fn parser_loads_first_machine_install_spec_command() {
        let path = write_temp_spec();
        let command = load_command(["first-machine-install".into(), "--spec".into(), path.into()])
            .expect("first-machine install command loads");

        let KeeperCommand::FirstMachineInstall(target) = command else {
            panic!("expected first-machine install command");
        };
        assert_eq!(target.machine_id.as_str(), "machine_1");
        // The spec carries no `dns` field: DNS defaults to install.
        assert_eq!(
            target.roles,
            ployz_core::roles::InstallRolePolicy::install_all()
        );
    }

    #[test]
    fn parser_honors_explicit_dns_opt_out_in_spec() {
        let path = write_temp_spec_with(FIRST_MACHINE_INSTALL_SPEC_NO_DNS, "no-dns");
        let command = load_command(["first-machine-install".into(), "--spec".into(), path.into()])
            .expect("first-machine install command loads");

        let KeeperCommand::FirstMachineInstall(target) = command else {
            panic!("expected first-machine install command");
        };
        assert_eq!(
            target.roles,
            ployz_core::roles::InstallRolePolicy::install_all().without_dns()
        );
    }

    #[test]
    fn parser_rejects_missing_first_machine_install_spec() {
        assert!(matches!(
            load_command(["first-machine-install".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::MissingRequiredArgument
        ));
    }

    #[test]
    fn parser_loads_substrate_update_exact_version() {
        let command = load_command([
            "substrate-update".into(),
            "--operation-id".into(),
            "op_update_1".into(),
            "--version".into(),
            "v0.0.2-alpha.16".into(),
        ])
        .expect("substrate update command loads");

        assert_eq!(
            command,
            KeeperCommand::SubstrateUpdate(KeeperSubstrateUpdate {
                operation_id: Some(
                    OperationId::try_new("op_update_1").expect("valid operation id")
                ),
                version: "v0.0.2-alpha.16".parse().expect("exact version parses"),
            })
        );
    }

    #[test]
    fn parser_rejects_substrate_update_channel() {
        assert!(matches!(
            load_command([
                "substrate-update".into(),
                "--operation-id".into(),
                "op_update_1".into(),
                "--version".into(),
                "alpha".into(),
            ]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::ValueValidation
        ));
    }

    #[test]
    fn startup_parser_rejects_first_machine_install_without_subcommand_validation() {
        assert!(matches!(
            load_startup(["first-machine-install".into()]),
            Err(KeeperCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::UnknownArgument
        ));
    }

    #[test]
    fn spec_source_accepts_stdin_marker() {
        assert_eq!("-".parse::<SpecSource>(), Ok(SpecSource::Stdin));
    }

    fn write_temp_spec() -> PathBuf {
        write_temp_spec_with(FIRST_MACHINE_INSTALL_SPEC, "default")
    }

    fn write_temp_spec_with(spec: &str, label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ployz-first-machine-install-{label}-{}.json",
            std::process::id()
        ));
        fs::write(&path, spec).expect("write spec");
        path
    }

    const FIRST_MACHINE_INSTALL_SPEC_NO_DNS: &str = r#"{
        "machine_id": "machine_1",
        "gateway": "install",
        "dns": "skip",
        "machine_public_ip": null,
        "machine_bootstrap_url": null,
        "machine_join_template_file": null,
        "machine_join_cluster_name": "ployz",
        "machine_join_runtime_nats_url": "tls://203.0.113.10:4222",
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

    const FIRST_MACHINE_INSTALL_SPEC: &str = r#"{
        "machine_id": "machine_1",
        "gateway": "install",
        "dns": "install",
        "machine_public_ip": null,
        "machine_bootstrap_url": null,
        "machine_join_template_file": null,
        "machine_join_cluster_name": "ployz",
        "machine_join_runtime_nats_url": "tls://203.0.113.10:4222",
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
