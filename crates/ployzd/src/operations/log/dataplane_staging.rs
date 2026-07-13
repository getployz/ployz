use super::{
    OperationRepository, OperationStatusStoreError, StageMachineDataplaneError, index_error,
    select_status,
};
use crate::core_store::{query_json, query_json_list, to_json};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{MachineAddOperationState, OperationStatus};
use ployz_core::state::{ActiveMachineState, StagedMachineDataplaneState};
use rusqlite::{Connection, params};

impl OperationRepository {
    /// Read the active roster and effective staged machine while holding the
    /// core store's single connection, so one intent snapshot cannot combine
    /// membership from opposite sides of an activation transaction.
    pub async fn intent_machine_sources(
        &self,
    ) -> Result<
        (Vec<ActiveMachineState>, Option<StagedMachineDataplaneState>),
        OperationStatusStoreError,
    > {
        self.store
            .call(|conn| {
                let active =
                    query_json_list(conn, "SELECT json FROM machines ORDER BY machine_id")?;
                let staged: Option<StagedMachineDataplaneState> = query_json(
                    conn,
                    "SELECT json FROM machine_dataplane_staging WHERE id = ?1",
                    "1",
                )?;
                let staged = match staged {
                    Some(staged)
                        if matches!(
                            select_status(conn, &staged.operation_id)?,
                            Some(OperationStatus::MachineAdd {
                                machine_id,
                                state: MachineAddOperationState::Joining { .. },
                                ..
                            }) if machine_id == staged.machine_id
                        ) =>
                    {
                        Some(staged)
                    }
                    Some(_) | None => None,
                };
                Ok((active, staged))
            })
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn stage_machine_dataplane(
        &self,
        state: StagedMachineDataplaneState,
    ) -> Result<(), StageMachineDataplaneError> {
        let outcome = self
            .store
            .call(move |conn| stage_machine_dataplane_txn(conn, &state))
            .await
            .map_err(|error| StageMachineDataplaneError::Store(index_error(&error)))?;
        match outcome {
            StageMachineDataplaneTxn::Stored => Ok(()),
            StageMachineDataplaneTxn::OperationNotJoining { operation_id } => {
                Err(StageMachineDataplaneError::OperationNotJoining { operation_id })
            }
            StageMachineDataplaneTxn::MachineMismatch { operation_id } => {
                Err(StageMachineDataplaneError::MachineMismatch { operation_id })
            }
            StageMachineDataplaneTxn::MachineAlreadyActive { machine_id } => {
                Err(StageMachineDataplaneError::MachineAlreadyActive { machine_id })
            }
            StageMachineDataplaneTxn::Occupied => Err(StageMachineDataplaneError::StagingOccupied),
        }
    }

    pub async fn machine_dataplane_staging(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<StagedMachineDataplaneState>, OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        self.store
            .call(move |conn| {
                query_json(
                    conn,
                    "SELECT json FROM machine_dataplane_staging WHERE operation_id = ?1",
                    operation_id.as_str(),
                )
            })
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn clear_staged_machine_dataplane(
        &self,
        operation_id: &OperationId,
    ) -> Result<bool, OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        self.store
            .call(move |conn| {
                Ok(conn.execute(
                    "DELETE FROM machine_dataplane_staging WHERE operation_id = ?1",
                    params![operation_id.as_str()],
                )? > 0)
            })
            .await
            .map_err(|error| index_error(&error))
    }
}

enum StageMachineDataplaneTxn {
    Stored,
    OperationNotJoining { operation_id: OperationId },
    MachineMismatch { operation_id: OperationId },
    MachineAlreadyActive { machine_id: MachineId },
    Occupied,
}

fn stage_machine_dataplane_txn(
    conn: &mut Connection,
    state: &StagedMachineDataplaneState,
) -> Result<StageMachineDataplaneTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let Some(OperationStatus::MachineAdd {
        machine_id,
        state: MachineAddOperationState::Joining { .. },
        ..
    }) = select_status(&transaction, &state.operation_id)?
    else {
        return Ok(StageMachineDataplaneTxn::OperationNotJoining {
            operation_id: state.operation_id.clone(),
        });
    };
    if machine_id != state.machine_id {
        return Ok(StageMachineDataplaneTxn::MachineMismatch {
            operation_id: state.operation_id.clone(),
        });
    }
    let active: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM machines WHERE machine_id = ?1)",
        params![state.machine_id.as_str()],
        |row| row.get(0),
    )?;
    if active {
        return Ok(StageMachineDataplaneTxn::MachineAlreadyActive {
            machine_id: state.machine_id.clone(),
        });
    }
    let existing: Option<StagedMachineDataplaneState> = query_json(
        &transaction,
        "SELECT json FROM machine_dataplane_staging WHERE id = ?1",
        "1",
    )?;
    if let Some(existing) = existing {
        if existing == *state {
            return Ok(StageMachineDataplaneTxn::Stored);
        }
        let existing_is_joining = matches!(
            select_status(&transaction, &existing.operation_id)?,
            Some(OperationStatus::MachineAdd {
                machine_id,
                state: MachineAddOperationState::Joining { .. },
                ..
            }) if machine_id == existing.machine_id
        );
        if existing_is_joining {
            return Ok(StageMachineDataplaneTxn::Occupied);
        }
        transaction.execute("DELETE FROM machine_dataplane_staging WHERE id = 1", [])?;
    }
    transaction.execute(
        "INSERT INTO machine_dataplane_staging (id, operation_id, machine_id, json)
         VALUES (1, ?1, ?2, ?3)",
        params![
            state.operation_id.as_str(),
            state.machine_id.as_str(),
            to_json(state)?
        ],
    )?;
    transaction.commit()?;
    Ok(StageMachineDataplaneTxn::Stored)
}
