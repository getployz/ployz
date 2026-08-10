//! Pure diagnostics projections shared by API responders and callers.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::MachineLensRow;
use crate::corrosion::{
    AcmeHttp01Document, CORROSION_NO_P99_LAG_SAMPLE, CertHoldingDocument, ClusterDocument,
    ContainerDocument, ControllerDocument, CorrosionDocument, CorrosionHealthResponse,
    CorrosionTable, GatewayObservationDocument, MachineDocument, MachineStatusDocument,
    MachineTransport, MalformedDocument, MeshProvider, NamespaceDocument, OperationDocument,
    PeerDocument, Principal, RouteBindingDocument, RowSkipReason, ServiceDocument, SkippedRow,
    StoredRow, TokenDocument, WireGuardHandshakeEvidence, read_named_roster_rows, read_rows,
};
use crate::ids::{ClusterName, MachineName, PeerName, TokenName};
use crate::network::MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS;

/// A transport-neutral health observation supplied to the status projection.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusCorrosionHealth {
    Reply(CorrosionHealthResponse),
    Unavailable,
    InvalidResponse,
}

/// All inputs needed to project one cheap status response.
#[derive(Debug)]
pub struct StatusProjectionInput {
    pub cluster: Option<ClusterDocument>,
    pub machines: Vec<MachineLensRow>,
    pub answering_machine_id: MachineName,
    pub health: StatusCorrosionHealth,
    pub wireguard_handshakes: Option<BTreeMap<MachineName, WireGuardHandshakeEvidence>>,
    pub now_unix_seconds: u64,
}

/// The public status response returned by the answering machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StatusDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub cluster: Option<StatusClusterSummary>,
    pub answering_machine: StatusAnsweringMachine,
    pub sync: StatusSync,
    pub barrier: StatusBarrier,
    pub machines: Vec<StatusMachineRow>,
    pub hints: Vec<StatusHint>,
}

/// Durable cluster identity and roster count visible to the answering machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StatusClusterSummary {
    pub id: ClusterName,
    pub name: String,
    pub machine_count: usize,
}

/// Whether the answering machine is present in its current accepted roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StatusAnsweringMachine {
    Known { id: MachineName, name: MachineName },
    Unknown { id: MachineName },
}

/// The answering Corrosion replica's sync observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StatusSync {
    CaughtUp {
        p99_lag: f64,
    },
    Syncing {
        gaps: i64,
        queue_size: u64,
        p99_lag: f64,
    },
    NoLagSample,
    Degraded {
        reason: StatusDegradationReason,
    },
}

/// The product-level reason Corrosion sync health cannot be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StatusDegradationReason {
    CorrosionUnavailable,
    InvalidCorrosionHealthResponse,
}

/// The durable roster barrier, independent of current connectivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StatusBarrier {
    Ready,
    CatchingUp,
    NoRoster,
}

/// One accepted roster machine in stable display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StatusMachineRow {
    pub id: MachineName,
    pub name: MachineName,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub address: IpAddr,
    pub handshake: StatusHandshakeEvidence,
}

/// Live handshake evidence as viewed by the answering machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StatusHandshakeEvidence {
    SelfMachine,
    NoTestimony,
    Never,
    Ago {
        seconds: u64,
        freshness: HandshakeFreshness,
    },
}

/// Whether an observed handshake age crosses the placement-health boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HandshakeFreshness {
    Healthy,
    Stale,
}

/// A derived status hint that hands deeper diagnosis to another primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StatusHint {
    AllPeerHandshakesStale,
}

/// Projects status from one accepted durable view and one local health sample.
#[must_use]
pub fn project_status(input: StatusProjectionInput) -> StatusDocument {
    let StatusProjectionInput {
        cluster,
        machines,
        answering_machine_id,
        health,
        wireguard_handshakes,
        now_unix_seconds,
    } = input;

    let barrier = if machines.is_empty() {
        if cluster.is_some() {
            StatusBarrier::CatchingUp
        } else {
            StatusBarrier::NoRoster
        }
    } else {
        StatusBarrier::Ready
    };
    let cluster = cluster.map(|cluster| StatusClusterSummary {
        id: cluster.cluster_id,
        name: cluster.name,
        machine_count: machines.len(),
    });
    let answering_machine = machines
        .iter()
        .find(|machine| machine.id == answering_machine_id)
        .map_or_else(
            || StatusAnsweringMachine::Unknown {
                id: answering_machine_id.clone(),
            },
            |machine| StatusAnsweringMachine::Known {
                id: answering_machine_id.clone(),
                name: machine.document.name.clone(),
            },
        );

    let mut projected_machines = machines
        .into_iter()
        .map(|machine| {
            let address = machine_address(&machine.document.transport);
            let handshake = project_handshake(
                &machine.id,
                &answering_machine_id,
                wireguard_handshakes.as_ref(),
                now_unix_seconds,
            );
            StatusMachineRow {
                id: machine.id,
                name: machine.document.name,
                address,
                handshake,
            }
        })
        .collect::<Vec<_>>();
    projected_machines.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });

    let hints = match wireguard_handshakes {
        None => Vec::new(),
        Some(_) => {
            let comparable =
                projected_machines
                    .iter()
                    .filter_map(|machine| match machine.handshake {
                        StatusHandshakeEvidence::SelfMachine
                        | StatusHandshakeEvidence::NoTestimony => None,
                        StatusHandshakeEvidence::Never => Some(HandshakeFreshness::Stale),
                        StatusHandshakeEvidence::Ago { freshness, .. } => Some(freshness),
                    });
            let mut comparable = comparable.peekable();
            if comparable.peek().is_some()
                && comparable.all(|freshness| freshness == HandshakeFreshness::Stale)
            {
                vec![StatusHint::AllPeerHandshakesStale]
            } else {
                Vec::new()
            }
        }
    };

    StatusDocument {
        cluster,
        answering_machine,
        sync: project_sync(health),
        barrier,
        machines: projected_machines,
        hints,
    }
}

fn machine_address(transport: &MachineTransport) -> IpAddr {
    match transport {
        MachineTransport::Wireguard { addr_v6, .. } => IpAddr::V6(*addr_v6),
        MachineTransport::Tailscale { ip, .. } => IpAddr::V4(*ip),
    }
}

fn project_handshake(
    machine_id: &MachineName,
    answering_machine_id: &MachineName,
    testimony: Option<&BTreeMap<MachineName, WireGuardHandshakeEvidence>>,
    now_unix_seconds: u64,
) -> StatusHandshakeEvidence {
    if machine_id == answering_machine_id {
        return StatusHandshakeEvidence::SelfMachine;
    }
    let Some(testimony) = testimony else {
        return StatusHandshakeEvidence::NoTestimony;
    };
    let Some(evidence) = testimony.get(machine_id) else {
        return StatusHandshakeEvidence::NoTestimony;
    };
    match evidence {
        WireGuardHandshakeEvidence::Never => StatusHandshakeEvidence::Never,
        WireGuardHandshakeEvidence::At { unix_seconds } => {
            let seconds = now_unix_seconds.saturating_sub(*unix_seconds);
            let freshness = if seconds <= MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS {
                HandshakeFreshness::Healthy
            } else {
                HandshakeFreshness::Stale
            };
            StatusHandshakeEvidence::Ago { seconds, freshness }
        }
    }
}

fn project_sync(health: StatusCorrosionHealth) -> StatusSync {
    match health {
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Response {
            gaps,
            members: _,
            p99_lag,
            queue_size,
        }) if gaps == 0 && queue_size == 0 => StatusSync::CaughtUp { p99_lag },
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Response {
            gaps,
            members: _,
            p99_lag,
            queue_size,
        }) => StatusSync::Syncing {
            gaps,
            queue_size,
            p99_lag,
        },
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Error(message))
            if message == CORROSION_NO_P99_LAG_SAMPLE =>
        {
            StatusSync::NoLagSample
        }
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Error(_))
        | StatusCorrosionHealth::Unavailable => StatusSync::Degraded {
            reason: StatusDegradationReason::CorrosionUnavailable,
        },
        StatusCorrosionHealth::InvalidResponse => StatusSync::Degraded {
            reason: StatusDegradationReason::InvalidCorrosionHealthResponse,
        },
    }
}

/// Raw rows from every v1 table inspected by the read-only doctor projection.
#[derive(Debug, Clone)]
pub struct DoctorRawRows {
    pub cluster: Vec<StoredRow>,
    pub machines: Vec<StoredRow>,
    pub peers: Vec<StoredRow>,
    pub tokens: Vec<StoredRow>,
    pub namespaces: Vec<StoredRow>,
    pub services: Vec<StoredRow>,
    pub route_bindings: Vec<StoredRow>,
    pub controller: Vec<StoredRow>,
    pub containers: Vec<StoredRow>,
    pub machine_status: Vec<StoredRow>,
    pub gateway_observations: Vec<StoredRow>,
    pub operations: Vec<StoredRow>,
    pub cert_holdings: Vec<StoredRow>,
    pub acme_http01: Vec<StoredRow>,
}

impl DoctorRawRows {
    /// Creates an explicit empty sweep containing every Corrosion table.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            cluster: Vec::new(),
            machines: Vec::new(),
            peers: Vec::new(),
            tokens: Vec::new(),
            namespaces: Vec::new(),
            services: Vec::new(),
            route_bindings: Vec::new(),
            controller: Vec::new(),
            containers: Vec::new(),
            machine_status: Vec::new(),
            gateway_observations: Vec::new(),
            operations: Vec::new(),
            cert_holdings: Vec::new(),
            acme_http01: Vec::new(),
        }
    }

    fn rows_for(&self, table: CorrosionTable) -> &[StoredRow] {
        match table {
            CorrosionTable::Cluster => &self.cluster,
            CorrosionTable::Machines => &self.machines,
            CorrosionTable::Peers => &self.peers,
            CorrosionTable::Tokens => &self.tokens,
            CorrosionTable::Namespaces => &self.namespaces,
            CorrosionTable::Services => &self.services,
            CorrosionTable::RouteBindings => &self.route_bindings,
            CorrosionTable::Controller => &self.controller,
            CorrosionTable::Containers => &self.containers,
            CorrosionTable::MachineStatus => &self.machine_status,
            CorrosionTable::GatewayObservations => &self.gateway_observations,
            CorrosionTable::Operations => &self.operations,
            CorrosionTable::CertHoldings => &self.cert_holdings,
            CorrosionTable::AcmeHttp01 => &self.acme_http01,
        }
    }
}

/// Accepted cluster context and the complete raw-table sweep for `doctor`.
#[derive(Debug, Clone)]
pub struct DoctorProjectionInput {
    pub cluster: ClusterDocument,
    pub rows: DoctorRawRows,
}

/// Read-only diagnostic evidence. Every repair is a typed handoff target only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorDocument {
    #[serde(default)]
    pub skipped_roster_rows: Vec<DoctorSkippedRosterRow>,
    #[serde(default)]
    pub noncanonical_rows: Vec<DoctorNoncanonicalRow>,
    pub skipped_newer_versions: Vec<DoctorSkippedNewerVersion>,
    pub versions: DoctorVersionReport,
    pub foreign_clusters: Vec<DoctorForeignClusterRows>,
}

/// One machine or peer row excluded from the accepted roster by reader law.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorSkippedRosterRow {
    pub table: DoctorRosterTable,
    pub key: String,
    pub reason: DoctorRosterRowSkipReason,
}

/// One same-cluster row whose key does not match its typed identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorNoncanonicalRow {
    pub table: CorrosionTable,
    pub key: String,
    pub expected: String,
}

/// The two tables that can carry cluster roster rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DoctorRosterTable {
    Machines,
    Peers,
}

/// A closed reason a same-cluster roster row did not enter the accepted roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctorRosterRowSkipReason {
    MeshProviderMismatch {
        expected: MeshProvider,
        found: MeshProvider,
    },
    MalformedDocument {
        class: DoctorMalformedRosterDocumentClass,
    },
}

/// A same-cluster malformed-document class with parser messages removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctorMalformedRosterDocumentClass {
    MissingVersion,
    InvalidVersion,
    UnsupportedVersion { found: u64 },
    InvalidPayload,
}

/// One row skipped because its additive document version is newer than Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorSkippedNewerVersion {
    pub table: CorrosionTable,
    pub key: String,
    pub found: u64,
    pub supported: u32,
}

/// A current accepted roster machine used in diagnostic evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorMachineIdentity {
    pub id: MachineName,
    pub name: MachineName,
}

/// Semantic Ployz-version evidence from current machine testimony.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorVersionReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub newest: Option<DoctorNewestVersion>,
    pub behind: Vec<DoctorMachineVersion>,
    pub invalid: Vec<DoctorMachineVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorNewestVersion {
    pub version: String,
    pub machines: Vec<DoctorMachineIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorMachineVersion {
    pub machine: DoctorMachineIdentity,
    pub version: String,
}

/// Foreign rows grouped under the cluster id that readers fence out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorForeignClusterRows {
    pub cluster_id: String,
    pub rows: Vec<DoctorForeignRowEvidence>,
}

/// One foreign row and its table-specific raw authorship evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DoctorForeignRowEvidence {
    pub table: CorrosionTable,
    pub key: String,
    pub authorship: DoctorForeignAuthorship,
}

/// Whether a foreign row's raw author is an actionable current machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctorForeignAuthorship {
    CurrentMachine { machine: DoctorMachineIdentity },
    NonCurrentMachine { machine_id: MachineName },
    Peer { peer_id: PeerName },
    ApiToken { token_id: TokenName },
    Unparseable,
}

/// Projects a complete, read-only doctor report from raw Corrosion rows.
#[must_use]
pub fn project_doctor(input: DoctorProjectionInput) -> DoctorDocument {
    let DoctorProjectionInput { cluster, rows } = input;
    let machine_report = read_named_roster_rows::<MachineDocument>(&cluster, rows.machines.clone());
    let current_machines = machine_report
        .accepted
        .iter()
        .filter_map(|row| {
            MachineName::try_new(row.source.key.clone()).ok().map(|id| {
                (
                    id.clone(),
                    DoctorMachineIdentity {
                        id,
                        name: row.value.name.clone(),
                    },
                )
            })
        })
        .collect::<BTreeMap<_, _>>();

    let peer_report = read_named_roster_rows::<PeerDocument>(&cluster, rows.peers.clone());
    let noncanonical_rows = noncanonical_row_evidence(
        &cluster.cluster_id,
        &rows,
        &machine_report.skipped,
        &peer_report.skipped,
    );
    let mut skipped_roster_rows =
        map_roster_skips(DoctorRosterTable::Machines, machine_report.skipped);
    skipped_roster_rows.extend(map_roster_skips(
        DoctorRosterTable::Peers,
        peer_report.skipped,
    ));
    skipped_roster_rows.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| left.key.cmp(&right.key))
    });

    DoctorDocument {
        skipped_roster_rows,
        noncanonical_rows,
        skipped_newer_versions: newer_version_evidence(&cluster.cluster_id, &rows),
        versions: project_versions(&cluster.cluster_id, &rows.machine_status, &current_machines),
        foreign_clusters: project_foreign_clusters(&cluster.cluster_id, &rows, &current_machines),
    }
}

fn map_roster_skips(
    table: DoctorRosterTable,
    skipped: Vec<SkippedRow>,
) -> Vec<DoctorSkippedRosterRow> {
    skipped
        .into_iter()
        .filter_map(|skipped| {
            let reason = match skipped.reason {
                RowSkipReason::MeshProviderMismatch { expected, found } => {
                    DoctorRosterRowSkipReason::MeshProviderMismatch { expected, found }
                }
                RowSkipReason::Malformed(malformed) => {
                    DoctorRosterRowSkipReason::MalformedDocument {
                        class: map_malformed_roster_document(malformed)?,
                    }
                }
                RowSkipReason::InvalidRowKey { .. } => return None,
                RowSkipReason::Empty
                | RowSkipReason::ForeignCluster { .. }
                | RowSkipReason::NewerVersion { .. } => return None,
            };
            Some(DoctorSkippedRosterRow {
                table,
                key: skipped.source.key,
                reason,
            })
        })
        .collect()
}

fn noncanonical_row_evidence(
    cluster_id: &ClusterName,
    rows: &DoctorRawRows,
    machine_skips: &[SkippedRow],
    peer_skips: &[SkippedRow],
) -> Vec<DoctorNoncanonicalRow> {
    let mut evidence = Vec::new();
    extend_noncanonical(&mut evidence, CorrosionTable::Machines, machine_skips);
    extend_noncanonical(&mut evidence, CorrosionTable::Peers, peer_skips);

    macro_rules! read_table {
        ($table:ident, $document:ty, $rows:ident) => {
            extend_noncanonical(
                &mut evidence,
                CorrosionTable::$table,
                &read_rows::<$document>(cluster_id, rows.$rows.clone()).skipped,
            );
        };
    }
    read_table!(Cluster, ClusterDocument, cluster);
    read_table!(Tokens, TokenDocument, tokens);
    read_table!(Namespaces, NamespaceDocument, namespaces);
    read_table!(Services, ServiceDocument, services);
    read_table!(RouteBindings, RouteBindingDocument, route_bindings);
    read_table!(Controller, ControllerDocument, controller);
    read_table!(Containers, ContainerDocument, containers);
    read_table!(MachineStatus, MachineStatusDocument, machine_status);
    read_table!(
        GatewayObservations,
        GatewayObservationDocument,
        gateway_observations
    );
    read_table!(Operations, OperationDocument, operations);
    read_table!(CertHoldings, CertHoldingDocument, cert_holdings);
    read_table!(AcmeHttp01, AcmeHttp01Document, acme_http01);

    evidence.sort_by(|left, right| {
        left.table
            .cmp(&right.table)
            .then_with(|| left.key.cmp(&right.key))
    });
    evidence
}

fn extend_noncanonical(
    evidence: &mut Vec<DoctorNoncanonicalRow>,
    table: CorrosionTable,
    skipped: &[SkippedRow],
) {
    evidence.extend(skipped.iter().filter_map(|skipped| {
        let RowSkipReason::InvalidRowKey { expected } = &skipped.reason else {
            return None;
        };
        Some(DoctorNoncanonicalRow {
            table,
            key: skipped.source.key.clone(),
            expected: expected.clone(),
        })
    }));
}

fn map_malformed_roster_document(
    malformed: MalformedDocument,
) -> Option<DoctorMalformedRosterDocumentClass> {
    match malformed {
        MalformedDocument::MissingVersion => {
            Some(DoctorMalformedRosterDocumentClass::MissingVersion)
        }
        MalformedDocument::InvalidVersion => {
            Some(DoctorMalformedRosterDocumentClass::InvalidVersion)
        }
        MalformedDocument::UnsupportedVersion { found } => {
            Some(DoctorMalformedRosterDocumentClass::UnsupportedVersion { found })
        }
        MalformedDocument::InvalidPayload { message: _ } => {
            Some(DoctorMalformedRosterDocumentClass::InvalidPayload)
        }
        MalformedDocument::InvalidJson { message: _ }
        | MalformedDocument::NotObject
        | MalformedDocument::MissingClusterId
        | MalformedDocument::InvalidClusterId => None,
    }
}

fn newer_version_evidence(
    cluster_id: &ClusterName,
    rows: &DoctorRawRows,
) -> Vec<DoctorSkippedNewerVersion> {
    let mut evidence = Vec::new();
    for table in CorrosionTable::ALL {
        for row in rows.rows_for(table) {
            let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&row.document) else {
                continue;
            };
            if fields.get("cluster_id").and_then(Value::as_str) != Some(cluster_id.as_str()) {
                continue;
            }
            let Some(found) = fields.get("v").and_then(Value::as_u64) else {
                continue;
            };
            let supported = supported_version(table);
            if found > u64::from(supported) {
                evidence.push(DoctorSkippedNewerVersion {
                    table,
                    key: row.key.clone(),
                    found,
                    supported,
                });
            }
        }
    }
    evidence
}

fn supported_version(table: CorrosionTable) -> u32 {
    match table {
        CorrosionTable::Cluster => ClusterDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Machines => MachineDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Peers => PeerDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Tokens => TokenDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Namespaces => NamespaceDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Services => ServiceDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::RouteBindings => RouteBindingDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Controller => ControllerDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Containers => ContainerDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::MachineStatus => MachineStatusDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::GatewayObservations => GatewayObservationDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::Operations => OperationDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::CertHoldings => CertHoldingDocument::SUPPORTED_VERSION.get(),
        CorrosionTable::AcmeHttp01 => AcmeHttp01Document::SUPPORTED_VERSION.get(),
    }
}

fn project_versions(
    cluster_id: &ClusterName,
    rows: &[StoredRow],
    current_machines: &BTreeMap<MachineName, DoctorMachineIdentity>,
) -> DoctorVersionReport {
    let report = read_rows::<MachineStatusDocument>(cluster_id, rows.iter().cloned());
    let mut valid = Vec::<(semver::Version, String, DoctorMachineIdentity)>::new();
    let mut invalid = Vec::new();
    for row in report.accepted {
        if row.source.key != row.value.machine_id.as_str() {
            continue;
        }
        let Some(machine) = current_machines.get(&row.value.machine_id).cloned() else {
            continue;
        };
        match semver::Version::parse(&row.value.ployz_version) {
            Ok(version) => valid.push((version, row.value.ployz_version, machine)),
            Err(_) => invalid.push(DoctorMachineVersion {
                machine,
                version: row.value.ployz_version,
            }),
        }
    }
    valid.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.2.name.cmp(&right.2.name))
            .then_with(|| left.2.id.cmp(&right.2.id))
    });
    invalid.sort_by(|left, right| {
        left.machine
            .name
            .cmp(&right.machine.name)
            .then_with(|| left.machine.id.cmp(&right.machine.id))
    });

    let Some((newest_semantic, newest_text, _)) = valid.last().cloned() else {
        return DoctorVersionReport {
            newest: None,
            behind: Vec::new(),
            invalid,
        };
    };
    let mut newest_machines = Vec::new();
    let mut behind = Vec::new();
    for (version, text, machine) in valid {
        if version == newest_semantic {
            newest_machines.push(machine);
        } else {
            behind.push(DoctorMachineVersion {
                machine,
                version: text,
            });
        }
    }
    newest_machines.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });

    DoctorVersionReport {
        newest: Some(DoctorNewestVersion {
            version: newest_text,
            machines: newest_machines,
        }),
        behind,
        invalid,
    }
}

fn project_foreign_clusters(
    cluster_id: &ClusterName,
    rows: &DoctorRawRows,
    current_machines: &BTreeMap<MachineName, DoctorMachineIdentity>,
) -> Vec<DoctorForeignClusterRows> {
    let mut grouped = BTreeMap::<String, Vec<DoctorForeignRowEvidence>>::new();
    for table in CorrosionTable::ALL {
        for row in rows.rows_for(table) {
            let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&row.document) else {
                continue;
            };
            let Some(found_cluster) = fields.get("cluster_id").and_then(Value::as_str) else {
                continue;
            };
            if found_cluster == cluster_id.as_str() {
                continue;
            }
            grouped
                .entry(found_cluster.to_owned())
                .or_default()
                .push(DoctorForeignRowEvidence {
                    table,
                    key: row.key.clone(),
                    authorship: extract_authorship(table, &fields, current_machines),
                });
        }
    }
    grouped
        .into_iter()
        .map(|(cluster_id, rows)| DoctorForeignClusterRows { cluster_id, rows })
        .collect()
}

fn extract_authorship(
    table: CorrosionTable,
    fields: &serde_json::Map<String, Value>,
    current_machines: &BTreeMap<MachineName, DoctorMachineIdentity>,
) -> DoctorForeignAuthorship {
    match table {
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Peers
        | CorrosionTable::Tokens
        | CorrosionTable::Namespaces
        | CorrosionTable::Services
        | CorrosionTable::RouteBindings => fields
            .get("written_by")
            .cloned()
            .and_then(|value| serde_json::from_value::<Principal>(value).ok())
            .map_or(
                DoctorForeignAuthorship::Unparseable,
                |principal| match principal {
                    Principal::Machine { machine_id } => {
                        machine_authorship(machine_id, current_machines)
                    }
                    Principal::Peer { peer_id } => DoctorForeignAuthorship::Peer { peer_id },
                    Principal::ApiToken { token_id } => {
                        DoctorForeignAuthorship::ApiToken { token_id }
                    }
                },
            ),
        CorrosionTable::Controller => fields
            .get("preferred_machine_id")
            .and_then(Value::as_str)
            .and_then(|value| MachineName::try_new(value).ok())
            .map_or(DoctorForeignAuthorship::Unparseable, |machine_id| {
                machine_authorship(machine_id, current_machines)
            }),
        CorrosionTable::Containers
        | CorrosionTable::MachineStatus
        | CorrosionTable::GatewayObservations
        | CorrosionTable::Operations
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => fields
            .get("machine_id")
            .and_then(Value::as_str)
            .and_then(|value| MachineName::try_new(value).ok())
            .map_or(DoctorForeignAuthorship::Unparseable, |machine_id| {
                machine_authorship(machine_id, current_machines)
            }),
    }
}

fn machine_authorship(
    machine_id: MachineName,
    current_machines: &BTreeMap<MachineName, DoctorMachineIdentity>,
) -> DoctorForeignAuthorship {
    match current_machines.get(&machine_id) {
        Some(machine) => DoctorForeignAuthorship::CurrentMachine {
            machine: machine.clone(),
        },
        None => DoctorForeignAuthorship::NonCurrentMachine { machine_id },
    }
}
