//! Command contracts for the local CLI and the machine-one bootstrap driver.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand};
use ployz_core::corrosion::{AutomaticHostnameMode, StorageMode};
use ployz_core::founding::InitStorageChoice;
use ployz_core::ids::PeerId;
use ployz_core::network::{DEFAULT_ENDPOINT_SUPERNET, MachineEndpointSupernet, WireGuardPublicKey};
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
    #[arg(long)]
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
                .map_or(AutomaticHostnameMode::Ployz, |value| value.0),
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
        "ployz" => Ok(AutomaticHostnameMode::Ployz),
        "disabled" => Ok(AutomaticHostnameMode::Disabled),
        _ => {
            let Some(suffix) = value.strip_prefix("custom:") else {
                return Err("service URLs must be ployz, disabled, or custom:<suffix>".to_owned());
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
        AutomaticHostnameMode::Ployz => "ployz".to_owned(),
        AutomaticHostnameMode::Disabled => "disabled".to_owned(),
        AutomaticHostnameMode::Custom { suffix } => format!("custom:{}", suffix.as_str()),
    }
}

fn parse_wireguard_public_key(value: &str) -> Result<WireGuardPublicKey, String> {
    WireGuardPublicKey::try_new(value).map_err(|error| error.to_string())
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
        for value in ["ployz", "disabled", "custom:apps.example.com"] {
            let parsed = parse_service_urls(value).expect("service URL mode parses");
            assert_eq!(render_service_urls(&parsed), value);
        }
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
