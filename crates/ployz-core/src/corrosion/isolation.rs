//! Complete container-IP namespace projection for Keeper's isolation wall.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use super::{MachineEndpointDocument, ReadReport, SkippedRow, service_endpoint_key};
use crate::ids::{CorrosionNamespaceName, MachineName};
use crate::network::MachineEndpointSupernet;

/// One accepted `container_ip -> namespace` mapping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContainerIsolationEntry {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub ip: Ipv4Addr,
    pub namespace_id: CorrosionNamespaceName,
}

/// The complete desired map. Absence from this set is deliberately fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DesiredContainerIsolation {
    pub prefix: MachineEndpointSupernet,
    pub entries: Vec<ContainerIsolationEntry>,
}

/// Why an otherwise accepted container row did not enter the desired map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ContainerIsolationAddressRejection {
    OutsideClusterPrefix,
    ReservedHostOctet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedContainerIsolationAddress {
    pub endpoint_key: String,
    pub ip: Ipv4Addr,
    pub reason: ContainerIsolationAddressRejection,
}

/// A conflicting IP is omitted instead of choosing one namespace arbitrarily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationConflict {
    pub ip: Ipv4Addr,
    pub namespaces: Vec<CorrosionNamespaceName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationEvidence {
    pub observed_endpoints: usize,
    pub skipped_rows: Vec<SkippedRow>,
    pub rejected_addresses: Vec<RejectedContainerIsolationAddress>,
    pub conflicts: Vec<ContainerIsolationConflict>,
    pub coalesced_duplicates: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationProjection {
    pub desired: DesiredContainerIsolation,
    pub evidence: ContainerIsolationEvidence,
}

/// Projects only accepted machine endpoint testimony. Service intent, deploy
/// selection, and liveness state cannot change this fail-closed map's meaning.
#[must_use]
pub fn project_container_isolation(
    prefix: MachineEndpointSupernet,
    accepted_machines: &BTreeSet<MachineName>,
    report: ReadReport<MachineEndpointDocument>,
) -> ContainerIsolationProjection {
    let ReadReport {
        accepted,
        mut skipped,
    } = report;
    let accepted = accepted
        .into_iter()
        .filter_map(|row| {
            let machine_name = MachineName::try_new(row.source.key.clone()).ok()?;
            accepted_machines
                .contains(&machine_name)
                .then_some((machine_name, row.value))
        })
        .collect::<Vec<_>>();
    let observed_endpoints = accepted
        .iter()
        .map(|(_, testimony)| testimony.endpoints.len())
        .sum::<usize>()
        .saturating_add(skipped.len());
    let mut candidates = BTreeMap::<Ipv4Addr, Vec<CorrosionNamespaceName>>::new();
    let mut rejected_addresses = Vec::new();

    for (machine_name, testimony) in accepted {
        for endpoint in testimony.endpoints {
            let [_, _, _, host_octet] = endpoint.ip.octets();
            let reason = if !prefix.contains_ipv4(endpoint.ip) {
                Some(ContainerIsolationAddressRejection::OutsideClusterPrefix)
            } else if !(2..=254).contains(&host_octet) {
                Some(ContainerIsolationAddressRejection::ReservedHostOctet)
            } else {
                None
            };
            if let Some(reason) = reason {
                rejected_addresses.push(RejectedContainerIsolationAddress {
                    endpoint_key: service_endpoint_key(&endpoint, &machine_name),
                    ip: endpoint.ip,
                    reason,
                });
                continue;
            }
            candidates
                .entry(endpoint.ip)
                .or_default()
                .push(endpoint.namespace_id);
        }
    }

    let mut entries = Vec::new();
    let mut conflicts = Vec::new();
    let mut coalesced_duplicates = 0;
    for (ip, mut namespaces) in candidates {
        namespaces.sort();
        let original_count = namespaces.len();
        namespaces.dedup();
        coalesced_duplicates += original_count - namespaces.len();
        match namespaces.as_slice() {
            [namespace_id] => entries.push(ContainerIsolationEntry {
                ip,
                namespace_id: namespace_id.clone(),
            }),
            [_, _, ..] => conflicts.push(ContainerIsolationConflict { ip, namespaces }),
            [] => unreachable!("every candidate has a namespace"),
        }
    }
    rejected_addresses.sort_by(|left, right| {
        left.ip
            .cmp(&right.ip)
            .then_with(|| left.endpoint_key.cmp(&right.endpoint_key))
    });
    skipped.sort_by(|left, right| left.source.key.cmp(&right.source.key));

    ContainerIsolationProjection {
        desired: DesiredContainerIsolation { prefix, entries },
        evidence: ContainerIsolationEvidence {
            observed_endpoints,
            skipped_rows: skipped,
            rejected_addresses,
            conflicts,
            coalesced_duplicates,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corrosion::{
        AcceptedRow, CorrosionDocumentVersion, CorrosionServiceName, CorrosionTimestamp,
        ServiceEndpoint, StoredRow,
    };
    use crate::ids::{ClusterName, CorrosionNamespaceName, DeployName, MachineName};

    const CLUSTER: &str = "main";
    const MACHINE: &str = "edge-a";
    const SERVICE: &str = "web";
    const DEPLOY: &str = "release-1";
    const NAMESPACE_A: &str = "production";
    const NAMESPACE_B: &str = "staging";

    fn row(ip: &str, namespace: &str) -> AcceptedRow<MachineEndpointDocument> {
        AcceptedRow {
            source: StoredRow::new(MACHINE, "accepted"),
            value: MachineEndpointDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: ClusterName::try_new(CLUSTER).expect("cluster"),
                observed_at: CorrosionTimestamp::try_new("2026-08-10T00:00:00Z")
                    .expect("timestamp"),
                endpoints: vec![ServiceEndpoint {
                    namespace_id: CorrosionNamespaceName::try_new(namespace).expect("namespace"),
                    service_name: CorrosionServiceName::try_new(SERVICE).expect("service"),
                    replica_slot: crate::deploy::ReplicaSlot::Global,
                    ip: ip.parse().expect("ip"),
                    deploy: DeployName::try_new(DEPLOY).expect("deploy"),
                }],
            },
        }
    }

    #[test]
    fn projection_sorts_coalesces_and_omits_conflicts() {
        let report = ReadReport {
            accepted: vec![
                row("10.77.2.4", NAMESPACE_A),
                row("10.77.2.3", NAMESPACE_A),
                row("10.77.2.3", NAMESPACE_B),
                row("10.77.2.4", NAMESPACE_A),
                row("10.77.2.2", NAMESPACE_B),
                row("10.77.2.254", NAMESPACE_A),
            ],
            skipped: Vec::new(),
        };

        let projection = project_container_isolation(
            MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
            &BTreeSet::from([MachineName::try_new(MACHINE).expect("machine")]),
            report,
        );

        assert_eq!(
            projection
                .desired
                .entries
                .iter()
                .map(|entry| entry.ip)
                .collect::<Vec<_>>(),
            vec![
                "10.77.2.2".parse::<Ipv4Addr>().expect("first ip"),
                "10.77.2.4".parse::<Ipv4Addr>().expect("second ip"),
                "10.77.2.254".parse::<Ipv4Addr>().expect("last host ip"),
            ]
        );
        assert_eq!(projection.evidence.coalesced_duplicates, 1);
        assert_eq!(projection.evidence.conflicts.len(), 1);
        let [conflict] = projection.evidence.conflicts.as_slice() else {
            panic!("one conflict");
        };
        assert_eq!(
            conflict.ip,
            "10.77.2.3".parse::<Ipv4Addr>().expect("conflict ip")
        );
    }

    #[test]
    fn projection_rejects_outside_prefix_and_reserved_host_octets() {
        let projection = project_container_isolation(
            MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
            &BTreeSet::from([MachineName::try_new(MACHINE).expect("machine")]),
            ReadReport {
                accepted: vec![
                    row("10.77.9.0", NAMESPACE_A),
                    row("10.77.9.1", NAMESPACE_A),
                    row("10.77.9.255", NAMESPACE_A),
                    row("10.78.9.2", NAMESPACE_A),
                ],
                skipped: Vec::new(),
            },
        );

        assert!(projection.desired.entries.is_empty());
        let [network, gateway, broadcast, foreign] =
            projection.evidence.rejected_addresses.as_slice()
        else {
            panic!("four rejected addresses");
        };
        assert_eq!(
            network.reason,
            ContainerIsolationAddressRejection::ReservedHostOctet
        );
        assert_eq!(
            gateway.reason,
            ContainerIsolationAddressRejection::ReservedHostOctet
        );
        assert_eq!(
            foreign.reason,
            ContainerIsolationAddressRejection::OutsideClusterPrefix
        );
        assert_eq!(
            broadcast.reason,
            ContainerIsolationAddressRejection::ReservedHostOctet
        );
    }

    #[test]
    fn projection_ignores_endpoint_testimony_outside_the_local_roster() {
        let projection = project_container_isolation(
            MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
            &BTreeSet::new(),
            ReadReport {
                accepted: vec![row("10.77.2.2", NAMESPACE_A)],
                skipped: Vec::new(),
            },
        );

        assert!(projection.desired.entries.is_empty());
        assert_eq!(projection.evidence.observed_endpoints, 0);
    }
}
