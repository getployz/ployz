//! Relay-only Cloud session: connect, about, and close.

use serde_json::Value;
use tokio::sync::Mutex;

use crate::connect::{Client, DialCredential, connect_relay};
use ployz_core::{
    ContractDescription, DescribeContractRequest, MachineId, RpcError, RpcErrorCode, op,
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
