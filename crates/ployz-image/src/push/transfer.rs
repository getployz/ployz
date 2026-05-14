use crate::archive::{ParsedImageArchive, ReceiverUploadReport, upload_archive_to_receiver};
use crate::operations::ImageOperationStore;
use crate::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageDistributeRequest,
    ImageOperationRecord, ImagePresence, ImageReceiveSessionPayload, ImageReceiveSessionRequest,
    ImageReceivedImportRequest, ImageTransferFailure, ImageTransferFailureStage,
    ImageTransferTargetResult, MachineId,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend};
use ployz_store_api::ImageAvailabilityStore;
use ployz_time::now_unix_secs;

use super::{ImagePeerClient, ImageService, image_ref_from_tag};

impl ImageService {
    pub(super) async fn image_receive_session_for_target(
        &self,
        target_machine: &MachineId,
        operation_id: &str,
        repository: String,
        peer_client: &dyn ImagePeerClient,
    ) -> Result<ImageReceiveSessionPayload, String> {
        let request = ImageReceiveSessionRequest {
            operation_id: operation_id.into(),
            source_machine: self.local_machine.clone(),
            repository: Some(repository),
        };
        let response = if *target_machine == self.local_machine {
            self.handle_image_receive_session(&request).await
        } else {
            peer_client
                .image_receive_session(target_machine, request)
                .await?
        };
        if !response.is_ok() {
            return Err(format!(
                "target receive session failed [{}]: {}",
                response.code(),
                response.message()
            ));
        }
        let Some(ImageServicePayload::ImageReceiveSession(payload)) = response.payload() else {
            return Err("target receive session response did not include a session payload".into());
        };
        Ok(payload)
    }

    pub(super) async fn distribute_archive_to_target(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: &MachineId,
        request: &ImageDistributeRequest,
        archive: &ParsedImageArchive,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> (ImageTransferTargetResult, Option<u64>) {
        if let Err(response) = self.update_image_distribute_stage(
            operation_store,
            operation,
            "opening receive session",
            target_machine,
        ) {
            return (
                failed_image_transfer_target(
                    target_machine.clone(),
                    ImageTransferFailureStage::ReceiveSession,
                    response.code(),
                    response.message().to_string(),
                ),
                None,
            );
        }
        let repository = default_receive_repository(&operation.id);
        let session = match self
            .image_receive_session_for_target(
                target_machine,
                &operation.id,
                repository.clone(),
                peer_client,
            )
            .await
        {
            Ok(session) => session,
            Err(message) => {
                return (
                    failed_image_transfer_target(
                        target_machine.clone(),
                        ImageTransferFailureStage::ReceiveSession,
                        "IMAGE_DISTRIBUTE_RECEIVE_SESSION_FAILED",
                        message,
                    ),
                    None,
                );
            }
        };

        if let Err(response) = self.update_image_distribute_stage(
            operation_store,
            operation,
            "uploading image blobs",
            target_machine,
        ) {
            return (
                failed_image_transfer_target(
                    target_machine.clone(),
                    ImageTransferFailureStage::Upload,
                    response.code(),
                    response.message().to_string(),
                ),
                None,
            );
        }
        let reference = operation.id.clone();
        let upload = match upload_archive_to_receiver(&session, &reference, archive).await {
            Ok(upload) => upload,
            Err(error) => {
                return (
                    failed_image_transfer_target(
                        target_machine.clone(),
                        ImageTransferFailureStage::Upload,
                        "IMAGE_DISTRIBUTE_UPLOAD_FAILED",
                        error.to_string(),
                    ),
                    None,
                );
            }
        };
        let _ = (
            upload.uploaded_blobs,
            upload.skipped_blobs,
            &upload.manifest_digest,
        );

        if let Err(response) = self.update_image_distribute_stage(
            operation_store,
            operation,
            "importing target image",
            target_machine,
        ) {
            return (
                failed_image_transfer_target(
                    target_machine.clone(),
                    ImageTransferFailureStage::Import,
                    response.code(),
                    response.message().to_string(),
                ),
                None,
            );
        }
        let record = match self
            .import_received_image_for_target(
                target_machine,
                ImageReceivedImportRequest {
                    operation_id: operation.id.clone(),
                    source_machine: request.source_machine.clone(),
                    repository,
                    reference,
                    expected_digest: request.digest.clone(),
                    platform: request.platform.clone(),
                    repo_tags: archive.repo_tags.clone(),
                },
                backend,
                peer_client,
            )
            .await
        {
            Ok(record) => record,
            Err(message) => {
                return (
                    failed_image_transfer_target(
                        target_machine.clone(),
                        ImageTransferFailureStage::Import,
                        "IMAGE_DISTRIBUTE_IMPORT_FAILED",
                        message,
                    ),
                    None,
                );
            }
        };

        (
            ImageTransferTargetResult::present(target_machine.clone(), record),
            Some(upload.bytes_uploaded),
        )
    }

    pub(super) async fn record_local_distributed_image_availability(
        &self,
        store: &dyn ImageAvailabilityStore,
        request: &ImageDistributeRequest,
        source_image: &RuntimeImage,
        operation_id: &str,
    ) -> Result<ImageAvailabilityRecord, String> {
        let now = now_unix_secs();
        let record = ImageAvailabilityRecord {
            machine_id: self.local_machine.clone(),
            digest: request.digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: image_ref_from_tag(request.digest.as_str(), request.digest.clone()),
                    platform: request.platform.clone().or(source_image.platform.clone()),
                    provenance: ImageArtifactProvenance::External {
                        source: Some(format!("image distribute from {}", request.source_machine)),
                    },
                    created_at: now,
                },
                recorded_at: now,
                source_operation_id: Some(operation_id.into()),
            },
            updated_at: now,
        };
        store
            .upsert_image_availability(&record)
            .await
            .map_err(|error| format!("record local image availability: {error}"))?;
        Ok(record)
    }

    pub(super) async fn push_archive_to_target(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: &MachineId,
        repository: &str,
        reference: &str,
        digest: &ployz_model::ImageDigest,
        platform: Option<ployz_model::ImagePlatform>,
        repo_tags: Vec<String>,
        archive: &ParsedImageArchive,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> Result<(ImageAvailabilityRecord, ReceiverUploadReport), ImageServiceResponse> {
        self.update_image_push_stage(
            operation_store,
            operation,
            "opening receive session",
            target_machine,
        )?;
        let session = self
            .image_receive_session_for_target(
                target_machine,
                &operation.id,
                repository.to_string(),
                peer_client,
            )
            .await
            .map_err(|message| {
                self.fail_image_push_operation(
                    operation_store,
                    operation,
                    target_machine.clone(),
                    message,
                )
            })?;

        self.update_image_push_stage(
            operation_store,
            operation,
            "uploading image blobs",
            target_machine,
        )?;
        let upload = upload_archive_to_receiver(&session, reference, archive)
            .await
            .map_err(|error| {
                self.fail_image_push_operation(
                    operation_store,
                    operation,
                    target_machine.clone(),
                    error.to_string(),
                )
            })?;

        self.update_image_push_stage(
            operation_store,
            operation,
            "importing target image",
            target_machine,
        )?;
        let record = self
            .import_received_image_for_target(
                target_machine,
                ImageReceivedImportRequest {
                    operation_id: operation.id.clone(),
                    source_machine: self.local_machine.clone(),
                    repository: repository.to_string(),
                    reference: reference.to_string(),
                    expected_digest: digest.clone(),
                    platform,
                    repo_tags,
                },
                backend,
                peer_client,
            )
            .await
            .map_err(|message| {
                self.fail_image_push_operation(
                    operation_store,
                    operation,
                    target_machine.clone(),
                    message,
                )
            })?;
        Ok((record, upload))
    }

    pub(super) async fn distribute_pushed_image_from_target(
        &self,
        source_machine: &MachineId,
        target_machine: &MachineId,
        digest: &ployz_model::ImageDigest,
        platform: Option<ployz_model::ImagePlatform>,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> ImageTransferTargetResult {
        let failure = |message: String| {
            failed_image_transfer_target(
                target_machine.clone(),
                ImageTransferFailureStage::DistributingPushedImage,
                "IMAGE_PUSH_TARGET_DISTRIBUTE_FAILED",
                message,
            )
        };
        let request = ImageDistributeRequest {
            digest: digest.clone(),
            source_machine: source_machine.clone(),
            target_machines: vec![target_machine.clone()],
            platform,
        };
        let response = if *source_machine == self.local_machine {
            self.handle_image_distribute_with_backend(&request, backend, peer_client)
                .await
        } else {
            match peer_client.image_distribute(source_machine, request).await {
                Ok(response) => response,
                Err(error) => {
                    return failure(format!(
                        "request image distribute from {source_machine}: {error}"
                    ));
                }
            }
        };
        if !response.is_ok() {
            if let Some(ImageServicePayload::ImageDistribute(payload)) = response.payload() {
                let [target] = payload.targets.as_slice() else {
                    return failure(format!(
                        "target image distribute failed [{}] with {} target results: {}",
                        response.code(),
                        payload.targets.len(),
                        response.message()
                    ));
                };
                return target.clone();
            }
            return failure(format!(
                "target image distribute failed [{}]: {}",
                response.code(),
                response.message()
            ));
        }
        let Some(ImageServicePayload::ImageDistribute(payload)) = response.payload() else {
            return failure("target image distribute response did not include a payload".into());
        };
        let [target] = payload.targets.as_slice() else {
            return failure(format!(
                "target image distribute returned {} target results",
                payload.targets.len()
            ));
        };
        target.clone()
    }

    pub(super) async fn import_received_image_for_target(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> Result<ImageAvailabilityRecord, String> {
        let response = if *target_machine == self.local_machine {
            self.handle_image_received_import_with_backend(&request, backend)
                .await
        } else {
            peer_client
                .image_received_import(target_machine, request)
                .await?
        };
        if !response.is_ok() {
            return Err(format!(
                "target image import failed [{}]: {}",
                response.code(),
                response.message()
            ));
        }
        let Some(ImageServicePayload::ImageReceivedImport(payload)) = response.payload() else {
            return Err("target image import response did not include an import payload".into());
        };
        Ok(payload.record)
    }
}

pub(super) fn default_receive_repository(operation_id: &str) -> String {
    let mut segment = String::with_capacity(operation_id.len().max(1));
    for ch in operation_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            segment.push(ch);
        } else {
            segment.push('-');
        }
    }
    if segment.is_empty() {
        segment.push_str("session");
    }
    format!("ployz/{segment}")
}

pub(super) fn failed_image_transfer_target(
    machine_id: MachineId,
    stage: ImageTransferFailureStage,
    code: impl Into<String>,
    message: String,
) -> ImageTransferTargetResult {
    let code = code.into();
    ImageTransferTargetResult::failed(
        machine_id,
        ImageTransferFailure {
            code,
            stage,
            message,
        },
    )
}

pub(super) fn image_transfer_target_failure_message(
    target: &ImageTransferTargetResult,
) -> Option<String> {
    target.failure().map(|failure| failure.message.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_receive_repository_sanitizes_operation_id() {
        assert_eq!(
            default_receive_repository("image push/../abc"),
            "ployz/image-push-..-abc"
        );
        assert_eq!(default_receive_repository(""), "ployz/session");
    }
}
