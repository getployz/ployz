use tokio::sync::oneshot;

use crate::daemon::DaemonState;
use crate::daemon::handlers::machine::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord,
    MachineOperationStatus, MachineOperationStore,
};

use super::installer::{prepare_machine_update, run_update_installer};
use super::update_row;
use super::version::requested_version_matches_current;

impl DaemonState {
    pub(super) async fn update_local_machine(
        &self,
        operation_id: &str,
        version: &str,
        response_flushed: Option<oneshot::Receiver<()>>,
        operation_store: MachineOperationStore,
        mut operation: MachineOperationRecord,
    ) -> Result<ployz_api::MachineUpdateRow, ployz_api::MachineUpdateRow> {
        if let Err(error) = prepare_machine_update(version).await {
            return Err(update_row(
                &self.identity.machine_id,
                version,
                format!("prepare failed: {error}"),
            ));
        }

        if requested_version_matches_current(version) {
            if let Err(error) = operation_store.update_status(
                &mut operation,
                MachineOperationStatus::Succeeded,
                None,
            ) {
                return Err(update_row(&self.identity.machine_id, version, error));
            }
            return Ok(update_row(
                &self.identity.machine_id,
                version,
                "already current",
            ));
        }

        if let Err(error) = operation_store.update_stage(&mut operation, "execute") {
            return Err(update_row(&self.identity.machine_id, version, error));
        }
        spawn_update_after_response(
            operation_id.to_string(),
            version.to_string(),
            response_flushed,
            operation_store,
            operation,
        );
        Ok(update_row(
            &self.identity.machine_id,
            version,
            "scheduled local update",
        ))
    }
}

pub(super) fn spawn_update_after_response(
    operation_id: String,
    version: String,
    response_flushed: Option<oneshot::Receiver<()>>,
    operation_store: MachineOperationStore,
    mut operation: MachineOperationRecord,
) {
    tokio::spawn(async move {
        if let Some(response_flushed) = response_flushed {
            let _ = response_flushed.await;
        }
        if let Err(error) = operation_store.update_stage(&mut operation, "installing") {
            tracing::error!(%operation_id, %version, %error, "machine update operation stage update failed");
        }
        match run_update_installer(&version).await {
            Ok(()) => {
                if let Err(error) = operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Succeeded,
                    None,
                ) {
                    tracing::error!(%operation_id, %version, %error, "machine update operation success update failed");
                }
            }
            Err(error) => {
                if let Err(save_error) = operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Failed,
                    Some(error.clone()),
                ) {
                    tracing::error!(%operation_id, %version, %error, %save_error, "machine update operation failure update failed");
                }
                tracing::error!(%operation_id, %version, %error, "machine update installer failed");
            }
        }
    });
}

pub(super) fn ensure_update_operation(
    operation_store: &MachineOperationStore,
    operation_id: &str,
    targets: &[String],
    version: &str,
    stage: &str,
) -> Result<MachineOperationRecord, String> {
    if let Some(mut existing) = operation_store.load(operation_id)? {
        operation_store.update_stage(&mut existing, stage)?;
        return Ok(existing);
    }
    operation_store.begin_with_id(
        operation_id.to_string(),
        MachineOperationKind::Update,
        None,
        targets.to_vec(),
        stage,
        MachineOperationArtifacts {
            requested_version: Some(version.to_string()),
            previous_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            ..MachineOperationArtifacts::default()
        },
    )
}
