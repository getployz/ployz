//! Native Cloud session: connect, observe_enrollment, register,
//! about, runtime.watch, preview, run, preview_project_removal, remove_volumes,
//! Data Loss for Machine, Project, and Cluster destroy, remove_machine,
//! destroy_project, destroy_cluster, and close.
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, Connector, TransportError, connect_selected_with};
use crate::context::{Connection, ConnectionSource, SelectedConnections};
use crate::deploy::{DeployIntent, DeployPlan, DeployPreview, VolumeFate};
use ployz_core::{
    ClusterTeardown, ContractDescription, DataLossConfirmation, DeployEvent, DeployOutcome,
    DescribeContractRequest, EnrollmentAssignment, EnrollmentSnapshot, ExecutionError,
    LocalMachineRemoved, MachineTarget, ObservedDataLoss, OpaquePayload, ProjectName,
    RUNTIME_WATCH_CAPABILITY, Registered, RemoveVolumesRequest, RpcError, RpcErrorCode,
    RuntimeWatchFrame, RuntimeWatchRequest, ServiceObservation, VolumeRemoval,
    decode_runtime_watch_frame, op,
};

pub use payloads::typescript_declarations;

mod payloads;

/// The public SDK Watch frame: the RPC frame plus the Services this observer
/// derives from its Containers. The RPC frame carries only Container observations.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct RuntimeWatchView {
    #[serde(flatten)]
    pub frame: RuntimeWatchFrame,
    pub services: Vec<ServiceObservation>,
}

impl From<RuntimeWatchFrame> for RuntimeWatchView {
    fn from(frame: RuntimeWatchFrame) -> Self {
        let services = frame.services();
        Self { frame, services }
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
    session: std::sync::Weak<SessionInner>,
    confirmed: AtomicBool,
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
                details: Value::Null,
            }),
            result = crate::enrollment::publish_enrollment(&mut client, assignment) => result,
        }
    }

    /// Clear pairing and request bounded Tailcat rotation. A reply is not revocation evidence.
    ///
    /// # Errors
    /// Returns cancellation, transport, or endpoint errors, including uncertain outcomes.
    pub async fn remove_cloud_pairing(
        &self,
        removal: ployz_core::TailcatRemoval,
    ) -> Result<(), RpcError> {
        let client = self.client()?;
        self.until_closed(async {
            client
                .call_unretried::<op::SetCloudPairing>(
                    ployz_core::SetCloudPairingRequest::Remove { removal },
                    None,
                )
                .await
                .map(|_| ())
                .map_err(RpcError::from)
        })
        .await
    }

    /// Inspect the selected Machine, including its Cloud Pairing presence.
    ///
    /// # Errors
    /// Returns cancellation or Inspect errors.
    pub async fn inspect(&self) -> Result<ployz_core::MachineDetails, RpcError> {
        let mut client = self.client()?;
        self.until_closed(async {
            client
                .call::<op::Inspect>(ployz_core::InspectRequest::default(), None)
                .await
                .map_err(RpcError::from)
        })
        .await
    }

    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or
    /// `DescribeContract` fails.
    pub async fn about(&self) -> Result<ContractDescription, RpcError> {
        let mut client = self.client()?;
        self.until_closed(async {
            client
                .call::<op::DescribeContract>(DescribeContractRequest {}, None)
                .await
                .map_err(RpcError::from)
        })
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
        let mut client = self.client()?;
        let description = self
            .until_closed(async {
                client
                    .call::<op::DescribeContract>(DescribeContractRequest {}, None)
                    .await
                    .map_err(RpcError::from)
            })
            .await?;
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
            session: Arc::downgrade(&self.inner),
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
        let mut client = self.client()?;
        let preview = tokio::select! {
            biased;
            () = self.inner.cancel.cancelled() => return Err(closed()),
            preview = client.preview_project_removal(&project_name, volumes) => preview?,
        };
        Ok(PreparedDeploy {
            preview,
            session: Arc::downgrade(&self.inner),
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
        let mut client = self.client()?;
        self.until_closed(client.remove_machine(&target, confirm_data_loss))
            .await
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
        let mut client = self.client()?;
        self.until_closed(client.data_loss_if_project_destroyed(&project_name, volumes))
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
        let mut client = self.client()?;
        self.until_closed(client.destroy_project(
            &project_name,
            confirm_data_loss,
            volumes,
            &self.inner.cancel,
            None,
        ))
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

impl PreparedDeploy {
    /// Informational preview; execution remains bound to this prepared handle.
    #[must_use]
    pub fn preview(&self) -> &DeployPreview {
        self.preview.preview()
    }

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
        let session = Session {
            inner: self.session.upgrade().ok_or_else(closed)?,
        };
        if session.inner.cancel.is_cancelled() {
            return Err(closed());
        }
        if self.confirmed.swap(true, Ordering::SeqCst) {
            return Err(invalid_argument(
                "this Deploy Preview already confirmed".into(),
            ));
        }
        let cancel = session.inner.cancel.child_token();
        let (tx, rx) = mpsc::unbounded_channel();
        let client = session.client()?;
        let preview = self.preview.clone();
        let token = cancel.clone();
        let session_cancel = session.inner.cancel.clone();
        let join = tokio::spawn(async move {
            tokio::select! {
                biased;
                () = session_cancel.cancelled() => Err(RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "session closed; in-flight Deploy outcome may be uncertain".into(),
                    details: Value::Null,
                }),
                outcome = client.confirm(&preview, &token, Some(tx)) => Ok(outcome),
            }
        });
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
    ///
    /// # Errors
    /// Returns unavailable when session closure interrupts execution; mutations may have completed.
    pub async fn finished(&self) -> Result<DeployOutcome<ExecutionError>, RpcError> {
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
                        () = client.observe_machine_storage(&mut frame.machines) => {
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
    }
}

fn invalid_argument(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message,
        details: Value::Null,
    }
}
