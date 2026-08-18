//! Napi package `@ployz/sdk`. Public payloads are generated from Rust.
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).
//! The handwritten façade is connect / about / deploy / close. Watch is a
//! later ticket.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz::sdk;
use ployz_core::{DeployIntent, RpcError, RpcErrorCode};

/// npm package name.
#[must_use]
#[napi]
pub fn package_name() -> &'static str {
    "@ployz/sdk"
}

/// Dial Credential and selected entry Machine for a Relay-only session.
#[napi(object)]
pub struct ConnectOptions {
    pub relay_url: String,
    pub bearer: String,
    pub machine_id: String,
}

/// Cloud session over one Relay Attach.
#[napi]
pub struct Client {
    inner: sdk::Session,
}

#[napi]
impl Client {
    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the session is closed
    /// or `DescribeContract` fails.
    #[napi]
    pub async fn about(&self) -> Result<serde_json::Value> {
        let description = self.inner.about().await.map_err(rpc_to_napi)?;
        serde_json::to_value(&description).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Submit a Deploy Intent and resolve to a Deploy Outcome.
    ///
    /// Invokes the shared Rust Client deployment operation. No TypeScript
    /// planning or policy logic.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `intent` is not
    /// [`DeployIntent`] data, the session is closed, or planning fails before
    /// execution. Execution failure is a [`ployz_core::DeployOutcome::Failed`]
    /// value, not this error.
    #[napi]
    pub async fn deploy(&self, intent: serde_json::Value) -> Result<serde_json::Value> {
        let intent: DeployIntent = serde_json::from_value(intent).map_err(|error| {
            rpc_to_napi(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: error.to_string(),
                details: serde_json::Value::Null,
            })
        })?;
        let outcome = self.inner.deploy(intent).await.map_err(rpc_to_napi)?;
        serde_json::to_value(&outcome).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Drop the Client and Relay tunnel.
    #[napi]
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

/// Connect to one selected Machine through Cloud Relay.
///
/// # Errors
///
/// Returns a generated [`RpcError`] JSON payload when the Dial Credential or
/// Machine ID is rejected, or when the Relay or inner RPC channel fails.
#[napi]
pub async fn connect(options: ConnectOptions) -> Result<Client> {
    let inner = sdk::connect(&options.relay_url, &options.bearer, &options.machine_id)
        .await
        .map_err(rpc_to_napi)?;
    Ok(Client { inner })
}

fn rpc_to_napi(error: RpcError) -> Error {
    match serde_json::to_string(&error) {
        Ok(json) => Error::from_reason(json),
        Err(_) => Error::from_reason(error.to_string()),
    }
}
