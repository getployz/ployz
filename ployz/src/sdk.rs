//! Relay-only Cloud session: connect, about, deploy, and close.

use serde_json::Value;
use tokio::sync::Mutex;

use crate::connect::{Client, DialCredential, connect_relay};
use crate::deploy::{DeployError, DeployIntent};
use ployz_core::{
    ContractDescription, DeployOutcome, DescribeContractRequest, ExecutionError, MachineId,
    RpcError, RpcErrorCode, op,
};

/// Connected Cloud session over one Relay Attach.
pub struct Session {
    inner: Mutex<Option<Client>>,
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
    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the session is closed or
    /// `DescribeContract` fails.
    pub async fn about(&self) -> Result<ContractDescription, RpcError> {
        let mut guard = self.inner.lock().await;
        let client = guard.as_mut().ok_or_else(closed)?;
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .map_err(RpcError::from)
    }

    /// Submit a Deploy Intent on the shared Rust Client and return a Deploy Outcome.
    ///
    /// Unary: no operation ID, reserve/submit, progress stream, or `ops.watch`.
    /// Execution failure is [`DeployOutcome::Failed`] with the completed prefix,
    /// failed operation, and unexecuted suffix. Planning and snapshot errors are
    /// [`RpcError`], not an outcome.
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
        let mut guard = self.inner.lock().await;
        let client = guard.as_mut().ok_or_else(closed)?;
        client.deploy(intent).await.map_err(deploy_error)
    }

    /// Drop the Client and Relay tunnel.
    ///
    /// Repeated calls are a no-op.
    pub async fn close(&self) {
        self.inner.lock().await.take();
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
