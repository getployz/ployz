use ployz_core::machine::VolumeEnsureFailure;

pub(crate) fn create_failure(failure: &ployz_sdk_types::VolumeCreateFailure) -> String {
    match failure {
        ployz_sdk_types::VolumeCreateFailure::IntentReadFailed { message } => {
            format!("volume intent could not be read: {}", message.as_str())
        }
        ployz_sdk_types::VolumeCreateFailure::MachineNotAccepted { machine_id } => {
            format!("machine {} is not accepted", machine_id.as_str())
        }
        ployz_sdk_types::VolumeCreateFailure::AdmissionFailed { failure } => {
            admission_failure(failure)
        }
        ployz_sdk_types::VolumeCreateFailure::PinCommitFailed { pin, message } => format!(
            "volume {}/{} pin to machine {} could not be committed: {}",
            pin.namespace_id().as_str(),
            pin.volume_name().as_str(),
            pin.machine_id().as_str(),
            message.as_str()
        ),
        ployz_sdk_types::VolumeCreateFailure::MachineUnavailable {
            machine_id,
            message,
        } => format!(
            "machine {} is unavailable: {}",
            machine_id.as_str(),
            message.as_str()
        ),
        ployz_sdk_types::VolumeCreateFailure::EnsureFailed {
            machine_id,
            volume_name,
            failure,
        } => format!(
            "volume {} could not be ensured on {}: {}",
            volume_name.as_str(),
            machine_id.as_str(),
            ensure_failure(failure)
        ),
    }
}

pub(crate) fn admission_failure(failure: &ployz_sdk_types::VolumeAdmissionFailure) -> String {
    use ployz_sdk_types::VolumeAdmissionFailure;

    match failure {
        VolumeAdmissionFailure::MissingDeclaration { volume_name } => {
            format!("volume {} has no declaration", volume_name.as_str())
        }
        VolumeAdmissionFailure::AmbiguousPins {
            volume_name,
            pin_count,
        } => format!(
            "volume {} has {pin_count} durable pins",
            volume_name.as_str()
        ),
        VolumeAdmissionFailure::PinnedToDifferentMachine {
            volume_name,
            pinned_machine_id,
            selected_machine_id,
        } => format!(
            "volume {} is pinned to machine {}, not selected machine {}",
            volume_name.as_str(),
            pinned_machine_id.as_str(),
            selected_machine_id.as_str()
        ),
        VolumeAdmissionFailure::KindConversion { volume_name, .. } => {
            format!(
                "volume {} cannot change kind after birth",
                volume_name.as_str()
            )
        }
        VolumeAdmissionFailure::QuotaShrink {
            volume_name,
            declared_max_size_bytes,
            pinned_max_size_bytes,
        } => format!(
            "provisioned volume {} cannot shrink from {} to {} bytes",
            volume_name.as_str(),
            pinned_max_size_bytes.get(),
            declared_max_size_bytes.get()
        ),
        VolumeAdmissionFailure::MachineSilent { machine_id } => {
            format!(
                "machine {} did not answer storage testimony",
                machine_id.as_str()
            )
        }
        VolumeAdmissionFailure::StorageTestimonyNotReported { machine_id } => format!(
            "machine {} did not report storage capability",
            machine_id.as_str()
        ),
        VolumeAdmissionFailure::StorageUnprepared { machine_id } => {
            format!("machine {} has not prepared storage", machine_id.as_str())
        }
        VolumeAdmissionFailure::StorageUnavailable { machine_id, reason } => format!(
            "machine {} storage is unavailable: {}",
            machine_id.as_str(),
            crate::machine::command::render_storage_unavailable_reason(reason)
        ),
        VolumeAdmissionFailure::PoolMismatch {
            volume_name,
            pinned_pool,
            reported_pool,
        } => format!(
            "provisioned volume {} belongs to pool {}, not ready pool {}",
            volume_name.as_str(),
            pinned_pool.as_str(),
            reported_pool.as_str()
        ),
        VolumeAdmissionFailure::DatasetIdentity {
            volume_name,
            source,
        } => format!(
            "cannot construct the dataset for volume {}: {source}",
            volume_name.as_str()
        ),
        VolumeAdmissionFailure::DuplicateDatasetQuota { dataset } => format!(
            "storage testimony contains duplicate quota rows for {}",
            dataset.as_str()
        ),
        VolumeAdmissionFailure::DatasetQuotaNotReported { dataset } => format!(
            "storage testimony has no quota row for existing dataset {}",
            dataset.as_str()
        ),
        VolumeAdmissionFailure::UnpinnedDatasetExists { dataset } => format!(
            "dataset {} exists without a durable volume pin",
            dataset.as_str()
        ),
        VolumeAdmissionFailure::CapacityOverflow => {
            "quota admission arithmetic overflowed".to_owned()
        }
        VolumeAdmissionFailure::CapacityExceeded {
            total_bytes,
            provisioned_used_bytes,
            free_bytes,
            required_headroom_bytes,
            requested_total_bytes,
        } => format!(
            "quota admission exceeds capacity: total={total_bytes} provisioned-used={provisioned_used_bytes} free={free_bytes} required-headroom={required_headroom_bytes} requested-total={requested_total_bytes}"
        ),
        VolumeAdmissionFailure::InconsistentCapacityFacts {
            total_bytes,
            provisioned_used_bytes,
            free_bytes,
        } => format!(
            "storage capacity facts are inconsistent: total={total_bytes} provisioned-used={provisioned_used_bytes} free={free_bytes}"
        ),
    }
}

pub(crate) fn ensure_failure(failure: &VolumeEnsureFailure) -> String {
    match failure {
        VolumeEnsureFailure::MachineMismatch {
            expected_machine_id,
            responder_machine_id,
        } => format!(
            "machine response was from {}, expected {}",
            responder_machine_id.as_str(),
            expected_machine_id.as_str()
        ),
        VolumeEnsureFailure::Dataset { dataset, failure } => format!(
            "dataset {} effect failed: {}",
            dataset.as_str(),
            storage_effect_failure(failure)
        ),
        VolumeEnsureFailure::DockerShapeMismatch {
            volume_name,
            retained_dataset,
            message,
        } => retained_dataset.as_ref().map_or_else(
            || {
                format!(
                    "Docker volume {} has the wrong shape: {message}",
                    volume_name.as_str()
                )
            },
            |dataset| {
                format!(
                    "Docker volume {} has the wrong shape; retained dataset {}: {message}",
                    volume_name.as_str(),
                    dataset.as_str()
                )
            },
        ),
        VolumeEnsureFailure::DockerEnsureFailed {
            volume_name,
            retained_dataset,
            message,
        } => retained_dataset.as_ref().map_or_else(
            || {
                format!(
                    "Docker volume {} ensure failed: {message}",
                    volume_name.as_str()
                )
            },
            |dataset| {
                format!(
                    "Docker volume {} ensure failed; retained dataset {}: {message}",
                    volume_name.as_str(),
                    dataset.as_str()
                )
            },
        ),
    }
}

pub(crate) fn storage_effect_failure(
    failure: &ployz_core::storage::StorageEffectFailure,
) -> String {
    use ployz_core::storage::StorageEffectFailure;

    match failure {
        StorageEffectFailure::UnsupportedPlatform => {
            "ZFS preparation is unsupported on this host profile".to_owned()
        }
        StorageEffectFailure::Installation { message } => {
            format!("ZFS installation or module loading failed: {message}")
        }
        StorageEffectFailure::PoolList { message } => {
            format!("failed to list imported ZFS pools: {message}")
        }
        StorageEffectFailure::AmbiguousPools { candidates } => format!(
            "multiple imported ZFS pools require an explicit selection: {}",
            candidates
                .iter()
                .map(|pool| pool.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        StorageEffectFailure::ExplicitPoolAbsent { pool } => {
            format!("explicit ZFS pool {} is not imported", pool.as_str())
        }
        StorageEffectFailure::OwnedPool { message } => {
            format!("Ployz owned image pool preparation failed: {message}")
        }
        StorageEffectFailure::OwnedPoolTooSmall {
            total_bytes,
            available_bytes,
            required_headroom_bytes,
            minimum_pool_bytes,
        } => format!(
            "Ployz cannot reserve an owned ZFS pool: host total {total_bytes} bytes, available {available_bytes} bytes, required host headroom {required_headroom_bytes} bytes, minimum pool {minimum_pool_bytes} bytes"
        ),
        StorageEffectFailure::Dataset { message } => {
            format!("ZFS property or dataset effect failed: {message}")
        }
        StorageEffectFailure::PreparedStateUnavailable { message } => {
            format!("prepared storage state is unavailable: {message}")
        }
        StorageEffectFailure::PreparedStateMismatch { message } => {
            format!("prepared storage state does not match observed ZFS state: {message}")
        }
        StorageEffectFailure::GatherParse { message } => {
            format!("ZFS fact output is invalid: {message}")
        }
        StorageEffectFailure::QuotaShrink {
            dataset,
            current,
            requested,
        } => format!(
            "quota shrink is forbidden for {}: current={current} requested={requested}",
            dataset.as_str()
        ),
        StorageEffectFailure::QuotaCapacityExceeded {
            total_bytes,
            provisioned_used_bytes,
            free_bytes,
            required_headroom_bytes,
            requested_total_bytes,
        } => format!(
            "quota admission exceeds capacity: total={total_bytes} provisioned-used={provisioned_used_bytes} free={free_bytes} required-headroom={required_headroom_bytes} requested-total={requested_total_bytes}"
        ),
        StorageEffectFailure::DestructiveEffect { message } => {
            format!("destructive ZFS effect refused: {message}")
        }
        StorageEffectFailure::OperationTimedOut => {
            "storage preparation exceeded its operation budget".to_owned()
        }
        StorageEffectFailure::ProcessFailed { message } => {
            format!("storage preparation process failed: {message}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{DatasetName, VolumeName, ZfsPoolName};
    use ployz_core::ids::NamespaceId;

    #[test]
    fn docker_shape_mismatch_presents_retained_dataset_evidence() {
        let volume_name = VolumeName::try_new("data").expect("volume");
        let dataset = DatasetName::for_volume(
            &ZfsPoolName::try_new("tank").expect("pool"),
            &NamespaceId::try_new("team-a").expect("namespace"),
            &volume_name,
        )
        .expect("dataset");
        let failure = VolumeEnsureFailure::DockerShapeMismatch {
            volume_name,
            retained_dataset: Some(dataset.clone()),
            message: "wrong driver options".to_owned(),
        };

        assert_eq!(
            ensure_failure(&failure),
            format!(
                "Docker volume data has the wrong shape; retained dataset {}: wrong driver options",
                dataset.as_str()
            )
        );
    }
}
