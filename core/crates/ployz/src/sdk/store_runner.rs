//! Cloud's Deployment runner. One call claims a Deployment, builds its Git and
//! uploaded Services, prepares and confirms it on the Organization's Cluster and records each step's
//! evidence, so the Deploy Intent with its unsealed secrets, the session and the raw
//! SDK evidence never leave it: the caller gets only the Deployment's summary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ployz_core::{
    ContainerId, DeployOperation, DeployOutcome, DeployWarning, ExecutionError, FailedOperation,
    HealthFailure, LastRun, MachineId, MachineName, MachineObservation, Operator, QualifiedService,
    RemoveContainerPurpose, RequestedServiceSpec, RpcError, RpcErrorCode, ServiceName,
};
use ployz_store::tools::{LogText, SealedValues, Withheld};
use ployz_store::{
    Actor, BuildReport, BuildStatus, Builder, Claimed, CommitSha, ConfigStore, DeployedNode,
    DeploymentId, DeploymentQuery, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentWarning, EnvironmentId, Failure, NodeStatus, ObservationFailure, RowState,
    RowTracker, RunEvidence, RunnerId, VolumeObservation,
};
use serde::{Deserialize as _, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use super::build::BuildOutcome;
use super::preparation::{BuildReceipt, PreparationInput, UploadDigest, needs_upload};
use super::{
    Dialed, ImageCleanup, PreparedDeploy, Running, RunningBuild, Session, Unprepared,
    connect_expecting,
};
use crate::connect::{Client, SystemConnector};
use crate::context::Connection;
use crate::deploy::MachineOperations;
use crate::deploy::cleanup::{self, Copy, Step};

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
    let run = Run {
        store,
        deployment,
        runner,
        over: OnceLock::new(),
        reader: checkouts
            .as_ref()
            .ok()
            .and_then(|sources| sources.reader.clone()),
    };
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

/// Which of `connections`' Servers hold each Docker Volume in `sought`: the
/// evidence a Deploy that removes deployed Volumes is admitted with. A check that
/// fails names each of `connections`' Servers unanswered, and says whether none
/// could be reached or one answered with an error, naming that one only once its
/// daemon confirmed which Server it is.
///
/// # Errors
/// Returns `invalid_argument` when `connections` is empty.
pub async fn observe_volumes(
    connections: Vec<Connection>,
    sought: Vec<ployz_core::DockerVolumeName>,
) -> Result<ployz_store::VolumeObservation, RpcError> {
    observe_volumes_expecting(connections, sought, None).await
}

/// [`observe_volumes`] through an entry of `expect`'s Cluster only, when given.
///
/// # Errors
/// As [`observe_volumes`].
pub async fn observe_volumes_expecting(
    connections: Vec<Connection>,
    sought: Vec<ployz_core::DockerVolumeName>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<ployz_store::VolumeObservation, RpcError> {
    if connections.is_empty() {
        return Err(RpcError::invalid_argument("connections must not be empty"));
    }
    let servers: Vec<MachineId> = connections
        .iter()
        .filter_map(Connection::machine_id)
        .copied()
        .collect();
    let session = match super::dial(connections, Arc::new(SystemConnector::default()), expect).await
    {
        Ok(session) => session,
        Err(error) => {
            let failure = if error.unreached() {
                ObservationFailure::unreachable(error.into())
            } else {
                // The daemon that refused never confirmed which Server it is.
                ObservationFailure::answered(None, error.into())
            };
            return Ok(VolumeObservation::failed(sought, servers, failure));
        }
    };
    let observed = match session.observe_volumes(sought.clone()).await {
        Ok(observed) => observed,
        Err(error) => {
            let server = session
                .client()
                .ok()
                .and_then(|client| client.entry_machine());
            let failure = ObservationFailure::answered(server, error);
            VolumeObservation::failed(sought, servers, failure)
        }
    };
    session.close().await;
    Ok(observed)
}

/// Which Services run on `connections`' Servers, by Namespace: the evidence a full
/// Deploy's prune is admitted with, naming each Service it removes. A check that
/// fails names each of `connections`' Servers unanswered and says why, as
/// [`observe_volumes`] does.
///
/// # Errors
/// Returns `invalid_argument` when `connections` is empty.
pub async fn observe_running(
    connections: Vec<Connection>,
) -> Result<ployz_store::RunningObservation, RpcError> {
    observe_running_expecting(connections, None).await
}

/// [`observe_running`] through an entry of `expect`'s Cluster only, when given.
///
/// # Errors
/// As [`observe_running`].
pub async fn observe_running_expecting(
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<ployz_store::RunningObservation, RpcError> {
    if connections.is_empty() {
        return Err(RpcError::invalid_argument("connections must not be empty"));
    }
    let servers: Vec<MachineId> = connections
        .iter()
        .filter_map(Connection::machine_id)
        .copied()
        .collect();
    let failed = |failure| ployz_store::RunningObservation {
        services: Vec::new(),
        unanswered: servers.clone(),
        failure: Some(failure),
        dropped: Vec::new(),
    };
    let session = match super::dial(connections, Arc::new(SystemConnector::default()), expect).await
    {
        Ok(session) => session,
        Err(error) => {
            let failure = if error.unreached() {
                ObservationFailure::unreachable(error.into())
            } else {
                ObservationFailure::answered(None, error.into())
            };
            return Ok(failed(failure));
        }
    };
    let observed = match session.observe_running().await {
        Ok(observed) => observed,
        Err(error) => {
            let server = session
                .client()
                .ok()
                .and_then(|client| client.entry_machine());
            failed(ObservationFailure::answered(server, error))
        }
    };
    session.close().await;
    Ok(observed)
}

/// Every copy of every Volume on `connections`' Servers, by role: what Cloud's sweep
/// finds orphaned mirror slots with. With `expect`, only through an entry of its
/// Cluster, naming the Machines it left.
///
/// # Errors
/// Returns the connection's error when no Server can be reached, or listing the
/// Machines fails.
pub async fn observe_copies(
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Dialed<super::CopyObservation>, RpcError> {
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, expect).await?;
    let foreign = session.foreign();
    let observed = session.observe_copies().await;
    session.close().await;
    Ok(Dialed {
        outcome: observed?,
        foreign,
    })
}

/// The old copies a Deployment's retire left running.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, TS)]
pub struct RetireOutcome {
    /// Copies it tried to retire and could not: a retry may still retire them.
    pub left: Vec<RetireLeft>,
    /// Copies it never retires, and Machines the Deploy could not reach: a retry
    /// changes nothing.
    pub kept: Vec<RetireLeft>,
}

/// One old copy still running.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct RetireLeft {
    /// The Machine's name as the Deploy reported it, or its ID when the Deploy
    /// did not know the name.
    pub machine: String,
    /// The Service as the Deployment names it, or none for a Machine the Deploy
    /// could not reach, whose copies it never saw.
    pub service: Option<ServiceName>,
    /// Users read it.
    pub reason: String,
}

/// Retire the old copies Deployment `deployment` of `who`'s Organization left
/// running, on one of `connections`: each copy its Deploy reported left running,
/// of a Service it deployed or removed. A failed or unattempted Service keeps its
/// old copy, which may be the one serving, and is reported left. A copy already
/// gone counts as retired, so a retry observes again and only retires what is
/// still there. A Machine the Deploy could not reach is reported left, since a copy
/// it never saw may run there. With `expect`, it retires only through an entry of
/// its Cluster, as [`run_deployment_expecting`] runs, naming the Machines it left.
///
/// # Errors
/// Returns `not_found` for a Deployment of another Organization, the connection's
/// error when no Server can be reached or listing the Machines fails, or a storage
/// error.
pub async fn retire_old(
    store: Arc<ConfigStore>,
    who: Actor,
    deployment: DeploymentId,
    connections: Vec<Connection>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Dialed<RetireOutcome>, RpcError> {
    let view = super::store_call(&store, move |store| {
        store.read(&who, &DeploymentQuery { id: deployment })
    })
    .await?;
    let copies = left_copies(&view);
    if copies.is_empty() {
        return Ok(Dialed::undialed(named_left(&view, Vec::new())));
    }
    let connector = Arc::new(SystemConnector::default());
    let session = connect_expecting(connections, connector, expect).await?;
    let foreign = session.foreign();
    let retired: Result<_, RpcError> = async {
        let mut client = session.client()?;
        let machines = client.machines().await.map_err(crate::connect::rpc_error)?;
        Ok(retire_copies(&client, &machines, copies).await)
    }
    .await;
    session.close().await;
    Ok(Dialed {
        outcome: named_left(&view, retired?),
        foreign,
    })
}

/// The Deploy's warnings, wherever its runner recorded them: the planner's on the
/// Deploy Preview, the executor's on the Deployment.
fn deploy_warnings(view: &DeploymentView) -> impl Iterator<Item = &DeployWarning> {
    let ran = view.deployment.warnings.iter().filter_map(|warning| {
        let DeploymentWarning::Deploy { warning } = warning else {
            return None;
        };
        Some(warning)
    });
    ran.chain(view.preview.iter().flat_map(|preview| &preview.warnings))
}

/// What `retired` left, plus every old copy the Deploy kept and every Machine it
/// could not reach, so nothing that may still run goes unnamed.
fn named_left(view: &DeploymentView, left: Vec<RetireLeft>) -> RetireOutcome {
    let mut kept = kept_copies(view);
    let reported: BTreeSet<&str> = deploy_warnings(view)
        .filter_map(|warning| {
            let DeployWarning::LeftRunning { machine, .. } = warning else {
                return None;
            };
            Some(machine.as_str())
        })
        .collect();
    for warning in deploy_warnings(view) {
        let DeployWarning::MixedAfterReturn { machine } = warning else {
            continue;
        };
        if reported.contains(machine.as_str())
            || left
                .iter()
                .chain(&kept)
                .any(|entry| entry.machine == *machine)
        {
            continue;
        }
        kept.push(RetireLeft {
            machine: machine.clone(),
            service: None,
            reason: format!(
                "{machine} was cut off during the Deploy, so a copy there may still run. \
                 Deploy again once it is back."
            ),
        });
    }
    RetireOutcome { left, kept }
}

struct LeftCopy {
    service: ServiceName,
    container_id: ContainerId,
    machine: String,
    /// Retire when the Deploy removed its Service, so it goes exactly as a plain
    /// Service removal takes a copy out; Rollout when a new copy replaced it.
    purpose: RemoveContainerPurpose,
}

/// Each copy the Deploy reported left running, with its Service's outcome.
fn reported_copies(view: &DeploymentView) -> impl Iterator<Item = (LeftCopy, NodeStatus)> {
    let services: BTreeMap<QualifiedService, (&ServiceName, NodeStatus)> = view
        .nodes
        .iter()
        .filter_map(|node| {
            let DeployedNode::Service { name, .. } = &node.node else {
                return None;
            };
            let runtime = view.runtime_names.get(name)?;
            let service = QualifiedService::new(view.namespace.clone(), runtime.clone());
            Some((service, (name, node.outcome)))
        })
        .collect();
    deploy_warnings(view).filter_map(move |warning| {
        let DeployWarning::LeftRunning {
            service,
            container_id,
            machine,
            ..
        } = warning
        else {
            return None;
        };
        let (name, outcome) = services
            .get(service)
            .map_or((&service.name, NodeStatus::Unknown), |(name, outcome)| {
                (*name, *outcome)
            });
        let copy = LeftCopy {
            service: name.clone(),
            container_id: *container_id,
            machine: machine.clone(),
            purpose: if outcome == NodeStatus::Removed {
                RemoveContainerPurpose::Retire
            } else {
                RemoveContainerPurpose::Rollout
            },
        };
        Some((copy, outcome))
    })
}

fn advanced(outcome: NodeStatus) -> bool {
    match outcome {
        NodeStatus::Deployed | NodeStatus::Removed => true,
        NodeStatus::Pending
        | NodeStatus::Failed
        | NodeStatus::NotAttempted
        | NodeStatus::Unchanged
        | NodeStatus::Unknown => false,
    }
}

fn left_copies(view: &DeploymentView) -> Vec<LeftCopy> {
    reported_copies(view)
        .filter(|(_, outcome)| advanced(*outcome))
        .map(|(copy, _)| copy)
        .collect()
}

/// The copies of every other Service: the old copy may be the one serving, so it stays.
fn kept_copies(view: &DeploymentView) -> Vec<RetireLeft> {
    reported_copies(view)
        .filter(|(_, outcome)| !advanced(*outcome))
        .map(|(copy, _)| RetireLeft {
            reason: format!(
                "{} did not deploy, so its old copy stays. Deploy again to replace it.",
                copy.service
            ),
            machine: copy.machine,
            service: Some(copy.service),
        })
        .collect()
}

async fn retire_copies<C: MachineOperations>(
    client: &C,
    machines: &[MachineObservation],
    copies: Vec<LeftCopy>,
) -> Vec<RetireLeft> {
    let names: BTreeMap<MachineId, String> = machines
        .iter()
        .map(|observed| (observed.machine.id, observed.machine.name.to_string()))
        .collect();
    let retired = futures_util::future::join_all(copies.iter().map(|copy| async {
        // Names are not unique: every Machine of that name may hold the copy.
        let held_by: Vec<MachineId> = machines
            .iter()
            .map(|observed| &observed.machine)
            .filter(|machine| {
                machine.name.as_str() == copy.machine || machine.id.to_string() == copy.machine
            })
            .map(|machine| machine.id)
            .collect();
        if held_by.is_empty() {
            return vec![format!("{} is not in the Cluster", copy.machine)];
        }
        let mut reasons = Vec::new();
        for machine_id in held_by {
            if let Some(reason) = retire_copy(client, &names, machine_id, copy).await {
                reasons.push(reason);
            }
        }
        reasons
    }))
    .await;
    copies
        .iter()
        .zip(retired)
        .flat_map(|(copy, reasons)| {
            reasons.into_iter().map(|reason| RetireLeft {
                machine: copy.machine.clone(),
                service: Some(copy.service.clone()),
                reason,
            })
        })
        .collect()
}

async fn retire_copy<C: MachineOperations>(
    client: &C,
    names: &BTreeMap<MachineId, String>,
    machine_id: MachineId,
    copy: &LeftCopy,
) -> Option<String> {
    let observed = match client
        .inspect_container(&machine_id, &copy.container_id)
        .await
    {
        Ok(details) => details.container,
        Err(error) if error.code == RpcErrorCode::NotFound => return None,
        Err(error) => return Some(machine_failure_reason(&copy.machine, error)),
    };
    let step = cleanup::retire(
        client,
        &Copy::observed(&observed),
        matches!(copy.purpose, RemoveContainerPurpose::Retire),
        copy.purpose.into(),
        &CancellationToken::new(),
    )
    .await;
    match step {
        Ok(Step::Done) => None,
        Ok(Step::Refused(refusal)) => Some(refusal.message),
        Ok(Step::NotConfirmed(unconfirmed)) if unconfirmed.machines.is_empty() => {
            Some("the route set did not name every live Machine".into())
        }
        Ok(Step::NotConfirmed(unconfirmed)) => {
            let proxies: Vec<String> = unconfirmed
                .machines
                .iter()
                .map(|id| names.get(id).cloned().unwrap_or_else(|| id.to_string()))
                .collect();
            Some(format!("proxy on {} did not confirm", proxies.join(", ")))
        }
        Ok(Step::Cancelled) => Some("interrupted".into()),
        Err(error) => Some(machine_failure_reason(&copy.machine, error)),
    }
}

fn machine_failure_reason(machine: &str, error: RpcError) -> String {
    if error.code == RpcErrorCode::Unavailable || error.target_unanswered() {
        format!("{machine} did not answer")
    } else {
        error.message
    }
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
    /// What the first read that found the store no longer holding the run said. The
    /// store never takes a run back from Unknown or an end, so no later read changes it.
    over: OnceLock<Over>,
    /// Handed to each Server with a rule's proof.
    reader: Option<ployz_core::GithubToken>,
}

/// What one read of the Deployment says this run may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Standing {
    /// The store holds the run: Running.
    Keep,
    /// A user cancelled the Deployment: Cancelling.
    Cancel,
    Over(Over),
    /// The read failed, so nothing is known: the run goes on and reads again.
    Unread,
}

/// The store no longer holds the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Over {
    /// The Deployment is Unknown: its lease lapsed, a newer claim took the Environment
    /// over, its runner claimed it again after a preview, or Cloud abandoned it after
    /// one. The store takes only an Interrupted outcome from the run.
    Lost,
    /// The Deployment ended, or another runner owns it: the store takes nothing more
    /// from the run.
    Ended,
}

/// What `read`, the store's answer to one of this run's records, says the run may do.
fn standing(read: &Result<DeploymentSummary, RpcError>) -> Standing {
    match read {
        Ok(summary) => match summary.status {
            DeploymentStatus::Running => Standing::Keep,
            DeploymentStatus::Cancelling => Standing::Cancel,
            DeploymentStatus::Unknown => Standing::Over(Over::Lost),
            DeploymentStatus::Applied
            | DeploymentStatus::Failed
            | DeploymentStatus::Cancelled
            | DeploymentStatus::WaitingForCi
            | DeploymentStatus::Queued
            | DeploymentStatus::Superseded => Standing::Over(Over::Ended),
        },
        // `record` refuses an ended Deployment, or another runner's, as a conflict.
        Err(refused) if refused.code == RpcErrorCode::Conflict => Standing::Over(Over::Ended),
        Err(_) => Standing::Unread,
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
    /// Claim the Deployment, with the Intent's options this runner plans it with,
    /// driving it as `operator`.
    async fn claim(&self, operator: Operator) -> Result<Claimed, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        let mut claimed =
            super::store_call(&self.store, move |store| store.claim(&deployment, &runner)).await?;
        // The Store lowers the Intent without knowing who will run it.
        claimed.intent.options.operator = operator;
        claimed.intent.options.block_unmountable = true;
        // Each labels its copies with its Deployment's id, as this run does.
        claimed.intent.options.ended_runs = claimed
            .ended_runs
            .iter()
            .filter_map(|ended| ended.as_str().parse().ok())
            .collect();
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
        let approval = claimed.approval.clone();
        pin_approved(&mut claimed.intent.target, approval.as_ref());
        let scrub = Scrub::new(&claimed);
        let earlier = earlier(&targets);
        let mut rules = crate::cluster::RuleProofs::default();
        let prepared = if targets.is_empty() && built.is_empty() {
            self.renewing(session.plan(claimed.intent), || {
                session.inner.cancel.cancel()
            })
            .await
        } else {
            match self.build(session, &claimed, &targets).await? {
                Ok(mut receipts) => {
                    receipts.extend(built);
                    rules = rule_proofs(session, &claimed, &receipts, self.reader.as_ref()).await;
                    self.prepare(session, claimed, targets, receipts).await
                }
                Err(unbuilt) => return self.unbuilt(unbuilt).await,
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(unprepared) => return self.unprepared(&earlier, &deletes, unprepared).await,
        };
        let recorded = self
            .record(RunEvidence::Prepared(prepared.preview().clone()))
            .await;
        match standing(&recorded) {
            Standing::Keep => {}
            Standing::Cancel => {
                return self
                    .not_executed("Cancelled before it executed".into())
                    .await;
            }
            Standing::Over(_) | Standing::Unread => return recorded,
        }
        let log_id = self.deployment.as_str().parse().ok();
        let running = match prepared.confirm_approved(log_id, ImageCleanup::Auto, approval, rules) {
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
                match self.standing().await {
                    Standing::Over(Over::Lost) => {
                        return self.interrupted(outcome, progress).await;
                    }
                    // The store refuses the outcome, and nothing more is deleted.
                    Standing::Over(Over::Ended) => {
                        return self.executed(outcome, progress, Vec::new()).await;
                    }
                    Standing::Keep | Standing::Cancel | Standing::Unread => {}
                }
                // The last chance: the store takes warnings only while the Deployment runs.
                self.record_warnings(&mut warnings).await;
                let removed = if matches!(outcome, DeployOutcome::Success { .. }) {
                    // Deleting is never interrupted: a half-deleted set stays accepted.
                    self.renewing(remove_volumes(session, deletes), || ()).await
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
            // The session ended mid-execution: what ran is unknown.
            Err(_) => self.record(RunEvidence::Abandoned).await,
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
                .report(service, BuildStatus::Building, None, String::new(), None)
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
        let mut server = None;
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
    /// reuses their images and only delivers them.
    async fn prepare(
        &self,
        session: &Session,
        claimed: Claimed,
        targets: Vec<Target>,
        receipts: BTreeMap<ServiceName, BuildReceipt>,
    ) -> Result<PreparedDeploy, Unprepared> {
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
        let running: Running<PreparedDeploy, Unprepared> = session.prepare_with(
            input,
            claimed.intent.registry_auth,
            Some((
                claimed.intent.options.operator,
                claimed.intent.options.ended_runs,
            )),
        )?;
        self.renewing(running.finished(), || running.abort()).await
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
        let mut stopped = BTreeMap::new();
        while let Some(event) = running.next().await {
            let mut rows = match event {
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
            if let Ok(client) = session.client() {
                for row in &mut rows {
                    if let ployz_core::OperationStatus::Failed { error } = &mut row.status {
                        self.fill_last_run(&client, scrub, row.machine_id, error, &mut stopped)
                            .await;
                    }
                }
            }
            for mut row in tracker.changes(&rows) {
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
        let mut outcome = running.finished().await?;
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
            if let Err(error) = self.record(RunEvidence::Warned(warning.clone())).await {
                tracing::warn!(
                    code = error.code.as_str(),
                    error = error.message.as_str(),
                    "couldn't record a Deploy warning; retrying later"
                );
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
        loop {
            tokio::select! {
                done = &mut work => return done,
                _ = poll.tick() => match self.standing().await {
                    // ponytail: a failed read skips one check; the next tick reads again.
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
        let recorded = self
            .record(RunEvidence::Executed {
                progress: progress.clone(),
                outcome: Box::new(outcome.clone()),
                removed,
            })
            .await;
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

    /// Gives a Container's stop its last run from the Log Store, read once per Container
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
        *last_run = Some(run.clone());
    }

    /// The failed Container's scrubbed tail, and the run its newest stored exit ended.
    async fn failed_log(
        &self,
        client: &Client,
        scrub: &Scrub,
        machine: MachineId,
        container: ContainerId,
    ) -> (LogText, LastRun) {
        let withheld = |why| (LogText::Withheld { why }, LastRun::default());
        let Some(env) = super::container_env(client, machine, container).await else {
            return withheld(Withheld::NoSecretContext);
        };
        let Ok(mut names) = self.sealed_names(&scrub.environment).await else {
            return withheld(Withheld::NoSecretContext);
        };
        names.extend(scrub.sealed_at_claim.iter().cloned());
        let Some(tail) = super::tail_frames(client, machine, container).await else {
            return withheld(Withheld::Unanswered);
        };
        match self.settled_sealed_names(&scrub.environment, names).await {
            Some(names) => tail.scrubbed(&scrub.values(&names, env)),
            None => (
                LogText::Withheld {
                    why: Withheld::NoSecretContext,
                },
                LastRun {
                    exit_code: tail.exit_code(),
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
        super::store_call(&self.store, move |store| store.sealed_names(&environment)).await
    }

    async fn record(&self, evidence: RunEvidence) -> Result<DeploymentSummary, RpcError> {
        let (deployment, runner) = (self.deployment.clone(), self.runner.clone());
        super::store_call(&self.store, move |store| {
            store.record(&deployment, &runner, evidence)
        })
        .await
    }

    /// Renew this runner's lease on the Deployment and read what the run may do. Once a
    /// read found the store no longer holding the run, it reads nothing more and says
    /// the same: a read that fails after a takeover still leaves the run lost.
    async fn standing(&self) -> Standing {
        if let Some(over) = self.over.get() {
            return Standing::Over(*over);
        }
        let read = standing(&self.record(RunEvidence::Alive).await);
        if let Standing::Over(over) = read {
            self.over.get_or_init(|| over);
        }
        read
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
                crate::ui::then_sentence(why, "No other Builder in your Build Order can take it")
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

/// The proof each Service built under `claimed`'s automatic deploy rule or preview rule
/// creates with: the rule, its image token, and the claim the token's audience names,
/// rebuilt from the rule, the pinned commit, the image the Machine received, the
/// Cluster's keys, and the generation and authority the token was built under. A
/// person's approval of the commit rides along. A Service without all of them gets
/// none, and an enforcing Server refuses it.
async fn rule_proofs(
    session: &Session,
    claimed: &Claimed,
    receipts: &BTreeMap<ServiceName, BuildReceipt>,
    reader: Option<&ployz_core::GithubToken>,
) -> crate::cluster::RuleProofs {
    use ployz_core::authority::{
        BuildClaim, BuildStage, CommitSha, ImageDigest, PolicyDigest, PreviewProof, RuleProof,
    };
    let mut proofs = crate::cluster::RuleProofs::default();
    // Each rule's ID, generation and the policy it approves for a Service.
    let rule = claimed.rule.as_ref().and_then(|signed| signed.open().ok());
    let preview = claimed
        .preview
        .as_ref()
        .and_then(|signed| signed.open().ok());
    let approved = |service: &ServiceName| match (&rule, &preview) {
        (Some(rule), _) => rule
            .service(service)
            .map(|approved| (rule.rule, &approved.policy)),
        (None, Some(preview)) => preview
            .service(service)
            .map(|approved| (preview.rule, &approved.policy)),
        (None, None) => None,
    };
    if (rule.is_none() && preview.is_none()) || claimed.image_tokens.is_empty() {
        return proofs;
    }
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
        let approved = approved(service);
        let receipt = receipts.get(service);
        let commit = claimed
            .sources
            .iter()
            .find(|source| source.service == *service)
            .and_then(|source| source.commit.as_ref())
            .and_then(|commit| CommitSha::parse(commit.as_str()).ok());
        let (Some(approved), Some(receipt), Some(commit)) = (approved, receipt, commit) else {
            continue;
        };
        let (id, approved) = approved;
        let (Ok(image), Ok(policy)) = (
            ImageDigest::parse(&receipt.image.reference),
            PolicyDigest::of(approved),
        ) else {
            continue;
        };
        // The generation and authority the token was built under: the Machine checks
        // each against the rule and, for a reviewed or approved build, the person's word.
        let claim = BuildClaim {
            cluster: public_key,
            rule: id,
            generation: token.generation,
            service: service.clone(),
            policy,
            commit,
            authority: token.authority,
            stage: BuildStage::Built { image, attest },
        };
        let token = ployz_core::GithubToken::new(token.token.clone());
        match (&claimed.rule, &claimed.preview) {
            (Some(signed), _) => {
                proofs.rules.insert(
                    service.clone(),
                    RuleProof {
                        rule: signed.clone(),
                        token,
                        claim,
                        approval: claimed.approvals.get(service).cloned(),
                        reader: reader.cloned(),
                    },
                );
            }
            (None, Some(signed)) => {
                proofs.previews.insert(
                    service.clone(),
                    PreviewProof {
                        rule: signed.clone(),
                        review: claimed.review.clone(),
                        token,
                        claim,
                        reader: reader.cloned(),
                    },
                );
            }
            (None, None) => {}
        }
    }
    proofs
}

/// Write the image each Service's approval pinned into its spec, so the Servers are asked
/// to create the spec the approval's digest covers. The Saved State keeps the tag. A
/// Server refuses a spec the approval doesn't match, so a runner can't choose the image.
fn pin_approved(
    target: &mut [RequestedServiceSpec],
    approval: Option<&ployz_core::authority::Signed<ployz_core::authority::DeployApproval>>,
) {
    let Some(approved) = approval.and_then(|approval| approval.open().ok()) else {
        return;
    };
    for spec in target {
        approved.pin(spec);
    }
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

#[cfg(test)]
#[path = "store_runner_tests.rs"]
mod tests;
