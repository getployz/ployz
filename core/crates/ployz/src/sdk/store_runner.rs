//! Cloud's Deployment runner. One call claims a Deployment, prepares and confirms it
//! on the Organization's Cluster and records each step's evidence, so the Deploy
//! Intent with its unsealed secrets, the session and the raw SDK evidence never leave
//! it: the caller gets only the Deployment's summary.

use std::sync::Arc;
use std::time::Duration;

use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{
    Actor, ConfigStore, DeploymentId, DeploymentStatus, DeploymentSummary, RunEvidence, RunnerId,
    Written,
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
    let recorded = run.execute(&session, claimed.intent).await;
    session.close().await;
    recorded
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
            Ok(outcome) => self.record(RunEvidence::Executed(Box::new(outcome))).await,
            // The session ended mid-execution: what ran is unknown.
            Err(_) => self.record(RunEvidence::Abandoned).await,
        }
    }

    async fn not_executed(&self, reason: String) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::NotExecuted(reason)).await
    }

    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        match self
            .store(move |store| store.record(&deployment, &runner, evidence))
            .await?
        {
            Written::Deployment(summary) => Ok(summary),
            _ => Err(internal(
                "The Store recorded something other than a Deployment",
            )),
        }
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

fn internal(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: message.to_owned(),
        details: serde_json::Value::Null,
    }
}
