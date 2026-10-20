//! Cloud's Deployment runner. One call claims a Deployment, builds its Git and
//! uploaded Services, prepares and confirms it on the Organization's Cluster and records each step's
//! evidence, so the Deploy Intent with its unsealed secrets, the session and the raw
//! SDK evidence never leave it: the caller gets only the Deployment's summary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::{
    ContainerId, DeployOperation, DeployOutcome, DeployWarning, ExecutionError, FailedOperation,
    MachineId, RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::tools::{LogText, SealedValues, Withheld};
use ployz_store::{
    BuildReport, BuildStatus, Builder, Claimed, CommitSha, ConfigStore, DeploymentId,
    DeploymentStatus, DeploymentSummary, EnvironmentId, Failure, RowState, RowTracker, RunEvidence,
    RunnerId,
};
use serde::Deserialize as _;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput, UploadDigest, needs_upload};
use super::{
    ImageCleanup, PreparedDeploy, RunningBuild, Session, connect_connections, connect_session,
};
use crate::connect::{Client, ConnectError, SystemConnector};
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
    /// An uploaded Service, from its upload when Cloud still holds it and extracted it.
    Upload {
        digest: UploadDigest,
        source: Option<PathBuf>,
        /// It builds from an earlier upload than the one its Deployment carries,
        /// which is the one Cloud extracts.
        earlier: bool,
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
        .connecting(connect_session(
            connections,
            Arc::new(SystemConnector::default()),
        ))
        .await
    {
        Ok(Some(session)) => session,
        Ok(None) => return run.not_executed("Cancelled before connecting".into()).await,
        // Users read it on the Deployment: plain words and one action first, then
        // what the connection said.
        Err(
            error @ ConnectError::AllFailed {
                server: Some(_), ..
            },
        ) => {
            return run
                .not_executed(Failure {
                    reason: error.to_string(),
                    cause: ployz_core::error_chain::causes(&error),
                })
                .await;
        }
        Err(error) => {
            let Failure { reason, mut cause } = Failure::from(&RpcError::from(error));
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

/// Every claim pair is kept, under its name: a name sealed after the claim
/// still names a value this Deploy shipped, and the names are read again at each tail.
/// A Container's Machine reports the rest, so a failed tail is scrubbed of both.
struct Scrub {
    environment: EnvironmentId,
    sealed_at_claim: BTreeSet<String>,
    claim: BTreeSet<(String, String)>,
}

impl Scrub {
    fn new(claimed: &Claimed) -> Self {
        Self {
            environment: claimed.deployment.environment_id.clone(),
            sealed_at_claim: claimed.sealed_names.clone(),
            claim: claimed
                .intent
                .target
                .iter()
                .flat_map(|spec| spec.container.environment.iter())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        }
    }

    /// The values under `names` from the claim and from `env`, what the Container's
    /// Machine reports: an image can supply a variable the claim never named.
    fn values(&self, names: &BTreeSet<String>, env: BTreeMap<String, String>) -> SealedValues {
        self.claim
            .iter()
            .filter(|(key, _)| names.contains(key))
            .cloned()
            .chain(env.into_iter().filter(|(key, _)| names.contains(key)))
            .collect()
    }
}

const SEALED_REREADS: usize = 2;

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
        let scrub = Scrub::new(&claimed);
        let earlier = earlier(&targets);
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
                    Some(services) => self.unbuilt(upload_needed(&earlier, services)).await,
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
            .renewing(self.executing(session, &running, &scrub), || {
                running.abort()
            })
            .await;
        match outcome {
            Ok((outcome, progress, mut warnings)) => {
                if self.lost_to_another_runner().await {
                    return self.interrupted(outcome, progress).await;
                }
                // The last chance: the store takes warnings only while the Deployment runs.
                self.record_warnings(&mut warnings).await;
                let removed = if matches!(outcome, DeployOutcome::Success { .. }) {
                    // Deleting is never interrupted: a half-deleted set stays accepted.
                    self.renewing(remove_volumes(session, deletes), || ()).await
                } else {
                    Vec::new()
                };
                if self.status().await.ok() != Some(DeploymentStatus::Cancelling) {
                    self.fill_ingress(session).await;
                }
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

    async fn fill_ingress(&self, session: &Session) {
        let filled = match session.client() {
            Ok(mut client) => {
                let stop = CancellationToken::new();
                self.renewing(crate::ingress::fill_slots(&mut client, &stop), || {
                    stop.cancel()
                })
                .await
                .map_err(|failure| crate::ui::row(&failure))
            }
            Err(error) => Err(crate::ui::row(&error)),
        };
        let evidence: Vec<_> = match filled {
            Ok(check) => (!check.repairs.is_empty())
                .then_some(RunEvidence::Ingress(check.repairs))
                .into_iter()
                .chain(
                    check
                        .unchecked
                        .into_iter()
                        .map(RunEvidence::IngressUnchecked),
                )
                .collect(),
            Err(cause) => vec![RunEvidence::IngressUnchecked(cause)],
        };
        for evidence in evidence {
            if let Err(error) = self.record(evidence).await {
                tracing::warn!(%error, "couldn't record what the Deploy did to ingress");
            }
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
            return Ok(Err(upload_needed(&earlier(targets), uploads)));
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
        loop {
            let next = if log.is_empty() {
                running.next().await
            } else {
                tokio::select! {
                    next = running.next() => next,
                    () = tokio::time::sleep_until(flushed + LOG_FLUSH) => {
                        // ponytail: a failed flush keeps the output for the next one.
                        if self
                            .report(service, BuildStatus::Building, None, log.clone())
                            .await
                            .is_ok()
                        {
                            log.clear();
                        }
                        flushed = tokio::time::Instant::now();
                        continue;
                    }
                }
            };
            let Some(event) = next else { break };
            log.push_str(&log_line(&event));
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
        scrub: &Scrub,
    ) -> Result<
        (
            DeployOutcome<ExecutionError>,
            Vec<ployz_store::ServerProgress>,
            Vec<DeployWarning>,
        ),
        RpcError,
    > {
        let mut confirmed = std::collections::BTreeSet::new();
        let mut tracker = RowTracker::default();
        let mut pending = BTreeMap::new();
        let mut warnings = Vec::new();
        while let Some(event) = running.next().await {
            let rows = match event {
                ployz_core::DeployEvent::Progress { rows, .. } => rows,
                ployz_core::DeployEvent::Warning { warning } => {
                    warnings.push(warning);
                    self.record_warnings(&mut warnings).await;
                    continue;
                }
                ployz_core::DeployEvent::Outcome { .. }
                | ployz_core::DeployEvent::ImagesPruned { .. } => continue,
            };
            self.record_warnings(&mut warnings).await;
            for mut row in tracker.changes(&rows) {
                let container = tracker.container(&row);
                if let (RowState::Failed { log: Some(log), .. }, Some(container), Ok(client)) =
                    (&mut row.state, container, session.client())
                {
                    *log = self
                        .failed_log(&client, scrub, row.machine, container)
                        .await;
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
        Ok((outcome, pending.into_values().collect(), warnings))
    }

    /// Record each held warning, keeping the ones the store refused for the next try.
    /// The store keeps a warning once, so a retry after a lost reply changes nothing.
    async fn record_warnings(&self, warnings: &mut Vec<DeployWarning>) {
        let mut unrecorded = Vec::new();
        for warning in warnings.drain(..) {
            if let Err(error) = self.record(RunEvidence::Warned(warning.clone())).await {
                tracing::warn!(%error, "couldn't record a Deploy warning; retrying later");
                unrecorded.push(warning);
            }
        }
        *warnings = unrecorded;
    }

    async fn connecting<T, E>(
        &self,
        work: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<Option<T>, E> {
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

    /// Await `work` while renewing this runner's lease on the Deployment; a cancel, or
    /// the store handing the environment to another runner, calls `abort`.
    async fn renewing<T>(&self, work: impl std::future::Future<Output = T>, abort: impl Fn()) -> T {
        tokio::pin!(work);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        loop {
            tokio::select! {
                done = &mut work => return done,
                _ = poll.tick() => {
                    // ponytail: a failed read skips one check; the next tick reads again.
                    if matches!(
                        self.status().await.ok(),
                        Some(DeploymentStatus::Cancelling | DeploymentStatus::Unknown)
                    ) {
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

    /// Records what ran before the store handed the Environment to another runner.
    async fn interrupted(
        &self,
        outcome: DeployOutcome<ExecutionError>,
        progress: Vec<ployz_store::ServerProgress>,
    ) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::Executed {
            progress,
            outcome: Box::new(interrupted(outcome)),
            removed: Vec::new(),
        })
        .await
    }

    async fn not_executed(&self, failure: Failure) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::NotExecuted(failure)).await
    }

    async fn failed_log(
        &self,
        client: &Client,
        scrub: &Scrub,
        machine: MachineId,
        container: ContainerId,
    ) -> LogText {
        let Some(env) = super::container_env(client, machine, container).await else {
            return LogText::Withheld {
                why: Withheld::NoSecretContext,
            };
        };
        let Ok(mut names) = self.sealed_names(&scrub.environment).await else {
            return LogText::Withheld {
                why: Withheld::NoSecretContext,
            };
        };
        names.extend(scrub.sealed_at_claim.iter().cloned());
        let Some(frames) = super::tail_frames(client, machine, container).await else {
            return LogText::Withheld {
                why: Withheld::Unanswered,
            };
        };
        match self.settled_sealed_names(&scrub.environment, names).await {
            Some(names) => super::tail_excerpt(frames, &scrub.values(&names, env)),
            None => LogText::Withheld {
                why: Withheld::NoSecretContext,
            },
        }
    }

    /// `names` grown by every name Sealed since they were read, once a read adds none.
    /// None when the set still grows after `SEALED_REREADS` reads, or the Store does not
    /// answer: the tail was read under names that may be stale, so it is withheld.
    async fn settled_sealed_names(
        &self,
        environment: &EnvironmentId,
        mut names: BTreeSet<String>,
    ) -> Option<BTreeSet<String>> {
        for _ in 0..=SEALED_REREADS {
            let fresh = self.sealed_names(environment).await.ok()?;
            if fresh.is_subset(&names) {
                return Some(names);
            }
            names.extend(fresh);
        }
        None
    }

    async fn sealed_names(
        &self,
        environment: &EnvironmentId,
    ) -> Result<BTreeSet<String>, RpcError> {
        let environment = environment.clone();
        super::store_call(&self.store, move |store| store.sealed_names(&environment)).await
    }

    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        super::store_call(&self.store, move |store| {
            store.record(&deployment, &runner, evidence)
        })
        .await
    }

    /// One bounded read after execution: the store may have handed the environment
    /// to another runner meanwhile, and then takes no outcome from this run.
    async fn lost_to_another_runner(&self) -> bool {
        self.status().await.ok() == Some(DeploymentStatus::Unknown)
    }

    /// Renew this runner's lease on the Deployment and read its status back, which
    /// says whether it was cancelled or handed to another runner.
    async fn status(&self) -> Result<DeploymentStatus, RpcError> {
        self.record(RunEvidence::Alive)
            .await
            .map(|summary| summary.status)
    }
}

/// `outcome` as the store's takeover left it: what completed stays completed, and
/// everything else, including the operation the abort stopped, was not executed.
fn interrupted(outcome: DeployOutcome<ExecutionError>) -> DeployOutcome<ExecutionError> {
    match outcome {
        DeployOutcome::Success {
            completed,
            services,
        } => DeployOutcome::Interrupted {
            completed,
            unexecuted: Vec::new(),
            services,
        },
        DeployOutcome::Failed {
            completed,
            failed,
            mut unexecuted,
            services,
        } => {
            let stopped = match failed {
                FailedOperation::Operation { operation, .. } => operation,
                FailedOperation::Replacement { operation, .. } => {
                    DeployOperation::ReplaceContainer(operation)
                }
            };
            unexecuted.insert(0, stopped);
            DeployOutcome::Interrupted {
                completed,
                unexecuted,
                services,
            }
        }
        DeployOutcome::Superseded {
            completed,
            unexecuted,
            services,
            ..
        }
        | DeployOutcome::Interrupted {
            completed,
            unexecuted,
            services,
        } => DeployOutcome::Interrupted {
            completed,
            unexecuted,
            services,
        },
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
    let missing: Vec<ServiceName> = claimed
        .uploads
        .iter()
        .filter(|(_, digest)| digest.is_none())
        .map(|(service, _)| service.clone())
        .collect();
    if !missing.is_empty() {
        return Err(Unbuilt::UploadNeeded(missing));
    }
    let carried = claimed
        .deployment
        .upload
        .as_ref()
        .map(|upload| &upload.digest);
    targets.extend(claimed.uploads.iter().filter_map(|(service, digest)| {
        let digest = digest.as_ref()?;
        let earlier = carried != Some(digest);
        Some(Target {
            service: service.clone(),
            build: Build::Upload {
                digest: digest.clone(),
                source: if earlier {
                    None
                } else {
                    sources.upload.clone()
                },
                earlier,
                github_skipped: claimed.build_order.contains(&Builder::Github),
            },
        })
    }));
    Ok((targets, built))
}

/// The uploaded Services among `targets` that build from an earlier upload than
/// their Deployment carries.
fn earlier(targets: &[Target]) -> BTreeSet<ServiceName> {
    targets
        .iter()
        .filter(|target| matches!(target.build, Build::Upload { earlier: true, .. }))
        .map(|target| target.service.clone())
        .collect()
}

/// Why `needed`, uploaded Services with neither their upload nor a usable image,
/// didn't build. One that builds from an earlier upload than its Deployment's may
/// still have it: Cloud extracts only the Deployment's, so it builds again on its
/// own. The rest need a new upload, the fix a user acts on first.
fn upload_needed(earlier: &BTreeSet<ServiceName>, needed: Vec<ServiceName>) -> Unbuilt {
    let (alone, rest): (Vec<_>, Vec<_>) = needed
        .into_iter()
        .partition(|service| earlier.contains(service));
    if !rest.is_empty() || alone.is_empty() {
        return Unbuilt::UploadNeeded(rest);
    }
    let names = alone
        .iter()
        .map(ServiceName::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let deploys = alone
        .iter()
        .map(|service| format!("ployz deploy {service}"))
        .collect::<Vec<_>>()
        .join(", then ");
    Unbuilt::Failed(format!(
        "{names} must build again from an earlier upload than this Deployment's, and a \
         Deployment builds from one upload. Deploy it on its own: {deploys}"
    ))
}

/// `value` as a one-entry map for `service`, or an empty one.
fn one<T>(service: &ServiceName, value: Option<T>) -> BTreeMap<ServiceName, T> {
    value
        .map(|value| BTreeMap::from([(service.clone(), value)]))
        .unwrap_or_default()
}

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
/// A build reads no Config, and Config files may hold secrets. It retires nothing.
pub(super) fn only(input: &Value, service: &ServiceName) -> Value {
    let mut input = input.clone();
    if let Some(input) = input.as_object_mut() {
        input.remove("configs");
        input.remove("removals");
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
    if text("/Build/Stage") == Some("Queued") {
        return "Waiting for a build slot\n".to_owned();
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
