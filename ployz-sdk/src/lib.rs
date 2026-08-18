//! Napi package `@ployz/sdk`. Public payloads are generated from Rust.
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).
//! The handwritten façade is connect / about / runtime.watch / deploy /
//! remove_volumes / close.

use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz::sdk;
use ployz_core::{DeployIntent, RemoveVolumesRequest, RpcError, RpcErrorCode};

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

/// Native Watch stream. The package façade exposes this as `runtime.watch()`.
#[napi]
pub struct WatchStream {
    inner: sdk::Watch,
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

    /// Open a Runtime Watch stream of complete frames.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when Watch is not
    /// advertised, the session is closed, or the stream cannot be opened.
    #[napi]
    pub async fn watch(&self) -> Result<WatchStream> {
        let inner = self.inner.watch().await.map_err(rpc_to_napi)?;
        Ok(WatchStream { inner })
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

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `request` is not
    /// [`RemoveVolumesRequest`] data, the session is closed, or listing
    /// Machines fails. Per-volume failures stay in the Partial Result.
    #[napi]
    pub async fn remove_volumes(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let request: RemoveVolumesRequest = serde_json::from_value(request).map_err(|error| {
            rpc_to_napi(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: error.to_string(),
                details: serde_json::Value::Null,
            })
        })?;
        let result = self
            .inner
            .remove_volumes(request)
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&result).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Drop the Client and Relay tunnel.
    #[napi]
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

#[napi]
impl WatchStream {
    /// Next complete `RuntimeWatchFrame`, or `null` when this stream ended.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the daemon, store, or
    /// RPC fails.
    #[napi]
    pub async fn next(&self) -> Result<Option<serde_json::Value>> {
        match self.inner.next().await {
            Ok(Some(frame)) => serde_json::to_value(&frame)
                .map(Some)
                .map_err(|error| Error::from_reason(error.to_string())),
            Ok(None) => Ok(None),
            Err(error) => Err(rpc_to_napi(error)),
        }
    }

    /// End this Watch stream. The Client stays usable.
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
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
