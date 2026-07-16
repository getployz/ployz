use ployz_core::deploy::VolumeName;
use ployz_core::ids::NamespaceId;
use ployz_core::machine::StorageCapability;
use serde::de::DeserializeOwned;
use std::process::Stdio;
use std::time::Duration;

const STORAGE_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);

pub(crate) fn docker_volume_name(namespace_id: &NamespaceId, volume_name: &VolumeName) -> String {
    volume_name.stable_storage_name(namespace_id)
}

pub(super) async fn observe_storage_capability() -> Option<StorageCapability> {
    run_storage_host_command("internal-storage-capability").await
}

async fn run_storage_host_command<T: DeserializeOwned>(subcommand: &str) -> Option<T> {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg(subcommand)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    decode_storage_host_command(command, STORAGE_HOST_COMMAND_TIMEOUT).await
}

async fn decode_storage_host_command<T: DeserializeOwned>(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Option<T> {
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
    use super::{decode_storage_host_command, docker_volume_name};
    use ployz_core::deploy::{VolumeName, ZfsPoolName};
    use ployz_core::machine::{PoolCapacityFacts, StorageCapability};
    use ployz_test_support::ids::namespace_id;
    use std::time::Duration;

    fn volume_name(value: &str) -> VolumeName {
        VolumeName::try_new(value).expect("valid volume name")
    }

    #[test]
    fn docker_volume_names_are_framed_to_avoid_collisions() {
        let left = namespace_id("a-b");
        let right = namespace_id("a");

        assert_ne!(
            docker_volume_name(&left, &volume_name("c")),
            docker_volume_name(&right, &volume_name("b-c"))
        );
    }

    #[tokio::test]
    async fn storage_host_protocol_distinguishes_answer_and_process_failure() {
        let mut answer = tokio::process::Command::new("sh");
        answer.args(["-c", "printf '%s' '{\"state\":\"unprepared\"}'"]);
        assert_eq!(
            decode_storage_host_command(answer, Duration::from_secs(1)).await,
            Some(StorageCapability::Unprepared)
        );

        let mut failure = tokio::process::Command::new("sh");
        failure.args(["-c", "exit 1"]);
        assert_eq!(
            decode_storage_host_command::<StorageCapability>(failure, Duration::from_secs(1)).await,
            None
        );
    }

    #[tokio::test]
    async fn storage_host_protocol_preserves_ready_capacity_testimony() {
        let mut answer = tokio::process::Command::new("sh");
        answer.args([
            "-c",
            "printf '%s' '{\"state\":\"ready\",\"pool\":\"tank\",\"capacity\":{\"available_bytes\":8192,\"child_quotas\":[]}}'",
        ]);

        assert_eq!(
            decode_storage_host_command(answer, Duration::from_secs(1)).await,
            Some(StorageCapability::Ready {
                pool: ZfsPoolName::try_new("tank").expect("valid pool"),
                capacity: PoolCapacityFacts {
                    available_bytes: 8192,
                    child_quotas: Vec::new(),
                },
            })
        );

        let mut missing_capacity = tokio::process::Command::new("sh");
        missing_capacity.args([
            "-c",
            "printf '%s' '{\"state\":\"ready\",\"pool\":\"tank\"}'",
        ]);
        assert_eq!(
            decode_storage_host_command::<StorageCapability>(
                missing_capacity,
                Duration::from_secs(1)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn storage_host_protocol_is_bounded() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "sleep 10"]);
        command.kill_on_drop(true);
        assert_eq!(
            decode_storage_host_command::<StorageCapability>(command, Duration::from_millis(10))
                .await,
            None
        );
    }
}
