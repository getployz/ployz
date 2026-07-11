//! Minimal Host Runner command-line contract.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use crate::artifacts::{
    ArtifactKind, ArtifactTargetError, DataplaneArtifactTargets, artifact_target,
};
use crate::join::{JoinTokenFileError, read_join_token_file};
use crate::nats_identity::{
    CoreSeeds, NatsIdentityError, ServerCertificateSans, generate_cluster_nats_identity,
    wrap_core_seeds,
};
use crate::recovery_secret::{self, RecoverySecretError};
use crate::release_manifest::{ExactPloyzVersion, ExactPloyzVersionError};
use crate::steps::{FirstMachineInstallTarget, JoinMaterialError, JoinToken};
use crate::systemd::{NatsServerUnitTarget, SupervisorUnitFileError};
use clap::{Parser, Subcommand};
use ployz_core::ids::OperationId;
use ployz_core::install::{
    FirstMachineInstallArtifacts, FirstMachineInstallSpec, InstallArtifactSpec,
    NatsServerInstallSpec, WrappedCaKey,
};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError};
use ployz_sdk_types::CloudBootstrapToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerCommand {
    Start(HostRunnerStartup),
    Bootstrap(HostRunnerBootstrap),
    FirstMachineInstall(Box<FirstMachineInstallTarget>),
    CorePromote(HostRunnerCorePromote),
    CoreDemote(HostRunnerCoreDemote),
    SubstrateUpdate(HostRunnerSubstrateUpdate),
}

pub const DEFAULT_CLOUD_HOST: &str = "https://ployz.dev";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerBootstrap {
    pub mode: HostRunnerBootstrapMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerBootstrapMode {
    LocalGuidance,
    CloudInteractive {
        cloud_host: Option<CloudHost>,
    },
    CloudToken {
        cloud_host: Option<CloudHost>,
        cloud_token: CloudBootstrapToken,
    },
    Core,
    Join {
        join_token: JoinToken,
    },
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
pub struct HostRunnerStartup {
    pub join: Option<StartupJoinToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerSubstrateUpdate {
    pub operation_id: Option<OperationId>,
    pub source: HostRunnerSubstrateUpdateSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRunnerSubstrateUpdateSource {
    Version(ExactPloyzVersion),
    ManifestFile(PathBuf),
}

impl HostRunnerSubstrateUpdateSource {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Version(version) => version.as_str().to_owned(),
            Self::ManifestFile(path) => path.display().to_string(),
        }
    }
}

/// `core-promote` reads everything from the machine's own state (its id + public
/// IP, the CA + wrapped recovery key, the fleet's machine publics). The only
/// external input is which release to pull the core `nats-server` from, and that
/// defaults to this Host Runner binary's own version — which, on a joined machine, is
/// the cluster's release. `version` overrides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerCorePromote {
    pub version: Option<ExactPloyzVersion>,
    pub check: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerCoreDemote {
    pub successor_nats_url: NatsClientUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupJoinToken {
    pub token: JoinToken,
    pub file: PathBuf,
}

pub fn load_command(
    args: impl IntoIterator<Item = OsString>,
) -> Result<HostRunnerCommand, HostRunnerCliError> {
    let parsed =
        HostRunnerCli::try_parse_from(std::iter::once(OsString::from("ployz host")).chain(args))
            .map_err(HostRunnerCliError::Clap)?;
    match parsed.command {
        None => Ok(HostRunnerCommand::Start(load_startup_from_path(
            parsed.join_token_file,
        )?)),
        Some(HostRunnerSubcommand::Bootstrap { command }) => {
            Ok(HostRunnerCommand::Bootstrap(load_bootstrap(command)))
        }
        Some(HostRunnerSubcommand::FirstMachineInstall { spec }) => {
            let spec = read_first_machine_install_spec(spec)?;
            Ok(HostRunnerCommand::FirstMachineInstall(Box::new(
                first_machine_install_target_from_spec(spec)?,
            )))
        }
        Some(HostRunnerSubcommand::CorePromote { version, check }) => {
            Ok(HostRunnerCommand::CorePromote(HostRunnerCorePromote {
                version,
                check,
            }))
        }
        Some(HostRunnerSubcommand::CoreDemote { successor_nats_url }) => {
            Ok(HostRunnerCommand::CoreDemote(HostRunnerCoreDemote {
                successor_nats_url,
            }))
        }
        Some(HostRunnerSubcommand::SubstrateUpdate {
            operation_id,
            version,
            manifest_file,
        }) => Ok(HostRunnerCommand::SubstrateUpdate(
            HostRunnerSubstrateUpdate {
                operation_id,
                source: match (version, manifest_file) {
                    (Some(version), None) => HostRunnerSubstrateUpdateSource::Version(version),
                    (None, Some(path)) => HostRunnerSubstrateUpdateSource::ManifestFile(path),
                    _ => unreachable!("clap requires exactly one substrate update source"),
                },
            },
        )),
    }
}

fn load_startup_from_path(
    join_token_file: Option<PathBuf>,
) -> Result<HostRunnerStartup, HostRunnerCliError> {
    let join = match join_token_file {
        Some(path) => Some(StartupJoinToken {
            token: read_join_token_file(&path)?,
            file: path,
        }),
        None => None,
    };

    Ok(HostRunnerStartup { join })
}

#[derive(Debug, Parser)]
#[command(
    name = "ployz host",
    disable_help_subcommand = true,
    args_conflicts_with_subcommands = true
)]
struct HostRunnerCli {
    #[arg(long)]
    join_token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<HostRunnerSubcommand>,
}

#[derive(Debug, Subcommand)]
enum HostRunnerSubcommand {
    Bootstrap {
        #[command(subcommand)]
        command: Option<HostRunnerBootstrapSubcommand>,
    },
    #[command(name = "install")]
    FirstMachineInstall {
        #[arg(long, value_name = "path|-")]
        spec: SpecSource,
    },
    CorePromote {
        #[arg(long, value_name = "version")]
        version: Option<ExactPloyzVersion>,
        #[arg(long)]
        check: bool,
    },
    #[command(name = "internal-core-demote", hide = true)]
    CoreDemote {
        #[arg(long = "successor-nats-url", value_name = "url", value_parser = parse_nats_client_url)]
        successor_nats_url: NatsClientUrl,
    },
    SubstrateUpdate {
        #[arg(long, value_name = "operation-id", value_parser = parse_operation_id)]
        operation_id: Option<OperationId>,
        #[arg(
            long,
            value_name = "version",
            required_unless_present = "manifest_file"
        )]
        version: Option<ExactPloyzVersion>,
        #[arg(long, value_name = "path", conflicts_with = "version")]
        manifest_file: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum HostRunnerBootstrapSubcommand {
    Core,
    Join {
        #[arg(value_name = "token", required_unless_present = "join_token")]
        token: Option<CliJoinToken>,
        #[arg(long, value_name = "token", hide = true)]
        join_token: Option<CliJoinToken>,
    },
    Cloud {
        #[arg(long, value_name = "host-or-https-url")]
        cloud_host: Option<CloudHost>,
        #[arg(long, value_name = "secret", value_parser = parse_cloud_bootstrap_token)]
        cloud_token: Option<CloudBootstrapToken>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliJoinToken(JoinToken);

impl std::str::FromStr for CliJoinToken {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        JoinToken::try_new(value)
            .map(Self)
            .map_err(|error| match error {
                JoinMaterialError::EmptyJoinToken => "join token is empty".to_owned(),
                JoinMaterialError::EmptyClusterName
                | JoinMaterialError::EmptyJoinMaterialValue { .. }
                | JoinMaterialError::InvalidJoinMaterialValue { .. } => {
                    "join token is invalid".to_owned()
                }
            })
    }
}

fn parse_operation_id(value: &str) -> Result<OperationId, String> {
    OperationId::try_new(value).map_err(|error| error.to_string())
}

fn parse_cloud_bootstrap_token(value: &str) -> Result<CloudBootstrapToken, String> {
    CloudBootstrapToken::try_new(value).map_err(|_| "cloud bootstrap token is invalid".to_owned())
}

fn parse_nats_client_url(value: &str) -> Result<NatsClientUrl, NatsClientUrlError> {
    NatsClientUrl::try_new(value)
}

fn load_bootstrap(command: Option<HostRunnerBootstrapSubcommand>) -> HostRunnerBootstrap {
    let mode = match command {
        None => HostRunnerBootstrapMode::LocalGuidance,
        Some(HostRunnerBootstrapSubcommand::Core) => HostRunnerBootstrapMode::Core,
        Some(HostRunnerBootstrapSubcommand::Join { token, join_token }) => {
            let join_token = token
                .or(join_token)
                .expect("clap requires positional token or hidden --join-token");
            HostRunnerBootstrapMode::Join {
                join_token: join_token.0,
            }
        }
        Some(HostRunnerBootstrapSubcommand::Cloud {
            cloud_host,
            cloud_token,
        }) => match cloud_token {
            Some(cloud_token) => HostRunnerBootstrapMode::CloudToken {
                cloud_host,
                cloud_token,
            },
            None => HostRunnerBootstrapMode::CloudInteractive { cloud_host },
        },
    };
    HostRunnerBootstrap { mode }
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
) -> Result<FirstMachineInstallSpec, HostRunnerCliError> {
    let mut bytes = String::new();
    match &source {
        SpecSource::Path(path) => {
            bytes =
                std::fs::read_to_string(path).map_err(|error| HostRunnerCliError::ReadSpec {
                    source: source.clone(),
                    error,
                })?;
        }
        SpecSource::Stdin => {
            std::io::stdin()
                .read_to_string(&mut bytes)
                .map_err(|error| HostRunnerCliError::ReadSpec {
                    source: source.clone(),
                    error,
                })?;
        }
    }
    serde_json::from_str(&bytes).map_err(|error| HostRunnerCliError::ParseSpec { source, error })
}

/// The machine hostname covered by the server certificate SANs. A host
/// without a UTF-8 hostname simply gets no hostname SAN.
fn machine_hostname() -> Option<String> {
    gethostname::gethostname().into_string().ok()
}

pub fn first_machine_install_target_from_spec(
    install: FirstMachineInstallSpec,
) -> Result<FirstMachineInstallTarget, HostRunnerCliError> {
    let roles = install.role_policy();
    let FirstMachineInstallSpec {
        machine_id,
        dataplane_endpoint_supernet,
        gateway: _,
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
    // Founding a first machine always installs the core nats-server; a
    // manifest without one (a dev substrate push) cannot found a cluster.
    let Some(nats_server) = nats_server else {
        return Err(HostRunnerCliError::MissingNatsServerArtifact);
    };
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
    // Precedence for the first-machine public address: an explicit spec value, then a
    // `--public-ip` override the founder command passed via env, then magic
    // self-discovery. A first machine must be publicly reachable to serve as a core;
    // binding the NATS listener external and the cert SANs both follow from this.
    let machine_public_ips = match machine_public_ip.or_else(public_ip_env_override) {
        Some(public_ip) => vec![public_ip],
        None => discover_machine_public_ips(),
    };
    let machine_public_ip = machine_public_ips.first().copied();
    let hostname = machine_hostname();
    let certificate_sans =
        ServerCertificateSans::try_new_many(machine_public_ips, hostname.clone())?;
    let nats_identity = generate_cluster_nats_identity(&certificate_sans)?;
    let recovery_secret = resolve_recovery_secret();
    let recovery_key_wrapped = WrappedCaKey::new(
        recovery_secret::wrap(&recovery_secret, nats_identity.ca_key.secret().as_bytes())
            .map_err(HostRunnerCliError::RecoverySecret)?,
    );
    let core_seeds_wrapped =
        wrap_core_seeds(&recovery_secret, &CoreSeeds::from_identity(&nats_identity))
            .map_err(HostRunnerCliError::RecoverySecret)?;

    let mut target = FirstMachineInstallTarget::new(
        machine_id,
        ployzd_artifact,
        DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
        nats_server_artifact,
        roles,
        nats_identity,
        recovery_key_wrapped,
        core_seeds_wrapped,
    )
    .with_nats_server_unit(nats_server_unit)
    .with_dataplane_endpoint_supernet(dataplane_endpoint_supernet);
    if let Some(hostname) = hostname {
        target = target.with_founder_hostname(hostname);
    }
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

/// The machine's public interface addresses (a syscall, no network). Used for
/// first-machine TLS SANs and the first address becomes the listener address.
/// If a public address is not on the NIC (e.g. a 1:1 NAT), discovery finds
/// nothing and the operator supplies `--public-ip`.
fn discover_machine_public_ips() -> Vec<std::net::IpAddr> {
    local_ip_address::list_afinet_netifas()
        .ok()
        .unwrap_or_default()
        .into_iter()
        .map(|(_, ip)| ip)
        .filter(|ip| ployz_core::reachability::is_public(*ip))
        .collect()
}

/// The operator's `--public-ip` override, delivered by the founder command as the
/// `PLOYZ_MACHINE_PUBLIC_IP` environment variable (inherited by this process).
fn public_ip_env_override() -> Option<std::net::IpAddr> {
    std::env::var("PLOYZ_MACHINE_PUBLIC_IP")
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The operator's cluster recovery secret: `PLOYZ_RECOVERY_SECRET` if set, else
/// generated and shown once (ADR 0031). It wraps the CA key and is required later
/// to `core-promote`; it is never itself persisted.
fn resolve_recovery_secret() -> String {
    match std::env::var("PLOYZ_RECOVERY_SECRET") {
        Ok(secret) if !secret.is_empty() => secret,
        _ => {
            let generated = recovery_secret::generate_recovery_secret();
            eprintln!(
                "cluster recovery secret (save this — required to promote a new core):\n    {generated}"
            );
            generated
        }
    }
}

#[derive(Debug)]
pub enum HostRunnerCliError {
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
    MissingNatsServerArtifact,
    ArtifactTarget(ArtifactTargetError),
    SupervisorUnit(SupervisorUnitFileError),
    NatsIdentity(NatsIdentityError),
    RecoverySecret(RecoverySecretError),
}

impl HostRunnerCliError {
    #[must_use]
    pub fn is_help_requested(&self) -> bool {
        matches!(
            self,
            Self::Clap(error) if error.kind() == clap::error::ErrorKind::DisplayHelp
        )
    }
}

impl From<JoinTokenFileError> for HostRunnerCliError {
    fn from(value: JoinTokenFileError) -> Self {
        Self::JoinTokenFile(value)
    }
}

impl From<ArtifactTargetError> for HostRunnerCliError {
    fn from(value: ArtifactTargetError) -> Self {
        Self::ArtifactTarget(value)
    }
}

impl From<SupervisorUnitFileError> for HostRunnerCliError {
    fn from(value: SupervisorUnitFileError) -> Self {
        Self::SupervisorUnit(value)
    }
}

impl From<NatsIdentityError> for HostRunnerCliError {
    fn from(value: NatsIdentityError) -> Self {
        Self::NatsIdentity(value)
    }
}

impl From<ExactPloyzVersionError> for HostRunnerCliError {
    fn from(value: ExactPloyzVersionError) -> Self {
        Self::ExactPloyzVersion(value)
    }
}

impl fmt::Display for HostRunnerCliError {
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
            Self::MissingNatsServerArtifact => formatter.write_str(
                "first-machine install requires a nats-server artifact, \
                 but the release manifest carries none",
            ),
            Self::ArtifactTarget(error) => write!(formatter, "{error}"),
            Self::SupervisorUnit(error) => write!(formatter, "{error}"),
            Self::NatsIdentity(error) => write!(formatter, "{error}"),
            Self::RecoverySecret(error) => write!(formatter, "{error}"),
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

impl std::error::Error for HostRunnerCliError {}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use clap::Parser;
    use ployz_core::ids::OperationId;
    use ployz_nats::connect::NatsClientUrl;

    use super::{
        CloudHost, HostRunnerBootstrap, HostRunnerBootstrapMode, HostRunnerCliError,
        HostRunnerCommand, HostRunnerCoreDemote, HostRunnerCorePromote, HostRunnerStartup,
        HostRunnerSubstrateUpdate, HostRunnerSubstrateUpdateSource, SpecSource, load_command,
    };
    use crate::steps::JoinToken;

    fn load_startup(
        args: impl IntoIterator<Item = OsString>,
    ) -> Result<HostRunnerStartup, HostRunnerCliError> {
        let parsed = HostRunnerStartupCli::try_parse_from(
            std::iter::once(OsString::from("ployz host")).chain(args),
        )
        .map_err(HostRunnerCliError::Clap)?;
        super::load_startup_from_path(parsed.join_token_file)
    }

    #[derive(Debug, Parser)]
    #[command(name = "ployz host", disable_help_subcommand = true)]
    struct HostRunnerStartupCli {
        #[arg(long)]
        join_token_file: Option<PathBuf>,
    }

    #[test]
    fn parser_accepts_no_args() {
        let command = load_command([]).expect("no args are valid");

        assert_eq!(
            command,
            HostRunnerCommand::Start(super::HostRunnerStartup { join: None })
        );
    }

    #[test]
    fn parser_accepts_bare_bootstrap_as_local_guidance() {
        let command = load_command(["bootstrap".into()]).expect("bootstrap command is valid");

        assert_eq!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::LocalGuidance,
            })
        );
    }

    #[test]
    fn parser_accepts_cloud_bootstrap_with_custom_cloud_host() {
        let command = load_command([
            "bootstrap".into(),
            "cloud".into(),
            "--cloud-host".into(),
            "cloud.example.com".into(),
        ])
        .expect("bootstrap command is valid");

        assert_eq!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::CloudInteractive {
                    cloud_host: Some(CloudHost::try_new("https://cloud.example.com").unwrap()),
                },
            })
        );
    }

    #[test]
    fn parser_accepts_cloud_bootstrap_token_without_custom_host_and_redacts_debug() {
        let secret = "cloud_bootstrap_secret_123";
        let command = load_command([
            "bootstrap".into(),
            "cloud".into(),
            "--cloud-token".into(),
            secret.into(),
        ])
        .expect("cloud bootstrap token is valid");

        let debug = format!("{command:?}");
        assert!(debug.contains("CloudBootstrapToken([redacted])"));
        assert!(!debug.contains(secret));
        assert!(matches!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::CloudToken {
                    cloud_host: None,
                    cloud_token: _,
                },
            })
        ));
    }

    #[test]
    fn parser_accepts_cloud_bootstrap_token_with_custom_host() {
        let command = load_command([
            "bootstrap".into(),
            "cloud".into(),
            "--cloud-host".into(),
            "cloud.example.com".into(),
            "--cloud-token".into(),
            "cloud_bootstrap_secret_123".into(),
        ])
        .expect("cloud bootstrap token and host are valid");

        assert!(matches!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::CloudToken {
                    cloud_host: Some(host),
                    cloud_token: _,
                },
            }) if host == CloudHost::try_new("cloud.example.com").unwrap()
        ));
    }

    #[test]
    fn parser_rejects_invalid_cloud_bootstrap_tokens() {
        for token in ["cloud bootstrap token", "cloud\nbootstrap-token"] {
            assert!(matches!(
                load_command([
                    "bootstrap".into(),
                    "cloud".into(),
                    "--cloud-token".into(),
                    token.into(),
                ]),
                Err(HostRunnerCliError::Clap(error))
                    if error.kind() == clap::error::ErrorKind::ValueValidation
            ));
        }
    }

    #[test]
    fn parser_accepts_core_bootstrap() {
        let command =
            load_command(["bootstrap".into(), "core".into()]).expect("bootstrap command is valid");

        assert_eq!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::Core,
            })
        );
    }

    #[test]
    fn parser_accepts_join_bootstrap() {
        let command = load_command(["bootstrap".into(), "join".into(), "join_once_123".into()])
            .expect("bootstrap command is valid");

        assert_eq!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::Join {
                    join_token: JoinToken::try_new("join_once_123").unwrap(),
                },
            })
        );
    }

    #[test]
    fn parser_accepts_legacy_join_token_flag() {
        let command = load_command([
            "bootstrap".into(),
            "join".into(),
            "--join-token".into(),
            "join_once_123".into(),
        ])
        .expect("bootstrap command is valid");

        assert_eq!(
            command,
            HostRunnerCommand::Bootstrap(HostRunnerBootstrap {
                mode: HostRunnerBootstrapMode::Join {
                    join_token: JoinToken::try_new("join_once_123").unwrap(),
                },
            })
        );
    }

    #[test]
    fn parser_rejects_insecure_cloud_host() {
        assert!(matches!(
            load_command([
                "bootstrap".into(),
                "cloud".into(),
                "--cloud-host".into(),
                "http://cloud.example.com".into(),
            ]),
            Err(HostRunnerCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::ValueValidation
        ));
    }

    #[test]
    fn parser_rejects_missing_join_token_file() {
        assert!(matches!(
            load_startup(["--join-token-file".into()]),
            Err(HostRunnerCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::InvalidValue
                    || error.kind() == clap::error::ErrorKind::MissingRequiredArgument
        ));
    }

    #[test]
    fn parser_rejects_extra_args() {
        assert!(matches!(
            load_startup(["--join-token-file".into(), "/tmp/join".into(), "extra".into()]),
            Err(HostRunnerCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::UnknownArgument
        ));
    }

    #[test]
    fn parser_loads_first_machine_install_spec_command() {
        let path = write_temp_spec();
        let command = load_command(["install".into(), "--spec".into(), path.into()])
            .expect("first-machine install command loads");

        let HostRunnerCommand::FirstMachineInstall(target) = command else {
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
    fn parser_rejects_explicit_dns_opt_out_in_spec() {
        let path = write_temp_spec_with(FIRST_MACHINE_INSTALL_SPEC_NO_DNS, "no-dns");
        assert!(load_command(["install".into(), "--spec".into(), path.into()]).is_err());
    }

    #[test]
    fn parser_rejects_missing_first_machine_install_spec() {
        assert!(matches!(
            load_command(["install".into()]),
            Err(HostRunnerCliError::Clap(error))
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
            HostRunnerCommand::SubstrateUpdate(HostRunnerSubstrateUpdate {
                operation_id: Some(
                    OperationId::try_new("op_update_1").expect("valid operation id")
                ),
                source: HostRunnerSubstrateUpdateSource::Version(
                    "v0.0.2-alpha.16".parse().expect("exact version parses")
                ),
            })
        );
    }

    #[test]
    fn parser_loads_substrate_update_manifest_file() {
        let command = load_command([
            "substrate-update".into(),
            "--manifest-file".into(),
            "/tmp/ployz-dev-release.env".into(),
        ])
        .expect("substrate update command loads");

        assert_eq!(
            command,
            HostRunnerCommand::SubstrateUpdate(HostRunnerSubstrateUpdate {
                operation_id: None,
                source: HostRunnerSubstrateUpdateSource::ManifestFile(PathBuf::from(
                    "/tmp/ployz-dev-release.env"
                )),
            })
        );
    }

    #[test]
    fn parser_accepts_core_promote_check() {
        let command =
            load_command(["core-promote".into(), "--check".into()]).expect("command loads");

        assert_eq!(
            command,
            HostRunnerCommand::CorePromote(HostRunnerCorePromote {
                version: None,
                check: true,
            })
        );
    }

    #[test]
    fn parser_accepts_core_demote() {
        let command = load_command([
            "internal-core-demote".into(),
            "--successor-nats-url".into(),
            "tls://203.0.113.10:4222".into(),
        ])
        .expect("internal core demote command loads");

        assert_eq!(
            command,
            HostRunnerCommand::CoreDemote(HostRunnerCoreDemote {
                successor_nats_url: NatsClientUrl::try_new("tls://203.0.113.10:4222")
                    .expect("valid url"),
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
            Err(HostRunnerCliError::Clap(error))
                if error.kind() == clap::error::ErrorKind::ValueValidation
        ));
    }

    #[test]
    fn startup_parser_rejects_first_machine_install_without_subcommand_validation() {
        assert!(matches!(
            load_startup(["install".into()]),
            Err(HostRunnerCliError::Clap(error))
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
