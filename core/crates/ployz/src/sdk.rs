use crate::cluster::{Call, Deadline, Repeat};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{
    Client, ConnectError, Connector, TransportError, connect_selected_with, name_refusal,
};
use crate::context::{Connection, ConnectionSource, SelectedConnections};
use crate::deploy::{DeployIntent, DeployPlan, VolumeFate};
use crate::sealing_backfill::{BackfillSealingKeyReport, BackfillSealingKeyRequest};
use ployz_core::{
    CertificateMaterialPublished, ClusterTeardown, ContractDescription, DataLossConfirmation,
    DeployEvent, DeployOutcome, DescribeContractRequest, EnrollmentAssignment, EnrollmentSnapshot,
    ExecutionError, LocalMachineRemoved, MachineTarget, Namespace, ObservedDataLoss, OpaquePayload,
    PublishCertificateMaterialRequest, RUNTIME_WATCH_CAPABILITY, Registered, RemoveVolumesRequest,
    Rpc, RpcError, RpcErrorCode, RpcRequestBody, RpcResponseBody, RuntimeWatchFrame,
    RuntimeWatchRequest, ServiceObservation, VolumeRemoval, decode_runtime_watch_frame, op,
};

pub use payloads::generated_files;

pub mod agent;
mod build;
mod deploy;
mod github_build;
mod logs;
mod metrics;
mod payloads;
pub(crate) mod preparation;
pub(crate) mod prepare;
mod running;
mod sealed;
mod store_call;
mod store_runner;
pub use build::{BuildOutcome, OutsideBuild};
pub use deploy::ImageCleanup;
pub use github_build::{
    GithubCheckIn, GithubFinish, GithubKey, GithubLook, GithubReported, GithubStart, RuleAsk,
    RuleCheckIn, RuleGranted, github_cancel, github_check_in, github_finish, github_report,
    github_rule_check_in, github_rule_grant, github_start,
};
pub use running::Running;
pub use store_call::store_call;
pub use store_runner::{
    RetireLeft, RetireOutcome, Sources, observe_copies, observe_running, observe_running_expecting,
    observe_volumes, observe_volumes_expecting, retire_old, run_deployment,
    run_deployment_expecting,
};

/// Cancellable preparation whose progress is retained until read, within a byte budget.
pub type RunningPreparation = Running<PreparedDeploy>;

/// Cancellable Image Build whose progress is retained until read, within a byte budget.
pub type RunningBuild = Running<BuildOutcome>;

/// Why a Deploy was not prepared, and whether deploying the same thing again fails the
/// same way. Decided from the typed refusal before it became an `RpcError`, never read
/// back from one's details: a remote error passes its details through unchanged.
pub(crate) struct Unprepared {
    pub(crate) error: RpcError,
    pub(crate) same_again: bool,
    /// The Server whose storage could not hold the Deploy's Volumes.
    pub(crate) short_of_storage: Option<ployz_core::MachineId>,
}

impl From<RpcError> for Unprepared {
    fn from(error: RpcError) -> Self {
        Self {
            error,
            same_again: false,
            short_of_storage: None,
        }
    }
}

impl From<Unprepared> for RpcError {
    fn from(unprepared: Unprepared) -> Self {
        unprepared.error
    }
}

impl From<crate::deploy::DeployError> for Unprepared {
    fn from(error: crate::deploy::DeployError) -> Self {
        Self {
            same_again: error.same_again(),
            short_of_storage: error.short_of_storage(),
            error: error.into(),
        }
    }
}

fn unprepared(
    error: crate::sdk::prepare::PreparationError,
    cancellation_requested: bool,
) -> Unprepared {
    Unprepared {
        same_again: error.same_again(),
        short_of_storage: error.short_of_storage(),
        error: preparation_error(error, cancellation_requested),
    }
}
pub(crate) use logs::{AgentTail, container_env, process_tail, service_tail, tail_frames};
pub use logs::{
    ContainerLogInput, ContainerLogStream, LogHistoryInput, LogHistoryRecord, LogHistoryStream,
};
pub use metrics::{MetricHistory, MetricHistoryInput, MetricSubject, metric_history};
pub use preparation::{
    BuildReceipt, BuildVariables, OutsideBuildInput, PreparationInput, UploadDigest, VERSION,
    expected_fingerprints,
};
pub use sealed::{
    LiveLogRecord, LogLine, PendingNames, ScrubbedHistoryStream, ScrubbedLogStream, SealedNames,
};

/// The public SDK Watch frame: the RPC frame plus what this observer derives
/// from it: the Services of its Containers, and each Machine's build
/// concurrency in effect (its explicit value, or the automatic one).
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct RuntimeWatchView {
    #[serde(flatten)]
    pub frame: RuntimeWatchFrame,
    pub services: Vec<ServiceObservation>,
    pub effective_build_concurrency:
        std::collections::BTreeMap<ployz_core::MachineId, ployz_core::BuildConcurrency>,
}

impl From<RuntimeWatchFrame> for RuntimeWatchView {
    fn from(frame: RuntimeWatchFrame) -> Self {
        let services = frame.services();
        let effective_build_concurrency = frame
            .machines
            .iter()
            .map(|observed| {
                (
                    observed.machine.id,
                    observed.machine.effective_build_concurrency(),
                )
            })
            .collect();
        Self {
            frame,
            services,
            effective_build_concurrency,
        }
    }
}

/// A Store call's outcome, and the Machines its dial left because they hold another
/// Cluster's key ([`ClusterCheck::foreign`](crate::connect::ClusterCheck::foreign)),
/// for Cloud to log as it logs an Organization's connection's.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Dialed<T> {
    #[serde(flatten)]
    pub outcome: T,
    pub foreign: Vec<ployz_core::MachineId>,
}

impl<T> Dialed<T> {
    /// `outcome`, of a call that dialed no Machine.
    pub(crate) fn undialed(outcome: T) -> Self {
        Self {
            outcome,
            foreign: Vec::new(),
        }
    }
}

/// Every copy of every Volume a Cluster's Machines hold, by role. Machines that did
/// not answer are named in `unanswered`, never assumed to hold nothing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct CopyObservation {
    pub copies: Vec<ObservedCopy>,
    pub unanswered: Vec<ployz_core::MachineId>,
}

/// One copy of a Volume on one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct ObservedCopy {
    pub machine_id: ployz_core::MachineId,
    pub name: ployz_core::DockerVolumeName,
    pub role: ployz_core::CopyRole,
}

/// The gRPC path of `body` when it is one of the Volume switch verbs a Volume run sends.
///
/// # Errors
/// Returns `invalid_argument` for every other command.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "an allowlist: every command not named is refused"
)]
fn volume_switch_path(body: &RpcRequestBody) -> Result<&'static str, RpcError> {
    use RpcRequestBody as Body;
    match body {
        Body::InspectVolumeCopy(_)
        | Body::SeedVolume(_)
        | Body::SetVolumeCopy(_)
        | Body::SetVolumeSize(_) => body.unary_path().ok_or_else(|| {
            RpcError::invalid_argument(format!("{} is not a unary RPC", body.command()))
        }),
        _ => Err(RpcError::invalid_argument(format!(
            "{} is not a Volume switch command",
            body.command()
        ))),
    }
}

/// The JSON a Volume switch verb's answer carries, or the Machine's error as it sent it.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "the Volume switch verbs answer with three reply kinds; any other kind is a Machine fault"
)]
fn volume_switch_payload(command: &str, body: RpcResponseBody) -> Result<Value, RpcError> {
    let payload = match body {
        RpcResponseBody::VolumeCopyView(view) => serde_json::to_value(view),
        RpcResponseBody::SetVolumeCopyReply(reply) => serde_json::to_value(reply),
        RpcResponseBody::SetVolumeSizeReply(reply) => serde_json::to_value(reply),
        RpcResponseBody::Error(error) => return Err(error),
        other => {
            return Err(RpcError::internal(format!(
                "Machine answered {command} with {}",
                other.kind().as_str()
            )));
        }
    };
    payload.map_err(|error| {
        RpcError::internal("Volume switch reply could not be encoded")
            .with_causes(vec![error.to_string()])
    })
}

struct SessionInner {
    client: std::sync::Mutex<Option<Client>>,
    cancel: CancellationToken,
}

/// Connected Cloud session over one confirmed management connection.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

/// Complete Runtime Watch frames from the entry Machine.
///
/// Drop or [`cancel`](Self::cancel) ends this stream only. The Client stays usable.
pub struct Watch {
    cancel: CancellationToken,
    session: std::sync::Weak<SessionInner>,
    stream: Arc<Mutex<Option<tonic::Streaming<OpaquePayload>>>>,
    /// Ends the Watch with the client that opened it.
    refused: crate::cluster::Refusal,
}

/// A planned Deploy that has not executed. [`Self::confirm`] runs these operations.
pub struct PreparedDeploy {
    preview: DeployPlan,
    build_receipts: std::collections::BTreeMap<ployz_core::ServiceName, preparation::BuildReceipt>,
    session: std::sync::Weak<SessionInner>,
    confirmed: AtomicBool,
    retained: std::sync::Mutex<Option<Vec<crate::build::BuiltService>>>,
    prune_targets: Vec<ployz_core::PruneTarget>,
}

type DeployTask = tokio::task::JoinHandle<Result<DeployOutcome<ExecutionError>, RpcError>>;

/// In-flight execution of one Deploy Preview.
pub struct RunningDeploy {
    cancel: CancellationToken,
    events: Mutex<Option<mpsc::UnboundedReceiver<DeployEvent>>>,
    join: Mutex<Option<DeployTask>>,
}

/// A failure, and whether no Machine sent it: the dial failed, or the call went out and
/// no status came back. A Machine's own refusal is `unanswered: false`, and so is a call
/// the entry Machine says it could not dial its target for, since no Machine got it.
#[derive(Debug)]
pub struct CallError {
    pub error: RpcError,
    pub unanswered: bool,
    /// The entry Machine says it could not dial the call's target.
    pub target_unreached: bool,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for CallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl From<ConnectError> for CallError {
    fn from(error: ConnectError) -> Self {
        Self {
            unanswered: (error.unreached() || error.unreplied()) && !error.target_unreached(),
            target_unreached: error.target_unreached(),
            error: error.into(),
        }
    }
}

impl From<RpcError> for CallError {
    fn from(error: RpcError) -> Self {
        Self {
            error,
            unanswered: false,
            target_unreached: false,
        }
    }
}

impl From<CallError> for RpcError {
    fn from(failure: CallError) -> Self {
        failure.error
    }
}

/// Select the first confirmed connection before any operation is dispatched.
///
/// # Errors
/// Returns connection/identity failures; an empty connection list is invalid.
pub async fn connect_connections(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
) -> Result<Session, RpcError> {
    Ok(connect_answered(connections, connector).await?)
}

/// [`connect_connections`], keeping whether a Machine refused or none answered.
///
/// # Errors
/// As [`connect_connections`].
pub async fn connect_answered(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
) -> Result<Session, CallError> {
    connect_expecting(connections, connector, None).await
}

/// [`connect_answered`], keeping only an entry in `expect`'s Cluster when it names one.
///
/// # Errors
/// As [`connect_connections`]; when every entry belongs to another Cluster, the last one's
/// [`ConnectError::ClusterMismatch`](crate::connect::ConnectError::ClusterMismatch).
pub async fn connect_expecting(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Session, CallError> {
    if connections.is_empty() {
        return Err(RpcError::invalid_argument("connections must not be empty").into());
    }
    Ok(dial(connections, connector, expect).await?)
}

/// [`connect_connections`] to nonempty `connections`, keeping the connection error.
async fn dial(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
    expect: Option<crate::connect::ClusterExpectation>,
) -> Result<Session, ConnectError> {
    let selected = SelectedConnections {
        source: ConnectionSource::Direct,
        connections,
    };
    let client = match expect {
        Some(expect) => {
            crate::connect::connect_selected_expecting(selected, connector, expect).await?
        }
        None => connect_selected_with(selected, connector).await?,
    };
    Ok(Session {
        inner: Arc::new(SessionInner {
            client: std::sync::Mutex::new(Some(client)),
            cancel: CancellationToken::new(),
        }),
    })
}

impl Session {
    /// What the first dial confirmed about the entry's Cluster; None when it expected none.
    /// A redial checks the Cluster again but leaves this as the first dial found it.
    #[must_use]
    pub fn cluster(&self) -> Option<crate::connect::ClusterCheck> {
        self.inner.client.lock().ok()?.as_ref()?.cluster().cloned()
    }

    /// The Machines the dial left because they hold another Cluster's key; none once closed.
    #[must_use]
    pub fn foreign(&self) -> Vec<ployz_core::MachineId> {
        self.cluster()
            .map(|check| check.foreign)
            .unwrap_or_default()
    }

    fn client(&self) -> Result<Client, RpcError> {
        self.inner
            .client
            .lock()
            .expect("session client lock")
            .as_ref()
            .ok_or_else(closed)
            .cloned()
    }

    async fn until_closed<T, E: From<RpcError>>(
        &self,
        work: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => Err(closed().into()),
            result = work => result,
        }
    }

    /// [`Self::until_closed`] for a call the Machine may take: a close once the call has
    /// started ends the wait, not the call, so it is `unanswered`. A close seen before the
    /// call starts sends nothing.
    async fn until_closed_answered<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, CallError>>,
    ) -> Result<T, CallError> {
        let started = AtomicBool::new(false);
        tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => Err(CallError {
                error: closed(),
                unanswered: started.load(Ordering::Relaxed),
                target_unreached: false,
            }),
            result = async {
                started.store(true, Ordering::Relaxed);
                work.await
            } => result,
        }
    }

    async fn unary<T: Rpc>(
        &self,
        request: T::Request,
        replay: Repeat<'static>,
    ) -> Result<T::Response, RpcError> {
        Ok(self.unary_answered::<T>(request, replay).await?)
    }

    async fn unary_answered<T: Rpc>(
        &self,
        request: T::Request,
        replay: Repeat<'static>,
    ) -> Result<T::Response, CallError> {
        let mut client = self.client()?;
        self.until_closed_answered(async {
            client
                .call::<T>(request, replay)
                .await
                .map_err(CallError::from)
        })
        .await
    }

    /// Observe enrollment facts on this confirmed Entry Machine.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or missing enrollment facts.
    pub async fn observe_enrollment(&self) -> Result<EnrollmentSnapshot, RpcError> {
        let mut client = self.client()?;
        self.until_closed(crate::enrollment::observe_enrollment(&mut client))
            .await
    }

    /// Publish a durably saved assignment on this Entry Machine without mutation replay.
    ///
    /// # Errors
    /// Returns cancellation, allocation conflicts, or publication errors.
    pub async fn register(
        &self,
        assignment: &EnrollmentAssignment,
    ) -> Result<Registered, RpcError> {
        let mut client = self.client()?;
        tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => Err(RpcError::unavailable("session closed; in-flight Register outcome may be uncertain")),
            result = crate::enrollment::publish_enrollment(&mut client, assignment) => result,
        }
    }

    /// Clear `label`'s Management Client slot. A reply confirms the Clear, but revoking
    /// the caller's own connection may drop it. A later dial confirms removal only with explicit
    /// `management_client: "cleared"` details; a replaced key refusal does not.
    ///
    /// # Errors
    /// Returns cancellation or transport errors, including uncertain outcomes.
    pub async fn clear_management_client(
        &self,
        label: ployz_core::ManagementClientLabel,
    ) -> Result<(), RpcError> {
        let client = self.client()?;
        self.until_closed(async {
            client
                .call_shared::<op::SetManagementClient>(
                    ployz_core::SetManagementClientRequest::Clear { label },
                    Call::once().within(Deadline::MutationReply(
                        crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
                    )),
                )
                .await
                .map(|_| ())
                .map_err(RpcError::from)
        })
        .await
    }

    /// Point this Machine's crash pushes at `url`, or stop them with `None`.
    ///
    /// # Errors
    /// Returns `unsupported` when the Machine does not advertise the operation,
    /// `invalid_argument` for a URL it will not push to, and cancellation or
    /// transport errors.
    pub async fn set_push_target(&self, url: Option<url::Url>) -> Result<(), RpcError> {
        let description = self.about().await?;
        if !description.supports(ployz_core::SET_PUSH_TARGET_CAPABILITY) {
            return Err(name_refusal(
                RpcError::unsupported(format!(
                    "{} is not advertised",
                    ployz_core::SET_PUSH_TARGET_CAPABILITY
                )),
                op::SetPushTarget::PATH,
                &MachineTarget::from(&description.machine_id),
            ));
        }
        self.unary::<op::SetPushTarget>(ployz_core::SetPushTargetRequest { url }, Call::keyed())
            .await
            .map(drop)
    }

    /// Name the Cluster through the entry Machine. The last name written wins across the
    /// Cluster, and each Machine's Internal DNS answers the alias once the name reaches it.
    ///
    /// # Errors
    /// Returns `unsupported` when the Machine predates Cluster Names, `unavailable` off a
    /// participating Machine, and transport errors.
    pub async fn set_cluster_name(
        &self,
        name: ployz_core::ClusterName,
    ) -> Result<ployz_core::ClusterNamed, RpcError> {
        let description = self.about().await?;
        if !description.supports(ployz_core::SET_CLUSTER_NAME_CAPABILITY) {
            return Err(name_refusal(
                RpcError::unsupported(format!(
                    "{} is not advertised",
                    ployz_core::SET_CLUSTER_NAME_CAPABILITY
                )),
                op::SetClusterName::PATH,
                &MachineTarget::from(&description.machine_id),
            ));
        }
        self.unary::<op::SetClusterName>(ployz_core::SetClusterNameRequest { name }, Call::keyed())
            .await
    }

    /// Set `label`'s Management Client slot to a fresh client key and return its Management
    /// Capability. A repeated Set rotates the key; the previous one works until the new one is used.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or `failed_precondition` off a participating Machine.
    pub async fn set_management_client(
        &self,
        label: ployz_core::ManagementClientLabel,
    ) -> Result<ployz_core::ManagementCapability, CallError> {
        let response = self
            .unary_answered::<op::SetManagementClient>(
                ployz_core::SetManagementClientRequest::Set { label },
                Call::keyed(),
            )
            .await?;
        response.capability.ok_or_else(|| {
            RpcError::internal("Machine set a Management Client without a Management Capability")
                .into()
        })
    }

    /// Inspect the selected Machine, including its Management Client labels.
    ///
    /// # Errors
    /// Returns cancellation or Inspect errors.
    pub async fn inspect(&self) -> Result<ployz_core::MachineDetails, RpcError> {
        self.unary::<op::Inspect>(ployz_core::InspectRequest::default(), Call::read())
            .await
    }

    /// Inspect `machine` through the entry, as [`Self::inspect`] does the entry itself.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed Machine Target, cancellation, or the
    /// Machine's Inspect error, or the transport's when it did not answer.
    pub async fn inspect_machine(
        &self,
        machine: &str,
    ) -> Result<ployz_core::MachineDetails, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let client = self.client()?;
        self.until_closed(async {
            client
                .call_shared::<op::Inspect>(
                    ployz_core::InspectRequest::default(),
                    Call::once()
                        .to(&target)
                        .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                )
                .await
                .map_err(crate::connect::rpc_error)
        })
        .await
    }

    /// Publish or clear Certificate Material for one hostname or single-level wildcard.
    /// Set and Clear are idempotent, so transport drops are retried.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or an `invalid_argument` refusal
    /// when the chain, key match, or hostname coverage does not hold.
    pub async fn publish_certificate_material(
        &self,
        request: PublishCertificateMaterialRequest,
    ) -> Result<CertificateMaterialPublished, RpcError> {
        self.unary::<op::PublishCertificateMaterial>(request, Call::keyed())
            .await
    }

    /// Mint a Build Grant on the entry Machine: one image push into `repository`.
    /// Not retried: a lost reply leaves an unused grant that expires by itself.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, an `invalid_argument` repository, or
    /// the Machine's image ingest failure.
    pub async fn mint_build_grant(
        &self,
        request: ployz_core::MintBuildGrantRequest,
    ) -> Result<ployz_core::BuildGrantMinted, RpcError> {
        let client = self.client()?;
        self.until_closed(async {
            client
                .call_shared::<op::MintBuildGrant>(
                    request,
                    Call::once().within(Deadline::MutationReply(
                        crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
                    )),
                )
                .await
                .map_err(RpcError::from)
        })
        .await
    }

    /// Read what a Build Grant on the entry Machine received. Reading does not end it.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or `not_found` once the grant expired
    /// or the Machine restarted.
    pub async fn inspect_build_grant(
        &self,
        request: ployz_core::InspectBuildGrantRequest,
    ) -> Result<ployz_core::BuildGrantInspected, RpcError> {
        self.unary::<op::InspectBuildGrant>(request, Call::read())
            .await
    }

    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or
    /// `DescribeContract` fails.
    pub async fn about(&self) -> Result<ContractDescription, RpcError> {
        self.unary::<op::DescribeContract>(DescribeContractRequest {}, Call::read())
            .await
    }

    /// Open a Runtime Watch stream of complete frames.
    ///
    /// Checks the advertised capability name. Missing Watch is unsupported; this
    /// never polls list RPCs. There is no cursor or resume protocol.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, Watch is not
    /// advertised, or the stream cannot be opened.
    pub async fn watch(&self) -> Result<Watch, RpcError> {
        let description = self.about().await?;
        let client = self.client()?;
        let entry = MachineTarget::from(&description.machine_id);
        if !description.supports(RUNTIME_WATCH_CAPABILITY) {
            return Err(name_refusal(
                RpcError::unsupported(format!("{RUNTIME_WATCH_CAPABILITY} is not advertised")),
                op::RuntimeWatch::PATH,
                &entry,
            ));
        }
        let payload = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
            .encode()
            .map_err(ConnectError::from)?;
        let stream = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed()),
            stream = client.open_stream::<op::RuntimeWatch>(None, tokio_stream::once(payload)) => stream
                .map_err(|error| ConnectError::Rpc(error.name_refusal(op::RuntimeWatch::PATH, &entry)))?,
        };
        let cancel = self.inner.cancel.child_token();
        let stream = Arc::new(Mutex::new(Some(stream)));
        let cleanup = stream.clone();
        let cancelled = cancel.clone();
        tokio::spawn(async move {
            cancelled.cancelled().await;
            cleanup.lock().await.take();
        });
        Ok(Watch {
            cancel,
            session: Arc::downgrade(&self.inner),
            stream,
            refused: client.refusal(),
        })
    }

    /// Start shared capture, build, fresh planning and image delivery.
    ///
    /// # Errors
    /// Rejects a closed session. Preparation failures arrive through `finished`.
    pub fn prepare(&self, input: PreparationInput) -> Result<RunningPreparation, RpcError> {
        self.prepare_with(input, std::collections::BTreeMap::new(), None)
    }

    /// [`Self::prepare`], pulling private images with `registry_auth`, which a
    /// lowering input never carries. A `runner` plans as that operator and blocks a
    /// Service whose Volume nothing can mount instead of refusing the Deploy; it
    /// removes first the Pending copies its claim's ended runs left. A
    /// failure reaches `finished` as `E`: the `RpcError` the SDK returns, or
    /// [`Unprepared`] for the store runner.
    pub(crate) fn prepare_with<E>(
        &self,
        input: PreparationInput,
        registry_auth: std::collections::BTreeMap<
            ployz_core::ServiceName,
            ployz_core::RegistryAuth,
        >,
        runner: Option<(
            ployz_core::Operator,
            std::collections::BTreeSet<ployz_core::DeploymentLogId>,
        )>,
    ) -> Result<Running<PreparedDeploy, E>, RpcError>
    where
        E: From<RpcError> + From<Unprepared> + Send + 'static,
    {
        let mut client = self.client()?;
        let token = self.inner.cancel.child_token();
        let session = Arc::downgrade(&self.inner);
        Ok(Running::spawn(token.clone(), move |reporter| async move {
            let mut captured = capture(input).await?;
            captured.intent.registry_auth = registry_auth;
            if let Some((operator, ended_runs)) = runner {
                captured.intent.options = captured.intent.options.as_runner(operator, ended_runs);
            }
            if token.is_cancelled() {
                return Err(E::from(preparation_error(
                    crate::sdk::prepare::PreparationError::Cancelled,
                    true,
                )));
            }
            let prepared = crate::sdk::prepare::prepare(
                &mut client,
                captured.intent,
                captured.build,
                &captured.reusable,
                captured.preference,
                &token,
                |progress| reporter.report(progress),
            )
            .await
            .map_err(|error| E::from(unprepared(error, token.is_cancelled())))?;
            let (preview, retained) = prepared.into_parts();
            let build_receipts =
                preparation::receipts(&captured.fingerprints, &captured.reused, &retained);
            let prune_targets = crate::image::prune_targets(&preview, &retained);
            Ok(PreparedDeploy {
                preview,
                build_receipts,
                session,
                confirmed: AtomicBool::new(false),
                retained: std::sync::Mutex::new(Some(retained)),
                prune_targets,
            })
        }))
    }

    /// Start one Image Build. `input` holds exactly one Git Service with its
    /// checkout and commit, or one uploaded Service with or without its source;
    /// its receipt, if any, is a reuse hint, and the only way to serve an upload
    /// that came without source. When
    /// `start_within` passes before a Build Machine admits the build, the build
    /// is withdrawn and `finished` reports [`BuildOutcome::Queued`]. An admitted
    /// build always runs to its end. The Machine's temporary image retention
    /// ends with the call; a later `prepare` reuses the image by digest.
    ///
    /// # Errors
    /// Rejects a closed session. Build failures arrive through `finished`.
    pub fn build(
        &self,
        input: PreparationInput,
        start_within: Option<std::time::Duration>,
    ) -> Result<RunningBuild, RpcError> {
        let client = self.client()?;
        let token = self.inner.cancel.child_token();
        Ok(Running::spawn(token.clone(), move |reporter| {
            build::run(client, input, start_within, token, reporter)
        }))
    }

    /// What a Builder outside the Cluster does for the one Git Service in
    /// `input.deployment` at `input.commit`: reuse `input.receipt` while a Machine
    /// still holds its image for every Machine the Service may run on, or build the
    /// Build Platform Requirement. Needs no checkout and never builds.
    ///
    /// # Errors
    /// Rejects a closed session or invalid deployment; returns transport errors,
    /// or `invalid_argument` naming a Machine no build platform runs.
    pub async fn outside_build(&self, input: OutsideBuildInput) -> Result<OutsideBuild, RpcError> {
        let client = self.client()?;
        let token = self.inner.cancel.child_token();
        build::outside(client, input, token).await
    }

    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Same planner, ingress expansion, and DNS warnings as the CLI. Confirming
    /// executes these operations; it does not re-plan.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, snapshot
    /// gathering fails, or planning fails.
    pub async fn preview(&self, intent: DeployIntent) -> Result<PreparedDeploy, RpcError> {
        self.plan(intent).await.map_err(RpcError::from)
    }

    /// [`Self::preview`], keeping whether a refusal would meet the same Deploy again.
    pub(crate) async fn plan(&self, intent: DeployIntent) -> Result<PreparedDeploy, Unprepared> {
        let mut client = self.client()?;
        let preview = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed().into()),
            preview = client.preview(intent) => preview?,
        };
        Ok(PreparedDeploy {
            preview,
            build_receipts: Default::default(),
            session: Arc::downgrade(&self.inner),
            confirmed: AtomicBool::new(false),
            retained: std::sync::Mutex::new(None),
            prune_targets: Vec::new(),
        })
    }

    /// Calculate a Namespace-removal preview. Confirming executes these operations.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, the Namespace
    /// is reserved, snapshot gathering fails, or planning fails.
    pub async fn preview_namespace_removal(
        &self,
        namespace: Namespace,
        volumes: VolumeFate,
    ) -> Result<PreparedDeploy, RpcError> {
        let mut client = self.client()?;
        let preview = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed()),
            preview = client.preview_namespace_removal(&namespace, volumes) => preview?,
        };
        Ok(PreparedDeploy {
            preview,
            build_receipts: Default::default(),
            session: Arc::downgrade(&self.inner),
            confirmed: AtomicBool::new(false),
            retained: std::sync::Mutex::new(None),
            prune_targets: Vec::new(),
        })
    }

    /// Preview, auto-confirm, and return the Deploy Outcome.
    ///
    /// Execution failure is a Deploy Outcome. Progress still streams on a
    /// [`RunningDeploy`] from [`PreparedDeploy::confirm`]; this method drains it.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when planning fails before any progress
    /// events, or when the session is closed.
    pub async fn run(
        &self,
        intent: DeployIntent,
        cancel: Option<&CancellationToken>,
    ) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        let prepared = self.preview(intent).await?;
        let running = prepared.confirm()?;
        if let Some(cancel) = cancel {
            let abort = running.cancel.clone();
            let watcher = cancel.clone();
            tokio::spawn(async move {
                watcher.cancelled().await;
                abort.cancel();
            });
        }
        running.finished().await
    }

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails. Already-absent Volumes count as successful removals;
    /// every failure or omission retains its Docker Volume identity.
    pub async fn remove_volumes(
        &self,
        request: RemoveVolumesRequest,
    ) -> Result<Vec<VolumeRemoval>, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.remove_volumes(request)).await
    }

    /// Which Machines hold each of the Docker Volumes `sought`, naming every Machine
    /// that did not answer.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails.
    pub async fn observe_volumes(
        &self,
        sought: Vec<ployz_core::DockerVolumeName>,
    ) -> Result<ployz_store::VolumeObservation, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.observe_volumes(sought)).await
    }

    /// Every Service running on a Machine that answered, by Namespace, naming every
    /// Machine that did not answer.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails.
    pub async fn observe_running(&self) -> Result<ployz_store::RunningObservation, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.observe_running()).await
    }

    /// Every copy of every Volume on every Machine, by role, naming every Machine that
    /// did not answer.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails.
    pub async fn observe_copies(&self) -> Result<CopyObservation, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.observe_copies()).await
    }

    /// Send one Volume switch request (`{command, payload}`) to `machine` and answer the
    /// reply payload. One-shot: the Machine's fence makes a resend safe, so the caller
    /// decides whether to retry.
    ///
    /// # Errors
    ///
    /// Returns `invalid_argument` when `request` is not one of the Volume switch
    /// commands or `machine` is not a Machine Target, and the Machine's [`RpcError`]
    /// otherwise, its `details` a `SwitchError` when the fence refused or a
    /// `SetVolumeCopyRefused` when SetVolumeCopy refused. A failure no Machine sent is
    /// `unanswered`: the Machine may have taken the request. One whose entry Machine
    /// could not dial `machine` is not, since `machine` never got it.
    pub async fn volume_switch(&self, machine: &str, request: Value) -> Result<Value, CallError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let body: RpcRequestBody = serde_json::from_value(request)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let path = volume_switch_path(&body)?;
        let request = ployz_core::RpcRequest::from(body);
        let client = self.client()?;
        let response = self
            .until_closed_answered(async {
                client
                    .call_raw(
                        &request,
                        path,
                        Call::once()
                            .to(&target)
                            .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                    )
                    .await
                    .map_err(CallError::from)
            })
            .await?;
        Ok(volume_switch_payload(
            request.body.command(),
            response.body,
        )?)
    }

    /// Live Observation of Data Loss that removing `machine` would cause.
    ///
    /// `machine` is a Machine Target. This is not a complete Cluster view.
    /// Mutates nothing: it is safe to call when the operator then cancels.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine`
    /// is not a Machine Target, the Machine is not visible or is ambiguous, or
    /// the Machine did not respond so Data Loss cannot be listed.
    pub async fn data_loss_if_machine_removed(
        &self,
        machine: &str,
    ) -> Result<ObservedDataLoss, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.data_loss_if_machine_removed(&target))
            .await
    }

    /// Remove `machine` after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` is derived from the Live Observation the caller
    /// showed a human. Re-reads Data Loss at execute time.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine`
    /// is not a Machine Target, the Machine is not visible or is the current
    /// entry while another Machine is visible, the Machine is the last one and a
    /// Management Client other than Cloud holds a key, the Machine did not respond so Data Loss cannot
    /// be listed, the confirmation does not cover the fresh Data Loss, or
    /// reset or shared-row removal fails. Unconfirmed names are in
    /// `UnconfirmedDataLoss` details.
    pub async fn remove_machine(
        &self,
        machine: &str,
        confirm_data_loss: &DataLossConfirmation,
    ) -> Result<LocalMachineRemoved, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.remove_machine(
            &target,
            confirm_data_loss,
            crate::cluster::Remover::Cloud,
        ))
        .await
    }

    /// Drain `machine` within `scope`: turn its services role off, retire its chosen
    /// Globals there, and move each chosen replicated Service off it, one at a time and
    /// start-first.
    ///
    /// Closing the session ends the Drain at its next safe point and still resolves: a
    /// move in flight finishes or removes its new Container again, and the Services not
    /// reached read `not_attempted`. That is why this is not wrapped in `until_closed`,
    /// which would drop the work mid-move and lose the report.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] before anything moved: the session is closed,
    /// `machine` is not a Machine Target or not visible (`not_found`, `ambiguous`), the
    /// services role can't be turned off or observed off within 30 s, or the Server's
    /// Services can't be observed.
    pub async fn drain_machine(
        &self,
        machine: &str,
        scope: &crate::drain::DrainScope,
    ) -> Result<crate::drain::DrainReport, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        client
            .drain(&target, scope, &self.inner.cancel, &mut |_| {})
            .await
            .map_err(RpcError::from)
    }

    /// Give every Participating Machine of a Cluster founded before Sealing Keys the one
    /// Sealing Key: observe each listed Machine, settle the key from its holders or ask
    /// `request.generator` for one, then carry it, sealed to each recipient's WireGuard
    /// key, from a holder to each Machine that needs access. Each Machine RPC is bounded
    /// and safe to repeat; rerun the whole verb after a `waiting` or a failed Machine.
    /// The report carries no ciphertext.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, or this entry's error
    /// when the Cluster's Machines cannot be listed. A Machine's refusal is in the report.
    pub async fn backfill_sealing_key(
        &self,
        request: BackfillSealingKeyRequest,
    ) -> Result<BackfillSealingKeyReport, RpcError> {
        let mut client = self.client()?;
        self.until_closed(crate::sealing_backfill::execute(&mut client, &request))
            .await
    }

    /// Start the Ingress Proxy on `machine` once its ingress role is on: wait until this
    /// entry observes the role, then create or start the proxy on every Server that holds
    /// it, never replacing one that runs. The repair for `machine` says why it didn't
    /// start; `unchecked` names Servers whose containers went unobserved.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// Machine Target or not visible, this entry hasn't observed the role within 30 s, or
    /// observation, image discovery or planning fails.
    pub async fn start_ingress(
        &self,
        machine: &str,
    ) -> Result<crate::ingress::IngressCheck, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        let cancel = &self.inner.cancel;
        self.until_closed(async {
            let id = visible_machine_id(&mut client, &target).await?;
            crate::cluster::wait_for_role(
                &mut client,
                &id,
                crate::cluster::RoleSetting::IngressOn,
                cancel,
            )
            .await
            .map_err(|error| match error {
                crate::cluster::RoleWaitError::Connect(error) => RpcError::from(error),
                error @ (crate::cluster::RoleWaitError::NotObserved(_)
                | crate::cluster::RoleWaitError::ChangeNotObserved
                | crate::cluster::RoleWaitError::Cancelled) => {
                    RpcError::unavailable(error.to_string())
                }
            })?;
            crate::ingress::fill_slots(&mut client, cancel)
                .await
                .map_err(|failure| failure.report())
        })
        .await
    }

    /// Open `doors` on every Server with the ingress role: `public`, `tunnel`, or both.
    /// A tunnel takes `token`, the Cloudflare tunnel token, sealed to the Cluster here;
    /// without one it keeps the token the Cluster already runs.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, a door name is
    /// unknown, the tunnel needs a token, no Server holds the Sealing Key, or
    /// observation fails. A Server's own refusal is one of the result's failures.
    pub async fn set_ingress_doors(
        &self,
        doors: &[String],
        token: Option<&str>,
    ) -> Result<crate::ingress::DoorsChanged, RpcError> {
        let doors = doors
            .iter()
            .map(|door| {
                serde_json::from_value::<ployz_core::IngressDoor>(Value::String(door.clone()))
                    .map_err(|_| {
                        RpcError::invalid_argument(format!(
                            "{door} is not a Door: public or tunnel"
                        ))
                    })
            })
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
        if doors.is_empty() {
            return Err(RpcError::invalid_argument(
                "name at least one Door: public or tunnel",
            ));
        }
        let doors = ployz_core::IngressDoors(doors);
        let mut client = self.client()?;
        let cancel = &self.inner.cancel;
        self.until_closed(async {
            let observed = crate::ingress::observe_doors(&mut client)
                .await
                .map_err(|failure| failure.report())?;
            let doors = crate::ingress::cluster_doors_for(&observed, doors, token)
                .map_err(|failure| failure.report())?;
            crate::ingress::use_doors(&mut client, observed, doors, cancel)
                .await
                .map_err(|failure| failure.report())
        })
        .await
    }

    /// Turn Private Access on or off on every Server: run the Tailscale router with the
    /// auth key `on` carries, sealed to the Cluster in the person's browser and approved
    /// by their passkey or Device, or remove it when `on` is `None`. Cloud never holds
    /// the key in the clear.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or observation fails.
    /// A Server's own refusal or a router that did not serve is one of the result's
    /// failures.
    pub async fn set_private_access(
        &self,
        on: Option<crate::private_access::PrivateAccessOn>,
    ) -> Result<crate::private_access::AccessChanged, RpcError> {
        let mut client = self.client()?;
        let cancel = &self.inner.cancel;
        self.until_closed(async {
            let observed = crate::private_access::observe_access(&mut client)
                .await
                .map_err(|failure| failure.report())?;
            Ok(match on {
                Some(on) => {
                    crate::private_access::turn_on(
                        &client,
                        observed.machines,
                        on.sealed_auth_key,
                        on.approval,
                        cancel,
                    )
                    .await
                }
                None => crate::private_access::turn_off(&client, observed.machines, cancel).await,
            })
        })
        .await
    }

    /// Stop serving web traffic on `machine` once its ingress role is off: wait until this
    /// entry observes the role, so nothing it plans next recreates the proxy, then stop and
    /// remove its Ingress Proxy. Nothing else stops it; the daemon never stops serving
    /// infrastructure on its own.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// Machine Target or not visible, this entry hasn't observed the role within 30 s, or
    /// the Machine does not acknowledge a listing, stop or removal.
    pub async fn stop_ingress(&self, machine: &str) -> Result<(), RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        let cancel = &self.inner.cancel;
        self.until_closed(async {
            let id = visible_machine_id(&mut client, &target).await?;
            crate::cluster::wait_for_role(
                &mut client,
                &id,
                crate::cluster::RoleSetting::IngressOff,
                cancel,
            )
            .await
            .map_err(|error| match error {
                crate::cluster::RoleWaitError::Connect(error) => RpcError::from(error),
                error @ (crate::cluster::RoleWaitError::NotObserved(_)
                | crate::cluster::RoleWaitError::ChangeNotObserved
                | crate::cluster::RoleWaitError::Cancelled) => {
                    RpcError::unavailable(error.to_string())
                }
            })?;
            crate::drain::retirement::retire_ingress_proxy(&client, &id, cancel).await
        })
        .await
    }

    /// Take `machine` out of the Cluster without resetting it: it keeps its state and
    /// its keys, Cloud's included. The last Machine isn't taken out this way.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine` is not a
    /// Machine Target or not visible, it is the last Machine and a Management Client
    /// holds a key or its holders can't be read, or shared-row removal fails.
    pub async fn remove_machine_membership(&self, machine: &str) -> Result<(), RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.remove_machine_membership(&target))
            .await
    }

    /// Container `container` on `machine` as its daemon holds it: the resolved spec with its
    /// real environment values, which replicated observations redact.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, the Machine Target or
    /// Container ID is invalid, or the Machine does not know `container`.
    pub async fn inspect_container(
        &self,
        machine: &str,
        container: &str,
    ) -> Result<Value, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let container_id = ployz_core::ContainerId::parse(container)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let client = self.client()?;
        let details = self
            .until_closed(async {
                client
                    .call_shared::<ployz_core::op::InspectContainer>(
                        ployz_core::InspectContainerRequest { container_id },
                        Call::once()
                            .to(&target)
                            .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                    )
                    .await
                    .map_err(crate::connect::rpc_error)
            })
            .await?;
        serde_json::to_value(details)
            .map_err(|error| RpcError::internal(format!("encoding Container {container}: {error}")))
    }

    /// Copy the image Container `container` runs on `source` to `dest`, by its local image
    /// ID and tagged as its spec names it. Never asks a registry, and does nothing when
    /// `dest` already holds that image.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, a Machine Target is
    /// invalid or not visible, `container` is not a Container ID or not on `source`, the
    /// source daemon reports no image ID, or the copy fails.
    pub async fn copy_container_image(
        &self,
        source: &str,
        container: &str,
        dest: &str,
    ) -> Result<(), RpcError> {
        let parse = |machine: &str| {
            MachineTarget::parse(machine)
                .map_err(|error| RpcError::invalid_argument(error.to_string()))
        };
        let (source, dest) = (parse(source)?, parse(dest)?);
        let container_id = ployz_core::ContainerId::parse(container)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(async {
            let machines = client.machines().await.map_err(RpcError::from)?;
            let source = crate::cluster::visible_machine(&source, &machines)?
                .machine
                .clone();
            let dest = crate::cluster::visible_machine(&dest, &machines)?
                .machine
                .clone();
            let details = client
                .call_shared::<ployz_core::op::InspectContainer>(
                    ployz_core::InspectContainerRequest { container_id },
                    Call::once()
                        .to(&MachineTarget::from(&source.id))
                        .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                )
                .await
                .map_err(crate::connect::rpc_error)?;
            let image_id = details.image_id.ok_or_else(|| {
                RpcError::unsupported(format!(
                    "{} does not report the image ID its Containers run",
                    source.name
                ))
            })?;
            let image = &details.container.resolved_spec.container.image;
            crate::image::copy_running_image(&client, &source, &dest, image, &image_id)
                .await
                .map_err(crate::failure::push_rpc_error)
        })
        .await
    }

    /// Apply one Machine policy edit (Machine Roles and build concurrency) to `machine` and return its updated record.
    ///
    /// One-shot: a lost response is read back from observation, never replayed.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine`
    /// is not a Machine Target, the update is empty or illegal, or the Machine
    /// does not respond.
    pub async fn update_machine(
        &self,
        machine: &str,
        update: ployz_core::MachineUpdate,
    ) -> Result<ployz_core::MachineUpdated, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let client = self.client()?;
        self.until_closed(async {
            client
                .call_shared::<op::UpdateMachine>(
                    ployz_core::UpdateMachineRequest { update },
                    Call::once()
                        .to(&target)
                        .within(Deadline::Target(crate::connect::TARGET_RPC_TIMEOUT)),
                )
                .await
                .map_err(crate::connect::rpc_error)
        })
        .await
    }

    /// Ask `machine` to Upgrade to `request.release`. Repeating the request with the same attempt ID
    /// returns that attempt, so resending it is the poll; a different one while an Upgrade or
    /// mutation runs is refused as `conflict`. The daemon restarts during the Upgrade, so a caller
    /// polling for the outcome keeps polling through `unavailable`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine`
    /// is not a Machine Target, the Machine refuses the request, or it does not
    /// answer.
    pub async fn request_machine_upgrade(
        &self,
        machine: &str,
        request: ployz_core::RequestMachineUpgradeRequest,
    ) -> Result<ployz_core::MachineUpgradeAttempt, RpcError> {
        let target = MachineTarget::parse(machine)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(async {
            client
                .call::<op::RequestMachineUpgrade>(
                    request,
                    Call::keyed()
                        .to(&target)
                        .budget(crate::connect::target_read_timeout()),
                )
                .await
                .map_err(crate::connect::rpc_error)
        })
        .await
    }

    /// Live Observation of Data Loss that destroying `namespace` would cause.
    ///
    /// [`VolumeFate::Preserve`] yields an empty list. Mutates nothing.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `namespace`
    /// is not a Namespace or is reserved, snapshot gathering fails, or
    /// destroying volumes is requested against a known incomplete snapshot.
    pub async fn data_loss_if_namespace_destroyed(
        &self,
        namespace: &str,
        volumes: VolumeFate,
    ) -> Result<ObservedDataLoss, RpcError> {
        let namespace = Namespace::parse(namespace)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.data_loss_if_namespace_destroyed(&namespace, volumes))
            .await
    }

    /// Destroy `namespace` after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` is derived from the Live Observation the caller
    /// showed a human. Confirmed identities that disappeared are ignored, so
    /// one confirmation can cover several Namespaces. Re-reads Data Loss at
    /// execute time. [`VolumeFate::Preserve`] is the non-destructive default.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `namespace`
    /// is not a Namespace or is reserved, the Namespace is not visible, the
    /// snapshot is incomplete, or the confirmation does not cover the fresh
    /// Data Loss. Unconfirmed names are in `UnconfirmedDataLoss` details.
    /// Execution failure is a [`DeployOutcome::Failed`].
    pub async fn destroy_namespace(
        &self,
        namespace: &str,
        confirm_data_loss: &DataLossConfirmation,
        volumes: VolumeFate,
    ) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        let namespace = Namespace::parse(namespace)
            .map_err(|error| RpcError::invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.destroy_namespace(
            &namespace,
            confirm_data_loss,
            volumes,
            &self.inner.cancel,
            None,
        ))
        .await
    }

    /// Live Observation of Data Loss that destroying this Cluster would cause.
    ///
    /// Unions Docker Volumes across every visible Namespace and Machine. Mutates
    /// nothing: it is safe to call when the operator then cancels.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails.
    pub async fn data_loss_if_cluster_destroyed(&self) -> Result<ObservedDataLoss, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.data_loss_if_cluster_destroyed())
            .await
    }

    /// Destroy this Cluster after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` is derived from the Live Observation the caller
    /// showed a human. Re-reads Data Loss at execute time. Confirmed identities
    /// that disappeared are ignored.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or the
    /// confirmation does not cover the fresh Data Loss. Unconfirmed names are
    /// in `UnconfirmedDataLoss` details. Unreachable Machines stay on the
    /// returned [`ClusterTeardown`].
    pub async fn destroy_cluster(
        &self,
        confirm_data_loss: &DataLossConfirmation,
    ) -> Result<ClusterTeardown, RpcError> {
        let mut client = self.client()?;
        self.until_closed(client.destroy_cluster(confirm_data_loss, &self.inner.cancel))
            .await
    }

    /// Drop the Client and transport session. Aborts in-flight Watch and Deploy.
    ///
    /// Repeated calls are a no-op.
    pub async fn close(&self) {
        self.inner.cancel.cancel();
        self.inner
            .client
            .lock()
            .expect("session client lock")
            .take();
    }
}

impl std::fmt::Debug for PreparedDeploy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedDeploy")
            .field("noop", &self.preview.noop())
            .field("operations", &self.preview.operations.len())
            .finish_non_exhaustive()
    }
}

impl Drop for SessionInner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Watch {
    /// Next complete frame, or `None` if this stream was cancelled.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the daemon, store, or RPC fails,
    /// including when the stream ends without cancellation. Once the client that opened
    /// it has ended, the refusal instead of any frame.
    pub async fn next(&self) -> Result<Option<RuntimeWatchFrame>, RpcError> {
        if self.cancel.is_cancelled() {
            return Ok(None);
        }
        let mut guard = self.stream.lock().await;
        self.ended(&mut guard)?;
        let message = {
            let Some(stream) = guard.as_mut() else {
                return Ok(None);
            };
            tokio::select! {
                () = self.cancel.cancelled() => None,
                message = stream.message() => Some(message),
            }
        };
        if message.is_some() {
            self.ended(&mut guard)?;
        }
        match message {
            None => {
                *guard = None;
                Ok(None)
            }
            Some(Ok(None)) => {
                *guard = None;
                if self.cancel.is_cancelled() {
                    Ok(None)
                } else {
                    Err(RpcError::unavailable(
                        "Watch stream ended; reconnect to resume",
                    ))
                }
            }
            Some(Ok(Some(payload))) => match decode_runtime_watch_frame(&payload) {
                Ok(mut frame) => {
                    let Some(inner) = self.session.upgrade() else {
                        return Ok(None);
                    };
                    let client = Session { inner }.client()?;
                    tokio::select! {
                        () = self.cancel.cancelled() => {
                            *guard = None;
                            Ok(None)
                        }
                        _ = client.observe_machine_storage(&mut frame.machines) => {
                            self.ended(&mut guard)?;
                            Ok(Some(frame))
                        }
                    }
                }
                Err(error) => {
                    *guard = None;
                    Err(crate::ui::rpc_error(RpcErrorCode::Internal, &error))
                }
            },
            Some(Err(_)) if self.cancel.is_cancelled() => {
                *guard = None;
                Ok(None)
            }
            Some(Err(status)) => {
                *guard = None;
                Err(RpcError::from(ConnectError::Rpc(
                    TransportError::from_stream_status(status),
                )))
            }
        }
    }

    /// Fails with the refusal once the client that opened this Watch has ended, and drops
    /// its stream.
    fn ended(&self, stream: &mut Option<tonic::Streaming<OpaquePayload>>) -> Result<(), RpcError> {
        self.refused.check().map_err(|refused| {
            *stream = None;
            RpcError::from(refused)
        })
    }

    /// End this Watch stream. The Client stays usable.
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Ok(mut guard) = self.stream.try_lock() {
            *guard = None;
        }
    }
}

fn closed() -> RpcError {
    RpcError::unavailable("client is closed")
}

/// The ID of the one visible Machine `target` names.
async fn visible_machine_id(
    client: &mut Client,
    target: &MachineTarget,
) -> Result<ployz_core::MachineId, RpcError> {
    let machines = client.machines().await?;
    Ok(crate::cluster::visible_machine(target, &machines)?
        .machine
        .id)
}

/// Capture checkouts off the async runtime.
async fn capture(input: PreparationInput) -> Result<preparation::CapturedPreparation, RpcError> {
    tokio::task::spawn_blocking(move || preparation::capture(input))
        .await
        .map_err(|_| RpcError::invalid_argument("source capture task failed"))?
}

pub(crate) fn preparation_error(
    error: crate::sdk::prepare::PreparationError,
    cancellation_requested: bool,
) -> RpcError {
    use crate::sdk::prepare::PreparationError;
    let message = crate::ui::rpc_error(RpcErrorCode::Internal, &error).message;
    let same_again = error.same_again();
    match error {
        PreparationError::Selection(error) => {
            let details = if let ConnectError::Remote(error) = &error {
                &error.details
            } else {
                &Value::Null
            };
            let rejections = details.get("rejections").cloned().unwrap_or(Value::Null);
            let reasons = details
                .get("reasons")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let message =
                "No eligible Build Machine was selected; no build was started.".to_owned();
            RpcError::unavailable(message.clone())
                .with_details(
                    serde_json::json!({"preparation":{"kind":"failed", "stage":"Selection",
                    "message":message, "rejections":rejections}}),
                )
                .with_causes(if reasons.is_empty() {
                    Vec::new()
                } else {
                    vec![reasons.join("; ")]
                })
        }
        PreparationError::Connect(_) => {
            RpcError::unavailable("Could not read Machine observations during preparation.")
                .with_details(
                    serde_json::json!({"preparation":{"kind":"failed", "stage":"Observation",
                "message":"Could not read Machine observations during preparation."}}),
                )
        }
        PreparationError::Build(crate::build::Error::RemoteBuild { outcome }) => {
            let cancelled = cancellation_requested
                && matches!(*outcome, crate::build::RemoteBuildFailure::Failed { .. });
            let mut details = serde_json::json!({"preparation": outcome});
            // Failed confirms termination; a cancellation request alone cannot erase Unknown.
            if cancelled {
                *details
                    .pointer_mut("/preparation/kind")
                    .expect("remote Build failures serialize a kind") =
                    serde_json::json!("cancelled");
            }
            RpcError::internal(message).with_details(details)
        }
        PreparationError::UploadNeeded(services) => preparation::upload_needed(&services),
        PreparationError::Cancelled => RpcError::unavailable(message)
            .with_details(serde_json::json!({"preparation":{"kind":"cancelled"}})),
        PreparationError::Build(_) | PreparationError::Plan(_) | PreparationError::Delivery(_) => {
            let mut details = serde_json::Map::new();
            details.insert(
                "preparation".into(),
                serde_json::json!({"kind":"failed", "message":message}),
            );
            if same_again {
                details.insert("same_again".into(), Value::Bool(true));
            }
            RpcError::internal(message).with_details(Value::Object(details))
        }
    }
}

/// The mock Machine RPC the SDK's flow tests share with `tests/deploy_client`.
#[cfg(test)]
#[path = "../tests/deploy_client/support.rs"]
#[allow(dead_code)]
mod test_support;

#[cfg(test)]
#[path = "sdk_tests.rs"]
mod preparation_tests;

#[cfg(test)]
#[path = "sdk/volume_switch_tests.rs"]
mod volume_switch_tests;

#[cfg(test)]
#[path = "sdk/direct_apply_tests.rs"]
mod direct_apply_tests;
