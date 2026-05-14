use ployz_api::DaemonResponse;
use ployz_model::{
    BranchEnvironmentFailure, BranchEnvironmentRecord, BranchEnvironmentState, DeployId,
    DeployRecordState, PreparedDeployRecord, PreparedDeployState,
};
use ployz_spec::Namespace;
use ployz_store_api::{DeployStore, StoreDriver};

pub(in crate::daemon::handlers::deploy) enum BranchApplyingReplayAction {
    Replayed(Box<BranchEnvironmentRecord>),
    ResumePreparedApply,
    Busy,
}

pub(in crate::daemon::handlers::deploy) async fn record_branch_apply_prepared_outcome(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
    response: &DaemonResponse,
) -> ployz_error::Result<BranchEnvironmentRecord> {
    let updated_at = ployz_time::now_unix_secs();
    if let Some(environment) =
        active_branch_prepared_replay_record(store, target_namespace, prepared_deploy_id).await?
    {
        return Ok(environment);
    }
    if response.is_ok() {
        match store
            .mark_branch_environment_active(
                target_namespace,
                prepared_deploy_id,
                prepared_deploy_id,
                updated_at,
            )
            .await
        {
            Ok(environment) => Ok(environment),
            Err(error) => {
                if let Some(environment) = active_branch_prepared_replay_record(
                    store,
                    target_namespace,
                    prepared_deploy_id,
                )
                .await?
                {
                    return Ok(environment);
                }
                Err(error)
            }
        }
    } else {
        if let Some(environment) =
            active_branch_prepared_replay_record(store, target_namespace, prepared_deploy_id)
                .await?
        {
            return Ok(environment);
        }
        let failure = BranchEnvironmentFailure {
            code: response.code().to_string(),
            message: response.message().to_string(),
            deploy_id: None,
        };
        match store
            .mark_branch_environment_failed(
                target_namespace,
                prepared_deploy_id,
                &failure,
                updated_at,
            )
            .await
        {
            Ok(environment) => Ok(environment),
            Err(error) => {
                if let Some(environment) = active_branch_prepared_replay_record(
                    store,
                    target_namespace,
                    prepared_deploy_id,
                )
                .await?
                {
                    return Ok(environment);
                }
                Err(error)
            }
        }
    }
}

pub(super) fn active_branch_prepared_replay(
    environment: &BranchEnvironmentRecord,
    prepared_deploy_id: &DeployId,
) -> bool {
    environment.state == BranchEnvironmentState::Active
        && environment.prepared_deploy_id.as_ref() == Some(prepared_deploy_id)
        && environment.applied_deploy_id.as_ref() == Some(prepared_deploy_id)
}

pub(super) async fn active_branch_prepared_replay_record(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
) -> ployz_error::Result<Option<BranchEnvironmentRecord>> {
    Ok(store
        .get_branch_environment(target_namespace)
        .await?
        .filter(|environment| active_branch_prepared_replay(environment, prepared_deploy_id)))
}

pub(super) async fn branch_environment_applying_record(
    store: &StoreDriver,
    target_namespace: &Namespace,
    prepared_deploy_id: &DeployId,
) -> ployz_error::Result<Option<BranchEnvironmentRecord>> {
    Ok(store
        .get_branch_environment(target_namespace)
        .await?
        .filter(|environment| {
            environment.state == BranchEnvironmentState::Applying
                && environment.prepared_deploy_id.as_ref() == Some(prepared_deploy_id)
        }))
}

pub(in crate::daemon::handlers::deploy) async fn branch_applying_replay_action(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
    prepared_deploy_id: &DeployId,
) -> ployz_error::Result<BranchApplyingReplayAction> {
    if let Some(environment) =
        active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id).await?
    {
        return Ok(BranchApplyingReplayAction::Replayed(Box::new(environment)));
    }
    if branch_environment_applying_record(store, &prepared.namespace, prepared_deploy_id)
        .await?
        .is_none()
    {
        return Ok(BranchApplyingReplayAction::Busy);
    }
    if store
        .get_deploy_commit(&prepared.namespace, prepared_deploy_id)
        .await?
        .is_some_and(|commit| {
            matches!(
                commit.deploy.state,
                DeployRecordState::Committed { .. } | DeployRecordState::CleanupPending { .. }
            )
        })
        || store
            .get_deploy(prepared_deploy_id)
            .await?
            .is_some_and(|record| {
                matches!(
                    record.state,
                    DeployRecordState::Committed { .. } | DeployRecordState::CleanupPending { .. }
                )
            })
    {
        return Ok(BranchApplyingReplayAction::ResumePreparedApply);
    }
    if prepared.state != PreparedDeployState::Applied {
        return Ok(BranchApplyingReplayAction::Busy);
    }
    match store
        .mark_branch_environment_active(
            &prepared.namespace,
            prepared_deploy_id,
            prepared_deploy_id,
            ployz_time::now_unix_secs(),
        )
        .await
    {
        Ok(environment) => Ok(BranchApplyingReplayAction::Replayed(Box::new(environment))),
        Err(error) => {
            if let Some(environment) =
                active_branch_prepared_replay_record(store, &prepared.namespace, prepared_deploy_id)
                    .await?
            {
                return Ok(BranchApplyingReplayAction::Replayed(Box::new(environment)));
            }
            Err(error)
        }
    }
}
