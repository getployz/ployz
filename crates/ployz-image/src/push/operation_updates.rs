use std::collections::BTreeMap;

use crate::operations::ImageOperationStore;
use crate::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::{
    ImageAvailabilityRecord, ImageDistributePayload, ImageDistributeRequest, ImageOperationRecord,
    ImageOperationTargetOutcome, ImageTransferFailureStage, ImageTransferTargetResult, MachineId,
    OperationStatus,
};

use super::ImageService;
use super::transfer::failed_image_transfer_target;

impl ImageService {
    pub(super) fn fail_image_distribute_operation(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: MachineId,
        message: String,
    ) -> ImageServiceResponse {
        if let Err(error) = operation_store.update_target(
            operation,
            ImageOperationTargetOutcome::failed(target_machine, message.clone()),
        ) {
            return self.err(
                "IMAGE_DISTRIBUTE_OPERATION_FAILED",
                format!(
                    "image distribute failed: {message}; also failed to persist target failure: {error}"
                ),
            );
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
        self.err("IMAGE_DISTRIBUTE_FAILED", message)
    }

    pub(super) fn fail_all_image_distribute_targets(
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
    ) -> ImageServiceResponse {
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
            Some(ImageServicePayload::ImageDistribute(
                ImageDistributePayload {
                    operation_id: operation.id.clone(),
                    digest: request.digest.clone(),
                    source_machine: request.source_machine.clone(),
                    targets,
                },
            )),
        )
    }

    pub(super) fn fail_image_push_operation(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        target_machine: MachineId,
        message: String,
    ) -> ImageServiceResponse {
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

    pub(super) fn update_image_push_stage(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        stage: &str,
        target_machine: &MachineId,
    ) -> Result<(), ImageServiceResponse> {
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

    pub(super) fn update_image_distribute_stage(
        &self,
        operation_store: &ImageOperationStore,
        operation: &mut ImageOperationRecord,
        stage: &str,
        target_machine: &MachineId,
    ) -> Result<(), ImageServiceResponse> {
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
}
