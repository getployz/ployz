//! Cloud's Deployment runner. One call claims a Deployment, builds its Git and
//! uploaded Services, prepares and confirms it on the Organization's Cluster and records each step's
//! evidence, so the Deploy Intent with its unsealed secrets, the session and the raw
//! SDK evidence never leave it: the caller gets only the Deployment's summary.

use ployz_core::deploy_steps::{StepName, Steps, now_ms};
use ployz_core::error_chain;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ployz_agent_tools::{LogText, SealedValues, Withheld};
use ployz_core::{
    ContainerId, ContainerRuntimeObservation, DeployOperation, DeployOutcome, DeployWarning,
    ExecutionError, FailedOperation, HealthFailure, LastRun, MachineId, MachineName, Operator,
    RpcError, RpcErrorCode, RunnerOptions, ServiceName,
};
use ployz_decide::standing::{Latch, Over, Read, Standing, standing};
use ployz_store::{
    BuildReport, BuildStatus, Builder, Claimed, CommitSha, ConfigStore, DeploymentId,
    DeploymentStatus, DeploymentSummary, DeploymentWarning, EnvironmentId, Failure, RowState,
    RowTracker, RunEvidence, RunnerId,
};
use serde::Deserialize as _;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput, UploadDigest, needs_upload};
use super::run_support::{log_line, only};
use super::sealed::{Names, values_under};
use super::{ImageCleanup, PreparedDeploy, Running, RunningBuild, Session, Unprepared};
use crate::connect::{Client, SystemConnector};
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
    /// A token that reads the branch head of the repository an automatic deploy rule
    /// builds, which each Server reads before it creates under the rule. None reads a
    /// public repository.
    pub reader: Option<ployz_core::GithubToken>,
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

/// Run Deployment `deployment` as `runner`, driving it as `operator`, on one of
/// `connections`, and return its summary once its outcome is recorded. Its Git and uploaded Services
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
    operator: Operator,
    store: Arc<ConfigStore>,
    deployment: DeploymentId,
    runner: RunnerId,
    connections: Vec<Connection>,
    checkouts: Checkouts,
) -> Result<DeploymentSummary, RpcError> {
    run_deployment_expecting(
        operator,
        store,
        deployment,
        runner,
        connections,
        None,
        checkouts,
    )
    .await
}

/// [`run_deployment`] through an entry of `expect`'s Cluster only, when given.
///
/// # Errors
/// As [`run_deployment`].
pub async fn run_deployment_expecting(
    operator: Operator,
    store: Arc<ConfigStore>,
    deployment: DeploymentId,
    runner: RunnerId,
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
    checkouts: Checkouts,
) -> Result<DeploymentSummary, RpcError> {
    let run = Run::new(store, deployment, runner, RunLease::default(), &checkouts);
    Box::pin(run_deployment_in(
        run,
        operator,
        connections,
        expect,
        checkouts,
    ))
    .await
}

async fn run_deployment_in(
    run: Run,
    operator: Operator,
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
    checkouts: Checkouts,
) -> Result<DeploymentSummary, RpcError> {
    let claimed = run.claim(operator).await?;
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
        .connecting(super::dial(
            connections,
            Arc::new(SystemConnector::default()),
            expect,
        ))
        .await
    {
        Ok(Some(session)) => session,
        // A run the store no longer holds stops here too. The store refuses this record
        // from it, and that refusal is what the run returns.
        Ok(None) => return run.not_executed("Cancelled before connecting".into()).await,
        // Users read it on the Deployment: the connection's own sentence first, naming the
        // Server where it knows which and saying whether a daemon answered, then what the
        // connection said.
        Err(error) => {
            return run
                .not_executed(Failure {
                    reason: error.to_string(),
                    cause: ployz_core::error_chain::causes(&error),
                    same_again: false,
                })
                .await;
        }
    };
    let recorded = run.execute(&session, claimed, targets, built).await;
    session.close().await;
    recorded
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
                .filter_map(|(key, value)| Some((key.clone(), value.text()?.to_owned())))
                .collect(),
        }
    }

    /// The values under `names` from the claim and from `env`, what the Container's
    /// Machine reports: an image can supply a variable the claim never named.
    fn values(&self, names: &BTreeSet<String>, env: BTreeMap<String, String>) -> SealedValues {
        values_under(
            self.claim.iter().cloned().chain(env),
            &Names::Only(names.clone()),
        )
    }
}

const SEALED_REREADS: usize = 2;

struct Run {
    store: Arc<ConfigStore>,
    deployment: DeploymentId,
    runner: RunnerId,
    over: Latch,
    /// When each step ran, until the next record takes the changed ones.
    steps: std::sync::Mutex<Steps>,
    /// Handed to each Server with a rule's proof.
    reader: Option<ployz_core::GithubToken>,
    pending_live_at: std::sync::Mutex<Option<i64>>,
    lease: RunLease,
}

/// The last lease acknowledged by the Store, shared with a local runner's CLI.
#[derive(Clone, Default)]
pub(crate) struct RunLease(tokio::sync::watch::Sender<Option<Lease>>);

struct Lease {
    until: i64,
    deadline: tokio::time::Instant,
}

impl RunLease {
    pub(crate) async fn run_local(
        self,
        store: Arc<ConfigStore>,
        deployment: DeploymentId,
        runner: RunnerId,
        connections: Vec<Connection>,
        checkouts: Checkouts,
    ) -> Result<DeploymentSummary, RpcError> {
        let run = Run::new(store, deployment, runner, self, &checkouts);
        run_deployment_in(run, Operator::Cli, connections, None, checkouts).await
    }

    pub(crate) fn expired(&self) -> bool {
        self.deadline()
            .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
    }

    pub(crate) fn acknowledge(&self, until: i64) {
        self.0.send_if_modified(|lease| {
            if lease
                .as_ref()
                .is_some_and(|previous| previous.until == until)
            {
                return false;
            }
            let remaining = until.saturating_mul(1000).saturating_sub(now_ms());
            *lease = Some(Lease {
                until,
                deadline: tokio::time::Instant::now()
                    + Duration::from_millis(remaining.try_into().unwrap_or(0)),
            });
            true
        });
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.0.borrow().as_ref().map(|lease| lease.deadline)
    }

    pub(crate) async fn wait<T>(
        &self,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T, RpcError> {
        tokio::pin!(work);
        let mut lease = self.0.subscribe();
        loop {
            let deadline = lease
                .borrow_and_update()
                .as_ref()
                .map(|lease| lease.deadline);
            let expired = async {
                if let Some(deadline) = deadline {
                    if deadline > tokio::time::Instant::now() {
                        tokio::time::sleep_until(deadline).await;
                    }
                } else {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                biased;
                _ = lease.changed() => {}
                () = expired => {
                    return Err(RpcError::unavailable("The Deployment runner's lease expired"));
                }
                result = &mut work => return Ok(result),
            }
        }
    }
}

/// What `read`, the store's answer to one of this run's records, says about the Deployment.
fn read_of(read: &Result<DeploymentSummary, RpcError>) -> Read {
    match read {
        Ok(summary) => match summary.status {
            DeploymentStatus::Running => Read::Running,
            DeploymentStatus::Cancelling => Read::Cancelling,
            DeploymentStatus::Unknown => Read::Unknown,
            DeploymentStatus::Applied
            | DeploymentStatus::Failed
            | DeploymentStatus::Cancelled
            | DeploymentStatus::WaitingForCi
            | DeploymentStatus::Queued
            | DeploymentStatus::Superseded => Read::Ended,
        },
        // `record` refuses an ended Deployment, or another runner's, as a conflict.
        Err(refused) if refused.code == RpcErrorCode::Conflict => Read::Ended,
        Err(_) => Read::Unavailable,
    }
}

fn ingress_failure_message<'a>(
    client: &Client,
    machine_names: &'a BTreeMap<MachineId, MachineName>,
) -> impl FnOnce(crate::failure::Failure) -> String + 'a {
    let server = client.server();
    let known = client
        .connection()
        .machine_id()
        .copied()
        .or_else(|| client.entry_machine())
        .and_then(|id| machine_names.get_key_value(&id));
    move |failure| crate::ingress::unchecked(&failure, server, known)
}

impl Run {
    fn new(
        store: Arc<ConfigStore>,
        deployment: DeploymentId,
        runner: RunnerId,
        lease: RunLease,
        checkouts: &Checkouts,
    ) -> Self {
        Self {
            store,
            deployment,
            runner,
            over: Latch::new(),
            steps: std::sync::Mutex::default(),
            pending_live_at: std::sync::Mutex::default(),
            lease,
            reader: checkouts
                .as_ref()
                .ok()
                .and_then(|sources| sources.reader.clone()),
        }
    }

    /// Keep the verified builds the Servers issued during the run, so a later Deployment
    /// or drain of the same image needs no token. Failing to keep them costs only that.
    async fn keep_verified(&self, proofs: &crate::deploy::RuleProofs) {
        let kept = proofs.issued();
        if kept.is_empty() {
            return;
        }
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        let stored = self
            .store_call(move |store| store.keep_verified(&deployment, &runner, &kept))
            .await;
        if let Err(error) = stored {
            tracing::warn!(%error, "could not keep the verified builds the Servers issued");
        }
    }

    /// Claim the Deployment, with the Intent's options this runner plans it with,
    /// driving it as `operator`.
    async fn claim(&self, operator: Operator) -> Result<Claimed, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        let mut claimed = self
            .store_call(move |store| store.claim(&deployment, &runner))
            .await?;
        self.acknowledge_lease(claimed.lease_until);
        // The Store lowers the Intent without knowing who will run it, so the runner
        // sets who does. Each ended run labels its copies with its Deployment's id,
        // as this run does.
        let ended_runs = claimed
            .ended_runs
            .iter()
            .filter_map(|ended| ended.as_str().parse().ok())
            .collect();
        let recreate = std::mem::take(&mut claimed.intent.options.recreate);
        // Each Service the claim hands signed versions for; a rule's proofs join them
        // once its builds are verified.
        let signed = claimed
            .ran
            .iter()
            .filter(|(_, pinned)| !pinned.versions.is_empty())
            .map(|(service, _)| service.clone())
            .collect();
        claimed.intent.options = claimed.intent.options.as_runner(RunnerOptions {
            operator,
            ended_runs,
            recreate,
            signed,
        });
        Ok(claimed)
    }

    async fn execute(
        &self,
        session: &Session,
        mut claimed: Claimed,
        targets: Vec<Target>,
        built: BTreeMap<ServiceName, BuildReceipt>,
    ) -> Result<DeploymentSummary, RpcError> {
        let deletes = claimed.deletes.clone();
        let closes = claimed.closes.clone();
        let pins = pins(&claimed.ran);
        for spec in &mut claimed.intent.target {
            if let Some(image) = pins.get(&spec.name) {
                spec.container.image.clone_from(image);
            }
        }
        let versions = claimed
            .ran
            .values()
            .flat_map(|pinned| pinned.versions.iter().cloned())
            .collect();
        let scrub = Scrub::new(&claimed);
        let earlier = earlier(&targets);
        let mut rules = crate::deploy::RuleProofs::default();
        registry_proofs(&claimed, &mut rules);
        claimed
            .intent
            .options
            .signed
            .extend(rules.services().cloned());
        let prepared = if targets.is_empty() && built.is_empty() {
            self.steps().start(StepName::Planning, None, now_ms());
            self.renewing(session.plan(claimed.intent), || {
                session.inner.cancel.cancel()
            })
            .await
        } else {
            match self.build(session, &claimed, &targets).await? {
                Ok(mut receipts) => {
                    {
                        let mut steps = self.steps();
                        let at = steps.last_ended(StepName::Building).unwrap_or_else(now_ms);
                        steps.start(StepName::Planning, None, at);
                    }
                    receipts.extend(built);
                    rules = rule_proofs(session, &claimed, &receipts, self).await;
                    registry_proofs(&claimed, &mut rules);
                    claimed
                        .intent
                        .options
                        .signed
                        .extend(rules.services().cloned());
                    self.prepare(session, claimed, targets, (receipts, pins))
                        .await
                }
                Err(unbuilt) => return self.unbuilt(unbuilt).await,
            }
        };
        let prepared = match prepared {
            Ok(prepared) => {
                self.steps().end(StepName::Planning, None, now_ms());
                prepared
            }
            Err(unprepared) => return self.unprepared(&earlier, &deletes, unprepared).await,
        };
        let recorded = self
            .record(RunEvidence::Prepared(prepared.preview().clone()))
            .await;
        match standing(read_of(&recorded)) {
            Standing::Keep => {}
            Standing::Cancel => {
                return self
                    .not_executed("Cancelled before it executed".into())
                    .await;
            }
            Standing::Over(_) | Standing::Unread => return recorded,
        }
        if let Some(behind) = registry_rule_behind(session, &rules, prepared.preview()).await {
            return self.not_executed(behind.into()).await;
        }
        let log_id = self.deployment.as_str().parse().ok();
        let issued = rules.clone();
        let (acknowledge, switch_commit) = tokio::sync::oneshot::channel();
        let running = match prepared.confirm_approved(
            log_id,
            ImageCleanup::Auto,
            rules,
            versions,
            Some(switch_commit),
        ) {
            Ok(running) => running,
            Err(error) => return self.not_executed(Failure::from(&error)).await,
        };
        let outcome = self
            .renewing(
                self.executing(session, &running, &scrub, Some(acknowledge)),
                || running.abort(),
            )
            .await;
        self.keep_verified(&issued).await;
        let (outcome, progress, mut warnings) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => return self.runtime_failed(error).await,
        };
        let standing = self.standing().await;
        if matches!(standing, Standing::Cancel) {
            running.abort();
        }
        let recorded = match standing {
            Standing::Over(Over::Lost) => self.interrupted(outcome, progress).await,
            // The store refuses the outcome, and nothing more is deleted.
            Standing::Over(Over::Ended) => self.executed(outcome, progress, Vec::new()).await,
            Standing::Keep | Standing::Cancel | Standing::Unread => {
                // The last chance: the store takes most warnings only while the
                // Deployment runs.
                self.record_warnings(&mut warnings).await;
                let removed = if matches!(outcome, DeployOutcome::Success { .. })
                    && !running.cancel.is_cancelled()
                {
                    if !deletes.is_empty() || closes.is_some() {
                        // An old copy may still mount a Volume this deletes.
                        if let Err(error) = self.retired(&running, &mut warnings).await {
                            return self.retirement_failed(error, warnings).await;
                        }
                    }
                    // Deleting is never interrupted: a half-deleted set stays accepted.
                    let removal = async {
                        match &closes {
                            Some(namespace) => session.close_preview(namespace, deletes).await,
                            None => remove_volumes(session, deletes).await,
                        }
                    };
                    self.renewing(removal, || running.abort()).await
                } else {
                    Vec::new()
                };
                match self.standing().await {
                    Standing::Keep | Standing::Unread => {
                        self.fill_ingress(session, prepared.preview.machine_names())
                            .await;
                    }
                    Standing::Cancel | Standing::Over(_) => {}
                }
                self.executed(outcome, progress, removed).await
            }
        };
        self.finish_retirement(&running, recorded, warnings).await
    }

    async fn runtime_failed(&self, error: RpcError) -> Result<DeploymentSummary, RpcError> {
        let _ = self
            .retry_store(|| self.record(RunEvidence::RuntimeFailed))
            .await;
        Err(error)
    }

    async fn retirement_failed(
        &self,
        mut error: RpcError,
        mut warnings: Vec<DeployWarning>,
    ) -> Result<DeploymentSummary, RpcError> {
        error.cause.extend(warnings.iter().map(ToString::to_string));
        self.record_warnings(&mut warnings).await;
        self.runtime_failed(error).await
    }

    fn pending_live_at(&self) -> std::sync::MutexGuard<'_, Option<i64>> {
        self.pending_live_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn record_switch(&self, live_at: Option<i64>) -> Result<DeploymentSummary, RpcError> {
        if let Some(at) = live_at {
            self.pending_live_at().get_or_insert(at);
        }
        self.record(RunEvidence::Finishing { live_at }).await
    }

    async fn commit_switch(
        &self,
        at: i64,
        cancelled: &CancellationToken,
    ) -> Result<(crate::deploy::SwitchDecision, bool), RpcError> {
        use crate::deploy::SwitchDecision;

        self.until_lease(async {
            let mut poll = tokio::time::interval(CANCEL_POLL);
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    biased;
                    () = cancelled.cancelled() => {}
                    _ = poll.tick() => {}
                }
                // A cancellation can arrive while the blocking write is committing.
                // Always resolve that write before telling the executor whether to undo.
                match self.record_switch(Some(at)).await {
                    Ok(summary) if summary.live_at.is_some() && summary.status.claimed() => {
                        return Ok((
                            SwitchDecision::Committed,
                            summary.status == DeploymentStatus::Cancelling
                                || summary.cleanup == Some(ployz_store::CleanupStatus::Unconfirmed),
                        ));
                    }
                    Err(error)
                        if error
                            .details
                            .get("switch_cancelled")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true) =>
                    {
                        *self.pending_live_at() = None;
                        return Ok((SwitchDecision::Refused, true));
                    }
                    Err(error)
                        if !cancelled.is_cancelled()
                            && !matches!(
                                error.code,
                                RpcErrorCode::Conflict | RpcErrorCode::NotFound
                            )
                            && self.over.latched() != Some(Over::Lost) =>
                    {
                        tracing::warn!(%error, "couldn't record Deployment evidence");
                    }
                    Err(error) => return Err(error),
                    Ok(_) => {
                        return Err(RpcError::unavailable(
                            "The Deployment no longer holds the traffic switch",
                        ));
                    }
                }
            }
        })
        .await?
    }

    async fn finish_retirement(
        &self,
        running: &super::RunningDeploy,
        recorded: Result<DeploymentSummary, RpcError>,
        mut warnings: Vec<DeployWarning>,
    ) -> Result<DeploymentSummary, RpcError> {
        if let Err(error) = self.retired(running, &mut warnings).await {
            return self.retirement_failed(error, warnings).await;
        }
        let finished = self
            .retry_store(|| {
                self.record(RunEvidence::Finished {
                    warnings: warnings.clone(),
                })
            })
            .await;
        if self.pending_live_at().is_some() {
            finished?;
            return Err(RpcError::internal(
                "Couldn't record the Deployment's traffic switch",
            ));
        }
        let recorded = recorded?;
        match finished {
            Ok(summary) => Ok(summary),
            Err(error) => {
                tracing::warn!(%error, "couldn't confirm the end of cleanup");
                let mut summary = recorded;
                summary.cleanup = Some(ployz_store::CleanupStatus::Unconfirmed);
                for warning in warnings {
                    let warning = DeploymentWarning::Deploy { warning };
                    if !summary.warnings.contains(&warning) {
                        summary.warnings.push(warning);
                    }
                }
                Ok(summary)
            }
        }
    }

    /// Wait out what the Deploy does after its Outcome, retiring the old copies and
    /// cleaning up images. Keep the warnings for the Finished record. After a
    /// successful join, a second wait returns at once.
    async fn retired(
        &self,
        running: &super::RunningDeploy,
        warnings: &mut Vec<DeployWarning>,
    ) -> Result<(), RpcError> {
        let mut renewed = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let mut poll =
            tokio::time::interval_at(tokio::time::Instant::now() + CANCEL_POLL, CANCEL_POLL);
        loop {
            let event = tokio::select! {
                biased;
                event = running.next() => event,
                _ = poll.tick(), if !running.cancel.is_cancelled() => {
                    let deployment = self.deployment.clone();
                    if self.store_call(move |store| store.cleanup_unconfirmed(&deployment)).await == Ok(true) {
                        running.abort();
                    }
                    continue;
                }
                _ = renewed.tick() => {
                    let _ = self.record(RunEvidence::Finishing { live_at: None }).await;
                    continue;
                }
            };
            let Some(event) = event else { break };
            if let ployz_core::DeployEvent::Retired { service } = &event {
                self.steps()
                    .drained(StepName::StoppingOld, Some(service), now_ms());
                self.record_steps().await;
            }
            if let ployz_core::DeployEvent::Warning { warning } = event {
                if let DeployWarning::LeftRunning { service, .. } = &warning {
                    self.steps()
                        .hold(StepName::StoppingOld, Some(&service.name));
                }
                warnings.push(warning);
            }
        }
        let _ = running.join_if_pending().await?;
        Ok(())
    }

    /// Record the steps that changed since the last record, after the Deployment ended
    /// too. A step write that fails only warns.
    async fn record_steps(&self) {
        let steps = self.steps().take_changed();
        if steps.is_empty() {
            return;
        }
        let deployment = self.deployment.clone();
        let recorded = self
            .store_call(move |store| store.record_steps(&deployment, &steps))
            .await;
        if let Err(error) = recorded {
            tracing::warn!(%error, "couldn't record the Deployment's steps");
        }
    }

    async fn fill_ingress(
        &self,
        session: &Session,
        machine_names: &BTreeMap<MachineId, ployz_core::MachineName>,
    ) {
        let filled = match session.client() {
            Ok(mut client) => {
                let message = ingress_failure_message(&client, machine_names);
                let stop = CancellationToken::new();
                self.renewing(crate::ingress::fill_slots(&mut client, &stop), || {
                    stop.cancel()
                })
                .await
                .map_err(message)
            }
            Err(error) => Err(error_chain::row(&error)),
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
                tracing::warn!(
                    code = error.code.as_str(),
                    error = error.message.as_str(),
                    "couldn't record what the Deploy did to ingress"
                );
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
        let mut starts = Vec::new();
        for (index, target) in targets.iter().enumerate() {
            let service = &target.service;
            let mut input = PreparationInput {
                deployment: only(&claimed.input, service)?,
                build_receipts: one(service, Some(receipts(claimed, service))),
                build_index: index,
                preferred_machine: match &target.build {
                    Build::Git {
                        preferred_machine, ..
                    } => *preferred_machine,
                    Build::Upload { .. } => None,
                },
                free: claimed.free.clone(),
                ..PreparationInput::default()
            };
            target.input(&mut input);
            starts.push(now_ms());
            let running = match session.build(input, None) {
                Ok(running) => running,
                Err(error) => return Err(stop_builds(&builds, error).await),
            };
            builds.push((target, running));
            if let Err(error) = self
                .report(service, BuildStatus::Building, None, String::new(), None)
                .await
            {
                return Err(stop_builds(&builds, error).await);
            }
        }
        let all = futures_util::future::join_all(
            builds
                .iter()
                .zip(&starts)
                .map(|((target, running), started)| self.follow(target, running, *started)),
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
                    None => {
                        failed.push(format!("{}: {}", target.service, error_chain::row(&error)))
                    }
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
        started: i64,
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
        let mut server = None;
        let mut queued = false;
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
                            .report(service, BuildStatus::Building, None, log.clone(), server.clone())
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
            build_stage(&mut self.steps(), service, (started, &mut queued), &event);
            if let Some(name) = event
                .pointer("/Selected/machine/name")
                .and_then(Value::as_str)
            {
                server = Some(name.to_owned());
            }
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
                let error = RpcError::internal("No Server started the build");
                (
                    BuildStatus::Failed,
                    Some(error_chain::row(&error)),
                    Err(error),
                )
            }
            Err(error) => (
                BuildStatus::Failed,
                Some(error_chain::row(&error)),
                Err(error),
            ),
        };
        build_ended(&mut self.steps(), service, started, ended.is_ok());
        self.report(service, status, message, log, server).await?;
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
    /// reuses their images and only delivers them. It reads no checkout: only a receipt
    /// whose image no longer runs everywhere its Service may be placed needs one, to
    /// rebuild from, and then preparation runs again with them.
    async fn prepare(
        &self,
        session: &Session,
        claimed: Claimed,
        targets: Vec<Target>,
        (receipts, pins): (
            BTreeMap<ServiceName, BuildReceipt>,
            BTreeMap<ServiceName, String>,
        ),
    ) -> Result<PreparedDeploy, Unprepared> {
        let input = |checkouts: bool| {
            let mut input = PreparationInput {
                deployment: claimed.input.clone(),
                build_receipts: receipts
                    .iter()
                    .map(|(service, receipt)| (service.clone(), vec![receipt.clone()]))
                    .collect(),
                pins: pins.clone(),
                free: claimed.free.clone(),
                ..PreparationInput::default()
            };
            for target in &targets {
                target.input(&mut input);
            }
            if !checkouts {
                input.sources.clear();
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
            input
        };
        match self.prepare_from(session, &claimed, input(false)).await {
            Err(unprepared) if needs_upload(&unprepared.error).is_some() => {
                self.prepare_from(session, &claimed, input(true)).await
            }
            prepared => prepared,
        }
    }

    async fn prepare_from(
        &self,
        session: &Session,
        claimed: &Claimed,
        input: PreparationInput,
    ) -> Result<PreparedDeploy, Unprepared> {
        // Delivering images can outlast the lease: keep renewing it.
        let running: Running<PreparedDeploy, Unprepared> = session.prepare_with(
            input,
            claimed.intent.registry_logins.clone(),
            claimed.intent.options.runner(),
        )?;
        let delivering = async {
            while let Some(event) = running.next().await {
                delivery(&mut self.steps(), &event);
            }
        };
        let (prepared, ()) = self
            .renewing(
                futures_util::future::join(running.finished(), delivering),
                || running.abort(),
            )
            .await;
        prepared
    }

    /// Records a Deploy its preparation refused: the upload it needs, or nothing
    /// executed, kept with whether deploying the same thing again fails the same way.
    async fn unprepared(
        &self,
        earlier: &BTreeSet<ServiceName>,
        deletes: &[ployz_core::DockerVolumeId],
        unprepared: Unprepared,
    ) -> Result<DeploymentSummary, RpcError> {
        match needs_upload(&unprepared.error) {
            Some(services) => self.unbuilt(upload_needed(earlier, services)).await,
            None => {
                self.not_executed(Failure {
                    same_again: unprepared.same_again,
                    ..refused_before_deletes(&unprepared, deletes)
                })
                .await
            }
        }
    }

    /// Wait for `running`'s outcome, recording each Service once every one of its
    /// planned operations completed: a replaced Service reads Deployed at once, and
    /// stays so if this runner is lost before the outcome. It returns at the Outcome,
    /// before the old copies retire; [`Self::retired`] waits for those.
    async fn executing(
        &self,
        session: &Session,
        running: &super::RunningDeploy,
        scrub: &Scrub,
        mut acknowledge: Option<tokio::sync::oneshot::Sender<crate::deploy::SwitchDecision>>,
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
        let mut stopped = BTreeMap::new();
        let mut ended = None;
        while let Some(event) = running.next().await {
            let mut rows = match event {
                ployz_core::DeployEvent::Progress { rows, live_at, .. } => {
                    if let Some(at) = live_at {
                        let (decision, cancel) = self
                            .commit_switch(at, &running.cancel)
                            .await
                            .inspect_err(|_| {
                                if self.over.latched() == Some(Over::Lost) {
                                    running.abort();
                                }
                            })?;
                        if cancel {
                            running.abort();
                        }
                        if let Some(acknowledge) = acknowledge.take() {
                            let _ = acknowledge.send(decision);
                        }
                    }
                    rows
                }
                ployz_core::DeployEvent::Warning { warning } => {
                    if let DeployWarning::LeftRunning { service, .. } = &warning {
                        self.steps()
                            .hold(StepName::StoppingOld, Some(&service.name));
                    }
                    warnings.push(warning);
                    self.record_warnings(&mut warnings).await;
                    continue;
                }
                // Traffic moved: the old copies retiring after it hold up nothing.
                ployz_core::DeployEvent::Outcome { outcome, .. } => {
                    if let Err(error) = self.record(RunEvidence::Finishing { live_at: None }).await
                    {
                        tracing::warn!(%error, "couldn't record the start of cleanup");
                    }
                    ended = Some(outcome);
                    break;
                }
                ployz_core::DeployEvent::ImagesPruned { .. }
                | ployz_core::DeployEvent::Retired { .. } => continue,
            };
            self.record_warnings(&mut warnings).await;
            if let Ok(client) = session.client() {
                for row in &mut rows {
                    if let ployz_core::OperationStatus::Failed { error } = &mut row.status {
                        self.fill_last_run(&client, scrub, row.machine_id, error, &mut stopped)
                            .await;
                    }
                }
            }
            let changes = tracker.changes(&rows);
            {
                let mut steps = self.steps();
                steps.rows(&changes, now_ms());
                // Its old copies retire after the Outcome, and the step waits for them.
                for row in &rows {
                    if let (
                        Some(service),
                        ployz_core::OperationStatus::Running {
                            phase: ployz_core::OperationPhase::RetiringOld { .. },
                        },
                    ) = (&row.service_name, &row.status)
                    {
                        steps.drain(StepName::StoppingOld, Some(service));
                    }
                }
            }
            for mut row in changes {
                let container = tracker.container(&row);
                if let (RowState::Failed { log: Some(log), .. }, Some(container), Ok(client)) =
                    (&mut row.state, container, session.client())
                {
                    *log = match stopped.get(&container) {
                        Some((read, _)) => read.clone(),
                        None => {
                            self.failed_log(&client, scrub, row.machine, container)
                                .await
                                .0
                        }
                    };
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
        let mut outcome = match ended {
            Some(outcome) => outcome,
            None => running.finished().await?,
        };
        if let (DeployOutcome::Failed { failed, .. }, Ok(client)) = (&mut outcome, session.client())
        {
            let (machine, error) = match failed {
                FailedOperation::Operation { operation, error } => (operation.machine_id(), error),
                FailedOperation::Replacement {
                    operation, error, ..
                } => (operation.machine_id, error),
            };
            self.fill_last_run(&client, scrub, machine, error, &mut stopped)
                .await;
        }
        Ok((outcome, pending.into_values().collect(), warnings))
    }

    /// Record each held warning, keeping the ones the store refused for the next try.
    /// The store keeps a warning once, so a retry after a lost reply changes nothing.
    async fn record_warnings(&self, warnings: &mut Vec<DeployWarning>) {
        let mut unrecorded = Vec::new();
        for warning in warnings.drain(..) {
            match self.record(RunEvidence::Warned(warning.clone())).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        code = error.code.as_str(),
                        error = error.message.as_str(),
                        "couldn't record a Deploy warning; retrying later"
                    );
                    unrecorded.push(warning);
                }
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
                _ = self.until_lease(std::future::pending::<()>()) => return Ok(None),
                _ = poll.tick() => match self.standing().await {
                    Standing::Keep | Standing::Unread => {}
                    Standing::Cancel | Standing::Over(_) => return Ok(None),
                },
                done = &mut work => return done.map(Some),
            }
        }
    }

    /// Await `work` while renewing this runner's lease on the Deployment; a cancel, or
    /// the store no longer holding the run, calls `abort`.
    async fn renewing<T>(&self, work: impl std::future::Future<Output = T>, abort: impl Fn()) -> T {
        tokio::pin!(work);
        let mut poll = tokio::time::interval(CANCEL_POLL);
        let mut expired = false;
        loop {
            tokio::select! {
                biased;
                _ = self.until_lease(std::future::pending::<()>()), if !expired => {
                    expired = true;
                    abort();
                }
                done = &mut work => return done,
                _ = poll.tick(), if !expired => match self.standing().await {
                    Standing::Keep | Standing::Unread => {}
                    Standing::Cancel | Standing::Over(_) => abort(),
                },
            }
        }
    }

    async fn report(
        &self,
        service: &ServiceName,
        status: BuildStatus,
        message: Option<String>,
        log: String,
        server: Option<String>,
    ) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::Build(BuildReport {
            service: service.clone(),
            status,
            message,
            log,
            server,
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

    /// Records what ran before the store set the Deployment Unknown: Interrupted is the
    /// one outcome it takes from the run then.
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

    /// Record what executing did. The store refuses any outcome but Interrupted from a
    /// run it set Unknown, so a refused one is sent again as Interrupted. Its other
    /// refusals turn on what it holds, not on the outcome sent, so the resend meets them
    /// again. The one exception changes nothing: a store that already holds this same
    /// Interrupted outcome accepts the resend as a replay. Not when the run tried to
    /// delete a Volume: the store reads an Interrupted outcome's removed Volumes as never
    /// attempted, and the refusal claims nothing.
    async fn executed(
        &self,
        outcome: DeployOutcome<ExecutionError>,
        progress: Vec<ployz_store::ServerProgress>,
        removed: Vec<ployz_core::VolumeRemoval>,
    ) -> Result<DeploymentSummary, RpcError> {
        let deleted = removed
            .iter()
            .any(|removal| !matches!(removal.outcome, ployz_core::VolumeRemovalOutcome::Omitted));
        let cancelled = matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: ployz_core::FailedOperation::Operation { error, .. }
                    | ployz_core::FailedOperation::Replacement { error, .. },
                ..
            } if error.cancelled()
        );
        let evidence = RunEvidence::Executed {
            progress: progress.clone(),
            outcome: Box::new(outcome.clone()),
            removed,
        };
        let mut recorded = self.record(evidence.clone()).await;
        if recorded.is_err() && cancelled && self.pending_live_at().take().is_some() {
            recorded = self.record(evidence).await;
        }
        match recorded {
            Err(refused) if refused.code == RpcErrorCode::Conflict && !deleted => {
                self.interrupted(outcome, progress).await.or(Err(refused))
            }
            recorded => recorded,
        }
    }

    async fn not_executed(&self, failure: Failure) -> Result<DeploymentSummary, RpcError> {
        self.record(RunEvidence::NotExecuted(failure)).await
    }

    /// Gives a Container's stop its last run from Docker, read once per Container
    /// into `stopped` with its row's log, so the row and the outcome say the same.
    async fn fill_last_run(
        &self,
        client: &Client,
        scrub: &Scrub,
        machine: MachineId,
        error: &mut ExecutionError,
        stopped: &mut BTreeMap<ContainerId, (LogText, LastRun)>,
    ) {
        let ExecutionError::Health {
            container_id,
            failure: HealthFailure::Runtime { .. },
            last_run,
            ..
        } = error
        else {
            return;
        };
        let (_, run) = match stopped.entry(*container_id) {
            std::collections::btree_map::Entry::Occupied(read) => read.into_mut(),
            std::collections::btree_map::Entry::Vacant(unread) => {
                unread.insert(self.failed_log(client, scrub, machine, *container_id).await)
            }
        };
        let seen = last_run.as_ref().and_then(|seen| seen.exit_code);
        *last_run = Some(LastRun {
            exit_code: seen.or(run.exit_code),
            last_line: run.last_line.clone(),
        });
    }

    /// The failed Container's scrubbed tail, and the run Docker says it ended: its exit code
    /// from inspect, read before the Container is removed. The code the Machine saw when it
    /// judged a restarting Container, which Docker stops showing once it runs again, wins in
    /// `fill_last_run`.
    async fn failed_log(
        &self,
        client: &Client,
        scrub: &Scrub,
        machine: MachineId,
        container: ContainerId,
    ) -> (LogText, LastRun) {
        let withheld = |why| (LogText::Withheld { why }, LastRun::default());
        let Some(details) = super::inspect_container(client, machine, container).await else {
            return withheld(Withheld::NoSecretContext);
        };
        let exit_code = match details.container.runtime {
            ContainerRuntimeObservation::Exited { code } => Some(code),
            ContainerRuntimeObservation::Restarting
            | ContainerRuntimeObservation::Running { .. } => details.last_exit_code,
            ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Removing
            | ContainerRuntimeObservation::Dead
            | ContainerRuntimeObservation::Unknown { .. } => None,
        };
        let Some(env) = details.environment else {
            return withheld(Withheld::NoSecretContext);
        };
        let Ok(mut names) = self.sealed_names(&scrub.environment).await else {
            return withheld(Withheld::NoSecretContext);
        };
        names.extend(scrub.sealed_at_claim.iter().cloned());
        let Some(tail) = super::tail_frames(client, machine, container).await else {
            return (
                LogText::Withheld {
                    why: Withheld::Unanswered,
                },
                LastRun {
                    exit_code,
                    last_line: None,
                },
            );
        };
        match self.settled_sealed_names(&scrub.environment, names).await {
            Some(names) => {
                let (text, last_line) = tail.scrubbed(&scrub.values(&names, env));
                (
                    text,
                    LastRun {
                        exit_code,
                        last_line,
                    },
                )
            }
            None => (
                LogText::Withheld {
                    why: Withheld::NoSecretContext,
                },
                LastRun {
                    exit_code,
                    last_line: None,
                },
            ),
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
        self.store_call(move |store| store.sealed_names(&environment))
            .await
    }

    fn steps(&self) -> std::sync::MutexGuard<'_, Steps> {
        self.steps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn acknowledge_lease(&self, until: i64) {
        self.lease.acknowledge(until);
    }

    #[cfg(test)]
    fn lease_deadline(&self) -> Option<tokio::time::Instant> {
        self.lease.deadline()
    }

    async fn until_lease<T>(
        &self,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T, RpcError> {
        self.lease.wait(work).await.inspect_err(|_| {
            self.over.settle(Read::Unknown);
        })
    }

    async fn store_call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&ConfigStore) -> Result<T, RpcError> + Send + 'static,
    ) -> Result<T, RpcError> {
        self.until_lease(super::store_call(&self.store, work))
            .await?
    }

    async fn retry_store<T, F>(&self, mut write: impl FnMut() -> F) -> Result<T, RpcError>
    where
        F: std::future::Future<Output = Result<T, RpcError>>,
    {
        self.until_lease(async {
            let mut poll = tokio::time::interval(CANCEL_POLL);
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                poll.tick().await;
                match write().await {
                    Err(error)
                        if !matches!(
                            error.code,
                            RpcErrorCode::Conflict | RpcErrorCode::NotFound
                        ) =>
                    {
                        tracing::warn!(%error, "couldn't record Deployment evidence");
                    }
                    recorded => return recorded,
                }
            }
        })
        .await?
    }

    /// Record `evidence`, after the steps that changed since the last record.
    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        let steps = if matches!(evidence, RunEvidence::Finishing { live_at: Some(_) }) {
            Vec::new()
        } else {
            self.steps().take_changed()
        };
        let pending_live_at = if matches!(evidence, RunEvidence::Alive) {
            None
        } else {
            *self.pending_live_at()
        };
        let recorded = self
            .store_call(move |store| {
                if !steps.is_empty()
                    && let Err(error) = store.record_steps(&deployment, &steps)
                {
                    tracing::warn!(%error, "couldn't record the Deployment's steps");
                }
                store.record_with_live_at(&deployment, &runner, evidence, pending_live_at)
            })
            .await;
        recorded.map(|(summary, lease)| {
            if let Some(until) = lease {
                self.acknowledge_lease(until);
            }
            if summary.live_at.is_some() {
                *self.pending_live_at() = None;
            }
            summary
        })
    }

    /// Renew this runner's lease on the Deployment and read what the run may do. Once a
    /// read found the store no longer holding the run, it reads nothing more and says
    /// the same: a read that fails after a takeover still leaves the run lost.
    async fn standing(&self) -> Standing {
        if let Some(over) = self.over.latched() {
            return Standing::Over(over);
        }
        let recorded = self.record(RunEvidence::Alive).await;
        self.over
            .latched()
            .map_or_else(|| self.over.settle(read_of(&recorded)), Standing::Over)
    }
}

/// Move one build's steps on with `event`, one of its progress frames. Its upload
/// starts with the build, or once a Build Machine admits it after queueing.
fn build_stage(
    steps: &mut Steps,
    service: &ServiceName,
    (started, queued): (i64, &mut bool),
    event: &Value,
) {
    let at = now_ms();
    let service = Some(service);
    match event.pointer("/Build/Stage").and_then(Value::as_str) {
        Some("Queued") => *queued = true,
        Some("Upload") => steps.start(
            StepName::Uploading,
            service,
            if *queued { at } else { started },
        ),
        Some("Preparation") => {
            steps.end(StepName::Uploading, service, at);
            steps.start(StepName::StartingBuilder, service, at);
        }
        Some("Building") => {
            steps.end(StepName::Uploading, service, at);
            steps.end(StepName::StartingBuilder, service, at);
            steps.start(StepName::Building, service, at);
        }
        _ => {}
    }
}

/// End one build's steps. A build that reused an image builds from its start to its
/// end; a failed one stays open in the step it failed in.
fn build_ended(steps: &mut Steps, service: &ServiceName, started: i64, built: bool) {
    let at = now_ms();
    let service = Some(service);
    let stages = [
        StepName::Uploading,
        StepName::StartingBuilder,
        StepName::Building,
    ];
    if !stages.iter().any(|stage| steps.started(*stage, service)) {
        steps.start(StepName::Building, service, started);
    }
    if built {
        for stage in stages {
            steps.end(stage, service, at);
        }
    }
}

/// Move a Service's image delivery on with `event`, one of preparation's progress
/// frames: from sending it until its Machines have it. Planning ends at the first send.
fn delivery(steps: &mut Steps, event: &Value) {
    let at = now_ms();
    let service = |pointer| {
        event
            .pointer(pointer)
            .and_then(Value::as_str)
            .and_then(|name| name.parse::<ServiceName>().ok())
    };
    if let Some(service) = service("/Sending/service") {
        steps.end(StepName::Planning, None, at);
        steps.start(StepName::DeliveringImage, Some(&service), at);
    } else if let Some(service) = service("/Delivered/service") {
        steps.end(StepName::DeliveringImage, Some(&service), at);
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
        DeployOutcome::Interrupted {
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
            let why = source.message.as_deref().unwrap_or_default();
            failed.push(format!(
                "{}: {}",
                source.service,
                error_chain::then_sentence(why, "No other Builder in your Build Order can take it")
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

/// The proof each Service built under `claimed`'s automatic deploy rule creates with:
/// the rule, and the verified build a Server issued for the image it runs, with the claim
/// that build covers; else the image token and the claim its audience names, rebuilt from
/// the rule, the pinned commit, the image the Machine received, the Cluster's keys, and
/// the generation and authority the token was built under. A Service without either
/// gets none, and a Server refuses it.
async fn rule_proofs(
    session: &Session,
    claimed: &Claimed,
    receipts: &BTreeMap<ServiceName, BuildReceipt>,
    run: &Run,
) -> crate::deploy::RuleProofs {
    use ployz_core::authority::{
        BuildClaim, BuildStage, BuiltImageDigest, CommitSha, KeptRule, PolicyDigest, RuleEvidence,
        RuleProof,
    };
    let mut proofs = crate::deploy::RuleProofs::default();
    let Some(signed) = &claimed.rule else {
        return proofs;
    };
    for (service, kept) in &claimed.verified {
        let runs = receipts
            .get(service)
            .and_then(|receipt| BuiltImageDigest::parse(&receipt.image.reference).ok());
        if matches!(kept.claim.stage, BuildStage::Built { image, .. } if Some(image) == runs) {
            proofs.insert(
                service.clone(),
                KeptRule::built(RuleProof {
                    rule: signed.clone(),
                    evidence: RuleEvidence::Verified(kept.verified),
                    claim: Some(kept.claim.clone()),
                    reader: None,
                }),
            );
        }
    }
    // An image this Deployment reuses without its own evidence carries what a Server
    // issued for it under the rule's current generation, as a drain's copy does, and
    // needs no token or branch head. With none, its build's token below proves it.
    for (service, receipt) in receipts {
        let Ok(image) = BuiltImageDigest::parse(&receipt.image.reference) else {
            continue;
        };
        if proofs.contains(service) {
            continue;
        }
        let (deployment, namespace, name) = (
            run.deployment.clone(),
            claimed.intent.namespace.clone(),
            service.clone(),
        );
        match run
            .store_call(move |store| store.verified_proof(&deployment, (&namespace, &name), image))
            .await
        {
            Ok(Some(proof)) => proofs.insert(service.clone(), KeptRule::built(proof)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, %service, "could not read a reused image's verified build")
            }
        }
    }
    if claimed.image_tokens.is_empty() {
        return proofs;
    }
    let Ok(rule) = signed.open() else {
        return proofs;
    };
    let contract = match session.about().await {
        Ok(contract) => contract,
        Err(error) => {
            tracing::warn!(%error, "could not read the Cluster's keys for a rule's proof");
            return proofs;
        }
    };
    let (Some(ployz_core::Sealing::Held { public_key }), Some(attest)) =
        (contract.sealing, contract.attest)
    else {
        return proofs;
    };
    for (service, token) in &claimed.image_tokens {
        if proofs.contains(service) {
            continue;
        }
        let approved = rule.build_policy(service);
        let receipt = receipts.get(service);
        let commit = claimed
            .sources
            .iter()
            .find(|source| source.service == *service)
            .and_then(|source| source.commit.as_ref())
            .and_then(|commit| CommitSha::parse(commit.as_str()).ok());
        let (Some(policy), Some(receipt), Some(commit)) = (approved, receipt, commit) else {
            continue;
        };
        let (Ok(image), Ok(policy)) = (
            BuiltImageDigest::parse(&receipt.image.reference),
            PolicyDigest::of(&policy),
        ) else {
            continue;
        };
        // The generation and authority the token was built under: the Machine checks
        // each against the rule.
        let claim = BuildClaim {
            cluster: public_key,
            rule: rule.rule,
            generation: token.generation,
            service: service.clone(),
            policy,
            commit,
            authority: token.authority.clone(),
            stage: BuildStage::Built { image, attest },
        };
        let token = ployz_core::GithubToken::new(token.token.clone());
        proofs.insert(
            service.clone(),
            KeptRule::built(RuleProof {
                rule: signed.clone(),
                evidence: RuleEvidence::Online(token),
                claim: Some(claim),
                reader: run.reader.clone(),
            }),
        );
    }
    proofs
}

/// The proof each image Service `claimed`'s registry rule covers creates with: the signed
/// rule alone, for a spec that names an image of the rule's repository by digest. Each
/// Server pulls that digest from the registry itself and checks the rule's settings, so
/// one the rule no longer approves is refused there. Under a preview rule, each Service it
/// pins creates with the rule alone, whose template names the image.
fn registry_proofs(claimed: &Claimed, proofs: &mut crate::deploy::RuleProofs) {
    use ployz_core::authority::{KeptRule, RuleEvidence, RuleProof};
    let Some(signed) = &claimed.rule else {
        return;
    };
    let Some(rule) = signed.open().ok().filter(|rule| !rule.ended) else {
        return;
    };
    let kept = |evidence, service: &ServiceName| KeptRule {
        proof: Box::new(RuleProof {
            rule: signed.clone(),
            evidence,
            claim: None,
            reader: None,
        }),
        // The logins this Deploy's own Deployment froze become what's applied.
        login: claimed.intent.registry_logins.get(service).cloned(),
    };
    if let Some(preview) = &rule.preview {
        for pinned in &preview.pinned {
            let runs = (claimed.intent.target.iter()).any(|spec| spec.name == pinned.name);
            if runs && !proofs.contains(&pinned.name) {
                proofs.insert(
                    pinned.name.clone(),
                    kept(RuleEvidence::Pinned, &pinned.name),
                );
            }
        }
        return;
    }
    let Some(source) = rule.source.registry() else {
        return;
    };
    for spec in &claimed.intent.target {
        if proofs.contains(&spec.name) || rule.service(&spec.name).is_none() {
            continue;
        }
        let ours = ployz_core::registry_image(&spec.container.image).is_ok_and(|named| {
            named.host == source.host
                && named.repository == source.repository
                && named.digest.is_some()
        });
        if ours {
            proofs.insert(spec.name.clone(), kept(RuleEvidence::Registry, &spec.name));
        }
    }
}

/// Why this Deploy stops before it runs: a Server it places a Service on under a registry
/// rule runs an older Ployz that can't check one. Asked once for the whole Deploy. A
/// listing that fails lets it run, since each such Server still refuses the Service.
async fn registry_rule_behind(
    session: &Session,
    rules: &crate::deploy::RuleProofs,
    preview: &ployz_core::DeployPreview,
) -> Option<String> {
    let placed: BTreeSet<&MachineId> = rules
        .registry_services()
        .filter_map(|service| preview.placed_on.get(service))
        .flatten()
        .collect();
    if placed.is_empty() {
        return None;
    }
    let listed = match session.client() {
        Ok(mut client) => client.machines().await.map_err(|error| error.to_string()),
        Err(error) => Err(error.message),
    };
    let machines = listed
        .inspect_err(|error| {
            tracing::warn!(%error, "couldn't check each Server can deploy under a registry rule");
        })
        .ok()?;
    machines
        .iter()
        .find(|observation| {
            placed.contains(&observation.machine.id)
                && !observation
                    .machine
                    .runtime
                    .capabilities
                    .contains(ployz_core::DEPLOY_REGISTRY_RULE_CAPABILITY)
        })
        .map(|observation| {
            format!(
                "{} runs an older Ployz that can't deploy images under a registry rule. Upgrade it, then deploy again.",
                observation.machine.name
            )
        })
}

/// The image each Service's tag runs as, which the runner writes into its spec so the
/// Servers are asked to create the spec a person signed. The Saved State keeps the tag.
fn pins(ran: &BTreeMap<ServiceName, ployz_store::Pinned>) -> BTreeMap<ServiceName, String> {
    ran.iter()
        .filter_map(|(service, pinned)| {
            let pin = pinned.ran.pin.as_ref()?;
            Some((service.clone(), pin.image.clone()))
        })
        .collect()
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

/// `unprepared` as a Failure. A Server short of storage whose Volumes this Deploy also
/// deletes is told why they don't help: they are deleted only once the Deploy succeeds.
fn refused_before_deletes(
    unprepared: &Unprepared,
    deletes: &[ployz_core::DockerVolumeId],
) -> Failure {
    let mut failure = Failure::from(&unprepared.error);
    if let Some(short) = unprepared.short_of_storage
        && deletes.iter().any(|volume| volume.machine_id == short)
    {
        failure.reason.push_str(
            " Volumes this deploy deletes free their space only after it succeeds, so they don't make room for its new volumes.",
        );
    }
    failure
}

/// Delete exactly the Docker Volumes admission accepted, never others of the same
/// name. Failing to reach the Cluster deletes none, so their Volumes stay deployed.
pub(super) async fn remove_volumes(
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

#[cfg(test)]
#[path = "store_runner_tests.rs"]
mod tests;
