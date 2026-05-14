use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::{ActiveMesh, DaemonState};
use ployz_api::DaemonResponse;
use ployz_cert_acme::InstantAcmeIssuerFactory;
use ployz_cert_acme_api::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_config::RuntimeTarget;
use ployz_model::{DeployId, DeployPhaseCommitPolicy, DeployPhaseFailure, DeployPhaseState};
use ployz_nats::{NatsDeployLock, NatsLocks, NatsStore};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::{
    DeployApplyPreconditions, apply_prepared_with_certificate_coordination,
    apply_with_deploy_id_and_preconditions, new_deploy_id,
};
use ployz_spec::{DeployManifest, Namespace};
use ployz_store_api::{DeployStore, StoreDriver, StoreRuntimeControl};

use super::node::NatsDeployParticipantClient;

const DEPLOY_LOCK_TTL: Duration = Duration::from_secs(30 * 60);
const DEPLOY_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(10 * 60);

type DeployApplyFuture<'a> =
    Pin<Box<dyn Future<Output = ployz_error::Result<ployz_model::DeployApplyResult>> + Send + 'a>>;

pub(super) struct DeployApplyRuntime {
    nats_store: NatsStore,
    nats_locks: NatsLocks,
    deploy_lock: NatsDeployLock,
}

#[derive(Debug, Clone, Copy)]
struct DeployApplyLabels {
    operation: &'static str,
    error_code: &'static str,
    encode_code: &'static str,
}

impl DeployApplyRuntime {
    pub(super) async fn release(self, warning: &'static str) {
        if let Err(error) = self.deploy_lock.release().await {
            tracing::warn!(%error, "{warning}");
        }
    }
}

impl DaemonState {
    pub(super) async fn prepare_deploy_apply_runtime(
        &self,
        active: &ActiveMesh,
        namespace: &Namespace,
        setup_failure_code: &'static str,
    ) -> Result<DeployApplyRuntime, DaemonResponse> {
        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope = ployz_nats::NatsScope::local_for_storage_participation(
            &active.config.storage_participation,
        );
        let nats_store =
            match ployz_nats::NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
                Ok(store) => store.with_asset_policy(active.config.storage_replicas),
                Err(error) => return Err(self.err(setup_failure_code, error.to_string())),
            };
        if let Err(error) = nats_store.start().await {
            return Err(self.err(setup_failure_code, error.to_string()));
        }
        let nats_locks = match NatsLocks::new(&nats_store).await {
            Ok(locks) => locks,
            Err(error) => return Err(self.err(setup_failure_code, error.to_string())),
        };
        let deploy_lock = match NatsDeployLock::acquire(
            nats_locks.clone(),
            namespace,
            &ReservationId::random().0,
            &self.identity.machine_id,
            DEPLOY_LOCK_TTL,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => return Err(self.err("DEPLOY_LOCK_FAILED", error.to_string())),
        };
        Ok(DeployApplyRuntime {
            nats_store,
            nats_locks,
            deploy_lock,
        })
    }

    pub(super) async fn apply_manifest_with_runtime(
        &self,
        active: &ActiveMesh,
        manifest: &DeployManifest,
        runtime: DeployApplyRuntime,
        preconditions: DeployApplyPreconditions,
    ) -> DaemonResponse {
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                runtime.nats_locks.clone(),
                self.identity.machine_id.clone(),
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::NatsChallengeReadiness::new(
                active.mesh.store.clone(),
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );
        let participant_client = NatsDeployParticipantClient::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );

        let deploy_id = new_deploy_id();
        let apply = Box::pin(apply_with_deploy_id_and_preconditions(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            manifest,
            deploy_id.clone(),
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
            preconditions,
        ));
        self.run_locked_deploy_apply(
            active,
            deploy_id,
            runtime,
            apply,
            DeployApplyLabels {
                operation: "apply",
                error_code: "DEPLOY_APPLY_FAILED",
                encode_code: "ENCODE_DEPLOY",
            },
        )
        .await
    }

    pub(super) async fn apply_prepared_with_runtime(
        &self,
        active: &ActiveMesh,
        prepared_deploy_id: &DeployId,
        runtime: DeployApplyRuntime,
    ) -> DaemonResponse {
        let certificate_coordinator = Arc::new(
            crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                runtime.nats_locks.clone(),
                self.identity.machine_id.clone(),
            ),
        );
        let account_coordinator: Arc<dyn AcmeAccountCoordinator> = certificate_coordinator.clone();
        let challenge_readiness = Arc::new(
            crate::daemon::cert_coordination::NatsChallengeReadiness::new(
                active.mesh.store.clone(),
            ),
        );
        let issuer_factory = Arc::new(InstantAcmeIssuerFactory::new(
            CertificateManagerConfig::from_env(),
        ));
        let prober = crate::daemon::deploy_probe::NatsRpcProbe::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );
        let participant_client = NatsDeployParticipantClient::new(
            ployz_nats::NatsNodeRpcClient::for_store(&runtime.nats_store),
        );

        let deploy_id = prepared_deploy_id.clone();
        let apply = Box::pin(apply_prepared_with_certificate_coordination(
            &active.mesh.store,
            &participant_client,
            &self.identity.machine_id,
            deploy_id.clone(),
            certificate_coordinator,
            account_coordinator,
            challenge_readiness,
            issuer_factory,
            &prober,
        ));
        self.run_locked_deploy_apply(
            active,
            deploy_id,
            runtime,
            apply,
            DeployApplyLabels {
                operation: "apply-prepared",
                error_code: "DEPLOY_APPLY_PREPARED_FAILED",
                encode_code: "ENCODE_DEPLOY_RESULT",
            },
        )
        .await
    }

    async fn run_locked_deploy_apply(
        &self,
        active: &ActiveMesh,
        deploy_id: DeployId,
        runtime: DeployApplyRuntime,
        mut apply: DeployApplyFuture<'_>,
        labels: DeployApplyLabels,
    ) -> DaemonResponse {
        let mut deploy_lock_renewer = tokio::spawn(renew_deploy_lock(
            runtime.deploy_lock.clone(),
            DEPLOY_LOCK_TTL,
            DEPLOY_LOCK_RENEW_INTERVAL,
        ));
        let result = tokio::select! {
            result = &mut apply => result,
            renewal = &mut deploy_lock_renewer => {
                let message = match renewal {
                    Ok(Ok(())) => {
                        format!(
                            "deploy lock renewal task exited before {} completed",
                            labels.operation
                        )
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => format!("deploy lock renewal task failed: {error}"),
                };
                match mark_deploy_failed_after_lock_loss(&active.mesh.store, &deploy_id, &message).await {
                    DeployLockLossOutcome::PastCommit => {
                        tracing::warn!(
                            %deploy_id,
                            %message,
                            operation = %labels.operation,
                            "deploy lock was lost after commit point; waiting for operation to finish"
                        );
                        (&mut apply).await
                    }
                    DeployLockLossOutcome::MarkedFailed | DeployLockLossOutcome::NotApplying => {
                        runtime.release("failed to release NATS deploy lock after renewal failure").await;
                        return self.err("DEPLOY_LOCK_FAILED", message);
                    }
                }
            }
        };
        deploy_lock_renewer.abort();
        if let Err(error) = deploy_lock_renewer.await
            && !error.is_cancelled()
        {
            tracing::warn!(%error, "deploy lock renewal task failed during shutdown");
        }
        runtime.release("failed to release NATS deploy lock").await;
        match result {
            Ok(result) => self.ok_json_pretty(&result, labels.encode_code, "encode deploy result"),
            Err(err) => self.deploy_error_response(labels.error_code, err),
        }
    }
}

async fn renew_deploy_lock(
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
pub(super) enum DeployLockLossOutcome {
    MarkedFailed,
    PastCommit,
    NotApplying,
}

pub(super) async fn mark_deploy_failed_after_lock_loss(
    store: &StoreDriver,
    deploy_id: &DeployId,
    message: &str,
) -> DeployLockLossOutcome {
    let deploy = match store.get_deploy(deploy_id).await {
        Ok(Some(mut deploy)) => {
            match deploy.state() {
                ployz_model::DeployState::Committed
                | ployz_model::DeployState::CleanupPending
                | ployz_model::DeployState::CheckpointCommitted => {
                    return DeployLockLossOutcome::PastCommit;
                }
                ployz_model::DeployState::Applying => {}
                ployz_model::DeployState::Planning
                | ployz_model::DeployState::FailedAfterCheckpoint
                | ployz_model::DeployState::Failed => {
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
