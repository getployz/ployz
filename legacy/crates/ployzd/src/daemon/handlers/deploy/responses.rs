use ployz_api::DeployFailurePayload;
use ployz_types::Error as PloyzError;
use ployz_types::error::DeployError;
use ployz_types::model::{DeployId, PreparedDeployState};

pub(super) fn deploy_error_code<'a>(default_code: &'a str, error: &PloyzError) -> &'a str {
    match error {
        PloyzError::Deploy(DeployError::DeployBaselineChanged { .. }) => "DEPLOY_BASELINE_CHANGED",
        PloyzError::Deploy(DeployError::DeployImageDigestRequired { .. }) => {
            "DEPLOY_IMAGE_DIGEST_REQUIRED"
        }
        PloyzError::Deploy(DeployError::DeployImageAvailabilityMissing { .. }) => {
            "DEPLOY_IMAGE_AVAILABILITY_MISSING"
        }
        PloyzError::Deploy(DeployError::DeployImageAvailabilityNotPresent { .. }) => {
            "DEPLOY_IMAGE_AVAILABILITY_NOT_PRESENT"
        }
        PloyzError::Deploy(DeployError::BranchSourceRevisionChanged { .. }) => {
            "BRANCH_SOURCE_REVISION_CHANGED"
        }
        PloyzError::Deploy(DeployError::VolumeCloneSourceChanged { .. }) => {
            "VOLUME_CLONE_SOURCE_CHANGED"
        }
        PloyzError::Deploy(DeployError::DeployOptionInvalid { .. }) => "INVALID_DEPLOY_OPTIONS",
        PloyzError::Deploy(DeployError::PreparedDeployMissing { .. }) => "PREPARED_DEPLOY_MISSING",
        PloyzError::Deploy(DeployError::PreparedDeployNotApplicable { .. }) => {
            "PREPARED_DEPLOY_NOT_APPLICABLE"
        }
        PloyzError::Deploy(DeployError::PreparedDeployExpired { .. }) => "PREPARED_DEPLOY_EXPIRED",
        PloyzError::Deploy(DeployError::PreparedDeployInvalid { .. }) => "PREPARED_DEPLOY_INVALID",
        _ => default_code,
    }
}
pub(super) fn deploy_failure_payload_for_error(error: &PloyzError) -> Option<DeployFailurePayload> {
    match error {
        PloyzError::Deploy(DeployError::NoEligiblePlacementTargets) => {
            Some(DeployFailurePayload::NoEligiblePlacementTargets)
        }
        PloyzError::Deploy(DeployError::DeployBaselineChanged { diff }) => {
            Some(DeployFailurePayload::DeployBaselineChanged {
                expected_baseline: diff.expected.clone(),
                actual_baseline: diff.actual.clone(),
                baseline_changed_components: diff.changed_components(),
            })
        }
        PloyzError::Deploy(DeployError::PreparedDeployMissing { prepared_deploy_id }) => {
            Some(DeployFailurePayload::PreparedDeployMissing {
                prepared_deploy_id: DeployId::new(prepared_deploy_id.clone()),
            })
        }
        PloyzError::Deploy(DeployError::PreparedDeployNotApplicable {
            prepared_deploy_id,
            state,
        }) => Some(DeployFailurePayload::PreparedDeployNotApplicable {
            prepared_deploy_id: DeployId::new(prepared_deploy_id.clone()),
            prepared_deploy_state: *state,
        }),
        PloyzError::Deploy(DeployError::PreparedDeployExpired {
            prepared_deploy_id,
            expires_at,
        }) => Some(DeployFailurePayload::PreparedDeployExpired {
            prepared_deploy_id: DeployId::new(prepared_deploy_id.clone()),
            prepared_deploy_state: PreparedDeployState::Expired,
            prepared_deploy_expires_at: *expires_at,
        }),
        PloyzError::Deploy(DeployError::PreparedDeployInvalid {
            prepared_deploy_id, ..
        }) => Some(DeployFailurePayload::PreparedDeployInvalid {
            prepared_deploy_id: DeployId::new(prepared_deploy_id.clone()),
        }),
        PloyzError::Deploy(DeployError::DeployImageDigestRequired { service, image }) => {
            Some(DeployFailurePayload::DeployImageDigestRequired {
                service: service.clone(),
                image: image.clone(),
            })
        }
        PloyzError::Deploy(DeployError::DeployImageAvailabilityMissing {
            service,
            slot_id,
            machine_id,
            image,
            digest,
        }) => Some(DeployFailurePayload::DeployImageAvailabilityMissing {
            service: service.clone(),
            slot_id: slot_id.clone(),
            machine_id: machine_id.clone(),
            image: image.clone(),
            digest: digest.clone(),
        }),
        PloyzError::Deploy(DeployError::DeployImageAvailabilityNotPresent {
            service,
            slot_id,
            machine_id,
            image,
            digest,
            state,
        }) => Some(DeployFailurePayload::DeployImageAvailabilityNotPresent {
            service: service.clone(),
            slot_id: slot_id.clone(),
            machine_id: machine_id.clone(),
            image: image.clone(),
            digest: digest.clone(),
            state: state.clone(),
        }),
        _ => None,
    }
}
