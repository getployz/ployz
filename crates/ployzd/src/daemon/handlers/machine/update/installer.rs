use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use super::version::{installer_version_argument, normalize_requested_version};

pub(super) async fn prepare_machine_update(version: &str) -> Result<(), String> {
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

pub(super) async fn run_update_installer(version: &str) -> Result<(), String> {
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
