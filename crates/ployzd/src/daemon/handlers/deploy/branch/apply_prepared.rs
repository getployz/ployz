use crate::daemon::DaemonState;
use ployz_api::{
    BranchApplyPreparedRequest, BranchEnvironmentPayload, DaemonPayload, DaemonResponse,
    DeployApplyPreparedRequest,
};
use ployz_error::{DeployError, Error as PloyzError};
use ployz_model::{BranchEnvironmentRecord, BranchEnvironmentState, DeployId};
use ployz_orchestrator::deploy::validated_prepared_manifest;
use ployz_spec::Namespace;
use ployz_store_api::{DeployStore, StoreDriver};

use super::state::{
    BranchApplyingReplayAction, active_branch_prepared_replay,
    active_branch_prepared_replay_record, branch_applying_replay_action,
    branch_environment_applying_record, record_branch_apply_prepared_outcome,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchApplyPreparedLifecycle {
    BestEffort,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchApplyPreparedRecording {
    None,
    Claimed,
}

impl DaemonState {
    pub async fn handle_deploy_apply_prepared(
        &self,
        request: &DeployApplyPreparedRequest,
    ) -> DaemonResponse {
        self.handle_deploy_apply_prepared_with_branch_lifecycle(
            request,
            BranchApplyPreparedLifecycle::BestEffort,
        )
        .await
    }

    async fn handle_deploy_apply_prepared_with_branch_lifecycle(
        &self,
        request: &DeployApplyPreparedRequest,
        lifecycle: BranchApplyPreparedLifecycle,
    ) -> DaemonResponse {
        let active = match self.require_active("NO_MESH", "no mesh is running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        let prepared = match active
            .mesh
            .store
            .get_prepared_deploy(&request.prepared_deploy_id)
            .await
        {
            Ok(Some(prepared)) => prepared,
            Ok(None) => {
                return self.deploy_error_response(
                    "DEPLOY_APPLY_PREPARED_FAILED",
                    PloyzError::Deploy(DeployError::PreparedDeployMissing {
                        prepared_deploy_id: request.prepared_deploy_id.as_str().to_string(),
                    }),
                );
            }
            Err(error) => return self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error),
        };
        let loaded_branch_environment = match active
            .mesh
            .store
            .get_branch_environment(&prepared.namespace)
            .await
        {
            Ok(environment) => environment,
            Err(error) => return self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error),
        };

        let branch_environment = match (lifecycle, loaded_branch_environment) {
            (BranchApplyPreparedLifecycle::Required, None) => {
                return self.err(
                    "BRANCH_ENVIRONMENT_NOT_FOUND",
                    format!(
                        "prepared deploy '{}' is not current for any branch environment",
                        request.prepared_deploy_id
                    ),
                );
            }
            (BranchApplyPreparedLifecycle::Required, Some(environment))
                if environment.prepared_deploy_id.as_ref() != Some(&request.prepared_deploy_id) =>
            {
                return self.err(
                    "BRANCH_PREPARED_DEPLOY_STALE",
                    format!(
                        "prepared deploy '{}' is not current for branch environment '{}'",
                        request.prepared_deploy_id, environment.target_namespace
                    ),
                );
            }
            (BranchApplyPreparedLifecycle::Required, Some(environment)) => Some(environment),
            (BranchApplyPreparedLifecycle::BestEffort, Some(environment))
                if environment.prepared_deploy_id.as_ref() == Some(&request.prepared_deploy_id) =>
            {
                Some(environment)
            }
            (BranchApplyPreparedLifecycle::BestEffort, Some(_) | None) => None,
        };

        if lifecycle == BranchApplyPreparedLifecycle::Required
            && let Some(environment) = branch_environment.as_ref()
            && active_branch_prepared_replay(environment, &request.prepared_deploy_id)
        {
            return self.branch_environment_replay_response(environment.clone());
        }

        let branch_recording = match branch_environment.as_ref() {
            Some(environment) if environment.state == BranchEnvironmentState::Active => {
                if lifecycle == BranchApplyPreparedLifecycle::Required {
                    return self.err(
                        "BRANCH_ENVIRONMENT_ALREADY_ACTIVE",
                        format!(
                            "branch environment '{}' is already active",
                            environment.target_namespace
                        ),
                    );
                }
                BranchApplyPreparedRecording::None
            }
            Some(environment) if environment.state == BranchEnvironmentState::Applying => {
                match branch_applying_replay_action(
                    &active.mesh.store,
                    &prepared,
                    &request.prepared_deploy_id,
                )
                .await
                {
                    Ok(BranchApplyingReplayAction::Replayed(environment)) => {
                        return self.branch_environment_replay_response(*environment);
                    }
                    Ok(BranchApplyingReplayAction::ResumePreparedApply) => {
                        BranchApplyPreparedRecording::Claimed
                    }
                    Ok(BranchApplyingReplayAction::Busy) => {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                environment.target_namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    Err(error) => {
                        return self.err(
                            "BRANCH_ENVIRONMENT_RECORD_FAILED",
                            format!("failed to repair applying branch environment: {error}"),
                        );
                    }
                }
            }
            Some(_) => match active
                .mesh
                .store
                .mark_branch_environment_applying(
                    &prepared.namespace,
                    &request.prepared_deploy_id,
                    ployz_time::now_unix_secs(),
                )
                .await
            {
                Ok(_) => BranchApplyPreparedRecording::Claimed,
                Err(error) if lifecycle == BranchApplyPreparedLifecycle::BestEffort => {
                    if let Some(environment) = active_branch_prepared_replay_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    {
                        return self.branch_environment_replay_response(environment);
                    }
                    if branch_environment_applying_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                    {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                prepared.namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    tracing::warn!(
                        %error,
                        target_namespace = %prepared.namespace,
                        prepared_deploy_id = %request.prepared_deploy_id,
                        "failed to claim branch apply-prepared lifecycle"
                    );
                    BranchApplyPreparedRecording::None
                }
                Err(error) => {
                    if let Some(environment) = active_branch_prepared_replay_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    {
                        return self.branch_environment_replay_response(environment);
                    }
                    if branch_environment_applying_record(
                        &active.mesh.store,
                        &prepared.namespace,
                        &request.prepared_deploy_id,
                    )
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                    {
                        return self.err(
                            "BRANCH_ENVIRONMENT_BUSY",
                            format!(
                                "branch environment '{}' is already applying prepared deploy '{}'",
                                prepared.namespace, request.prepared_deploy_id
                            ),
                        );
                    }
                    return self.err(
                        "BRANCH_ENVIRONMENT_RECORD_FAILED",
                        format!("failed to claim branch apply-prepared lifecycle: {error}"),
                    );
                }
            },
            None => BranchApplyPreparedRecording::None,
        };

        let manifest = match validated_prepared_manifest(&prepared) {
            Ok(manifest) => manifest,
            Err(error) => {
                let response = self.deploy_error_response("DEPLOY_APPLY_PREPARED_FAILED", error);
                if branch_recording != BranchApplyPreparedRecording::None {
                    return self
                        .branch_apply_prepared_response(
                            &active.mesh.store,
                            &prepared.namespace,
                            &request.prepared_deploy_id,
                            response,
                            branch_recording,
                        )
                        .await;
                }
                return response;
            }
        };
        let runtime = match self
            .prepare_deploy_apply_runtime(
                active,
                &manifest.namespace,
                "DEPLOY_APPLY_PREPARED_FAILED",
            )
            .await
        {
            Ok(runtime) => runtime,
            Err(response) => {
                if branch_recording != BranchApplyPreparedRecording::None {
                    return self
                        .branch_apply_prepared_response(
                            &active.mesh.store,
                            &prepared.namespace,
                            &request.prepared_deploy_id,
                            response,
                            branch_recording,
                        )
                        .await;
                }
                return response;
            }
        };
        let response = self
            .apply_prepared_with_runtime(active, &request.prepared_deploy_id, runtime)
            .await;
        if branch_recording != BranchApplyPreparedRecording::None {
            return self
                .branch_apply_prepared_response(
                    &active.mesh.store,
                    &prepared.namespace,
                    &request.prepared_deploy_id,
                    response,
                    branch_recording,
                )
                .await;
        }
        response
    }

    fn branch_environment_replay_response(
        &self,
        environment: BranchEnvironmentRecord,
    ) -> DaemonResponse {
        self.ok_with_payload(
            format!(
                "branch environment '{}' is {}",
                environment.target_namespace, environment.state
            ),
            Some(DaemonPayload::BranchEnvironment(BranchEnvironmentPayload {
                environment,
            })),
        )
    }

    async fn branch_apply_prepared_response(
        &self,
        store: &StoreDriver,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        response: DaemonResponse,
        recording: BranchApplyPreparedRecording,
    ) -> DaemonResponse {
        match record_branch_apply_prepared_outcome(
            store,
            target_namespace,
            prepared_deploy_id,
            &response,
        )
        .await
        {
            Ok(_) => response,
            Err(error) if recording != BranchApplyPreparedRecording::Claimed => {
                tracing::warn!(
                    %error,
                    %target_namespace,
                    %prepared_deploy_id,
                    "failed to record branch apply-prepared outcome"
                );
                response
            }
            Err(error) => self.err(
                "BRANCH_ENVIRONMENT_RECORD_FAILED",
                format!(
                    "failed to record branch apply-prepared outcome: {error}; deploy response was {}: {}",
                    response.code(), response.message()
                ),
            ),
        }
    }

    pub async fn handle_branch_apply_prepared(
        &self,
        request: &BranchApplyPreparedRequest,
    ) -> DaemonResponse {
        self.handle_deploy_apply_prepared_with_branch_lifecycle(
            &DeployApplyPreparedRequest {
                prepared_deploy_id: request.prepared_deploy_id.clone(),
            },
            BranchApplyPreparedLifecycle::Required,
        )
        .await
    }
}
