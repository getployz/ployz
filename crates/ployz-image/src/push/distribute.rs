use std::collections::BTreeMap;

use crate::archive::parse_image_archive;
use crate::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::{
    ImageDistributePayload, ImageDistributeRequest, ImageOperationKind,
    ImageOperationTargetOutcome, ImagePresence, ImageTransferFailureStage,
    ImageTransferTargetResult, ImageTransferTargetStatus, OperationStatus,
};
use ployz_runtime_api::RuntimeImageBackend;
use ployz_store_api::ImageAvailabilityStore;

use super::transfer::{failed_image_transfer_target, image_transfer_target_failure_message};
use super::{
    ImagePeerClient, ImageService, cleanup_image_work_dir, validate_image_distribute_request,
};

impl ImageService {
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
