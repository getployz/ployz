//! Corrosion-backed preferred-controller appointment access.

use ployz_core::corrosion::{
    ControllerDocument, CorrosionDocumentVersion, CorrosionTable, CorrosionTimestamp,
    SqliteParameter, Statement, StoredRow, is_preferred_controller, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, StoredRowCollectionError, StoredRowLimit,
    collect_stored_rows,
};

const MAX_CONTROLLER_ROWS: usize = 2;

/// Access to this cluster's singleton preferred-controller row.
pub(super) struct ControllerStore {
    corrosion: CorrosionClient,
    cluster_id: ClusterName,
    local_machine_id: MachineName,
}

impl ControllerStore {
    pub(super) fn new(
        corrosion: CorrosionClient,
        cluster_id: ClusterName,
        local_machine_id: MachineName,
    ) -> Self {
        Self {
            corrosion,
            cluster_id,
            local_machine_id,
        }
    }

    pub(super) const fn cluster_id(&self) -> &ClusterName {
        &self.cluster_id
    }

    pub(super) const fn local_machine_id(&self) -> &MachineName {
        &self.local_machine_id
    }

    /// Reads the current appointment, if this cluster has one.
    pub(super) async fn current(&self) -> Result<Option<ControllerDocument>, ControllerStoreError> {
        let mut stream = self
            .corrosion
            .query(&select_controller(&self.cluster_id))
            .await?;
        let rows =
            collect_stored_rows(&mut stream, StoredRowLimit::new(MAX_CONTROLLER_ROWS)).await?;
        decode_controller_rows(&self.cluster_id, rows)
    }

    /// Creates the first appointment without replacing a row won by a racer.
    pub(super) async fn initial_self_appointment(
        &self,
        now: CorrosionTimestamp,
    ) -> Result<(), ControllerStoreError> {
        let appointment = self.new_appointment(now);
        self.corrosion
            .execute(&[initial_appointment_statement(&appointment)?])
            .await?;
        Ok(())
    }

    /// Refreshes the exact locally-owned appointment.
    pub(super) async fn heartbeat(
        &self,
        current: &ControllerDocument,
        now: CorrosionTimestamp,
    ) -> Result<(), ControllerStoreError> {
        let mut appointment = current.clone();
        appointment.heartbeat_at = now;
        self.corrosion
            .execute(&[replace_appointment_statement(&appointment, current)?])
            .await?;
        Ok(())
    }

    /// Replaces an exact stale appointment with this machine as controller.
    pub(super) async fn appoint_self_if_current_is_stale(
        &self,
        current: &ControllerDocument,
        now: CorrosionTimestamp,
    ) -> Result<(), ControllerStoreError> {
        let appointment = self.new_appointment(now);
        self.corrosion
            .execute(&[replace_appointment_statement(&appointment, current)?])
            .await?;
        Ok(())
    }

    /// Re-reads Corrosion and checks this node's current preferred machine.
    pub(super) async fn local_machine_is_preferred(&self) -> Result<bool, ControllerStoreError> {
        Ok(self
            .current()
            .await?
            .is_some_and(|controller| is_preferred_controller(&controller, &self.local_machine_id)))
    }

    fn new_appointment(&self, heartbeat_at: CorrosionTimestamp) -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            preferred_machine_id: self.local_machine_id.clone(),
            heartbeat_at,
        }
    }
}

fn decode_controller_rows(
    cluster_id: &ClusterName,
    rows: Vec<StoredRow>,
) -> Result<Option<ControllerDocument>, ControllerStoreError> {
    let report = read_rows::<ControllerDocument>(cluster_id, rows);
    if !report.skipped.is_empty() || report.accepted.len() > 1 {
        return Err(ControllerStoreError::InvalidControllerRows {
            accepted: report.accepted.len(),
            skipped: report.skipped.len(),
        });
    }
    Ok(report.accepted.into_iter().next().map(|row| row.value))
}

fn select_controller(cluster_id: &ClusterName) -> Statement {
    Statement::with_params(
        format!(
            "SELECT id, document FROM {} WHERE id = ?",
            CorrosionTable::Controller.as_str()
        ),
        vec![SqliteParameter::Text(cluster_id.as_str().to_owned())],
    )
}

fn initial_appointment_statement(
    document: &ControllerDocument,
) -> Result<Statement, serde_json::Error> {
    appointment_statement(document, "ON CONFLICT(id) DO NOTHING")
}

fn replace_appointment_statement(
    replacement: &ControllerDocument,
    current: &ControllerDocument,
) -> Result<Statement, serde_json::Error> {
    Ok(Statement::with_params(
        "UPDATE controller SET document = ? WHERE id = ? AND json_extract(document, '$.preferred_machine_id') = ? AND COALESCE(json_extract(document, '$.heartbeat_at'), '1970-01-01T00:00:00.000000000Z') = ?",
        vec![
            SqliteParameter::Text(serde_json::to_string(replacement)?),
            SqliteParameter::Text(replacement.cluster_id.as_str().to_owned()),
            SqliteParameter::Text(current.preferred_machine_id.as_str().to_owned()),
            SqliteParameter::Text(current.heartbeat_at.to_string()),
        ],
    ))
}

fn appointment_statement(
    document: &ControllerDocument,
    conflict: &str,
) -> Result<Statement, serde_json::Error> {
    Ok(Statement::with_params(
        format!(
            "INSERT INTO {} (id, document) VALUES (?, ?) {conflict}",
            CorrosionTable::Controller.as_str()
        ),
        vec![
            SqliteParameter::Text(document.cluster_id.as_str().to_owned()),
            SqliteParameter::Text(serde_json::to_string(document)?),
        ],
    ))
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ControllerStoreError {
    #[error("local Corrosion request failed: {0}")]
    Corrosion(#[from] CorrosionClientError),
    #[error("local Corrosion controller query was malformed: {0}")]
    Rows(#[from] StoredRowCollectionError),
    #[error(
        "local Corrosion controller query returned invalid rows ({accepted} accepted, {skipped} skipped)"
    )]
    InvalidControllerRows { accepted: usize, skipped: usize },
    #[error("could not encode the controller appointment: {0}")]
    Encode(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        ControllerDocument, CorrosionDocumentVersion, CorrosionTimestamp, SqliteParameter,
        Statement, StoredRow,
    };
    use ployz_core::ids::{ClusterName, MachineName};

    use super::{
        ControllerStoreError, decode_controller_rows, initial_appointment_statement,
        replace_appointment_statement, select_controller,
    };

    const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn cluster_id() -> ClusterName {
        ClusterName::try_new(CLUSTER_ID).expect("cluster id")
    }

    fn document() -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            preferred_machine_id: MachineName::try_new(MACHINE_ID).expect("machine id"),
            heartbeat_at: CorrosionTimestamp::try_new("2026-08-09T12:00:00Z")
                .expect("heartbeat timestamp"),
        }
    }

    #[test]
    fn controller_read_accepts_only_the_cluster_keyed_singleton() {
        let document = document();
        let encoded = serde_json::to_string(&document).expect("document JSON");
        assert_eq!(
            decode_controller_rows(
                &cluster_id(),
                vec![StoredRow::new(CLUSTER_ID, encoded.clone())]
            )
            .expect("controller read"),
            Some(document)
        );
        assert!(matches!(
            decode_controller_rows(&cluster_id(), vec![StoredRow::new(MACHINE_ID, encoded)]),
            Err(ControllerStoreError::InvalidControllerRows {
                accepted: 0,
                skipped: 1
            })
        ));
    }

    #[test]
    fn appointment_statements_keep_initial_and_exact_replacement_distinct() {
        let Statement::WithParams(select, select_params) = select_controller(&cluster_id()) else {
            panic!("controller select must be parameterized")
        };
        assert_eq!(select, "SELECT id, document FROM controller WHERE id = ?");
        assert_eq!(
            select_params,
            vec![SqliteParameter::Text(CLUSTER_ID.to_owned())]
        );

        let Statement::WithParams(initial, initial_params) =
            initial_appointment_statement(&document()).expect("initial statement")
        else {
            panic!("initial appointment must be parameterized")
        };
        let previous = document();
        let mut replacement = previous.clone();
        replacement.preferred_machine_id = MachineName::try_new("edge-b").expect("machine");
        let Statement::WithParams(replace, replace_params) =
            replace_appointment_statement(&replacement, &previous).expect("replace statement")
        else {
            panic!("replacement appointment must be parameterized")
        };
        assert!(initial.ends_with("ON CONFLICT(id) DO NOTHING"));
        assert!(replace.starts_with("UPDATE controller SET document = ?"));
        assert!(replace.contains("json_extract(document, '$.preferred_machine_id') = ?"));
        assert!(replace.contains("json_extract(document, '$.heartbeat_at')"));
        let [initial_cluster, _] = initial_params.as_slice() else {
            panic!("initial appointment must have two parameters")
        };
        let [_, replace_cluster, previous_machine, previous_heartbeat] = replace_params.as_slice()
        else {
            panic!("replacement appointment must have four parameters")
        };
        assert_eq!(
            initial_cluster,
            &SqliteParameter::Text(CLUSTER_ID.to_owned())
        );
        assert_eq!(
            replace_cluster,
            &SqliteParameter::Text(CLUSTER_ID.to_owned())
        );
        assert_eq!(
            previous_machine,
            &SqliteParameter::Text(MACHINE_ID.to_owned())
        );
        assert_eq!(
            previous_heartbeat,
            &SqliteParameter::Text("2026-08-09T12:00:00.000000000Z".to_owned())
        );
    }
}
