mod lock;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::daemon::{ActiveMesh, DaemonState};
use ployz_api::DaemonResponse;
use ployz_cert_acme::InstantAcmeIssuerFactory;
use ployz_cert_acme_api::{AcmeAccountCoordinator, CertificateManagerConfig};
use ployz_model::DeployId;
use ployz_nats::{NatsDeployLock, NatsLocks};
use ployz_orchestrator::coordination::ReservationId;
use ployz_orchestrator::deploy::{
    DeployApplyPreconditions, apply_prepared_with_certificate_coordination,
    apply_with_deploy_id_and_preconditions, new_deploy_id,
};
use ployz_spec::{DeployManifest, Namespace};

use super::node::NatsDeployParticipantClient;
use lock::{DEPLOY_LOCK_RENEW_INTERVAL, DEPLOY_LOCK_TTL, DeployApplyRuntime, renew_deploy_lock};
pub(super) use lock::{DeployLockLossOutcome, mark_deploy_failed_after_lock_loss};

type DeployApplyFuture<'a> =
    Pin<Box<dyn Future<Output = ployz_error::Result<ployz_model::DeployApplyResult>> + Send + 'a>>;

#[derive(Debug, Clone, Copy)]
struct DeployApplyLabels {
    operation: &'static str,
    error_code: &'static str,
    encode_code: &'static str,
}

impl DaemonState {
    pub(super) async fn prepare_deploy_apply_runtime(
        &self,
        active: &ActiveMesh,
        namespace: &Namespace,
        setup_failure_code: &'static str,
    ) -> Result<DeployApplyRuntime, DaemonResponse> {
        let nats_store = self
            .connect_deploy_nats_store(active, setup_failure_code)
            .await?;
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
