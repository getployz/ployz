//! Command contracts for the local CLI and the machine-one bootstrap driver.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};
use ployz_core::MachineUpgradeUrl;
use ployz_core::corrosion::{
    AutomaticHostnameMode, CorrosionNamespaceName, CorrosionServiceName, StorageMode,
};
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::{DeployName, PeerName};
use ployz_core::install::{ExactPloyzVersion, InstallSha256Digest};
use ployz_core::join::{JoinBlob, JoinTokenTtlSeconds};
use ployz_core::machine::MachineName;
use ployz_core::network::{DEFAULT_ENDPOINT_SUPERNET, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::{RouteHostname, RoutePort};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Telemetry(TelemetryCommand),
    Init(Box<InitCommand>),
    Machine(MachineCommand),
    Token(TokenCommand),
    Namespace(NamespaceCommand),
    Deploy(DeployCommand),
    Ops(OpsCommand),
    Logs(LogsCommand),
    Peer(PeerCommand),
    Service(ServiceCommand),
    Route(RouteCommand),
    Status(DiagnosticsCommand),
    Doctor(DiagnosticsCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerCommand {
    Remove(PeerRemoveCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRemoveCommand {
    pub name: PeerName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceCommand {
    Create(NamespaceCreateCommand),
    Remove(NamespaceRemoveCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceCreateCommand {
    pub namespace: CorrosionNamespaceName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveCommand {
    pub namespace: CorrosionNamespaceName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub namespace: CorrosionNamespaceName,
    pub deploy: DeployName,
    pub services: ployz_core::DeployServices,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsCommand {
    List(OpsListCommand),
    Watch(OpsWatchCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsListCommand {
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsWatchCommand {
    pub namespace_name: CorrosionNamespaceName,
    pub deploy_name: DeployName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsCommand {
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub tail_lines: ployz_core::CorrosionLogsTailLines,
    /// Selects the replica hosted by the named machine when the service runs
    /// containers on more than one machine.
    pub machine: Option<MachineName>,
    pub follow: bool,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceCommand {
    Remove(ServiceRemoveCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRemoveCommand {
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteCommand {
    Attach(RouteAttachCommand),
    Remove(RouteRemoveCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAttachCommand {
    pub hostname: RouteHostname,
    pub namespace: CorrosionNamespaceName,
    pub service: CorrosionServiceName,
    pub endpoint_port: RoutePort,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRemoveCommand {
    pub hostname: RouteHostname,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsCommand {
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineCommand {
    List(MachineListCommand),
    Remove(MachineRemoveCommand),
    EndpointSet(MachineEndpointSetCommand),
    Upgrade(MachineUpgradeCommand),
    Join(MachineJoinCommand),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineListCommand {
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRemoveCommand {
    pub machine: MachineName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineEndpointSetCommand {
    pub machine: MachineName,
    pub endpoint: SocketAddr,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpgradeCommand {
    pub selector: MachineUpgradeSelector,
    pub source: MachineUpgradeSource,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineUpgradeSelector {
    Names(Vec<MachineName>),
    All,
    Outdated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineUpgradeSource {
    Channel,
    Version(ExactPloyzVersion),
    Manual {
        url: MachineUpgradeUrl,
        sha256: InstallSha256Digest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineJoinCommand {
    pub blob: JoinBlob,
    pub storage: InitStorageChoice,
    pub wireguard_endpoint: Option<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCommand {
    Create(TokenCreateCommand),
    List(TokenListCommand),
    Revoke(TokenRevokeCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCreateCommand {
    pub name: ployz_core::ids::TokenName,
    pub ttl: JoinTokenTtlSeconds,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenListCommand {
    pub include_expired: bool,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRevokeCommand {
    pub token_id: ployz_core::ids::TokenName,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryCommand {
    Enable,
    Disable,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CloudToken(String);

impl CloudToken {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudToken([REDACTED])")
    }
}

impl FromStr for CloudToken {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("cloud token must not be empty".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitCommand {
    pub driver: InitDriver,
    pub storage: InitStorageChoice,
    pub container_network: MachineEndpointSupernet,
    pub service_urls: AutomaticHostnameMode,
    pub cluster_name: Option<String>,
    pub machine_name: Option<String>,
    pub wireguard_endpoint: Option<SocketAddr>,
    pub prompt: InitPromptMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitPromptMask {
    pub storage: bool,
    pub container_network: bool,
    pub service_urls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitDriver {
    OnHost,
    Cloud(CloudToken),
    SshTarget(SshTarget),
    SshPeer(DriverPeerArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget(String);

impl SshTarget {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn inferred_wireguard_endpoint(&self) -> Option<SocketAddr> {
        let host = self.0.strip_prefix("root@")?;
        let host = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host);
        host.parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT))
    }
}

impl FromStr for SshTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(host) = value.strip_prefix("root@") else {
            return Err("SSH init target must be root@<host>".to_owned());
        };
        if host.is_empty()
            || !host.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-' | b'[' | b']')
            })
        {
            return Err("SSH init target contains an invalid host".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for SshTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for SshTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SshTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Public peer material carried across the already-authenticated SSH channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverPeerArgs {
    pub id: PeerName,
    pub public_key: WireGuardPublicKey,
}

/// Parses a `ployz` invocation.
pub fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, clap::Error> {
    let parsed = Cli::try_parse_from(std::iter::once("ployz".to_owned()).chain(args))?;
    let Some(command) = parsed.command else {
        return Err(root_help());
    };

    match command {
        CommandCli::Telemetry { command } => Ok(Command::Telemetry(match command {
            TelemetryCli::Enable => TelemetryCommand::Enable,
            TelemetryCli::Disable => TelemetryCommand::Disable,
        })),
        CommandCli::Init(args) => args
            .into_command()
            .map(Box::new)
            .map(Command::Init)
            .map_err(clap_value_error),
        CommandCli::Machine { command } => Ok(Command::Machine(match command {
            MachineCli::List(args) => MachineCommand::List(MachineListCommand {
                target: args.target,
            }),
            MachineCli::Remove(args) => MachineCommand::Remove(MachineRemoveCommand {
                machine: MachineName::try_new(args.machine)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
            MachineCli::Endpoint { command } => match command {
                MachineEndpointCli::Set(args) => {
                    MachineCommand::EndpointSet(MachineEndpointSetCommand {
                        machine: MachineName::try_new(args.machine)
                            .map_err(|error| clap_value_error(error.to_string()))?,
                        endpoint: args.endpoint,
                        target: args.target,
                    })
                }
            },
            MachineCli::Upgrade(args) => {
                MachineCommand::Upgrade(args.into_command().map_err(clap_value_error)?)
            }
            MachineCli::Join(args) => MachineCommand::Join(MachineJoinCommand {
                blob: args.blob,
                storage: args
                    .storage
                    .map_or(InitStorageChoice::Automatic, |value| value.0),
                wireguard_endpoint: args.wireguard_endpoint,
            }),
            MachineCli::Reset => MachineCommand::Reset,
        })),
        CommandCli::Token { command } => Ok(Command::Token(match command {
            TokenCli::Create(args) => TokenCommand::Create(TokenCreateCommand {
                name: args.name,
                ttl: parse_ttl(&args.ttl).map_err(clap_value_error)?,
                target: args.target,
            }),
            TokenCli::List(args) => TokenCommand::List(TokenListCommand {
                include_expired: args.all,
                target: args.target,
            }),
            TokenCli::Revoke(args) => TokenCommand::Revoke(TokenRevokeCommand {
                token_id: args.token_id,
                target: args.target,
            }),
        })),
        CommandCli::Namespace { command } => Ok(Command::Namespace(match command {
            NamespaceCli::Create(args) => NamespaceCommand::Create(NamespaceCreateCommand {
                namespace: CorrosionNamespaceName::try_new(args.namespace)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
            NamespaceCli::Remove(args) => NamespaceCommand::Remove(NamespaceRemoveCommand {
                namespace: CorrosionNamespaceName::try_new(args.namespace)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
        })),
        CommandCli::Deploy(args) => args
            .into_command()
            .map(Command::Deploy)
            .map_err(clap_value_error),
        CommandCli::Ops { command } => Ok(Command::Ops(match command {
            OpsCli::List(args) => OpsCommand::List(OpsListCommand {
                target: args.target,
            }),
            OpsCli::Watch(args) => OpsCommand::Watch(OpsWatchCommand {
                namespace_name: args.namespace_name,
                deploy_name: args.deploy_name,
                target: args.target,
            }),
        })),
        CommandCli::Logs(args) => Ok(Command::Logs(LogsCommand {
            namespace_name: CorrosionNamespaceName::try_new(args.namespace)
                .map_err(|error| clap_value_error(error.to_string()))?,
            service_name: CorrosionServiceName::try_new(args.service)
                .map_err(|error| clap_value_error(error.to_string()))?,
            tail_lines: ployz_core::CorrosionLogsTailLines::try_new(args.tail)
                .map_err(|error| clap_value_error(error.to_string()))?,
            machine: args
                .machine
                .map(MachineName::try_new)
                .transpose()
                .map_err(|error| clap_value_error(error.to_string()))?,
            follow: args.follow,
            target: args.target,
        })),
        CommandCli::Peer { command } => Ok(Command::Peer(match command {
            PeerCli::Remove(args) => PeerCommand::Remove(PeerRemoveCommand {
                name: PeerName::try_new(args.name)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
        })),
        CommandCli::Service { command } => Ok(Command::Service(match command {
            ServiceCli::Remove(args) => ServiceCommand::Remove(ServiceRemoveCommand {
                namespace_name: args.namespace_name,
                service_name: args.service_name,
                target: args.target,
            }),
        })),
        CommandCli::Route { command } => Ok(Command::Route(match command {
            RouteCli::Attach(args) => RouteCommand::Attach(RouteAttachCommand {
                hostname: RouteHostname::try_new(args.hostname)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                namespace: CorrosionNamespaceName::try_new(args.namespace)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                service: CorrosionServiceName::try_new(args.service)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                endpoint_port: RoutePort::try_new(args.port)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
            RouteCli::Remove(args) => RouteCommand::Remove(RouteRemoveCommand {
                hostname: RouteHostname::try_new(args.hostname)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
        })),
        CommandCli::Status(args) => Ok(Command::Status(DiagnosticsCommand {
            target: args.target,
        })),
        CommandCli::Doctor(args) => Ok(Command::Doctor(DiagnosticsCommand {
            target: args.target,
        })),
    }
}

fn clap_value_error(message: String) -> clap::Error {
    clap::Error::raw(clap::error::ErrorKind::ValueValidation, message)
}

fn root_help() -> clap::Error {
    Cli::try_parse_from(["ployz", "--help"]).expect_err("--help always produces a display result")
}

#[derive(Debug, Parser)]
#[command(
    name = "ployz",
    version,
    about = "Command small Ployz clusters",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandCli>,
}

#[derive(Debug, Subcommand)]
enum CommandCli {
    /// Found a Ployz cluster on this machine or over SSH.
    Init(Box<InitArgs>),
    /// Configure anonymous usage and error telemetry.
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCli,
    },
    /// Inspect and manage cluster machines.
    Machine {
        #[command(subcommand)]
        command: MachineCli,
    },
    /// Mint, inspect, and revoke join tokens.
    Token {
        #[command(subcommand)]
        command: TokenCli,
    },
    /// Manage operator peers.
    Peer {
        #[command(subcommand)]
        command: PeerCli,
    },
    /// Create and remove namespaces.
    Namespace {
        #[command(subcommand)]
        command: NamespaceCli,
    },
    /// Deploy or update the sole service in a namespace.
    Deploy(DeployArgs),
    /// List or watch coarse deploy-operation state.
    Ops {
        #[command(subcommand)]
        command: OpsCli,
    },
    /// Tail or follow one service's current container logs.
    Logs(LogsArgs),
    /// Manage services.
    Service {
        #[command(subcommand)]
        command: ServiceCli,
    },
    /// Manage route bindings.
    Route {
        #[command(subcommand)]
        command: RouteCli,
    },
    /// Show a cheap cluster-health summary.
    Status(DiagnosticsArgs),
    /// Sweep replicated state for actionable anomalies.
    Doctor(DiagnosticsArgs),
}

#[derive(Debug, Subcommand)]
enum PeerCli {
    /// Remove one peer row, refusing ambiguous names unless an id is supplied.
    #[command(name = "rm")]
    Remove(PeerRemoveArgs),
}

#[derive(Debug, Args)]
struct PeerRemoveArgs {
    name: String,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Subcommand)]
enum NamespaceCli {
    /// Create one namespace.
    Create(NamespaceCreateArgs),
    /// Remove one empty namespace.
    #[command(name = "rm")]
    Remove(NamespaceRemoveArgs),
}

#[derive(Debug, Args)]
struct NamespaceCreateArgs {
    namespace: String,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct NamespaceRemoveArgs {
    namespace: String,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct DeployArgs {
    namespace: String,
    deploy: String,
    /// JSON namespace manifest containing the complete name-keyed `services` object.
    #[arg(long, value_name = "PATH")]
    file: PathBuf,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployManifest {
    services: ployz_core::DeployServices,
}

impl DeployArgs {
    fn into_command(self) -> Result<DeployCommand, String> {
        let bytes = std::fs::read(&self.file)
            .map_err(|error| format!("could not read {}: {error}", self.file.display()))?;
        let manifest: DeployManifest = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid deploy manifest {}: {error}", self.file.display()))?;
        Ok(DeployCommand {
            namespace: CorrosionNamespaceName::try_new(self.namespace)
                .map_err(|error| error.to_string())?,
            deploy: DeployName::try_new(self.deploy).map_err(|error| error.to_string())?,
            services: manifest.services,
            target: self.target,
        })
    }
}

#[derive(Debug, Subcommand)]
enum OpsCli {
    /// List converged operation summaries.
    List(OpsListArgs),
    /// Watch one deploy operation's coarse state.
    Watch(OpsWatchArgs),
}

#[derive(Debug, Args)]
struct OpsListArgs {
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Subcommand)]
enum ServiceCli {
    /// Remove one canonically named service from a namespace.
    #[command(name = "rm")]
    Remove(ServiceRemoveArgs),
}

#[derive(Debug, Args)]
struct ServiceRemoveArgs {
    namespace_name: CorrosionNamespaceName,
    service_name: CorrosionServiceName,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Subcommand)]
enum RouteCli {
    /// Attach one hostname to a named service.
    Attach(RouteAttachArgs),
    /// Remove one canonically named route binding.
    #[command(name = "rm")]
    Remove(RouteRemoveArgs),
}

#[derive(Debug, Args)]
struct RouteAttachArgs {
    hostname: String,
    #[arg(long)]
    namespace: String,
    #[arg(long)]
    service: String,
    #[arg(long)]
    port: u16,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct RouteRemoveArgs {
    hostname: String,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct OpsWatchArgs {
    namespace_name: CorrosionNamespaceName,
    deploy_name: DeployName,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct LogsArgs {
    namespace: String,
    service: String,
    /// Number of existing lines to print before following.
    #[arg(long, default_value_t = 100)]
    tail: u16,
    /// Select the replica hosted by this machine when the service runs on
    /// more than one.
    #[arg(long = "machine", value_name = "NAME")]
    machine: Option<String>,
    /// Continue following new log lines.
    #[arg(short = 'f', long)]
    follow: bool,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct DiagnosticsArgs {
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Subcommand)]
enum MachineCli {
    /// List current machines.
    #[command(name = "ls")]
    List(MachineListArgs),
    /// Fence a machine from the roster and sweep its testimony.
    #[command(name = "rm")]
    Remove(MachineRemoveArgs),
    /// Manage a machine's public WireGuard endpoint.
    Endpoint {
        #[command(subcommand)]
        command: MachineEndpointCli,
    },
    /// Stage and apply a verified ployzd binary on selected machines.
    Upgrade(MachineUpgradeArgs),
    /// Join this Linux machine to a cluster through its public token door.
    Join(MachineJoinArgs),
    /// Remove this Linux machine's Ployz state so it can join afresh.
    Reset,
}

#[derive(Debug, Subcommand)]
enum MachineEndpointCli {
    /// Record the endpoint token creation will publish in join blobs.
    Set(MachineEndpointSetArgs),
}

#[derive(Debug, Args)]
struct MachineEndpointSetArgs {
    machine: String,
    #[arg(value_parser = parse_nonzero_socket_addr)]
    endpoint: SocketAddr,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct MachineRemoveArgs {
    machine: String,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct MachineUpgradeArgs {
    /// Machines to upgrade by their roster names.
    #[arg(value_name = "MACHINE", conflicts_with_all = ["all", "outdated"])]
    machines: Vec<String>,
    /// Upgrade every rostered machine, one at a time.
    #[arg(long, conflicts_with = "outdated")]
    all: bool,
    /// Upgrade each machine whose reported version is behind the target.
    #[arg(long)]
    outdated: bool,
    /// Use an exact released Ployz version, including an intentional downgrade.
    #[arg(long, conflicts_with_all = ["url", "sha256"])]
    version: Option<ExactPloyzVersion>,
    /// Fetch a manually supplied HTTPS artifact URL from each target machine.
    #[arg(long, requires = "sha256", conflicts_with = "version")]
    url: Option<String>,
    /// SHA-256 for --url; a manually supplied URL and digest are inseparable.
    #[arg(long, requires = "url", conflicts_with = "version")]
    sha256: Option<String>,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

impl MachineUpgradeArgs {
    fn into_command(self) -> Result<MachineUpgradeCommand, String> {
        if let Some(duplicate) = self
            .machines
            .iter()
            .enumerate()
            .find_map(|(index, machine)| {
                self.machines
                    .iter()
                    .take(index)
                    .any(|prior| prior == machine)
                    .then_some(machine.as_str())
            })
        {
            return Err(format!(
                "machine {duplicate} was selected more than once; list each machine once"
            ));
        }
        let selector = match (self.machines.as_slice(), self.all, self.outdated) {
            ([], false, false) => {
                return Err(
                    "choose machine names, --all, or --outdated; upgrading every machine requires --all"
                        .to_owned(),
                );
            }
            ([], true, false) => MachineUpgradeSelector::All,
            ([], false, true) => MachineUpgradeSelector::Outdated,
            ([_, ..], false, false) => MachineUpgradeSelector::Names(
                self.machines
                    .into_iter()
                    .map(MachineName::try_new)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?,
            ),
            _ => {
                return Err(
                    "machine upgrade selectors are mutually exclusive: use names, --all, or --outdated"
                        .to_owned(),
                );
            }
        };
        let source = match (self.version, self.url, self.sha256) {
            (None, None, None) => MachineUpgradeSource::Channel,
            (Some(version), None, None) => MachineUpgradeSource::Version(version),
            (None, Some(url), Some(sha256)) => MachineUpgradeSource::Manual {
                url: MachineUpgradeUrl::try_new(url).map_err(|error| error.to_string())?,
                sha256: InstallSha256Digest::try_new(sha256).map_err(|error| error.to_string())?,
            },
            (None, Some(_), None) | (None, None, Some(_)) => {
                return Err("--url and --sha256 must be provided together".to_owned());
            }
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                return Err("--version cannot be combined with --url or --sha256".to_owned());
            }
        };
        if matches!(selector, MachineUpgradeSelector::Outdated)
            && matches!(source, MachineUpgradeSource::Manual { .. })
        {
            return Err(
                "--outdated needs a released target version; use machine names or --all with --url and --sha256"
                    .to_owned(),
            );
        }
        Ok(MachineUpgradeCommand {
            selector,
            source,
            target: self.target,
        })
    }
}

#[derive(Debug, Args)]
struct MachineJoinArgs {
    blob: JoinBlob,
    /// Machine storage selection. Automatic applies the cluster default and host eligibility.
    #[arg(long)]
    storage: Option<StorageArg>,
    /// Public WireGuard endpoint. Omit for a roaming/NAT'd machine.
    #[arg(long, value_parser = parse_nonzero_socket_addr)]
    wireguard_endpoint: Option<SocketAddr>,
}

#[derive(Debug, Subcommand)]
enum TokenCli {
    /// Mint a show-once join credential.
    Create(TokenCreateArgs),
    /// List live join tokens.
    List(TokenListArgs),
    /// Revoke a join token by deleting its row.
    Revoke(TokenRevokeArgs),
}

#[derive(Debug, Args)]
struct TokenCreateArgs {
    /// Durable name for the token.
    name: ployz_core::ids::TokenName,
    /// Token lifetime, expressed as whole seconds, minutes, hours, or days.
    #[arg(long, default_value = "24h")]
    ttl: String,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct TokenListArgs {
    /// Include expired tokens.
    #[arg(long)]
    all: bool,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct TokenRevokeArgs {
    token_id: ployz_core::ids::TokenName,
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct MachineListArgs {
    /// Select the cluster founded through this SSH target.
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Remote root SSH target. Omit when running on machine one.
    target: Option<SshTarget>,
    /// Machine-one storage selection.
    #[arg(long)]
    storage: Option<StorageArg>,
    /// Cluster-fixed container network. Must be an IPv4 /16.
    #[arg(long)]
    container_network: Option<String>,
    /// Automatic URLs: disabled or custom:<suffix>.
    #[arg(long)]
    service_urls: Option<ServiceUrlsArg>,
    /// Cluster name. Defaults to machine one's hostname.
    #[arg(long)]
    cluster_name: Option<String>,
    /// Machine name. Defaults to machine one's hostname.
    #[arg(long)]
    machine_name: Option<String>,
    /// Public WireGuard endpoint when it cannot be detected.
    #[arg(long)]
    wireguard_endpoint: Option<SocketAddr>,
    /// Cloud callback and public-enrollment envelope.
    #[arg(long, conflicts_with = "target")]
    cloud_token: Option<CloudToken>,
    #[arg(long, hide = true, conflicts_with_all = ["target", "cloud_token"], requires = "driver_peer_public_key")]
    driver_peer_id: Option<PeerName>,
    #[arg(long, hide = true, conflicts_with_all = ["target", "cloud_token"], requires = "driver_peer_id", value_parser = parse_wireguard_public_key)]
    driver_peer_public_key: Option<WireGuardPublicKey>,
}

impl InitArgs {
    fn into_command(self) -> Result<InitCommand, String> {
        let prompt = InitPromptMask {
            storage: self.storage.is_none(),
            container_network: self.container_network.is_none(),
            service_urls: self.service_urls.is_none(),
        };
        let driver = match (
            self.target,
            self.cloud_token,
            self.driver_peer_id,
            self.driver_peer_public_key,
        ) {
            (Some(target), None, None, None) => InitDriver::SshTarget(target),
            (None, Some(token), None, None) => InitDriver::Cloud(token),
            (None, None, Some(id), Some(public_key)) => {
                InitDriver::SshPeer(DriverPeerArgs { id, public_key })
            }
            (None, None, None, None) => InitDriver::OnHost,
            _ => return Err("init driver arguments are inconsistent".to_owned()),
        };
        Ok(InitCommand {
            driver,
            storage: self
                .storage
                .map_or(InitStorageChoice::Automatic, |value| value.0),
            container_network: MachineEndpointSupernet::try_new(
                self.container_network
                    .unwrap_or_else(|| DEFAULT_ENDPOINT_SUPERNET.to_owned()),
            )
            .map_err(|error| error.to_string())?,
            service_urls: self
                .service_urls
                .map_or(AutomaticHostnameMode::Disabled, |value| value.0),
            cluster_name: self.cluster_name,
            machine_name: self.machine_name,
            wireguard_endpoint: self.wireguard_endpoint,
            prompt,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StorageArg(InitStorageChoice);

impl FromStr for StorageArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_storage(value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceUrlsArg(AutomaticHostnameMode);

impl FromStr for ServiceUrlsArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_service_urls(value).map(Self)
    }
}

pub(crate) fn parse_storage(value: &str) -> Result<InitStorageChoice, String> {
    match value {
        "auto" => Ok(InitStorageChoice::Automatic),
        "zfs" => Ok(InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        }),
        "plain" => Ok(InitStorageChoice::Flag {
            mode: StorageMode::Plain,
        }),
        _ => Err("storage must be auto, zfs, or plain".to_owned()),
    }
}

#[must_use]
pub(crate) const fn render_storage(value: InitStorageChoice) -> &'static str {
    match value {
        InitStorageChoice::Automatic => "auto",
        InitStorageChoice::Flag {
            mode: StorageMode::Zfs,
        } => "zfs",
        InitStorageChoice::Flag {
            mode: StorageMode::Plain,
        } => "plain",
    }
}

pub(crate) fn parse_service_urls(value: &str) -> Result<AutomaticHostnameMode, String> {
    match value {
        "disabled" => Ok(AutomaticHostnameMode::Disabled),
        _ => {
            let Some(suffix) = value.strip_prefix("custom:") else {
                return Err("service URLs must be disabled or custom:<suffix>".to_owned());
            };
            Ok(AutomaticHostnameMode::Custom {
                suffix: RouteHostname::try_new(suffix).map_err(|error| error.to_string())?,
            })
        }
    }
}

#[must_use]
pub(crate) fn render_service_urls(value: &AutomaticHostnameMode) -> String {
    match value {
        AutomaticHostnameMode::Disabled => "disabled".to_owned(),
        AutomaticHostnameMode::Custom { suffix } => format!("custom:{}", suffix.as_str()),
    }
}

fn parse_wireguard_public_key(value: &str) -> Result<WireGuardPublicKey, String> {
    WireGuardPublicKey::try_new(value).map_err(|error| error.to_string())
}

fn parse_nonzero_socket_addr(value: &str) -> Result<SocketAddr, String> {
    let endpoint = value
        .parse::<SocketAddr>()
        .map_err(|error| error.to_string())?;
    if endpoint.port() == 0 {
        return Err("endpoint port must not be zero".to_owned());
    }
    Ok(endpoint)
}

fn parse_ttl(value: &str) -> Result<JoinTokenTtlSeconds, String> {
    let Some((digits, suffix)) = value.split_at_checked(value.len().saturating_sub(1)) else {
        return Err("TTL must be a positive whole duration such as 24h".to_owned());
    };
    let amount = digits
        .parse::<u64>()
        .map_err(|_| "TTL must be a positive whole duration such as 24h".to_owned())?;
    if amount == 0 {
        return Err("TTL must be greater than zero".to_owned());
    }
    let multiplier = match suffix {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err("TTL unit must be s, m, h, or d".to_owned()),
    };
    let seconds = amount
        .checked_mul(multiplier)
        .ok_or_else(|| "TTL is too large".to_owned())?;
    let seconds = u32::try_from(seconds).map_err(|_| "TTL is too large".to_owned())?;
    JoinTokenTtlSeconds::try_new(seconds).map_err(|error| error.to_string())
}

#[derive(Debug, Subcommand)]
enum TelemetryCli {
    /// Enable telemetry.
    Enable,
    /// Disable telemetry.
    Disable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_manifest_is_a_complete_service_list() {
        let manifest: DeployManifest = serde_json::from_value(serde_json::json!({
            "services": {
                "api": {
                    "image": "registry.example/api:latest",
                    "runtime": {
                        "environment": {},
                        "volume_mounts": []
                    }
                },
                "worker": {
                    "image": "registry.example/worker:latest",
                    "runtime": {
                        "environment": {},
                        "volume_mounts": []
                    }
                }
            }
        }))
        .expect("manifest");

        assert!(
            manifest
                .services
                .get(&CorrosionServiceName::try_new("api").expect("api"))
                .is_some()
        );
        assert!(
            manifest
                .services
                .get(&CorrosionServiceName::try_new("worker").expect("worker"))
                .is_some()
        );
    }

    #[test]
    fn logs_are_selected_by_namespace_and_service_names() {
        let Command::Logs(command) =
            parse_command(["logs", "production", "api"].map(str::to_owned)).expect("logs command")
        else {
            panic!("logs command");
        };
        assert_eq!(command.namespace_name.as_str(), "production");
        assert_eq!(command.service_name.as_str(), "api");
    }

    #[test]
    fn bare_invocation_displays_help() {
        let error = parse_command(std::iter::empty()).expect_err("command is required");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
