use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;

use async_trait::async_trait;
use ployz_api::{
    DaemonPayload, DaemonResponse, ImageDistributePayload, ImageDistributeRequest,
    ImageDistributeValidationFailure, ImageDistributeValidationPayload, ImagePushPayload,
    ImagePushRequest, ImageReceiveSessionPayload, ImageReceiveSessionRequest,
    ImageReceivedImportPayload, ImageReceivedImportRequest, ImageTransferFailure,
    ImageTransferFailureStage, ImageTransferTargetResult, ImageTransferTargetStatus,
};
use ployz_model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageOperationKind,
    ImageOperationRecord, ImageOperationTargetOutcome, ImagePresence, ImageRef, MachineId,
    OperationStatus,
};
use ployz_runtime_api::{ImageArchiveReader, RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
use ployz_time::now_unix_secs;

use crate::archive::{
    ParsedImageArchive, ReceiverUploadReport, parse_image_archive, reconstruct_received_archive,
    upload_archive_to_receiver,
};
use crate::operations::{ImageOperationStore, validate_operation_id};
use crate::registry::{
    ImageRegistry, REGISTRY_OPERATION_HEADER, REGISTRY_SESSION_HEADER,
    REGISTRY_SOURCE_MACHINE_HEADER, validate_repository,
};

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
    ) -> Result<DaemonResponse, String>;

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<DaemonResponse, String>;

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<DaemonResponse, String>;
}

impl ImageService {
    fn ok_with_payload(
        &self,
        message: impl Into<String>,
        payload: Option<DaemonPayload>,
    ) -> DaemonResponse {
        DaemonResponse::success(message, payload)
    }

    fn err(&self, code: impl Into<String>, message: impl Into<String>) -> DaemonResponse {
        DaemonResponse::error(code, message, None)
    }

    fn err_with_payload(
        &self,
        code: impl Into<String>,
        message: impl Into<String>,
        payload: Option<DaemonPayload>,
    ) -> DaemonResponse {
        DaemonResponse::error(code, message, payload)
    }

    pub async fn handle_image_push_with_backend(
        &self,
        request: &ImagePushRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> DaemonResponse {
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
                Some(DaemonPayload::ImagePush(payload)),
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
            Some(DaemonPayload::ImagePush(payload)),
        )
    }

    pub async fn handle_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> DaemonResponse {
        if let Err(response) = validate_image_distribute_request(self.local_machine, request) {
            return response;
        }
        self.handle_validated_image_distribute_with_backend(request, backend)
            .await
    }

    async fn handle_validated_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> DaemonResponse {
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
                Some(DaemonPayload::ImageDistribute(ImageDistributePayload {
                    operation_id: operation.id,
                    digest: request.digest.clone(),
                    source_machine: request.source_machine.clone(),
                    targets,
                })),
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
        if missing_targets
            .iter()
            .any(|target_machine| target_machine == self.local_machine)
        {
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
            .filter(|target_machine| *target_machine != self.local_machine)
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
            } else if target_machine == self.local_machine {
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
                Some(DaemonPayload::ImageDistribute(payload)),
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
            Some(DaemonPayload::ImageDistribute(payload)),
        )
    }

    pub async fn handle_image_receive_session(
        &self,
        request: &ImageReceiveSessionRequest,
    ) -> DaemonResponse {
        let Some(bind_addr) = self.receiver_bind_addr else {
            return self.err(
                "IMAGE_RECEIVER_INACTIVE",
                "image receiver listener is not running",
            );
        };
        let repository = request
            .repository
            .clone()
            .unwrap_or_else(|| default_receive_repository(&request.operation_id));
        if let Err(error) = validate_repository(&repository) {
            return self.err("IMAGE_RECEIVER_INVALID_REPOSITORY", error.to_string());
        }
        let machines = match self.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "IMAGE_RECEIVER_SOURCE_LOOKUP_FAILED",
                    format!("list machines for image receive source validation: {error}"),
                );
            }
        };
        if !machines
            .iter()
            .any(|machine| machine.id == request.source_machine)
        {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_UNKNOWN",
                format!(
                    "source machine '{}' is not a cluster member",
                    request.source_machine
                ),
            );
        }
        if bind_addr.ip().is_loopback() && &request.source_machine != self.local_machine {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_NOT_LOCAL",
                format!(
                    "image receiver is bound to loopback; source machine '{}' must match local machine '{}'",
                    request.source_machine, self.local_machine
                ),
            );
        }
        let session = self
            .registry
            .register_session(
                &request.operation_id,
                request.source_machine.clone(),
                repository.clone(),
            )
            .await;
        let mut headers = BTreeMap::new();
        headers.insert(
            REGISTRY_OPERATION_HEADER.to_string(),
            session.operation_id.clone(),
        );
        headers.insert(
            REGISTRY_SOURCE_MACHINE_HEADER.to_string(),
            session.source_machine.as_str().to_string(),
        );
        headers.insert(REGISTRY_SESSION_HEADER.to_string(), session.token.clone());
        let payload = ImageReceiveSessionPayload {
            target_machine: self.local_machine.clone(),
            endpoint: receiver_endpoint(bind_addr, &session.repository),
            token: session.token,
            expires_at_unix_secs: session.expires_at_unix_secs,
            headers,
        };

        self.ok_with_payload(
            "image receive session created",
            Some(DaemonPayload::ImageReceiveSession(payload)),
        )
    }

    pub async fn handle_image_received_import_with_backend(
        &self,
        request: &ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> DaemonResponse {
        if let Err(error) = validate_operation_id(&request.operation_id) {
            return self.err("IMAGE_RECEIVED_IMPORT_INVALID_OPERATION", error);
        }
        if let Err(error) = validate_repository(&request.repository) {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_INVALID_REPOSITORY",
                error.to_string(),
            );
        }
        let archive_path = self
            .data_dir
            .join("image-import")
            .join(request.operation_id.clone())
            .join("received.tar");
        let import_dir = archive_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                self.data_dir
                    .join("image-import")
                    .join(&request.operation_id)
            });
        let archive_path = match reconstruct_received_archive(
            &self.registry,
            &request.repository,
            &request.reference,
            &request.repo_tags,
            archive_path,
        )
        .await
        {
            Ok(path) => path,
            Err(error) => {
                cleanup_image_work_dir(&import_dir).await;
                return self.err(
                    "IMAGE_RECEIVED_IMPORT_RECONSTRUCT_FAILED",
                    error.to_string(),
                );
            }
        };
        let archive = match tokio::fs::File::open(&archive_path).await {
            Ok(file) => Box::pin(file) as ImageArchiveReader,
            Err(error) => {
                cleanup_image_work_dir(&import_dir).await;
                return self.err(
                    "IMAGE_RECEIVED_IMPORT_ARCHIVE_OPEN_FAILED",
                    format!(
                        "open received image archive '{}': {error}",
                        archive_path.display()
                    ),
                );
            }
        };
        if let Err(error) = backend.import_image_archive(archive).await {
            cleanup_image_work_dir(&import_dir).await;
            return self.err(
                "IMAGE_RECEIVED_IMPORT_RUNTIME_FAILED",
                format!("import received image archive: {error}"),
            );
        }
        cleanup_image_work_dir(&import_dir).await;
        let verify_reference = request
            .repo_tags
            .first()
            .map(String::as_str)
            .unwrap_or_else(|| request.expected_digest.as_str());
        if let Err(error) = backend
            .verify_image_digest(verify_reference, &request.expected_digest)
            .await
        {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_VERIFY_FAILED",
                format!("verify imported image '{verify_reference}': {error}"),
            );
        }
        let now = now_unix_secs();
        let record = ImageAvailabilityRecord {
            machine_id: self.local_machine.clone(),
            digest: request.expected_digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: image_ref_from_tag(verify_reference, request.expected_digest.clone()),
                    platform: request.platform.clone(),
                    provenance: ImageArtifactProvenance::External {
                        source: Some(format!("image distribute from {}", request.source_machine)),
                    },
                    created_at: now,
                },
                recorded_at: now,
                source_operation_id: Some(request.operation_id.clone()),
            },
            updated_at: now,
        };
        if let Err(error) = self.store.upsert_image_availability(&record).await {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_AVAILABILITY_FAILED",
                format!("record imported image availability: {error}"),
            );
        }
        self.ok_with_payload(
            format!(
                "image {} imported on {}",
                request.expected_digest.as_str(),
                self.local_machine
            ),
            Some(DaemonPayload::ImageReceivedImport(
                ImageReceivedImportPayload {
                    target_machine: self.local_machine.clone(),
                    record,
                },
            )),
        )
    }

    fn fail_image_distribute_operation(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: MachineId,
        message: String,
    ) -> DaemonResponse {
        if let Err(error) = operation_store.update_target(
            operation,
            ImageOperationTargetOutcome::failed(target_machine, message.clone()),
        ) {
            return self.err(
                "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                format!("image distribute failed: {message}; also failed to persist target failure: {error}"),
            );
        }
        if let Err(error) =
            operation_store.update_status(operation, OperationStatus::Failed, Some(message.clone()))
        {
            return self.err(
                "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                format!("image distribute failed: {message}; also failed to persist operation failure: {error}"),
            );
        }
        self.err("IMAGE_DISTRIBUTE_FAILED", message)
    }

    fn fail_all_image_distribute_targets(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        request: &ImageDistributeRequest,
        skipped_present: &BTreeMap<MachineId, ImageAvailabilityRecord>,
        local_present: &BTreeMap<MachineId, ImageAvailabilityRecord>,
        local_failures: &BTreeMap<MachineId, String>,
        stage: ImageTransferFailureStage,
        code: &'static str,
        message: String,
    ) -> DaemonResponse {
        let mut targets = Vec::with_capacity(request.target_machines.len());
        for target_machine in &request.target_machines {
            let (result, status, last_error) =
                if let Some(record) = skipped_present.get(target_machine) {
                    (
                        ImageTransferTargetResult::skipped_present(
                            target_machine.clone(),
                            record.clone(),
                        ),
                        OperationStatus::Succeeded,
                        None,
                    )
                } else if let Some(record) = local_present.get(target_machine) {
                    (
                        ImageTransferTargetResult::present(target_machine.clone(), record.clone()),
                        OperationStatus::Succeeded,
                        None,
                    )
                } else if let Some(local_message) = local_failures.get(target_machine) {
                    (
                        failed_image_transfer_target(
                            target_machine.clone(),
                            ImageTransferFailureStage::LocalAvailability,
                            "IMAGE_DISTRIBUTE_LOCAL_AVAILABILITY_FAILED",
                            local_message.clone(),
                        ),
                        OperationStatus::Failed,
                        Some(local_message.clone()),
                    )
                } else {
                    (
                        failed_image_transfer_target(
                            target_machine.clone(),
                            stage,
                            code,
                            message.clone(),
                        ),
                        OperationStatus::Failed,
                        Some(message.clone()),
                    )
                };
            let outcome = match status {
                OperationStatus::Succeeded => {
                    ImageOperationTargetOutcome::succeeded(target_machine.clone(), None)
                }
                OperationStatus::Failed => ImageOperationTargetOutcome::failed(
                    target_machine.clone(),
                    last_error.unwrap_or_else(|| message.clone()),
                ),
                OperationStatus::Interrupted => {
                    ImageOperationTargetOutcome::interrupted(target_machine.clone(), last_error)
                }
                OperationStatus::Running => {
                    ImageOperationTargetOutcome::running(target_machine.clone())
                }
            };
            if let Err(error) = operation_store.update_target(operation, outcome.clone()) {
                return self.err(
                    "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                    format!(
                        "image distribute failed: {message}; also failed to persist target failure: {error}"
                    ),
                );
            }
            targets.push(result);
        }
        if let Err(error) =
            operation_store.update_status(operation, OperationStatus::Failed, Some(message.clone()))
        {
            return self.err(
                "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                format!(
                    "image distribute failed: {message}; also failed to persist operation failure: {error}"
                ),
            );
        }
        let code = if skipped_present.is_empty() && local_present.is_empty() {
            "IMAGE_DISTRIBUTE_FAILED"
        } else {
            "IMAGE_DISTRIBUTE_PARTIAL_FAILED"
        };
        self.err_with_payload(
            code,
            message,
            Some(DaemonPayload::ImageDistribute(ImageDistributePayload {
                operation_id: operation.id.clone(),
                digest: request.digest.clone(),
                source_machine: request.source_machine.clone(),
                targets,
            })),
        )
    }

    fn fail_image_push_operation(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: MachineId,
        message: String,
    ) -> DaemonResponse {
        if let Err(error) = operation_store.update_target(
            operation,
            ImageOperationTargetOutcome::failed(target_machine, message.clone()),
        ) {
            return self.err(
                "IMAGE_PUSH_OPERATION_FAILED",
                format!(
                    "image push failed: {message}; also failed to persist target failure: {error}"
                ),
            );
        }
        if let Err(error) =
            operation_store.update_status(operation, OperationStatus::Failed, Some(message.clone()))
        {
            return self.err(
                "IMAGE_PUSH_OPERATION_FAILED",
                format!(
                    "image push failed: {message}; also failed to persist operation failure: {error}"
                ),
            );
        }
        self.err("IMAGE_PUSH_FAILED", message)
    }

    fn update_image_push_stage(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        stage: &str,
        target_machine: &MachineId,
    ) -> Result<(), DaemonResponse> {
        operation_store
            .update_stage(operation, stage)
            .map_err(|error| {
                self.fail_image_push_operation(
                    operation_store,
                    operation,
                    target_machine.clone(),
                    format!("update image push stage '{stage}': {error}"),
                )
            })
    }

    fn update_image_distribute_stage(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        stage: &str,
        target_machine: &MachineId,
    ) -> Result<(), DaemonResponse> {
        operation_store
            .update_stage(operation, stage)
            .map_err(|error| {
                self.fail_image_distribute_operation(
                    operation_store,
                    operation,
                    target_machine.clone(),
                    format!("update image distribute stage '{stage}': {error}"),
                )
            })
    }

    async fn image_receive_session_for_target(
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
        let response = if target_machine == self.local_machine {
            self.handle_image_receive_session(&request).await
        } else {
            peer_client.image_receive_session(target_machine, request).await?
        };
        if !response.is_ok() {
            return Err(format!(
                "target receive session failed [{}]: {}",
                response.code(),
                response.message().to_string()
            ));
        }
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload() else {
            return Err("target receive session response did not include a session payload".into());
        };
        Ok(payload)
    }

    async fn distribute_archive_to_target(
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

    async fn record_local_distributed_image_availability(
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

    async fn push_archive_to_target(
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
    ) -> Result<(ImageAvailabilityRecord, ReceiverUploadReport), DaemonResponse> {
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

    async fn distribute_pushed_image_from_target(
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
        let response = if source_machine == self.local_machine {
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
            if let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() {
                let [target] = payload.targets.as_slice() else {
                    return failure(format!(
                        "target image distribute failed [{}] with {} target results: {}",
                        response.code(),
                        payload.targets.len(),
                        response.message().to_string()
                    ));
                };
                return target.clone();
            }
            return failure(format!(
                "target image distribute failed [{}]: {}",
                response.code(),
                response.message().to_string()
            ));
        }
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload() else {
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

    async fn import_received_image_for_target(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
        peer_client: &dyn ImagePeerClient,
    ) -> Result<ImageAvailabilityRecord, String> {
        let response = if target_machine == self.local_machine {
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
                response.message().to_string()
            ));
        }
        let Some(DaemonPayload::ImageReceivedImport(payload)) = response.payload() else {
            return Err("target image import response did not include an import payload".into());
        };
        Ok(payload.record)
    }
}

pub fn validate_image_distribute_request(
    local_machine: &MachineId,
    request: &ImageDistributeRequest,
) -> Result<(), DaemonResponse> {
    if request.target_machines.is_empty() {
        return Err(DaemonResponse::error(
            "IMAGE_DISTRIBUTE_TARGET_REQUIRED",
            "image distribute requires at least one target machine",
            Some(DaemonPayload::ImageDistributeValidation(
                image_distribute_validation_payload(
                    request,
                    ImageDistributeValidationFailure::TargetRequired {
                        target_count: request.target_machines.len(),
                    },
                ),
            )),
        ));
    }
    if let Some(duplicate) = first_duplicate_machine(&request.target_machines) {
        return Err(DaemonResponse::error(
            "IMAGE_DISTRIBUTE_DUPLICATE_TARGET",
            format!("image distribute target '{duplicate}' was provided more than once"),
            Some(DaemonPayload::ImageDistributeValidation(
                image_distribute_validation_payload(
                    request,
                    ImageDistributeValidationFailure::DuplicateTarget {
                        duplicate_target: duplicate,
                    },
                ),
            )),
        ));
    }
    if &request.source_machine != local_machine {
        return Err(DaemonResponse::error(
            "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL",
            format!(
                "image distribute source '{}' must match local machine '{}'",
                request.source_machine, local_machine
            ),
            Some(DaemonPayload::ImageDistributeValidation(
                image_distribute_validation_payload(
                    request,
                    ImageDistributeValidationFailure::SourceNotLocal {
                        source_machine: request.source_machine.clone(),
                        local_machine: local_machine.clone(),
                    },
                ),
            )),
        ));
    }
    Ok(())
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

fn receiver_endpoint(bind_addr: SocketAddr, repository: &str) -> String {
    format!("http://{bind_addr}/v2/{repository}")
}

fn default_receive_repository(operation_id: &str) -> String {
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

fn first_duplicate_machine(machines: &[MachineId]) -> Option<MachineId> {
    let mut seen = BTreeSet::new();
    machines
        .iter()
        .find(|machine| !seen.insert((*machine).clone()))
        .cloned()
}

fn failed_image_transfer_target(
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

fn image_transfer_target_failure_message(target: &ImageTransferTargetResult) -> Option<String> {
    target.failure().map(|failure| failure.message.clone())
}

fn image_distribute_validation_payload(
    request: &ImageDistributeRequest,
    failure: ImageDistributeValidationFailure,
) -> ImageDistributeValidationPayload {
    ImageDistributeValidationPayload {
        request: request.clone(),
        failure,
    }
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
    use ployz_api::ImageDistributeValidationFailure;
    use ployz_model::ImageDigest;

    fn machine(id: &str) -> MachineId {
        MachineId::new(id)
    }

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid image digest")
    }

    fn distribute_request(
        source_machine: MachineId,
        target_machines: Vec<MachineId>,
    ) -> ImageDistributeRequest {
        ImageDistributeRequest {
            digest: digest(),
            source_machine,
            target_machines,
            platform: None,
        }
    }

    fn validation_failure(response: &DaemonResponse) -> ImageDistributeValidationFailure {
        let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload() else {
            panic!("expected image distribute validation payload");
        };
        payload.failure.clone()
    }

    #[test]
    fn validate_image_distribute_request_rejects_empty_targets() {
        let local_machine = machine("local");
        let request = distribute_request(local_machine.clone(), vec![]);

        let response =
            validate_image_distribute_request(&local_machine, &request).expect_err("invalid");

        assert_eq!(response.code(), "IMAGE_DISTRIBUTE_TARGET_REQUIRED");
        assert_eq!(
            validation_failure(&response),
            ImageDistributeValidationFailure::TargetRequired { target_count: 0 }
        );
    }

    #[test]
    fn validate_image_distribute_request_rejects_duplicate_targets() {
        let local_machine = machine("local");
        let target = machine("target");
        let request =
            distribute_request(local_machine.clone(), vec![target.clone(), target.clone()]);

        let response =
            validate_image_distribute_request(&local_machine, &request).expect_err("invalid");

        assert_eq!(response.code(), "IMAGE_DISTRIBUTE_DUPLICATE_TARGET");
        assert_eq!(
            validation_failure(&response),
            ImageDistributeValidationFailure::DuplicateTarget {
                duplicate_target: target
            }
        );
    }

    #[test]
    fn validate_image_distribute_request_rejects_remote_source() {
        let local_machine = machine("local");
        let source_machine = machine("source");
        let request = distribute_request(source_machine.clone(), vec![machine("target")]);

        let response =
            validate_image_distribute_request(&local_machine, &request).expect_err("invalid");

        assert_eq!(response.code(), "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL");
        assert_eq!(
            validation_failure(&response),
            ImageDistributeValidationFailure::SourceNotLocal {
                source_machine,
                local_machine
            }
        );
    }

    #[test]
    fn default_receive_repository_sanitizes_operation_id() {
        assert_eq!(
            default_receive_repository("image push/../abc"),
            "ployz/image-push-..-abc"
        );
        assert_eq!(default_receive_repository(""), "ployz/session");
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
