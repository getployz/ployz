mod operation_updates;
mod receive;
mod transfer;
mod validation;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::response::{ImageServicePayload, ImageServiceResponse};
use async_trait::async_trait;
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageDistributePayload, ImageDistributeRequest,
    ImageOperationKind, ImageOperationTargetOutcome, ImagePresence, ImagePushPayload,
    ImagePushRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, ImageRef,
    ImageTransferFailureStage, ImageTransferTargetResult, ImageTransferTargetStatus, MachineId,
    OperationStatus,
};
use ployz_runtime_api::{RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::{ImageAvailabilityStore, StoreDriver};
use ployz_time::now_unix_secs;

use crate::archive::parse_image_archive;
use crate::operations::ImageOperationStore;
use crate::registry::ImageRegistry;
use transfer::{
    default_receive_repository, failed_image_transfer_target, image_transfer_target_failure_message,
};
pub use validation::validate_image_distribute_request;

pub struct ImageService {
    pub local_machine: MachineId,
    pub data_dir: PathBuf,
    pub operation_store: ImageOperationStore,
    pub registry: ImageRegistry,
    pub store: StoreDriver,
    pub receiver_bind_addr: Option<SocketAddr>,
}

#[async_trait]
pub trait ImagePeerClient: Send + Sync {
    async fn image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageServiceResponse, String>;

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageServiceResponse, String>;

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageServiceResponse, String>;
}

impl ImageService {
    fn ok_with_payload(
        &self,
        message: impl Into<String>,
        payload: Option<ImageServicePayload>,
    ) -> ImageServiceResponse {
        ImageServiceResponse::success(message, payload)
    }

    fn err(&self, code: impl Into<String>, message: impl Into<String>) -> ImageServiceResponse {
        ImageServiceResponse::error(code, message, None)
    }

    fn err_with_payload(
        &self,
        code: impl Into<String>,
        message: impl Into<String>,
        payload: Option<ImageServicePayload>,
    ) -> ImageServiceResponse {
        ImageServiceResponse::error(code, message, payload)
    }

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

    pub async fn handle_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> ImageServiceResponse {
        if let Err(response) = validate_image_distribute_request(&self.local_machine, request) {
            return response;
        }
        self.handle_validated_image_distribute_with_backend(request, backend, peer_client)
            .await
    }

    async fn handle_validated_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> ImageServiceResponse {
        let image_store = &self.store;

        let operation_store = self.operation_store.clone();
        let mut operation = match operation_store.begin(
            ImageOperationKind::Distribute,
            "verifying source image",
            Some(request.digest.clone()),
            Some(request.source_machine.clone()),
            request.target_machines.clone(),
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error),
        };

        let mut skipped_present = BTreeMap::new();
        let mut missing_targets = Vec::new();
        for target_machine in &request.target_machines {
            match image_store
                .get_image_availability(target_machine, &request.digest)
                .await
            {
                Ok(Some(record)) if matches!(record.presence, ImagePresence::Present { .. }) => {
                    skipped_present.insert(target_machine.clone(), record);
                }
                Ok(_) => missing_targets.push(target_machine.clone()),
                Err(error) => {
                    let message =
                        format!("read image availability for target '{target_machine}': {error}");
                    return self.fail_all_image_distribute_targets(
                        &operation_store,
                        &mut operation,
                        request,
                        &skipped_present,
                        &BTreeMap::new(),
                        &BTreeMap::new(),
                        ImageTransferFailureStage::AvailabilityRead,
                        "IMAGE_DISTRIBUTE_AVAILABILITY_READ_FAILED",
                        message,
                    );
                }
            }
        }

        if missing_targets.is_empty() {
            let mut targets = Vec::with_capacity(request.target_machines.len());
            for target_machine in &request.target_machines {
                let Some(record) = skipped_present.get(target_machine).cloned() else {
                    return self.err(
                        "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                        format!("missing skipped-present record for target '{target_machine}'"),
                    );
                };
                if let Err(error) = operation_store.update_target(
                    &mut operation,
                    ImageOperationTargetOutcome::succeeded(target_machine.clone(), None),
                ) {
                    return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
                }
                targets.push(ImageTransferTargetResult::skipped_present(
                    target_machine.clone(),
                    record,
                ));
            }
            if let Err(error) =
                operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
            {
                return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
            }
            return self.ok_with_payload(
                format!(
                    "image {} already present on {} target(s)",
                    request.digest.as_str(),
                    targets.len()
                ),
                Some(ImageServicePayload::ImageDistribute(
                    ImageDistributePayload {
                        operation_id: operation.id,
                        digest: request.digest.clone(),
                        source_machine: request.source_machine.clone(),
                        targets,
                    },
                )),
            );
        }

        let source_reference = request.digest.as_str();
        let source_image = match backend
            .verify_image_digest(source_reference, &request.digest)
            .await
        {
            Ok(image) => image,
            Err(error) => {
                let message = format!("verify source image '{source_reference}': {error}");
                return self.fail_all_image_distribute_targets(
                    &operation_store,
                    &mut operation,
                    request,
                    &skipped_present,
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    ImageTransferFailureStage::SourceVerify,
                    "IMAGE_DISTRIBUTE_SOURCE_VERIFY_FAILED",
                    message,
                );
            }
        };

        let mut local_present = BTreeMap::new();
        let mut local_failures = BTreeMap::new();
        if missing_targets.contains(&self.local_machine) {
            match self
                .record_local_distributed_image_availability(
                    image_store,
                    request,
                    &source_image,
                    &operation.id,
                )
                .await
            {
                Ok(record) => {
                    local_present.insert(self.local_machine.clone(), record);
                }
                Err(message) => {
                    local_failures.insert(self.local_machine.clone(), message);
                }
            }
        }

        let transfer_targets = missing_targets
            .iter()
            .filter(|target_machine| **target_machine != self.local_machine)
            .cloned()
            .collect::<Vec<_>>();
        let mut archive = None;
        let mut work_dir = None;
        if !transfer_targets.is_empty() {
            if let Err(error) =
                operation_store.update_stage(&mut operation, "exporting source image")
            {
                return self.fail_all_image_distribute_targets(
                    &operation_store,
                    &mut operation,
                    request,
                    &skipped_present,
                    &local_present,
                    &local_failures,
                    ImageTransferFailureStage::SourceExport,
                    "IMAGE_DISTRIBUTE_STAGE_UPDATE_FAILED",
                    format!("update image distribute stage 'exporting source image': {error}"),
                );
            }
            let archive_reader = match backend.export_image_archive(source_reference).await {
                Ok(reader) => reader,
                Err(error) => {
                    let message = format!("export source image '{source_reference}': {error}");
                    return self.fail_all_image_distribute_targets(
                        &operation_store,
                        &mut operation,
                        request,
                        &skipped_present,
                        &local_present,
                        &local_failures,
                        ImageTransferFailureStage::SourceExport,
                        "IMAGE_DISTRIBUTE_SOURCE_EXPORT_FAILED",
                        message,
                    );
                }
            };
            let parse_work_dir = self
                .data_dir
                .join("image-transfer")
                .join(operation.id.clone());
            match parse_image_archive(archive_reader, &parse_work_dir).await {
                Ok(parsed) => {
                    archive = Some(parsed);
                    work_dir = Some(parse_work_dir);
                }
                Err(error) => {
                    cleanup_image_work_dir(&parse_work_dir).await;
                    return self.fail_all_image_distribute_targets(
                        &operation_store,
                        &mut operation,
                        request,
                        &skipped_present,
                        &local_present,
                        &local_failures,
                        ImageTransferFailureStage::ArchiveParse,
                        "IMAGE_DISTRIBUTE_ARCHIVE_PARSE_FAILED",
                        error.to_string(),
                    );
                }
            }
        }

        let mut targets = Vec::with_capacity(request.target_machines.len());
        for target_machine in &request.target_machines {
            let (target_result, bytes_transferred) = if let Some(record) =
                skipped_present.get(target_machine).cloned()
            {
                (
                    ImageTransferTargetResult::skipped_present(target_machine.clone(), record),
                    None,
                )
            } else if let Some(record) = local_present.get(target_machine).cloned() {
                (
                    ImageTransferTargetResult::present(target_machine.clone(), record),
                    None,
                )
            } else if let Some(message) = local_failures.get(target_machine).cloned() {
                (
                    failed_image_transfer_target(
                        target_machine.clone(),
                        ImageTransferFailureStage::LocalAvailability,
                        "IMAGE_DISTRIBUTE_LOCAL_AVAILABILITY_FAILED",
                        message,
                    ),
                    None,
                )
            } else if *target_machine == self.local_machine {
                match self
                    .record_local_distributed_image_availability(
                        image_store,
                        request,
                        &source_image,
                        &operation.id,
                    )
                    .await
                {
                    Ok(record) => (
                        ImageTransferTargetResult::present(target_machine.clone(), record),
                        None,
                    ),
                    Err(message) => (
                        failed_image_transfer_target(
                            target_machine.clone(),
                            ImageTransferFailureStage::LocalAvailability,
                            "IMAGE_DISTRIBUTE_LOCAL_AVAILABILITY_FAILED",
                            message,
                        ),
                        None,
                    ),
                }
            } else {
                let Some(parsed_archive) = archive.as_ref() else {
                    return self.err(
                        "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                        format!("missing parsed archive for transfer target '{target_machine}'"),
                    );
                };
                self.distribute_archive_to_target(
                    &operation_store,
                    &mut operation,
                    target_machine,
                    request,
                    parsed_archive,
                    backend,
                    peer_client,
                )
                .await
            };
            let outcome = match target_result.status() {
                ImageTransferTargetStatus::Present | ImageTransferTargetStatus::SkippedPresent => {
                    ImageOperationTargetOutcome::succeeded(
                        target_machine.clone(),
                        bytes_transferred,
                    )
                }
                ImageTransferTargetStatus::Failed => ImageOperationTargetOutcome::failed(
                    target_machine.clone(),
                    image_transfer_target_failure_message(&target_result).unwrap_or_else(|| {
                        format!("image distribute to target '{target_machine}' failed")
                    }),
                ),
            };
            if let Err(error) = operation_store.update_target(&mut operation, outcome) {
                if let Some(work_dir) = work_dir.as_deref() {
                    cleanup_image_work_dir(work_dir).await;
                }
                return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
            }
            targets.push(target_result);
        }
        if let Some(work_dir) = work_dir.as_deref() {
            cleanup_image_work_dir(work_dir).await;
        }

        let payload = ImageDistributePayload {
            operation_id: operation.id.clone(),
            digest: request.digest.clone(),
            source_machine: request.source_machine.clone(),
            targets,
        };
        let failed_targets = payload
            .targets
            .iter()
            .filter(|target| target.status() == ImageTransferTargetStatus::Failed)
            .count();
        if failed_targets > 0 {
            let message = format!(
                "image {} distributed with {failed_targets} failed target(s)",
                request.digest.as_str()
            );
            if let Err(error) = operation_store.update_status(
                &mut operation,
                OperationStatus::Failed,
                Some(message.clone()),
            ) {
                return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
            }
            let successful_targets = payload.targets.len() - failed_targets;
            let code = if successful_targets > 0 {
                "IMAGE_DISTRIBUTE_PARTIAL_FAILED"
            } else {
                "IMAGE_DISTRIBUTE_FAILED"
            };
            return self.err_with_payload(
                code,
                message,
                Some(ImageServicePayload::ImageDistribute(payload)),
            );
        }
        if let Err(error) =
            operation_store.update_status(&mut operation, OperationStatus::Succeeded, None)
        {
            return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
        }

        self.ok_with_payload(
            format!(
                "image {} distributed to {} target(s)",
                request.digest.as_str(),
                payload.targets.len()
            ),
            Some(ImageServicePayload::ImageDistribute(payload)),
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

fn image_ref_from_tag(reference: &str, digest: ployz_model::ImageDigest) -> ImageRef {
    if reference.starts_with("sha256:") {
        return ImageRef::digest_only(digest);
    }

    match reference.rsplit_once(':') {
        Some((repository, tag)) if !repository.is_empty() && !tag.is_empty() => {
            ImageRef::repository_digest(repository, Some(tag.to_string()), digest)
        }
        _ => ImageRef::repository_digest(reference, None, digest),
    }
}

async fn cleanup_image_work_dir(path: &Path) {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cleanup image work dir failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_model::ImageDigest;

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid image digest")
    }

    #[test]
    fn image_ref_from_tag_preserves_repository_and_tag() {
        let digest = digest();

        let by_digest = image_ref_from_tag(digest.as_str(), digest.clone());
        assert_eq!(by_digest, ImageRef::digest_only(digest.clone()));

        let tagged = image_ref_from_tag("registry.example.com/app:stable", digest.clone());
        assert_eq!(
            tagged,
            ImageRef::repository_digest("registry.example.com/app", Some("stable".into()), digest)
        );
    }
}
