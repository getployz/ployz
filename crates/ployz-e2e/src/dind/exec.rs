//! Exec-in-container with collected output. No silent failures: every call
//! returns the exit code plus both output streams.

use super::{DindError, docker_api_error};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use std::time::Duration;

pub use ployz_test_support::shell::shell_quote;

/// Bound on a single exec; the harness must never wait forever on a machine.
const EXEC_BUDGET: Duration = Duration::from_secs(120);

/// Result of one command run inside a machine container.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub exit_code: i64,
    pub stdout: String,
    pub stderr: String,
}

impl ExecOutcome {
    /// True when the command exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Writes `contents` to `path` inside the container (parent directories
/// created, file mode applied). Content travels base64-packed through one
/// `sh -c` exec so no stdin plumbing is needed.
pub async fn write_file_in_container(
    docker: &Docker,
    container_id: &str,
    path: &str,
    contents: &str,
    mode: &str,
) -> Result<(), DindError> {
    let encoded = base64_encode(contents.as_bytes());
    let quoted_path = shell_quote(path);
    let script = format!(
        "set -eu; dir=$(dirname {quoted_path}); mkdir -p \"$dir\"; \
         printf '%s' '{encoded}' | base64 -d > {quoted_path}; chmod {mode} {quoted_path}"
    );
    let outcome = exec_in_container(docker, container_id, &["sh", "-c", &script]).await?;
    if outcome.success() {
        Ok(())
    } else {
        Err(DindError::DockerApi {
            context: format!("write file {path} in container"),
            message: format!("exit {}: {}", outcome.exit_code, outcome.stderr.trim()),
        })
    }
}

/// Reads a file out of the container; the file must exist and be readable.
pub async fn read_file_from_container(
    docker: &Docker,
    container_id: &str,
    path: &str,
) -> Result<String, DindError> {
    let outcome = exec_in_container(docker, container_id, &["cat", path]).await?;
    if outcome.success() {
        Ok(outcome.stdout)
    } else {
        Err(DindError::DockerApi {
            context: format!("read file {path} from container"),
            message: format!("exit {}: {}", outcome.exit_code, outcome.stderr.trim()),
        })
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Runs `command` inside `container_id`, attached, and collects its output.
pub async fn exec_in_container(
    docker: &Docker,
    container_id: &str,
    command: &[&str],
) -> Result<ExecOutcome, DindError> {
    let run = run_exec(docker, container_id, command);
    match tokio::time::timeout(EXEC_BUDGET, run).await {
        Ok(outcome) => outcome,
        Err(_elapsed) => Err(DindError::ExecTimeout {
            container: container_id.to_owned(),
            command: command.join(" "),
        }),
    }
}

async fn run_exec(
    docker: &Docker,
    container_id: &str,
    command: &[&str],
) -> Result<ExecOutcome, DindError> {
    let created = docker
        .create_exec(
            container_id,
            CreateExecOptions::<String> {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(command.iter().map(|part| (*part).to_owned()).collect()),
                ..Default::default()
            },
        )
        .await
        .map_err(docker_api_error("create exec"))?;

    let started = docker
        .start_exec(&created.id, None)
        .await
        .map_err(docker_api_error("start exec"))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    match started {
        StartExecResults::Attached {
            mut output,
            input: _,
        } => {
            while let Some(chunk) = output.next().await {
                let chunk = chunk.map_err(docker_api_error("read exec output"))?;
                match chunk {
                    LogOutput::StdOut { message } | LogOutput::Console { message } => {
                        stdout.extend_from_slice(&message);
                    }
                    LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
                    LogOutput::StdIn { message: _ } => {}
                }
            }
        }
        StartExecResults::Detached => {
            return Err(DindError::ExecDetached {
                container: container_id.to_owned(),
            });
        }
    }

    let inspected = docker
        .inspect_exec(&created.id)
        .await
        .map_err(docker_api_error("inspect exec"))?;
    let Some(exit_code) = inspected.exit_code else {
        return Err(DindError::ExecExitCodeMissing {
            container: container_id.to_owned(),
            command: command.join(" "),
        });
    };

    Ok(ExecOutcome {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}
