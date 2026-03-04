use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command as TokioCommand;

pub async fn run_ssh(target: &str, remote_script: &str) -> Result<(), String> {
    let output = TokioCommand::new("ssh")
        .arg(target)
        .arg(remote_script)
        .output()
        .await
        .map_err(|e| format!("ssh execution failed: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!(
        "ssh to '{target}' failed (status: {}){}",
        output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

pub fn shell_escape(input: &str) -> String {
    input.replace('"', "\\\"")
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
