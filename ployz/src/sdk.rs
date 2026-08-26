//! Relay-only Cloud session: connect, list_held, register, revoke_pairing,
//! about, runtime.watch, preview, run, preview_project_removal, remove_volumes,
//! Data Loss for Machine, Project, and Cluster destroy, remove_machine,
//! destroy_project, destroy_cluster, and close.
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::connect::{
    Client, ConnectError, DialCredential, HeldRegister, PairingCredential, connect_relay,
    list_held as list_held_relay, revoke_cloud_pairing,
};
use crate::deploy::{DeployIntent, DeployPreview, VolumeFate};
use ployz_core::{
    ClusterTeardown, ContractDescription, DataLossConfirmation, DeployEvent, DeployOutcome,
    DescribeContractRequest, DockerVolumeName, ExecutionError, LocalMachineRemoved, MachineId,
    MachineTarget, ObservedDataLoss, OpaquePayload, PartialResult, ProjectName,
    RUNTIME_WATCH_CAPABILITY, RegisterRequest, Registered, RemoveVolumesRequest, RpcError,
    RpcErrorCode, RuntimeWatchFrame, RuntimeWatchRequest, decode_runtime_watch_frame, op,
};

struct SessionInner {
    client: Mutex<Option<Client>>,
    cancel: CancellationToken,
}

/// Connected Cloud session over one Relay Attach.
#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

/// Complete Runtime Watch frames from the entry Machine.
///
/// Drop or [`cancel`](Self::cancel) ends this stream only. The Client stays usable.
pub struct Watch {
    cancel: CancellationToken,
    client: Client,
    stream: Mutex<Option<tonic::Streaming<OpaquePayload>>>,
}

/// A planned Deploy that has not executed. [`Self::confirm`] runs these operations.
pub struct PreparedDeploy {
    preview: DeployPreview,
    client: Client,
    session_cancel: CancellationToken,
    confirmed: AtomicBool,
}

/// In-flight execution of one Deploy Preview.
pub struct RunningDeploy {
    cancel: CancellationToken,
    events: Mutex<Option<mpsc::UnboundedReceiver<DeployEvent>>>,
    join: Mutex<Option<tokio::task::JoinHandle<DeployOutcome<ExecutionError>>>>,
}

/// Open a Machine RPC channel through Cloud Relay.
///
/// Succeeds only after Relay Dial and Machine Attach produce a usable RPC
/// channel. Does not mint Attach credentials, perform Cloud Pairing, or choose
/// an entry Machine.
///
/// # Errors
///
/// Returns a generated [`RpcError`] when the bearer, pairing, or Machine ID is
/// rejected, or when the Relay or inner RPC channel fails.
pub async fn connect(
    relay_url: &str,
    bearer: &str,
    pairing: &str,
    machine_id: &str,
) -> Result<Session, RpcError> {
    let credential = parse_dial(bearer)?;
    let pairing = parse_pairing(pairing)?;
    let machine_id = MachineId::parse(machine_id).map_err(|error| RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: error.to_string(),
        details: Value::Null,
    })?;
    let client = connect_relay(relay_url, credential, pairing, machine_id).await?;
    Ok(Session {
        inner: Arc::new(SessionInner {
            client: Mutex::new(Some(client)),
            cancel: CancellationToken::new(),
        }),
    })
}

/// Dial a held Machine, send Machine RPC Register, then close.
///
/// Same Dial tuple as [`connect`]. Callers never see the session.
///
/// # Errors
///
/// Returns a generated [`RpcError`] when the bearer, pairing, or Machine ID is
/// rejected, when the Relay or inner RPC channel fails, or when Machine RPC
/// Register fails.
pub async fn register(
    relay_url: &str,
    bearer: &str,
    pairing: &str,
    machine_id: &str,
    identity: RegisterRequest,
) -> Result<Registered, RpcError> {
    let session = connect(relay_url, bearer, pairing, machine_id).await?;
    let result = async {
        let mut client = session.client().await?;
        client
            .call::<op::Register>(identity, None)
            .await
            .map_err(RpcError::from)
    }
    .await;
    session.close().await;
    result
}

/// List Machines currently holding Register for this pairing.
///
/// # Errors
///
/// Returns a generated [`RpcError`] when the bearer or pairing is rejected, or
/// when the Relay call fails.
pub async fn list_held(
    relay_url: &str,
    bearer: &str,
    pairing: &str,
) -> Result<Vec<HeldRegister>, RpcError> {
    let credential = parse_dial(bearer)?;
    let pairing = parse_pairing(pairing)?;
    list_held_relay(relay_url, &credential, &pairing)
        .await
        .map_err(RpcError::from)
}

/// Revoke a Pairing Credential so later Register with that bearer fails.
///
/// # Errors
///
/// Returns a generated [`RpcError`] when the bearer or pairing is rejected, or
/// when the Relay call fails.
pub async fn revoke_pairing(relay_url: &str, bearer: &str, pairing: &str) -> Result<(), RpcError> {
    let credential = parse_dial(bearer)?;
    let pairing = parse_pairing(pairing)?;
    revoke_cloud_pairing(relay_url, &credential, &pairing)
        .await
        .map_err(RpcError::from)
}

fn parse_dial(bearer: &str) -> Result<DialCredential, RpcError> {
    DialCredential::parse(bearer).map_err(|error| RpcError {
        code: RpcErrorCode::Unauthenticated,
        message: error.to_string(),
        details: Value::Null,
    })
}

fn parse_pairing(pairing: &str) -> Result<PairingCredential, RpcError> {
    PairingCredential::parse(pairing).map_err(|error| RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: error.to_string(),
        details: Value::Null,
    })
}

impl Session {
    async fn client(&self) -> Result<Client, RpcError> {
        self.inner
            .client
            .lock()
            .await
            .as_ref()
            .ok_or_else(closed)
            .cloned()
    }

    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or
    /// `DescribeContract` fails.
    pub async fn about(&self) -> Result<ContractDescription, RpcError> {
        let mut client = self.client().await?;
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .map_err(RpcError::from)
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
        let mut client = self.client().await?;
        let description = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        if !description.supports(RUNTIME_WATCH_CAPABILITY) {
            return Err(RpcError {
                code: RpcErrorCode::Unsupported,
                message: format!("{RUNTIME_WATCH_CAPABILITY} is not advertised"),
                details: Value::Null,
            });
        }
        let payload = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
            .encode()
            .map_err(ConnectError::from)?;
        let stream = client
            .runtime_watch_stream(payload)
            .await
            .map_err(ConnectError::Rpc)?;
        Ok(Watch {
            cancel: self.inner.cancel.child_token(),
            client,
            stream: Mutex::new(Some(stream)),
        })
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
        let mut client = self.client().await?;
        let preview = client.preview(intent).await?;
        Ok(PreparedDeploy {
            preview,
            client,
            session_cancel: self.inner.cancel.clone(),
            confirmed: AtomicBool::new(false),
        })
    }

    /// Calculate a Project-removal preview. Confirming executes these operations.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, the Project
    /// is reserved, snapshot gathering fails, or planning fails.
    pub async fn preview_project_removal(
        &self,
        project_name: ProjectName,
        volumes: VolumeFate,
    ) -> Result<PreparedDeploy, RpcError> {
        let mut client = self.client().await?;
        let preview = client
            .preview_project_removal(&project_name, volumes)
            .await?;
        Ok(PreparedDeploy {
            preview,
            client,
            session_cancel: self.inner.cancel.clone(),
            confirmed: AtomicBool::new(false),
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
        Ok(running.finished().await)
    }

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails. Per-volume not-found and other Machine errors stay in
    /// the Partial Result.
    pub async fn remove_volumes(
        &self,
        request: RemoveVolumesRequest,
    ) -> Result<PartialResult<DockerVolumeName, RpcError>, RpcError> {
        let mut client = self.client().await?;
        client.remove_volumes(request).await
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
        let mut client = self.client().await?;
        client.data_loss_if_machine_removed(&target).await
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
    /// entry while another Machine is visible, the Machine is the last
    /// Cloud-paired Machine, the Machine did not respond so Data Loss cannot
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
        let mut client = self.client().await?;
        client.remove_machine(&target, confirm_data_loss).await
    }

    /// Live Observation of Data Loss that destroying `project` would cause.
    ///
    /// [`VolumeFate::Preserve`] yields an empty list. Mutates nothing.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `project`
    /// is not a Project Name or is reserved, snapshot gathering fails, or
    /// destroying volumes is requested against a known incomplete snapshot.
    pub async fn data_loss_if_project_destroyed(
        &self,
        project: &str,
        volumes: VolumeFate,
    ) -> Result<ObservedDataLoss, RpcError> {
        let project_name =
            ProjectName::parse(project).map_err(|error| invalid_argument(error.to_string()))?;
        let mut client = self.client().await?;
        client
            .data_loss_if_project_destroyed(&project_name, volumes)
            .await
    }

    /// Destroy `project` after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` is derived from the Live Observation the caller
    /// showed a human. Confirmed identities that disappeared are ignored, so
    /// one confirmation can cover several Projects. Re-reads Data Loss at
    /// execute time. [`VolumeFate::Preserve`] is the non-destructive default.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, `project`
    /// is not a Project Name or is reserved, the Project is not visible, the
    /// snapshot is incomplete, or the confirmation does not cover the fresh
    /// Data Loss. Unconfirmed names are in `UnconfirmedDataLoss` details.
    /// Execution failure is a [`DeployOutcome::Failed`].
    pub async fn destroy_project(
        &self,
        project: &str,
        confirm_data_loss: &DataLossConfirmation,
        volumes: VolumeFate,
    ) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        let project_name =
            ProjectName::parse(project).map_err(|error| invalid_argument(error.to_string()))?;
        let mut client = self.client().await?;
        client
            .destroy_project(
                &project_name,
                confirm_data_loss,
                volumes,
                &self.inner.cancel,
                None,
            )
            .await
    }

    /// Live Observation of Data Loss that destroying this Cluster would cause.
    ///
    /// Unions Docker Volumes across every visible Project and Machine. Mutates
    /// nothing: it is safe to call when the operator then cancels.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or listing
    /// Machines fails.
    pub async fn data_loss_if_cluster_destroyed(&self) -> Result<ObservedDataLoss, RpcError> {
        let mut client = self.client().await?;
        client.data_loss_if_cluster_destroyed().await
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
        let mut client = self.client().await?;
        client
            .destroy_cluster(confirm_data_loss, &self.inner.cancel)
            .await
    }

    /// Drop the Client and Relay tunnel. Aborts in-flight Watch and Deploy.
    ///
    /// Repeated calls are a no-op.
    pub async fn close(&self) {
        self.inner.cancel.cancel();
        self.inner.client.lock().await.take();
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

impl PreparedDeploy {
    /// True when this preview planned no operations.
    #[must_use]
    pub fn noop(&self) -> bool {
        self.preview.noop()
    }

    /// Execute these operations. Illegal after a previous confirm.
    ///
    /// # Errors
    ///
    /// Returns when this preview already confirmed, or when the session is closed.
    pub fn confirm(&self) -> Result<RunningDeploy, RpcError> {
        if self.session_cancel.is_cancelled() {
            return Err(closed());
        }
        if self.confirmed.swap(true, Ordering::SeqCst) {
            return Err(invalid_argument(
                "this Deploy Preview already confirmed".into(),
            ));
        }
        let cancel = self.session_cancel.child_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        let preview = self.preview.clone();
        let token = cancel.clone();
        let join = tokio::spawn(async move { client.confirm(&preview, &token, Some(tx)).await });
        Ok(RunningDeploy {
            cancel,
            events: Mutex::new(Some(rx)),
            join: Mutex::new(Some(join)),
        })
    }
}

impl Deref for PreparedDeploy {
    type Target = DeployPreview;

    fn deref(&self) -> &Self::Target {
        &self.preview
    }
}

impl RunningDeploy {
    /// Cancel this Deploy. The outcome is a failed Deploy with `cancelled`.
    pub fn abort(&self) {
        self.cancel.cancel();
    }

    /// Next progress or outcome event, or `None` when the stream ended.
    pub async fn next(&self) -> Option<DeployEvent> {
        let mut guard = self.events.lock().await;
        let rx = guard.as_mut()?;
        if let Some(event) = rx.recv().await {
            return Some(event);
        }
        *guard = None;
        None
    }

    /// Wait for the Deploy Outcome. Progress events are still produced.
    pub async fn finished(&self) -> DeployOutcome<ExecutionError> {
        let handle = self
            .join
            .lock()
            .await
            .take()
            .expect("deploy already finished");
        let outcome = handle.await.expect("deploy task joins");
        while self.next().await.is_some() {}
        outcome
    }
}

impl Watch {
    /// Next complete frame, or `None` if this stream was cancelled or ended.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the daemon, store, or RPC fails.
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
            None | Some(Ok(None)) => {
                *guard = None;
                Ok(None)
            }
            Some(Ok(Some(payload))) => match decode_runtime_watch_frame(&payload) {
                Ok(mut frame) => {
                    tokio::select! {
                        () = self.cancel.cancelled() => {
                            *guard = None;
                            Ok(None)
                        }
                        () = self.client.observe_machine_storage(&mut frame.machines) => {
                            Ok(Some(frame))
                        }
                    }
                }
                Err(error) => {
                    *guard = None;
                    Err(RpcError {
                        code: RpcErrorCode::Internal,
                        message: error.to_string(),
                        details: Value::Null,
                    })
                }
            },
            Some(Err(status))
                if self.cancel.is_cancelled() || status.code() == tonic::Code::Cancelled =>
            {
                *guard = None;
                Ok(None)
            }
            Some(Err(status)) => {
                *guard = None;
                Err(RpcError::from(ConnectError::from(status)))
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
    }
}

fn invalid_argument(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message,
        details: Value::Null,
    }
}
