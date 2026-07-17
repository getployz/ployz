//! Fresh, bounded Docker volume usage testimony.

use std::process::Stdio;
use std::time::Duration;

use ployz_core::intent::{VolumeKind, VolumePinState};
use ployz_core::machine::VolumeUsageFacts;
use tokio::io::AsyncReadExt;

use super::provisioned_volume::{plain_volume_request, validate_volume_shape};
use super::runner::DockerManagedContainerRunner;
use crate::roles::machine::runner::MachineVolumeUsageReader;
use crate::roles::machine::volume::{docker_volume_name, read_provisioned_volume_usage};

const PLAIN_VOLUME_USAGE_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_COMMAND_OUTPUT_BYTES: u64 = 1024 * 1024;

impl MachineVolumeUsageReader for DockerManagedContainerRunner {
    async fn read_volume_usage(&self, volume: &VolumePinState) -> Option<VolumeUsageFacts> {
        match volume.kind() {
            VolumeKind::Provisioned { dataset, .. } => read_provisioned_volume_usage(dataset).await,
            VolumeKind::Plain => {
                let docker = self.docker().await.ok()?;
                let storage_name = docker_volume_name(volume.namespace_id(), volume.volume_name());
                let observed = docker.inspect_volume(&storage_name).await.ok()?;
                let expected = plain_volume_request(volume);
                validate_volume_shape(
                    volume,
                    observed.clone(),
                    "local",
                    &expected.driver_opts.unwrap_or_default(),
                )
                .ok()?;
                if observed.mountpoint.is_empty() {
                    return None;
                }
                read_plain_volume_usage(&observed.mountpoint).await
            }
        }
    }
}

async fn read_plain_volume_usage(mountpoint: &str) -> Option<VolumeUsageFacts> {
    let mut usage = tokio::process::Command::new("du");
    usage.args(["-s", "-B1", "--", mountpoint]);
    let mut latest_write = tokio::process::Command::new("find");
    latest_write.args([mountpoint, "-xdev", "-printf", "%T@\n"]);

    let gathered = tokio::time::timeout(PLAIN_VOLUME_USAGE_TIMEOUT, async {
        tokio::join!(
            run_bounded(usage, MAX_COMMAND_OUTPUT_BYTES),
            run_bounded(latest_write, MAX_COMMAND_OUTPUT_BYTES),
        )
    })
    .await
    .ok()?;
    let (used, modified) = (gathered.0?, gathered.1?);
    Some(VolumeUsageFacts {
        used_bytes: parse_du_bytes(&used)?,
        last_write_unix_seconds: parse_latest_write(&modified)?,
    })
}

async fn run_bounded(
    mut command: tokio::process::Command,
    max_output_bytes: u64,
) -> Option<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let mut output = Vec::new();
    stdout
        .take(max_output_bytes.saturating_add(1))
        .read_to_end(&mut output)
        .await
        .ok()?;
    if output.len() as u64 > max_output_bytes {
        return None;
    }
    if !child.wait().await.ok()?.success() {
        return None;
    }
    Some(output)
}

fn parse_du_bytes(output: &[u8]) -> Option<u64> {
    std::str::from_utf8(output)
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn parse_latest_write(output: &[u8]) -> Option<u64> {
    let output = std::str::from_utf8(output).ok()?;
    let mut latest = None;
    for value in output.lines() {
        let value = value.trim();
        let (seconds, fraction) = value.split_once('.')?;
        if seconds.is_empty()
            || fraction.is_empty()
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let seconds = seconds.parse::<u64>().ok()?;
        latest = Some(latest.map_or(seconds, |current: u64| current.max(seconds)));
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::{parse_latest_write, run_bounded};

    #[test]
    fn recursive_latest_write_prefers_newer_nested_entry() {
        assert_eq!(
            parse_latest_write(b"1700000000.900\n1700000123.100\n"),
            Some(1_700_000_123)
        );
    }

    #[test]
    fn recursive_latest_write_rejects_malformed_or_empty_output() {
        assert_eq!(parse_latest_write(b""), None);
        assert_eq!(parse_latest_write(b"1700000000\n"), None);
        assert_eq!(parse_latest_write(b"not-a-time\n"), None);
    }

    #[tokio::test]
    async fn bounded_command_rejects_failure_and_excess_output() {
        let mut failure = tokio::process::Command::new("sh");
        failure.args(["-c", "exit 1"]);
        assert_eq!(run_bounded(failure, 16).await, None);

        let mut excess = tokio::process::Command::new("sh");
        excess.args(["-c", "printf '0123456789'"]);
        assert_eq!(run_bounded(excess, 4).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn caller_can_cancel_a_slow_usage_command() {
        let mut slow = tokio::process::Command::new("sh");
        slow.args(["-c", "sleep 60"]);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), run_bounded(slow, 16))
                .await
                .is_err()
        );
    }
}
