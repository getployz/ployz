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
    DeploymentStatus, DeploymentSummary, RunEvidence, RunnerId,
};
use serde::Deserialize as _;
use serde_json::Value;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput, UploadDigest};
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

/// `candidates` as preparation takes them: the first as the Service's receipt, the
/// rest borrowed.
fn hinted(input: &mut PreparationInput, service: &ServiceName, candidates: Vec<BuildReceipt>) {
    let mut candidates = candidates.into_iter();
    if let Some(first) = candidates.next() {
        input.build_receipts.insert(service.clone(), first);
    }
    let rest: Vec<_> = candidates.collect();
    if !rest.is_empty() {
        input.borrowed.insert(service.clone(), rest);
    }
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
        run.store(move |store| store.claim(&deployment, &runner))
            .await?
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
    let session = match connect_connections(connections, Arc::new(SystemConnector::default())).await
    {
        Ok(session) => session,
        // Users read it on the Deployment: plain words and one action, not transport detail.
        Err(_) => {
            return run
                .not_executed(
                    "Ployz couldn't reach your Servers. Check that they're online, then retry."
                        .into(),
                )
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
            session.preview(claimed.intent).await
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
                return match upload_needed(&error) {
                    Some(services) => self.unbuilt(Unbuilt::UploadNeeded(services)).await,
                    None => self.not_executed(error.message).await,
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
            Err(error) => return self.not_executed(error.message).await,
        };
        let outcome = self.renewing(running.finished(), || running.abort()).await;
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
            let candidates = receipts(claimed, service);
            let hint = candidates.first().cloned();
            let mut input = PreparationInput {
                deployment: only(&claimed.input, service),
                sources: BTreeMap::new(),
                source_commits: BTreeMap::new(),
                uploads: BTreeMap::new(),
                build_receipts: BTreeMap::new(),
                borrowed: BTreeMap::new(),
                build_index: index,
                preferred_machine: match &target.build {
                    Build::Git {
                        preferred_machine, ..
                    } => *preferred_machine,
                    Build::Upload { .. } => None,
                },
            };
            target.input(&mut input);
            hinted(&mut input, service, candidates);
            builds.push((target, hint, session.build(input, None)?));
        }
        let all = futures_util::future::join_all(
            builds
                .iter()
                .map(|(target, hint, running)| self.follow(target, hint.as_ref(), running)),
        );
        let ended = self
            .renewing(all, || {
                for (_, _, running) in &builds {
                    running.abort();
                }
            })
            .await;
        let mut receipts = BTreeMap::new();
        let mut failed = Vec::new();
        let mut uploads = Vec::new();
        for (ended, (target, _, _)) in ended.into_iter().zip(&builds) {
            match ended? {
                Ok(receipt) => {
                    receipts.insert(target.service.clone(), receipt);
                }
                Err(error) => match upload_needed(&error) {
                    Some(services) => uploads.extend(services),
                    None => failed.push(format!("{}: {}", target.service, error.message)),
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
        hint: Option<&BuildReceipt>,
        running: &RunningBuild,
    ) -> Result<Result<BuildReceipt, RpcError>, RpcError> {
        let service = &target.service;
        self.report(service, BuildStatus::Building, None, String::new())
            .await?;
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
            Ok(BuildOutcome::Built { receipt }) => {
                // The same image content: reused, even when only its variables changed.
                let reused =
                    hint.is_some_and(|hint| hint.image.reference == receipt.image.reference);
                let status = if reused {
                    BuildStatus::Reused
                } else {
                    BuildStatus::Built
                };
                (status, None, Ok(receipt))
            }
            Ok(BuildOutcome::Queued) => {
                let error = internal("No Server started the build");
                (BuildStatus::Failed, Some(error.message.clone()), Err(error))
            }
            Err(error) => (BuildStatus::Failed, Some(error.message.clone()), Err(error)),
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
            source_commits: BTreeMap::new(),
            sources: BTreeMap::new(),
            uploads: BTreeMap::new(),
            build_receipts: receipts,
            borrowed: BTreeMap::new(),
            build_index: 0,
            preferred_machine: None,
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
            Unbuilt::Failed(reason) => self.not_executed(reason).await,
            Unbuilt::UploadNeeded(services) => {
                self.record(RunEvidence::UploadNeeded(services)).await
            }
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

    /// Renew this runner's lease on the Deployment and read its status back, which
    /// says whether it was cancelled.
    async fn status(&self) -> Result<DeploymentStatus, RpcError> {
        self.record(RunEvidence::Alive)
            .await
            .map(|summary| summary.status)
    }

    /// Store calls block on the database, so they run off the async threads.
    async fn store<T: Send + 'static>(
        &self,
        work: impl FnOnce(&ConfigStore) -> Result<T, RpcError> + Send + 'static,
    ) -> Result<T, RpcError> {
        super::store_call(&self.store, work).await
    }
}

/// What `claimed` builds: each Git Service from its checkout at its pin, and each
/// uploaded Service from the upload, if Cloud still holds it.
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
    let digest = UploadDigest::parse(upload.digest.as_str())
        .map_err(|error| Unbuilt::Failed(error.message))?;
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

/// The uploaded Services `error` says need a new upload, if that's what it says.
fn upload_needed(error: &RpcError) -> Option<Vec<ServiceName>> {
    let preparation = error.details.get("preparation")?;
    if preparation.get("kind")?.as_str()? != "upload_needed" {
        return None;
    }
    serde_json::from_value(preparation.get("services")?.clone()).ok()
}

/// Lowering input `input` narrowed to Service `service`: what its own build takes.
pub(super) fn only(input: &Value, service: &ServiceName) -> Value {
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
    }
}
