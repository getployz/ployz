//! Request principals and accepted-roster source-address resolution.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::document::{MachineTransport, PeerTransport};
use crate::ids::{MachineName, PeerName, TokenName};

/// The authenticated identity of a request or durable write provenance.
///
/// Authentication resolves an address against accepted roster winners. Durable
/// provenance records the resulting value but never participates in a later
/// authentication decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Principal {
    Machine { machine_id: MachineName },
    Peer { peer_id: PeerName },
    ApiToken { token_id: TokenName },
}

/// The durable-provenance name for [`Principal`].
///
/// The alias keeps durable-write callers and request authentication on one
/// wire union.
pub type OperationInitiator = Principal;

/// An accepted, provider-fenced roster winner eligible for source-address
/// resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptedRosterPrincipal {
    Machine {
        machine_id: MachineName,
        transport: MachineTransport,
    },
    Peer {
        peer_id: PeerName,
        transport: PeerTransport,
    },
}

impl AcceptedRosterPrincipal {
    /// Creates a machine candidate from an accepted named machines-row winner.
    #[must_use]
    pub fn machine(machine_id: MachineName, transport: MachineTransport) -> Self {
        Self::Machine {
            machine_id,
            transport,
        }
    }

    /// Creates a peer candidate from an accepted named peers-row winner.
    #[must_use]
    pub fn peer(peer_id: PeerName, transport: PeerTransport) -> Self {
        Self::Peer { peer_id, transport }
    }

    fn principal(&self) -> Principal {
        match self {
            Self::Machine { machine_id, .. } => Principal::Machine {
                machine_id: machine_id.clone(),
            },
            Self::Peer { peer_id, .. } => Principal::Peer {
                peer_id: peer_id.clone(),
            },
        }
    }

    fn matches_source(&self, source: IpAddr) -> bool {
        match (self, source) {
            (
                Self::Machine {
                    transport: MachineTransport::Wireguard { addr_v6, .. },
                    ..
                }
                | Self::Peer {
                    transport: PeerTransport::Wireguard { addr_v6, .. },
                    ..
                },
                IpAddr::V6(source),
            ) => source == *addr_v6,
            (
                Self::Machine {
                    transport: MachineTransport::Tailscale { ip, .. },
                    ..
                }
                | Self::Peer {
                    transport: PeerTransport::Tailscale { ip },
                    ..
                },
                IpAddr::V4(source),
            ) => source == *ip,
            _ => false,
        }
    }
}

/// Why accepted roster winners did not resolve a source address to one principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourcePrincipalResolutionError {
    UnknownSource {
        source: IpAddr,
    },
    AmbiguousSource {
        source: IpAddr,
        candidate_count: usize,
    },
}

/// Resolves a mesh source address using only accepted, provider-fenced roster
/// winners supplied by the daemon.
///
/// Endpoint and container-subnet fields are intentionally not candidates for
/// identity. A caller is authenticated only when exactly one roster candidate
/// claims the source address.
pub fn resolve_source_principal<'candidate>(
    source: IpAddr,
    accepted_roster: impl IntoIterator<Item = &'candidate AcceptedRosterPrincipal>,
) -> Result<Principal, SourcePrincipalResolutionError> {
    let matches = accepted_roster
        .into_iter()
        .filter(|candidate| candidate.matches_source(source))
        .map(AcceptedRosterPrincipal::principal)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Err(SourcePrincipalResolutionError::UnknownSource { source }),
        [principal] => Ok(principal.clone()),
        candidates => Err(SourcePrincipalResolutionError::AmbiguousSource {
            source,
            candidate_count: candidates.len(),
        }),
    }
}
