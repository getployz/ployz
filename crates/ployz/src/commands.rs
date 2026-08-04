//! Command contracts for the local CLI and the machine-one bootstrap driver.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ployz_core::corrosion::{AutomaticHostnameMode, StorageMode};
use ployz_core::founding::InitStorageChoice;
use ployz_core::network::{DEFAULT_ENDPOINT_SUPERNET, MachineEndpointSupernet};
use ployz_core::operation::RouteHostname;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Telemetry(TelemetryCommand),
    Init(Box<InitCommand>),
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
    pub target: InitTarget,
    pub storage: InitStorageChoice,
    pub container_network: MachineEndpointSupernet,
    pub service_urls: AutomaticHostnameMode,
    pub cluster_name: Option<String>,
    pub machine_name: Option<String>,
    pub wireguard_endpoint: Option<SocketAddr>,
    pub cloud_token: Option<CloudToken>,
    pub driver_peer: Option<DriverPeerArgs>,
    pub prompt: InitPromptMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitPromptMask {
    pub storage: bool,
    pub container_network: bool,
    pub service_urls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitTarget {
    OnHost,
    Ssh(SshTarget),
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

/// Public peer material carried across the already-authenticated SSH channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverPeerArgs {
    pub id: String,
    pub name: String,
    pub public_key: String,
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
}

#[derive(Debug, Args)]
struct InitArgs {
    /// Remote root SSH target. Omit when running on machine one.
    target: Option<SshTarget>,
    /// Machine-one storage selection.
    #[arg(long, value_enum)]
    storage: Option<StorageArg>,
    /// Cluster-fixed container network. Must be an IPv4 /16.
    #[arg(long)]
    container_network: Option<String>,
    /// Automatic URLs: ployz, disabled, or custom:<suffix>.
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
    #[arg(long, hide = true, requires_all = ["driver_peer_name", "driver_peer_public_key"])]
    driver_peer_id: Option<String>,
    #[arg(long, hide = true, requires_all = ["driver_peer_id", "driver_peer_public_key"])]
    driver_peer_name: Option<String>,
    #[arg(long, hide = true, requires_all = ["driver_peer_id", "driver_peer_name"])]
    driver_peer_public_key: Option<String>,
}

impl InitArgs {
    fn into_command(self) -> Result<InitCommand, String> {
        let prompt = InitPromptMask {
            storage: self.storage.is_none(),
            container_network: self.container_network.is_none(),
            service_urls: self.service_urls.is_none(),
        };
        let target = self.target.map_or(InitTarget::OnHost, InitTarget::Ssh);
        if matches!(target, InitTarget::Ssh(_)) && self.driver_peer_id.is_some() {
            return Err("driver peer flags are accepted only by on-host init".to_owned());
        }
        let driver_peer = match (
            self.driver_peer_id,
            self.driver_peer_name,
            self.driver_peer_public_key,
        ) {
            (Some(id), Some(name), Some(public_key)) => Some(DriverPeerArgs {
                id,
                name,
                public_key,
            }),
            (None, None, None) => None,
            _ => return Err("driver peer enrollment is incomplete".to_owned()),
        };
        if self.cloud_token.is_some() && driver_peer.is_some() {
            return Err("cloud and SSH driver enrollment cannot be combined".to_owned());
        }
        Ok(InitCommand {
            target,
            storage: self.storage.unwrap_or(StorageArg::Auto).into(),
            container_network: MachineEndpointSupernet::try_new(
                self.container_network
                    .unwrap_or_else(|| DEFAULT_ENDPOINT_SUPERNET.to_owned()),
            )
            .map_err(|error| error.to_string())?,
            service_urls: self
                .service_urls
                .map_or(AutomaticHostnameMode::Ployz, |value| value.0),
            cluster_name: self.cluster_name,
            machine_name: self.machine_name,
            wireguard_endpoint: self.wireguard_endpoint,
            cloud_token: self.cloud_token,
            driver_peer,
            prompt,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StorageArg {
    Auto,
    Zfs,
    Plain,
}

impl From<StorageArg> for InitStorageChoice {
    fn from(value: StorageArg) -> Self {
        match value {
            StorageArg::Auto => Self::Automatic,
            StorageArg::Zfs => Self::Flag {
                mode: StorageMode::Zfs,
            },
            StorageArg::Plain => Self::Flag {
                mode: StorageMode::Plain,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceUrlsArg(AutomaticHostnameMode);

impl FromStr for ServiceUrlsArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mode = match value {
            "ployz" => AutomaticHostnameMode::Ployz,
            "disabled" => AutomaticHostnameMode::Disabled,
            _ => {
                let Some(suffix) = value.strip_prefix("custom:") else {
                    return Err(
                        "service URLs must be ployz, disabled, or custom:<suffix>".to_owned()
                    );
                };
                AutomaticHostnameMode::Custom {
                    suffix: RouteHostname::try_new(suffix).map_err(|error| error.to_string())?,
                }
            }
        };
        Ok(Self(mode))
    }
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
        assert_eq!(command.service_urls, AutomaticHostnameMode::Ployz);
        assert!(matches!(command.target, InitTarget::Ssh(_)));
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
            format!("{:?}", command.cloud_token),
            "Some(CloudToken([REDACTED]))"
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
    fn bare_invocation_displays_help() {
        let error = parse(&[]).expect_err("command is required");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(error.to_string().contains("init"));
    }
}
