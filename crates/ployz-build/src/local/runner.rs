use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use ployz_build_api::{BuildCommand, BuildCommandOutput};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

const BUILD_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const BUILD_OUTPUT_TAIL_BYTES: usize = 64 * 1024;

#[async_trait]
pub trait BuildCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String>;
}

pub struct TokioBuildCommandRunner;

#[async_trait]
impl BuildCommandRunner for TokioBuildCommandRunner {
    async fn run(
        &self,
        command: &BuildCommand,
        current_dir: &Path,
    ) -> Result<BuildCommandOutput, String> {
        let mut child = Command::new(command.program)
            .args(&command.args)
            .envs(command.env.iter().map(|(key, value)| (key, value)))
            .current_dir(current_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                format!(
                    "spawn {} {}: {error}",
                    command.program,
                    command.redacted_args().join(" ")
                )
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("capture {} stdout", command.program))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("capture {} stderr", command.program))?;
        let stdout_task = tokio::spawn(read_tail(stdout, BUILD_OUTPUT_TAIL_BYTES));
        let stderr_task = tokio::spawn(read_tail(stderr, BUILD_OUTPUT_TAIL_BYTES));
        let (status_success, timed_out) = match timeout(BUILD_COMMAND_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => (status.success(), false),
            Ok(Err(error)) => {
                return Err(format!(
                    "wait for {} {}: {error}",
                    command.program,
                    command.redacted_args().join(" ")
                ));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (false, true)
            }
        };
        Ok(BuildCommandOutput {
            status_success,
            timed_out,
            stdout: stdout_task
                .await
                .map_err(|error| format!("read {} stdout: {error}", command.program))?,
            stderr: stderr_task
                .await
                .map_err(|error| format!("read {} stderr: {error}", command.program))?,
        })
    }
}

async fn read_tail<R>(mut reader: R, limit: usize) -> String
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tail.extend_from_slice(format!("\n[read error: {error}]").as_bytes());
                break;
            }
        };
        let Some(chunk) = buffer.get(..read) else {
            tail.extend_from_slice(b"\n[read error: read exceeded buffer length]");
            break;
        };
        tail.extend_from_slice(chunk);
        if tail.len() > limit {
            let excess = tail.len() - limit;
            tail.drain(0..excess);
            truncated = true;
        };
    }
    let body = String::from_utf8_lossy(&tail).to_string();
    if truncated {
        format!("[output truncated to last {limit} bytes]\n{body}")
    } else {
        body
    }
}

#[must_use]
pub fn build_command_failure_message(
    command: &BuildCommand,
    output: &BuildCommandOutput,
) -> String {
    let mut lines = vec![format!(
        "{} {} exited unsuccessfully",
        command.program,
        command.redacted_args().join(" ")
    )];
    if output.timed_out {
        lines.push(format!(
            "timed out after {} seconds",
            BUILD_COMMAND_TIMEOUT.as_secs()
        ));
    }
    if !output.stdout.trim().is_empty() {
        lines.push(format!(
            "stdout: {}",
            command.redact_captured_output(output.stdout.trim())
        ));
    }
    if !output.stderr.trim().is_empty() {
        lines.push(format!(
            "stderr: {}",
            command.redact_captured_output(output.stderr.trim())
        ));
    }
    lines.join("; ")
}
