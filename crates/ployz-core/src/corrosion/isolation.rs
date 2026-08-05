//! Complete container-IP namespace projection for Keeper's isolation wall.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};

use super::{ContainerDocument, ReadReport, SkippedRow};
use crate::ids::NamespaceRowId;
use crate::network::MachineEndpointSupernet;

/// One accepted `container_ip -> namespace` mapping.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContainerIsolationEntry {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub ip: Ipv4Addr,
    pub namespace_id: NamespaceRowId,
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
    pub row_key: String,
    pub ip: Ipv4Addr,
    pub reason: ContainerIsolationAddressRejection,
}

/// A conflicting IP is omitted instead of choosing one namespace arbitrarily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationConflict {
    pub ip: Ipv4Addr,
    pub namespaces: Vec<NamespaceRowId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerIsolationEvidence {
    pub observed_rows: usize,
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

/// Projects only accepted container rows. Service, deploy, and liveness state
/// cannot change this map's meaning.
#[must_use]
pub fn project_container_isolation(
    prefix: MachineEndpointSupernet,
    report: ReadReport<ContainerDocument>,
) -> ContainerIsolationProjection {
    let ReadReport {
        accepted,
        mut skipped,
    } = report;
    let observed_rows = accepted.len().saturating_add(skipped.len());
    let mut candidates = BTreeMap::<Ipv4Addr, Vec<NamespaceRowId>>::new();
    let mut rejected_addresses = Vec::new();

    for row in accepted {
        let [_, _, _, host_octet] = row.value.ip.octets();
        let reason = if !prefix.contains_ipv4(row.value.ip) {
            Some(ContainerIsolationAddressRejection::OutsideClusterPrefix)
        } else if !(2..=254).contains(&host_octet) {
            Some(ContainerIsolationAddressRejection::ReservedHostOctet)
        } else {
            None
        };
        if let Some(reason) = reason {
            rejected_addresses.push(RejectedContainerIsolationAddress {
                row_key: row.source.key,
                ip: row.value.ip,
                reason,
            });
            continue;
        }
        candidates
            .entry(row.value.ip)
            .or_default()
            .push(row.value.namespace_id);
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
            .then_with(|| left.row_key.cmp(&right.row_key))
    });
    skipped.sort_by(|left, right| left.source.key.cmp(&right.source.key));

    ContainerIsolationProjection {
        desired: DesiredContainerIsolation { prefix, entries },
        evidence: ContainerIsolationEvidence {
            observed_rows,
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
    use crate::corrosion::{AcceptedRow, CorrosionDocumentVersion, StoredRow};
    use crate::ids::{ClusterId, MachineRowId, OperationRowId, ServiceRowId};

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const DEPLOY: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const NAMESPACE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    const NAMESPACE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";

    fn row(key: &str, ip: &str, namespace: &str) -> AcceptedRow<ContainerDocument> {
        AcceptedRow {
            source: StoredRow::new(key, "accepted"),
            value: ContainerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: ClusterId::try_new(CLUSTER).expect("cluster"),
                machine_id: MachineRowId::try_new(MACHINE).expect("machine"),
                service_id: ServiceRowId::try_new(SERVICE).expect("service"),
                namespace_id: NamespaceRowId::try_new(namespace).expect("namespace"),
                ip: ip.parse().expect("ip"),
                deploy: OperationRowId::try_new(DEPLOY).expect("deploy"),
            },
        }
    }

    #[test]
    fn projection_sorts_coalesces_and_omits_conflicts() {
        let report = ReadReport {
            accepted: vec![
                row("four", "10.77.2.4", NAMESPACE_A),
                row("three-a", "10.77.2.3", NAMESPACE_A),
                row("three-b", "10.77.2.3", NAMESPACE_B),
                row("four-duplicate", "10.77.2.4", NAMESPACE_A),
                row("two", "10.77.2.2", NAMESPACE_B),
                row("last-host", "10.77.2.254", NAMESPACE_A),
            ],
            skipped: Vec::new(),
        };

        let projection = project_container_isolation(
            MachineEndpointSupernet::try_new("10.77.0.0/16").expect("prefix"),
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
            ReadReport {
                accepted: vec![
                    row("network", "10.77.9.0", NAMESPACE_A),
                    row("gateway", "10.77.9.1", NAMESPACE_A),
                    row("broadcast", "10.77.9.255", NAMESPACE_A),
                    row("foreign", "10.78.9.2", NAMESPACE_A),
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
}
