use clap::Args;
use ployz_core::ids::OperationId;
use ployz_sdk_types::{
    EbpfAttachmentStatus, InternalDnsResolverStatus, NetworkDataplaneTestimony,
    NetworkInternalDnsTestimony, NetworkRepairRequest, NetworkResolveMachineTestimony,
    NetworkResolveRequest, NetworkResolveResult, NetworkStatusMachine, NetworkStatusMode,
    NetworkStatusRequest, NetworkStatusResult, WireGuardConfiguredMtu, WireGuardDetectedMtu,
    WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardMtuProbe,
    WireGuardPeerEndpointSubnet, WireGuardRttStatus,
};
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::client_ids::generate_client_network_repair_id;
use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusCommand {
    pub mode: NetworkStatusMode,
}

impl NetworkStatusCommand {
    #[must_use]
    pub const fn into_request(self) -> NetworkStatusRequest {
        NetworkStatusRequest { mode: self.mode }
    }
}

pub(crate) const fn network_status_command(parsed: NetworkStatusCli) -> NetworkStatusCommand {
    NetworkStatusCommand {
        mode: if parsed.probe {
            NetworkStatusMode::ProbePathMtu
        } else {
            NetworkStatusMode::Snapshot
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResolveCommand {
    pub name: String,
}

impl NetworkResolveCommand {
    #[must_use]
    pub fn into_request(self) -> NetworkResolveRequest {
        NetworkResolveRequest { name: self.name }
    }
}

pub(crate) fn network_resolve_command(parsed: NetworkResolveCli) -> NetworkResolveCommand {
    NetworkResolveCommand { name: parsed.name }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRepairCommand {
    pub operation_id: OperationId,
    pub machine_id: Option<ployz_core::ids::MachineId>,
    pub detach: bool,
}

impl NetworkRepairCommand {
    #[must_use]
    pub fn into_request(self) -> NetworkRepairRequest {
        NetworkRepairRequest {
            operation_id: self.operation_id,
            machine_id: self.machine_id,
        }
    }
}

pub(crate) fn network_repair_command(
    parsed: NetworkRepairCli,
) -> Result<NetworkRepairCommand, PloyzctlCliError> {
    let machine_id = parsed
        .machine
        .map(ployz_core::ids::MachineId::try_new)
        .transpose()
        .map_err(|error| invalid_value("machine id", error))?;
    let operation_id = generate_client_network_repair_id(machine_id.as_ref())
        .map_err(|error| invalid_value("network repair", error))?
        .operation_id;
    Ok(NetworkRepairCommand {
        operation_id,
        machine_id,
        detach: parsed.detach,
    })
}

#[derive(Debug, Args)]
pub(crate) struct NetworkStatusCli {
    #[arg(long)]
    probe: bool,
}

#[derive(Debug, Args)]
pub(crate) struct NetworkResolveCli {
    name: String,
}

#[derive(Debug, Args)]
pub(crate) struct NetworkRepairCli {
    #[arg(long)]
    machine: Option<String>,
    #[arg(long)]
    detach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatusOutput {
    pub machines: Vec<NetworkStatusMachine>,
}

impl NetworkStatusOutput {
    #[must_use]
    pub fn from_result(result: NetworkStatusResult) -> Self {
        Self {
            machines: result.machines,
        }
    }

    #[must_use]
    pub fn render(&self) -> String {
        render_rows(self.machines.iter().flat_map(render_status_rows))
    }
}

fn render_status_rows(machine: &NetworkStatusMachine) -> Vec<String> {
    let mut rows = vec![format!(
        "machine {} {} endpoint-subnet {}",
        machine.active.machine_id.as_str(),
        machine.active.name.as_str(),
        machine.active.endpoint_subnet.as_string(),
    )];
    match &machine.dataplane {
        NetworkDataplaneTestimony::NoAnswer => rows.push("  dataplane no answer".to_owned()),
        NetworkDataplaneTestimony::ReadFailed { message } => {
            rows.push(format!("  dataplane read failed: {}", message.as_str()));
        }
        NetworkDataplaneTestimony::WrongResponder { actual_machine_id } => rows.push(format!(
            "  dataplane wrong responder: {}",
            actual_machine_id.as_str()
        )),
        NetworkDataplaneTestimony::TimedOut => rows.push("  dataplane timed out".to_owned()),
        NetworkDataplaneTestimony::RequestFailed { message } => {
            rows.push(format!("  dataplane request failed: {message}"));
        }
        NetworkDataplaneTestimony::ProtocolFailed { message } => {
            rows.push(format!("  dataplane protocol failed: {message}"));
        }
        NetworkDataplaneTestimony::DecodeFailed { message } => {
            rows.push(format!("  dataplane decode failed: {message}"));
        }
        NetworkDataplaneTestimony::Answered { value } => {
            let configured = match value.wireguard.configured_mtu {
                WireGuardConfiguredMtu::Auto => "auto".to_owned(),
                WireGuardConfiguredMtu::Fixed { mtu } => mtu.to_string(),
            };
            let detected = match &value.wireguard.detected_mtu {
                WireGuardDetectedMtu::Detected { mtu } => mtu.to_string(),
                WireGuardDetectedMtu::Unavailable { message } => {
                    format!("unavailable({message})")
                }
            };
            let interface_mtu = match &value.wireguard.interface_mtu {
                WireGuardInterfaceMtu::Detected { mtu } => mtu.to_string(),
                WireGuardInterfaceMtu::Unavailable { message } => {
                    format!("unavailable({message})")
                }
            };
            let ebpf = match &value.ebpf_attachment {
                EbpfAttachmentStatus::Attached => "attached".to_owned(),
                EbpfAttachmentStatus::Detached { message } => format!("detached({message})"),
                EbpfAttachmentStatus::Unknown { message } => format!("unknown({message})"),
            };
            rows.push(format!(
                "  dataplane wg={} mtu-configured={} mtu-detected={} mtu-interface={} ebpf={}",
                value.wireguard.interface, configured, detected, interface_mtu, ebpf
            ));
            rows.extend(value.wireguard.peers.iter().map(|peer| {
                let endpoint = peer
                    .endpoint
                    .map_or_else(|| "none".to_owned(), |endpoint| endpoint.to_string());
                let handshake = match peer.handshake {
                    WireGuardHandshakeStatus::Never => "never".to_owned(),
                    WireGuardHandshakeStatus::Ago { seconds } => format!("{seconds}s"),
                };
                let rtt = match &peer.rtt {
                    WireGuardRttStatus::Measured { micros } => format!("{micros}us"),
                    WireGuardRttStatus::Unavailable { message } => {
                        format!("unavailable({message})")
                    }
                };
                let mtu_probe = match &peer.mtu_probe {
                    WireGuardMtuProbe::NotRequested => "not-requested".to_owned(),
                    WireGuardMtuProbe::Measured { mtu } => mtu.to_string(),
                    WireGuardMtuProbe::Unavailable { message } => {
                        format!("unavailable({message})")
                    }
                };
                let endpoint_subnet = match &peer.endpoint_subnet {
                    WireGuardPeerEndpointSubnet::Missing => "unknown".to_owned(),
                    WireGuardPeerEndpointSubnet::Valid { subnet } => subnet.as_string(),
                    WireGuardPeerEndpointSubnet::Invalid { value, message } => {
                        format!("invalid({value}: {message})")
                    }
                };
                format!(
                    "  peer key={} subnet={} endpoint={} handshake-age={} rtt={} rx={} tx={} mtu-probe={}",
                    peer.public_key.as_str(),
                    endpoint_subnet,
                    endpoint,
                    handshake,
                    rtt,
                    peer.rx_bytes,
                    peer.tx_bytes,
                    mtu_probe,
                )
            }));
        }
    }
    match &machine.internal_dns {
        NetworkInternalDnsTestimony::NoAnswer => rows.push("  internal-dns no answer".to_owned()),
        NetworkInternalDnsTestimony::WrongResponder { actual_machine_id } => rows.push(format!(
            "  internal-dns wrong responder: {}",
            actual_machine_id.as_str()
        )),
        NetworkInternalDnsTestimony::TimedOut => {
            rows.push("  internal-dns timed out".to_owned());
        }
        NetworkInternalDnsTestimony::RequestFailed { message } => {
            rows.push(format!("  internal-dns request failed: {message}"));
        }
        NetworkInternalDnsTestimony::ProtocolFailed { message } => {
            rows.push(format!("  internal-dns protocol failed: {message}"));
        }
        NetworkInternalDnsTestimony::DecodeFailed { message } => {
            rows.push(format!("  internal-dns decode failed: {message}"));
        }
        NetworkInternalDnsTestimony::Answered { value } => {
            let resolver = match &value.resolver {
                InternalDnsResolverStatus::AwaitingBind { attempts } => {
                    format!("awaiting-bind(attempts={attempts})")
                }
                InternalDnsResolverStatus::Serving { bound } => format!("serving({bound})"),
                InternalDnsResolverStatus::NotConfigured => "not-configured".to_owned(),
            };
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let facts = value
                .fact_watermarks
                .iter()
                .map(|fact| {
                    format!(
                        "{}:{}s",
                        fact.machine_id.as_str(),
                        now_ms.saturating_sub(fact.observed_at_unix_ms) / 1_000
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            rows.push(format!(
                "  internal-dns {} fact-ages={}",
                resolver,
                if facts.is_empty() { "none" } else { &facts }
            ));
        }
    }
    rows
}

fn render_addresses(addresses: &[Ipv4Addr]) -> String {
    let rendered = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if rendered.is_empty() {
        "none".to_owned()
    } else {
        rendered
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkResolveOutput {
    pub result: NetworkResolveResult,
}

impl NetworkResolveOutput {
    #[must_use]
    pub const fn new(result: NetworkResolveResult) -> Self {
        Self { result }
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut answer_sets = self
            .result
            .machines
            .iter()
            .filter_map(|machine| match machine {
                NetworkResolveMachineTestimony::Answered { addresses, .. } => Some(addresses),
                NetworkResolveMachineTestimony::NoAnswer { .. }
                | NetworkResolveMachineTestimony::WrongResponder { .. }
                | NetworkResolveMachineTestimony::TimedOut { .. }
                | NetworkResolveMachineTestimony::RequestFailed { .. }
                | NetworkResolveMachineTestimony::ProtocolFailed { .. }
                | NetworkResolveMachineTestimony::DecodeFailed { .. } => None,
            })
            .map(Vec::as_slice);
        let consistent = answer_sets
            .next()
            .is_none_or(|first| answer_sets.all(|addresses| addresses == first));
        let summary = if consistent {
            "answer-sets consistent"
        } else {
            "answer-sets divergent"
        };
        render_rows(
            std::iter::once(summary.to_owned()).chain(self.result.machines.iter().map(|machine| {
                match machine {
                    NetworkResolveMachineTestimony::Answered {
                        machine_id,
                        addresses,
                    } => {
                        format!(
                            "machine {} {} A {}",
                            machine_id.as_str(),
                            self.result.name.as_str(),
                            render_addresses(addresses)
                        )
                    }
                    NetworkResolveMachineTestimony::NoAnswer { machine_id } => {
                        format!(
                            "machine {} {} no answer",
                            machine_id.as_str(),
                            self.result.name.as_str()
                        )
                    }
                    NetworkResolveMachineTestimony::WrongResponder {
                        machine_id,
                        actual_machine_id,
                    } => format!(
                        "machine {} {} wrong responder: {}",
                        machine_id.as_str(),
                        self.result.name.as_str(),
                        actual_machine_id.as_str()
                    ),
                    NetworkResolveMachineTestimony::TimedOut { machine_id } => format!(
                        "machine {} {} timed out",
                        machine_id.as_str(),
                        self.result.name.as_str()
                    ),
                    NetworkResolveMachineTestimony::RequestFailed {
                        machine_id,
                        message,
                    } => format!(
                        "machine {} {} request failed: {message}",
                        machine_id.as_str(),
                        self.result.name.as_str()
                    ),
                    NetworkResolveMachineTestimony::ProtocolFailed {
                        machine_id,
                        message,
                    } => format!(
                        "machine {} {} protocol failed: {message}",
                        machine_id.as_str(),
                        self.result.name.as_str()
                    ),
                    NetworkResolveMachineTestimony::DecodeFailed {
                        machine_id,
                        message,
                    } => format!(
                        "machine {} {} decode failed: {message}",
                        machine_id.as_str(),
                        self.result.name.as_str()
                    ),
                }
            })),
        )
    }
}

fn render_rows(rows: impl IntoIterator<Item = String>) -> String {
    let rendered = rows.into_iter().collect::<Vec<_>>().join("\n");
    if rendered.is_empty() {
        rendered
    } else {
        rendered + "\n"
    }
}
