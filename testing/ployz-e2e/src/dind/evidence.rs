//! Generic per-machine diagnostics under `target/dind-evidence/<run>/<machine>`.

use super::DindError;
use super::cluster::DindRunId;
use super::exec::{ExecOutcome, exec_in_container};
use super::machine::DindMachine;
use bollard::Docker;
use std::path::{Path, PathBuf};

const EVIDENCE_DIR_NAME: &str = "dind-evidence";

#[must_use]
pub fn evidence_dir(run_id: &DindRunId) -> PathBuf {
    target_dir().join(EVIDENCE_DIR_NAME).join(run_id.as_str())
}

/// Captures bounded system, failed-unit, and inner-Docker diagnostics.
pub async fn capture_machine_evidence(
    docker: &Docker,
    run_id: &DindRunId,
    machine: &DindMachine,
) -> Result<PathBuf, DindError> {
    let machine_dir = evidence_dir(run_id).join(&machine.name);
    std::fs::create_dir_all(&machine_dir).map_err(|source| DindError::EvidenceIo {
        path: machine_dir.clone(),
        message: source.to_string(),
    })?;

    let captures: [(&str, &[&str]); 3] = [
        (
            "journal.txt",
            &["journalctl", "--no-pager", "--lines", "2000"],
        ),
        (
            "systemctl-failed.txt",
            &["systemctl", "--failed", "--no-pager"],
        ),
        ("docker-ps.txt", &["docker", "ps", "-a"]),
    ];
    for (file_name, command) in captures {
        let content = match exec_in_container(docker, &machine.container_id, command).await {
            Ok(outcome) => render_outcome(&outcome),
            Err(error) => format!("evidence capture failed: {error}\n"),
        };
        let path = machine_dir.join(file_name);
        std::fs::write(&path, content).map_err(|source| DindError::EvidenceIo {
            path: path.clone(),
            message: source.to_string(),
        })?;
    }
    Ok(machine_dir)
}

fn render_outcome(outcome: &ExecOutcome) -> String {
    let ExecOutcome {
        exit_code,
        stdout,
        stderr,
    } = outcome;
    if *exit_code == 0 && stderr.is_empty() {
        return stdout.clone();
    }
    format!("{stdout}\n--- exit code {exit_code}; stderr ---\n{stderr}")
}

fn target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}
