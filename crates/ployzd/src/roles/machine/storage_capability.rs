//! Bounded read-only observation of Host Runner's storage capability.

use std::process::Stdio;
use std::time::Duration;

use ployz_core::machine::StorageCapability;

const STORAGE_CAPABILITY_TIMEOUT: Duration = Duration::from_secs(4);

pub(super) async fn observe_storage_capability() -> Option<StorageCapability> {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg("internal-storage-capability")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    observe_from_command(command, STORAGE_CAPABILITY_TIMEOUT).await
}

async fn observe_from_command(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Option<StorageCapability> {
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capability_protocol_distinguishes_answer_and_process_failure() {
        let mut answer = tokio::process::Command::new("sh");
        answer.args(["-c", "printf '%s' '{\"state\":\"unprepared\"}'"]);
        assert_eq!(
            observe_from_command(answer, Duration::from_secs(1)).await,
            Some(StorageCapability::Unprepared)
        );

        let mut failure = tokio::process::Command::new("sh");
        failure.args(["-c", "exit 1"]);
        assert_eq!(
            observe_from_command(failure, Duration::from_secs(1)).await,
            None
        );
    }

    #[tokio::test]
    async fn capability_protocol_is_bounded() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "sleep 10"]);
        command.kill_on_drop(true);
        assert_eq!(
            observe_from_command(command, Duration::from_millis(10)).await,
            None
        );
    }
}
