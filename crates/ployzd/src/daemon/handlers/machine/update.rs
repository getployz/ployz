use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineUpdatePayload, MachineUpdateRow,
};
use ployz_types::model::{MachineId, MachineMembership, NetworkId};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep};

use crate::daemon::DaemonState;
use crate::daemon::handlers::machine::list::find_machine_record;
use crate::daemon::handlers::peer_rpc::{overlay_rpc, overlay_rpc_expect_ok};

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
            vec![self.identity.machine_id.0.clone()]
        } else {
            ids.to_vec()
        };
        if let Some(duplicate) = first_duplicate(targets.as_slice()) {
            return self.err(
                "DUPLICATE_MACHINE",
                format!("machine '{duplicate}' was targeted more than once"),
            );
        }

        let operation_id = format!("machine-update-{}", NetworkId::random());
        let mut updated = Vec::new();
        for target in targets {
            let result = if target == self.identity.machine_id.0 {
                self.update_local_machine(&operation_id, &version, response_flushed.take())
                    .await
            } else {
                self.update_remote_machine(&operation_id, &target, &version)
                    .await
            };

            match result {
                Ok(row) => updated.push(row),
                Err(row) => {
                    let payload = MachineUpdatePayload {
                        operation_id,
                        updated,
                        failed: vec![row],
                    };
                    return self.err_with_payload(
                        "MACHINE_UPDATE_FAILED",
                        "machine update failed",
                        Some(DaemonPayload::MachineUpdate(payload)),
                    );
                }
            }
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
            return self.ok(format!(
                "machine update '{operation_id}' skipped; daemon already reports version {}",
                env!("CARGO_PKG_VERSION")
            ));
        }

        spawn_update_after_response(operation_id.to_string(), version, response_flushed);
        self.ok(format!("machine update '{operation_id}' scheduled"))
    }

    async fn update_local_machine(
        &self,
        operation_id: &str,
        version: &str,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> Result<MachineUpdateRow, MachineUpdateRow> {
        if let Err(error) = prepare_machine_update(version).await {
            return Err(update_row(
                &self.identity.machine_id,
                version,
                format!("prepare failed: {error}"),
            ));
        }

        if requested_version_matches_current(version) {
            return Ok(update_row(
                &self.identity.machine_id,
                version,
                "already current",
            ));
        }

        spawn_update_after_response(
            operation_id.to_string(),
            version.to_string(),
            response_flushed,
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
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => {
                return Err(update_row(
                    &MachineId(target.to_string()),
                    version,
                    response.message,
                ));
            }
        };
        let machine_id = MachineId(target.to_string());
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
        let peer_rpc_port = match self.peer_control_port() {
            Ok(port) => port,
            Err(error) => return Err(update_row(&machine_id, version, error.to_string())),
        };

        let prepare = DaemonRequest::MeshPeerPrepareUpdate {
            operation_id: operation_id.to_string(),
            version: version.to_string(),
        };
        if let Err(error) = overlay_rpc_expect_ok(record.overlay_ip, peer_rpc_port, prepare).await {
            return Err(update_row(
                &machine_id,
                version,
                format!("prepare rejected: {error}"),
            ));
        }

        let execute = DaemonRequest::MeshPeerExecuteUpdate {
            operation_id: operation_id.to_string(),
            version: version.to_string(),
        };
        if let Err(error) = overlay_rpc_expect_ok(record.overlay_ip, peer_rpc_port, execute).await {
            return Err(update_row(
                &machine_id,
                version,
                format!("execute rejected: {error}"),
            ));
        }

        match wait_for_remote_update(record, peer_rpc_port, version).await {
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
    record: MachineMembership,
    peer_rpc_port: u16,
    version: &str,
) -> Result<String, String> {
    sleep(Duration::from_secs(2)).await;
    let deadline = Instant::now() + UPDATE_READINESS_TIMEOUT;
    let mut last_error = String::from("remote daemon did not report readiness");
    while Instant::now() < deadline {
        match overlay_rpc(record.overlay_ip, peer_rpc_port, DaemonRequest::Status).await {
            Ok(response) if response.ok => {
                let Some(DaemonPayload::Status(status)) = response.payload else {
                    last_error = "remote status response did not include status payload".into();
                    sleep(UPDATE_READINESS_INTERVAL).await;
                    continue;
                };
                if version == "latest" {
                    return Ok(format!("ready with version {}", status.version));
                }
                if status.version == version {
                    return Ok(format!("ready with version {}", status.version));
                }
                last_error = format!(
                    "remote reports version {}, waiting for {}",
                    status.version, version
                );
            }
            Ok(response) => {
                last_error = format!(
                    "remote status failed [{}]: {}",
                    response.code, response.message
                );
            }
            Err(error) => {
                last_error = error;
            }
        }
        sleep(UPDATE_READINESS_INTERVAL).await;
    }
    Err(last_error)
}

fn spawn_update_after_response(
    operation_id: String,
    version: String,
    response_flushed: Option<oneshot::Receiver<()>>,
) {
    tokio::spawn(async move {
        if let Some(response_flushed) = response_flushed {
            let _ = response_flushed.await;
        }
        if let Err(error) = run_update_installer(&version).await {
            tracing::error!(%operation_id, %version, %error, "machine update installer failed");
        }
    });
}

async fn run_update_installer(version: &str) -> Result<(), String> {
    let installer = ployz_install::find_installer_script()?;
    let status = Command::new("bash")
        .arg(&installer)
        .arg("install")
        .arg("--source")
        .arg("release")
        .arg("--version")
        .arg(version)
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

fn update_row(id: &MachineId, version: &str, message: impl Into<String>) -> MachineUpdateRow {
    MachineUpdateRow {
        id: id.0.clone(),
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

#[cfg(test)]
mod tests {
    use super::{first_duplicate, normalize_requested_version, requested_version_matches_current};

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
}
