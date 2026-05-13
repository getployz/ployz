use std::sync::Arc;

use ployz_api::{DaemonPayload, DaemonResponse, ImageInspectPayload, ImageInspectRequest};
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDigest,
    ImageOperationKind, ImageOperationTargetOutcome, ImagePresence, ImageRef, MachineId,
    OperationStatus,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::ImageAvailabilityStore;
use ployz_time::now_unix_secs;

use crate::operations::ImageOperationStore;

pub async fn inspect_image_with_backend(
    store: &dyn ImageAvailabilityStore,
    operation_store: &ImageOperationStore,
    local_machine: &MachineId,
    request: &ImageInspectRequest,
    backend_result: Result<Arc<dyn RuntimeImageBackend>, String>,
) -> DaemonResponse {
    let target_machine = match inspect_target_machine(local_machine, request) {
        Ok(machine_id) => machine_id,
        Err(error) => return DaemonResponse::error(error.code, error.message, None),
    };
    let reference = image_inspect_reference(request);
    let mut operation = match operation_store.begin(
        ImageOperationKind::Inspect,
        "inspecting",
        Some(request.digest.clone()),
        None,
        vec![target_machine.clone()],
    ) {
        Ok(operation) => operation,
        Err(error) => return DaemonResponse::error("IMAGE_INSPECT_OPERATION_FAILED", error, None),
    };

    let record = match backend_result {
        Ok(backend) => {
            inspect_runtime_image_record(
                backend.as_ref(),
                target_machine.clone(),
                request.digest.clone(),
                &reference,
                &operation.id,
            )
            .await
        }
        Err(error) => failed_image_record(
            target_machine.clone(),
            request.digest.clone(),
            error,
            Some(operation.id.clone()),
        ),
    };

    if let Err(error) = store.upsert_image_availability(&record).await {
        let message = error.to_string();
        let _ = operation_store.update_target(
            &mut operation,
            image_operation_target(
                target_machine,
                OperationStatus::Failed,
                Some(message.clone()),
            ),
        );
        let _ = operation_store.update_status(
            &mut operation,
            OperationStatus::Failed,
            Some(message.clone()),
        );
        return DaemonResponse::error(
            "IMAGE_INSPECT_STORE_FAILED",
            message,
            Some(image_inspect_payload(operation.id, record)),
        );
    }

    let (status, last_error) = image_operation_result(&record);
    if let Err(error) = operation_store.update_target(
        &mut operation,
        image_operation_target(target_machine.clone(), status, last_error.clone()),
    ) {
        return DaemonResponse::error(
            "IMAGE_INSPECT_OPERATION_FAILED",
            error,
            Some(image_inspect_payload(operation.id, record)),
        );
    }
    if let Err(error) = operation_store.update_status(&mut operation, status, last_error.clone()) {
        return DaemonResponse::error(
            "IMAGE_INSPECT_OPERATION_FAILED",
            error,
            Some(image_inspect_payload(operation.id, record)),
        );
    }

    match &record.presence {
        ImagePresence::Present { .. } | ImagePresence::Absent { .. } => DaemonResponse::success(
            render_image_inspect_record(&operation.id, &record),
            Some(image_inspect_payload(operation.id, record)),
        ),
        ImagePresence::Failed { reason, .. } => DaemonResponse::error(
            "IMAGE_INSPECT_FAILED",
            format!("{}  {}", operation.id, reason),
            Some(image_inspect_payload(operation.id, record)),
        ),
        ImagePresence::Transferring { .. } => DaemonResponse::error(
            "IMAGE_INSPECT_FAILED",
            "image inspect produced an invalid transferring record",
            Some(image_inspect_payload(operation.id, record)),
        ),
    }
}

pub async fn inspect_runtime_image_record(
    backend: &dyn RuntimeImageBackend,
    machine_id: MachineId,
    digest: ImageDigest,
    reference: &str,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    match backend.verify_image_digest(reference, &digest).await {
        Ok(image) => present_image_record(machine_id, digest, reference, image, operation_id),
        Err(RuntimeImageError::NotFound { .. }) => absent_image_record(machine_id, digest),
        Err(
            error @ (RuntimeImageError::UnsupportedCapability { .. }
            | RuntimeImageError::MissingDigest { .. }
            | RuntimeImageError::DigestMismatch { .. }
            | RuntimeImageError::Backend { .. }),
        ) => failed_image_record(
            machine_id,
            digest,
            error.to_string(),
            Some(operation_id.into()),
        ),
    }
}

fn present_image_record(
    machine_id: MachineId,
    digest: ImageDigest,
    reference: &str,
    image: RuntimeImage,
    operation_id: &str,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest: digest.clone(),
        presence: ImagePresence::Present {
            artifact: ImageArtifact {
                image: ImageRef::repository_digest(image.reference, None, digest),
                platform: image.platform,
                provenance: ImageArtifactProvenance::External {
                    source: Some(reference.into()),
                },
                created_at: now,
            },
            recorded_at: now,
            source_operation_id: Some(operation_id.into()),
        },
        updated_at: now,
    }
}

pub fn absent_image_record(machine_id: MachineId, digest: ImageDigest) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest,
        presence: ImagePresence::Absent { observed_at: now },
        updated_at: now,
    }
}

fn failed_image_record(
    machine_id: MachineId,
    digest: ImageDigest,
    reason: String,
    operation_id: Option<String>,
) -> ImageAvailabilityRecord {
    let now = now_unix_secs();
    ImageAvailabilityRecord {
        machine_id,
        digest,
        presence: ImagePresence::Failed {
            reason,
            failed_at: now,
            operation_id,
        },
        updated_at: now,
    }
}

pub fn inspect_target_machine(
    local_machine: &MachineId,
    request: &ImageInspectRequest,
) -> Result<MachineId, InspectTargetError> {
    match request.machines.as_slice() {
        [] => Ok(local_machine.clone()),
        [machine_id] if machine_id == local_machine => Ok(machine_id.clone()),
        [machine_id] => Err(InspectTargetError {
            code: "IMAGE_INSPECT_REMOTE_UNSUPPORTED",
            message: format!(
                "image inspect only supports local machine '{}' in this release, got '{}'",
                local_machine.as_str(),
                machine_id.as_str()
            ),
        }),
        [first, second, rest @ ..] => Err(InspectTargetError {
            code: "IMAGE_INSPECT_REMOTE_UNSUPPORTED",
            message: format!(
                "image inspect supports one local target in this release, got '{}', '{}' and {} more",
                first.as_str(),
                second.as_str(),
                rest.len()
            ),
        }),
    }
}

pub fn image_inspect_reference(request: &ImageInspectRequest) -> String {
    request
        .reference
        .as_ref()
        .cloned()
        .unwrap_or_else(|| request.digest.as_str().into())
}

fn image_operation_result(record: &ImageAvailabilityRecord) -> (OperationStatus, Option<String>) {
    match &record.presence {
        ImagePresence::Present { .. } | ImagePresence::Absent { .. } => {
            (OperationStatus::Succeeded, None)
        }
        ImagePresence::Failed { reason, .. } => (OperationStatus::Failed, Some(reason.clone())),
        ImagePresence::Transferring { .. } => (
            OperationStatus::Failed,
            Some("image inspect produced an invalid transferring record".into()),
        ),
    }
}

fn image_operation_target(
    machine_id: MachineId,
    status: OperationStatus,
    last_error: Option<String>,
) -> ImageOperationTargetOutcome {
    match status {
        OperationStatus::Running => ImageOperationTargetOutcome::running(machine_id),
        OperationStatus::Succeeded => ImageOperationTargetOutcome::succeeded(machine_id, None),
        OperationStatus::Failed => ImageOperationTargetOutcome::failed(
            machine_id,
            last_error.unwrap_or_else(|| "image inspect target failed".into()),
        ),
        OperationStatus::Interrupted => {
            ImageOperationTargetOutcome::interrupted(machine_id, last_error)
        }
    }
}

fn image_inspect_payload(operation_id: String, record: ImageAvailabilityRecord) -> DaemonPayload {
    DaemonPayload::ImageInspect(ImageInspectPayload {
        operation_id,
        records: vec![record],
    })
}

pub fn render_image_inspect_record(operation_id: &str, record: &ImageAvailabilityRecord) -> String {
    format!(
        "{}  {}  {}  {}",
        operation_id,
        record.machine_id.as_str(),
        record.digest.as_str(),
        image_presence_label(record)
    )
}

fn image_presence_label(record: &ImageAvailabilityRecord) -> &'static str {
    match record.presence {
        ImagePresence::Present { .. } => "present",
        ImagePresence::Absent { .. } => "absent",
        ImagePresence::Transferring { .. } => "transferring",
        ImagePresence::Failed { .. } => "failed",
    }
}

#[derive(Debug)]
pub struct InspectTargetError {
    pub code: &'static str,
    pub message: String,
}
