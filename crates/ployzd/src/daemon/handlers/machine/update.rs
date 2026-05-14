use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ployz_api::{DaemonPayload, DaemonResponse, MachineUpdatePayload, MachineUpdateRow};
use ployz_model::{MachineId, MachineMembership};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject};
use ployz_node_api::NodeRequest;
use ployz_node_runtime::MachineUpdateNodeClient;
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep};

use crate::daemon::DaemonState;
use crate::daemon::handlers::machine::list::find_machine_record;
use crate::daemon::handlers::machine::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord,
    MachineOperationStatus, MachineOperationStore,
};
use crate::daemon::node_rpc::NatsMachineUpdateRpcTransport;

const UPDATE_READINESS_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_READINESS_INTERVAL: Duration = Duration::from_secs(1);

impl DaemonState {
    pub(crate) async fn handle_machine_update(
        &self,
        ids: &[String],
        version: &str,
        mut response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        let version = normalize_requested_version(version);
        if version.is_empty() {
            return self.err("INVALID_VERSION", "machine update version cannot be empty");
        }

        let targets = if ids.is_empty() {
            vec![self.identity.machine_id.as_str().to_string()]
        } else {
            if let Some(duplicate) = first_duplicate(ids) {
                return self.err(
                    "DUPLICATE_MACHINE",
                    format!("machine '{duplicate}' was targeted more than once"),
                );
            }
            for id in ids {
                if let Err(error) = MachineId::try_new(id.as_str()) {
                    return self.err("MACHINE_UPDATE_INVALID_TARGET", error);
                }
            }
            update_targets_with_self_last(ids, &self.identity.machine_id)
        };

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::Update,
            self.active
                .as_ref()
                .map(|active| active.config.name.0.clone()),
            targets.clone(),
            "resolved",
            MachineOperationArtifacts {
                requested_version: Some(version.clone()),
                previous_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                ..MachineOperationArtifacts::default()
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("MACHINE_UPDATE_OPERATION_FAILED", error),
        };
        let operation_id = operation.id.clone();
        let mut updated = Vec::new();
        for target in targets {
            if let Err(error) =
                operation_store.update_stage(&mut operation, format!("updating:{target}"))
            {
                return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
            }
            let result = if target == self.identity.machine_id.as_str() {
                self.update_local_machine(
                    &operation_id,
                    &version,
                    response_flushed.take(),
                    operation_store.clone(),
                    operation.clone(),
                )
                .await
            } else {
                self.update_remote_machine(&operation_id, &target, &version)
                    .await
            };

            match result {
                Ok(row) => {
                    let deferred_self_update = target == self.identity.machine_id.as_str()
                        && row.message == "scheduled local update";
                    updated.push(row);
                    if deferred_self_update {
                        let payload = MachineUpdatePayload {
                            operation_id,
                            updated,
                            failed: Vec::new(),
                        };
                        return self.ok_with_payload(
                            "machine update scheduled",
                            Some(DaemonPayload::MachineUpdate(payload)),
                        );
                    }
                }
                Err(row) => {
                    let _ = operation_store.update_status(
                        &mut operation,
                        MachineOperationStatus::Failed,
                        Some(row.message.clone()),
                    );
                    let message = format!("machine '{}' update failed: {}", row.id, row.message);
                    let payload = MachineUpdatePayload {
                        operation_id,
                        updated,
                        failed: vec![row],
                    };
                    return self.err_with_payload(
                        "MACHINE_UPDATE_FAILED",
                        message,
                        Some(DaemonPayload::MachineUpdate(payload)),
                    );
                }
            }
        }

        if let Err(error) =
            operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None)
        {
            return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
        }
        let payload = MachineUpdatePayload {
            operation_id,
            updated,
            failed: Vec::new(),
        };
        self.ok_with_payload(
            "machine update scheduled",
            Some(DaemonPayload::MachineUpdate(payload)),
        )
    }

    pub(crate) async fn handle_mesh_peer_prepare_update(
        &self,
        operation_id: &str,
        version: &str,
    ) -> DaemonResponse {
        match prepare_machine_update(version).await {
            Ok(()) => self.ok(format!("machine update '{operation_id}' prepared")),
            Err(error) => self.err("MACHINE_UPDATE_PREPARE_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_execute_update(
        &self,
        operation_id: &str,
        version: &str,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        let version = normalize_requested_version(version);
        if let Err(error) = prepare_machine_update(&version).await {
            return self.err("MACHINE_UPDATE_PREPARE_FAILED", error);
        }

        if requested_version_matches_current(&version) {
            let operation_store = self.machine_operation_store();
            if let Err(error) = ensure_update_operation(
                &operation_store,
                operation_id,
                &[self.identity.machine_id.as_str().to_string()],
                &version,
                "already-current",
            )
            .and_then(|mut operation| {
                operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Succeeded,
                    None,
                )
            }) {
                return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
            }
            return self.ok(format!(
                "machine update '{operation_id}' skipped; daemon already reports version {}",
                env!("CARGO_PKG_VERSION")
            ));
        }

        let operation_store = self.machine_operation_store();
        let operation = match ensure_update_operation(
            &operation_store,
            operation_id,
            &[self.identity.machine_id.as_str().to_string()],
            &version,
            "execute",
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("MACHINE_UPDATE_OPERATION_FAILED", error),
        };
        spawn_update_after_response(
            operation_id.to_string(),
            version,
            response_flushed,
            operation_store,
            operation,
        );
        self.ok(format!("machine update '{operation_id}' scheduled"))
    }

    async fn update_local_machine(
        &self,
        operation_id: &str,
        version: &str,
        response_flushed: Option<oneshot::Receiver<()>>,
        operation_store: MachineOperationStore,
        mut operation: MachineOperationRecord,
    ) -> Result<MachineUpdateRow, MachineUpdateRow> {
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

    async fn update_remote_machine(
        &self,
        operation_id: &str,
        target: &str,
        version: &str,
    ) -> Result<MachineUpdateRow, MachineUpdateRow> {
        let machine_id = MachineId::try_new(target).map_err(|error| MachineUpdateRow {
            id: target.to_string(),
            version: version.to_string(),
            message: error,
        })?;
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => {
                return Err(update_row(&machine_id, version, response.message()));
            }
        };
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(update_row(
                    &machine_id,
                    version,
                    format!("machine '{target}' not found"),
                ));
            }
            Err(error) => {
                return Err(update_row(
                    &machine_id,
                    version,
                    format!("failed to read machines: {error}"),
                ));
            }
        };
        let client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return Err(update_row(&machine_id, version, error)),
        };
        let update_client =
            MachineUpdateNodeClient::new(NatsMachineUpdateRpcTransport::new(client.clone()));

        if let Err(error) = update_client
            .prepare_update(&record.id, operation_id, version)
            .await
        {
            return Err(update_row(
                &machine_id,
                version,
                format!("prepare rejected: {error}"),
            ));
        }

        if let Err(error) = update_client
            .execute_update(&record.id, operation_id, version)
            .await
        {
            return Err(update_row(
                &machine_id,
                version,
                format!("execute rejected: {error}"),
            ));
        }

        match wait_for_remote_update(client, record, operation_id, version).await {
            Ok(message) => Ok(update_row(&machine_id, version, message)),
            Err(error) => Err(update_row(&machine_id, version, error)),
        }
    }
}

async fn prepare_machine_update(version: &str) -> Result<(), String> {
    if normalize_requested_version(version).is_empty() {
        return Err("machine update version cannot be empty".into());
    }
    let installer = ployz_install::find_installer_script()?;
    ensure_installer_reports_existing_install(&installer).await
}

async fn ensure_installer_reports_existing_install(installer: &Path) -> Result<(), String> {
    let output = Command::new("bash")
        .arg(installer)
        .arg("probe")
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| format!("run installer probe '{}': {error}", installer.display()))?;
    if !output.status.success() {
        return Err(format!(
            "installer probe '{}' exited with status {}",
            installer.display(),
            output.status
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("decode installer probe output: {error}"))?;
    if value
        .get("installed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err("ployz install manifest is missing; run ployz.sh install first".into())
    }
}

async fn wait_for_remote_update(
    client: NatsNodeRpcClient,
    record: MachineMembership,
    operation_id: &str,
    version: &str,
) -> Result<String, String> {
    sleep(Duration::from_secs(2)).await;
    let deadline = Instant::now() + UPDATE_READINESS_TIMEOUT;
    let mut last_error = String::from("remote update operation did not complete");
    while Instant::now() < deadline {
        match client
            .request(
                NodeCommandSubject::machine_operation_get(&record.id),
                &NodeRequest::MachineOperationGet {
                    id: operation_id.to_string(),
                },
            )
            .await
        {
            Ok(response) if response.is_ok() => {
                let Some(DaemonPayload::MachineOperation(payload)) = response.payload() else {
                    last_error =
                        "remote operation response did not include machine operation payload"
                            .into();
                    sleep(UPDATE_READINESS_INTERVAL).await;
                    continue;
                };
                match payload.operation.status.as_str() {
                    "succeeded" => {
                        if version == "latest" {
                            return Ok("remote update operation succeeded".into());
                        }
                        verify_remote_version(&client, &record, version).await?;
                        return Ok(format!("ready with version {version}"));
                    }
                    "failed" | "interrupted" => {
                        return Err(payload.operation.last_error.unwrap_or_else(|| {
                            format!("remote update {}", payload.operation.status)
                        }));
                    }
                    "running" => {
                        last_error = format!("remote update stage {}", payload.operation.stage);
                    }
                    other => {
                        last_error = format!("remote update has unknown status '{other}'");
                    }
                }
            }
            Ok(response) => {
                last_error = format!(
                    "remote status failed [{}]: {}",
                    response.code(),
                    response.message()
                );
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        sleep(UPDATE_READINESS_INTERVAL).await;
    }
    Err(last_error)
}

async fn verify_remote_version(
    client: &NatsNodeRpcClient,
    record: &MachineMembership,
    version: &str,
) -> Result<(), String> {
    let response = client
        .request(NodeCommandSubject::status(&record.id), &NodeRequest::Status)
        .await
        .map_err(|error| error.to_string())?;
    if !response.is_ok() {
        return Err(format!(
            "remote status failed [{}]: {}",
            response.code(),
            response.message()
        ));
    }
    let Some(DaemonPayload::Status(status)) = response.payload() else {
        return Err("remote status response did not include status payload".into());
    };
    if status.version == version {
        Ok(())
    } else {
        Err(format!(
            "remote reports version {}, expected {}",
            status.version, version
        ))
    }
}

fn spawn_update_after_response(
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

fn ensure_update_operation(
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

async fn run_update_installer(version: &str) -> Result<(), String> {
    let installer = ployz_install::find_installer_script()?;
    let installer_version = installer_version_argument(version);
    let status = Command::new("bash")
        .arg(&installer)
        .arg("install")
        .arg("--source")
        .arg("release")
        .arg("--version")
        .arg(&installer_version)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| format!("spawn installer '{}': {error}", installer.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("installer exited with status {status}"))
    }
}

fn installer_version_argument(canonical: &str) -> String {
    if canonical == "latest" {
        canonical.to_string()
    } else {
        format!("v{canonical}")
    }
}

fn update_row(id: &MachineId, version: &str, message: impl Into<String>) -> MachineUpdateRow {
    MachineUpdateRow {
        id: id.as_str().to_string(),
        version: version.to_string(),
        message: message.into(),
    }
}

fn normalize_requested_version(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed == "latest" {
        return trimmed.to_string();
    }
    trimmed.strip_prefix('v').unwrap_or(trimmed).to_string()
}

fn requested_version_matches_current(version: &str) -> bool {
    version != "latest" && normalize_requested_version(version) == env!("CARGO_PKG_VERSION")
}

fn first_duplicate(values: &[String]) -> Option<String> {
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Some(value.clone());
        }
    }
    None
}

fn update_targets_with_self_last(ids: &[String], local_machine_id: &MachineId) -> Vec<String> {
    let mut targets: Vec<String> = ids
        .iter()
        .filter(|id| *id != &local_machine_id.as_str())
        .cloned()
        .collect();
    if ids.iter().any(|id| id == &local_machine_id.as_str()) {
        targets.push(local_machine_id.as_str().to_string());
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::{
        first_duplicate, installer_version_argument, normalize_requested_version,
        requested_version_matches_current, update_targets_with_self_last,
    };
    use ployz_model::MachineId;

    #[test]
    fn version_normalization_trims_and_drops_v_prefix() {
        assert_eq!(
            normalize_requested_version(" v0.5.3-alpha.1 "),
            "0.5.3-alpha.1"
        );
        assert_eq!(normalize_requested_version("latest"), "latest");
    }

    #[test]
    fn requested_version_matches_current_build() {
        assert!(requested_version_matches_current(env!("CARGO_PKG_VERSION")));
        assert!(requested_version_matches_current(&format!(
            "v{}",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!requested_version_matches_current("latest"));
    }

    #[test]
    fn duplicate_detection_returns_first_repeated_value() {
        let values = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        assert_eq!(first_duplicate(values.as_slice()).as_deref(), Some("a"));
    }

    #[test]
    fn duplicate_detection_catches_repeated_self() {
        let values = vec!["self".to_string(), "self".to_string()];
        assert_eq!(first_duplicate(values.as_slice()).as_deref(), Some("self"));
    }

    #[test]
    fn explicit_self_target_is_moved_last() {
        let targets = update_targets_with_self_last(
            &[
                "self".to_string(),
                "remote-a".to_string(),
                "remote-b".to_string(),
            ],
            &MachineId::new("self"),
        );

        assert_eq!(targets, vec!["remote-a", "remote-b", "self"]);
    }

    #[test]
    fn installer_version_argument_re_adds_v_prefix_for_pinned_versions() {
        assert_eq!(installer_version_argument("0.5.3"), "v0.5.3");
        assert_eq!(
            installer_version_argument("0.5.3-alpha.1"),
            "v0.5.3-alpha.1"
        );
        assert_eq!(installer_version_argument("latest"), "latest");
    }
}
