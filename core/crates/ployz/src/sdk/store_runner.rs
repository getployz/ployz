//! Cloud's Deployment runner. One call claims a Deployment, builds its Git and
//! uploaded Services, prepares and confirms it on the Organization's Cluster and records each step's
//! evidence, so the Deploy Intent with its unsealed secrets, the session and the raw
//! SDK evidence never leave it: the caller gets only the Deployment's summary.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::{DeployOutcome, RpcError, RpcErrorCode, ServiceName};
use ployz_store::{
    BuildReport, BuildStatus, Builder, Claimed, CommitSha, ConfigStore, DeploymentId,
    DeploymentStatus, DeploymentSummary, Failure, RowState, RowTracker, RunEvidence, RunnerId,
};
use serde::Deserialize as _;
use serde_json::Value;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput, UploadDigest, needs_upload};
use super::{ImageCleanup, PreparedDeploy, RunningBuild, Session, connect_connections};
use crate::connect::SystemConnector;
use crate::context::Connection;

/// How often a running Deployment renews its lease and checks whether it was cancelled.
const CANCEL_POLL: Duration = Duration::from_secs(2);

/// How often a build's new log output is recorded.
const LOG_FLUSH: Duration = Duration::from_secs(3);

/// Each Git Service's checkout at its pinned commit, by runtime Service name, and the
/// Deployment's upload, if Cloud still holds it; or why Cloud could not read them.
/// Users read the reason: it holds no secret.
pub type Checkouts = Result<Sources, String>;

/// Where a Deployment's Services build from.
#[derive(Debug, Default)]
pub struct Sources {
    /// Each Git Service's checkout, by runtime Service name.
    pub checkouts: BTreeMap<ServiceName, PathBuf>,
    /// The Deployment's uploaded directory. Without it, uploaded Services reuse
    /// their latest usable image or need a new upload.
    pub upload: Option<PathBuf>,
}

/// Why a Deployment's builds didn't all succeed.
enum Unbuilt {
    /// Users read it.
    Failed(String),
    /// These uploaded Services need a new upload.
    UploadNeeded(Vec<ServiceName>),
}

/// One Service to build.
struct Target {
    service: ServiceName,
    build: Build,
}

/// What a Service builds from.
enum Build {
    /// A Git Service's checkout at its pinned commit.
    Git {
        commit: CommitSha,
        checkout: PathBuf,
        /// The Server the build tries first.
        preferred_machine: Option<ployz_core::MachineId>,
    },
    /// An uploaded Service, from the upload when Cloud still holds it.
    Upload {
        digest: UploadDigest,
        source: Option<PathBuf>,
        /// Its Build Order has GitHub, which can't build it.
        github_skipped: bool,
    },
}

impl Target {
    /// This target's part of a preparation input.
    fn input(&self, input: &mut PreparationInput) {
        let service = self.service.clone();
        match &self.build {
            Build::Git {
                commit, checkout, ..
            } => {
                input
                    .source_commits
                    .insert(service.clone(), commit.to_string());
                input.sources.insert(service, checkout.clone());
            }
            Build::Upload { digest, source, .. } => {
                input.uploads.insert(service.clone(), digest.clone());
                input.sources.extend(one(&service, source.clone()));
            }
        }
    }
}

/// The receipts `claimed` holds for `service`, own first, that decode.
fn receipts(claimed: &Claimed, service: &ServiceName) -> Vec<BuildReceipt> {
    claimed
        .receipts
        .get(service)
        .into_iter()
        .flatten()
        .filter_map(|receipt| BuildReceipt::deserialize(receipt).ok())
        .collect()
}

/// Run Deployment `deployment` as `runner` on one of `connections`, and
/// return its summary once its outcome is recorded. Its Git and uploaded Services
/// build first, at once, each from its checkout or the upload in `checkouts`; then
/// preparation reuses their images. A failed connection, build or preparation is
/// recorded as not executed; an uploaded Service with neither its upload nor a usable
/// image records that it needs a new upload; a cancel stops it; losing the session
/// mid-execution records that the outcome is unknown.
///
/// # Errors
/// Returns the Store's `conflict` when this runner has nothing to run (another runner
/// owns it, it was replaced, cancelled or ended, or this runner already prepared it),
/// or a storage error.
pub async fn run_deployment(
    store: Arc<ConfigStore>,
    deployment: DeploymentId,
    runner: RunnerId,
    connections: Vec<Connection>,
    checkouts: Checkouts,
) -> Result<DeploymentSummary, RpcError> {
    let run = Run {
        store,
        deployment,
        runner,
    };
    let claimed = {
        let (deployment, runner) = (run.deployment.clone(), run.runner.clone());
        super::store_call(&run.store, move |store| store.claim(&deployment, &runner)).await?
    };
    let (targets, built) = match checkouts
        .map_err(Unbuilt::Failed)
        .and_then(|sources| targets(&claimed, sources))
    {
        Ok(targets) => targets,
        Err(unbuilt) => return run.unbuilt(unbuilt).await,
    };
    if connections.is_empty() {
        return run
            .not_executed("No Server is enrolled in this Organization".into())
            .await;
    }
    let session = match run
        .connecting(connect_connections(
            connections,
            Arc::new(SystemConnector::default()),
        ))
        .await
    {
        Ok(Some(session)) => session,
        Ok(None) => return run.not_executed("Cancelled before connecting".into()).await,
        // Users read it on the Deployment: plain words and one action first, then
        // what the connection said.
        Err(error) => {
            let Failure { reason, mut cause } = Failure::from(&error);
            cause.insert(0, reason);
            return run
                .not_executed(Failure {
                    reason:
                        "Ployz couldn't reach your Servers. Check that they're online, then retry."
                            .into(),
                    cause,
                })
                .await;
        }
    };
    let recorded = run.execute(&session, claimed, targets, built).await;
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

/// Every copy of every Volume on `connections`' Servers, by role: what Cloud's sweep
/// finds orphaned mirror slots with.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or listing the
/// Machines fails.
pub async fn observe_copies(
    connections: Vec<Connection>,
) -> Result<super::CopyObservation, RpcError> {
    let session = connect_connections(connections, Arc::new(SystemConnector::default())).await?;
    let observed = session.observe_copies().await;
    session.close().await;
    observed
}

struct Run {
    store: Arc<ConfigStore>,
    deployment: DeploymentId,
    runner: RunnerId,
}

impl Run {
    async fn execute(
        &self,
        session: &Session,
        claimed: Claimed,
        targets: Vec<Target>,
        built: BTreeMap<ServiceName, BuildReceipt>,
    ) -> Result<DeploymentSummary, RpcError> {
        let deletes = claimed.deletes.clone();
        let prepared = if targets.is_empty() && built.is_empty() {
            self.renewing(session.preview(claimed.intent), || {
                session.inner.cancel.cancel()
            })
            .await
        } else {
            match self.build(session, &claimed, &targets).await? {
                Ok(mut receipts) => {
                    receipts.extend(built);
                    self.prepare(session, claimed, targets, receipts).await
                }
                Err(unbuilt) => return self.unbuilt(unbuilt).await,
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return match needs_upload(&error) {
                    Some(services) => self.unbuilt(Unbuilt::UploadNeeded(services)).await,
                    None => self.not_executed(Failure::from(&error)).await,
                };
            }
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
            Err(error) => return self.not_executed(Failure::from(&error)).await,
        };
        let outcome = self
            .renewing(self.executing(session, &running), || running.abort())
            .await;
        match outcome {
            Ok((outcome, progress)) => {
                let removed = if matches!(outcome, DeployOutcome::Success { .. }) {
                    // Deleting is never interrupted: a half-deleted set stays accepted.
                    self.renewing(remove_volumes(session, deletes), || ()).await
                } else {
                    Vec::new()
                };
                self.record(RunEvidence::Executed {
                    progress,
                    outcome: Box::new(outcome),
                    removed,
                })
                .await
            }
            // The session ended mid-execution: what ran is unknown.
            Err(_) => self.record(RunEvidence::Abandoned).await,
        }
    }

    /// Build every target at once and record each build's progress, log and receipt.
    /// Returns every receipt, or why they didn't all build once all of them ended: the
    /// ones that succeeded still count on a retry.
    async fn build(
        &self,
        session: &Session,
        claimed: &Claimed,
        targets: &[Target],
    ) -> Result<Result<BTreeMap<ServiceName, BuildReceipt>, Unbuilt>, RpcError> {
        let mut builds = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            let service = &target.service;
            let mut input = PreparationInput {
                deployment: only(&claimed.input, service),
                build_receipts: one(service, Some(receipts(claimed, service))),
                build_index: index,
                preferred_machine: match &target.build {
                    Build::Git {
                        preferred_machine, ..
                    } => *preferred_machine,
                    Build::Upload { .. } => None,
                },
                ..PreparationInput::default()
            };
            target.input(&mut input);
            let running = match session.build(input, None) {
                Ok(running) => running,
                Err(error) => return Err(stop_builds(&builds, error).await),
            };
            builds.push((target, running));
            if let Err(error) = self
                .report(service, BuildStatus::Building, None, String::new())
                .await
            {
                return Err(stop_builds(&builds, error).await);
            }
        }
        let all = futures_util::future::join_all(
            builds
                .iter()
                .map(|(target, running)| self.follow(target, running)),
        );
        let ended = self
            .renewing(all, || {
                for (_, running) in &builds {
                    running.abort();
                }
            })
            .await;
        let mut receipts = BTreeMap::new();
        let mut failed = Vec::new();
        let mut uploads = Vec::new();
        for (ended, (target, _)) in ended.into_iter().zip(&builds) {
            match ended? {
                Ok(receipt) => {
                    receipts.insert(target.service.clone(), receipt);
                }
                Err(error) => match needs_upload(&error) {
                    Some(services) => uploads.extend(services),
                    None => failed.push(format!("{}: {}", target.service, crate::ui::row(&error))),
                },
            }
        }
        // A new upload is the one fix a user can act on first.
        if !uploads.is_empty() {
            return Ok(Err(Unbuilt::UploadNeeded(uploads)));
        }
        if !failed.is_empty() {
            return Ok(Err(Unbuilt::Failed(format!(
                "Build failed. {}",
                failed.join("; ")
            ))));
        }
        Ok(Ok(receipts))
    }

    /// Follow one build to its end, recording its progress and log as it goes.
    async fn follow(
        &self,
        target: &Target,
        running: &RunningBuild,
    ) -> Result<Result<BuildReceipt, RpcError>, RpcError> {
        let service = &target.service;
        // GitHub can't build uploaded source: the walk skips it, and says so when the
        // Build Order has it.
        let github_skipped = matches!(
            target.build,
            Build::Upload {
                github_skipped: true,
                ..
            }
        );
        let mut log = if github_skipped {
            "GitHub can't build uploaded source: it builds on your Servers\n".to_owned()
        } else {
            String::new()
        };
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
        let (status, message, ended) = match running.finished().await {
            Ok(BuildOutcome::Built { receipt, reused }) => {
                let status = if reused {
                    BuildStatus::Reused
                } else {
                    BuildStatus::Built
                };
                (status, None, Ok(receipt))
            }
            Ok(BuildOutcome::Queued) => {
                let error = internal("No Server started the build");
                (
                    BuildStatus::Failed,
                    Some(crate::ui::row(&error)),
                    Err(error),
                )
            }
            Err(error) => (
                BuildStatus::Failed,
                Some(crate::ui::row(&error)),
                Err(error),
            ),
        };
        self.report(service, status, message, log).await?;
        let receipt = match ended {
            Ok(receipt) => receipt,
            Err(error) => return Ok(Err(error)),
        };
        let recorded = serde_json::to_value(&receipt).expect("a build receipt is JSON");
        self.record(RunEvidence::Built(BTreeMap::from([(
            service.clone(),
            recorded,
        )])))
        .await?;
        Ok(Ok(receipt))
    }

    /// Prepare the Deployment with its built Services' fresh receipts, so preparation
    /// reuses their images and only delivers them.
    async fn prepare(
        &self,
        session: &Session,
        claimed: Claimed,
        targets: Vec<Target>,
        receipts: BTreeMap<ServiceName, BuildReceipt>,
    ) -> Result<PreparedDeploy, RpcError> {
        let mut input = PreparationInput {
            deployment: claimed.input,
            build_receipts: receipts
                .into_iter()
                .map(|(service, receipt)| (service, vec![receipt]))
                .collect(),
            ..PreparationInput::default()
        };
        for target in &targets {
            target.input(&mut input);
        }
        // A Service GitHub built keeps its pin, so its receipt's fingerprint matches.
        for source in &claimed.sources {
            if let Some(commit) = &source.commit
                && input.build_receipts.contains_key(&source.service)
            {
                input
                    .source_commits
                    .entry(source.service.clone())
                    .or_insert_with(|| commit.to_string());
            }
        }
        // Delivering images can outlast the lease: keep renewing it.
        let running = session.prepare_with(input, claimed.intent.registry_auth)?;
        self.renewing(running.finished(), || running.abort()).await
    }

    /// Wait for `running`'s outcome, recording each Service once every one of its
    /// planned operations completed: a replaced Service reads Deployed at once, and
    /// stays so if this runner is lost before the outcome.
    async fn executing(
        &self,
        session: &Session,
        running: &super::RunningDeploy,
    ) -> Result<
        (
            DeployOutcome<ployz_core::ExecutionError>,
            Vec<ployz_store::ServerProgress>,
        ),
        RpcError,
    > {
        let mut confirmed = std::collections::BTreeSet::new();
        let mut tracker = RowTracker::default();
        let mut pending = BTreeMap::new();
        while let Some(event) = running.next().await {
            let ployz_core::DeployEvent::Progress { rows, .. } = event else {
                continue;
            };
            for mut row in tracker.changes(&rows) {
                let container = tracker.container(&row);
                if let (RowState::Failed { log, .. }, Some(container), Ok(client)) =
                    (&mut row.state, container, session.client())
                {
                    *log = crate::deploy::log_tail(&client, row.machine, container).await;
                }
                pending.insert((row.service.clone(), row.machine), row);
            }
            if !pending.is_empty()
                && self
                    .record(RunEvidence::Progress(pending.values().cloned().collect()))
                    .await
                    .is_ok()
            {
                pending.clear();
            }
            let mut done: BTreeMap<&ServiceName, bool> = BTreeMap::new();
            for row in &rows {
                if let Some(service) = &row.service_name {
                    *done.entry(service).or_insert(true) &=
                        matches!(row.status, ployz_core::OperationStatus::Completed);
                }
            }
            let new: Vec<ServiceName> = done
                .into_iter()
                .filter(|(service, done)| *done && !confirmed.contains(*service))
                .map(|(service, _)| service.clone())
                .collect();
            if new.is_empty() {
                continue;
            }
            // ponytail: a refused record leaves these to the outcome, which confirms them too.
            if self
                .record(RunEvidence::Confirmed(new.clone()))
                .await
                .is_ok()
            {
                confirmed.extend(new);
            }
        }
        let outcome = running.finished().await?;
        Ok((outcome, pending.into_values().collect()))
    }

    async fn connecting<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, RpcError>>,
    ) -> Result<Option<T>, RpcError> {
        tokio::pin!(work);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        loop {
            tokio::select! {
                biased;
                _ = poll.tick() => {
                    if self.status().await.ok() == Some(DeploymentStatus::Cancelling) {
                        return Ok(None);
                    }
                }
                done = &mut work => return done.map(Some),
            }
        }
    }

    /// Await `work` while renewing this runner's lease on the Deployment; a cancel
    /// calls `abort`.
    async fn renewing<T>(&self, work: impl std::future::Future<Output = T>, abort: impl Fn()) -> T {
        tokio::pin!(work);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        loop {
            tokio::select! {
                done = &mut work => return done,
                _ = poll.tick() => {
                    // ponytail: a failed read skips one check; the next tick reads again.
                    if self.status().await.ok() == Some(DeploymentStatus::Cancelling) {
                        abort();
                    }
                }
            }
        }
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

    async fn unbuilt(&self, unbuilt: Unbuilt) -> Result<DeploymentSummary, RpcError> {
        match unbuilt {
            Unbuilt::Failed(reason) => self.not_executed(reason.into()).await,
            Unbuilt::UploadNeeded(services) => {
                self.record(RunEvidence::UploadNeeded(services)).await
            }
        }
    }

    async fn not_executed(&self, failure: Failure) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::NotExecuted(failure)).await
    }

    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        super::store_call(&self.store, move |store| {
            store.record(&deployment, &runner, evidence)
        })
        .await
    }

    /// Renew this runner's lease on the Deployment and read its status back, which
    /// says whether it was cancelled.
    async fn status(&self) -> Result<DeploymentStatus, RpcError> {
        self.record(RunEvidence::Alive)
            .await
            .map(|summary| summary.status)
    }
}

async fn stop_builds(builds: &[(&Target, RunningBuild)], mut error: RpcError) -> RpcError {
    for (_, running) in builds {
        running.abort();
    }
    let settled = futures_util::future::join_all(builds.iter().map(|(target, running)| async {
        running
            .finished()
            .await
            .err()
            .map(|error| serde_json::json!({ "service": target.service, "error": error }))
    }))
    .await;
    let cleanup: Vec<_> = settled.into_iter().flatten().collect();
    if !cleanup.is_empty() {
        let mut details = match error.details {
            Value::Object(details) => details,
            Value::Null => serde_json::Map::new(),
            original @ (Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_)) => {
                serde_json::Map::from_iter([("original".into(), original)])
            }
        };
        details.insert("build_cleanup".into(), Value::Array(cleanup));
        error.details = Value::Object(details);
    }
    error
}

/// What to build on the Servers, and the images GitHub already built, by Service.
/// A build GitHub failed, or one whose walk leaves the Servers out, fails them all.
fn targets(
    claimed: &Claimed,
    sources: Sources,
) -> Result<(Vec<Target>, BTreeMap<ServiceName, BuildReceipt>), Unbuilt> {
    let mut targets = Vec::new();
    let mut built = BTreeMap::new();
    let mut failed = Vec::new();
    for source in &claimed.sources {
        match (
            source.status,
            receipts(claimed, &source.service).into_iter().next(),
        ) {
            (Some(BuildStatus::Built | BuildStatus::Reused), Some(receipt)) => {
                built.insert(source.service.clone(), receipt);
                continue;
            }
            (Some(BuildStatus::Failed), _) => {
                let why = source.message.as_deref().unwrap_or("its build failed");
                failed.push(format!("{}: {why}", source.service));
                continue;
            }
            _ => {}
        }
        if !source.builders.contains(&Builder::Servers) {
            let why = source
                .message
                .as_deref()
                .map_or_else(String::new, |message| format!("{message}. "));
            failed.push(format!(
                "{}: {why}No other Builder in your Build Order can take it",
                source.service
            ));
            continue;
        }
        let (Some(commit), Some(checkout)) =
            (&source.commit, sources.checkouts.get(&source.service))
        else {
            return Err(Unbuilt::Failed(format!(
                "{} has no source at a pinned commit",
                source.service
            )));
        };
        targets.push(Target {
            service: source.service.clone(),
            build: Build::Git {
                commit: commit.clone(),
                checkout: checkout.clone(),
                preferred_machine: source.preferred_machine,
            },
        });
    }
    if !failed.is_empty() {
        return Err(Unbuilt::Failed(format!(
            "Build failed. {}",
            failed.join("; ")
        )));
    }
    if claimed.uploads.is_empty() {
        return Ok((targets, built));
    }
    let Some(upload) = &claimed.deployment.upload else {
        return Err(Unbuilt::UploadNeeded(claimed.uploads.clone()));
    };
    let digest = upload.digest.clone();
    targets.extend(claimed.uploads.iter().map(|service| Target {
        service: service.clone(),
        build: Build::Upload {
            digest: digest.clone(),
            source: sources.upload.clone(),
            github_skipped: claimed.build_order.contains(&Builder::Github),
        },
    }));
    Ok((targets, built))
}

/// `value` as a one-entry map for `service`, or an empty one.
fn one<T>(service: &ServiceName, value: Option<T>) -> BTreeMap<ServiceName, T> {
    value
        .map(|value| BTreeMap::from([(service.clone(), value)]))
        .unwrap_or_default()
}

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
/// A build reads no Config, and Config files may hold secrets.
pub(super) fn only(input: &Value, service: &ServiceName) -> Value {
    let mut input = input.clone();
    if let Some(input) = input.as_object_mut() {
        input.remove("configs");
    }
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
pub(super) fn log_line(event: &Value) -> String {
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

pub(super) fn internal(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: message.to_owned(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
#[path = "store_runner_tests.rs"]
mod tests;
