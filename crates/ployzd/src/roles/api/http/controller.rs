//! Corrosion-backed preferred-controller appointment access.

use ployz_core::corrosion::{
    ControllerDocument, ControllerRevision, CorrosionDocumentVersion, CorrosionHealthResponse,
    CorrosionTable, CorrosionTimestamp, SqliteParameter, Statement, StoredRow,
    controller_visibility_allows_work, owns_current_controller_appointment, read_rows,
};
use ployz_core::ids::{ClusterName, MachineName};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, CorrosionHealthReadError, StoredRowCollectionError,
    StoredRowLimit, collect_stored_rows,
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

    /// Applies the isolated-member brake when the roster has multiple nodes.
    pub(super) async fn has_work_visibility(
        &self,
        accepted_roster_members: usize,
    ) -> Result<bool, ControllerStoreError> {
        match accepted_roster_members {
            0 => Ok(false),
            1 | 2 => Ok(true),
            _ => {
                multi_node_work_visibility(accepted_roster_members, self.corrosion.health().await?)
            }
        }
    }

    /// Creates the first appointment without replacing a row won by a racer.
    ///
    /// The returned row is the current readback and may belong to another
    /// machine that appointed itself concurrently.
    pub(super) async fn initial_self_appointment(
        &self,
        accepted_roster_members: usize,
        now: CorrosionTimestamp,
    ) -> Result<ControllerDocument, ControllerStoreError> {
        if let Some(current) = self.current().await? {
            return Ok(current);
        }
        self.require_work_visibility(accepted_roster_members)
            .await?;
        let appointment = self.new_appointment(ControllerRevision::INITIAL, now);
        self.corrosion
            .execute(&[initial_appointment_statement(&appointment)?])
            .await?;
        self.readback_after_write().await
    }

    /// Refreshes the exact locally-owned appointment.
    pub(super) async fn heartbeat(
        &self,
        accepted_roster_members: usize,
        current: &ControllerDocument,
        now: CorrosionTimestamp,
    ) -> Result<ControllerDocument, ControllerStoreError> {
        self.require_work_visibility(accepted_roster_members)
            .await?;
        if current.preferred_machine_id != self.local_machine_id {
            return Err(ControllerStoreError::NotLocalAppointment);
        }
        let mut appointment = current.clone();
        appointment.heartbeat_at = now;
        self.corrosion
            .execute(&[replace_appointment_statement(&appointment, current)?])
            .await?;
        self.readback_after_write().await
    }

    /// Replaces an exact stale appointment with this machine as controller.
    pub(super) async fn appoint_self_if_current_is_stale(
        &self,
        accepted_roster_members: usize,
        current: &ControllerDocument,
        now: CorrosionTimestamp,
    ) -> Result<ControllerDocument, ControllerStoreError> {
        self.require_work_visibility(accepted_roster_members)
            .await?;
        let next = current
            .appointment_id
            .next()
            .map_err(|_| ControllerStoreError::RevisionExhausted)?;
        let appointment = self.new_appointment(next, now);
        self.corrosion
            .execute(&[replace_appointment_statement(&appointment, current)?])
            .await?;
        self.readback_after_write().await
    }

    /// Re-reads Corrosion and checks this machine's exact appointment revision.
    pub(super) async fn exact_local_appointment_is_current(
        &self,
        appointment_id: &ControllerRevision,
    ) -> Result<bool, ControllerStoreError> {
        Ok(self.current().await?.is_some_and(|controller| {
            owns_current_controller_appointment(&controller, &self.local_machine_id, appointment_id)
        }))
    }

    async fn require_work_visibility(
        &self,
        accepted_roster_members: usize,
    ) -> Result<(), ControllerStoreError> {
        if self.has_work_visibility(accepted_roster_members).await? {
            Ok(())
        } else {
            Err(ControllerStoreError::InsufficientVisibility)
        }
    }

    fn new_appointment(
        &self,
        appointment_id: ControllerRevision,
        heartbeat_at: CorrosionTimestamp,
    ) -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            preferred_machine_id: self.local_machine_id.clone(),
            appointment_id,
            heartbeat_at,
        }
    }

    async fn readback_after_write(&self) -> Result<ControllerDocument, ControllerStoreError> {
        self.current()
            .await?
            .ok_or(ControllerStoreError::WriteNotVisible)
    }
}

fn multi_node_work_visibility(
    accepted_roster_members: usize,
    health: CorrosionHealthResponse,
) -> Result<bool, ControllerStoreError> {
    Ok(controller_visibility_allows_work(
        accepted_roster_members,
        visible_members(health)?,
    ))
}

fn visible_members(health: CorrosionHealthResponse) -> Result<usize, ControllerStoreError> {
    match health {
        CorrosionHealthResponse::Response { members, .. } => {
            let peers = usize::try_from(members)
                .map_err(|_| ControllerStoreError::InvalidVisibleMemberCount { members })?;
            // Corrosion reports other visible agents; the answering node is
            // also visible to itself.
            peers
                .checked_add(1)
                .ok_or(ControllerStoreError::InvalidVisibleMemberCount { members })
        }
        CorrosionHealthResponse::Error(message) => {
            Err(ControllerStoreError::HealthReportedError { message })
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
        "UPDATE controller SET document = ? WHERE id = ? AND CAST(json_extract(document, '$.appointment_id') AS TEXT) = ? AND json_extract(document, '$.preferred_machine_id') = ? AND COALESCE(json_extract(document, '$.heartbeat_at'), '1970-01-01T00:00:00.000000000Z') = ?",
        vec![
            SqliteParameter::Text(serde_json::to_string(replacement)?),
            SqliteParameter::Text(replacement.cluster_id.as_str().to_owned()),
            SqliteParameter::Text(current.appointment_id.get().to_string()),
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
    #[error("local Corrosion health read failed: {0}")]
    Health(#[from] CorrosionHealthReadError),
    #[error("local Corrosion health reported an error: {message}")]
    HealthReportedError { message: String },
    #[error("local Corrosion health reported an invalid visible member count: {members}")]
    InvalidVisibleMemberCount { members: i64 },
    #[error("local Corrosion controller query was malformed: {0}")]
    Rows(#[from] StoredRowCollectionError),
    #[error(
        "local Corrosion controller query returned invalid rows ({accepted} accepted, {skipped} skipped)"
    )]
    InvalidControllerRows { accepted: usize, skipped: usize },
    #[error("controller work requires another visible roster member")]
    InsufficientVisibility,
    #[error("cannot heartbeat an appointment owned by another machine")]
    NotLocalAppointment,
    #[error("could not encode the controller appointment: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("the controller appointment write was not visible on readback")]
    WriteNotVisible,
    #[error("the controller revision counter is exhausted")]
    RevisionExhausted,
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, TcpListener};
    use std::time::Duration;

    use ployz_core::corrosion::{
        ControllerDocument, ControllerRevision, CorrosionDocumentVersion, CorrosionHealthResponse,
        CorrosionTimestamp, SqliteParameter, Statement, StoredRow,
    };
    use ployz_core::ids::{ClusterName, MachineName};

    use super::{
        ControllerStore, ControllerStoreError, decode_controller_rows,
        initial_appointment_statement, multi_node_work_visibility, replace_appointment_statement,
        select_controller,
    };
    use crate::corrosion::{
        BearerToken, CorrosionClient, CorrosionClientBounds, CorrosionClientConfig,
    };

    const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const APPOINTMENT_ID: u64 = 7;

    fn cluster_id() -> ClusterName {
        ClusterName::try_new(CLUSTER_ID).expect("cluster id")
    }

    fn document() -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            preferred_machine_id: MachineName::try_new(MACHINE_ID).expect("machine id"),
            appointment_id: ControllerRevision::try_new(APPOINTMENT_ID)
                .expect("appointment revision"),
            heartbeat_at: CorrosionTimestamp::try_new("2026-08-09T12:00:00Z")
                .expect("heartbeat timestamp"),
        }
    }

    fn health(members: i64) -> CorrosionHealthResponse {
        CorrosionHealthResponse::Response {
            gaps: 0,
            members,
            p99_lag: 0.0,
            queue_size: 0,
        }
    }

    #[test]
    fn health_members_drive_only_the_three_plus_node_isolation_brake() {
        assert!(!multi_node_work_visibility(3, health(0)).expect("isolated visibility"));
        assert!(multi_node_work_visibility(200, health(1)).expect("two visible members"));
        assert!(matches!(
            multi_node_work_visibility(3, health(-1)),
            Err(ControllerStoreError::InvalidVisibleMemberCount { members: -1 })
        ));
        assert!(matches!(
            multi_node_work_visibility(3, CorrosionHealthResponse::Error("cold".to_owned())),
            Err(ControllerStoreError::HealthReportedError { message }) if message == "cold"
        ));
    }

    #[tokio::test]
    async fn small_cluster_visibility_does_not_wait_for_corrosion_health() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("test listener");
        let config = CorrosionClientConfig::new(
            listener.local_addr().expect("test listener address"),
            BearerToken::new("test-token").expect("bearer token"),
            CorrosionClientBounds {
                connect_timeout: Duration::from_millis(10),
                request_timeout: Duration::from_millis(10),
                stream_idle_timeout: Duration::from_millis(10),
                max_ndjson_frame_bytes: 1_024,
                max_error_body_bytes: 1_024,
            },
        )
        .expect("Corrosion client config");
        let store = ControllerStore::new(
            CorrosionClient::new(config).expect("Corrosion client"),
            cluster_id(),
            MachineName::try_new(MACHINE_ID).expect("machine id"),
        );

        for roster_members in [1, 2] {
            assert!(
                store
                    .has_work_visibility(roster_members)
                    .await
                    .is_ok_and(|allowed| allowed)
            );
        }
        drop(listener);
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
        replacement.appointment_id = previous.appointment_id.next().expect("next revision");
        let Statement::WithParams(replace, replace_params) =
            replace_appointment_statement(&replacement, &previous).expect("replace statement")
        else {
            panic!("replacement appointment must be parameterized")
        };
        assert!(initial.ends_with("ON CONFLICT(id) DO NOTHING"));
        assert!(replace.starts_with("UPDATE controller SET document = ?"));
        assert!(replace.contains("json_extract(document, '$.appointment_id') AS TEXT"));
        assert!(replace.contains("json_extract(document, '$.preferred_machine_id') = ?"));
        assert!(replace.contains("json_extract(document, '$.heartbeat_at')"));
        let [initial_cluster, _] = initial_params.as_slice() else {
            panic!("initial appointment must have two parameters")
        };
        let [
            _,
            replace_cluster,
            previous_revision,
            previous_machine,
            previous_heartbeat,
        ] = replace_params.as_slice()
        else {
            panic!("replacement appointment must have five parameters")
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
            previous_revision,
            &SqliteParameter::Text(APPOINTMENT_ID.to_string())
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
