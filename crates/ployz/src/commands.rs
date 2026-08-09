//! Command contracts for the local CLI and the machine-one bootstrap driver.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};
use ployz_core::corrosion::{
    AutomaticHostnameMode, CorrosionNamespaceName, CorrosionServiceName, HostPortBinding,
    HostPortBindings, HostPortProtocol, ServiceReplicaCount, StorageMode,
};
use ployz_core::deploy::{EnvName, EnvValue, ImageReference, ServiceEnvironment};
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::{
    MachineRowId, NamespaceRowId, OperationRowId, PeerId, RouteBindingRowId, ServiceRowId,
};
use ployz_core::install::{ExactPloyzVersion, InstallSha256Digest};
use ployz_core::join::{JoinBlob, JoinTokenTtlSeconds};
use ployz_core::machine::MachineName;
use ployz_core::network::{DEFAULT_ENDPOINT_SUPERNET, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::{RouteHostname, RoutePort};
use ployz_core::{
    HealthGatePolicy, MachineUpgradeUrl, PinnedMachineNames, RequestedPins, RequestedPlacement,
};
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
    pub name: String,
    pub peer_id: Option<PeerId>,
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
    pub namespace_id: Option<NamespaceRowId>,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCommand {
    pub namespace: CorrosionNamespaceName,
    pub service: CorrosionServiceName,
    pub image: ImageReference,
    pub environment: ServiceEnvironment,
    pub health_gate: HealthGatePolicy,
    /// `None` inherits the service row's placement unchanged.
    pub placement: Option<RequestedPlacement>,
    /// `None` inherits the service row's pin set unchanged.
    pub machines: Option<RequestedPins>,
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
    pub operation_id: OperationRowId,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsCommand {
    pub service: CorrosionServiceName,
    pub service_id: Option<ServiceRowId>,
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
    pub namespace_id: NamespaceRowId,
    pub name: String,
    pub service_id: Option<ServiceRowId>,
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
    pub namespace_id: Option<NamespaceRowId>,
    pub service: CorrosionServiceName,
    pub service_id: Option<ServiceRowId>,
    pub endpoint_port: RoutePort,
    pub target: Option<SshTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRemoveCommand {
    pub hostname: RouteHostname,
    pub route_id: Option<RouteBindingRowId>,
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
    pub machine_id: Option<MachineRowId>,
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
    pub token_id: ployz_core::ids::TokenId,
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
    pub id: PeerId,
    pub name: String,
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
                machine_id: args.machine_id,
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
                namespace_id: args.namespace_id,
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
                operation_id: args.operation_id,
                target: args.target,
            }),
        })),
        CommandCli::Logs(args) => Ok(Command::Logs(LogsCommand {
            service: CorrosionServiceName::try_new(args.service)
                .map_err(|error| clap_value_error(error.to_string()))?,
            service_id: args.service_id,
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
                name: args.name,
                peer_id: args.id,
                target: args.target,
            }),
        })),
        CommandCli::Service { command } => Ok(Command::Service(match command {
            ServiceCli::Remove(args) => ServiceCommand::Remove(ServiceRemoveCommand {
                namespace_id: args.namespace_id,
                name: args.name,
                service_id: args.id,
                target: args.target,
            }),
        })),
        CommandCli::Route { command } => Ok(Command::Route(match command {
            RouteCli::Attach(args) => RouteCommand::Attach(RouteAttachCommand {
                hostname: RouteHostname::try_new(args.hostname)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                namespace: CorrosionNamespaceName::try_new(args.namespace)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                namespace_id: args.namespace_id,
                service: CorrosionServiceName::try_new(args.service)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                service_id: args.service_id,
                endpoint_port: RoutePort::try_new(args.port)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                target: args.target,
            }),
            RouteCli::Remove(args) => RouteCommand::Remove(RouteRemoveCommand {
                hostname: RouteHostname::try_new(args.hostname)
                    .map_err(|error| clap_value_error(error.to_string()))?,
                route_id: args.id,
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
    id: Option<PeerId>,
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
    /// Select one exact row when this name is ambiguous.
    #[arg(long = "id")]
    namespace_id: Option<NamespaceRowId>,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct DeployArgs {
    namespace: String,
    service: String,
    image: String,
    /// Set an environment value as NAME=VALUE. Values are redacted from diagnostics.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    environment: Vec<String>,
    /// Emergency skip: deploy without waiting for the container to prove healthy.
    #[arg(long = "no-health-gate")]
    no_health_gate: bool,
    /// Service mode: replicated runs a chosen replica count; global runs one
    /// replica on every eligible machine.
    #[arg(long, value_enum)]
    mode: Option<DeployModeArg>,
    /// Replica count for a replicated service.
    #[arg(long, value_name = "N")]
    replicas: Option<u16>,
    /// Publish a host port into a global service (protocol defaults to tcp).
    #[arg(
        short = 'p',
        long = "publish",
        value_name = "[HOST:]CONTAINER[/PROTOCOL]"
    )]
    publish: Vec<String>,
    /// Pin placement to a named machine (repeatable); `--machine any` clears
    /// the pin set.
    #[arg(long = "machine", value_name = "NAME")]
    machines: Vec<String>,
    #[arg(long)]
    target: Option<SshTarget>,
}

/// Service mode requested on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum DeployModeArg {
    Replicated,
    Global,
}

/// The pin-set literal that clears a service's pinned machines.
const ANY_MACHINE_PIN: &str = "any";

impl DeployArgs {
    fn into_command(self) -> Result<DeployCommand, String> {
        let mut environment = BTreeMap::new();
        for assignment in self.environment {
            let Some((name, value)) = assignment.split_once('=') else {
                return Err("--env must be NAME=VALUE".to_owned());
            };
            let name = EnvName::try_new(name).map_err(|error| error.to_string())?;
            let value = EnvValue::try_new(value).map_err(|error| error.to_string())?;
            if environment.insert(name.clone(), value).is_some() {
                return Err(format!(
                    "environment variable {} was supplied more than once",
                    name.as_str()
                ));
            }
        }
        Ok(DeployCommand {
            namespace: CorrosionNamespaceName::try_new(self.namespace)
                .map_err(|error| error.to_string())?,
            service: CorrosionServiceName::try_new(self.service)
                .map_err(|error| error.to_string())?,
            image: ImageReference::try_new(self.image).map_err(|error| error.to_string())?,
            environment: ServiceEnvironment::from(environment),
            health_gate: if self.no_health_gate {
                HealthGatePolicy::Skip
            } else {
                HealthGatePolicy::Enforce
            },
            placement: requested_placement(self.mode, self.replicas, &self.publish)?,
            machines: requested_pins(self.machines)?,
            target: self.target,
        })
    }
}

/// Maps the placement flags to a placement request. No flags means `None`:
/// the deploy inherits the service row's placement unchanged.
fn requested_placement(
    mode: Option<DeployModeArg>,
    replicas: Option<u16>,
    publish: &[String],
) -> Result<Option<RequestedPlacement>, String> {
    let mut bindings = Vec::new();
    for spec in publish {
        bindings.push(parse_publish(spec)?);
    }
    match mode {
        Some(DeployModeArg::Global) => {
            if replicas.is_some() {
                return Err(
                    "global runs one replica on every eligible machine; drop --replicas".to_owned(),
                );
            }
            let host_ports =
                HostPortBindings::try_new(bindings).map_err(|error| error.to_string())?;
            Ok(Some(RequestedPlacement::Global { host_ports }))
        }
        Some(DeployModeArg::Replicated) => {
            if !bindings.is_empty() {
                return Err(
                    "published host ports are legal only on global services; expose a replicated service through a route binding"
                        .to_owned(),
                );
            }
            // A count-free `--mode replicated` keeps a replicated incumbent's
            // count; the server defaults one replica on a first deploy or a
            // global conversion.
            let replicas = replicas
                .map(ServiceReplicaCount::try_new)
                .transpose()
                .map_err(|error| error.to_string())?;
            Ok(Some(RequestedPlacement::Replicated { replicas }))
        }
        None => {
            if !bindings.is_empty() {
                return Err(
                    "published host ports are legal only on global services; expose a replicated service through a route binding"
                        .to_owned(),
                );
            }
            // A bare `--replicas N` is the mode-preserving count change.
            match replicas {
                Some(replicas) => {
                    let replicas = ServiceReplicaCount::try_new(replicas)
                        .map_err(|error| error.to_string())?;
                    Ok(Some(RequestedPlacement::Replicas { replicas }))
                }
                None => Ok(None),
            }
        }
    }
}

/// Maps repeated `--machine` flags to a pin request. `any` clears the pin
/// set and cannot be combined with machine names.
fn requested_pins(machines: Vec<String>) -> Result<Option<RequestedPins>, String> {
    if machines.is_empty() {
        return Ok(None);
    }
    let any_count = machines
        .iter()
        .filter(|machine| machine.as_str() == ANY_MACHINE_PIN)
        .count();
    if any_count > 0 {
        if any_count < machines.len() {
            return Err(
                "--machine any clears the pin set and cannot be combined with machine names"
                    .to_owned(),
            );
        }
        return Ok(Some(RequestedPins::Any));
    }
    let mut names = Vec::new();
    for machine in machines {
        names.push(MachineName::try_new(machine).map_err(|error| error.to_string())?);
    }
    let names = PinnedMachineNames::try_new(names).map_err(|error| error.to_string())?;
    Ok(Some(RequestedPins::Machines { names }))
}

/// Parses one `--publish` value: `[host:]container[/protocol]`, protocol
/// defaulting to tcp, host defaulting to the container port.
fn parse_publish(spec: &str) -> Result<HostPortBinding, String> {
    let (ports, protocol) = match spec.rsplit_once('/') {
        Some((ports, "tcp")) => (ports, HostPortProtocol::Tcp),
        Some((ports, "udp")) => (ports, HostPortProtocol::Udp),
        Some((_, other)) => {
            return Err(format!(
                "--publish protocol must be tcp or udp, got {other}"
            ));
        }
        None => (spec, HostPortProtocol::Tcp),
    };
    let (host, container) = match ports.split_once(':') {
        Some((host, container)) => (host, container),
        None => (ports, ports),
    };
    Ok(HostPortBinding {
        host_port: parse_published_port(host)?,
        container_port: parse_published_port(container)?,
        protocol,
    })
}

fn parse_published_port(value: &str) -> Result<std::num::NonZeroU16, String> {
    value
        .parse::<std::num::NonZeroU16>()
        .map_err(|_| format!("--publish ports must be between 1 and 65535, got {value:?}"))
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
    /// Remove one service row, refusing ambiguous names unless an id is supplied.
    #[command(name = "rm")]
    Remove(ServiceRemoveArgs),
}

#[derive(Debug, Args)]
struct ServiceRemoveArgs {
    name: String,
    /// Namespace row that owns this service identity.
    #[arg(long)]
    namespace_id: NamespaceRowId,
    #[arg(long)]
    id: Option<ServiceRowId>,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Subcommand)]
enum RouteCli {
    /// Attach one hostname to a named service.
    Attach(RouteAttachArgs),
    /// Remove one route-binding row, refusing ambiguous hostnames unless an id is supplied.
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
    namespace_id: Option<NamespaceRowId>,
    #[arg(long)]
    service_id: Option<ServiceRowId>,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct RouteRemoveArgs {
    hostname: String,
    #[arg(long)]
    id: Option<RouteBindingRowId>,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct OpsWatchArgs {
    operation_id: OperationRowId,
    #[arg(long)]
    target: Option<SshTarget>,
}

#[derive(Debug, Args)]
struct LogsArgs {
    service: String,
    /// Select one exact service row when this name is ambiguous.
    #[arg(long = "id")]
    service_id: Option<ServiceRowId>,
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
    /// Match one exact machine identity when the name is ambiguous.
    #[arg(long = "id")]
    machine_id: Option<MachineRowId>,
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
    token_id: ployz_core::ids::TokenId,
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
    #[arg(long, hide = true, conflicts_with_all = ["target", "cloud_token"], requires_all = ["driver_peer_name", "driver_peer_public_key"])]
    driver_peer_id: Option<PeerId>,
    #[arg(long, hide = true, conflicts_with_all = ["target", "cloud_token"], requires_all = ["driver_peer_id", "driver_peer_public_key"])]
    driver_peer_name: Option<String>,
    #[arg(long, hide = true, conflicts_with_all = ["target", "cloud_token"], requires_all = ["driver_peer_id", "driver_peer_name"], value_parser = parse_wireguard_public_key)]
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
            self.driver_peer_name,
            self.driver_peer_public_key,
        ) {
            (Some(target), None, None, None, None) => InitDriver::SshTarget(target),
            (None, Some(token), None, None, None) => InitDriver::Cloud(token),
            (None, None, Some(id), Some(name), Some(public_key)) => {
                InitDriver::SshPeer(DriverPeerArgs {
                    id,
                    name,
                    public_key,
                })
            }
            (None, None, None, None, None) => InitDriver::OnHost,
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

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        parse_command(args.iter().map(ToString::to_string))
    }

    #[test]
    fn parses_local_telemetry_preferences() {
        for (verb, expected) in [
            ("enable", TelemetryCommand::Enable),
            ("disable", TelemetryCommand::Disable),
        ] {
            assert_eq!(
                parse(&["telemetry", verb]).expect("telemetry preference parses"),
                Command::Telemetry(expected)
            );
        }
    }

    #[test]
    fn parses_each_named_remove_with_optional_exact_row_id() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            parse(&["peer", "rm", "operator", "--id", id]).expect("peer remove parses"),
            Command::Peer(PeerCommand::Remove(PeerRemoveCommand {
                name: "operator".to_owned(),
                peer_id: Some(PeerId::try_new(id).expect("peer id")),
                target: None,
            }))
        );
        assert_eq!(
            parse(&["namespace", "rm", "prod", "--id", id]).expect("namespace remove parses"),
            Command::Namespace(NamespaceCommand::Remove(NamespaceRemoveCommand {
                namespace: CorrosionNamespaceName::try_new("prod").expect("namespace name"),
                namespace_id: Some(NamespaceRowId::try_new(id).expect("namespace id")),
                target: None,
            }))
        );
        assert_eq!(
            parse(&["service", "rm", "web", "--namespace-id", id, "--id", id,])
                .expect("service remove parses"),
            Command::Service(ServiceCommand::Remove(ServiceRemoveCommand {
                namespace_id: NamespaceRowId::try_new(id).expect("namespace id"),
                name: "web".to_owned(),
                service_id: Some(ServiceRowId::try_new(id).expect("service id")),
                target: None,
            }))
        );
        assert_eq!(
            parse(&["route", "rm", "WEB.EXAMPLE.COM", "--id", id]).expect("route remove parses"),
            Command::Route(RouteCommand::Remove(RouteRemoveCommand {
                hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
                route_id: Some(RouteBindingRowId::try_new(id).expect("route id")),
                target: None,
            }))
        );
        assert!(matches!(
            parse(&["service", "rm", "web", "--namespace-id", id])
                .expect("unqualified remove parses"),
            Command::Service(ServiceCommand::Remove(ServiceRemoveCommand {
                namespace_id,
                service_id: None,
                ..
            })) if namespace_id.as_str() == id
        ));
        assert!(parse(&["service", "rm", "web"]).is_err());
    }

    #[test]
    fn parses_route_attach_with_named_and_optional_exact_selectors() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        assert_eq!(
            parse(&[
                "route",
                "attach",
                "WEB.EXAMPLE.COM",
                "--namespace",
                "production",
                "--service",
                "web",
                "--port",
                "8080",
                "--namespace-id",
                id,
                "--service-id",
                id,
            ])
            .expect("route attach parses"),
            Command::Route(RouteCommand::Attach(RouteAttachCommand {
                hostname: RouteHostname::try_new("web.example.com").expect("hostname"),
                namespace: CorrosionNamespaceName::try_new("production").expect("namespace"),
                namespace_id: Some(NamespaceRowId::try_new(id).expect("namespace id")),
                service: CorrosionServiceName::try_new("web").expect("service"),
                service_id: Some(ServiceRowId::try_new(id).expect("service id")),
                endpoint_port: RoutePort::try_new(8080).expect("port"),
                target: None,
            }))
        );
        assert!(
            parse(&[
                "route",
                "attach",
                "web.example.com",
                "--namespace",
                "production",
                "--service",
                "web",
                "--port",
                "8080",
                "--ingress",
                "cloudflare-tunnel",
            ])
            .is_err(),
            "unshipped ingress modes are not a CLI surface"
        );
    }

    #[test]
    fn machine_list_parses_with_optional_explicit_target() {
        assert_eq!(
            parse(&["machine", "ls"]).expect("machine list parses"),
            Command::Machine(MachineCommand::List(MachineListCommand { target: None }))
        );
        assert_eq!(
            parse(&["machine", "ls", "--target", "root@cluster.example"])
                .expect("targeted machine list parses"),
            Command::Machine(MachineCommand::List(MachineListCommand {
                target: Some("root@cluster.example".parse().expect("SSH target")),
            }))
        );
        assert!(parse(&["machine", "ls", "root@cluster.example"]).is_err());
    }

    #[test]
    fn machine_remove_parses_name_and_optional_identity() {
        let machine = MachineName::try_new("edge-b").expect("machine name");
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        assert_eq!(
            parse(&["machine", "rm", "edge-b"]).expect("machine removal parses"),
            Command::Machine(MachineCommand::Remove(MachineRemoveCommand {
                machine: machine.clone(),
                machine_id: None,
                target: None,
            }))
        );
        assert_eq!(
            parse(&[
                "machine",
                "rm",
                "edge-b",
                "--id",
                machine_id.as_str(),
                "--target",
                "root@cluster.example",
            ])
            .expect("identity-qualified machine removal parses"),
            Command::Machine(MachineCommand::Remove(MachineRemoveCommand {
                machine,
                machine_id: Some(machine_id),
                target: Some("root@cluster.example".parse().expect("SSH target")),
            }))
        );
        assert!(parse(&["machine", "rm"]).is_err());
        assert!(parse(&["machine", "rm", "edge-b", "--id", "not-a-machine-id"]).is_err());
    }

    #[test]
    fn machine_upgrade_requires_one_selector_and_keeps_artifact_sources_distinct() {
        let edge_a = MachineName::try_new("edge-a").expect("machine name");
        let edge_b = MachineName::try_new("edge-b").expect("machine name");
        assert_eq!(
            parse(&["machine", "upgrade", "edge-a", "edge-b"]).expect("named upgrade parses"),
            Command::Machine(MachineCommand::Upgrade(MachineUpgradeCommand {
                selector: MachineUpgradeSelector::Names(vec![edge_a.clone(), edge_b]),
                source: MachineUpgradeSource::Channel,
                target: None,
            }))
        );
        assert_eq!(
            parse(&[
                "machine",
                "upgrade",
                "--all",
                "--version",
                "v0.1.0-alpha.7",
                "--target",
                "root@cluster.example",
            ])
            .expect("versioned all upgrade parses"),
            Command::Machine(MachineCommand::Upgrade(MachineUpgradeCommand {
                selector: MachineUpgradeSelector::All,
                source: MachineUpgradeSource::Version(
                    "v0.1.0-alpha.7".parse().expect("exact release"),
                ),
                target: Some("root@cluster.example".parse().expect("SSH target")),
            }))
        );
        for args in [
            vec!["machine", "upgrade"],
            vec!["machine", "upgrade", "edge-a", "--all"],
            vec!["machine", "upgrade", "edge-a", "edge-a"],
            vec!["machine", "upgrade", "--all", "--outdated"],
            vec![
                "machine",
                "upgrade",
                "--all",
                "--url",
                "https://releases.example/ployzd",
            ],
            vec![
                "machine",
                "upgrade",
                "--all",
                "--sha256",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            vec![
                "machine",
                "upgrade",
                "--all",
                "--version",
                "v0.1.0",
                "--url",
                "https://releases.example/ployzd",
                "--sha256",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            vec![
                "machine",
                "upgrade",
                "--outdated",
                "--url",
                "https://releases.example/ployzd",
                "--sha256",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?} must be refused");
        }
    }

    #[test]
    fn token_commands_parse_the_locked_surface() {
        assert_eq!(
            parse(&["token", "create"]).expect("default token create parses"),
            Command::Token(TokenCommand::Create(TokenCreateCommand {
                ttl: JoinTokenTtlSeconds::default_v1(),
                target: None,
            }))
        );
        assert_eq!(
            parse(&[
                "token",
                "create",
                "--ttl",
                "15m",
                "--target",
                "root@cluster.example",
            ])
            .expect("explicit token create parses"),
            Command::Token(TokenCommand::Create(TokenCreateCommand {
                ttl: JoinTokenTtlSeconds::try_new(15 * 60).expect("TTL"),
                target: Some("root@cluster.example".parse().expect("SSH target")),
            }))
        );
        assert_eq!(
            parse(&["token", "list", "--all"]).expect("token list parses"),
            Command::Token(TokenCommand::List(TokenListCommand {
                include_expired: true,
                target: None,
            }))
        );
        let token_id = ployz_core::ids::TokenId::generate();
        assert_eq!(
            parse(&["token", "revoke", token_id.as_str()]).expect("token revoke parses"),
            Command::Token(TokenCommand::Revoke(TokenRevokeCommand {
                token_id,
                target: None,
            }))
        );
        for invalid in [
            "0h",
            "59s",
            "31d",
            "1.5h",
            "forever",
            "18446744073709551615d",
        ] {
            assert!(parse(&["token", "create", "--ttl", invalid]).is_err());
        }
    }

    #[test]
    fn v2_operation_commands_parse_typed_names_ids_and_redact_env_values() {
        let namespace_id = NamespaceRowId::generate();
        let operation_id = OperationRowId::generate();
        let service_id = ServiceRowId::generate();
        assert!(matches!(
            parse(&["namespace", "create", "production"]).expect("namespace create"),
            Command::Namespace(NamespaceCommand::Create(_))
        ));
        assert!(matches!(
            parse(&[
                "namespace",
                "rm",
                "production",
                "--id",
                namespace_id.as_str(),
            ])
            .expect("namespace remove"),
            Command::Namespace(NamespaceCommand::Remove(NamespaceRemoveCommand {
                namespace_id: Some(id),
                ..
            })) if id == namespace_id
        ));
        let deploy = parse(&[
            "deploy",
            "production",
            "web",
            "registry.example/web:latest",
            "--env",
            "API_TOKEN=sentinel-secret",
        ])
        .expect("deploy");
        assert!(matches!(
            &deploy,
            Command::Deploy(DeployCommand {
                health_gate: HealthGatePolicy::Enforce,
                ..
            })
        ));
        assert!(!format!("{deploy:?}").contains("sentinel-secret"));
        assert!(matches!(
            parse(&[
                "deploy",
                "production",
                "web",
                "registry.example/web:latest",
                "--no-health-gate",
            ])
            .expect("deploy without health gate"),
            Command::Deploy(DeployCommand {
                health_gate: HealthGatePolicy::Skip,
                ..
            })
        ));
        assert!(matches!(
            parse(&["ops", "list"]).expect("ops list"),
            Command::Ops(OpsCommand::List(_))
        ));
        assert!(matches!(
            parse(&["ops", "watch", operation_id.as_str()]).expect("ops watch"),
            Command::Ops(OpsCommand::Watch(OpsWatchCommand { operation_id: id, .. }))
                if id == operation_id
        ));
        assert!(matches!(
            parse(&[
                "logs",
                "web",
                "--id",
                service_id.as_str(),
                "--tail",
                "250",
                "--follow",
            ])
            .expect("logs"),
            Command::Logs(LogsCommand { service_id: Some(id), follow: true, machine: None, .. })
                if id == service_id
        ));
        assert!(matches!(
            parse(&["logs", "web", "--machine", "edge-a"]).expect("logs with machine selector"),
            Command::Logs(LogsCommand { machine: Some(machine), .. })
                if machine.as_str() == "edge-a"
        ));
    }

    fn parsed_deploy(extra: &[&str]) -> DeployCommand {
        let mut args = vec!["deploy", "production", "web", "registry.example/web:latest"];
        args.extend_from_slice(extra);
        let Command::Deploy(command) = parse(&args).expect("deploy parses") else {
            panic!("expected deploy");
        };
        command
    }

    #[test]
    fn deploy_placement_flags_map_to_the_wire_request() {
        let inherit = parsed_deploy(&[]);
        assert_eq!(inherit.placement, None);
        assert_eq!(inherit.machines, None);

        let count_only = parsed_deploy(&["--replicas", "3"]);
        assert_eq!(
            count_only.placement,
            Some(RequestedPlacement::Replicas {
                replicas: ServiceReplicaCount::try_new(3).expect("replica count"),
            }),
            "a bare --replicas is the mode-preserving count change"
        );

        let replicated_mode_only = parsed_deploy(&["--mode", "replicated"]);
        assert_eq!(
            replicated_mode_only.placement,
            Some(RequestedPlacement::Replicated { replicas: None }),
            "a count-free mode change never scales the incumbent"
        );

        let replicated_with_count = parsed_deploy(&["--mode", "replicated", "--replicas", "5"]);
        assert_eq!(
            replicated_with_count.placement,
            Some(RequestedPlacement::Replicated {
                replicas: Some(ServiceReplicaCount::try_new(5).expect("replica count")),
            })
        );

        let global = parsed_deploy(&["--mode", "global"]);
        assert_eq!(
            global.placement,
            Some(RequestedPlacement::Global {
                host_ports: HostPortBindings::default(),
            })
        );

        let published =
            parsed_deploy(&["--mode", "global", "-p", "8080:80", "--publish", "53/udp"]);
        assert_eq!(
            published.placement,
            Some(RequestedPlacement::Global {
                host_ports: HostPortBindings::try_new([
                    HostPortBinding {
                        host_port: std::num::NonZeroU16::new(8080).expect("host port"),
                        container_port: std::num::NonZeroU16::new(80).expect("container port"),
                        protocol: HostPortProtocol::Tcp,
                    },
                    HostPortBinding {
                        host_port: std::num::NonZeroU16::new(53).expect("host port"),
                        container_port: std::num::NonZeroU16::new(53).expect("container port"),
                        protocol: HostPortProtocol::Udp,
                    },
                ])
                .expect("host ports"),
            })
        );

        let pinned = parsed_deploy(&["--machine", "edge-b", "--machine", "edge-a"]);
        let Some(RequestedPins::Machines { names }) = pinned.machines else {
            panic!("expected machine pins");
        };
        assert_eq!(
            names.iter().map(MachineName::as_str).collect::<Vec<_>>(),
            vec!["edge-a", "edge-b"]
        );

        let unpinned = parsed_deploy(&["--machine", "any"]);
        assert_eq!(unpinned.machines, Some(RequestedPins::Any));
    }

    #[test]
    fn deploy_placement_flag_refusals_never_build_a_request() {
        for (args, expected) in [
            (
                vec!["-p", "8080:80"],
                "published host ports are legal only on global services",
            ),
            (
                vec!["--mode", "replicated", "-p", "8080:80"],
                "route binding",
            ),
            (
                vec!["--mode", "global", "--replicas", "2"],
                "drop --replicas",
            ),
            (
                vec!["--machine", "any", "--machine", "edge-a"],
                "cannot be combined with machine names",
            ),
            (vec!["--replicas", "0"], "at least one replica"),
            (
                vec!["--mode", "global", "-p", "0:80"],
                "between 1 and 65535",
            ),
            (
                vec!["--mode", "global", "-p", "80/sctp"],
                "protocol must be tcp or udp",
            ),
            (
                vec!["--mode", "global", "-p", "80:81", "-p", "80:82"],
                "published more than once",
            ),
        ] {
            let mut full = vec!["deploy", "production", "web", "registry.example/web:latest"];
            full.extend_from_slice(&args);
            let error = parse(&full).expect_err("placement flag combination must refuse");
            assert!(
                error.to_string().contains(expected),
                "{args:?} produced {error}"
            );
        }
    }

    #[test]
    fn v2_operation_commands_reject_invalid_names_env_and_tail_bounds() {
        for args in [
            vec!["namespace", "create", "Production"],
            vec!["deploy", "production", "Web", "busybox:latest"],
            vec![
                "deploy",
                "production",
                "web",
                "busybox:latest",
                "--env",
                "MISSING",
            ],
            vec!["logs", "web", "--tail", "0"],
            vec!["logs", "web", "--tail", "1001"],
        ] {
            assert!(parse(&args).is_err(), "{args:?} must be rejected");
        }
    }

    #[test]
    fn diagnostics_commands_parse_with_the_shared_target_selector() {
        assert_eq!(
            parse(&["status"]).expect("status parses"),
            Command::Status(DiagnosticsCommand { target: None })
        );
        assert_eq!(
            parse(&["doctor", "--target", "root@cluster.example"]).expect("targeted doctor parses"),
            Command::Doctor(DiagnosticsCommand {
                target: Some("root@cluster.example".parse().expect("SSH target")),
            })
        );
        assert!(parse(&["doctor", "--fix"]).is_err());
    }

    #[test]
    fn endpoint_set_and_on_host_join_parse_without_secret_debug_leaks() {
        let endpoint = "203.0.113.7:51820".parse().expect("endpoint");
        let machine = MachineName::try_new("edge-b").expect("machine name");
        assert_eq!(
            parse(&["machine", "endpoint", "set", "edge-b", "203.0.113.7:51820",])
                .expect("endpoint set parses"),
            Command::Machine(MachineCommand::EndpointSet(MachineEndpointSetCommand {
                machine,
                endpoint,
                target: None,
            }))
        );

        let blob = JoinBlob::try_new(
            ployz_core::ids::TokenId::generate(),
            ployz_core::join::JoinTokenSecret::try_from_bytes([0x5a; 32]),
            ployz_core::join::JoinDoorCertFingerprint::try_new("11".repeat(32))
                .expect("fingerprint"),
            vec!["203.0.113.7:2021".parse().expect("door endpoint")],
        )
        .expect("join blob");
        let opaque = blob.expose().to_owned();
        let parsed = parse(&[
            "machine",
            "join",
            &opaque,
            "--storage",
            "plain",
            "--wireguard-endpoint",
            "203.0.113.8:51820",
        ])
        .expect("machine join parses");
        assert!(matches!(
            parsed,
            Command::Machine(MachineCommand::Join(MachineJoinCommand {
                storage: InitStorageChoice::Flag {
                    mode: StorageMode::Plain
                },
                ..
            }))
        ));
        assert!(!format!("{parsed:?}").contains(&opaque));
        assert!(parse(&["machine", "join", "not-a-join-blob"]).is_err());
        assert!(parse(&["machine", "endpoint", "set", "edge-b", "203.0.113.7:0",]).is_err());
        assert!(
            parse(&[
                "machine",
                "join",
                &opaque,
                "--wireguard-endpoint",
                "203.0.113.8:0",
            ])
            .is_err()
        );
    }

    #[test]
    fn machine_reset_is_on_host_only_and_has_no_force_form() {
        assert_eq!(
            parse(&["machine", "reset"]).expect("on-host reset parses"),
            Command::Machine(MachineCommand::Reset)
        );
        for arguments in [
            ["machine", "reset", "root@machine.example"].as_slice(),
            ["machine", "reset", "--target", "root@machine.example"].as_slice(),
            ["machine", "reset", "--force"].as_slice(),
        ] {
            assert!(parse(arguments).is_err(), "{arguments:?} must not parse");
        }
    }

    #[test]
    fn init_defaults_are_working_and_noninteractive() {
        let Command::Init(command) = parse(&["init", "root@203.0.113.7"]).expect("init parses")
        else {
            panic!("expected init")
        };
        assert_eq!(command.storage, InitStorageChoice::Automatic);
        assert_eq!(
            command.container_network,
            MachineEndpointSupernet::default_v1()
        );
        assert_eq!(command.service_urls, AutomaticHostnameMode::Disabled);
        assert!(matches!(command.driver, InitDriver::SshTarget(_)));
    }

    #[test]
    fn parses_every_shared_answer_as_flags() {
        let Command::Init(command) = parse(&[
            "init",
            "--storage",
            "plain",
            "--container-network",
            "172.30.0.0/16",
            "--service-urls",
            "custom:apps.example.com",
            "--cluster-name",
            "lab",
            "--machine-name",
            "ares",
            "--wireguard-endpoint",
            "203.0.113.7:51820",
        ])
        .expect("flag-complete init parses") else {
            panic!("expected init")
        };
        assert_eq!(
            command.storage,
            InitStorageChoice::Flag {
                mode: StorageMode::Plain
            }
        );
        assert_eq!(command.container_network.as_string(), "172.30.0.0/16");
        assert!(matches!(
            command.service_urls,
            AutomaticHostnameMode::Custom { suffix } if suffix.as_str() == "apps.example.com"
        ));
    }

    #[test]
    fn cloud_is_on_host_only_and_secret_is_redacted() {
        let error = parse(&["init", "root@203.0.113.7", "--cloud-token", "top-secret"])
            .expect_err("Cloud and SSH are mutually exclusive");
        assert!(!error.to_string().contains("top-secret"));

        let Command::Init(command) =
            parse(&["init", "--cloud-token", "top-secret"]).expect("Cloud init parses")
        else {
            panic!("expected init")
        };
        assert_eq!(
            format!("{:?}", command.driver),
            "Cloud(CloudToken([REDACTED]))"
        );
    }

    #[test]
    fn target_and_network_validation_are_strict() {
        assert!(parse(&["init", "ubuntu@host"]).is_err());
        assert!(parse(&["init", "root@host;reboot"]).is_err());
        assert!(parse(&["init", "--container-network", "10.0.0.0/24"]).is_err());
        assert!(parse(&["init", "--service-urls", "custom:*bad.example"]).is_err());
    }

    #[test]
    fn hidden_ssh_peer_is_typed_and_driver_combinations_do_not_parse() {
        let peer_id = PeerId::generate();
        let Command::Init(command) = parse(&[
            "init",
            "--driver-peer-id",
            peer_id.as_str(),
            "--driver-peer-name",
            "operator laptop",
            "--driver-peer-public-key",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        ])
        .expect("typed SSH peer parses") else {
            panic!("expected init")
        };
        assert!(matches!(
            command.driver,
            InitDriver::SshPeer(DriverPeerArgs { id, public_key, .. })
                if id == peer_id
                    && public_key.as_str()
                        == "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        ));

        assert!(
            parse(&[
                "init",
                "--cloud-token",
                "opaque",
                "--driver-peer-id",
                peer_id.as_str(),
                "--driver-peer-name",
                "operator laptop",
                "--driver-peer-public-key",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ])
            .is_err()
        );
    }

    #[test]
    fn init_value_parsers_and_renderers_are_canonical_round_trips() {
        for value in ["auto", "zfs", "plain"] {
            let parsed = parse_storage(value).expect("storage parses");
            assert_eq!(render_storage(parsed), value);
        }
        for value in ["disabled", "custom:apps.example.com"] {
            let parsed = parse_service_urls(value).expect("service URL mode parses");
            assert_eq!(render_service_urls(&parsed), value);
        }
        assert!(parse_service_urls("ployz").is_err());
    }

    #[test]
    fn ssh_ip_target_infers_machine_one_wireguard_endpoint() {
        for (target, expected) in [
            ("root@203.0.113.7", "203.0.113.7:51820"),
            ("root@[2001:db8::7]", "[2001:db8::7]:51820"),
        ] {
            let target: SshTarget = target.parse().expect("SSH target");
            assert_eq!(
                target
                    .inferred_wireguard_endpoint()
                    .expect("IP endpoint")
                    .to_string(),
                expected
            );
        }
        let hostname: SshTarget = "root@machine.example".parse().expect("hostname target");
        assert_eq!(hostname.inferred_wireguard_endpoint(), None);
    }

    #[test]
    fn ssh_target_json_is_the_validated_cli_wire_string() {
        let target: SshTarget = "root@machine.example".parse().expect("target");
        assert_eq!(
            serde_json::to_string(&target).expect("target JSON"),
            "\"root@machine.example\""
        );
        assert_eq!(
            serde_json::from_str::<SshTarget>("\"root@machine.example\"").expect("decoded target"),
            target
        );
        assert!(serde_json::from_str::<SshTarget>("\"machine.example\"").is_err());
    }

    #[test]
    fn bare_invocation_displays_help() {
        let error = parse(&[]).expect_err("command is required");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("init"));
    }
}
