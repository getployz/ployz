//! Validation of accepted machine and peer join material.

use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, MachineDocument, MachineTransport, MeshProvider, PeerDocument,
    derive_builtin_wireguard_member,
};
use crate::ids::MachineName;

use super::JoinAcceptanceValidationError;
use super::admission::{MachineJoinRequest, PeerJoinRequest};
use super::substrate::JoinMachineSubstrate;
use super::token::{JoinDoorCertFingerprint, JoinDoorMaterial};

/// A current roster machine the joiner can reach before Corrosion has synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReachableSeedMachine {
    pub machine_name: MachineName,
    pub transport: MachineTransport,
}

impl ReachableSeedMachine {
    pub fn try_new(
        machine_name: MachineName,
        transport: MachineTransport,
    ) -> Result<Self, JoinAcceptanceValidationError> {
        if matches!(
            transport,
            MachineTransport::Wireguard { endpoint: None, .. }
        ) {
            return Err(JoinAcceptanceValidationError::SeedIsNotReachable);
        }
        Ok(Self {
            machine_name,
            transport,
        })
    }
}

/// Corrosion facts that let a new database join before it owns roster state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CorrosionBootstrapFacts {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub seed_gossip_address: SocketAddr,
}

impl CorrosionBootstrapFacts {
    pub fn try_new(seed_gossip_address: SocketAddr) -> Result<Self, JoinAcceptanceValidationError> {
        if seed_gossip_address.port() == 0 {
            return Err(JoinAcceptanceValidationError::InvalidCorrosionSeed);
        }
        Ok(Self {
            seed_gossip_address,
        })
    }
}

/// Variant-specific accepted machine material; peers cannot carry these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineJoinAccepted {
    pub cluster: ClusterDocument,
    pub machine: MachineDocument,
    pub seed: ReachableSeedMachine,
    pub door: JoinDoorMaterial,
    pub corrosion: CorrosionBootstrapFacts,
    pub substrate: JoinMachineSubstrate,
}

impl MachineJoinAccepted {
    pub fn try_validate(
        self,
        request: &MachineJoinRequest,
        expected_fingerprint: &JoinDoorCertFingerprint,
    ) -> Result<ValidatedMachineJoinAccepted, JoinAcceptanceValidationError> {
        self.substrate.validate()?;
        if self.cluster.provider != MeshProvider::BuiltinWireguard {
            return Err(JoinAcceptanceValidationError::ProviderMismatch);
        }
        if self.machine.cluster_id != self.cluster.cluster_id {
            return Err(JoinAcceptanceValidationError::ClusterMismatch);
        }
        if self.machine.name != request.name {
            return Err(JoinAcceptanceValidationError::AcceptedNameMismatch);
        }
        let MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
            subnet_v4,
        } = &self.machine.transport
        else {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        };
        if pubkey != &request.public_key || endpoint != &request.endpoint {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        }
        if derive_builtin_wireguard_member(&self.cluster.cluster_id, pubkey)
            .bind_address()
            .get()
            != *addr_v6
        {
            return Err(JoinAcceptanceValidationError::AcceptedAddressMismatch);
        }
        if !self.cluster.prefix.contains_subnet(subnet_v4) {
            return Err(JoinAcceptanceValidationError::EndpointSubnetOutsideClusterPrefix);
        }
        if &self.door.fingerprint != expected_fingerprint {
            return Err(JoinAcceptanceValidationError::DoorFingerprintMismatch);
        }
        validate_seed(
            &self.cluster,
            &self.seed,
            Some(&request.name),
            &self.corrosion,
        )?;
        Ok(ValidatedMachineJoinAccepted(self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMachineJoinAccepted(MachineJoinAccepted);

impl ValidatedMachineJoinAccepted {
    #[must_use]
    pub const fn accepted(&self) -> &MachineJoinAccepted {
        &self.0
    }

    #[must_use]
    pub fn into_accepted(self) -> MachineJoinAccepted {
        self.0
    }
}

/// Variant-specific accepted peer material; no storage or `/24` can appear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerJoinAccepted {
    pub cluster: ClusterDocument,
    pub peer: PeerDocument,
    pub seed: ReachableSeedMachine,
    pub corrosion: CorrosionBootstrapFacts,
}

impl PeerJoinAccepted {
    pub fn try_validate(
        self,
        request: &PeerJoinRequest,
    ) -> Result<ValidatedPeerJoinAccepted, JoinAcceptanceValidationError> {
        if self.cluster.provider != MeshProvider::BuiltinWireguard {
            return Err(JoinAcceptanceValidationError::ProviderMismatch);
        }
        if self.peer.cluster_id != self.cluster.cluster_id {
            return Err(JoinAcceptanceValidationError::ClusterMismatch);
        }
        if self.peer.name != request.name {
            return Err(JoinAcceptanceValidationError::AcceptedNameMismatch);
        }
        let crate::corrosion::PeerTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
        } = &self.peer.transport
        else {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        };
        if pubkey != &request.public_key || endpoint != &request.endpoint {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        }
        if derive_builtin_wireguard_member(&self.cluster.cluster_id, pubkey)
            .bind_address()
            .get()
            != *addr_v6
        {
            return Err(JoinAcceptanceValidationError::AcceptedAddressMismatch);
        }
        validate_seed(&self.cluster, &self.seed, None, &self.corrosion)?;
        Ok(ValidatedPeerJoinAccepted(self))
    }
}

fn validate_seed(
    cluster: &ClusterDocument,
    seed: &ReachableSeedMachine,
    joining_machine_id: Option<&MachineName>,
    corrosion: &CorrosionBootstrapFacts,
) -> Result<(), JoinAcceptanceValidationError> {
    ReachableSeedMachine::try_new(seed.machine_name.clone(), seed.transport.clone())?;
    if joining_machine_id.is_some_and(|joining| joining == &seed.machine_name) {
        return Err(JoinAcceptanceValidationError::SeedIsJoiningMachine);
    }
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint: Some(_),
        subnet_v4,
    } = &seed.transport
    else {
        return Err(JoinAcceptanceValidationError::SeedProviderMismatch);
    };
    if derive_builtin_wireguard_member(&cluster.cluster_id, pubkey)
        .bind_address()
        .get()
        != *addr_v6
    {
        return Err(JoinAcceptanceValidationError::SeedAddressMismatch);
    }
    if !cluster.prefix.contains_subnet(subnet_v4) {
        return Err(JoinAcceptanceValidationError::SeedSubnetOutsideClusterPrefix);
    }
    CorrosionBootstrapFacts::try_new(corrosion.seed_gossip_address)?;
    if corrosion.seed_gossip_address.ip() != IpAddr::V6(*addr_v6) {
        return Err(JoinAcceptanceValidationError::CorrosionSeedDoesNotMatchMachine);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPeerJoinAccepted(PeerJoinAccepted);

impl ValidatedPeerJoinAccepted {
    #[must_use]
    pub const fn accepted(&self) -> &PeerJoinAccepted {
        &self.0
    }

    #[must_use]
    pub fn into_accepted(self) -> PeerJoinAccepted {
        self.0
    }
}
