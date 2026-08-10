//! Bounded, read-only status and doctor handlers.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    ClusterDocument, CorrosionTable, MachineDocument, MachineStatusDocument, Statement, StoredRow,
    read_named_roster_rows, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::{
    ApiRefusal, DoctorProjectionInput, DoctorRawRows, MachineLensRow, StatusCorrosionHealth,
    StatusProjectionInput, project_doctor, project_status,
};

use super::roster::corrosion_unavailable_refusal;
use super::server::{ApiService, HttpBody, json_response, refusal_response};
use crate::corrosion::{
    CorrosionClient, CorrosionHealthReadError, StoredRowLimit, collect_stored_rows,
};

const MAX_DIAGNOSTIC_ROWS_PER_TABLE: usize = 10_000;

pub(super) async fn status_response(service: &ApiService) -> Response<HttpBody> {
    let health = service.corrosion.health();
    let inputs = read_status_inputs(
        &service.corrosion,
        &service.cluster_id,
        &service.local_machine_id,
    );
    let (health, inputs) = tokio::join!(health, inputs);
    let health = match health {
        Ok(response) => StatusCorrosionHealth::Reply(response),
        Err(CorrosionHealthReadError::Unavailable) => StatusCorrosionHealth::Unavailable,
        Err(CorrosionHealthReadError::InvalidResponse) => StatusCorrosionHealth::InvalidResponse,
    };
    let inputs = match inputs {
        Ok(inputs) => inputs,
        Err(error) => {
            tracing::warn!(error = %error, "could not read status diagnostics");
            return refusal_response(error.refusal());
        }
    };
    let now_unix_seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(error) => {
            tracing::warn!(error = %error, "system clock predates the Unix epoch");
            return refusal_response(corrosion_unavailable_refusal());
        }
    };
    typed_ok(&project_status(StatusProjectionInput {
        cluster: inputs.cluster,
        machines: inputs.machines,
        answering_machine_id: service.local_machine_id.clone(),
        health,
        wireguard_handshakes: inputs.wireguard_handshakes,
        now_unix_seconds,
    }))
}

pub(super) async fn doctor_response(service: &ApiService) -> Response<HttpBody> {
    let rows = match read_doctor_rows(&service.corrosion).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(error = %error, "could not read doctor diagnostics");
            return refusal_response(error.refusal());
        }
    };
    let cluster = match accepted_cluster(&service.cluster_id, rows.cluster.clone()) {
        Ok(Some(cluster)) => cluster,
        Ok(None) => return refusal_response(ApiRefusal::MissingCluster),
        Err(error) => return refusal_response(error.refusal()),
    };
    typed_ok(&project_doctor(DoctorProjectionInput { cluster, rows }))
}

fn typed_ok<Value: serde::Serialize>(value: &Value) -> Response<HttpBody> {
    match serde_json::to_vec(value) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode diagnostics response");
            refusal_response(corrosion_unavailable_refusal())
        }
    }
}

struct StatusInputs {
    cluster: Option<ClusterDocument>,
    machines: Vec<MachineLensRow>,
    wireguard_handshakes:
        Option<BTreeMap<MachineName, ployz_core::corrosion::WireGuardHandshakeEvidence>>,
}

async fn read_status_inputs(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
    local_machine_id: &MachineName,
) -> Result<StatusInputs, DiagnosticsReadError> {
    let cluster_rows = query_rows(corrosion, select_cluster(cluster_id));
    let machine_rows = query_rows(corrosion, select_all(CorrosionTable::Machines));
    let local_status_rows = query_rows(corrosion, select_local_machine_status(local_machine_id));
    let (cluster_rows, machine_rows, local_status_rows) =
        tokio::try_join!(cluster_rows, machine_rows, local_status_rows)?;
    let cluster = accepted_cluster(cluster_id, cluster_rows)?;
    let Some(ref accepted_cluster) = cluster else {
        return Ok(StatusInputs {
            cluster: None,
            machines: Vec::new(),
            wireguard_handshakes: None,
        });
    };

    let machines = read_named_roster_rows::<MachineDocument>(accepted_cluster, machine_rows)
        .accepted
        .into_iter()
        .map(|row| {
            let id = MachineName::try_new(row.source.key).map_err(|error| {
                DiagnosticsReadError::InvalidAcceptedRow {
                    table: CorrosionTable::Machines,
                    detail: error.to_string(),
                }
            })?;
            Ok(MachineLensRow {
                id,
                document: row.value,
            })
        })
        .collect::<Result<Vec<_>, DiagnosticsReadError>>()?;

    let mut statuses = read_rows::<MachineStatusDocument>(cluster_id, local_status_rows)
        .accepted
        .into_iter()
        .filter(|row| {
            row.source.key == local_machine_id.as_str() && row.value.machine_id == *local_machine_id
        });
    let wireguard_handshakes = statuses
        .next()
        .map(|row| row.value.wireguard_handshakes)
        .unwrap_or(None);
    if statuses.next().is_some() {
        return Err(DiagnosticsReadError::InvalidAcceptedRow {
            table: CorrosionTable::MachineStatus,
            detail: format!("duplicate row for machine {local_machine_id}"),
        });
    }

    Ok(StatusInputs {
        cluster,
        machines,
        wireguard_handshakes,
    })
}

async fn read_doctor_rows(
    corrosion: &CorrosionClient,
) -> Result<DoctorRawRows, DiagnosticsReadError> {
    let [
        cluster,
        machines,
        peers,
        tokens,
        namespaces,
        route_bindings,
        controller,
        machine_endpoints,
        machine_status,
        gateway_observations,
        operations,
        cert_holdings,
        acme_http01,
    ] = doctor_statements();
    let cluster = query_rows(corrosion, cluster);
    let machines = query_rows(corrosion, machines);
    let peers = query_rows(corrosion, peers);
    let tokens = query_rows(corrosion, tokens);
    let namespaces = query_rows(corrosion, namespaces);
    let route_bindings = query_rows(corrosion, route_bindings);
    let controller = query_rows(corrosion, controller);
    let machine_endpoints = query_rows(corrosion, machine_endpoints);
    let machine_status = query_rows(corrosion, machine_status);
    let gateway_observations = query_rows(corrosion, gateway_observations);
    let operations = query_rows(corrosion, operations);
    let cert_holdings = query_rows(corrosion, cert_holdings);
    let acme_http01 = query_rows(corrosion, acme_http01);
    let (
        cluster,
        machines,
        peers,
        tokens,
        namespaces,
        route_bindings,
        controller,
        machine_endpoints,
        machine_status,
        gateway_observations,
        operations,
        cert_holdings,
        acme_http01,
    ) = tokio::try_join!(
        cluster,
        machines,
        peers,
        tokens,
        namespaces,
        route_bindings,
        controller,
        machine_endpoints,
        machine_status,
        gateway_observations,
        operations,
        cert_holdings,
        acme_http01,
    )?;

    Ok(DoctorRawRows {
        cluster,
        machines,
        peers,
        tokens,
        namespaces,
        route_bindings,
        controller,
        machine_endpoints,
        machine_status,
        gateway_observations,
        operations,
        cert_holdings,
        acme_http01,
    })
}

fn accepted_cluster(
    cluster_id: &ClusterName,
    rows: Vec<StoredRow>,
) -> Result<Option<ClusterDocument>, DiagnosticsReadError> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut accepted = read_rows::<ClusterDocument>(cluster_id, rows)
        .accepted
        .into_iter();
    let Some(row) = accepted.next() else {
        return Err(DiagnosticsReadError::InvalidCluster);
    };
    if accepted.next().is_some() || row.source.key != cluster_id.as_str() {
        return Err(DiagnosticsReadError::InvalidCluster);
    }
    Ok(Some(row.value))
}

async fn query_rows(
    corrosion: &CorrosionClient,
    statement: Statement,
) -> Result<Vec<StoredRow>, DiagnosticsReadError> {
    let mut stream = corrosion.query(&statement).await?;
    collect_stored_rows(
        &mut stream,
        StoredRowLimit::new(MAX_DIAGNOSTIC_ROWS_PER_TABLE),
    )
    .await
    .map_err(DiagnosticsReadError::from)
}

fn doctor_statements() -> [Statement; 13] {
    CorrosionTable::ALL.map(select_all)
}

fn select_cluster(cluster_id: &ClusterName) -> Statement {
    Statement::with_params(
        "SELECT id, document FROM cluster WHERE id = ?",
        vec![ployz_core::corrosion::SqliteParameter::Text(
            cluster_id.as_str().to_owned(),
        )],
    )
}

fn select_all(table: CorrosionTable) -> Statement {
    match table {
        CorrosionTable::MachineStatus | CorrosionTable::GatewayObservations => Statement::simple(
            format!("SELECT machine_id AS id, document FROM {}", table.as_str()),
        ),
        CorrosionTable::Cluster
        | CorrosionTable::Machines
        | CorrosionTable::Peers
        | CorrosionTable::Tokens
        | CorrosionTable::Namespaces
        | CorrosionTable::RouteBindings
        | CorrosionTable::Controller
        | CorrosionTable::MachineEndpoints
        | CorrosionTable::Operations
        | CorrosionTable::CertHoldings
        | CorrosionTable::AcmeHttp01 => {
            Statement::simple(format!("SELECT id, document FROM {}", table.as_str()))
        }
    }
}

fn select_local_machine_status(machine_id: &MachineName) -> Statement {
    Statement::with_params(
        "SELECT machine_id AS id, document FROM machine_status WHERE machine_id = ?",
        vec![ployz_core::corrosion::SqliteParameter::Text(
            machine_id.as_str().to_owned(),
        )],
    )
}

#[derive(Debug, thiserror::Error)]
enum DiagnosticsReadError {
    #[error("local Corrosion diagnostics query failed: {0}")]
    Client(#[from] crate::corrosion::CorrosionClientError),
    #[error("local Corrosion diagnostics rows were invalid: {0}")]
    Rows(#[from] crate::corrosion::StoredRowCollectionError),
    #[error("configured cluster row is invalid")]
    InvalidCluster,
    #[error("accepted {table:?} row is invalid: {detail}")]
    InvalidAcceptedRow {
        table: CorrosionTable,
        detail: String,
    },
}

impl DiagnosticsReadError {
    fn refusal(&self) -> ApiRefusal {
        match self {
            Self::InvalidCluster | Self::InvalidAcceptedRow { .. } => ApiRefusal::InvalidCluster,
            Self::Client(_) | Self::Rows(_) => corrosion_unavailable_refusal(),
        }
    }
}
