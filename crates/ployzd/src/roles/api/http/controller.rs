//! Corrosion-backed preferred-controller appointment access.

use ployz_core::corrosion::{
    ControllerAppointmentId, ControllerDocument, CorrosionDocumentVersion, CorrosionHealthResponse,
    CorrosionTable, SqliteParameter, Statement, StoredRow, controller_visibility_allows_work,
    owns_current_controller_appointment, read_rows,
};
use ployz_core::ids::{ClusterId, MachineRowId};

use crate::corrosion::{
    CorrosionClient, CorrosionClientError, CorrosionHealthReadError, StoredRowCollectionError,
    StoredRowLimit, collect_stored_rows,
};

const MAX_CONTROLLER_ROWS: usize = 2;

/// Access to this cluster's singleton preferred-controller row.
pub(super) struct ControllerStore {
    corrosion: CorrosionClient,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
}

impl ControllerStore {
    pub(super) fn new(
        corrosion: CorrosionClient,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
    ) -> Self {
        Self {
            corrosion,
            cluster_id,
            local_machine_id,
        }
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

    /// Samples local Corrosion health and applies the isolated-member brake.
    pub(super) async fn has_work_visibility(
        &self,
        accepted_roster_members: usize,
    ) -> Result<bool, ControllerStoreError> {
        let health = self.corrosion.health().await?;
        work_visibility(accepted_roster_members, health)
    }

    /// Creates the first appointment without replacing a row won by a racer.
    ///
    /// The returned row is the current readback and may belong to another
    /// machine that appointed itself concurrently.
    pub(super) async fn initial_self_appointment(
        &self,
        accepted_roster_members: usize,
    ) -> Result<ControllerDocument, ControllerStoreError> {
        if let Some(current) = self.current().await? {
            return Ok(current);
        }
        self.require_work_visibility(accepted_roster_members)
            .await?;
        let appointment = self.new_appointment();
        self.corrosion
            .execute(&[initial_appointment_statement(&appointment)?])
            .await?;
        self.readback_after_write().await
    }

    /// Replaces the row after the caller has independently decided to fail over.
    ///
    /// The replacement applies only while the exact failed appointment is
    /// still current. The returned row makes a concurrent winner visible.
    pub(super) async fn appoint_self_after_failover_decision(
        &self,
        accepted_roster_members: usize,
        failed_appointment_id: &ControllerAppointmentId,
    ) -> Result<ControllerDocument, ControllerStoreError> {
        self.require_work_visibility(accepted_roster_members)
            .await?;
        let appointment = self.new_appointment();
        self.corrosion
            .execute(&[failover_appointment_statement(
                &appointment,
                failed_appointment_id,
            )?])
            .await?;
        self.readback_after_write().await
    }

    /// Re-reads Corrosion and checks this machine's exact opaque appointment.
    pub(super) async fn exact_local_appointment_is_current(
        &self,
        appointment_id: &ControllerAppointmentId,
    ) -> Result<bool, ControllerStoreError> {
        Ok(self.current().await?.is_some_and(|controller| {
            owns_current_controller_appointment(&controller, &self.local_machine_id, appointment_id)
        }))
    }

    async fn require_work_visibility(
        &self,
        accepted_roster_members: usize,
    ) -> Result<(), ControllerStoreError> {
        let health = self.corrosion.health().await?;
        let visible_members = visible_members(health)?;
        if controller_visibility_allows_work(accepted_roster_members, visible_members) {
            Ok(())
        } else {
            Err(ControllerStoreError::InsufficientVisibility {
                accepted_roster_members,
                visible_members,
            })
        }
    }

    fn new_appointment(&self) -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.cluster_id.clone(),
            preferred_machine_id: self.local_machine_id.clone(),
            appointment_id: ControllerAppointmentId::generate(),
        }
    }

    async fn readback_after_write(&self) -> Result<ControllerDocument, ControllerStoreError> {
        self.current()
            .await?
            .ok_or(ControllerStoreError::WriteNotVisible)
    }
}

fn work_visibility(
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
        CorrosionHealthResponse::Response { members, .. } => usize::try_from(members)
            .map_err(|_| ControllerStoreError::InvalidVisibleMemberCount { members }),
        CorrosionHealthResponse::Error(message) => {
            Err(ControllerStoreError::HealthReportedError { message })
        }
    }
}

fn decode_controller_rows(
    cluster_id: &ClusterId,
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

fn select_controller(cluster_id: &ClusterId) -> Statement {
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

fn failover_appointment_statement(
    document: &ControllerDocument,
    failed_appointment_id: &ControllerAppointmentId,
) -> Result<Statement, serde_json::Error> {
    Ok(Statement::with_params(
        "UPDATE controller SET document = ? WHERE id = ? AND json_extract(document, '$.appointment_id') = ?",
        vec![
            SqliteParameter::Text(serde_json::to_string(document)?),
            SqliteParameter::Text(document.cluster_id.as_str().to_owned()),
            SqliteParameter::Text(failed_appointment_id.as_str().to_owned()),
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
    #[error(
        "controller work requires more visibility ({visible_members} visible of {accepted_roster_members} accepted roster members)"
    )]
    InsufficientVisibility {
        accepted_roster_members: usize,
        visible_members: usize,
    },
    #[error("could not encode the controller appointment: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("the controller appointment write was not visible on readback")]
    WriteNotVisible,
}

#[cfg(test)]
mod tests {
    use ployz_core::corrosion::{
        ControllerAppointmentId, ControllerDocument, CorrosionDocumentVersion,
        CorrosionHealthResponse, SqliteParameter, Statement, StoredRow,
    };
    use ployz_core::ids::{ClusterId, MachineRowId};

    use super::{
        ControllerStoreError, decode_controller_rows, failover_appointment_statement,
        initial_appointment_statement, select_controller, work_visibility,
    };

    const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const MACHINE_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const APPOINTMENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

    fn cluster_id() -> ClusterId {
        ClusterId::try_new(CLUSTER_ID).expect("cluster id")
    }

    fn document() -> ControllerDocument {
        ControllerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id(),
            preferred_machine_id: MachineRowId::try_new(MACHINE_ID).expect("machine id"),
            appointment_id: ControllerAppointmentId::try_new(APPOINTMENT_ID)
                .expect("appointment id"),
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
    fn health_members_drive_only_the_isolated_member_brake() {
        assert!(work_visibility(1, health(1)).expect("singleton visibility"));
        assert!(!work_visibility(3, health(1)).expect("isolated visibility"));
        assert!(work_visibility(200, health(2)).expect("two visible members"));
        assert!(matches!(
            work_visibility(3, health(-1)),
            Err(ControllerStoreError::InvalidVisibleMemberCount { members: -1 })
        ));
        assert!(matches!(
            work_visibility(3, CorrosionHealthResponse::Error("cold".to_owned())),
            Err(ControllerStoreError::HealthReportedError { message }) if message == "cold"
        ));
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
    fn appointment_statements_keep_initial_and_explicit_failover_distinct() {
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
        let failed = ControllerAppointmentId::generate();
        let Statement::WithParams(failover, failover_params) =
            failover_appointment_statement(&document(), &failed).expect("failover statement")
        else {
            panic!("failover appointment must be parameterized")
        };
        assert!(initial.ends_with("ON CONFLICT(id) DO NOTHING"));
        assert!(failover.starts_with("UPDATE controller SET document = ?"));
        assert!(failover.contains("json_extract(document, '$.appointment_id') = ?"));
        let [initial_cluster, _] = initial_params.as_slice() else {
            panic!("initial appointment must have two parameters")
        };
        let [_, failover_cluster, failed_appointment] = failover_params.as_slice() else {
            panic!("failover appointment must have three parameters")
        };
        assert_eq!(
            initial_cluster,
            &SqliteParameter::Text(CLUSTER_ID.to_owned())
        );
        assert_eq!(
            failover_cluster,
            &SqliteParameter::Text(CLUSTER_ID.to_owned())
        );
        assert_eq!(
            failed_appointment,
            &SqliteParameter::Text(failed.as_str().to_owned())
        );
    }
}
