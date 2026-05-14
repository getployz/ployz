use crate::archive::parse_image_archive;
use crate::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageOperationKind, ImageOperationTargetOutcome,
    ImagePushPayload, ImagePushRequest, ImageTransferTargetResult, ImageTransferTargetStatus,
    OperationStatus,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_time::now_unix_secs;

use super::transfer::{default_receive_repository, image_transfer_target_failure_message};
use super::{ImagePeerClient, ImageService, cleanup_image_work_dir, image_ref_from_tag};

impl ImageService {
    pub async fn handle_image_push_with_backend(
        &self,
        request: &ImagePushRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> ImageServiceResponse {
        let [first_target, rest_targets @ ..] = request.target_machines.as_slice() else {
            return self.err(
                "IMAGE_PUSH_TARGET_REQUIRED",
                "image push requires at least one target machine",
            );
        };

        let operation_store = self.operation_store.clone();
        let mut operation = match operation_store.begin(
            ImageOperationKind::Push,
            "verifying source image",
            request.expected_digest.clone(),
            Some(self.local_machine.clone()),
            request.target_machines.clone(),
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("IMAGE_PUSH_OPERATION_FAILED", error),
        };

        let (digest, source_image) = match resolve_push_source_image(
            backend,
            &request.source_image,
            &request.expected_digest,
        )
        .await
        {
            Ok(source) => source,
            Err(error) => {
                return self.fail_image_push_operation(
                    &operation_store,
                    &mut operation,
                    first_target.clone(),
                    format!("verify source image '{}': {error}", request.source_image),
                );
            }
        };
        if operation.digest.as_ref() != Some(&digest) {
            operation.digest = Some(digest.clone());
            if let Err(error) = operation_store.save(&operation) {
                return self.fail_image_push_operation(
                    &operation_store,
                    &mut operation,
                    first_target.clone(),
                    format!(
                        "persist resolved source image digest '{}': {error}",
                        digest.as_str()
                    ),
                );
            }
        }

        if let Err(error) = self.update_image_push_stage(
            &operation_store,
            &mut operation,
            "exporting source image",
            first_target,
        ) {
            return error;
        }
        let archive_reader = match backend.export_image_archive(&request.source_image).await {
            Ok(reader) => reader,
            Err(error) => {
                return self.fail_image_push_operation(
                    &operation_store,
                    &mut operation,
                    first_target.clone(),
                    format!("export source image '{}': {error}", request.source_image),
                );
            }
        };
        let work_dir = self.data_dir.join("image-push").join(operation.id.clone());
        let archive = match parse_image_archive(archive_reader, &work_dir).await {
            Ok(archive) => archive,
            Err(error) => {
                cleanup_image_work_dir(&work_dir).await;
                return self.fail_image_push_operation(
                    &operation_store,
                    &mut operation,
                    first_target.clone(),
                    error.to_string(),
                );
            }
        };

        let repository = default_receive_repository(&operation.id);
        let reference = operation.id.clone();
        let (first_record, first_upload) = match self
            .push_archive_to_target(
                &operation_store,
                &mut operation,
                first_target,
                &repository,
                &reference,
                &digest,
                request.platform.clone().or(source_image.platform.clone()),
                archive.repo_tags.clone(),
                &archive,
                backend,
                peer_client,
            )
            .await
        {
            Ok(result) => result,
            Err(response) => {
                cleanup_image_work_dir(&work_dir).await;
                return response;
            }
        };
        cleanup_image_work_dir(&work_dir).await;

        let mut targets = vec![ImageTransferTargetResult::present(
            first_target.clone(),
            first_record,
        )];
        if let Err(error) = operation_store.update_target(
            &mut operation,
            ImageOperationTargetOutcome::succeeded(
                first_target.clone(),
                Some(first_upload.bytes_uploaded),
            ),
        ) {
            return self.err("IMAGE_PUSH_OPERATION_FAILED", error);
        }

        let platform = request.platform.clone().or(source_image.platform);
        for target in rest_targets {
            if let Err(error) = self.update_image_push_stage(
                &operation_store,
                &mut operation,
                "distributing pushed image",
                target,
            ) {
                return error;
            }
            let target_result = self
                .distribute_pushed_image_from_target(
                    first_target,
                    target,
                    &digest,
                    platform.clone(),
                    backend,
                    peer_client,
                )
                .await;
            match target_result.status() {
                ImageTransferTargetStatus::Present | ImageTransferTargetStatus::SkippedPresent => {
                    if let Err(error) = operation_store.update_target(
                        &mut operation,
                        ImageOperationTargetOutcome::succeeded(target.clone(), None),
                    ) {
                        return self.err("IMAGE_PUSH_OPERATION_FAILED", error);
                    }
                }
                ImageTransferTargetStatus::Failed => {
                    let message = image_transfer_target_failure_message(&target_result)
                        .unwrap_or_else(|| format!("image distribute to target '{target}' failed"));
                    if let Err(error) = operation_store.update_target(
                        &mut operation,
                        ImageOperationTargetOutcome::failed(target.clone(), message.clone()),
                    ) {
                        return self.err(
                            "IMAGE_PUSH_OPERATION_FAILED",
                            format!(
                                "image push failed for target '{target}': {message}; also failed to persist target failure: {error}"
                            ),
                        );
                    }
                }
            }
            targets.push(target_result);
        }

        let artifact = ImageArtifact {
            image: image_ref_from_tag(&request.source_image, digest.clone()),
            platform,
            provenance: ImageArtifactProvenance::External {
                source: Some(format!("image push from {}", self.local_machine)),
            },
            created_at: now_unix_secs(),
        };
        let payload = ImagePushPayload {
            operation_id: operation.id.clone(),
            artifact,
            targets,
        };
        let failed_targets = payload
            .targets
            .iter()
            .filter(|target| target.status() == ImageTransferTargetStatus::Failed)
            .count();
        if failed_targets > 0 {
            let message = format!(
                "image {} pushed with {failed_targets} failed target(s)",
                digest.as_str()
            );
            if let Err(error) = operation_store.update_status(
                &mut operation,
                OperationStatus::Failed,
                Some(message.clone()),
            ) {
                return self.err("IMAGE_PUSH_OPERATION_FAILED", error);
            }
            return self.err_with_payload(
                "IMAGE_PUSH_PARTIAL_FAILED",
                message,
                Some(ImageServicePayload::ImagePush(payload)),
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
        {
            return self.err("IMAGE_PUSH_OPERATION_FAILED", error);
        }
        self.ok_with_payload(
            format!(
                "image {} pushed to {} target(s)",
                digest.as_str(),
                payload.targets.len()
            ),
            Some(ImageServicePayload::ImagePush(payload)),
        )
    }
}

async fn resolve_push_source_image(
    backend: &dyn RuntimeImageBackend,
    source_image: &str,
    expected_digest: &Option<ployz_model::ImageDigest>,
) -> Result<(ployz_model::ImageDigest, RuntimeImage), RuntimeImageError> {
    if let Some(expected_digest) = expected_digest {
        let image = backend
            .verify_image_digest(source_image, expected_digest)
            .await?;
        let digest = push_runtime_image_identity(source_image, &image)?;
        return Ok((digest, image));
    }
    let Some(image) = backend.inspect_image(source_image).await? else {
        return Err(RuntimeImageError::NotFound {
            reference: source_image.into(),
        });
    };
    let digest = push_runtime_image_identity(source_image, &image)?;
    Ok((digest, image))
}

fn push_runtime_image_identity(
    source_image: &str,
    image: &RuntimeImage,
) -> Result<ployz_model::ImageDigest, RuntimeImageError> {
    let Some(id) = image.id.as_deref() else {
        return Err(RuntimeImageError::MissingDigest {
            reference: source_image.into(),
        });
    };
    ployz_model::ImageDigest::try_new(id).map_err(|_| RuntimeImageError::MissingDigest {
        reference: source_image.into(),
    })
}
