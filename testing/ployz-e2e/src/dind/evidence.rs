//! Evidence capture for failed assertions: per-machine dumps under
//! `target/dind-evidence/<run_id>/<machine>/`.
//!
//! Operation rules: logs are evidence, not the audience — tests assert on
//! operations and state, and these dumps exist so a failed gated run leaves
//! something inspectable behind.

use super::DindError;
use super::cluster::DindRunId;
use super::exec::{ExecOutcome, exec_in_container};
use super::machine::DindMachine;
use bollard::Docker;
use std::path::{Path, PathBuf};

/// Directory under the cargo target dir collecting all run evidence.
const EVIDENCE_DIR_NAME: &str = "dind-evidence";

/// Evidence directory for one run: `target/dind-evidence/<run_id>/`.
#[must_use]
pub fn evidence_dir(run_id: &DindRunId) -> PathBuf {
    target_dir().join(EVIDENCE_DIR_NAME).join(run_id.as_str())
}

/// Dumps the standard evidence set for one machine and returns the directory
/// the files were written to.
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

    let captures: [(&str, &[&str]); 4] = [
        (
            "journal.txt",
            &[
                "journalctl",
                "--no-pager",
                "-u",
                "nats-server",
                "-u",
                "ployzd-*",
            ],
        ),
        (
            "systemctl-failed.txt",
            &["systemctl", "--failed", "--no-pager"],
        ),
        ("docker-ps.txt", &["docker", "ps", "-a"]),
        (
            "authorized-users.conf",
            &["cat", "/etc/nats/authorized-users.conf"],
        ),
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
    // testing/ployz-e2e -> workspace root -> target
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}
