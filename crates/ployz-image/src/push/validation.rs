use std::collections::BTreeSet;

use crate::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::{
    ImageDistributeRequest, ImageDistributeValidationFailure, ImageDistributeValidationPayload,
    MachineId,
};

pub fn validate_image_distribute_request(
    local_machine: &MachineId,
    request: &ImageDistributeRequest,
) -> Result<(), ImageServiceResponse> {
    if request.target_machines.is_empty() {
        return Err(ImageServiceResponse::error(
            "IMAGE_DISTRIBUTE_TARGET_REQUIRED",
            "image distribute requires at least one target machine",
            Some(ImageServicePayload::ImageDistributeValidation(
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
        return Err(ImageServiceResponse::error(
            "IMAGE_DISTRIBUTE_DUPLICATE_TARGET",
            format!("image distribute target '{duplicate}' was provided more than once"),
            Some(ImageServicePayload::ImageDistributeValidation(
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
        return Err(ImageServiceResponse::error(
            "IMAGE_DISTRIBUTE_SOURCE_NOT_LOCAL",
            format!(
                "image distribute source '{}' must match local machine '{}'",
                request.source_machine, local_machine
            ),
            Some(ImageServicePayload::ImageDistributeValidation(
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

fn first_duplicate_machine(machines: &[MachineId]) -> Option<MachineId> {
    let mut seen = BTreeSet::new();
    machines
        .iter()
        .find(|machine| !seen.insert((*machine).clone()))
        .cloned()
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

#[cfg(test)]
mod tests {
    use super::*;
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

    fn validation_failure(response: &ImageServiceResponse) -> ImageDistributeValidationFailure {
        let Some(ImageServicePayload::ImageDistributeValidation(payload)) = response.payload()
        else {
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
}
