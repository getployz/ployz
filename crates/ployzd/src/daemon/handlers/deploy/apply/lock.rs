use std::collections::HashSet;
use std::time::Duration;

use ployz_model::{
    DeployId, DeployPhaseCommitPolicy, DeployPhaseFailure, DeployPhaseState, DeployState,
};
use ployz_nats::{NatsDeployLock, NatsLocks, NatsStore};
use ployz_spec::Namespace;
use ployz_store_api::{DeployStore, StoreDriver};

pub(super) const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
pub(super) const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);

pub(in crate::daemon::handlers::deploy) struct DeployApplyRuntime {
    pub(super) nats_store: NatsStore,
    pub(super) nats_locks: NatsLocks,
    pub(super) deploy_lock: NatsDeployLock,
}

impl DeployApplyRuntime {
    pub(in crate::daemon::handlers::deploy) async fn release(self, warning: &'static str) {
        if let Err(error) = self.deploy_lock.release().await {
            tracing::warn!(%error, "{warning}");
        }
    }
}

pub(super) async fn renew_deploy_lock(
    deploy_lock: NatsDeployLock,
    ttl: Duration,
    interval: Duration,
) -> ployz_error::Result<()> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        deploy_lock.renew(ttl).await?;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon::handlers::deploy) enum DeployLockLossOutcome {
    MarkedFailed,
    PastCommit,
    NotApplying,
}

pub(in crate::daemon::handlers::deploy) async fn mark_deploy_failed_after_lock_loss(
    store: &StoreDriver,
    deploy_id: &DeployId,
    message: &str,
) -> DeployLockLossOutcome {
    let deploy = match store.get_deploy(deploy_id).await {
        Ok(Some(mut deploy)) => {
            match deploy.state() {
                DeployState::Committed
                | DeployState::CleanupPending
                | DeployState::CheckpointCommitted => {
                    return DeployLockLossOutcome::PastCommit;
                }
                DeployState::Applying => {}
                DeployState::Planning
                | DeployState::FailedAfterCheckpoint
                | DeployState::Failed => {
                    return DeployLockLossOutcome::NotApplying;
                }
            }
            if deploy_has_checkpoint_commit_point(store, &deploy.namespace, deploy_id).await {
                return DeployLockLossOutcome::PastCommit;
            }
            let finished_at = ployz_time::now_unix_secs();
            let mut summary_json = deploy.summary_json().to_string();
            if let Ok(mut preview) =
                serde_json::from_str::<ployz_model::DeployPreview>(deploy.summary_json())
            {
                preview
                    .warnings
                    .push(format!("deploy lock lost during apply: {message}"));
                if let Ok(next_summary_json) = serde_json::to_string(&preview) {
                    summary_json = next_summary_json;
                }
            }
            deploy.mark_failed(finished_at, summary_json);
            deploy
        }
        Ok(None) => return DeployLockLossOutcome::NotApplying,
        Err(error) => {
            tracing::warn!(%error, %deploy_id, "failed to read deploy record after lock loss");
            return DeployLockLossOutcome::NotApplying;
        }
    };
    if let Err(error) = store.write_deploy_status(&deploy).await {
        tracing::warn!(%error, %deploy_id, "failed to mark deploy failed after lock loss");
        return DeployLockLossOutcome::NotApplying;
    }
    if let Err(error) = mark_running_deploy_phases_failed_after_lock_loss(
        store,
        &deploy.namespace,
        deploy_id,
        message,
    )
    .await
    {
        tracing::warn!(
            %error,
            %deploy_id,
            "failed to mark running deploy phases failed after lock loss"
        );
    }
    DeployLockLossOutcome::MarkedFailed
}

async fn deploy_has_checkpoint_commit_point(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
) -> bool {
    let phases = match store.list_deploy_phases(namespace, deploy_id).await {
        Ok(phases) => phases,
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy phases while classifying lock loss"
            );
            return false;
        }
    };
    let checkpoint_phase_ids = phases
        .iter()
        .filter(|phase| phase.commit_policy() == DeployPhaseCommitPolicy::Checkpoint)
        .map(|phase| {
            DeployId::new(format!(
                "{}:phase:{}",
                deploy_id.as_str(),
                phase.phase_id.as_str()
            ))
        })
        .collect::<HashSet<_>>();
    if checkpoint_phase_ids.is_empty() {
        return false;
    }
    if phases.iter().any(|phase| {
        phase.commit_policy() == DeployPhaseCommitPolicy::Checkpoint
            && matches!(phase.lifecycle_state(), DeployPhaseState::Succeeded { .. })
    }) {
        return true;
    }
    match store.list_deploy_releases(namespace).await {
        Ok(releases) => {
            if releases
                .iter()
                .any(|release| checkpoint_phase_ids.contains(&release.release.updated_by_deploy_id))
            {
                return true;
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy releases while classifying lock loss"
            );
        }
    }
    match store.list_volumes(namespace).await {
        Ok(volumes) => volumes.iter().any(|volume| {
            checkpoint_phase_ids.contains(&volume.created_by_deploy_id)
                || checkpoint_phase_ids.contains(&volume.last_modified_by_deploy_id)
        }),
        Err(error) => {
            tracing::warn!(
                %error,
                %deploy_id,
                "failed to inspect deploy volumes while classifying lock loss"
            );
            false
        }
    }
}

async fn mark_running_deploy_phases_failed_after_lock_loss(
    store: &StoreDriver,
    namespace: &Namespace,
    deploy_id: &DeployId,
    message: &str,
) -> ployz_error::Result<()> {
    let phases = store.list_deploy_phases(namespace, deploy_id).await?;
    let completed_at = ployz_time::now_unix_secs();
    for mut phase in phases {
        if !matches!(
            phase.lifecycle_state(),
            DeployPhaseState::Pending | DeployPhaseState::Running
        ) {
            continue;
        }
        phase.mark_failed(
            completed_at,
            DeployPhaseFailure {
                code: "DEPLOY_LOCK_LOST".into(),
                message: format!("deploy lock lost during apply: {message}"),
            },
        );
        store.upsert_deploy_phase(&phase).await?;
    }
    Ok(())
}
