//! Bootstrap-only admission of machine one's initial authority rows.

use std::time::Duration;

use ployz_core::corrosion::{
    CorrosionTable, MachineTransport, SqliteParameter, Statement, StoredRow,
};
use ployz_core::founding::{FoundingResult, ValidatedFoundingRequest};
use ployz_core::network::MachineEndpointSubnet;
use serde::Serialize;

use crate::WireGuardMtuPolicy;
use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{MachineContainerRunner, MachineEndpointNetworkError};

const MAX_FOUNDING_ROWS_PER_TABLE: usize = 1;
const ENDPOINT_NETWORK_TIMEOUT: Duration = Duration::from_secs(30);
const ENDPOINT_BRIDGE_IFNAME: &str = "br-ployz";
const WIREGUARD_IFNAME: &str = "ployz0";

pub(super) struct FoundingWrite {
    pub(super) result: FoundingResult,
    pub(super) machine_subnet: MachineEndpointSubnet,
}

pub(super) async fn write_initial_rows(
    corrosion: &CorrosionClient,
    validated: &ValidatedFoundingRequest,
) -> Result<FoundingWrite, FoundingWriteError> {
    let request = validated.request();
    let before = read_initial_rows(corrosion).await?;
    let plan = FoundingPlan::build(validated, &before)?;
    let result = plan.result;
    if !plan.statements.is_empty() {
        corrosion.execute(&plan.statements).await?;
    }

    let after = read_initial_rows(corrosion).await?;
    FoundingPlan::verify_complete(validated, &after)?;
    let MachineTransport::Wireguard { subnet_v4, .. } = &request.machine.transport else {
        unreachable!("validated founding requests use builtin WireGuard")
    };
    Ok(FoundingWrite {
        result,
        machine_subnet: subnet_v4.clone(),
    })
}

pub(super) async fn ensure_endpoint_network(
    subnet: &MachineEndpointSubnet,
) -> Result<(), FoundingEndpointNetworkError> {
    let runner = DockerManagedContainerRunner::lazy_local_defaults(
        subnet.as_string(),
        ENDPOINT_BRIDGE_IFNAME.to_owned(),
        WIREGUARD_IFNAME.to_owned(),
        WireGuardMtuPolicy::Auto,
    );
    tokio::time::timeout(
        ENDPOINT_NETWORK_TIMEOUT,
        runner.ensure_projection_endpoint_network(subnet),
    )
    .await
    .map_err(|_| FoundingEndpointNetworkError::Timeout {
        timeout: ENDPOINT_NETWORK_TIMEOUT,
    })?
    .map_err(FoundingEndpointNetworkError::Ensure)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FoundingRows {
    cluster: Vec<StoredRow>,
    machines: Vec<StoredRow>,
    peers: Vec<StoredRow>,
}

async fn read_initial_rows(
    corrosion: &CorrosionClient,
) -> Result<FoundingRows, FoundingWriteError> {
    let cluster = query_table(corrosion, CorrosionTable::Cluster);
    let machines = query_table(corrosion, CorrosionTable::Machines);
    let peers = query_table(corrosion, CorrosionTable::Peers);
    let (cluster, machines, peers) = tokio::try_join!(cluster, machines, peers)?;
    Ok(FoundingRows {
        cluster,
        machines,
        peers,
    })
}

async fn query_table(
    corrosion: &CorrosionClient,
    table: CorrosionTable,
) -> Result<Vec<StoredRow>, FoundingWriteError> {
    let mut stream = corrosion
        .query(&Statement::simple(format!(
            "SELECT id, document FROM {}",
            table.as_str()
        )))
        .await?;
    collect_stored_rows(
        &mut stream,
        StoredRowLimit::new(MAX_FOUNDING_ROWS_PER_TABLE),
    )
    .await
    .map_err(FoundingWriteError::Rows)
}

struct FoundingPlan {
    result: FoundingResult,
    statements: Vec<Statement>,
}

impl FoundingPlan {
    fn build(
        validated: &ValidatedFoundingRequest,
        existing: &FoundingRows,
    ) -> Result<Self, FoundingWriteError> {
        let expected = ExpectedRows::new(validated)?;
        let cluster = expected.disposition(&existing.cluster, &expected.cluster)?;
        let machine = expected.disposition(&existing.machines, &expected.machine)?;
        let peer = match &expected.peer {
            Some(peer) => Some(expected.disposition(&existing.peers, peer)?),
            None if existing.peers.is_empty() => None,
            None => return Err(FoundingWriteError::ExistingRowsMismatch),
        };
        let existing_count = usize::from(cluster == RowDisposition::Exact)
            + usize::from(machine == RowDisposition::Exact)
            + usize::from(peer == Some(RowDisposition::Exact));
        let expected_count = 2 + usize::from(expected.peer.is_some());
        let result = if existing_count == 0 {
            FoundingResult::Found
        } else if existing_count == expected_count {
            FoundingResult::NoOp
        } else {
            FoundingResult::Resumed
        };
        let mut statements = Vec::with_capacity(expected_count - existing_count);
        if cluster == RowDisposition::Missing {
            statements.push(expected.cluster.insert_statement());
        }
        if machine == RowDisposition::Missing {
            statements.push(expected.machine.insert_statement());
        }
        if peer == Some(RowDisposition::Missing) {
            let Some(peer) = &expected.peer else {
                unreachable!("a missing peer disposition has an expected peer row")
            };
            statements.push(peer.insert_statement());
        }
        Ok(Self { result, statements })
    }

    fn verify_complete(
        validated: &ValidatedFoundingRequest,
        existing: &FoundingRows,
    ) -> Result<(), FoundingWriteError> {
        let plan = Self::build(validated, existing)?;
        if plan.result == FoundingResult::NoOp {
            Ok(())
        } else {
            Err(FoundingWriteError::ReadbackIncomplete)
        }
    }
}

struct ExpectedRows {
    cluster: ExpectedRow,
    machine: ExpectedRow,
    peer: Option<ExpectedRow>,
}

impl ExpectedRows {
    fn new(validated: &ValidatedFoundingRequest) -> Result<Self, FoundingWriteError> {
        let request = validated.request();
        let peer = request
            .driver
            .enrolled_peer()
            .map(|(id, document)| ExpectedRow::new(CorrosionTable::Peers, id.as_str(), document));
        Ok(Self {
            cluster: ExpectedRow::new(
                CorrosionTable::Cluster,
                request.cluster_id.as_str(),
                &request.cluster,
            )?,
            machine: ExpectedRow::new(
                CorrosionTable::Machines,
                request.machine_id.as_str(),
                &request.machine,
            )?,
            peer: peer.transpose()?,
        })
    }

    fn disposition(
        &self,
        stored: &[StoredRow],
        expected: &ExpectedRow,
    ) -> Result<RowDisposition, FoundingWriteError> {
        match stored {
            [] => Ok(RowDisposition::Missing),
            [row] if row.key == expected.key => {
                let value = serde_json::from_str::<serde_json::Value>(&row.document)
                    .map_err(|_| FoundingWriteError::ExistingRowsMismatch)?;
                if value == expected.document {
                    Ok(RowDisposition::Exact)
                } else {
                    Err(FoundingWriteError::ExistingRowsMismatch)
                }
            }
            [_] | [_, ..] => Err(FoundingWriteError::ExistingRowsMismatch),
        }
    }
}

struct ExpectedRow {
    table: CorrosionTable,
    key: String,
    document: serde_json::Value,
    encoded: String,
}

impl ExpectedRow {
    fn new(
        table: CorrosionTable,
        key: &str,
        document: &impl Serialize,
    ) -> Result<Self, FoundingWriteError> {
        let document = serde_json::to_value(document).map_err(FoundingWriteError::Encode)?;
        let encoded = serde_json::to_string(&document).map_err(FoundingWriteError::Encode)?;
        Ok(Self {
            table,
            key: key.to_owned(),
            document,
            encoded,
        })
    }

    fn insert_statement(&self) -> Statement {
        Statement::with_params(
            format!(
                "INSERT INTO {} (id, document) VALUES (?, ?)",
                self.table.as_str()
            ),
            vec![
                SqliteParameter::Text(self.key.clone()),
                SqliteParameter::Text(self.encoded.clone()),
            ],
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowDisposition {
    Missing,
    Exact,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FoundingWriteError {
    #[error("could not encode a validated founding row: {0}")]
    Encode(serde_json::Error),
    #[error("local Corrosion request failed: {0}")]
    Client(#[from] CorrosionClientError),
    #[error("local Corrosion founding query was malformed: {0}")]
    Rows(StoredRowCollectionError),
    #[error("existing Corrosion rows do not exactly match this founding request")]
    ExistingRowsMismatch,
    #[error("the founding transaction did not become visible on readback")]
    ReadbackIncomplete,
}

impl FoundingWriteError {
    pub(super) const fn is_state_mismatch(&self) -> bool {
        matches!(
            self,
            Self::ExistingRowsMismatch
                | Self::ReadbackIncomplete
                | Self::Rows(StoredRowCollectionError::RowLimit { .. })
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FoundingEndpointNetworkError {
    #[error("endpoint network fold exceeded its {timeout:?} deadline")]
    Timeout { timeout: Duration },
    #[error("endpoint network fold failed: {0:?}")]
    Ensure(MachineEndpointNetworkError),
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
        MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
        MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument, PeerTransport,
        StorageMode, derive_builtin_wireguard_member,
    };
    use ployz_core::founding::{FoundingDriverEnrollment, FoundingRequest};
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
    use ployz_core::machine::{MachineLifecycle, MachineName};
    use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const MACHINE_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const PEER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    fn cluster_id() -> ClusterId {
        ClusterId::try_new(CLUSTER).expect("fixture cluster id")
    }

    fn machine_id() -> MachineRowId {
        MachineRowId::try_new(MACHINE).expect("fixture machine id")
    }

    fn provenance() -> OperatorWriteProvenance {
        OperatorWriteProvenance {
            written_by: OperationInitiator::Machine {
                machine_id: machine_id(),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z")
                .expect("fixture timestamp"),
        }
    }

    fn request(with_peer: bool) -> ValidatedFoundingRequest {
        let cluster_id = cluster_id();
        let machine_key =
            WireGuardPublicKey::try_new(MACHINE_KEY).expect("fixture machine public key");
        let driver = if with_peer {
            let peer_key = WireGuardPublicKey::try_new(PEER_KEY).expect("fixture peer public key");
            FoundingDriverEnrollment::Ssh {
                peer_id: PeerId::try_new(PEER).expect("fixture peer id"),
                document: PeerDocument {
                    v: CorrosionDocumentVersion::V1,
                    cluster_id: cluster_id.clone(),
                    provenance: provenance(),
                    name: "operator-laptop".to_owned(),
                    transport: PeerTransport::Wireguard {
                        addr_v6: derive_builtin_wireguard_member(&cluster_id, &peer_key)
                            .bind_address()
                            .get(),
                        pubkey: peer_key,
                        endpoint: None,
                    },
                },
            }
        } else {
            FoundingDriverEnrollment::OnHost
        };
        FoundingRequest {
            cluster_id: cluster_id.clone(),
            cluster: ClusterDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance(),
                name: "ares".to_owned(),
                storage_default: StorageMode::Plain,
                hostname_mode: AutomaticHostnameMode::Ployz,
                prefix: MachineEndpointSupernet::default_v1(),
                provider: MeshProvider::BuiltinWireguard,
                acme_directory_url: "https://acme.example/directory".to_owned(),
                acme_contact: None,
            },
            machine_id: machine_id(),
            machine: MachineDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance(),
                name: MachineName::try_new("ares").expect("fixture machine name"),
                lifecycle: MachineLifecycle::Active,
                transport: MachineTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &machine_key)
                        .bind_address()
                        .get(),
                    pubkey: machine_key,
                    endpoint: None,
                    subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24")
                        .expect("fixture first subnet"),
                },
                storage: MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Default,
                },
            },
            driver,
        }
        .try_validate()
        .expect("fixture founding request validates")
    }

    fn stored(expected: &ExpectedRow) -> StoredRow {
        StoredRow::new(expected.key.clone(), expected.encoded.clone())
    }

    #[test]
    fn clean_founding_writes_only_cluster_machine_and_optional_peer() {
        for (with_peer, expected_count) in [(false, 2), (true, 3)] {
            let validated = request(with_peer);
            let rows = FoundingRows {
                cluster: Vec::new(),
                machines: Vec::new(),
                peers: Vec::new(),
            };
            let plan = FoundingPlan::build(&validated, &rows).expect("clean state can found");
            assert_eq!(plan.result, FoundingResult::Found);
            assert_eq!(plan.statements.len(), expected_count);
            let encoded = serde_json::to_string(&plan.statements).expect("statements serialize");
            assert!(!encoded.contains("tokens"));
            assert!(!encoded.contains("namespaces"));
        }
    }

    #[test]
    fn exact_partial_state_resumes_and_exact_complete_state_is_a_no_op() {
        let validated = request(true);
        let expected = ExpectedRows::new(&validated).expect("expected rows encode");
        let partial = FoundingRows {
            cluster: vec![stored(&expected.cluster)],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        let plan = FoundingPlan::build(&validated, &partial).expect("exact partial state resumes");
        assert_eq!(plan.result, FoundingResult::Resumed);
        assert_eq!(plan.statements.len(), 2);
        assert!(matches!(
            FoundingPlan::verify_complete(&validated, &partial),
            Err(FoundingWriteError::ReadbackIncomplete)
        ));

        let complete = FoundingRows {
            cluster: vec![stored(&expected.cluster)],
            machines: vec![stored(&expected.machine)],
            peers: vec![stored(expected.peer.as_ref().expect("peer row"))],
        };
        let plan = FoundingPlan::build(&validated, &complete).expect("exact replay succeeds");
        assert_eq!(plan.result, FoundingResult::NoOp);
        assert!(plan.statements.is_empty());
        FoundingPlan::verify_complete(&validated, &complete)
            .expect("complete readback satisfies the founding barrier");
    }

    #[test]
    fn any_extra_or_mismatched_row_refuses_before_building_a_transaction() {
        let validated = request(false);
        let expected = ExpectedRows::new(&validated).expect("expected rows encode");
        let mismatched = FoundingRows {
            cluster: vec![StoredRow::new(expected.cluster.key.clone(), "{}")],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &mismatched),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let unexpected_peer = FoundingRows {
            cluster: Vec::new(),
            machines: Vec::new(),
            peers: vec![StoredRow::new(PEER, "{}")],
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &unexpected_peer),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let overfull_rows = FoundingRows {
            cluster: vec![
                stored(&expected.cluster),
                StoredRow::new("01ARZ3NDEKTSV4RRFFQ69G5FAZ", "{}"),
            ],
            machines: Vec::new(),
            peers: Vec::new(),
        };
        assert!(matches!(
            FoundingPlan::build(&validated, &overfull_rows),
            Err(FoundingWriteError::ExistingRowsMismatch)
        ));

        let overfull_table = FoundingWriteError::Rows(StoredRowCollectionError::RowLimit {
            limit: MAX_FOUNDING_ROWS_PER_TABLE,
        });
        assert!(overfull_table.is_state_mismatch());
    }
}
