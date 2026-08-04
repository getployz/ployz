//! Privileged, bounded host effects for the built-in WireGuard mesh.
//!
//! Cluster policy stays in `ployz-core`. This module accepts a complete,
//! validated desired mesh and owns only local key, interface, route, and eBPF
//! convergence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::time::Duration;

use defguard_wireguard_rs::key::Key;
use ipnet::{IpNet, Ipv6Net};
use ployz_core::corrosion::{
    DesiredBuiltinWireguardLocal, DesiredBuiltinWireguardMesh, DesiredMachineContainerRoute,
};
pub use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::network::WireGuardPublicKey;
use thiserror::Error;

use crate::{
    AssignedHostPort, FileMode, FirewallBackend, HostRunnerCommandOutput, HostRunnerCommandRunner,
    StagedFile, SupervisorBackend, SystemHostRunnerCommandRunner, detect_firewall_backend,
};

pub const DEFAULT_PRIVATE_KEY_PATH: &str = "/etc/ployz/wireguard.key";
pub const DEFAULT_WIREGUARD_IFNAME: &str = "ployz0";
pub const DEFAULT_WIREGUARD_MTU: u16 = 1_420;

const PERSISTENT_KEEPALIVE_SECONDS: u16 = 25;
const MAX_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Validated local-effect configuration. The bridge is supplied by the
/// separate bridge owner; this module reports its absence without inventing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardHostConfig {
    private_key_path: PathBuf,
    wg_ifname: String,
    listen_port: NonZeroU16,
    mtu: u16,
    ebpf: BuiltinWireguardEbpfConfig,
    supervisor: SupervisorBackend,
    command_timeout: Duration,
}

impl BuiltinWireguardHostConfig {
    pub fn try_new(
        private_key_path: PathBuf,
        wg_ifname: String,
        listen_port: u16,
        mtu: u16,
        ebpf: BuiltinWireguardEbpfConfig,
        supervisor: SupervisorBackend,
        command_timeout: Duration,
    ) -> Result<Self, BuiltinWireguardConfigError> {
        validate_path("private key", &private_key_path)?;
        validate_ifname("WireGuard", &wg_ifname)?;
        let Some(listen_port) = NonZeroU16::new(listen_port) else {
            return Err(BuiltinWireguardConfigError::ZeroListenPort);
        };
        if !(1_280..=9_000).contains(&mtu) {
            return Err(BuiltinWireguardConfigError::InvalidMtu { mtu });
        }
        if command_timeout.is_zero() {
            return Err(BuiltinWireguardConfigError::ZeroCommandTimeout);
        }
        if command_timeout > MAX_HOST_COMMAND_TIMEOUT {
            return Err(BuiltinWireguardConfigError::CommandTimeoutTooLong {
                milliseconds: command_timeout.as_millis(),
            });
        }
        Ok(Self {
            private_key_path,
            wg_ifname,
            listen_port,
            mtu,
            ebpf,
            supervisor,
            command_timeout,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardEbpfConfig {
    bridge_ifname: String,
    ctl_path: PathBuf,
    bytecode_path: PathBuf,
    pinning: EbpfPinning,
}

impl BuiltinWireguardEbpfConfig {
    pub fn try_new(
        bridge_ifname: String,
        ctl_path: PathBuf,
        bytecode_path: PathBuf,
        pinning: EbpfPinning,
    ) -> Result<Self, BuiltinWireguardConfigError> {
        validate_ifname("bridge", &bridge_ifname)?;
        validate_path("eBPF control program", &ctl_path)?;
        validate_path("eBPF bytecode", &bytecode_path)?;
        if let EbpfPinning::Explicit(path) = &pinning {
            validate_path("eBPF pin", path)?;
        }
        Ok(Self {
            bridge_ifname,
            ctl_path,
            bytecode_path,
            pinning,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EbpfPinning {
    Default,
    Explicit(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinWireguardConfigError {
    #[error("{field} path must not be empty")]
    EmptyPath { field: &'static str },
    #[error("{field} interface name is invalid: {value:?}")]
    InvalidInterfaceName { field: &'static str, value: String },
    #[error("WireGuard listen port must not be zero")]
    ZeroListenPort,
    #[error("WireGuard MTU must be between 1280 and 9000, got {mtu}")]
    InvalidMtu { mtu: u16 },
    #[error("host command timeout must not be zero")]
    ZeroCommandTimeout,
    #[error("host command timeout must not exceed 60 seconds, got {milliseconds}ms")]
    CommandTimeoutTooLong { milliseconds: u128 },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinWireguardHostError {
    #[error("built-in WireGuard host effect failed: {message}")]
    HostEffect { message: String },
    #[error("built-in WireGuard UDP port is blocked by unmanaged firewall {backend:?}")]
    UnmanagedFirewall { backend: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardHostOutcome {
    pub wireguard: WireguardHostReady,
    pub ebpf: EbpfHostOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireguardHostReady {
    pub public_key: WireGuardPublicKey,
    pub bind_address: Ipv6Net,
    pub peer_count: usize,
    pub route_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireguardLocalBinding {
    pub public_key: WireGuardPublicKey,
    pub bind_address: Ipv6Net,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EbpfHostOutcome {
    Ready { route_count: usize },
    Degraded { reason: EbpfDegradedReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EbpfDegradedReason {
    MissingBridge { ifname: String },
    HostEffect { message: String },
}

/// The local-effect module. `R` is the existing Host Runner OS seam; all
/// production subprocesses use the bounded system implementation.
#[derive(Debug)]
pub struct BuiltinWireguardHost<R = SystemHostRunnerCommandRunner> {
    config: BuiltinWireguardHostConfig,
    runner: R,
}

impl BuiltinWireguardHost<SystemHostRunnerCommandRunner> {
    #[must_use]
    pub fn new(config: BuiltinWireguardHostConfig) -> Self {
        let runner = SystemHostRunnerCommandRunner::new(config.command_timeout);
        Self { config, runner }
    }
}

impl<R: HostRunnerCommandRunner> BuiltinWireguardHost<R> {
    #[must_use]
    pub fn with_runner(config: BuiltinWireguardHostConfig, runner: R) -> Self {
        Self { config, runner }
    }

    /// Creates the private key once with private permissions, or reads the
    /// existing identity, and returns its canonical public key.
    pub fn provision_and_read_public_key(
        &mut self,
    ) -> Result<WireGuardPublicKey, BuiltinWireguardHostError> {
        let private_key = provision_private_key(&self.config.private_key_path)?;
        let key = Key::try_from(private_key.as_str()).map_err(|error| {
            host_error(format!(
                "parse WireGuard private key {}: {error}",
                self.config.private_key_path.display()
            ))
        })?;
        WireGuardPublicKey::try_new(key.public_key().to_string())
            .map_err(|error| host_error(format!("derive WireGuard public key: {error}")))
    }

    /// Provisions and binds the local `/112` needed by Corrosion bootstrap.
    /// This operation deliberately does not inspect or mutate WireGuard peers.
    pub fn bind_local(
        &mut self,
        local: &DesiredBuiltinWireguardLocal,
    ) -> Result<WireguardLocalBinding, BuiltinWireguardHostError> {
        let public_key = self.provision_and_read_public_key()?;
        if public_key != local.public_key {
            return Err(host_error(format!(
                "local WireGuard public key does not match desired identity: host={} desired={}",
                public_key.as_str(),
                local.public_key.as_str()
            )));
        }
        self.ensure_ipv6_sysctls_before_interface()
            .map_err(host_error)?;
        let ifname = self.config.wg_ifname.clone();
        if !self
            .run("ip", &["link", "show", "dev", &ifname])
            .map_err(host_error)?
            .success
        {
            self.require("ip", &["link", "add", "dev", &ifname, "type", "wireguard"])
                .map_err(host_error)?;
        }
        self.ensure_interface_sysctls().map_err(host_error)?;

        let ifname = self.config.wg_ifname.clone();
        let private_key_path = self.config.private_key_path.display().to_string();
        let listen_port = self.config.listen_port.get().to_string();
        self.require(
            "wg",
            &[
                "set",
                &ifname,
                "private-key",
                &private_key_path,
                "listen-port",
                &listen_port,
            ],
        )
        .map_err(host_error)?;
        let bind_address = Ipv6Net::new(local.bind_address.get(), 112)
            .expect("Core desired local address is a valid /112 host address");
        let bind_cidr = bind_address.to_string();
        self.require(
            "ip",
            &["-6", "address", "replace", &bind_cidr, "dev", &ifname],
        )
        .map_err(host_error)?;
        self.remove_stale_local_addresses(bind_address)
            .map_err(host_error)?;
        let mtu = self.config.mtu.to_string();
        self.require("ip", &["link", "set", "dev", &ifname, "mtu", &mtu])
            .map_err(host_error)?;
        self.require("ip", &["link", "set", "dev", &ifname, "up"])
            .map_err(host_error)?;
        self.ensure_wireguard_firewall()
            .map_err(|error| match error {
                FirewallEffectError::Host(message) => host_error(message),
                FirewallEffectError::Unmanaged(backend) => {
                    BuiltinWireguardHostError::UnmanagedFirewall { backend }
                }
            })?;
        self.verify_local_binding(&public_key, bind_address)
            .map_err(host_error)?;
        Ok(WireguardLocalBinding {
            public_key,
            bind_address,
        })
    }

    /// Converges one nonempty, locally-authorized Core projection.
    pub fn converge(
        &mut self,
        desired: &DesiredBuiltinWireguardMesh,
    ) -> Result<BuiltinWireguardHostOutcome, BuiltinWireguardHostError> {
        let binding = self.bind_local(&desired.local)?;
        let peers = render_desired_peers(desired);
        let observed_peers = self.read_observed_peers().map_err(host_error)?;
        let observed_routes = self.read_observed_routes().map_err(host_error)?;
        let actions = convergence_plan(&observed_peers, &observed_routes, &peers);
        self.apply_convergence_actions(&actions)
            .map_err(host_error)?;
        let wireguard = WireguardHostReady {
            public_key: binding.public_key,
            bind_address: binding.bind_address,
            peer_count: peers.len(),
            route_count: peers.iter().flat_map(|peer| &peer.allowed_ips).count(),
        };
        let ebpf = self.converge_ebpf(&desired.ebpf_routes);
        Ok(BuiltinWireguardHostOutcome { wireguard, ebpf })
    }

    /// Explicitly removes every peer and owned route while retaining the local
    /// identity/address. Keeper calls this only for a nonempty roster that no
    /// longer contains the local machine.
    pub fn fence(
        &mut self,
        local: &DesiredBuiltinWireguardLocal,
    ) -> Result<BuiltinWireguardHostOutcome, BuiltinWireguardHostError> {
        let binding = self.bind_local(local)?;
        let observed_peers = self.read_observed_peers().map_err(host_error)?;
        let observed_routes = self.read_observed_routes().map_err(host_error)?;
        let actions = convergence_plan(&observed_peers, &observed_routes, &[]);
        self.apply_convergence_actions(&actions)
            .map_err(host_error)?;
        let wireguard = WireguardHostReady {
            public_key: binding.public_key,
            bind_address: binding.bind_address,
            peer_count: 0,
            route_count: 0,
        };
        let ebpf = self.converge_ebpf(&[]);
        Ok(BuiltinWireguardHostOutcome { wireguard, ebpf })
    }

    fn ensure_ipv6_sysctls_before_interface(&mut self) -> Result<(), String> {
        for (name, value) in [
            ("net.ipv6.conf.all.disable_ipv6", "0"),
            ("net.ipv6.conf.default.disable_ipv6", "0"),
            ("net.ipv6.conf.all.forwarding", "1"),
            ("net.ipv6.conf.default.forwarding", "1"),
            ("net.ipv4.ip_forward", "1"),
            ("net.ipv4.conf.all.rp_filter", "0"),
            ("net.ipv4.conf.default.rp_filter", "0"),
        ] {
            self.set_and_verify_sysctl(name, value)?;
        }
        Ok(())
    }

    fn ensure_interface_sysctls(&mut self) -> Result<(), String> {
        let disable_ipv6 = format!("net.ipv6.conf.{}.disable_ipv6", self.config.wg_ifname);
        let rp_filter = format!("net.ipv4.conf.{}.rp_filter", self.config.wg_ifname);
        self.set_and_verify_sysctl(&disable_ipv6, "0")?;
        self.set_and_verify_sysctl(&rp_filter, "0")
    }

    fn ensure_wireguard_firewall(&mut self) -> Result<(), FirewallEffectError> {
        let backend = detect_firewall_backend(self.config.supervisor, &mut self.runner)
            .map_err(|error| FirewallEffectError::Host(error.as_str().to_owned()))?;
        if let FirewallBackend::Unmanaged(name) = backend {
            return Err(FirewallEffectError::Unmanaged(name));
        }
        backend
            .open_with(
                AssignedHostPort::udp(self.config.listen_port.get()),
                &mut self.runner,
            )
            .map_err(|error| FirewallEffectError::Host(error.as_str().to_owned()))
    }

    fn set_and_verify_sysctl(&mut self, name: &str, value: &str) -> Result<(), String> {
        let assignment = format!("{name}={value}");
        self.require("sysctl", &["-w", &assignment])?;
        let observed = self.require("sysctl", &["-n", name])?;
        if observed.trim() == value {
            Ok(())
        } else {
            Err(format!(
                "sysctl verification failed for {name}: wanted {value}, observed {:?}",
                observed.trim()
            ))
        }
    }

    fn verify_local_binding(
        &mut self,
        public_key: &WireGuardPublicKey,
        bind_address: Ipv6Net,
    ) -> Result<(), String> {
        let ifname = self.config.wg_ifname.clone();
        let observed_key = self.require("wg", &["show", &ifname, "public-key"])?;
        if observed_key.trim() != public_key.as_str() {
            return Err(format!(
                "WireGuard public-key verification failed: wanted {}, observed {:?}",
                public_key.as_str(),
                observed_key.trim()
            ));
        }
        let addresses = self.require(
            "ip",
            &[
                "-o", "-6", "address", "show", "dev", &ifname, "scope", "global",
            ],
        )?;
        let wanted = bind_address.to_string();
        if addresses
            .split_ascii_whitespace()
            .any(|field| field == wanted)
        {
            Ok(())
        } else {
            Err(format!(
                "WireGuard IPv6 bind verification failed: wanted {wanted}, observed {:?}",
                addresses.trim()
            ))
        }
    }

    fn remove_stale_local_addresses(&mut self, desired: Ipv6Net) -> Result<(), String> {
        let ifname = self.config.wg_ifname.clone();
        let output = self.run(
            "ip",
            &[
                "-o", "-6", "address", "show", "dev", &ifname, "scope", "global",
            ],
        )?;
        if !output.success {
            return Err(output.failure);
        }
        if output.stdout_truncated {
            return Err("WireGuard IPv6 address observation was truncated".to_owned());
        }
        for stale in parse_interface_ipv6_addresses(&output.stdout)?
            .into_iter()
            .filter(|address| *address != desired)
        {
            let stale = stale.to_string();
            self.require("ip", &["-6", "address", "del", &stale, "dev", &ifname])?;
        }
        Ok(())
    }

    fn read_observed_peers(
        &mut self,
    ) -> Result<BTreeMap<WireGuardPublicKey, ObservedPeer>, String> {
        let ifname = self.config.wg_ifname.clone();
        let output = self.run("wg", &["show", &ifname, "dump"])?;
        if !output.success {
            return Err(output.failure);
        }
        if output.stdout_truncated {
            return Err("WireGuard peer observation was truncated".to_owned());
        }
        parse_wireguard_dump(&output.stdout)
    }

    fn read_observed_routes(&mut self) -> Result<BTreeSet<IpNet>, String> {
        let ifname = self.config.wg_ifname.clone();
        let mut routes = BTreeSet::new();
        for family in ["-4", "-6"] {
            let args = owned_route_observation_args(family, &ifname);
            let output = self.run("ip", &args)?;
            if !output.success {
                return Err(output.failure);
            }
            if output.stdout_truncated {
                return Err(format!(
                    "WireGuard {family} route observation was truncated"
                ));
            }
            routes.extend(parse_owned_routes(&output.stdout)?);
        }
        Ok(routes)
    }

    fn apply_convergence_actions(&mut self, actions: &[ConvergenceAction]) -> Result<(), String> {
        for action in actions {
            match action {
                ConvergenceAction::UpsertPeer(peer) => self.upsert_peer(peer)?,
                ConvergenceAction::ReplaceRoute(route) => self.replace_route(*route)?,
                ConvergenceAction::RemovePeer(public_key) => {
                    let ifname = self.config.wg_ifname.clone();
                    self.require(
                        "wg",
                        &["set", &ifname, "peer", public_key.as_str(), "remove"],
                    )?;
                }
                ConvergenceAction::RemoveRoute(route) => self.remove_route(*route)?,
            }
        }
        Ok(())
    }

    fn upsert_peer(&mut self, peer: &DesiredPeer) -> Result<(), String> {
        let ifname = self.config.wg_ifname.clone();
        let args = wireguard_peer_args(&ifname, peer);
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        self.require("wg", &refs).map(|_| ())
    }

    fn replace_route(&mut self, route: IpNet) -> Result<(), String> {
        let family = route_family(route);
        let route = route.to_string();
        let ifname = self.config.wg_ifname.clone();
        self.require(
            "ip",
            &[
                family, "route", "replace", &route, "dev", &ifname, "proto", "boot", "scope",
                "link",
            ],
        )
        .map(|_| ())
    }

    fn remove_route(&mut self, route: IpNet) -> Result<(), String> {
        let family = route_family(route);
        let route = route.to_string();
        let ifname = self.config.wg_ifname.clone();
        let output = self.run(
            "ip",
            &[
                family, "route", "del", &route, "dev", &ifname, "proto", "boot", "scope", "link",
            ],
        )?;
        // Route observation and deletion race harmlessly with another local
        // convergence pass. Any other failure remains retry-visible.
        if output.success || output.exit_code == Some(2) {
            Ok(())
        } else {
            Err(output.failure)
        }
    }

    fn converge_ebpf(
        &mut self,
        routes: &[ployz_core::corrosion::DesiredBuiltinWireguardEbpfRoute],
    ) -> EbpfHostOutcome {
        let result = self.try_converge_ebpf(routes);
        ebpf_outcome(routes.len(), &self.config.ebpf.bridge_ifname, result)
    }

    fn try_converge_ebpf(
        &mut self,
        routes: &[ployz_core::corrosion::DesiredBuiltinWireguardEbpfRoute],
    ) -> Result<(), EbpfEffectError> {
        let bridge = self.config.ebpf.bridge_ifname.clone();
        let bridge_output = self
            .run("ip", &["link", "show", "dev", &bridge])
            .map_err(EbpfEffectError::HostEffect)?;
        if !bridge_output.success {
            return Err(EbpfEffectError::MissingBridge);
        }
        for (description, path) in [
            ("eBPF control program", &self.config.ebpf.ctl_path),
            ("eBPF bytecode", &self.config.ebpf.bytecode_path),
        ] {
            if !path.is_file() {
                return Err(EbpfEffectError::HostEffect(format!(
                    "{description} is missing: {}",
                    path.display()
                )));
            }
        }
        fs::create_dir_all("/sys/fs/bpf").map_err(|error| {
            EbpfEffectError::HostEffect(format!("create bpffs mountpoint: {error}"))
        })?;
        let mountpoint = self
            .run("mountpoint", &["-q", "/sys/fs/bpf"])
            .map_err(EbpfEffectError::HostEffect)?;
        if !mountpoint.success {
            self.require("mount", &["-t", "bpf", "bpf", "/sys/fs/bpf"])
                .map_err(EbpfEffectError::HostEffect)?;
        }
        let ctl = self.config.ebpf.ctl_path.display().to_string();
        let bytecode = self.config.ebpf.bytecode_path.display().to_string();
        self.require(&ctl, &["validate", &bytecode])
            .map_err(EbpfEffectError::HostEffect)?;
        let ifname = self.config.wg_ifname.clone();
        let attach_args = self.ebpf_args([
            "ensure-attached".to_owned(),
            bytecode,
            bridge.clone(),
            ifname.clone(),
        ]);
        let attach_refs = attach_args.iter().map(String::as_str).collect::<Vec<_>>();
        self.require(&ctl, &attach_refs)
            .map_err(EbpfEffectError::HostEffect)?;
        self.ensure_forwarding_firewall(&bridge, &ifname)
            .map_err(EbpfEffectError::HostEffect)?;
        let mut route_args = vec!["route".to_owned(), "replace-all-ifname".to_owned(), ifname];
        route_args.extend(routes.iter().map(|route| route.subnet_v4.as_string()));
        let route_args = self.ebpf_args(route_args);
        let route_refs = route_args.iter().map(String::as_str).collect::<Vec<_>>();
        self.require(&ctl, &route_refs)
            .map_err(EbpfEffectError::HostEffect)?;
        Ok(())
    }

    fn ebpf_args(&self, args: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut rendered = Vec::new();
        if let EbpfPinning::Explicit(pin_path) = &self.config.ebpf.pinning {
            rendered.extend(["--pin-path".to_owned(), pin_path.display().to_string()]);
        }
        rendered.extend(args);
        rendered
    }

    fn ensure_forwarding_firewall(&mut self, bridge: &str, wg: &str) -> Result<(), String> {
        for (input, output) in [(wg, bridge), (bridge, wg)] {
            let check = ["-C", "FORWARD", "-i", input, "-o", output, "-j", "ACCEPT"];
            if !self.run("iptables", &check)?.success {
                self.require(
                    "iptables",
                    &[
                        "-I", "FORWARD", "1", "-i", input, "-o", output, "-j", "ACCEPT",
                    ],
                )?;
            }
        }
        Ok(())
    }

    fn run(&mut self, program: &str, args: &[&str]) -> Result<HostRunnerCommandOutput, String> {
        self.runner
            .command_with_timeout(program, args, self.config.command_timeout)
            .map_err(|error| error.as_str().to_owned())
    }

    fn require(&mut self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = self.run(program, args)?;
        if output.success {
            Ok(output.stdout)
        } else {
            Err(output.failure)
        }
    }
}

#[derive(Debug)]
enum EbpfEffectError {
    MissingBridge,
    HostEffect(String),
}

#[derive(Debug)]
enum FirewallEffectError {
    Host(String),
    Unmanaged(String),
}

fn ebpf_outcome(
    route_count: usize,
    bridge_ifname: &str,
    result: Result<(), EbpfEffectError>,
) -> EbpfHostOutcome {
    match result {
        Ok(()) => EbpfHostOutcome::Ready { route_count },
        Err(EbpfEffectError::MissingBridge) => EbpfHostOutcome::Degraded {
            reason: EbpfDegradedReason::MissingBridge {
                ifname: bridge_ifname.to_owned(),
            },
        },
        Err(EbpfEffectError::HostEffect(message)) => EbpfHostOutcome::Degraded {
            reason: EbpfDegradedReason::HostEffect { message },
        },
    }
}

fn render_desired_peers(desired: &DesiredBuiltinWireguardMesh) -> Vec<DesiredPeer> {
    let machines = desired.machine_peers.iter().map(|peer| {
        let mut allowed_ips = BTreeSet::from([IpNet::V6(peer.subnet_v6.network())]);
        if let DesiredMachineContainerRoute::Claimed { subnet } = &peer.container_route {
            allowed_ips.insert(subnet.ipnet());
        }
        DesiredPeer {
            public_key: peer.public_key.clone(),
            endpoint: peer.endpoint,
            allowed_ips,
        }
    });
    let roaming = desired.roaming_peers.iter().map(|peer| DesiredPeer {
        public_key: peer.public_key.clone(),
        endpoint: peer.endpoint,
        allowed_ips: BTreeSet::from([IpNet::V6(peer.subnet_v6.network())]),
    });
    machines.chain(roaming).collect()
}

fn render_allowed_ips(allowed_ips: &BTreeSet<IpNet>) -> String {
    allowed_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn wireguard_peer_args(ifname: &str, peer: &DesiredPeer) -> Vec<String> {
    let mut args = vec![
        "set".to_owned(),
        ifname.to_owned(),
        "peer".to_owned(),
        peer.public_key.as_str().to_owned(),
        "allowed-ips".to_owned(),
        render_allowed_ips(&peer.allowed_ips),
    ];
    if let Some(endpoint) = peer.endpoint {
        args.extend([
            "endpoint".to_owned(),
            endpoint.to_string(),
            "persistent-keepalive".to_owned(),
            PERSISTENT_KEEPALIVE_SECONDS.to_string(),
        ]);
    }
    args
}

fn route_family(route: IpNet) -> &'static str {
    match route {
        IpNet::V4(_) => "-4",
        IpNet::V6(_) => "-6",
    }
}

fn owned_route_observation_args<'a>(family: &'a str, ifname: &'a str) -> [&'a str; 8] {
    [
        "-o", family, "route", "show", "dev", ifname, "proto", "boot",
    ]
}

fn parse_wireguard_dump(dump: &str) -> Result<BTreeMap<WireGuardPublicKey, ObservedPeer>, String> {
    let mut lines = dump.lines();
    let Some(_interface) = lines.next() else {
        return Err("WireGuard dump had no interface row".to_owned());
    };
    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let [
                public_key,
                _preshared_key,
                endpoint,
                allowed_ips,
                _handshake,
                _rx,
                _tx,
                keepalive,
            ] = fields.as_slice()
            else {
                return Err(format!(
                    "WireGuard dump peer row {} had {} fields, expected 8",
                    index + 2,
                    fields.len()
                ));
            };
            let public_key = WireGuardPublicKey::try_new(*public_key)
                .map_err(|error| format!("parse observed WireGuard public key: {error}"))?;
            let endpoint = match *endpoint {
                "(none)" => None,
                value => Some(value.parse::<SocketAddr>().map_err(|error| {
                    format!("parse observed WireGuard endpoint {value:?}: {error}")
                })?),
            };
            let allowed_ips = match *allowed_ips {
                "(none)" => BTreeSet::new(),
                value => value
                    .split(',')
                    .map(|route| {
                        route.parse::<IpNet>().map_err(|error| {
                            format!("parse observed WireGuard allowed IP {route:?}: {error}")
                        })
                    })
                    .collect::<Result<_, _>>()?,
            };
            let persistent_keepalive = match *keepalive {
                "off" | "0" => None,
                value => Some(value.parse::<u16>().map_err(|error| {
                    format!("parse observed WireGuard keepalive {value:?}: {error}")
                })?),
            };
            Ok((
                public_key,
                ObservedPeer {
                    endpoint,
                    allowed_ips,
                    persistent_keepalive,
                },
            ))
        })
        .collect()
}

fn parse_owned_routes(output: &str) -> Result<BTreeSet<IpNet>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let route = line
                .split_ascii_whitespace()
                .next()
                .expect("nonempty route row has a first field");
            route
                .parse::<IpNet>()
                .map_err(|error| format!("parse owned WireGuard route {route:?}: {error}"))
        })
        .collect()
}

fn parse_interface_ipv6_addresses(output: &str) -> Result<BTreeSet<Ipv6Net>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            let Some(address) = fields.windows(2).find_map(|pair| match pair {
                ["inet6", address] => Some(*address),
                [_, _] | [] | [_] | [_, _, ..] => None,
            }) else {
                return Err(format!(
                    "WireGuard IPv6 address row had no inet6 field: {line:?}"
                ));
            };
            address.parse::<Ipv6Net>().map_err(|error| {
                format!("parse WireGuard IPv6 interface address {address:?}: {error}")
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesiredPeer {
    public_key: WireGuardPublicKey,
    endpoint: Option<SocketAddr>,
    allowed_ips: BTreeSet<IpNet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedPeer {
    endpoint: Option<SocketAddr>,
    allowed_ips: BTreeSet<IpNet>,
    persistent_keepalive: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConvergenceAction {
    UpsertPeer(DesiredPeer),
    ReplaceRoute(IpNet),
    RemovePeer(WireGuardPublicKey),
    RemoveRoute(IpNet),
}

fn convergence_plan(
    observed_peers: &BTreeMap<WireGuardPublicKey, ObservedPeer>,
    observed_routes: &BTreeSet<IpNet>,
    desired_peers: &[DesiredPeer],
) -> Vec<ConvergenceAction> {
    let desired_by_key = desired_peers
        .iter()
        .map(|peer| (peer.public_key.clone(), peer))
        .collect::<BTreeMap<_, _>>();
    let desired_routes = desired_peers
        .iter()
        .flat_map(|peer| peer.allowed_ips.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut actions = Vec::new();
    actions.extend(desired_peers.iter().filter_map(|desired| {
        let matches = observed_peers
            .get(&desired.public_key)
            .is_some_and(|observed| peer_matches(observed, desired));
        (!matches).then(|| ConvergenceAction::UpsertPeer(desired.clone()))
    }));
    actions.extend(
        desired_routes
            .difference(observed_routes)
            .copied()
            .map(ConvergenceAction::ReplaceRoute),
    );
    actions.extend(
        observed_peers
            .keys()
            .filter(|key| !desired_by_key.contains_key(*key))
            .cloned()
            .map(ConvergenceAction::RemovePeer),
    );
    actions.extend(
        observed_routes
            .difference(&desired_routes)
            .copied()
            .map(ConvergenceAction::RemoveRoute),
    );
    actions
}

fn peer_matches(observed: &ObservedPeer, desired: &DesiredPeer) -> bool {
    let endpoint_matches = match desired.endpoint {
        Some(endpoint) => {
            observed.endpoint == Some(endpoint)
                && observed.persistent_keepalive == Some(PERSISTENT_KEEPALIVE_SECONDS)
        }
        None => true,
    };
    endpoint_matches && observed.allowed_ips == desired.allowed_ips
}

fn provision_private_key(path: &Path) -> Result<String, BuiltinWireguardHostError> {
    match fs::read_to_string(path) {
        Ok(value) => return validate_private_key(path, value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(host_error(format!(
                "read WireGuard private key {}: {error}",
                path.display()
            )));
        }
    }
    let Some(directory) = path.parent() else {
        return Err(host_error(format!(
            "WireGuard private key path has no parent: {}",
            path.display()
        )));
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|error| {
        host_error(format!(
            "create WireGuard key directory {}: {error}",
            directory.display()
        ))
    })?;
    let private_key = Key::generate().to_string();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| host_error(format!("invalid private key file name: {}", path.display())))?;
    let mut staged = StagedFile::create(directory, file_name, "key", FileMode::Secret0600)
        .map_err(|error| host_error(format!("stage WireGuard private key: {error:?}")))?;
    staged
        .file()
        .write_all(format!("{private_key}\n").as_bytes())
        .and_then(|()| staged.file().sync_all())
        .map_err(|error| host_error(format!("write WireGuard private key: {error}")))?;
    match commit_new_key(&mut staged, path) {
        Ok(()) => Ok(private_key),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => fs::read_to_string(path)
            .map_err(|read_error| {
                host_error(format!(
                    "read concurrently-created WireGuard private key {}: {read_error}",
                    path.display()
                ))
            })
            .and_then(|value| validate_private_key(path, value)),
        Err(error) => Err(host_error(format!(
            "commit WireGuard private key {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(target_os = "linux")]
fn commit_new_key(staged: &mut StagedFile, path: &Path) -> std::io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, staged.path(), CWD, path, RenameFlags::NOREPLACE)
        .map_err(std::io::Error::from)?;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(target_os = "linux"))]
fn commit_new_key(staged: &mut StagedFile, path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "WireGuard private key already exists",
        ));
    }
    staged.commit_to(path)
}

fn validate_private_key(path: &Path, value: String) -> Result<String, BuiltinWireguardHostError> {
    ensure_private_key_permissions(path)?;
    let value = value.trim().to_owned();
    Key::try_from(value.as_str()).map_err(|error| {
        host_error(format!(
            "parse WireGuard private key {}: {error}",
            path.display()
        ))
    })?;
    Ok(value)
}

fn ensure_private_key_permissions(path: &Path) -> Result<(), BuiltinWireguardHostError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            host_error(format!(
                "make WireGuard private key private {}: {error}",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn validate_path(field: &'static str, path: &Path) -> Result<(), BuiltinWireguardConfigError> {
    if path.as_os_str().is_empty() {
        Err(BuiltinWireguardConfigError::EmptyPath { field })
    } else {
        Ok(())
    }
}

fn validate_ifname(field: &'static str, value: &str) -> Result<(), BuiltinWireguardConfigError> {
    let valid = !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(BuiltinWireguardConfigError::InvalidInterfaceName {
            field,
            value: value.to_owned(),
        })
    }
}

fn host_error(message: impl Into<String>) -> BuiltinWireguardHostError {
    BuiltinWireguardHostError::HostEffect {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> WireGuardPublicKey {
        use base64::Engine as _;
        WireGuardPublicKey::try_new(base64::engine::general_purpose::STANDARD.encode([byte; 32]))
            .expect("valid test key")
    }

    fn desired_peer(byte: u8, endpoint: Option<&str>, allowed: &[&str]) -> DesiredPeer {
        DesiredPeer {
            public_key: key(byte),
            endpoint: endpoint.map(|value| value.parse().expect("valid endpoint")),
            allowed_ips: allowed
                .iter()
                .map(|value| value.parse().expect("valid CIDR"))
                .collect(),
        }
    }

    #[test]
    fn optional_endpoint_is_valid_and_only_endpoint_peers_need_keepalive() {
        let endpointless = desired_peer(1, None, &["fd01::/112"]);
        let observed_endpoint = ObservedPeer {
            endpoint: Some("192.0.2.1:51820".parse().expect("endpoint")),
            allowed_ips: endpointless.allowed_ips.clone(),
            persistent_keepalive: Some(PERSISTENT_KEEPALIVE_SECONDS),
        };
        assert!(peer_matches(&observed_endpoint, &endpointless));

        let endpoint = desired_peer(1, Some("192.0.2.2:51820"), &["fd01::/112"]);
        assert!(!peer_matches(&observed_endpoint, &endpoint));
        assert!(
            !wireguard_peer_args("ployz0", &endpointless)
                .iter()
                .any(|arg| arg == "persistent-keepalive")
        );
        assert!(
            wireguard_peer_args("ployz0", &endpoint)
                .windows(2)
                .any(|pair| pair == ["persistent-keepalive", "25"])
        );
    }

    #[test]
    fn allowed_ips_render_dual_stack_without_host_widening() {
        let allowed = BTreeSet::from([
            "10.42.2.0/24".parse().expect("IPv4 /24"),
            "fd42:1:2:3::/112".parse().expect("IPv6 /112"),
        ]);

        let rendered = render_allowed_ips(&allowed);

        assert_eq!(
            rendered.split(',').collect::<BTreeSet<_>>(),
            BTreeSet::from(["10.42.2.0/24", "fd42:1:2:3::/112"])
        );
    }

    #[test]
    fn wireguard_dump_preserves_endpointless_peers() {
        let dump = format!(
            "private\t{}\t51820\toff\n{}\t(none)\t(none)\tfd42::/112\t0\t0\t0\toff\n",
            key(9).as_str(),
            key(1).as_str()
        );

        assert_eq!(
            parse_wireguard_dump(&dump).expect("dump parses"),
            BTreeMap::from([(
                key(1),
                ObservedPeer {
                    endpoint: None,
                    allowed_ips: BTreeSet::from(["fd42::/112".parse().expect("CIDR")]),
                    persistent_keepalive: None,
                }
            )])
        );
    }

    #[test]
    fn interface_address_observation_finds_only_explicit_ipv6_cidrs() {
        let output = concat!(
            "7: ployz0    inet6 fd42::1/112 scope global nodad\n",
            "7: ployz0    inet6 fd43::1/112 scope global deprecated\n",
        );

        assert_eq!(
            parse_interface_ipv6_addresses(output).expect("addresses parse"),
            BTreeSet::from([
                "fd42::1/112".parse().expect("address"),
                "fd43::1/112".parse().expect("address"),
            ])
        );
    }

    #[test]
    fn owned_route_observation_does_not_filter_by_ipv6_scope() {
        assert_eq!(
            owned_route_observation_args("-6", "ployz0"),
            [
                "-o", "-6", "route", "show", "dev", "ployz0", "proto", "boot"
            ]
        );
    }

    #[test]
    fn upserts_and_new_routes_precede_removals() {
        let desired = desired_peer(2, None, &["fd02::/112", "10.42.2.0/24"]);
        let observed_peers = BTreeMap::from([(
            key(1),
            ObservedPeer {
                endpoint: None,
                allowed_ips: BTreeSet::from(["fd01::/112".parse().expect("CIDR")]),
                persistent_keepalive: None,
            },
        )]);
        let observed_routes = BTreeSet::from(["fd01::/112".parse().expect("CIDR")]);

        let actions = convergence_plan(&observed_peers, &observed_routes, &[desired]);

        let [
            ConvergenceAction::UpsertPeer(_),
            ConvergenceAction::ReplaceRoute(_),
            ConvergenceAction::ReplaceRoute(_),
            ConvergenceAction::RemovePeer(_),
            ConvergenceAction::RemoveRoute(_),
        ] = actions.as_slice()
        else {
            panic!("upserts and route additions must precede removals: {actions:?}");
        };
    }

    #[test]
    fn stale_routes_are_removed_even_without_peer_drift() {
        let desired = desired_peer(2, None, &["fd02::/112"]);
        let observed_peers = BTreeMap::from([(
            key(2),
            ObservedPeer {
                endpoint: None,
                allowed_ips: desired.allowed_ips.clone(),
                persistent_keepalive: None,
            },
        )]);
        let stale = "10.42.9.0/24".parse().expect("CIDR");
        let observed_routes = BTreeSet::from(["fd02::/112".parse().expect("CIDR"), stale]);

        assert_eq!(
            convergence_plan(&observed_peers, &observed_routes, &[desired]),
            [ConvergenceAction::RemoveRoute(stale)]
        );
    }

    #[test]
    fn fence_plan_removes_every_peer_and_owned_route() {
        let observed_peers = BTreeMap::from([(
            key(1),
            ObservedPeer {
                endpoint: None,
                allowed_ips: BTreeSet::new(),
                persistent_keepalive: None,
            },
        )]);
        let route = "fd01::/112".parse().expect("CIDR");

        assert_eq!(
            convergence_plan(&observed_peers, &BTreeSet::from([route]), &[]),
            [
                ConvergenceAction::RemovePeer(key(1)),
                ConvergenceAction::RemoveRoute(route),
            ]
        );
    }

    #[test]
    fn single_machine_plan_has_no_peer_or_route_actions() {
        assert!(convergence_plan(&BTreeMap::new(), &BTreeSet::new(), &[]).is_empty());
    }

    #[test]
    fn missing_bridge_degrades_ebpf_without_changing_wireguard_readiness() {
        let outcome = BuiltinWireguardHostOutcome {
            wireguard: WireguardHostReady {
                public_key: key(1),
                bind_address: "fd42::1/112".parse().expect("bind address"),
                peer_count: 1,
                route_count: 2,
            },
            ebpf: ebpf_outcome(1, "br-ployz", Err(EbpfEffectError::MissingBridge)),
        };
        assert_eq!(outcome.wireguard.peer_count, 1);
        assert_eq!(
            outcome.ebpf,
            EbpfHostOutcome::Degraded {
                reason: EbpfDegradedReason::MissingBridge {
                    ifname: "br-ployz".to_owned(),
                },
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn key_creation_is_private_without_chmodding_a_shared_parent() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("tempdir");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("set shared parent mode");
        let path = directory.path().join("wireguard.key");

        let key = provision_private_key(&path).expect("key provisions");

        assert!(!key.is_empty());
        assert_eq!(
            fs::metadata(directory.path())
                .expect("parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(path)
                .expect("key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn config_rejects_unbounded_or_kernel_invalid_values() {
        let ebpf = || {
            BuiltinWireguardEbpfConfig::try_new(
                "ployz0".to_owned(),
                PathBuf::from("/ctl"),
                PathBuf::from("/bytecode"),
                EbpfPinning::Default,
            )
            .expect("eBPF config")
        };
        let invalid_ifname = BuiltinWireguardHostConfig::try_new(
            PathBuf::from("/key"),
            "interface-name-is-too-long".to_owned(),
            51_820,
            1_420,
            ebpf(),
            SupervisorBackend::Systemd,
            Duration::from_secs(5),
        )
        .expect_err("long ifname is invalid");
        assert!(matches!(
            invalid_ifname,
            BuiltinWireguardConfigError::InvalidInterfaceName { .. }
        ));
        assert_eq!(
            BuiltinWireguardHostConfig::try_new(
                PathBuf::from("/key"),
                "ployz0".to_owned(),
                51_820,
                1_420,
                ebpf(),
                SupervisorBackend::Systemd,
                Duration::ZERO,
            ),
            Err(BuiltinWireguardConfigError::ZeroCommandTimeout)
        );
        assert_eq!(
            BuiltinWireguardHostConfig::try_new(
                PathBuf::from("/key"),
                "ployz0".to_owned(),
                51_820,
                1_420,
                ebpf(),
                SupervisorBackend::Systemd,
                Duration::from_secs(61),
            ),
            Err(BuiltinWireguardConfigError::CommandTimeoutTooLong {
                milliseconds: 61_000
            })
        );
    }
}
