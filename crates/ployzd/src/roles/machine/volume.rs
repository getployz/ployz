use futures_util::{StreamExt, stream};
use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName};
use ployz_core::ids::MachineId;
use ployz_core::ids::NamespaceId;
use ployz_core::intent::VolumePinState;
use ployz_core::machine::{StorageCapability, VolumeUsageFacts};
use ployz_core::storage::StorageEffectFailure;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::de::DeserializeOwned;
use std::process::Stdio;
use std::time::Duration;

pub(super) const STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const VOLUME_USAGE_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_CONCURRENT_VOLUME_READS: usize = 16;
const VOLUME_TESTIMONY_COLLECTION_TIMEOUT: Duration = Duration::from_secs(4);
pub(crate) const VOLUME_TESTIMONY_ENDPOINT_TIMEOUT: Duration = Duration::from_millis(4_500);
pub(super) const DATASET_ENSURE_HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(2 * 60);
pub(super) const DATASET_DESTROY_HOST_COMMAND_TIMEOUT: Duration =
    ployz_core::storage::DATASET_DESTROY_MAX_INNER_BUDGET.saturating_add(Duration::from_secs(20));

pub(crate) fn docker_volume_name(namespace_id: &NamespaceId, volume_name: &VolumeName) -> String {
    volume_name.stable_storage_name(namespace_id)
}

pub(super) async fn observe_storage_capability() -> Option<StorageCapability> {
    run_storage_host_command("internal-storage-capability").await
}

pub(super) async fn ensure_provisioned_dataset(
    dataset: &DatasetName,
    quota: VolumeMaxSizeBytes,
) -> Result<(), StorageEffectFailure> {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg("internal-storage-dataset-ensure")
        .arg("--dataset")
        .arg(dataset.as_str())
        .arg("--quota")
        .arg(quota.get().to_string())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    decode_typed_storage_host_command(
        command,
        DATASET_ENSURE_HOST_COMMAND_TIMEOUT,
        "dataset ensure",
    )
    .await
    .map(|_: serde_json::Value| ())
}

pub(super) async fn destroy_provisioned_dataset(
    dataset: &DatasetName,
) -> Result<(), StorageEffectFailure> {
    let command = dataset_destroy_command(dataset);
    decode_typed_storage_host_command(
        command,
        DATASET_DESTROY_HOST_COMMAND_TIMEOUT,
        "dataset destroy",
    )
    .await
    .map(|_: serde_json::Value| ())
}

fn dataset_destroy_command(dataset: &DatasetName) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg("internal-storage-dataset-destroy")
        .arg("--dataset")
        .arg(dataset.as_str())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

pub(super) async fn read_provisioned_volume_usage(
    dataset: &DatasetName,
) -> Option<VolumeUsageFacts> {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg("internal-storage-dataset-facts")
        .arg("--dataset")
        .arg(dataset.as_str())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    decode_storage_host_command(command, VOLUME_USAGE_HOST_COMMAND_TIMEOUT).await
}

pub(crate) async fn handle_volume_testimony<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: crate::roles::machine::runner::MachineVolumeUsageReader,
{
    use crate::roles::machine::protocol::{
        MachineVolumeTestimony, MachineVolumeTestimonyDomainError, MachineVolumeTestimonyResult,
        MachineVolumeTestimonyRpcOk, MachineVolumeTestimonyRpcRequest,
        MachineVolumeTestimonyRpcResponse,
    };
    use crate::roles::machine::response::{machine_domain_error, machine_success};

    let MachineVolumeTestimonyRpcRequest { mut pins } =
        match decode_json_request::<MachineVolumeTestimonyRpcRequest>(&request) {
            Ok(request) => request,
            Err(response) => return response,
        };
    pins.sort_by(compare_volume_pins);
    pins.dedup();
    if let Some(pin) = pins.iter().find(|pin| pin.machine_id() != &machine_id) {
        return machine_domain_error(MachineVolumeTestimonyRpcResponse::DomainError {
            machine_id: machine_id.clone(),
            error: MachineVolumeTestimonyDomainError::MachineMismatch {
                expected_machine_id: pin.machine_id().clone(),
                responder_machine_id: machine_id,
            },
        });
    }

    let mut reads = stream::iter(pins.iter().cloned())
        .map(|pin| {
            let runner = &runner;
            async move {
                let testimony = match runner.read_volume_usage(&pin).await {
                    Some(facts) => MachineVolumeTestimony::Available { facts },
                    None => MachineVolumeTestimony::Unavailable,
                };
                MachineVolumeTestimonyResult { pin, testimony }
            }
        })
        .buffer_unordered(MAX_CONCURRENT_VOLUME_READS);
    let deadline = tokio::time::Instant::now() + VOLUME_TESTIMONY_COLLECTION_TIMEOUT;
    let mut completed = Vec::new();
    while let Ok(Some(result)) = tokio::time::timeout_at(deadline, reads.next()).await {
        completed.push(result);
    }
    drop(reads);
    completed.sort_by(|left, right| compare_volume_pins(&left.pin, &right.pin));
    let mut completed = completed.into_iter().peekable();
    let results = pins
        .into_iter()
        .map(|pin| {
            while completed
                .peek()
                .is_some_and(|result| compare_volume_pins(&result.pin, &pin).is_lt())
            {
                completed.next();
            }
            if let Some(result) =
                completed.next_if(|result| compare_volume_pins(&result.pin, &pin).is_eq())
            {
                return result;
            }
            MachineVolumeTestimonyResult {
                pin,
                testimony: MachineVolumeTestimony::Unavailable,
            }
        })
        .collect();

    machine_success(MachineVolumeTestimonyRpcResponse::Ok(
        MachineVolumeTestimonyRpcOk {
            machine_id,
            results,
        },
    ))
}

fn compare_volume_pins(left: &VolumePinState, right: &VolumePinState) -> std::cmp::Ordering {
    let identity = (left.namespace_id(), left.volume_name(), left.machine_id()).cmp(&(
        right.namespace_id(),
        right.volume_name(),
        right.machine_id(),
    ));
    if identity != std::cmp::Ordering::Equal {
        return identity;
    }
    match (left.kind(), right.kind()) {
        (ployz_core::intent::VolumeKind::Plain, ployz_core::intent::VolumeKind::Plain) => {
            std::cmp::Ordering::Equal
        }
        (
            ployz_core::intent::VolumeKind::Plain,
            ployz_core::intent::VolumeKind::Provisioned { .. },
        ) => std::cmp::Ordering::Less,
        (
            ployz_core::intent::VolumeKind::Provisioned { .. },
            ployz_core::intent::VolumeKind::Plain,
        ) => std::cmp::Ordering::Greater,
        (
            ployz_core::intent::VolumeKind::Provisioned {
                dataset: left_dataset,
                max_size_bytes: left_max_size,
            },
            ployz_core::intent::VolumeKind::Provisioned {
                dataset: right_dataset,
                max_size_bytes: right_max_size,
            },
        ) => left_dataset
            .cmp(right_dataset)
            .then_with(|| left_max_size.get().cmp(&right_max_size.get())),
    }
}

async fn decode_typed_storage_host_command<T: DeserializeOwned>(
    mut command: tokio::process::Command,
    timeout: Duration,
    effect: &str,
) -> Result<T, StorageEffectFailure> {
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| StorageEffectFailure::OperationTimedOut)?
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: error.to_string(),
        })?;
    if output.status.success() {
        return serde_json::from_slice(&output.stdout).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("{effect} returned invalid JSON: {error}"),
            }
        });
    }
    let failure =
        serde_json::from_slice::<StorageEffectFailure>(&output.stderr).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!(
                    "{effect} failed without typed evidence: {error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }
        })?;
    Err(failure)
}

async fn run_storage_host_command<T: DeserializeOwned>(subcommand: &str) -> Option<T> {
    let mut command = tokio::process::Command::new("ployz");
    command
        .arg("host")
        .arg(subcommand)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    decode_storage_host_command(command, STORAGE_CAPABILITY_HOST_COMMAND_TIMEOUT).await
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
    use super::{
        DATASET_DESTROY_HOST_COMMAND_TIMEOUT, dataset_destroy_command, decode_storage_host_command,
        decode_typed_storage_host_command, docker_volume_name,
    };
    use ployz_core::deploy::{DatasetName, VolumeName, ZfsPoolName};
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

    #[test]
    fn dataset_destroy_child_budget_exceeds_owned_image_inner_maximum() {
        assert!(
            DATASET_DESTROY_HOST_COMMAND_TIMEOUT
                > ployz_core::storage::DATASET_DESTROY_MAX_INNER_BUDGET
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
            "printf '%s' '{\"state\":\"ready\",\"pool\":\"tank\",\"capacity\":{\"total_bytes\":16384,\"provisioned_used_bytes\":0,\"free_bytes\":8192,\"child_quotas\":[]}}'",
        ]);

        assert_eq!(
            decode_storage_host_command(answer, Duration::from_secs(1)).await,
            Some(StorageCapability::Ready {
                pool: ZfsPoolName::try_new("tank").expect("valid pool"),
                capacity: PoolCapacityFacts {
                    total_bytes: 16_384,
                    provisioned_used_bytes: 0,
                    free_bytes: 8192,
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

    #[tokio::test]
    async fn storage_host_protocol_decodes_canonical_volume_usage() {
        let mut command = tokio::process::Command::new("sh");
        command.args([
            "-c",
            "printf '%s' '{\"used_bytes\":4096,\"last_write_unix_seconds\":1700000000}'",
        ]);

        assert_eq!(
            decode_storage_host_command(command, Duration::from_secs(1)).await,
            Some(ployz_core::machine::VolumeUsageFacts {
                used_bytes: 4096,
                last_write_unix_seconds: 1_700_000_000,
            })
        );
    }

    #[tokio::test]
    async fn dataset_ensure_protocol_preserves_typed_failure() {
        let mut command = tokio::process::Command::new("sh");
        command.args([
            "-c",
            "printf '%s' '{\"kind\":\"quota_shrink\",\"dataset\":\"tank/ployz/volumes/ployz-n7-default-v4-data\",\"current\":2048,\"requested\":1024}' >&2; exit 1",
        ]);
        let result = decode_typed_storage_host_command::<serde_json::Value>(
            command,
            Duration::from_secs(1),
            "dataset ensure",
        )
        .await;

        assert!(matches!(
            result,
            Err(ployz_core::storage::StorageEffectFailure::QuotaShrink {
                current: 2048,
                requested: 1024,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn dataset_destroy_protocol_preserves_typed_failure() {
        let mut command = tokio::process::Command::new("sh");
        command.args([
            "-c",
            "printf '%s' '{\"kind\":\"process_failed\",\"message\":\"zfs destroy failed\"}' >&2; exit 1",
        ]);

        let result = decode_typed_storage_host_command::<serde_json::Value>(
            command,
            Duration::from_secs(1),
            "dataset destroy",
        )
        .await;

        assert_eq!(
            result,
            Err(ployz_core::storage::StorageEffectFailure::ProcessFailed {
                message: "zfs destroy failed".to_owned(),
            })
        );
    }

    #[test]
    fn dataset_destroy_command_forwards_the_exact_dataset() {
        let dataset = DatasetName::for_volume(
            &ZfsPoolName::try_new("stored-pool").expect("valid pool"),
            &namespace_id("exact-namespace"),
            &volume_name("exact-volume"),
        )
        .expect("valid dataset");
        let command = dataset_destroy_command(&dataset);
        let args = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            [
                "host",
                "internal-storage-dataset-destroy",
                "--dataset",
                dataset.as_str(),
            ]
        );
    }

    #[tokio::test]
    async fn dataset_destroy_protocol_rejects_malformed_failure_evidence() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "printf '%s' 'not-json' >&2; exit 1"]);

        let result = decode_typed_storage_host_command::<serde_json::Value>(
            command,
            Duration::from_secs(1),
            "dataset destroy",
        )
        .await;

        assert!(matches!(
            result,
            Err(ployz_core::storage::StorageEffectFailure::ProcessFailed { message })
                if message.contains("dataset destroy failed without typed evidence")
                    && message.contains("stderr=not-json")
        ));
    }
}
