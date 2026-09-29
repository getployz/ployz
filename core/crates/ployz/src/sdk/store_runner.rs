//! Cloud's Deployment runner. One call claims a Deployment, prepares and confirms it
//! on the Organization's Cluster and records each step's evidence, so the Deploy
//! Intent with its unsealed secrets, the session and the raw SDK evidence never leave
//! it: the caller gets only the Deployment's summary.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::{DeployOutcome, RpcError, RpcErrorCode};
use ployz_store::{
    Actor, ConfigStore, DeploymentId, DeploymentStatus, DeploymentSummary, RunEvidence, RunnerId,
};

use super::{ImageCleanup, Session, connect_connections};
use crate::connect::SystemConnector;
use crate::context::Connection;

/// How often a running Deploy checks whether it was cancelled.
const CANCEL_POLL: Duration = Duration::from_secs(2);

/// Run Deployment `deployment` of `who` as `runner` on one of `connections`, and
/// return its summary once its outcome is recorded. A failed connection or
/// preparation is recorded as not executed; a cancel stops it; losing the session
/// mid-execution records that the outcome is unknown.
///
/// # Errors
/// Returns the Store's `conflict` when this runner has nothing to run (another runner
/// owns it, it was replaced, cancelled or ended, or this runner already prepared it),
/// or a storage error.
pub async fn run_deployment(
    store: Arc<ConfigStore>,
    who: Actor,
    deployment: DeploymentId,
    runner: RunnerId,
    connections: Vec<Connection>,
) -> Result<DeploymentSummary, RpcError> {
    let run = Run {
        store,
        who,
        deployment,
        runner,
    };
    let claimed = {
        let (deployment, runner) = (run.deployment.clone(), run.runner.clone());
        run.store(move |store| store.claim(&deployment, &runner))
            .await?
    };
    if connections.is_empty() {
        return run
            .not_executed("No Server is enrolled in this Organization".into())
            .await;
    }
    let session = match connect_connections(connections, Arc::new(SystemConnector::default())).await
    {
        Ok(session) => session,
        Err(error) => return run.not_executed(error.message).await,
    };
    let recorded = run.execute(&session, claimed.intent, claimed.deletes).await;
    session.close().await;
    recorded
}

/// Which of `connections`' Servers hold each Docker Volume in `sought`: the
/// evidence a Deploy that removes deployed Volumes is admitted with.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or listing the
/// Machines fails.
pub async fn observe_volumes(
    connections: Vec<Connection>,
    sought: Vec<ployz_core::DockerVolumeName>,
) -> Result<ployz_store::VolumeObservation, RpcError> {
    let session = connect_connections(connections, Arc::new(SystemConnector::default())).await?;
    let observed = session.observe_volumes(sought).await;
    session.close().await;
    observed
}

struct Run {
    store: Arc<ConfigStore>,
    who: Actor,
    deployment: DeploymentId,
    runner: RunnerId,
}

impl Run {
    async fn execute(
        &self,
        session: &Session,
        intent: ployz_core::DeployIntent,
        deletes: Vec<ployz_core::DockerVolumeId>,
    ) -> Result<DeploymentSummary, RpcError> {
        let prepared = match session.preview(intent).await {
            Ok(prepared) => prepared,
            Err(error) => return self.not_executed(error.message).await,
        };
        let summary = self
            .record(RunEvidence::Prepared(prepared.preview().clone()))
            .await?;
        if summary.status == DeploymentStatus::Cancelling {
            return self
                .not_executed("Cancelled before it executed".into())
                .await;
        }
        let log_id = self.deployment.as_str().parse().ok();
        let running = match prepared.confirm_with_log_id(log_id, ImageCleanup::Auto) {
            Ok(running) => running,
            Err(error) => return self.not_executed(error.message).await,
        };
        let finished = running.finished();
        tokio::pin!(finished);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        let outcome = loop {
            tokio::select! {
                outcome = &mut finished => break outcome,
                _ = poll.tick() => {
                    // ponytail: a failed read skips one check; the next tick reads again.
                    if self.status().await.ok() == Some(DeploymentStatus::Cancelling) {
                        running.abort();
                    }
                }
            }
        };
        match outcome {
            Ok(outcome) => {
                let removed = if matches!(outcome, DeployOutcome::Success { .. }) {
                    remove_volumes(session, deletes).await
                } else {
                    Vec::new()
                };
                self.record(RunEvidence::Executed {
                    outcome: Box::new(outcome),
                    removed,
                })
                .await
            }
            // The session ended mid-execution: what ran is unknown.
            Err(_) => self.record(RunEvidence::Abandoned).await,
        }
    }

    async fn not_executed(&self, reason: String) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::NotExecuted(reason)).await
    }

    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        self.store(move |store| store.record(&deployment, &runner, evidence))
            .await
    }

    async fn status(&self) -> Result<DeploymentStatus, RpcError> {
        let (who, deployment) = (self.who.clone(), self.deployment.clone());
        self.store(move |store| store.deployment(&who, &deployment))
            .await
            .map(|view| view.deployment.status)
    }

    /// Store calls block on the database, so they run off the async threads.
    async fn store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&ConfigStore) -> Result<T, RpcError> + Send + 'static,
    ) -> Result<T, RpcError> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|_| internal("A Config Store call stopped unexpectedly"))?
    }
}

/// Delete exactly the Docker Volumes admission accepted, never others of the same
/// name. Failing to reach the Cluster deletes none, so their Volumes stay deployed.
async fn remove_volumes(
    session: &Session,
    volumes: Vec<ployz_core::DockerVolumeId>,
) -> Vec<ployz_core::VolumeRemoval> {
    if volumes.is_empty() {
        return Vec::new();
    }
    session
        .remove_volumes(ployz_core::RemoveVolumesRequest {
            volumes,
            force: false,
        })
        .await
        .unwrap_or_default()
}

fn internal(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: message.to_owned(),
        details: serde_json::Value::Null,
    }
}
