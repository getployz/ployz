use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::Ipv4Addr,
    path::Path,
};

use bollard::{
    models::{
        ContainerSummary, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
        NetworkCreateRequest, NetworkDisconnectRequest, NetworkInspect, RestartPolicyNameEnum,
    },
    query_parameters::{ListContainersOptionsBuilder, StopContainerOptionsBuilder},
};
use ployz_core::{DOCKER_NETWORK_CONFLICT_RECOVERY, MachineSubnet, labels};
use ployz_decide::bridge::{self, Settle};

use crate::docker_image::is_not_found;

use crate::network::{
    DOCKER_NETWORK_NAME, NetworkError, WIREGUARD_INTERFACE_NAME, checked_command,
    docker_firewall::Ipv6Off,
    firewall::{BridgeFence, docker_ipv6_firewall, fence_container_ipv6},
};

use super::NetworkPlane;
use super::{pins::Pins, stopped::Stopped};

/// The static address each member takes: its own, or the next free one.
pub(super) fn addresses<'a>(
    members: impl IntoIterator<Item = &'a BridgeMember>,
    mut taken: BTreeSet<Ipv4Addr>,
    subnet: MachineSubnet,
) -> Vec<(&'a BridgeMember, Option<Ipv4Addr>)> {
    let mut last = None;
    members
        .into_iter()
        .map(|member| {
            let ipv4 = member.ipv4.or_else(|| {
                let next = crate::docker::bridge_address::next(subnet, &taken, last);
                last = next.or(last);
                next
            });
            taken.extend(ipv4);
            (member, ipv4)
        })
        .collect()
}

fn report_readdress(take_ipv6: bool, error: &NetworkError) {
    eprintln!(
        "{failure}: {error}",
        failure = readdress_failure(take_ipv6),
        error = ployz_core::error_chain::inline(error),
    );
}

pub(super) fn readdress_failure(take_ipv6: bool) -> String {
    let change = if take_ipv6 { "take" } else { "drop" };
    format!("the {DOCKER_NETWORK_NAME} network could not {change} IPv6")
}

fn report_stop(id: &str, error: &(dyn std::error::Error + 'static)) {
    eprintln!(
        "container {id} could not be stopped to move it to the new {DOCKER_NETWORK_NAME} network: {error}",
        error = ployz_core::error_chain::inline(error),
    );
}

/// Whether Docker refused `error` because the container is between restarts.
pub(super) fn between_restarts(error: &bollard::errors::Error) -> bool {
    let bollard::errors::Error::DockerResponseServerError { message, .. } = error else {
        return false;
    };
    bridge::between_restarts(message, DOCKER_NETWORK_NAME)
}

pub(super) fn settle(
    running: bool,
    restarting: bool,
    policy: Option<RestartPolicyNameEnum>,
) -> Settle {
    bridge::settle(running, restarting, restart_policy(policy))
}

/// The policy Docker holds for a container, or `None` when it reports none. An empty name is
/// no policy, as Docker treats it. Only the name tells whether Docker runs the container again.
fn restart_policy(name: Option<RestartPolicyNameEnum>) -> Option<ployz_core::RestartPolicy> {
    match name? {
        RestartPolicyNameEnum::EMPTY => None,
        RestartPolicyNameEnum::NO => Some(ployz_core::RestartPolicy::No),
        RestartPolicyNameEnum::ALWAYS => Some(ployz_core::RestartPolicy::Always),
        RestartPolicyNameEnum::UNLESS_STOPPED => Some(ployz_core::RestartPolicy::UnlessStopped),
        RestartPolicyNameEnum::ON_FAILURE => Some(ployz_core::RestartPolicy::OnFailure {
            maximum_retry_count: None,
        }),
    }
}

/// A container on the ployz bridge, or created for it.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct BridgeMember {
    pub(super) id: String,
    pub(super) ipv4: Option<Ipv4Addr>,
    pub(super) attached: bool,
    pub(super) pinned: bool,
    pub(super) ipv6: bool,
}

impl BridgeMember {
    /// Detached, or still on a dynamic address.
    pub(super) fn needs_pin(&self) -> bool {
        bridge::needs_pin(self.attached, self.pinned)
    }
}

pub(super) fn bridge_members(containers: &[ContainerSummary]) -> Vec<BridgeMember> {
    let mut members = containers
        .iter()
        .filter_map(|container| {
            let endpoint = container
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                .and_then(|networks| networks.get(DOCKER_NETWORK_NAME));
            let created_on = container
                .host_config
                .as_ref()
                .and_then(|host| host.network_mode.as_deref())
                == Some(DOCKER_NETWORK_NAME);
            (endpoint.is_some() || created_on).then(|| BridgeMember {
                id: container.id.clone().unwrap_or_default(),
                ipv4: endpoint
                    .and_then(|endpoint| endpoint.ip_address.as_deref())
                    .and_then(|address| address.parse().ok())
                    .or_else(|| endpoint.and_then(crate::docker::bridge_address::pinned)),
                attached: endpoint.is_some(),
                pinned: endpoint
                    .and_then(crate::docker::bridge_address::pinned)
                    .is_some(),
                ipv6: container
                    .labels
                    .as_ref()
                    .is_some_and(crate::docker::takes_bridge_ipv6),
            })
        })
        .filter(|member| !member.id.is_empty())
        .collect::<Vec<_>>();
    members.sort();
    members
}

/// Docker turns on IPv6 forwarding for a dual-stack bridge, and a forwarding
/// interface ignores Router Advertisements unless its `accept_ra` is 2. A host that
/// learns its IPv6 route or address from them would lose it when that expires, so
/// its bridge stays IPv4-only until the operator sets 2, which the next start sees.
fn bridge_ipv6_allowed() -> bool {
    if !host_has_ipv6() {
        return false;
    }
    let listing = |args: &[&str]| {
        checked_command("ip", args)
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    let exposed = router_advertised_interfaces(
        &listing(&["-6", "-o", "route", "show", "default"]),
        &listing(&["-6", "-o", "addr", "show", "scope", "global"]),
    )
    .into_iter()
    .filter(|interface| {
        std::fs::read_to_string(format!("/proc/sys/net/ipv6/conf/{interface}/accept_ra"))
            .is_ok_and(|accept_ra| accept_ra.trim() == "1")
    })
    .collect::<Vec<_>>();
    if exposed.is_empty() {
        return true;
    }
    let settings = exposed
        .iter()
        .map(|interface| format!("net.ipv6.conf.{}.accept_ra=2", interface.replace('.', "/")))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "the {DOCKER_NETWORK_NAME} network stays IPv4-only, so Services have no IPv6 egress and apps see IPv6 visitors as its gateway: \
         {interfaces} learn IPv6 from router advertisements, which IPv6 forwarding would stop. \
         Run `sysctl -w {settings}`, persist it in /etc/sysctl.d, then `systemctl restart ployz`.",
        interfaces = exposed.join(", "),
    );
    false
}

/// Interfaces with a default route or a global address from a router advertisement,
/// read from `ip -6 -o route show default` and `ip -6 -o addr show scope global`.
pub(super) fn router_advertised_interfaces(routes: &str, addresses: &str) -> BTreeSet<String> {
    let routed = routes.lines().filter_map(|line| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        let after = |key| {
            words
                .iter()
                .skip_while(|word| **word != key)
                .nth(1)
                .copied()
        };
        (after("proto") == Some("ra"))
            .then(|| after("dev").map(str::to_owned))
            .flatten()
    });
    let addressed = addresses.lines().filter_map(|line| {
        let mut words = line.split_whitespace();
        let interface = words.nth(1)?;
        words
            .any(|word| word == "dynamic")
            .then(|| interface.to_owned())
    });
    routed.chain(addressed).collect()
}

pub(super) fn ipv6_firewall_off(
    host_has_ipv6: bool,
    firewall: &Result<(), Ipv6Off>,
) -> Option<String> {
    let Err(off) = firewall else {
        return None;
    };
    host_has_ipv6.then(|| {
        format!(
            "the {DOCKER_NETWORK_NAME} network stays IPv4-only, so Services have no IPv6 egress and apps see IPv6 visitors as its gateway: {off}"
        )
    })
}

/// The host can carry IPv6 on a new bridge: the kernel has it and new interfaces
/// don't start with it disabled.
fn host_has_ipv6() -> bool {
    Path::new("/proc/net/if_inet6").exists()
        && std::fs::read_to_string("/proc/sys/net/ipv6/conf/default/disable_ipv6")
            .is_ok_and(|disabled| disabled.trim() == "0")
}

/// A ULA `/64` that embeds the Machine Subnet, so no two Machines' containers share
/// one. IPv6 visitors reach published ports by DNAT, with their own source address,
/// instead of through docker-proxy, which would replace it with the gateway (#2837).
pub(super) fn docker_network_ipv6(subnet: MachineSubnet) -> IpamConfig {
    let [a, b, c, _] = subnet.gateway().0.octets();
    let prefix = std::net::Ipv6Addr::new(
        0xfdcd,
        0,
        u16::from_be_bytes([a, b]),
        u16::from_be_bytes([c, 0]),
        0,
        0,
        0,
        0,
    );
    let gateway = std::net::Ipv6Addr::from(u128::from(prefix) + 1);
    IpamConfig {
        subnet: Some(format!("{prefix}/64")),
        gateway: Some(gateway.to_string()),
        ..Default::default()
    }
}

pub(super) fn docker_network_request(
    subnet: MachineSubnet,
    options: &HashMap<String, String>,
    ipv6: Option<&IpamConfig>,
) -> NetworkCreateRequest {
    let ipv4 = IpamConfig {
        subnet: Some(subnet.to_string()),
        gateway: Some(subnet.gateway().0.to_string()),
        ..Default::default()
    };
    NetworkCreateRequest {
        name: DOCKER_NETWORK_NAME.into(),
        driver: Some("bridge".into()),
        scope: Some("local".into()),
        enable_ipv6: Some(ipv6.is_some()),
        ipam: Some(Ipam {
            config: Some(std::iter::once(ipv4).chain(ipv6.cloned()).collect()),
            ..Default::default()
        }),
        options: Some(options.clone()),
        labels: Some(labels::system_labels()),
        ..Default::default()
    }
}

pub(super) fn docker_network_ipv6_matches(
    network: &NetworkInspect,
    wanted: Option<&IpamConfig>,
) -> bool {
    let observed = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .into_iter()
        .flatten()
        .find(|config| {
            config
                .subnet
                .as_deref()
                .is_some_and(|subnet| subnet.contains(':'))
        });
    network.enable_ipv6.unwrap_or(false) == wanted.is_some()
        && observed.map(|config| (&config.subnet, &config.gateway))
            == wanted.map(|config| (&config.subnet, &config.gateway))
}

pub(super) fn required_docker_network_options(mtu: u32) -> HashMap<String, String> {
    HashMap::from([
        (
            "com.docker.network.bridge.name".into(),
            DOCKER_NETWORK_NAME.into(),
        ),
        (
            "com.docker.network.bridge.trusted_host_interfaces".into(),
            WIREGUARD_INTERFACE_NAME.into(),
        ),
        ("com.docker.network.driver.mtu".into(), mtu.to_string()),
    ])
}

pub(super) fn docker_network_matches(
    network: &NetworkInspect,
    subnet: &str,
    gateway: &str,
    required_options: &HashMap<String, String>,
) -> bool {
    let ipam = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .and_then(|configs| {
            configs.iter().find(|config| {
                config
                    .subnet
                    .as_deref()
                    .is_some_and(|subnet| !subnet.contains(':'))
            })
        });
    docker_network_owned(network)
        && network.driver.as_deref() == Some("bridge")
        && network.scope.as_deref() == Some("local")
        && ipam.and_then(|config| config.subnet.as_deref()) == Some(subnet)
        && ipam.and_then(|config| config.gateway.as_deref()) == Some(gateway)
        && required_options.iter().all(|(key, value)| {
            network
                .options
                .as_ref()
                .and_then(|options| options.get(key))
                == Some(value)
        })
}

pub(super) fn docker_network_owned(network: &NetworkInspect) -> bool {
    network.name.as_deref() == Some(DOCKER_NETWORK_NAME)
        && network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(labels::SYSTEM_MANAGED))
            .is_some_and(String::is_empty)
}

pub(super) fn reset_network_id<'network>(
    network: &'network NetworkInspect,
    subnet: &str,
    gateway: &str,
    options: &HashMap<String, String>,
) -> Result<&'network str, NetworkError> {
    if docker_network_matches(network, subnet, gateway, options)
        && let Some(id) = network.id.as_deref()
    {
        return Ok(id);
    }
    Err(docker_network_conflict(
        network,
        subnet,
        gateway,
        options,
        "the network does not match the resetting Machine or has no stable Docker ID",
    ))
}

/// What a start does with the `ployz` network Docker already has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FoundNetwork {
    /// Shaped for this Machine: kept.
    Fits,
    /// Ployz's and empty, but shaped for another Machine: replaced.
    Stale,
    /// Ployz's and shaped for another Machine, with containers attached: refused.
    Attached,
    /// Not provably Ployz's: refused.
    Unowned,
}

pub(super) fn found_network(
    network: &NetworkInspect,
    subnet: &str,
    gateway: &str,
    required_options: &HashMap<String, String>,
) -> FoundNetwork {
    if docker_network_matches(network, subnet, gateway, required_options) {
        FoundNetwork::Fits
    } else if !docker_network_owned(network) {
        FoundNetwork::Unowned
    } else if network
        .containers
        .as_ref()
        .is_some_and(|containers| !containers.is_empty())
    {
        FoundNetwork::Attached
    } else {
        FoundNetwork::Stale
    }
}

pub(super) fn docker_network_conflict(
    network: &NetworkInspect,
    subnet: &str,
    gateway: &str,
    required_options: &HashMap<String, String>,
    reason: impl Into<String>,
) -> NetworkError {
    let quoted = |value: Option<&str>| {
        value.map_or_else(
            || "unknown".to_owned(),
            |value| format!("\"{}\"", value.escape_debug()),
        )
    };
    let options = |values: &HashMap<String, String>| {
        let mut pairs = values
            .iter()
            .map(|(key, value)| format!("{}={}", quoted(Some(key)), quoted(Some(value))))
            .collect::<Vec<_>>();
        pairs.sort();
        format!("{{{}}}", pairs.join(", "))
    };
    let ipam = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .map(|configs| {
            let configs = configs
                .iter()
                .map(|config| {
                    format!(
                        "{{subnet={}, gateway={}}}",
                        quoted(config.subnet.as_deref()),
                        quoted(config.gateway.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("({configs})")
        });
    let containers = network.containers.as_ref().map(|containers| {
        let mut ids = containers
            .keys()
            .map(|id| quoted(Some(id)))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        format!("({})", ids.join(", "))
    });
    NetworkError::DockerNetworkConflict {
        reason: reason.into(),
        expected: format!(
            "name={DOCKER_NETWORK_NAME}, driver=bridge, scope=local, label={}=\"\", subnet={subnet}, gateway={gateway}, options={}",
            labels::SYSTEM_MANAGED,
            options(required_options)
        ),
        observed: format!(
            "id={}, name={}, driver={}, scope={}, labels={}, IPAM={}, options={}, containers={}",
            quoted(network.id.as_deref()),
            quoted(network.name.as_deref()),
            quoted(network.driver.as_deref()),
            quoted(network.scope.as_deref()),
            network
                .labels
                .as_ref()
                .map(&options)
                .as_deref()
                .unwrap_or("unknown"),
            ipam.as_deref().unwrap_or("unknown"),
            network
                .options
                .as_ref()
                .map(options)
                .as_deref()
                .unwrap_or("unknown"),
            containers.as_deref().unwrap_or("unknown"),
        ),
        recovery: DOCKER_NETWORK_CONFLICT_RECOVERY,
    }
}

impl NetworkPlane {
    pub(super) async fn remove_docker_network(&self) -> Result<(), NetworkError> {
        let network = match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => network,
            Err(error) if is_not_found(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let options = required_docker_network_options(self.mtu);
        let id = reset_network_id(&network, &subnet, &gateway, &options)?;
        match self.docker.remove_network(id).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub(super) async fn ensure_docker_network(
        &self,
        data_dir: &Path,
    ) -> Result<(bool, BridgeFence), NetworkError> {
        // The fence goes in before any container holds IPv6 that NAT66 could carry
        // into the mesh, including one an earlier start gave IPv6; without it the
        // bridge stays IPv4-only.
        let firewall = docker_ipv6_firewall();
        if let Some(line) = ipv6_firewall_off(host_has_ipv6(), &firewall) {
            eprintln!("{line}");
        }
        let fenced = firewall.is_ok()
            && fence_container_ipv6()
                .inspect_err(|error| {
                    eprintln!(
                        "the {DOCKER_NETWORK_NAME} network stays IPv4-only: the IPv6 mesh fence failed: {error}",
                        error = ployz_core::error_chain::inline(error),
                    );
                })
                .is_ok();
        let ipv6 =
            (fenced && bridge_ipv6_allowed()).then(|| docker_network_ipv6(self.machine.subnet));
        // Both passes may stop members, which start again only once a bridge stands.
        let _extend =
            crate::systemd_timeout::extend_systemd_start_timeout("settling the ployz network");
        let mut stopped = Stopped::load(data_dir);
        let mut pins = Pins::load(data_dir);
        let created = self
            .ensure_docker_network_shape(ipv6.as_ref(), &mut stopped, &mut pins)
            .await?;
        self.pin_members(&mut stopped, pins).await;
        stopped.start(&self.docker).await;
        let fence = if fenced {
            BridgeFence::Required
        } else {
            BridgeFence::IfAvailable
        };
        Ok((created, fence))
    }

    async fn ensure_docker_network_shape(
        &self,
        ipv6: Option<&IpamConfig>,
        stopped: &mut Stopped,
        pins: &mut Pins,
    ) -> Result<bool, NetworkError> {
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let required_options = required_docker_network_options(self.mtu);
        let request = docker_network_request(self.machine.subnet, &required_options, ipv6);
        let ipv4_only = ipv6
            .is_some()
            .then(|| docker_network_request(self.machine.subnet, &required_options, None));
        match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => {
                let refusal = match found_network(&network, &subnet, &gateway, &required_options) {
                    FoundNetwork::Fits => {
                        if !docker_network_ipv6_matches(&network, ipv6) {
                            self.readdress_docker_network(
                                &network, request, ipv4_only, stopped, pins,
                            )
                            .await?;
                        }
                        return Ok(false);
                    }
                    FoundNetwork::Stale => {
                        let Some(network_id) = network.id.as_deref() else {
                            return Err(docker_network_conflict(
                                &network,
                                &subnet,
                                &gateway,
                                &required_options,
                                "the inspected network has no stable Docker ID",
                            ));
                        };
                        match self.docker.remove_network(network_id).await {
                            Ok(()) => None,
                            Err(error) if is_not_found(&error) => None,
                            Err(error) => Some(format!(
                                "Docker refused to remove the network after inspection: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            )),
                        }
                    }
                    FoundNetwork::Attached => Some("containers are attached".to_owned()),
                    FoundNetwork::Unowned => Some(
                        "ownership is unproven because the exact name and `ployzd.managed` label do not both match"
                            .to_owned(),
                    ),
                };
                if let Some(reason) = refusal {
                    return Err(docker_network_conflict(
                        &network,
                        &subnet,
                        &gateway,
                        &required_options,
                        reason,
                    ));
                }
            }
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(refused) = self.create_bridge(request, ipv4_only).await? {
            report_readdress(true, &refused);
        }
        Ok(true)
    }

    /// Docker can't change a network's addressing in place, so the bridge is replaced
    /// under its members, stopped ones included, since a stopped container keeps the
    /// old network's ID and could not start. Running containers keep running. Each
    /// member rejoins at the address it holds or is pinned to, so routes to it stay
    /// valid; one with neither takes a free one. Docker won't detach or attach a
    /// container between restarts, Ployz's or not, so such a member is stopped first
    /// and noted in `stopped` to start again. The bridge is replaced only once every
    /// member is detached, so a failure leaves a bridge serving, with every member on
    /// it; the next start tries again. Each member's address is written down in
    /// `pins` before any detach, so a start that dies midway leaves the next one the
    /// addresses Docker forgot.
    ///
    /// # Errors
    ///
    /// Returns an error only when the Machine is left with no bridge at all.
    async fn readdress_docker_network(
        &self,
        network: &NetworkInspect,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
        stopped: &mut Stopped,
        pins: &mut Pins,
    ) -> Result<(), NetworkError> {
        let take_ipv6 = ipv4_only.is_some();
        let mut members = match self.bridge_members().await {
            Ok(members) => members,
            Err(error) => {
                report_readdress(take_ipv6, &error);
                return Ok(());
            }
        };
        pins.restore(&mut members);
        if let Err(error) = pins.record(&members) {
            report_readdress(take_ipv6, &error.into());
            return Ok(());
        }
        members.sort_by_key(|member| (member.ipv4.is_none(), member.ipv4));
        let mut detached = Vec::new();
        let mut disconnected = Ok(());
        for member in members.iter().filter(|member| member.attached) {
            match self.detach(&member.id, stopped).await {
                Ok(true) => detached.push(member),
                Ok(false) => {}
                Err(error) => {
                    disconnected = Err(error.into());
                    break;
                }
            }
        }
        let replaced = match disconnected {
            Ok(()) => {
                self.replace_docker_network(network, request, ipv4_only)
                    .await
            }
            Err(error) => Ok(Some(error)),
        };
        if let Ok(Some(error)) = &replaced {
            report_readdress(take_ipv6, error);
        }
        let taken = members.iter().filter_map(|member| member.ipv4).collect();
        for (member, ipv4) in addresses(detached, taken, self.machine.subnet) {
            self.rejoin(member, ipv4, stopped).await;
        }
        replaced.map(|_| ())
    }

    /// Returns why the bridge stays as it was or fell back to IPv4-only, or an error
    /// when no bridge could be created.
    async fn replace_docker_network(
        &self,
        network: &NetworkInspect,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
    ) -> Result<Option<NetworkError>, NetworkError> {
        let id = network.id.as_deref().unwrap_or(DOCKER_NETWORK_NAME);
        match self.docker.remove_network(id).await {
            Ok(()) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Ok(Some(error.into())),
        }
        self.create_bridge(request, ipv4_only).await
    }

    /// Falls back to the IPv4-only bridge when Docker refuses the dual-stack one.
    /// Returns why it fell back, or an error when no bridge could be created.
    async fn create_bridge(
        &self,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
    ) -> Result<Option<NetworkError>, NetworkError> {
        let Err(refused) = self.docker.create_network(request).await else {
            return Ok(None);
        };
        let Some(ipv4_only) = ipv4_only else {
            return Err(refused.into());
        };
        match self.docker.create_network(ipv4_only).await {
            Ok(_) => Ok(Some(refused.into())),
            Err(error) => Err(NetworkError::Io(io::Error::other(format!(
                "Docker refused the {DOCKER_NETWORK_NAME} network both dual-stack ({refused}) and IPv4-only ({error})",
                refused = ployz_core::error_chain::inline(&refused),
                error = ployz_core::error_chain::inline(&error),
            )))),
        }
    }

    async fn bridge_members(&self) -> Result<Vec<BridgeMember>, NetworkError> {
        let containers = self.bridge_containers().await?;
        Ok(bridge_members(&containers))
    }

    async fn bridge_containers(&self) -> Result<Vec<ContainerSummary>, NetworkError> {
        Ok(self
            .docker
            .list_containers(Some(
                ListContainersOptionsBuilder::default().all(true).build(),
            ))
            .await?)
    }

    /// Every Container on the bridge holds a static address, so Docker can't hand a
    /// Container's address to another when it starts them all at boot. One without
    /// keeps the address it has, or takes a free one. A container created on the
    /// bridge that lost its endpoint, such as one a readdress detached, joins it again.
    /// One Ployz doesn't manage is pinned too, since a dynamic one could take a
    /// pinned address at boot, and it rejoins with its own aliases and options. A
    /// detached member takes back the address `pins` holds for it, if any.
    async fn pin_members(&self, stopped: &mut Stopped, mut pins: Pins) {
        let containers = match self.bridge_containers().await {
            Ok(containers) => containers,
            Err(error) => {
                eprintln!(
                    "could not list containers to pin their {DOCKER_NETWORK_NAME} network addresses: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return;
            }
        };
        let mut members = bridge_members(&containers);
        pins.restore(&mut members);
        let mut taken = crate::docker::bridge_address::taken(&containers);
        taken.extend(members.iter().filter_map(|member| member.ipv4));
        let detaches = members
            .iter()
            .any(|member| member.attached && member.needs_pin());
        let recorded = !detaches
            || pins
                .record(&members)
                .inspect_err(|error| {
                    eprintln!(
                        "containers keep dynamic {DOCKER_NETWORK_NAME} network addresses: {error}",
                    );
                })
                .is_ok();
        let unpinned = members.iter().filter(|member| member.needs_pin());
        let mut unjoined = BTreeSet::new();
        for (member, ipv4) in addresses(unpinned, taken, self.machine.subnet) {
            let mut endpoint = crate::docker::bridge_address::endpoint(member.ipv6, None);
            if member.attached {
                if !recorded {
                    continue;
                }
                match self.docker.inspect_container(&member.id, None).await {
                    Ok(inspected) => {
                        if let Some(current) = inspected
                            .network_settings
                            .and_then(|settings| settings.networks)
                            .and_then(|mut networks| networks.remove(DOCKER_NETWORK_NAME))
                        {
                            endpoint = crate::docker::bridge_address::reconnect(current);
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "container {container} keeps a dynamic {DOCKER_NETWORK_NAME} network address: {error}",
                            container = member.id,
                            error = ployz_core::error_chain::inline(&error),
                        );
                        continue;
                    }
                }
                match self.detach(&member.id, stopped).await {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        eprintln!(
                            "container {container} keeps a dynamic {DOCKER_NETWORK_NAME} network address: {error}",
                            container = member.id,
                            error = ployz_core::error_chain::inline(&error),
                        );
                        continue;
                    }
                }
            }
            if !self.rejoin_with(member, endpoint, ipv4, stopped).await {
                unjoined.insert(member.id.clone());
            }
        }
        pins.finish(&unjoined);
    }

    /// Detaches `id` from the bridge, stopping it first when Docker holds it between
    /// restarts. Returns `false` when the container is gone.
    async fn detach(
        &self,
        id: &str,
        stopped: &mut Stopped,
    ) -> Result<bool, bollard::errors::Error> {
        let disconnect = || {
            self.docker.disconnect_network(
                DOCKER_NETWORK_NAME,
                NetworkDisconnectRequest {
                    container: id.to_owned(),
                    force: Some(true),
                },
            )
        };
        let mut result = disconnect().await;
        if result.as_ref().is_err_and(between_restarts) {
            self.stop(id, stopped).await;
            result = disconnect().await;
        }
        match result {
            Ok(()) => Ok(true),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Settles `id`, which Docker holds between restarts, so the operation Docker
    /// refused can be retried; see `settle`. A stop drops a restart Docker has
    /// pending and waits out its own, so Docker can't attach the member behind a
    /// detach. A member is noted in `stopped` before its stop, and left alone when
    /// that note can't be written.
    async fn stop(&self, id: &str, stopped: &mut Stopped) {
        let inspected = match self.docker.inspect_container(id, None).await {
            Ok(inspected) => inspected,
            Err(error) => {
                if !is_not_found(&error) {
                    report_stop(id, &error);
                }
                return;
            }
        };
        let state = inspected.state.unwrap_or_default();
        let policy = inspected
            .host_config
            .and_then(|host| host.restart_policy)
            .and_then(|policy| policy.name);
        match settle(
            state.running == Some(true),
            state.restarting == Some(true),
            policy,
        ) {
            Settle::Leave => return,
            Settle::Stop => {}
            Settle::StopAndNote => {
                if let Err(error) = stopped.note(id) {
                    report_stop(id, &error);
                    return;
                }
            }
        }
        let options = StopContainerOptionsBuilder::default().t(0).build();
        if let Err(error) = self.docker.stop_container(id, Some(options)).await
            && !is_not_found(&error)
        {
            report_stop(id, &error);
        }
    }

    async fn rejoin(&self, member: &BridgeMember, ipv4: Option<Ipv4Addr>, stopped: &mut Stopped) {
        let endpoint = crate::docker::bridge_address::endpoint(member.ipv6, None);
        let _ = self.rejoin_with(member, endpoint, ipv4, stopped).await;
    }

    /// Falls back to any free address when `ipv4` can't be had. Stops a container
    /// Docker holds between restarts first, as `detach` does. Returns whether it
    /// rejoined.
    async fn rejoin_with(
        &self,
        member: &BridgeMember,
        endpoint: EndpointSettings,
        ipv4: Option<Ipv4Addr>,
        stopped: &mut Stopped,
    ) -> bool {
        let connect = |ipv4| NetworkConnectRequest {
            container: member.id.clone(),
            endpoint_config: Some(crate::docker::bridge_address::pin(endpoint.clone(), ipv4)),
        };
        let mut result = self
            .docker
            .connect_network(DOCKER_NETWORK_NAME, connect(ipv4))
            .await;
        if result.as_ref().is_err_and(between_restarts) {
            self.stop(&member.id, stopped).await;
            result = self
                .docker
                .connect_network(DOCKER_NETWORK_NAME, connect(ipv4))
                .await;
        }
        if result.is_err() && ipv4.is_some() {
            result = self
                .docker
                .connect_network(DOCKER_NETWORK_NAME, connect(None))
                .await;
        }
        if let Err(error) = &result {
            eprintln!(
                "container {container} could not rejoin the {DOCKER_NETWORK_NAME} network: {error}",
                container = member.id,
                error = ployz_core::error_chain::inline(error),
            );
        }
        result.is_ok()
    }
}
