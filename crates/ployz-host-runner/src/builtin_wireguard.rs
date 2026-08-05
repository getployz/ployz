//! Privileged, bounded host effects for the built-in WireGuard mesh.
//!
//! Cluster policy stays in `ployz-core`. This module accepts a complete,
//! validated desired mesh and owns only local key, interface, route, and eBPF
//! convergence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

use defguard_wireguard_rs::key::Key;
use ipnet::{IpNet, Ipv6Net};
use ployz_core::corrosion::{
    BuiltinWireguardMemberSubnet, DesiredBuiltinWireguardLocal, DesiredBuiltinWireguardMesh,
};
pub use ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
use thiserror::Error;

#[cfg(test)]
use crate::SupervisorBackend;
use crate::{
    AssignedHostPort, FirewallBackend, HostRunnerCommandOutput, HostRunnerCommandRunner,
    SystemHostRunnerCommandRunner, detect_firewall_backend,
};

mod config;
#[cfg(test)]
mod firewall_tests;
mod mesh_state;
mod private_key;

pub use config::{
    BuiltinWireguardConfigError, BuiltinWireguardEbpfConfig, BuiltinWireguardHostConfig,
    BuiltinWireguardPorts,
};
pub use mesh_state::WireguardHandshakeEpoch;
use mesh_state::*;
use private_key::provision_private_key;

pub const DEFAULT_PRIVATE_KEY_PATH: &str = "/etc/ployz/wireguard.key";
pub const DEFAULT_WIREGUARD_IFNAME: &str = "ployz0";
pub const DEFAULT_WIREGUARD_MTU: u16 = 1_420;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinWireguardHostError {
    #[error("built-in WireGuard host effect failed: {message}")]
    HostEffect { message: String },
    #[error("required built-in WireGuard ports are blocked by unmanaged firewall {backend:?}")]
    UnmanagedFirewall { backend: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardHostOutcome {
    pub ebpf: EbpfHostOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireguardLocalBinding {
    pub public_key: WireGuardPublicKey,
    pub bind_address: Ipv6Net,
}

/// The single reachable roster machine used before a joiner's Corrosion store has synced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardJoinSeed {
    pub public_key: WireGuardPublicKey,
    pub subnet_v6: BuiltinWireguardMemberSubnet,
    pub endpoint: std::net::SocketAddr,
    pub subnet_v4: MachineEndpointSubnet,
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

#[derive(Clone, Copy)]
enum CorrosionGossipPhase<'a> {
    Bootstrap,
    Desired(&'a BTreeSet<Ipv6Net>),
    Fenced,
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

    /// Reads one peer's latest-handshake epoch without changing mesh state.
    pub fn observe_peer_handshake(
        &mut self,
        public_key: &WireGuardPublicKey,
    ) -> Result<WireguardHandshakeEpoch, BuiltinWireguardHostError> {
        let ifname = self.config.wg_ifname.clone();
        let output = self
            .run("wg", &["show", &ifname, "dump"])
            .map_err(host_error)?;
        if !output.success {
            return Err(host_error(output.failure));
        }
        if output.stdout_truncated {
            return Err(host_error("WireGuard peer observation was truncated"));
        }
        parse_wireguard_handshake_epoch(&output.stdout, public_key).map_err(host_error)
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
        self.bind_local_effects(local, CorrosionGossipPhase::Bootstrap)
    }

    /// Makes one accepted seed reachable before Corrosion can supply the full roster.
    /// Keeper replaces this bootstrap projection with the ordinary roster projection.
    pub fn bootstrap_join_seed(
        &mut self,
        local: &DesiredBuiltinWireguardLocal,
        seed: &BuiltinWireguardJoinSeed,
    ) -> Result<WireguardLocalBinding, BuiltinWireguardHostError> {
        let sources = BTreeSet::from([local.subnet_v6.network(), seed.subnet_v6.network()]);
        let binding = self.bind_local_effects(local, CorrosionGossipPhase::Desired(&sources))?;
        let desired = DesiredPeer {
            public_key: seed.public_key.clone(),
            endpoint: Some(seed.endpoint),
            allowed_ips: BTreeSet::from([
                IpNet::V6(seed.subnet_v6.network()),
                seed.subnet_v4.ipnet(),
            ]),
        };
        let observed_peers = self.read_observed_peers().map_err(host_error)?;
        let observed_routes = self.read_observed_routes().map_err(host_error)?;
        let actions = convergence_plan(&observed_peers, &observed_routes, &[desired]);
        self.apply_convergence_actions(&actions)
            .map_err(host_error)?;
        Ok(binding)
    }

    fn bind_local_effects(
        &mut self,
        local: &DesiredBuiltinWireguardLocal,
        gossip: CorrosionGossipPhase<'_>,
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
        let listen_port = self.config.ports.listen.get().to_string();
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
        self.activate_mesh_interface(gossip)?;
        self.verify_local_binding(&public_key, bind_address)
            .map_err(host_error)?;
        Ok(WireguardLocalBinding {
            public_key,
            bind_address,
        })
    }

    fn activate_mesh_interface(
        &mut self,
        gossip: CorrosionGossipPhase<'_>,
    ) -> Result<(), BuiltinWireguardHostError> {
        let ifname = self.config.wg_ifname.clone();
        match gossip {
            CorrosionGossipPhase::Bootstrap => {}
            CorrosionGossipPhase::Desired(sources) => self
                .ensure_corrosion_firewall(sources)
                .map_err(firewall_host_error)?,
            CorrosionGossipPhase::Fenced => self
                .ensure_corrosion_firewall(&BTreeSet::new())
                .map_err(firewall_host_error)?,
        }
        self.require("ip", &["link", "set", "dev", &ifname, "up"])
            .map_err(host_error)?;
        self.ensure_wireguard_firewall(!matches!(gossip, CorrosionGossipPhase::Bootstrap))
            .map_err(|error| match error {
                FirewallEffectError::Host(message) => host_error(message),
                FirewallEffectError::Unmanaged(backend) => {
                    BuiltinWireguardHostError::UnmanagedFirewall { backend }
                }
            })
    }

    /// Converges one nonempty, locally-authorized Core projection.
    pub fn converge(
        &mut self,
        desired: &DesiredBuiltinWireguardMesh,
    ) -> Result<BuiltinWireguardHostOutcome, BuiltinWireguardHostError> {
        let machine_sources = machine_gossip_sources(desired);
        self.bind_local_effects(
            &desired.local,
            CorrosionGossipPhase::Desired(&machine_sources),
        )?;
        let peers = render_desired_peers(desired);
        let observed_peers = self.read_observed_peers().map_err(host_error)?;
        let observed_routes = self.read_observed_routes().map_err(host_error)?;
        let actions = convergence_plan(&observed_peers, &observed_routes, &peers);
        self.apply_convergence_actions(&actions)
            .map_err(host_error)?;
        let ebpf = self.converge_ebpf(&desired.ebpf_routes);
        Ok(BuiltinWireguardHostOutcome { ebpf })
    }

    /// Explicitly removes every peer and owned route while retaining the local
    /// identity/address. Keeper calls this only for a nonempty roster that no
    /// longer contains the local machine.
    pub fn fence(
        &mut self,
        local: &DesiredBuiltinWireguardLocal,
    ) -> Result<BuiltinWireguardHostOutcome, BuiltinWireguardHostError> {
        self.bind_local_effects(local, CorrosionGossipPhase::Fenced)?;
        let observed_peers = self.read_observed_peers().map_err(host_error)?;
        let observed_routes = self.read_observed_routes().map_err(host_error)?;
        let actions = convergence_plan(&observed_peers, &observed_routes, &[]);
        self.apply_convergence_actions(&actions)
            .map_err(host_error)?;
        let ebpf = self.converge_ebpf(&[]);
        Ok(BuiltinWireguardHostOutcome { ebpf })
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

    fn ensure_wireguard_firewall(
        &mut self,
        admit_control_plane: bool,
    ) -> Result<(), FirewallEffectError> {
        let backend = detect_firewall_backend(self.config.supervisor, &mut self.runner)
            .map_err(|error| FirewallEffectError::Host(error.as_str().to_owned()))?;
        if let FirewallBackend::Unmanaged(name) = backend {
            return Err(FirewallEffectError::Unmanaged(name));
        }
        self.ensure_required_firewall_ports(&backend, admit_control_plane)
            .map_err(|error| FirewallEffectError::Host(error.as_str().to_owned()))
    }

    fn ensure_required_firewall_ports(
        &mut self,
        backend: &FirewallBackend,
        admit_control_plane: bool,
    ) -> Result<(), ployz_core::operation::FailureMessage> {
        backend.open_with(
            AssignedHostPort::udp(self.config.ports.listen.get()),
            &mut self.runner,
        )?;
        backend.open_with(
            AssignedHostPort::tcp(self.config.ports.join_door_https.get()),
            &mut self.runner,
        )?;
        if admit_control_plane {
            backend.allow_control_plane_http_with(
                &self.config.wg_ifname,
                self.config.ports.api_http.get(),
                &mut self.runner,
            )?;
        }
        Ok(())
    }

    fn ensure_corrosion_firewall(
        &mut self,
        machine_sources: &BTreeSet<Ipv6Net>,
    ) -> Result<(), FirewallEffectError> {
        let backend = detect_firewall_backend(self.config.supervisor, &mut self.runner)
            .map_err(|error| FirewallEffectError::Host(error.as_str().to_owned()))?;
        if let FirewallBackend::Unmanaged(name) = backend {
            return Err(FirewallEffectError::Unmanaged(name));
        }
        backend
            .restrict_udp_to_ipv6_sources_with(
                &self.config.wg_ifname,
                self.config.ports.corrosion_gossip.get(),
                machine_sources,
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

    fn converge_ebpf(&mut self, routes: &[MachineEndpointSubnet]) -> EbpfHostOutcome {
        let result = self.try_converge_ebpf(routes);
        ebpf_outcome(routes.len(), &self.config.ebpf.bridge_ifname, result)
    }

    fn try_converge_ebpf(
        &mut self,
        routes: &[MachineEndpointSubnet],
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
        route_args.extend(routes.iter().map(MachineEndpointSubnet::as_string));
        let route_args = self.ebpf_args(route_args);
        let route_refs = route_args.iter().map(String::as_str).collect::<Vec<_>>();
        self.require(&ctl, &route_refs)
            .map_err(EbpfEffectError::HostEffect)?;
        Ok(())
    }

    fn ebpf_args(&self, args: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut rendered = vec![
            "--pin-path".to_owned(),
            self.config.ebpf.pin_path.display().to_string(),
        ];
        rendered.extend(args);
        rendered
    }

    fn ensure_forwarding_firewall(&mut self, bridge: &str, wg: &str) -> Result<(), String> {
        let backend = detect_firewall_backend(self.config.supervisor, &mut self.runner)
            .map_err(|error| error.as_str().to_owned())?;
        if let FirewallBackend::Unmanaged(name) = &backend {
            return Err(format!(
                "active host firewall {name} is not managed by Ployz"
            ));
        }
        backend
            .allow_forwarding_between_with(wg, bridge, &mut self.runner)
            .map_err(|error| error.as_str().to_owned())
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

fn host_error(message: impl Into<String>) -> BuiltinWireguardHostError {
    BuiltinWireguardHostError::HostEffect {
        message: message.into(),
    }
}

fn firewall_host_error(error: FirewallEffectError) -> BuiltinWireguardHostError {
    match error {
        FirewallEffectError::Host(message) => host_error(message),
        FirewallEffectError::Unmanaged(backend) => {
            BuiltinWireguardHostError::UnmanagedFirewall { backend }
        }
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
    fn wireguard_dump_exposes_latest_handshake_epoch_without_confusing_never_and_absent() {
        let dump = format!(
            "private\t{}\t51820\toff\n{}\t(none)\t(none)\tfd42::/112\t0\t0\t0\toff\n{}\t(none)\t(none)\tfd43::/112\t123\t0\t0\toff\n",
            key(9).as_str(),
            key(1).as_str(),
            key(2).as_str(),
        );

        assert_eq!(
            parse_wireguard_handshake_epoch(&dump, &key(1)).expect("never parses"),
            WireguardHandshakeEpoch::Never
        );
        assert_eq!(
            parse_wireguard_handshake_epoch(&dump, &key(2)).expect("epoch parses"),
            WireguardHandshakeEpoch::Observed(std::num::NonZeroU64::new(123).expect("nonzero"))
        );
        assert_eq!(
            parse_wireguard_handshake_epoch(&dump, &key(3)).expect("absence parses"),
            WireguardHandshakeEpoch::PeerAbsent
        );
    }

    #[test]
    fn malformed_latest_handshake_epoch_is_rejected() {
        let dump = format!(
            "private\t{}\t51820\toff\n{}\t(none)\t(none)\tfd42::/112\tnot-an-epoch\t0\t0\toff\n",
            key(9).as_str(),
            key(1).as_str(),
        );

        assert!(
            parse_wireguard_handshake_epoch(&dump, &key(1))
                .expect_err("malformed epoch")
                .contains("latest-handshake epoch")
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
    fn missing_bridge_degrades_ebpf() {
        let outcome = BuiltinWireguardHostOutcome {
            ebpf: ebpf_outcome(1, "br-ployz", Err(EbpfEffectError::MissingBridge)),
        };
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
        assert_eq!(
            BuiltinWireguardPorts::try_new(0, 8_787, 2_020, 2_021),
            Err(BuiltinWireguardConfigError::ZeroListenPort)
        );
        assert_eq!(
            BuiltinWireguardPorts::try_new(51_820, 0, 2_020, 2_021),
            Err(BuiltinWireguardConfigError::ZeroCorrosionGossipPort)
        );
        assert_eq!(
            BuiltinWireguardPorts::try_new(51_820, 8_787, 0, 2_021),
            Err(BuiltinWireguardConfigError::ZeroApiHttpPort)
        );
        assert_eq!(
            BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 0),
            Err(BuiltinWireguardConfigError::ZeroJoinDoorHttpsPort)
        );
        let ebpf = || {
            BuiltinWireguardEbpfConfig::try_new(
                "ployz0".to_owned(),
                PathBuf::from("/ctl"),
                PathBuf::from("/bytecode"),
                PathBuf::from("/pin"),
            )
            .expect("eBPF config")
        };
        assert!(matches!(
            BuiltinWireguardEbpfConfig::try_new(
                "ployz0".to_owned(),
                PathBuf::from("relative-ctl"),
                PathBuf::from("/bytecode"),
                PathBuf::from("/pin"),
            ),
            Err(BuiltinWireguardConfigError::RelativePath {
                field: "eBPF control program",
                ..
            })
        ));
        assert!(matches!(
            BuiltinWireguardHostConfig::try_new(
                PathBuf::from("relative-key"),
                "ployz0".to_owned(),
                BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
                1_420,
                ebpf(),
                SupervisorBackend::Systemd,
                Duration::from_secs(5),
            ),
            Err(BuiltinWireguardConfigError::RelativePath {
                field: "private key",
                ..
            })
        ));
        let invalid_ifname = BuiltinWireguardHostConfig::try_new(
            PathBuf::from("/key"),
            "interface-name-is-too-long".to_owned(),
            BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
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
                BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
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
                BuiltinWireguardPorts::try_new(51_820, 8_787, 2_020, 2_021).expect("ports"),
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
