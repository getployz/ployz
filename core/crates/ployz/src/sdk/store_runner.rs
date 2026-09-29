//! Cloud's Deployment runner. One call claims a Deployment, builds its Git Services,
//! prepares and confirms it on the Organization's Cluster and records each step's
//! evidence, so the Deploy Intent with its unsealed secrets, the session and the raw
//! SDK evidence never leave it: the caller gets only the Deployment's summary.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::{DeployOutcome, RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, BuildReport, BuildStatus, Claimed, ConfigStore, DeploymentId, DeploymentStatus,
    DeploymentSummary, GitSource, RunEvidence, RunnerId,
};
use serde_json::Value;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput};
use super::{ImageCleanup, PreparedDeploy, RunningBuild, Session, connect_connections};
use crate::connect::SystemConnector;
use crate::context::Connection;

/// How often a running Deploy checks whether it was cancelled.
const CANCEL_POLL: Duration = Duration::from_secs(2);

/// How often a build's new log output is recorded.
const LOG_FLUSH: Duration = Duration::from_secs(3);

/// Each Git Service's checkout at its pinned commit, by runtime Service name, or why
/// Cloud could not read them. Users read the reason: it holds no secret.
pub type Checkouts = Result<BTreeMap<ServiceName, PathBuf>, String>;

/// Run Deployment `deployment` of `who` as `runner` on one of `connections`, and
/// return its summary once its outcome is recorded. Its Git Services build first,
/// each from its checkout in `checkouts`, at once; then preparation reuses their
/// images. A failed connection, build or preparation is recorded as not executed; a
/// cancel stops it; losing the session mid-execution records that the outcome is
/// unknown.
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
    checkouts: Checkouts,
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
    let checkouts = match checkouts {
        Ok(checkouts) => checkouts,
        Err(reason) => return run.not_executed(reason).await,
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
    let recorded = run.execute(&session, claimed, checkouts).await;
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
        claimed: Claimed,
        checkouts: BTreeMap<ServiceName, PathBuf>,
    ) -> Result<DeploymentSummary, RpcError> {
        let deletes = claimed.deletes.clone();
        let prepared = if claimed.sources.is_empty() {
            session.preview(claimed.intent).await
        } else {
            match self.build(session, &claimed, &checkouts).await? {
                Ok(receipts) => self.prepare(session, claimed, checkouts, receipts).await,
                Err(reason) => return self.not_executed(reason).await,
            }
        };
        let prepared = match prepared {
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

    /// Build every Git Service at once, each from its own checkout, and record each
    /// build's progress, log and receipt. Returns every receipt, or why a build failed
    /// once all of them ended: the ones that succeeded still count on a retry.
    async fn build(
        &self,
        session: &Session,
        claimed: &Claimed,
        checkouts: &BTreeMap<ServiceName, PathBuf>,
    ) -> Result<Result<BTreeMap<ServiceName, BuildReceipt>, String>, RpcError> {
        let mut builds = Vec::new();
        for (index, source) in claimed.sources.iter().enumerate() {
            let (Some(commit), Some(checkout)) = (&source.commit, checkouts.get(&source.service))
            else {
                return Ok(Err(format!(
                    "{} has no source at a pinned commit",
                    source.service
                )));
            };
            let hint = claimed
                .receipts
                .get(&source.service)
                .and_then(|receipt| serde_json::from_value::<BuildReceipt>(receipt.clone()).ok());
            let input = PreparationInput {
                deployment: only(&claimed.input, &source.service),
                sources: BTreeMap::from([(source.service.clone(), checkout.clone())]),
                source_commits: BTreeMap::from([(source.service.clone(), commit.clone())]),
                uploads: BTreeMap::new(),
                build_receipts: hint
                    .clone()
                    .map(|hint| BTreeMap::from([(source.service.clone(), hint)]))
                    .unwrap_or_default(),
                build_index: index,
                preferred_machine: None,
            };
            builds.push((source, hint, session.build(input, None)?));
        }
        let all = futures_util::future::join_all(
            builds
                .iter()
                .map(|(source, hint, running)| self.follow(source, hint.as_ref(), running)),
        );
        tokio::pin!(all);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        let ended = loop {
            tokio::select! {
                ended = &mut all => break ended,
                _ = poll.tick() => {
                    if self.status().await.ok() == Some(DeploymentStatus::Cancelling) {
                        for (_, _, running) in &builds {
                            running.abort();
                        }
                    }
                }
            }
        };
        let mut receipts = BTreeMap::new();
        let mut failed = Vec::new();
        for (ended, (source, _, _)) in ended.into_iter().zip(&builds) {
            match ended? {
                Ok(receipt) => {
                    receipts.insert(source.service.clone(), receipt);
                }
                Err(message) => failed.push(format!("{}: {message}", source.service)),
            }
        }
        if !failed.is_empty() {
            return Ok(Err(format!("Build failed. {}", failed.join("; "))));
        }
        Ok(Ok(receipts))
    }

    /// Follow one build to its end, recording its progress and log as it goes.
    async fn follow(
        &self,
        source: &GitSource,
        hint: Option<&BuildReceipt>,
        running: &RunningBuild,
    ) -> Result<Result<BuildReceipt, String>, RpcError> {
        let service = &source.service;
        self.report(service, BuildStatus::Building, None, String::new())
            .await?;
        let mut log = String::new();
        let mut flushed = tokio::time::Instant::now();
        while let Some(event) = running.next().await {
            log.push_str(&log_line(&event));
            if flushed.elapsed() >= LOG_FLUSH && !log.is_empty() {
                // ponytail: a failed flush keeps the output for the next one.
                if self
                    .report(service, BuildStatus::Building, None, log.clone())
                    .await
                    .is_ok()
                {
                    log.clear();
                }
                flushed = tokio::time::Instant::now();
            }
        }
        let (status, message, receipt) = match running.finished().await {
            Ok(BuildOutcome::Built { receipt }) => {
                let reused = hint.is_some_and(|hint| {
                    hint.fingerprint == receipt.fingerprint
                        && hint.image.reference == receipt.image.reference
                });
                let status = if reused {
                    BuildStatus::Reused
                } else {
                    BuildStatus::Built
                };
                (status, None, Some(receipt))
            }
            Ok(BuildOutcome::Queued) => (
                BuildStatus::Failed,
                Some("No Server started the build".to_owned()),
                None,
            ),
            Err(error) => (BuildStatus::Failed, Some(error.message), None),
        };
        self.report(service, status, message.clone(), log).await?;
        let Some(receipt) = receipt else {
            return Ok(Err(message.unwrap_or_default()));
        };
        let recorded = serde_json::to_value(&receipt).expect("a build receipt is JSON");
        self.record(RunEvidence::Built(BTreeMap::from([(
            service.clone(),
            recorded,
        )])))
        .await?;
        Ok(Ok(receipt))
    }

    /// Prepare the Deployment with its Git Services' fresh receipts, so preparation
    /// reuses their images and only delivers them.
    async fn prepare(
        &self,
        session: &Session,
        claimed: Claimed,
        checkouts: BTreeMap<ServiceName, PathBuf>,
        receipts: BTreeMap<ServiceName, BuildReceipt>,
    ) -> Result<PreparedDeploy, RpcError> {
        let input = PreparationInput {
            deployment: claimed.input,
            source_commits: claimed
                .sources
                .into_iter()
                .filter_map(|source| Some((source.service, source.commit?)))
                .collect(),
            sources: checkouts,
            uploads: BTreeMap::new(),
            build_receipts: receipts,
            build_index: 0,
            preferred_machine: None,
        };
        session
            .prepare_with(input, claimed.intent.registry_auth)?
            .finished()
            .await
    }

    async fn report(
        &self,
        service: &ServiceName,
        status: BuildStatus,
        message: Option<String>,
        log: String,
    ) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::Build(BuildReport {
            service: service.clone(),
            status,
            message,
            log,
        }))
        .await
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

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
fn only(input: &Value, service: &ServiceName) -> Value {
    let mut input = input.clone();
    if let Some(snapshots) = input.get_mut("snapshots").and_then(Value::as_array_mut) {
        snapshots.retain(|snapshot| {
            snapshot
                .pointer("/config/privateDns")
                .and_then(Value::as_str)
                == Some(service.as_str())
        });
    }
    if let Some(selected) = input.get_mut("selected") {
        *selected = Value::Array(Vec::new());
    }
    input
}

/// What one build progress event adds to its log.
fn log_line(event: &Value) -> String {
    let text = |pointer: &str| event.pointer(pointer).and_then(Value::as_str);
    if let Some(machine) = text("/Selected/machine/name") {
        return format!("Building on {machine}\n");
    }
    if let Some(output) = text("/Build/StepOutput/text") {
        return output.to_owned();
    }
    if let Some(bytes) = event.pointer("/Build/Output").and_then(Value::as_array) {
        let bytes: Vec<u8> = bytes
            .iter()
            .filter_map(|byte| u8::try_from(byte.as_u64()?).ok())
            .collect();
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let name = text("/Build/Step/name").unwrap_or_default();
    if let Some(error) = text("/Build/Step/error") {
        return format!("ERROR {name}: {error}\n");
    }
    if text("/Build/Step/completed").is_some() {
        let cached = event.pointer("/Build/Step/cached") == Some(&Value::Bool(true));
        return format!("{} {name}\n", if cached { "CACHED" } else { "DONE" });
    }
    String::new()
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
