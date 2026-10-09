//! Native Cloud session: connect, observe_enrollment, register,
//! about, publish_certificate_material, runtime.watch, prepare, build, preview, run,
//! preview_namespace_removal, remove_volumes, Data Loss for Machine, Namespace, and
//! Cluster destroy, remove_machine, drain_machine, request and inspect a Machine
//! Upgrade, destroy_namespace, destroy_cluster, and close.
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, Connector, TransportError, connect_selected_with};
use crate::context::{Connection, ConnectionSource, SelectedConnections};
use crate::deploy::{DeployIntent, DeployPlan, VolumeFate};
use ployz_core::{
    CertificateMaterialPublished, ClusterTeardown, ContractDescription, DataLossConfirmation,
    DeployEvent, DeployOutcome, DescribeContractRequest, EnrollmentAssignment, EnrollmentSnapshot,
    ExecutionError, LocalMachineRemoved, MachineTarget, Namespace, ObservedDataLoss, OpaquePayload,
    PublishCertificateMaterialRequest, RUNTIME_WATCH_CAPABILITY, Registered, RemoveVolumesRequest,
    Rpc, RpcError, RpcErrorCode, RpcRequestBody, RpcResponseBody, RuntimeWatchFrame,
    RuntimeWatchRequest, ServiceObservation, VolumeRemoval, decode_runtime_watch_frame, op,
};

pub use payloads::typescript_declarations;

mod build;
mod deploy;
mod github_build;
mod logs;
mod payloads;
pub(crate) mod preparation;
pub(crate) mod prepare;
mod running;
mod store_call;
mod store_runner;
pub use build::{BuildOutcome, OutsideBuild};
pub use deploy::ImageCleanup;
pub use github_build::{
    GithubCheckIn, GithubFinish, GithubReported, GithubStart, github_cancel, github_check_in,
    github_finish, github_report, github_start,
};
pub use running::Running;
pub use store_call::store_call;
pub use store_runner::{Sources, observe_copies, observe_volumes, run_deployment};

/// Cancellable preparation whose progress is retained until read, within a byte budget.
pub type RunningPreparation = Running<PreparedDeploy>;

/// Cancellable Image Build whose progress is retained until read, within a byte budget.
pub type RunningBuild = Running<BuildOutcome>;
pub use logs::{ContainerLogInput, ContainerLogRecord, ContainerLogStream};
pub use preparation::{
    BuildReceipt, BuildVariables, OutsideBuildInput, PreparationInput, UploadDigest, VERSION,
    expected_fingerprints,
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
        | Body::DeclareMirror(_)
        | Body::BeginRound(_)
        | Body::CommitSnapshots(_)
        | Body::WarmSnapshot(_)
        | Body::StartReceive(_)
        | Body::InspectReceive(_)
        | Body::PruneMirror(_)
        | Body::DestroyMirror(_)
        | Body::ForgetSnapshots(_)
        | Body::ForgetLease(_)
        | Body::DemoteVolume(_)
        | Body::Withdraw(_)
        | Body::Freeze(_)
        | Body::HandOver(_)
        | Body::Thaw(_)
        | Body::Close(_)
        | Body::AcceptHandOff(_)
        | Body::Promote(_)
        | Body::StartHandedContainer(_)
        | Body::ClearFinal(_)
        | Body::Restore(_) => body
            .unary_path()
            .ok_or_else(|| invalid_argument(format!("{} is not a unary RPC", body.command()))),
        _ => Err(invalid_argument(format!(
            "{} is not a Volume switch command",
            body.command()
        ))),
    }
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

/// Select the first confirmed connection before any operation is dispatched.
///
/// # Errors
/// Returns connection/identity failures; an empty connection list is invalid.
pub async fn connect_connections(
    connections: Vec<Connection>,
    connector: Arc<dyn Connector>,
) -> Result<Session, RpcError> {
    if connections.is_empty() {
        return Err(invalid_argument("connections must not be empty".into()));
    }
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections,
        },
        connector,
    )
    .await?;
    Ok(Session {
        inner: Arc::new(SessionInner {
            client: std::sync::Mutex::new(Some(client)),
            cancel: CancellationToken::new(),
        }),
    })
}

impl Session {
    fn client(&self) -> Result<Client, RpcError> {
        self.inner
            .client
            .lock()
            .expect("session client lock")
            .as_ref()
            .ok_or_else(closed)
            .cloned()
    }

    async fn until_closed<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, RpcError>>,
    ) -> Result<T, RpcError> {
        tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => Err(closed()),
            result = work => result,
        }
    }

    async fn unary<T: Rpc>(&self, request: T::Request) -> Result<T::Response, RpcError> {
        let mut client = self.client()?;
        self.until_closed(async {
            client
                .call::<T>(request, None)
                .await
                .map_err(RpcError::from)
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
            () = self.inner.cancel.cancelled() => Err(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "session closed; in-flight Register outcome may be uncertain".into(),
                details: Value::Null, cause: Vec::new(),
            }),
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
                .call_unretried::<op::SetManagementClient>(
                    ployz_core::SetManagementClientRequest::Clear { label },
                    None,
                    crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
                )
                .await
                .map(|_| ())
                .map_err(RpcError::from)
        })
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
    ) -> Result<ployz_core::ManagementCapability, RpcError> {
        let response = self
            .unary::<op::SetManagementClient>(ployz_core::SetManagementClientRequest::Set { label })
            .await?;
        response.capability.ok_or_else(|| RpcError {
            code: RpcErrorCode::Internal,
            message: "Machine set a Management Client without a Management Capability".into(),
            details: Value::Null,
            cause: Vec::new(),
        })
    }

    /// Inspect the selected Machine, including its Management Client labels.
    ///
    /// # Errors
    /// Returns cancellation or Inspect errors.
    pub async fn inspect(&self) -> Result<ployz_core::MachineDetails, RpcError> {
        self.unary::<op::Inspect>(ployz_core::InspectRequest::default())
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
        self.unary::<op::PublishCertificateMaterial>(request).await
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
                .call_unretried::<op::MintBuildGrant>(
                    request,
                    None,
                    crate::cluster::LIVE_MUTATION_REPLY_TIMEOUT,
                )
                .await
                .map_err(RpcError::from)
        })
        .await
    }

    /// End a Build Grant on the entry Machine and read what it received. Idempotent.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or `not_found` once the grant expired
    /// or the Machine restarted.
    pub async fn end_build_grant(
        &self,
        request: ployz_core::EndBuildGrantRequest,
    ) -> Result<ployz_core::BuildGrantEnded, RpcError> {
        self.unary::<op::EndBuildGrant>(request).await
    }

    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or
    /// `DescribeContract` fails.
    pub async fn about(&self) -> Result<ContractDescription, RpcError> {
        self.unary::<op::DescribeContract>(DescribeContractRequest {})
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
        if !description.supports(RUNTIME_WATCH_CAPABILITY) {
            return Err(RpcError {
                code: RpcErrorCode::Unsupported,
                message: format!("{RUNTIME_WATCH_CAPABILITY} is not advertised"),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
        let payload = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
            .encode()
            .map_err(ConnectError::from)?;
        let stream = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed()),
            stream = client.runtime_watch_stream(payload) => stream.map_err(ConnectError::Rpc)?,
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
        })
    }

    /// Start shared capture, build, fresh planning and image delivery.
    ///
    /// # Errors
    /// Rejects a closed session. Preparation failures arrive through `finished`.
    pub fn prepare(&self, input: PreparationInput) -> Result<RunningPreparation, RpcError> {
        self.prepare_with(input, std::collections::BTreeMap::new())
    }

    /// [`Self::prepare`], pulling private images with `registry_auth`, which a
    /// lowering input never carries.
    pub(crate) fn prepare_with(
        &self,
        input: PreparationInput,
        registry_auth: std::collections::BTreeMap<
            ployz_core::ServiceName,
            ployz_core::RegistryAuth,
        >,
    ) -> Result<RunningPreparation, RpcError> {
        let mut client = self.client()?;
        let token = self.inner.cancel.child_token();
        let session = Arc::downgrade(&self.inner);
        Ok(Running::spawn(token.clone(), move |reporter| async move {
            let mut captured = capture(input).await?;
            captured.intent.registry_auth = registry_auth;
            if token.is_cancelled() {
                return Err(preparation_error(
                    crate::sdk::prepare::PreparationError::Cancelled,
                    true,
                ));
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
            .map_err(|error| preparation_error(error, token.is_cancelled()))?;
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
        let mut client = self.client()?;
        let preview = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed()),
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
    /// otherwise, its `details` a `SwitchError` when the fence refused.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "the switch verbs answer with three reply kinds; any other kind is a Machine fault"
    )]
    pub async fn volume_switch(&self, machine: &str, request: Value) -> Result<Value, RpcError> {
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let body: RpcRequestBody =
            serde_json::from_value(request).map_err(|error| invalid_argument(error.to_string()))?;
        let path = volume_switch_path(&body)?;
        let request = ployz_core::RpcRequest::from(body);
        let client = self.client()?;
        let response = self
            .until_closed(client.invoke_raw(
                &request,
                path,
                &target,
                Some(crate::connect::TARGET_RPC_TIMEOUT),
            ))
            .await?;
        let payload = match response.body {
            RpcResponseBody::SwitchReply(reply) => serde_json::to_value(reply),
            RpcResponseBody::VolumeCopyView(view) => serde_json::to_value(view),
            RpcResponseBody::ReceiveView(view) => serde_json::to_value(view),
            RpcResponseBody::Error(error) => return Err(error),
            other => {
                return Err(RpcError {
                    code: RpcErrorCode::Internal,
                    message: format!(
                        "Machine answered {} with {}",
                        request.body.command(),
                        other.kind().as_str()
                    ),
                    details: Value::Null,
                    cause: Vec::new(),
                });
            }
        };
        payload.map_err(|error| RpcError {
            code: RpcErrorCode::Internal,
            message: "Volume switch reply could not be encoded".into(),
            details: Value::Null,
            cause: vec![error.to_string()],
        })
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        client
            .drain(&target, scope, &self.inner.cancel, &mut |_| {})
            .await
            .map_err(RpcError::from)
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let container_id = ployz_core::ContainerId::parse(container)
            .map_err(|error| invalid_argument(error.to_string()))?;
        let client = self.client()?;
        let details = self
            .until_closed(client.invoke::<ployz_core::op::InspectContainer>(
                ployz_core::InspectContainerRequest { container_id },
                &target,
                Some(crate::connect::TARGET_RPC_TIMEOUT),
            ))
            .await?;
        serde_json::to_value(details).map_err(|error| RpcError {
            code: RpcErrorCode::Internal,
            message: format!("encoding Container {container}: {error}"),
            details: Value::Null,
            cause: Vec::new(),
        })
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
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))
        };
        let (source, dest) = (parse(source)?, parse(dest)?);
        let container_id = ployz_core::ContainerId::parse(container)
            .map_err(|error| invalid_argument(error.to_string()))?;
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
                .invoke::<ployz_core::op::InspectContainer>(
                    ployz_core::InspectContainerRequest { container_id },
                    &MachineTarget::from(&source.id),
                    Some(crate::connect::TARGET_RPC_TIMEOUT),
                )
                .await?;
            let image_id = details.image_id.ok_or_else(|| RpcError {
                code: RpcErrorCode::Unsupported,
                message: format!(
                    "{} does not report the image ID its Containers run",
                    source.name
                ),
                details: Value::Null,
                cause: Vec::new(),
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
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let client = self.client()?;
        self.until_closed(client.invoke::<op::UpdateMachine>(
            ployz_core::UpdateMachineRequest { update },
            &target,
            Some(crate::connect::TARGET_RPC_TIMEOUT),
        ))
        .await
    }

    /// Ask `machine` to Upgrade to `request.release`. Repeating the request with the same attempt ID
    /// returns that attempt; a different one while an Upgrade or mutation runs is refused as `conflict`.
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
        self.repeatable::<op::RequestMachineUpgrade>(machine, request)
            .await
    }

    /// Read `machine`'s Upgrade attempt. Its daemon restarts during the Upgrade, so a caller
    /// polling for the outcome keeps polling through `unavailable`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `machine`
    /// is not a Machine Target, the attempt is not the Machine's latest
    /// (`not_found`), or the Machine does not answer.
    pub async fn inspect_machine_upgrade(
        &self,
        machine: &str,
        request: ployz_core::InspectMachineUpgradeRequest,
    ) -> Result<ployz_core::MachineUpgradeAttempt, RpcError> {
        self.repeatable::<op::InspectMachineUpgrade>(machine, request)
            .await
    }

    /// A call on `machine` that is safe to repeat, retried across dropped connections for
    /// [`crate::connect::TARGET_RPC_TIMEOUT`]. Unlike the CLI's retries it never writes to stderr.
    async fn repeatable<T: Rpc>(
        &self,
        machine: &str,
        request: T::Request,
    ) -> Result<T::Response, RpcError> {
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let mut client = self.client()?;
        self.until_closed(client.read::<T>(request, &target)).await
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
        let namespace =
            Namespace::parse(namespace).map_err(|error| invalid_argument(error.to_string()))?;
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
        let namespace =
            Namespace::parse(namespace).map_err(|error| invalid_argument(error.to_string()))?;
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
    /// including when the stream ends without cancellation.
    pub async fn next(&self) -> Result<Option<RuntimeWatchFrame>, RpcError> {
        if self.cancel.is_cancelled() {
            return Ok(None);
        }
        let mut guard = self.stream.lock().await;
        let message = {
            let Some(stream) = guard.as_mut() else {
                return Ok(None);
            };
            tokio::select! {
                () = self.cancel.cancelled() => None,
                message = stream.message() => Some(message),
            }
        };
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
                    Err(RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "Watch stream ended; reconnect to resume".into(),
                        details: Value::Null,
                        cause: Vec::new(),
                    })
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

    /// End this Watch stream. The Client stays usable.
    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Ok(mut guard) = self.stream.try_lock() {
            *guard = None;
        }
    }
}

fn closed() -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: "client is closed".into(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

fn invalid_argument(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message,
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// Capture checkouts off the async runtime.
async fn capture(input: PreparationInput) -> Result<preparation::CapturedPreparation, RpcError> {
    tokio::task::spawn_blocking(move || preparation::capture(input))
        .await
        .map_err(|_| invalid_argument("source capture task failed".into()))?
}

pub(crate) fn preparation_error(
    error: crate::sdk::prepare::PreparationError,
    cancellation_requested: bool,
) -> RpcError {
    use crate::sdk::prepare::PreparationError;
    let message = crate::ui::rpc_error(RpcErrorCode::Internal, &error).message;
    match error {
        PreparationError::Selection(error) => {
            let rejections = if let ConnectError::Remote(error) = &error {
                error
                    .details
                    .get("rejections")
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            RpcError {
                code: RpcErrorCode::Unavailable,
                message: "No eligible Build Machine was selected; no build was started.".into(),
                details: serde_json::json!({"preparation":{"kind":"failed", "stage":"Selection",
                    "message":"No eligible Build Machine was selected; no build was started.", "rejections":rejections}}),
                cause: Vec::new(),
            }
        }
        PreparationError::Connect(_) => RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Could not read Machine observations during preparation.".into(),
            details: serde_json::json!({"preparation":{"kind":"failed", "stage":"Observation",
                "message":"Could not read Machine observations during preparation."}}),
            cause: Vec::new(),
        },
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
            RpcError {
                code: RpcErrorCode::Internal,
                message,
                details,
                cause: Vec::new(),
            }
        }
        PreparationError::UploadNeeded(services) => preparation::upload_needed(&services),
        PreparationError::Cancelled => RpcError {
            code: RpcErrorCode::Unavailable,
            message,
            details: serde_json::json!({"preparation":{"kind":"cancelled"}}),
            cause: Vec::new(),
        },
        PreparationError::Build(_) | PreparationError::Plan(_) | PreparationError::Delivery(_) => {
            RpcError {
                code: RpcErrorCode::Internal,
                details: serde_json::json!({"preparation":{"kind":"failed", "message":message}}),
                message,
                cause: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
#[path = "sdk_tests.rs"]
mod preparation_tests;

#[cfg(test)]
#[path = "sdk/volume_switch_tests.rs"]
mod volume_switch_tests;
