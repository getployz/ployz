//! Relay-only Cloud session: connect, about, runtime.watch, preview, deploy,
//! remove_volumes, Data Loss for Machine removal, and close.

use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError, DialCredential, connect_relay};
use crate::deploy::{DeployError, DeployIntent, DeployPreview};
use ployz_core::{
    ContractDescription, DeployOutcome, DescribeContractRequest, DockerVolumeName, ExecutionError,
    MachineId, MachineTarget, ObservedDataLoss, OpaquePayload, PartialResult,
    RUNTIME_WATCH_CAPABILITY, RemoveVolumesRequest, RpcError, RpcErrorCode, RuntimeWatchFrame,
    RuntimeWatchRequest, op,
};

/// Connected Cloud session over one Relay Attach.
pub struct Session {
    inner: Mutex<Option<Client>>,
}

/// Complete Runtime Watch frames from the entry Machine.
///
/// Drop or [`cancel`](Self::cancel) ends this stream only. The Client stays usable.
pub struct Watch {
    cancel: CancellationToken,
    stream: Mutex<Option<tonic::Streaming<OpaquePayload>>>,
}

/// Open a Machine RPC channel through Cloud Relay.
///
/// Succeeds only after Relay Dial and Machine Attach produce a usable RPC
/// channel. Does not mint Attach credentials, perform Cloud Pairing, or choose
/// an entry Machine.
///
/// # Errors
///
/// Returns a generated [`RpcError`] when the bearer or Machine ID is rejected,
/// or when the Relay or inner RPC channel fails.
pub async fn connect(relay_url: &str, bearer: &str, machine_id: &str) -> Result<Session, RpcError> {
    let credential = DialCredential::parse(bearer).map_err(|error| RpcError {
        code: RpcErrorCode::Unauthenticated,
        message: error.to_string(),
        details: Value::Null,
    })?;
    let machine_id = MachineId::parse(machine_id).map_err(|error| RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: error.to_string(),
        details: Value::Null,
    })?;
    let client = connect_relay(relay_url, credential, machine_id).await?;
    Ok(Session {
        inner: Mutex::new(Some(client)),
    })
}

impl Session {
    async fn client(&self) -> Result<Client, RpcError> {
        self.inner.lock().await.as_ref().ok_or_else(closed).cloned()
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
            cancel: CancellationToken::new(),
            stream: Mutex::new(Some(stream)),
        })
    }

    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Same planner, ingress expansion, and DNS warnings as the CLI. The
    /// preview is not a handle: [`Self::deploy`] re-plans against a fresh
    /// snapshot rather than replaying these operations.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, snapshot
    /// gathering fails, ingress expansion fails, or planning fails.
    pub async fn preview(&self, intent: DeployIntent) -> Result<DeployPreview, RpcError> {
        let mut client = self.client().await?;
        client.preview(intent).await.map_err(deploy_error)
    }

    /// Submit a Deploy Intent on the shared Rust Client and return a Deploy Outcome.
    ///
    /// Unary: no operation ID, reserve/submit, progress stream, or `ops.watch`.
    /// Execution failure is [`DeployOutcome::Failed`] with the completed prefix,
    /// failed operation, and unexecuted suffix. Planning and snapshot errors are
    /// [`RpcError`], not an outcome. Always re-plans against a fresh snapshot.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed, snapshot
    /// gathering fails, ingress expansion fails, or planning fails before
    /// execution starts.
    pub async fn deploy(
        &self,
        intent: DeployIntent,
    ) -> Result<DeployOutcome<ExecutionError>, RpcError> {
        let mut client = self.client().await?;
        client.deploy(intent).await.map_err(deploy_error)
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
    /// this observer cannot list Docker Volumes on that Machine.
    pub async fn data_loss_if_machine_removed(
        &self,
        machine: &str,
    ) -> Result<ObservedDataLoss, RpcError> {
        let target =
            MachineTarget::parse(machine).map_err(|error| invalid_argument(error.to_string()))?;
        let mut client = self.client().await?;
        client.data_loss_if_machine_removed(&target).await
    }

    /// Drop the Client and Relay tunnel.
    ///
    /// Repeated calls are a no-op.
    pub async fn close(&self) {
        self.inner.lock().await.take();
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
            Some(Ok(Some(payload))) => payload
                .decode_json()
                .map(Some)
                .map_err(|error| RpcError::from(ConnectError::from(error))),
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

fn deploy_error(error: DeployError) -> RpcError {
    match error {
        DeployError::Connect(error) => error.into(),
        DeployError::Plan(error) => invalid_argument(error.to_string()),
        DeployError::Ingress(error) => invalid_argument(error.to_string()),
    }
}

fn invalid_argument(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::InvalidArgument,
        message,
        details: Value::Null,
    }
}
