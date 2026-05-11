use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use ployz_api::{
    DaemonPayload, DaemonRequest, ImageDistributePayload, ImageDistributeRequest,
    ImageDistributeValidationFailure, ImageDistributeValidationPayload, ImagePushPayload,
    ImagePushRequest, ImageReceiveSessionPayload, ImageReceiveSessionRequest,
    ImageReceivedImportPayload, ImageReceivedImportRequest, ImageTransferFailure,
    ImageTransferFailureStage, ImageTransferTargetResult, ImageTransferTargetStatus,
};
use ployz_nats::{NodeCommandSubject, RpcPolicy};
use ployz_runtime_api::{ImageArchiveReader, RuntimeImage, RuntimeImageBackend, RuntimeImageError};
use ployz_store_api::{ImageAvailabilityStore, MachineMembershipStore};
use ployz_types::model::{
    ImageArtifact, ImageArtifactProvenance, ImageAvailabilityRecord, ImageOperationKind,
    ImageOperationRecord, ImageOperationTargetOutcome, ImagePresence, ImageRef, MachineId,
    OperationStatus,
};
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;
use crate::daemon::handlers::image::archive::{
    parse_image_archive, reconstruct_received_archive, upload_archive_to_receiver,
};
use crate::daemon::handlers::image::operations::{ImageOperationStore, validate_operation_id};
use crate::daemon::handlers::image::registry::{
    REGISTRY_OPERATION_HEADER, REGISTRY_SESSION_HEADER, REGISTRY_SOURCE_MACHINE_HEADER,
    validate_repository,
};

const IMAGE_RECEIVED_IMPORT_RPC_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const IMAGE_DISTRIBUTE_RPC_TIMEOUT: Duration = Duration::from_secs(30 * 60);

impl DaemonState {
    pub(crate) async fn handle_image_push(
        &self,
        request: &ImagePushRequest,
    ) -> ployz_api::DaemonResponse {
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_PUSH_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_push_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_push_with_backend(
        &self,
        request: &ImagePushRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> ployz_api::DaemonResponse {
        let [first_target, rest_targets @ ..] = request.target_machines.as_slice() else {
            return self.err(
                "IMAGE_PUSH_TARGET_REQUIRED",
                "image push requires at least one target machine",
            );
        };
        if let Err(response) =
            self.require_active("IMAGE_PUSH_INACTIVE", "image push requires a running mesh")
        {
            return *response;
        }

        let operation_store = self.image_operation_store();
        let mut operation = match operation_store.begin(
            ImageOperationKind::Push,
            "verifying source image",
            request.expected_digest.clone(),
            Some(self.identity.machine_id.clone()),
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

        let mut targets = vec![ImageTransferTargetResult {
            machine_id: first_target.clone(),
            status: ImageTransferTargetStatus::Present,
            record: Some(first_record),
            failure: None,
            error: None,
        }];
        if let Err(error) = operation_store.update_target(
            &mut operation,
            ImageOperationTargetOutcome {
                machine_id: first_target.clone(),
                status: OperationStatus::Succeeded,
                bytes_transferred: Some(first_upload.bytes_uploaded),
                last_error: None,
            },
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
            match self
                .distribute_pushed_image_from_target(
                    first_target,
                    target,
                    &digest,
                    platform.clone(),
                    backend,
                )
                .await
            {
                Ok(target_result) => {
                    if let Err(error) = operation_store.update_target(
                        &mut operation,
                        ImageOperationTargetOutcome {
                            machine_id: target.clone(),
                            status: OperationStatus::Succeeded,
                            bytes_transferred: None,
                            last_error: None,
                        },
                    ) {
                        return self.err("IMAGE_PUSH_OPERATION_FAILED", error);
                    }
                    targets.push(target_result);
                }
                Err(target_result) => {
                    let message = target_result
                        .failure
                        .as_ref()
                        .map(|failure| failure.message.clone())
                        .or_else(|| target_result.error.clone())
                        .unwrap_or_else(|| format!("image distribute to target '{target}' failed"));
                    if let Err(error) = operation_store.update_target(
                        &mut operation,
                        ImageOperationTargetOutcome {
                            machine_id: target.clone(),
                            status: OperationStatus::Failed,
                            bytes_transferred: None,
                            last_error: Some(message.clone()),
                        },
                    ) {
                        return self.err(
                            "IMAGE_PUSH_OPERATION_FAILED",
                            format!(
                                "image push failed for target '{target}': {message}; also failed to persist target failure: {error}"
                            ),
                        );
                    }
                    targets.push(target_result);
                }
            }
        }

        let artifact = ImageArtifact {
            image: image_ref_from_tag(&request.source_image, digest.clone()),
            platform,
            provenance: ImageArtifactProvenance::External {
                source: Some(format!("image push from {}", self.identity.machine_id)),
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
            .filter(|target| target.status == ImageTransferTargetStatus::Failed)
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

    pub(crate) async fn handle_image_distribute(
        &self,
        request: &ImageDistributeRequest,
    ) -> ployz_api::DaemonResponse {
        if let Err(response) = self.validate_image_distribute_request(request) {
            return response;
        }
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_DISTRIBUTE_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_distribute_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> ployz_api::DaemonResponse {
        if let Err(response) = self.validate_image_distribute_request(request) {
            return response;
        }
        let active = match self.require_active(
            "IMAGE_DISTRIBUTE_INACTIVE",
            "image distribute requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let image_store = active.mesh.store.clone();

        let operation_store = self.image_operation_store();
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
                    ImageOperationTargetOutcome {
                        machine_id: target_machine.clone(),
                        status: OperationStatus::Succeeded,
                        bytes_transferred: None,
                        last_error: None,
                    },
                ) {
                    return self.err("IMAGE_DISTRIBUTE_OPERATION_FAILED", error);
                }
                targets.push(ImageTransferTargetResult {
                    machine_id: target_machine.clone(),
                    status: ImageTransferTargetStatus::SkippedPresent,
                    record: Some(record),
                    failure: None,
                    error: None,
                });
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
                    ImageTransferFailureStage::SourceVerify,
                    "IMAGE_DISTRIBUTE_SOURCE_VERIFY_FAILED",
                    message,
                );
            }
        };

        let mut local_present = BTreeMap::new();
        if missing_targets
            .iter()
            .any(|target_machine| target_machine == &self.identity.machine_id)
        {
            if let Ok(record) = self
                .record_local_distributed_image_availability(
                    &image_store,
                    request,
                    &source_image,
                    &operation.id,
                )
                .await
            {
                local_present.insert(self.identity.machine_id.clone(), record);
            }
        }

        let transfer_targets = missing_targets
            .iter()
            .filter(|target_machine| *target_machine != &self.identity.machine_id)
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
                    ImageTransferTargetResult {
                        machine_id: target_machine.clone(),
                        status: ImageTransferTargetStatus::SkippedPresent,
                        record: Some(record),
                        failure: None,
                        error: None,
                    },
                    None,
                )
            } else if let Some(record) = local_present.get(target_machine).cloned() {
                (
                    ImageTransferTargetResult {
                        machine_id: target_machine.clone(),
                        status: ImageTransferTargetStatus::Present,
                        record: Some(record),
                        failure: None,
                        error: None,
                    },
                    None,
                )
            } else if target_machine == &self.identity.machine_id {
                match self
                    .record_local_distributed_image_availability(
                        &image_store,
                        request,
                        &source_image,
                        &operation.id,
                    )
                    .await
                {
                    Ok(record) => (
                        ImageTransferTargetResult {
                            machine_id: target_machine.clone(),
                            status: ImageTransferTargetStatus::Present,
                            record: Some(record),
                            failure: None,
                            error: None,
                        },
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
                )
                .await
            };
            let operation_status = match target_result.status {
                ImageTransferTargetStatus::Present | ImageTransferTargetStatus::SkippedPresent => {
                    OperationStatus::Succeeded
                }
                ImageTransferTargetStatus::Failed => OperationStatus::Failed,
            };
            if let Err(error) = operation_store.update_target(
                &mut operation,
                ImageOperationTargetOutcome {
                    machine_id: target_machine.clone(),
                    status: operation_status,
                    bytes_transferred,
                    last_error: target_result.error.clone(),
                },
            ) {
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
            .filter(|target| target.status == ImageTransferTargetStatus::Failed)
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

    pub(crate) async fn handle_image_receive_session(
        &self,
        request: &ImageReceiveSessionRequest,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "IMAGE_RECEIVER_INACTIVE",
            "image receive session requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let Some(bind_addr) = active.image_receiver_bind_addr else {
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
        let machines = match active.mesh.store.list_machines().await {
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
        if bind_addr.ip().is_loopback() && request.source_machine != self.identity.machine_id {
            return self.err(
                "IMAGE_RECEIVER_SOURCE_NOT_LOCAL",
                format!(
                    "image receiver is bound to loopback; source machine '{}' must match local machine '{}'",
                    request.source_machine, self.identity.machine_id
                ),
            );
        }
        let session = self
            .image_registry
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
            session.source_machine.0.clone(),
        );
        headers.insert(REGISTRY_SESSION_HEADER.to_string(), session.token.clone());
        let payload = ImageReceiveSessionPayload {
            target_machine: self.identity.machine_id.clone(),
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

    pub(crate) async fn handle_image_received_import(
        &self,
        request: &ImageReceivedImportRequest,
    ) -> ployz_api::DaemonResponse {
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_RECEIVED_IMPORT_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_received_import_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_received_import_with_backend(
        &self,
        request: &ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active(
            "IMAGE_RECEIVED_IMPORT_INACTIVE",
            "image received import requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
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
            &self.image_registry,
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
            Ok(file) => Box::pin(file) as ImageArchiveReader<'static>,
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
            machine_id: self.identity.machine_id.clone(),
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
        if let Err(error) = active.mesh.store.upsert_image_availability(&record).await {
            return self.err(
                "IMAGE_RECEIVED_IMPORT_AVAILABILITY_FAILED",
                format!("record imported image availability: {error}"),
            );
        }
        self.ok_with_payload(
            format!(
                "image {} imported on {}",
                request.expected_digest.as_str(),
                self.identity.machine_id
            ),
            Some(DaemonPayload::ImageReceivedImport(
                ImageReceivedImportPayload {
                    target_machine: self.identity.machine_id.clone(),
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
    ) -> ployz_api::DaemonResponse {
        if let Err(error) = operation_store.update_target(
            operation,
            ImageOperationTargetOutcome {
                machine_id: target_machine,
                status: OperationStatus::Failed,
                bytes_transferred: None,
                last_error: Some(message.clone()),
            },
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
        stage: ImageTransferFailureStage,
        code: &'static str,
        message: String,
    ) -> ployz_api::DaemonResponse {
        let targets = request
            .target_machines
            .iter()
            .map(|target_machine| {
                let (result, status, last_error) =
                    if let Some(record) = skipped_present.get(target_machine) {
                        (
                            ImageTransferTargetResult {
                                machine_id: target_machine.clone(),
                                status: ImageTransferTargetStatus::SkippedPresent,
                                record: Some(record.clone()),
                                failure: None,
                                error: None,
                            },
                            OperationStatus::Succeeded,
                            None,
                        )
                    } else if let Some(record) = local_present.get(target_machine) {
                        (
                            ImageTransferTargetResult {
                                machine_id: target_machine.clone(),
                                status: ImageTransferTargetStatus::Present,
                                record: Some(record.clone()),
                                failure: None,
                                error: None,
                            },
                            OperationStatus::Succeeded,
                            None,
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
                let outcome = ImageOperationTargetOutcome {
                    machine_id: target_machine.clone(),
                    status,
                    bytes_transferred: None,
                    last_error,
                };
                (result, outcome)
            })
            .collect::<Vec<_>>();
        for (_, outcome) in &targets {
            if let Err(error) = operation_store.update_target(operation, outcome.clone()) {
                return self.err(
                    "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                    format!(
                        "image distribute failed: {message}; also failed to persist target failure: {error}"
                    ),
                );
            }
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
                targets: targets
                    .into_iter()
                    .map(|(target_result, _)| target_result)
                    .collect(),
            })),
        )
    }

    fn validate_image_distribute_request(
        &self,
        request: &ImageDistributeRequest,
    ) -> Result<(), ployz_api::DaemonResponse> {
        if request.target_machines.is_empty() {
            return Err(self.err_with_payload(
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
            return Err(self.err_with_payload(
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
        if request.source_machine != self.identity.machine_id {
            return Err(self.err_with_payload(
                "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL",
                format!(
                    "image distribute source '{}' must match local machine '{}'",
                    request.source_machine, self.identity.machine_id
                ),
                Some(DaemonPayload::ImageDistributeValidation(
                    image_distribute_validation_payload(
                        request,
                        ImageDistributeValidationFailure::SourceNotLocal {
                            source_machine: request.source_machine.clone(),
                            local_machine: self.identity.machine_id.clone(),
                        },
                    ),
                )),
            ));
        }
        Ok(())
    }

    fn fail_image_push_operation(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: MachineId,
        message: String,
    ) -> ployz_api::DaemonResponse {
        if let Err(error) = operation_store.update_target(
            operation,
            ImageOperationTargetOutcome {
                machine_id: target_machine,
                status: OperationStatus::Failed,
                bytes_transferred: None,
                last_error: Some(message.clone()),
            },
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
    ) -> Result<(), ployz_api::DaemonResponse> {
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
    ) -> Result<(), ployz_api::DaemonResponse> {
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
    ) -> Result<ImageReceiveSessionPayload, String> {
        let request = ImageReceiveSessionRequest {
            operation_id: operation_id.into(),
            source_machine: self.identity.machine_id.clone(),
            repository: Some(repository),
        };
        let response = if target_machine == &self.identity.machine_id {
            self.handle_image_receive_session(&request).await
        } else {
            let client = self
                .nats_node_rpc_client()
                .await
                .map_err(|error| format!("connect node rpc for image receive session: {error}"))?;
            client
                .request(
                    NodeCommandSubject::image_receive_session(target_machine),
                    &DaemonRequest::ImageReceiveSession { request },
                )
                .await
                .map_err(|error| {
                    format!("request image receive session from {target_machine}: {error}")
                })?
        };
        if !response.ok {
            return Err(format!(
                "target receive session failed [{}]: {}",
                response.code, response.message
            ));
        }
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
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
        archive: &crate::daemon::handlers::image::archive::ParsedImageArchive,
        backend: &dyn RuntimeImageBackend,
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
                    response.code,
                    response.message,
                ),
                None,
            );
        }
        let repository = default_receive_repository(&operation.id);
        let session = match self
            .image_receive_session_for_target(target_machine, &operation.id, repository.clone())
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
                    response.code,
                    response.message,
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
                    response.code,
                    response.message,
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
            ImageTransferTargetResult {
                machine_id: target_machine.clone(),
                status: ImageTransferTargetStatus::Present,
                record: Some(record),
                failure: None,
                error: None,
            },
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
            machine_id: self.identity.machine_id.clone(),
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
        digest: &ployz_types::model::ImageDigest,
        platform: Option<ployz_types::model::ImagePlatform>,
        repo_tags: Vec<String>,
        archive: &crate::daemon::handlers::image::archive::ParsedImageArchive,
        backend: &dyn RuntimeImageBackend,
    ) -> Result<
        (
            ImageAvailabilityRecord,
            crate::daemon::handlers::image::archive::ReceiverUploadReport,
        ),
        ployz_api::DaemonResponse,
    > {
        self.update_image_push_stage(
            operation_store,
            operation,
            "opening receive session",
            target_machine,
        )?;
        let session = self
            .image_receive_session_for_target(target_machine, &operation.id, repository.to_string())
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
                    source_machine: self.identity.machine_id.clone(),
                    repository: repository.to_string(),
                    reference: reference.to_string(),
                    expected_digest: digest.clone(),
                    platform,
                    repo_tags,
                },
                backend,
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
        digest: &ployz_types::model::ImageDigest,
        platform: Option<ployz_types::model::ImagePlatform>,
        backend: &dyn RuntimeImageBackend,
    ) -> Result<ImageTransferTargetResult, ImageTransferTargetResult> {
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
        let response = if source_machine == &self.identity.machine_id {
            self.handle_image_distribute_with_backend(&request, backend)
                .await
        } else {
            let client = self.nats_node_rpc_client().await.map_err(|error| {
                failure(format!("connect node rpc for image distribute: {error}"))
            })?;
            client
                .with_policy(RpcPolicy {
                    timeout: IMAGE_DISTRIBUTE_RPC_TIMEOUT,
                })
                .request(
                    NodeCommandSubject::image_distribute(source_machine),
                    &DaemonRequest::ImageDistribute { request },
                )
                .await
                .map_err(|error| {
                    failure(format!(
                        "request image distribute from {source_machine}: {error}"
                    ))
                })?
        };
        if !response.ok {
            if let Some(DaemonPayload::ImageDistribute(payload)) = response.payload {
                let [target] = payload.targets.as_slice() else {
                    return Err(failure(format!(
                        "target image distribute failed [{}] with {} target results: {}",
                        response.code,
                        payload.targets.len(),
                        response.message
                    )));
                };
                return Err(target.clone());
            }
            return Err(failure(format!(
                "target image distribute failed [{}]: {}",
                response.code, response.message
            )));
        }
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            return Err(failure(
                "target image distribute response did not include a payload".into(),
            ));
        };
        let [target] = payload.targets.as_slice() else {
            return Err(failure(format!(
                "target image distribute returned {} target results",
                payload.targets.len()
            )));
        };
        Ok(target.clone())
    }

    async fn import_received_image_for_target(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> Result<ImageAvailabilityRecord, String> {
        let response = if target_machine == &self.identity.machine_id {
            self.handle_image_received_import_with_backend(&request, backend)
                .await
        } else {
            let client = self
                .nats_node_rpc_client()
                .await
                .map_err(|error| format!("connect node rpc for image import: {error}"))?;
            client
                .with_policy(RpcPolicy {
                    timeout: IMAGE_RECEIVED_IMPORT_RPC_TIMEOUT,
                })
                .request(
                    NodeCommandSubject::image_received_import(target_machine),
                    &DaemonRequest::ImageReceivedImport { request },
                )
                .await
                .map_err(|error| format!("request image import from {target_machine}: {error}"))?
        };
        if !response.ok {
            return Err(format!(
                "target image import failed [{}]: {}",
                response.code, response.message
            ));
        }
        let Some(DaemonPayload::ImageReceivedImport(payload)) = response.payload else {
            return Err("target image import response did not include an import payload".into());
        };
        Ok(payload.record)
    }
}

async fn resolve_push_source_image(
    backend: &dyn RuntimeImageBackend,
    source_image: &str,
    expected_digest: &Option<ployz_types::model::ImageDigest>,
) -> Result<(ployz_types::model::ImageDigest, RuntimeImage), RuntimeImageError> {
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
) -> Result<ployz_types::model::ImageDigest, RuntimeImageError> {
    let Some(id) = image.id.as_deref() else {
        return Err(RuntimeImageError::MissingDigest {
            reference: source_image.into(),
        });
    };
    ployz_types::model::ImageDigest::try_new(id).map_err(|_| RuntimeImageError::MissingDigest {
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
    ImageTransferTargetResult {
        machine_id,
        status: ImageTransferTargetStatus::Failed,
        record: None,
        failure: Some(ImageTransferFailure {
            code,
            stage,
            message: message.clone(),
        }),
        error: Some(message),
    }
}

fn image_distribute_validation_payload(
    request: &ImageDistributeRequest,
    failure: ImageDistributeValidationFailure,
) -> ImageDistributeValidationPayload {
    ImageDistributeValidationPayload {
        digest: request.digest.clone(),
        source_machine: request.source_machine.clone(),
        target_machines: request.target_machines.clone(),
        failure,
    }
}

fn image_ref_from_tag(reference: &str, digest: ployz_types::model::ImageDigest) -> ImageRef {
    let (repository, tag) = if reference.starts_with("sha256:") {
        (None, None)
    } else {
        match reference.rsplit_once(':') {
            Some((repository, tag)) if !repository.is_empty() && !tag.is_empty() => {
                (Some(repository.to_string()), Some(tag.to_string()))
            }
            _ => (None, None),
        }
    };
    ImageRef {
        repository,
        tag,
        digest,
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
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use ployz_api::DaemonPayload;
    use ployz_orchestrator::{Mesh, WireguardDriver};
    use ployz_runtime_api::{Identity, RuntimeImage, RuntimeImageError, RuntimeImageImportResult};
    use ployz_store_api::StoreDriver;
    use ployz_types::model::{
        ImageDigest, ImagePresence, MachineId, MachineMembership, NetworkLifecycle, NetworkName,
        OverlayIp, PublicKey,
    };
    use sha2::{Digest as _, Sha256};
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncReadExt as _;
    use tower::ServiceExt as _;

    use crate::daemon::{ActiveMesh, RetainedSubnet};
    use crate::mesh_state::network::NetworkConfig;

    #[tokio::test]
    async fn image_push_rejects_zero_targets_before_operation_side_effects() {
        let state = make_state();
        let response = state
            .handle_image_push(&ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: Vec::new(),
                platform: None,
                expected_digest: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_TARGET_REQUIRED");
        assert!(
            state
                .image_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn image_push_self_target_uploads_imports_and_records_availability() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("bind addr"),
            state.image_registry.clone(),
        )
        .await
        .expect("serve registry");
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = Some(listener.bind_addr());
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImagePush(payload)) = response.payload else {
            panic!("expected push payload");
        };
        assert_eq!(payload.artifact.digest(), &digest);
        assert_eq!(payload.targets.len(), 1);
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::Present
        );
        assert!(payload.targets[0].record.is_some());
        assert!(*backend.imported.lock().expect("imported lock"));
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].kind, ImageOperationKind::Push);
        assert_eq!(operations[0].status, OperationStatus::Succeeded);
        assert!(
            !state
                .data_dir
                .join("image-push")
                .join(&payload.operation_id)
                .exists()
        );
        listener.shutdown().await;
    }

    #[tokio::test]
    async fn image_push_source_verify_failure_marks_operation_failed() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let actual = digest('a');
        let expected = digest('b');
        let backend = FakeImageBackend::new(actual, docker_archive_bytes());

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(expected.clone()),
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_FAILED");
        assert_no_availability(&state, &expected).await;
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].kind, ImageOperationKind::Push);
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(operations[0].targets[0].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_push_import_failure_does_not_record_availability() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("bind addr"),
            state.image_registry.clone(),
        )
        .await
        .expect("serve registry");
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = Some(listener.bind_addr());
        let digest = digest('a');
        let backend =
            FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_import_error();

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_FAILED");
        assert_no_availability(&state, &digest).await;
        listener.shutdown().await;
    }

    #[tokio::test]
    async fn image_push_without_expected_digest_uses_image_id_when_repo_digest_is_absent() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("bind addr"),
            state.image_registry.clone(),
        )
        .await
        .expect("serve registry");
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = Some(listener.bind_addr());
        let digest = digest('a');
        let backend =
            FakeImageBackend::new(digest.clone(), docker_archive_bytes()).without_repo_digests();

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: None,
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImagePush(payload)) = response.payload else {
            panic!("expected push payload");
        };
        assert_eq!(payload.artifact.digest(), &digest);
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].digest.as_ref(), Some(&digest));
        listener.shutdown().await;
    }

    #[tokio::test]
    async fn image_push_expected_repo_digest_uses_image_id_for_import_identity() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("bind addr"),
            state.image_registry.clone(),
        )
        .await
        .expect("serve registry");
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = Some(listener.bind_addr());
        let repo_digest = digest('a');
        let image_id = digest('b');
        let backend = FakeImageBackend::new(repo_digest.clone(), docker_archive_bytes())
            .with_image_id(image_id.clone());

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(repo_digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImagePush(payload)) = response.payload else {
            panic!("expected push payload");
        };
        assert_eq!(payload.artifact.digest(), &image_id);
        assert_no_availability(&state, &repo_digest).await;
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &image_id)
            .await
            .expect("get availability")
            .expect("image id availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        listener.shutdown().await;
    }

    #[tokio::test]
    async fn image_push_expected_repo_digest_without_image_id_fails_before_transfer() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend =
            FakeImageBackend::new(digest.clone(), docker_archive_bytes()).without_image_id();

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_FAILED");
        assert!(!*backend.imported.lock().expect("imported lock"));
        assert_no_availability(&state, &digest).await;
    }

    #[tokio::test]
    async fn image_push_without_expected_digest_fails_when_runtime_has_no_digest_identity() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest, docker_archive_bytes())
            .without_repo_digests()
            .without_image_id();

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_FAILED");
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_push_receive_session_failure_marks_operation_failed() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = None;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                    expected_digest: Some(digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_FAILED");
        assert_no_availability(&state, &digest).await;
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(operations[0].targets[0].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_push_later_target_failure_preserves_first_target_success() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let listener = crate::daemon::handlers::image::registry::serve(
            "127.0.0.1:0".parse().expect("bind addr"),
            state.image_registry.clone(),
        )
        .await
        .expect("serve registry");
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = Some(listener.bind_addr());
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_push_with_backend(
                &ImagePushRequest {
                    source_image: "example/app:latest".into(),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                    expected_digest: Some(digest.clone()),
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_PARTIAL_FAILED");
        let Some(DaemonPayload::ImagePush(payload)) = response.payload else {
            panic!("expected push payload");
        };
        assert_eq!(payload.targets.len(), 2);
        assert_eq!(payload.targets[0].machine_id, MachineId("founder".into()));
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::Present
        );
        assert_eq!(payload.targets[1].machine_id, MachineId("machine-a".into()));
        assert_eq!(payload.targets[1].status, ImageTransferTargetStatus::Failed);
        let failure = payload.targets[1].failure.as_ref().expect("target failure");
        assert_eq!(failure.code, "IMAGE_DISTRIBUTE_RECEIVE_SESSION_FAILED");
        assert_eq!(failure.stage, ImageTransferFailureStage::ReceiveSession);
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("first target availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        let push = operations
            .iter()
            .find(|operation| operation.kind == ImageOperationKind::Push)
            .expect("push operation");
        assert_eq!(push.status, OperationStatus::Failed);
        assert!(
            push.targets
                .iter()
                .any(|target| target.machine_id == MachineId("founder".into())
                    && target.status == OperationStatus::Succeeded)
        );
        assert!(
            push.targets
                .iter()
                .any(|target| target.machine_id == MachineId("machine-a".into())
                    && target.status == OperationStatus::Failed)
        );
        listener.shutdown().await;
    }

    #[tokio::test]
    async fn image_distribute_rejects_non_local_source_before_operation_side_effects() {
        let state = make_state();
        let response = state
            .handle_image_distribute(&ImageDistributeRequest {
                digest: ImageDigest::try_new(format!("sha256:{}", "a".repeat(64)))
                    .expect("valid digest"),
                source_machine: MachineId("machine-a".into()),
                target_machines: vec![MachineId("machine-b".into())],
                platform: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL");
        let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload else {
            panic!("expected validation payload");
        };
        assert_eq!(
            payload.failure,
            ImageDistributeValidationFailure::SourceNotLocal {
                source_machine: MachineId("machine-a".into()),
                local_machine: MachineId("founder".into()),
            }
        );
        assert!(
            state
                .image_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn image_distribute_validates_before_runtime_backend_lookup() {
        let state = make_state();
        let response = state
            .handle_image_distribute(&ImageDistributeRequest {
                digest: digest('a'),
                source_machine: MachineId("founder".into()),
                target_machines: Vec::new(),
                platform: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_TARGET_REQUIRED");
        assert!(
            state
                .image_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn image_distribute_source_target_verifies_and_records_local_availability() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.digest, digest);
        assert_eq!(payload.targets.len(), 1);
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::Present
        );
        assert!(payload.targets[0].record.is_some());
        assert!(!*backend.imported.lock().expect("imported lock"));
        assert_eq!(backend.export_count(), 0);
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &payload.digest)
            .await
            .expect("get availability")
            .expect("availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Succeeded);
        assert!(
            !state
                .data_dir
                .join("image-transfer")
                .join(&payload.operation_id)
                .exists()
        );
        assert!(
            !state
                .data_dir
                .join("image-import")
                .join(&payload.operation_id)
                .exists()
        );
    }

    #[tokio::test]
    async fn image_distribute_rejects_zero_and_duplicate_targets_before_operation_side_effects() {
        let state = make_state();
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: Vec::new(),
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_TARGET_REQUIRED");
        let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload else {
            panic!("expected validation payload");
        };
        assert_eq!(
            payload.failure,
            ImageDistributeValidationFailure::TargetRequired { target_count: 0 }
        );

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest,
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("founder".into()), MachineId("founder".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_DUPLICATE_TARGET");
        let Some(DaemonPayload::ImageDistributeValidation(payload)) = response.payload else {
            panic!("expected validation payload");
        };
        assert_eq!(
            payload.failure,
            ImageDistributeValidationFailure::DuplicateTarget {
                duplicate_target: MachineId("founder".into())
            }
        );
        assert!(
            state
                .image_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn image_distribute_skips_present_targets_without_exporting() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());
        let store = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .clone();
        store
            .upsert_image_availability(&present_availability_record("founder", digest.clone()))
            .await
            .expect("seed founder availability");
        store
            .upsert_image_availability(&present_availability_record("machine-a", digest.clone()))
            .await
            .expect("seed machine-a availability");

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 2);
        assert!(
            payload
                .targets
                .iter()
                .all(|target| target.status == ImageTransferTargetStatus::SkippedPresent)
        );
        assert_eq!(backend.export_count(), 0);
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].status, OperationStatus::Succeeded);
        assert!(
            operations[0]
                .targets
                .iter()
                .all(|target| target.status == OperationStatus::Succeeded)
        );
    }

    #[tokio::test]
    async fn image_distribute_partial_failure_preserves_success_and_attempts_later_targets() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 2);
        assert_eq!(payload.targets[0].machine_id, MachineId("founder".into()));
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::Present
        );
        assert_eq!(payload.targets[1].machine_id, MachineId("machine-a".into()));
        assert_eq!(payload.targets[1].status, ImageTransferTargetStatus::Failed);
        assert_eq!(
            payload.targets[1]
                .failure
                .as_ref()
                .expect("target failure")
                .stage,
            ImageTransferFailureStage::ReceiveSession
        );
        assert_eq!(backend.export_count(), 1);
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("founder availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(operations[0].targets[0].status, OperationStatus::Succeeded);
        assert_eq!(operations[0].targets[1].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_distribute_continues_after_failed_target() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("machine-a".into()),
                        MachineId("founder".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 2);
        assert_eq!(payload.targets[0].machine_id, MachineId("machine-a".into()));
        assert_eq!(payload.targets[0].status, ImageTransferTargetStatus::Failed);
        assert_eq!(payload.targets[1].machine_id, MachineId("founder".into()));
        assert_eq!(
            payload.targets[1].status,
            ImageTransferTargetStatus::Present
        );
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("founder availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
    }

    #[tokio::test]
    async fn image_distribute_attempts_multiple_transfer_targets_from_one_export() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest,
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("machine-a".into()),
                        MachineId("machine-b".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(
            payload
                .targets
                .iter()
                .map(|target| target.machine_id.clone())
                .collect::<Vec<_>>(),
            vec![MachineId("machine-a".into()), MachineId("machine-b".into())]
        );
        assert!(
            payload
                .targets
                .iter()
                .all(|target| target.status == ImageTransferTargetStatus::Failed)
        );
        assert_eq!(backend.export_count(), 1);
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].targets.len(), 2);
        assert!(
            operations[0]
                .targets
                .iter()
                .all(|target| target.status == OperationStatus::Failed)
        );
    }

    #[tokio::test]
    async fn image_distribute_export_failure_preserves_skipped_targets() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend =
            FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_export_error();
        let store = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .clone();
        store
            .upsert_image_availability(&present_availability_record("founder", digest.clone()))
            .await
            .expect("seed availability");

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::SkippedPresent
        );
        assert_eq!(payload.targets[1].status, ImageTransferTargetStatus::Failed);
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].targets[0].status, OperationStatus::Succeeded);
        assert_eq!(operations[0].targets[1].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_distribute_export_failure_preserves_local_source_target() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend =
            FakeImageBackend::new(digest.clone(), docker_archive_bytes()).with_export_error();

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_PARTIAL_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::Present
        );
        assert_eq!(payload.targets[1].status, ImageTransferTargetStatus::Failed);
        let stored = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("founder availability");
        assert!(matches!(stored.presence, ImagePresence::Present { .. }));
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].targets[0].status, OperationStatus::Succeeded);
        assert_eq!(operations[0].targets[1].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_distribute_archive_parse_failure_marks_all_targets_failed_and_cleans_work_dir() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), b"not a tar archive".to_vec());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("machine-a".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 1);
        assert_eq!(payload.targets[0].status, ImageTransferTargetStatus::Failed);
        assert!(
            !state
                .data_dir
                .join("image-transfer")
                .join(&payload.operation_id)
                .exists()
        );
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].targets[0].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_distribute_source_verify_failure_marks_all_targets_failed() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let actual = digest('a');
        let expected = digest('b');
        let backend = FakeImageBackend::new(actual, docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: expected.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![
                        MachineId("founder".into()),
                        MachineId("machine-a".into()),
                    ],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_FAILED");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 2);
        assert!(
            payload
                .targets
                .iter()
                .all(|target| target.status == ImageTransferTargetStatus::Failed)
        );
        assert_eq!(backend.export_count(), 0);
        assert_no_availability(&state, &expected).await;
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert!(
            operations[0]
                .targets
                .iter()
                .all(|target| target.status == OperationStatus::Failed)
        );
    }

    #[tokio::test]
    async fn image_distribute_receive_session_failure_marks_operation_failed() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        state
            .active
            .as_mut()
            .expect("active mesh")
            .image_receiver_bind_addr = None;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest,
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("machine-a".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_FAILED");
        let operations = state
            .image_operation_store()
            .list()
            .expect("list operations");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].status, OperationStatus::Failed);
        assert_eq!(operations[0].targets[0].status, OperationStatus::Failed);
    }

    #[tokio::test]
    async fn image_distribute_target_failure_does_not_record_availability() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("machine-a".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_FAILED");
        assert_no_availability_on(&state, "machine-a", &digest).await;
    }

    #[tokio::test]
    async fn image_distribute_source_target_skips_existing_availability_without_rewriting() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());
        let store = state
            .active
            .as_ref()
            .expect("active mesh")
            .mesh
            .store
            .clone();
        let existing = present_availability_record("founder", digest.clone());
        store
            .upsert_image_availability(&existing)
            .await
            .expect("seed availability");

        let response = state
            .handle_image_distribute_with_backend(
                &ImageDistributeRequest {
                    digest: digest.clone(),
                    source_machine: MachineId("founder".into()),
                    target_machines: vec![MachineId("founder".into())],
                    platform: None,
                },
                &backend,
            )
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImageDistribute(payload)) = response.payload else {
            panic!("expected distribute payload");
        };
        assert_eq!(payload.targets.len(), 1);
        assert_eq!(
            payload.targets[0].status,
            ImageTransferTargetStatus::SkippedPresent
        );
        assert_eq!(backend.export_count(), 0);
        let stored = store
            .get_image_availability(&MachineId("founder".into()), &digest)
            .await
            .expect("get availability")
            .expect("availability");
        assert_eq!(stored, existing);
    }

    #[tokio::test]
    async fn image_received_import_missing_manifest_does_not_record_availability() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_received_import_with_backend(
                &ImageReceivedImportRequest {
                    operation_id: "op-1".into(),
                    source_machine: MachineId("founder".into()),
                    repository: "ployz/op-1".into(),
                    reference: "op-1".into(),
                    expected_digest: digest.clone(),
                    platform: None,
                    repo_tags: vec!["example/app:latest".into()],
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVED_IMPORT_RECONSTRUCT_FAILED");
        assert!(
            state
                .active
                .as_ref()
                .expect("active mesh")
                .mesh
                .store
                .get_image_availability(&MachineId("founder".into()), &digest)
                .await
                .expect("get availability")
                .is_none()
        );
    }

    #[tokio::test]
    async fn image_received_import_rejects_unsafe_operation_id_before_filesystem_work() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let digest = digest('a');
        let backend = FakeImageBackend::new(digest.clone(), docker_archive_bytes());

        let response = state
            .handle_image_received_import_with_backend(
                &ImageReceivedImportRequest {
                    operation_id: "../outside".into(),
                    source_machine: MachineId("founder".into()),
                    repository: "ployz/op-1".into(),
                    reference: "op-1".into(),
                    expected_digest: digest,
                    platform: None,
                    repo_tags: vec!["example/app:latest".into()],
                },
                &backend,
            )
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVED_IMPORT_INVALID_OPERATION");
        assert!(!state.data_dir.join("outside").exists());
    }

    #[tokio::test]
    async fn image_receive_session_requires_active_mesh() {
        let state = make_state();
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVER_INACTIVE");
    }

    #[tokio::test]
    async fn image_receive_session_returns_endpoint_token_and_headers_for_local_source() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;

        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("founder".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(response.ok, "{response:?}");
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        assert_eq!(payload.target_machine, MachineId("founder".into()));
        assert_eq!(
            payload.endpoint,
            "http://127.0.0.1:4320/v2/ployz/image-push-1"
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_OPERATION_HEADER)
                .map(String::as_str),
            Some("image-push-1")
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_SOURCE_MACHINE_HEADER)
                .map(String::as_str),
            Some("founder")
        );
        assert_eq!(
            payload
                .headers
                .get(REGISTRY_SESSION_HEADER)
                .map(String::as_str),
            Some(payload.token.as_str())
        );
        assert!(payload.expires_at_unix_secs > 0);
    }

    #[tokio::test]
    async fn image_receive_session_rejects_unknown_source_machine() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;

        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("unknown".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVER_SOURCE_UNKNOWN");
    }

    #[tokio::test]
    async fn image_receive_session_rejects_remote_source_for_loopback_receiver() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;

        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("machine-a".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_RECEIVER_SOURCE_NOT_LOCAL");
    }

    #[tokio::test]
    async fn image_receive_session_token_authorizes_registry_upload() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("founder".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        let digest = test_sha256_digest(b"hello");
        let router = state.image_registry.clone().router();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "/v2/ployz/image-push-1/blobs/uploads/?digest={digest}"
            ))
            .header(
                REGISTRY_OPERATION_HEADER,
                payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
            )
            .header(
                REGISTRY_SOURCE_MACHINE_HEADER,
                payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
            )
            .header(
                REGISTRY_SESSION_HEADER,
                payload.headers[REGISTRY_SESSION_HEADER].as_str(),
            )
            .body(Body::from("hello"))
            .expect("request");

        let response = router.oneshot(request).await.expect("registry response");

        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn image_receive_session_token_is_scoped_to_repository() {
        let mut state = make_state();
        install_active_mesh(&mut state).await;
        let response = state
            .handle_image_receive_session(&ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId("founder".into()),
                repository: Some("ployz/image-push-1".into()),
            })
            .await;
        let Some(DaemonPayload::ImageReceiveSession(payload)) = response.payload else {
            panic!("expected image receive session payload");
        };
        let digest = test_sha256_digest(b"hello");
        let router = state.image_registry.clone().router();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/v2/other/repo/blobs/uploads/?digest={digest}"))
            .header(
                REGISTRY_OPERATION_HEADER,
                payload.headers[REGISTRY_OPERATION_HEADER].as_str(),
            )
            .header(
                REGISTRY_SOURCE_MACHINE_HEADER,
                payload.headers[REGISTRY_SOURCE_MACHINE_HEADER].as_str(),
            )
            .header(
                REGISTRY_SESSION_HEADER,
                payload.headers[REGISTRY_SESSION_HEADER].as_str(),
            )
            .body(Body::from("hello"))
            .expect("request");

        let response = router.oneshot(request).await.expect("registry response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    struct FakeImageBackend {
        digest: ImageDigest,
        archive: Vec<u8>,
        repo_digests: Vec<ImageDigest>,
        image_id: Option<String>,
        imported: Arc<Mutex<bool>>,
        export_count: Arc<Mutex<u64>>,
        export_error: bool,
        import_error: bool,
        import_verify_error: bool,
    }

    impl FakeImageBackend {
        fn new(digest: ImageDigest, archive: Vec<u8>) -> Self {
            Self {
                repo_digests: vec![digest.clone()],
                image_id: Some(digest.as_str().into()),
                digest,
                archive,
                imported: Arc::new(Mutex::new(false)),
                export_count: Arc::new(Mutex::new(0)),
                export_error: false,
                import_error: false,
                import_verify_error: false,
            }
        }

        fn with_export_error(mut self) -> Self {
            self.export_error = true;
            self
        }

        fn with_import_error(mut self) -> Self {
            self.import_error = true;
            self
        }

        fn without_repo_digests(mut self) -> Self {
            self.repo_digests.clear();
            self
        }

        fn without_image_id(mut self) -> Self {
            self.image_id = None;
            self
        }

        fn with_image_id(mut self, digest: ImageDigest) -> Self {
            self.image_id = Some(digest.as_str().into());
            self
        }

        fn export_count(&self) -> u64 {
            *self.export_count.lock().expect("export count lock")
        }
    }

    #[async_trait::async_trait]
    impl RuntimeImageBackend for FakeImageBackend {
        async fn inspect_image(
            &self,
            reference: &str,
        ) -> Result<Option<RuntimeImage>, RuntimeImageError> {
            let (repo_digests, id) =
                if self.import_verify_error && reference != self.digest.as_str() {
                    (Vec::new(), None)
                } else {
                    (self.repo_digests.clone(), self.image_id.clone())
                };
            Ok(Some(RuntimeImage {
                reference: reference.into(),
                id,
                repo_digests,
                platform: None,
                size_bytes: None,
            }))
        }

        async fn export_image_archive<'a>(
            &'a self,
            reference: &'a str,
        ) -> Result<ImageArchiveReader<'a>, RuntimeImageError> {
            let _ = reference;
            *self.export_count.lock().expect("export count lock") += 1;
            if self.export_error {
                return Err(RuntimeImageError::backend(
                    "fake image export",
                    "export failed",
                ));
            }
            Ok(Box::pin(std::io::Cursor::new(self.archive.clone())))
        }

        async fn import_image_archive(
            &self,
            mut archive: ImageArchiveReader<'static>,
        ) -> Result<RuntimeImageImportResult, RuntimeImageError> {
            if self.import_error {
                return Err(RuntimeImageError::backend(
                    "fake image import",
                    "import failed",
                ));
            }
            let mut body = Vec::new();
            archive.read_to_end(&mut body).await.map_err(|error| {
                RuntimeImageError::backend("fake image import", error.to_string())
            })?;
            if body.is_empty() {
                return Err(RuntimeImageError::backend(
                    "fake image import",
                    "archive was empty",
                ));
            }
            *self.imported.lock().expect("imported lock") = true;
            Ok(RuntimeImageImportResult {
                messages: vec!["imported".into()],
            })
        }
    }

    fn make_state() -> DaemonState {
        let data_dir =
            std::env::temp_dir().join(format!("ployz-image-push-handler-{}", uuid::Uuid::new_v4()));
        let identity = Identity::generate(MachineId("founder".into()), [31; 32]);
        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }

    async fn install_active_mesh(state: &mut DaemonState) {
        let identity = Identity::generate(MachineId("founder".into()), [31; 32]);
        let mut config = NetworkConfig::new(
            NetworkName("alpha".into()),
            &identity.public_key,
            "10.210.0.0/16",
            "10.210.0.0/24".parse().expect("valid subnet"),
        );
        config.lifecycle = NetworkLifecycle::Running;
        let store = StoreDriver::memory();
        store
            .upsert_self_machine(&MachineMembership::seed(
                MachineId("founder".into()),
                PublicKey([31; 32]),
                OverlayIp("fd00::31".parse().expect("valid overlay")),
                None,
                Vec::new(),
            ))
            .await
            .expect("insert local machine");
        store
            .upsert_self_machine(&MachineMembership::seed(
                MachineId("machine-a".into()),
                PublicKey([12; 32]),
                OverlayIp("fd00::12".parse().expect("valid overlay")),
                None,
                Vec::new(),
            ))
            .await
            .expect("insert source machine");
        let mesh = Mesh::new(
            WireguardDriver::memory(),
            store,
            None,
            state.identity.machine_id.clone(),
            51820,
        );
        state.active = Some(ActiveMesh {
            retained_subnet: RetainedSubnet::from_running_config(config.subnet),
            config,
            mesh,
            nats_control: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            zfs_transfer: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            image_receiver_bind_addr: Some("127.0.0.1:4320".parse().expect("valid address")),
            gateway: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            dns: Box::new(ployz_runtime_api::NoopRuntimeHandle),
            certificate_renewal: None,
            bootstrap_peer_seed: None,
        });
    }

    fn test_sha256_digest(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(body);
        format!("sha256:{:x}", hasher.finalize())
    }

    fn digest(hex: char) -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", hex.to_string().repeat(64)))
            .expect("valid digest")
    }

    async fn assert_no_availability(state: &DaemonState, digest: &ImageDigest) {
        assert_no_availability_on(state, "founder", digest).await;
    }

    async fn assert_no_availability_on(
        state: &DaemonState,
        machine_id: &str,
        digest: &ImageDigest,
    ) {
        assert!(
            state
                .active
                .as_ref()
                .expect("active mesh")
                .mesh
                .store
                .get_image_availability(&MachineId(machine_id.into()), digest)
                .await
                .expect("get availability")
                .is_none()
        );
    }

    fn present_availability_record(
        machine_id: &str,
        digest: ImageDigest,
    ) -> ImageAvailabilityRecord {
        let now = now_unix_secs();
        let reference = digest.as_str().to_string();
        ImageAvailabilityRecord {
            machine_id: MachineId(machine_id.into()),
            digest: digest.clone(),
            presence: ImagePresence::Present {
                artifact: ImageArtifact {
                    image: image_ref_from_tag(&reference, digest),
                    platform: None,
                    provenance: ImageArtifactProvenance::External {
                        source: Some("test".into()),
                    },
                    created_at: now,
                },
                recorded_at: now,
                source_operation_id: Some("seed".into()),
            },
            updated_at: now,
        }
    }

    fn docker_archive_bytes() -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut output);
            append_tar_member(&mut builder, "config.json", br#"{"architecture":"amd64"}"#);
            append_tar_member(&mut builder, "layer-one/layer.tar", b"layer-one");
            append_tar_member(&mut builder, "layer-two/layer.tar", b"layer-two");
            append_tar_member(
                &mut builder,
                "manifest.json",
                br#"[{"Config":"config.json","RepoTags":["example/app:latest"],"Layers":["layer-one/layer.tar","layer-two/layer.tar"]}]"#,
            );
            builder.finish().expect("finish tar");
        }
        output
    }

    fn append_tar_member(builder: &mut tar::Builder<&mut Vec<u8>>, name: &str, body: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, body)
            .expect("append tar member");
    }
}
