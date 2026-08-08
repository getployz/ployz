//! Ployz-managed DNS target allocation.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::time::Duration;

use ployz_core::certificate::{
    MANAGED_LEASE_DOMAIN_SUFFIX, ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
};
use ployz_core::corrosion::{
    ClusterDocument, MachineTransport, OperatorWriteProvenance, PloyzDnsTargetState,
    SqliteParameter, Statement, TransactionResult,
};
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::operation::RouteHostname;
use sha2::{Digest, Sha256};

use super::super::store::{AcceptedCluster, read_accepted_roster, read_cluster};
use super::error::AutomaticRouteBindingError;
use crate::lease::{LeaseClient, LeaseClientError, LeaseWorkerOrigin, load_or_create_token};

const LEASE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_LEASE_RESPONSE_BYTES: usize = 64 * 1024;
const LEASE_ALLOCATION_ATTEMPTS: usize = 3;

pub(super) struct ManagedTargetAllocator {
    corrosion: crate::corrosion::CorrosionClient,
    cluster_id: ClusterId,
    machine_id: MachineRowId,
    worker: LeaseClient,
    token_path: PathBuf,
}

impl ManagedTargetAllocator {
    pub(super) fn new(
        corrosion: crate::corrosion::CorrosionClient,
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        worker_origin: LeaseWorkerOrigin,
        token_path: PathBuf,
    ) -> Result<Self, LeaseClientError> {
        Ok(Self {
            corrosion,
            cluster_id,
            machine_id,
            worker: LeaseClient::new(
                worker_origin,
                LEASE_ACQUIRE_TIMEOUT,
                MAX_LEASE_RESPONSE_BYTES,
            )?,
            token_path,
        })
    }

    /// Converges only the managed target decision. Automatic hostname policy
    /// is interpreted by the deploy-binding module, not by this allocator.
    pub(super) async fn settle_cluster(
        &self,
        provenance: &OperatorWriteProvenance,
    ) -> Result<AcceptedCluster, AutomaticRouteBindingError> {
        for _ in 0..LEASE_ALLOCATION_ATTEMPTS {
            let incumbent = read_cluster(&self.corrosion, &self.cluster_id).await?;
            match &incumbent.document.ployz_dns_target {
                PloyzDnsTargetState::Allocated { .. } | PloyzDnsTargetState::Disabled => {
                    return Ok(incumbent);
                }
                PloyzDnsTargetState::Pending => {}
            }

            let addresses = self.roster_addresses().await?;
            if addresses.ipv4.is_empty() && addresses.ipv6.is_empty() {
                return Err(AutomaticRouteBindingError::NoRosterEndpoints);
            }
            let acquired = self
                .worker
                .acquire(ManagedLeaseAcquireRequest {
                    acquisition_id: acquisition_id(&self.cluster_id)?,
                    token: load_or_create_token(&self.token_path)?,
                    ipv4: addresses.ipv4,
                    ipv6: addresses.ipv6,
                })
                .await?;
            let hostname = RouteHostname::try_new(format!(
                "{}.{}",
                acquired.lease.name.as_str(),
                MANAGED_LEASE_DOMAIN_SUFFIX
            ))?;
            let replacement = allocation_document(
                incumbent.document,
                hostname,
                self.machine_id.clone(),
                provenance.clone(),
            );
            self.write_allocation(&replacement).await?;
        }

        let observed = read_cluster(&self.corrosion, &self.cluster_id).await?;
        if matches!(
            observed.document.ployz_dns_target,
            PloyzDnsTargetState::Allocated { .. }
        ) {
            Ok(observed)
        } else {
            Err(AutomaticRouteBindingError::AllocationUnsettled)
        }
    }

    async fn roster_addresses(&self) -> Result<RosterAddresses, AutomaticRouteBindingError> {
        let roster = read_accepted_roster(&self.corrosion, &self.cluster_id).await?;
        let mut ipv4 = BTreeSet::new();
        let mut ipv6 = BTreeSet::new();
        for machine in roster.machines {
            let address = match machine.document.transport {
                MachineTransport::Wireguard { endpoint, .. } => endpoint.map(|value| value.ip()),
                MachineTransport::Tailscale { ip, .. } => Some(IpAddr::V4(ip)),
            };
            match address {
                Some(IpAddr::V4(address)) => {
                    ipv4.insert(address);
                }
                Some(IpAddr::V6(address)) => {
                    ipv6.insert(address);
                }
                None => {}
            }
        }
        Ok(RosterAddresses {
            ipv4: ipv4.into_iter().collect(),
            ipv6: ipv6.into_iter().collect(),
        })
    }

    /// The stable acquisition id makes concurrent lease requests converge.
    /// Corrosion's LWW document is written normally and then read back; a
    /// local affected-row count is not cluster-wide authority.
    async fn write_allocation(
        &self,
        replacement: &ClusterDocument,
    ) -> Result<(), AutomaticRouteBindingError> {
        let replacement = serde_json::to_string(replacement)
            .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))?;
        let response = self
            .corrosion
            .execute(&[Statement::with_params(
                "UPDATE cluster SET document = ? WHERE id = ?",
                vec![
                    SqliteParameter::Text(replacement),
                    SqliteParameter::Text(self.cluster_id.as_str().to_owned()),
                ],
            )])
            .await?;
        let [TransactionResult::Success(_)] = response.results.as_slice() else {
            return Err(AutomaticRouteBindingError::Protocol(
                "cluster allocation write returned an invalid result".to_owned(),
            ));
        };
        Ok(())
    }
}

fn allocation_document(
    mut incumbent: ClusterDocument,
    hostname: RouteHostname,
    acquired_by: MachineRowId,
    provenance: OperatorWriteProvenance,
) -> ClusterDocument {
    incumbent.provenance = provenance;
    incumbent.ployz_dns_target = PloyzDnsTargetState::Allocated {
        hostname,
        acquired_by,
    };
    incumbent
}

fn acquisition_id(
    cluster_id: &ClusterId,
) -> Result<ManagedLeaseAcquisitionId, AutomaticRouteBindingError> {
    let digest = Sha256::digest(cluster_id.as_str().as_bytes());
    ManagedLeaseAcquisitionId::try_new(format!("{digest:x}"))
        .map_err(|error| AutomaticRouteBindingError::Protocol(error.to_string()))
}

struct RosterAddresses {
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionTimestamp, OperatorWriteProvenance,
        PloyzDnsTargetState, Principal,
    };
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
    use ployz_core::operation::RouteHostname;

    use super::{acquisition_id, allocation_document};

    #[test]
    fn worker_acquisition_identity_is_stable_hex_without_a_token() {
        let cluster_id = ClusterId::try_new("01J00000000000000000000010").expect("cluster");
        let first = acquisition_id(&cluster_id).expect("acquisition id");
        let second = acquisition_id(&cluster_id).expect("acquisition id");

        assert_eq!(first, second);
        assert_eq!(first.as_str().len(), 64);
        assert!(first.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn allocation_replaces_provenance_without_changing_hostname_policy() {
        let incumbent: ClusterDocument = serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": "01J00000000000000000000010",
            "written_by": { "kind": "peer", "peer_id": "01J00000000000000000000011" },
            "written_at": "2026-08-07T00:00:00Z",
            "name": "test",
            "storage_default": "plain",
            "hostname_mode": { "mode": "custom", "suffix": "apps.example.test" },
            "ployz_dns_target": { "state": "pending" },
            "prefix": "10.210.0.0/16",
            "provider": "builtin_wireguard",
            "acme_directory_url": "https://acme.example/directory",
            "acme_contact": null
        }))
        .expect("cluster");
        let writer = OperatorWriteProvenance {
            written_by: Principal::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000012").expect("peer"),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("timestamp"),
        };
        let hostname = RouteHostname::try_new("lease.up.ployz.app").expect("hostname");
        let machine = MachineRowId::try_new("01J00000000000000000000013").expect("machine");

        let replacement =
            allocation_document(incumbent, hostname.clone(), machine.clone(), writer.clone());

        assert!(matches!(
            replacement.hostname_mode,
            AutomaticHostnameMode::Custom { .. }
        ));
        assert_eq!(replacement.provenance, writer);
        assert_eq!(
            replacement.ployz_dns_target,
            PloyzDnsTargetState::Allocated {
                hostname,
                acquired_by: machine,
            }
        );
    }
}
