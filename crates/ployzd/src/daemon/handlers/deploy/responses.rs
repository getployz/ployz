use ployz_api::{DeployFailurePayload, DeployFailureReason};
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
        PloyzError::Deploy(DeployError::NoEligiblePlacementTargets) => Some(DeployFailurePayload {
            reason: DeployFailureReason::NoEligiblePlacementTargets,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: None,
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: None,
            slot_id: None,
            machine_id: None,
            image: None,
            digest: None,
            state: None,
        }),
        PloyzError::Deploy(DeployError::DeployBaselineChanged { diff }) => {
            Some(DeployFailurePayload {
                reason: DeployFailureReason::DeployBaselineChanged,
                expected_baseline: Some(diff.expected.clone()),
                actual_baseline: Some(diff.actual.clone()),
                baseline_changed_components: diff.changed_components(),
                prepared_deploy_id: None,
                prepared_deploy_state: None,
                prepared_deploy_expires_at: None,
                service: None,
                slot_id: None,
                machine_id: None,
                image: None,
                digest: None,
                state: None,
            })
        }
        PloyzError::Deploy(DeployError::PreparedDeployMissing { prepared_deploy_id }) => {
            Some(DeployFailurePayload {
                reason: DeployFailureReason::PreparedDeployMissing,
                expected_baseline: None,
                actual_baseline: None,
                baseline_changed_components: Vec::new(),
                prepared_deploy_id: Some(DeployId::new(prepared_deploy_id.clone())),
                prepared_deploy_state: None,
                prepared_deploy_expires_at: None,
                service: None,
                slot_id: None,
                machine_id: None,
                image: None,
                digest: None,
                state: None,
            })
        }
        PloyzError::Deploy(DeployError::PreparedDeployNotApplicable {
            prepared_deploy_id,
            state,
        }) => Some(DeployFailurePayload {
            reason: DeployFailureReason::PreparedDeployNotApplicable,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: Some(DeployId::new(prepared_deploy_id.clone())),
            prepared_deploy_state: Some(*state),
            prepared_deploy_expires_at: None,
            service: None,
            slot_id: None,
            machine_id: None,
            image: None,
            digest: None,
            state: None,
        }),
        PloyzError::Deploy(DeployError::PreparedDeployExpired {
            prepared_deploy_id,
            expires_at,
        }) => Some(DeployFailurePayload {
            reason: DeployFailureReason::PreparedDeployExpired,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: Some(DeployId::new(prepared_deploy_id.clone())),
            prepared_deploy_state: Some(PreparedDeployState::Expired),
            prepared_deploy_expires_at: Some(*expires_at),
            service: None,
            slot_id: None,
            machine_id: None,
            image: None,
            digest: None,
            state: None,
        }),
        PloyzError::Deploy(DeployError::PreparedDeployInvalid {
            prepared_deploy_id, ..
        }) => Some(DeployFailurePayload {
            reason: DeployFailureReason::PreparedDeployInvalid,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: Some(DeployId::new(prepared_deploy_id.clone())),
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: None,
            slot_id: None,
            machine_id: None,
            image: None,
            digest: None,
            state: None,
        }),
        PloyzError::Deploy(DeployError::DeployImageDigestRequired { service, image }) => {
            Some(DeployFailurePayload {
                reason: DeployFailureReason::DeployImageDigestRequired,
                expected_baseline: None,
                actual_baseline: None,
                baseline_changed_components: Vec::new(),
                prepared_deploy_id: None,
                prepared_deploy_state: None,
                prepared_deploy_expires_at: None,
                service: Some(service.clone()),
                slot_id: None,
                machine_id: None,
                image: Some(image.clone()),
                digest: None,
                state: None,
            })
        }
        PloyzError::Deploy(DeployError::DeployImageAvailabilityMissing {
            service,
            slot_id,
            machine_id,
            image,
            digest,
        }) => Some(DeployFailurePayload {
            reason: DeployFailureReason::DeployImageAvailabilityMissing,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: None,
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: Some(service.clone()),
            slot_id: Some(slot_id.clone()),
            machine_id: Some(machine_id.clone()),
            image: Some(image.clone()),
            digest: Some(digest.clone()),
            state: None,
        }),
        PloyzError::Deploy(DeployError::DeployImageAvailabilityNotPresent {
            service,
            slot_id,
            machine_id,
            image,
            digest,
            state,
        }) => Some(DeployFailurePayload {
            reason: DeployFailureReason::DeployImageAvailabilityNotPresent,
            expected_baseline: None,
            actual_baseline: None,
            baseline_changed_components: Vec::new(),
            prepared_deploy_id: None,
            prepared_deploy_state: None,
            prepared_deploy_expires_at: None,
            service: Some(service.clone()),
            slot_id: Some(slot_id.clone()),
            machine_id: Some(machine_id.clone()),
            image: Some(image.clone()),
            digest: Some(digest.clone()),
            state: Some(state.clone()),
        }),
        _ => None,
    }
}
