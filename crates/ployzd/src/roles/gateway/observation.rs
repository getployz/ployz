//! Machine-owned Gateway Observation publication.

use std::net::SocketAddr;

use crate::corrosion::{CorrosionClient, CorrosionClientError};
use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTimestamp, GatewayObservationDocument,
    GatewayProjectionAggregateFailure, GatewayRouteObservation, SqliteParameter, Statement,
    TransactionResult,
};
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::machine::{GatewayProcessHealth, GatewayServingStatus};

#[derive(Clone)]
pub(super) struct GatewayObservationPublisher {
    client: CorrosionClient,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
    listen_addr: SocketAddr,
}

impl GatewayObservationPublisher {
    #[must_use]
    pub(super) const fn new(
        client: CorrosionClient,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
        listen_addr: SocketAddr,
    ) -> Self {
        Self {
            client,
            cluster_id,
            local_machine_id,
            listen_addr,
        }
    }

    pub(super) async fn publish(
        &self,
        serving: GatewayServingStatus,
        routes: &[GatewayRouteObservation],
        aggregate_failures: &[GatewayProjectionAggregateFailure],
        process_health: &GatewayProcessHealth,
    ) -> Result<(), GatewayObservationPublishError> {
        let document = GatewayObservationDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            machine_id: self.local_machine_id.clone(),
            observed_at: CorrosionTimestamp::now_utc(),
            listen_addr: self.listen_addr,
            serving,
            routes: routes.to_vec(),
            aggregate_failures: aggregate_failures.to_vec(),
            process_health: process_health.clone(),
        };
        let statement = statement(&document)?;
        let response = self.client.execute(&[statement]).await?;
        let [TransactionResult::Success(result)] = response.results.as_slice() else {
            return Err(GatewayObservationPublishError::UnexpectedWriteResult);
        };
        if result.rows_affected != 1 {
            return Err(GatewayObservationPublishError::UnexpectedRowsAffected {
                rows_affected: result.rows_affected,
            });
        }
        Ok(())
    }
}

fn statement(
    document: &GatewayObservationDocument,
) -> Result<Statement, GatewayObservationPublishError> {
    let encoded = serde_json::to_string(document).map_err(|source| {
        GatewayObservationPublishError::Encode {
            detail: source.to_string(),
        }
    })?;
    Ok(Statement::with_params(
        "INSERT INTO gateway_observations (machine_id, document) VALUES (?, ?) \
         ON CONFLICT(machine_id) DO UPDATE SET document = excluded.document",
        vec![
            SqliteParameter::Text(document.machine_id.as_str().to_owned()),
            SqliteParameter::Text(encoded),
        ],
    ))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum GatewayObservationPublishError {
    #[error(transparent)]
    Corrosion(#[from] CorrosionClientError),
    #[error("could not encode Gateway Observation: {detail}")]
    Encode { detail: String },
    #[error("Gateway Observation write returned an unexpected result")]
    UnexpectedWriteResult,
    #[error("Gateway Observation write affected {rows_affected} rows instead of one")]
    UnexpectedRowsAffected { rows_affected: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_upsert_derives_the_row_key_from_document_identity() {
        let document = GatewayObservationDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster"),
            machine_id: MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine"),
            observed_at: CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("timestamp"),
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 80)),
            serving: GatewayServingStatus::Current,
            routes: Vec::new(),
            aggregate_failures: Vec::new(),
            process_health: GatewayProcessHealth::default(),
        };

        let Statement::WithParams(sql, parameters) = statement(&document).expect("statement")
        else {
            panic!("observation UPSERT is parameterized");
        };
        assert!(sql.starts_with("INSERT INTO gateway_observations"));
        let [SqliteParameter::Text(key), SqliteParameter::Text(encoded)] = parameters.as_slice()
        else {
            panic!("observation UPSERT has key and document");
        };
        let decoded: GatewayObservationDocument = serde_json::from_str(encoded).expect("document");
        assert_eq!(key, decoded.machine_id.as_str());
    }
}
