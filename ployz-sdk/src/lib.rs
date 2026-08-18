//! Napi package `@ployz/sdk`. Public payloads are generated from Rust.
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).
//! The handwritten façade is connect / about / runtime.watch / preview / run /
//! previewProjectRemoval / remove_volumes / dataLossIfMachineRemoved /
//! removeMachine / dataLossIfProjectDestroyed / destroyProject /
//! dataLossIfClusterDestroyed / destroyCluster / close.
use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz::sdk;
use ployz_core::{
    DataLoss, DeployIntent, ProjectName, RemoveVolumesRequest, RpcError, RpcErrorCode,
};

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

/// Planned Deploy that has not executed. `confirm` runs these operations.
#[napi]
pub struct DeployPreviewHandle {
    inner: sdk::PreparedDeploy,
}

/// In-flight execution of one Deploy Preview.
#[napi]
pub struct RunningDeployHandle {
    inner: sdk::RunningDeploy,
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

    /// Calculate a Deploy Preview for a Deploy Intent without executing it.
    ///
    /// Confirming executes these operations. It does not re-plan.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `intent` is not
    /// [`DeployIntent`] data, the session is closed, or planning fails.
    #[napi]
    pub async fn preview(&self, intent: serde_json::Value) -> Result<DeployPreviewHandle> {
        let intent = parse_intent(intent)?;
        let inner = self.inner.preview(intent).await.map_err(rpc_to_napi)?;
        Ok(DeployPreviewHandle { inner })
    }

    /// Calculate a Project-removal preview. Confirming executes these operations.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `project_name` is not
    /// a Project Name, the Project is reserved, the session is closed, or
    /// planning fails.
    #[napi]
    pub async fn preview_project_removal(
        &self,
        project_name: String,
        destroy_volumes: bool,
    ) -> Result<DeployPreviewHandle> {
        let project_name = ProjectName::parse(project_name).map_err(|error| {
            rpc_to_napi(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: error.to_string(),
                details: serde_json::Value::Null,
            })
        })?;
        let volumes = volume_fate(destroy_volumes);
        let inner = self
            .inner
            .preview_project_removal(project_name, volumes)
            .await
            .map_err(rpc_to_napi)?;
        Ok(DeployPreviewHandle { inner })
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

    /// Live Observation of Data Loss that removing `machine` would cause.
    ///
    /// Mutates nothing. Not a complete Cluster view.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the session is
    /// closed, `machine` is not a Machine Target, the Machine is not visible,
    /// or this observer cannot list Docker Volumes on that Machine.
    #[napi]
    pub async fn data_loss_if_machine_removed(&self, machine: String) -> Result<serde_json::Value> {
        let observed = self
            .inner
            .data_loss_if_machine_removed(&machine)
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&observed).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Remove `machine` after a named Data Loss confirmation.
    ///
    /// `confirm_data_loss` must name Data Loss identities, not a boolean and
    /// not an ObservedDataLoss read.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a Data Loss list, the session is closed, the Machine cannot be
    /// removed, or the confirmation does not cover the fresh Data Loss.
    #[napi]
    pub async fn remove_machine(
        &self,
        machine: String,
        confirm_data_loss: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: Vec<DataLoss> =
            serde_json::from_value(confirm_data_loss).map_err(|error| {
                rpc_to_napi(RpcError {
                    code: RpcErrorCode::InvalidArgument,
                    message: error.to_string(),
                    details: serde_json::Value::Null,
                })
            })?;
        let removed = self
            .inner
            .remove_machine(&machine, &confirm_data_loss)
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&removed).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Live Observation of Data Loss that destroying `project_name` would cause.
    ///
    /// `destroy_volumes` false is empty. Mutates nothing.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `project_name` is not
    /// a Project Name, the Project is reserved, the session is closed, snapshot
    /// gathering fails, or destroying volumes is requested against a known
    /// incomplete snapshot.
    #[napi]
    pub async fn data_loss_if_project_destroyed(
        &self,
        project_name: String,
        destroy_volumes: bool,
    ) -> Result<serde_json::Value> {
        let observed = self
            .inner
            .data_loss_if_project_destroyed(&project_name, volume_fate(destroy_volumes))
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&observed).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Destroy `project_name` after a named Data Loss confirmation.
    ///
    /// `confirm_data_loss` must name Data Loss identities, not a boolean and
    /// not an ObservedDataLoss read. Extra names are ignored.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a Data Loss list, the session is closed, the Project cannot be
    /// destroyed, or the confirmation does not cover the fresh Data Loss.
    #[napi]
    pub async fn destroy_project(
        &self,
        project_name: String,
        confirm_data_loss: serde_json::Value,
        destroy_volumes: bool,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: Vec<DataLoss> =
            serde_json::from_value(confirm_data_loss).map_err(|error| {
                rpc_to_napi(RpcError {
                    code: RpcErrorCode::InvalidArgument,
                    message: error.to_string(),
                    details: serde_json::Value::Null,
                })
            })?;
        let outcome = self
            .inner
            .destroy_project(
                &project_name,
                &confirm_data_loss,
                volume_fate(destroy_volumes),
            )
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&outcome).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Live Observation of Data Loss that destroying this Cluster would cause.
    ///
    /// Mutates nothing. Not a complete Cluster view.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the session is
    /// closed or listing Machines fails.
    #[napi]
    pub async fn data_loss_if_cluster_destroyed(&self) -> Result<serde_json::Value> {
        let observed = self
            .inner
            .data_loss_if_cluster_destroyed()
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&observed).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Destroy this Cluster after a named Data Loss confirmation.
    ///
    /// `confirm_data_loss` must name Data Loss identities, not a boolean and
    /// not an ObservedDataLoss read. Extra names are ignored.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a Data Loss list, the session is closed, or the confirmation
    /// does not cover the fresh Data Loss.
    #[napi]
    pub async fn destroy_cluster(
        &self,
        confirm_data_loss: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: Vec<DataLoss> =
            serde_json::from_value(confirm_data_loss).map_err(|error| {
                rpc_to_napi(RpcError {
                    code: RpcErrorCode::InvalidArgument,
                    message: error.to_string(),
                    details: serde_json::Value::Null,
                })
            })?;
        let teardown = self
            .inner
            .destroy_cluster(&confirm_data_loss)
            .await
            .map_err(rpc_to_napi)?;
        serde_json::to_value(&teardown).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Drop the Client and Relay tunnel. Aborts in-flight Watch and Deploy.
    #[napi]
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

#[napi]
impl DeployPreviewHandle {
    /// Planned rows and warnings.
    ///
    /// # Errors
    ///
    /// Returns when the preview cannot be encoded as JSON.
    #[napi]
    pub fn payload(&self) -> Result<serde_json::Value> {
        serde_json::to_value(&*self.inner).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Execute these operations. Illegal after a previous confirm.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when this preview already
    /// confirmed.
    #[napi]
    pub fn confirm(&self) -> Result<RunningDeployHandle> {
        let inner = self.inner.confirm().map_err(rpc_to_napi)?;
        Ok(RunningDeployHandle { inner })
    }
}

#[napi]
impl RunningDeployHandle {
    /// Cancel this Deploy. The outcome is a failed Deploy with `cancelled`.
    #[napi]
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Next progress or outcome event, or `null` when the stream ended.
    ///
    /// # Errors
    ///
    /// Returns when an event cannot be encoded as JSON.
    #[napi]
    pub async fn next(&self) -> Result<Option<serde_json::Value>> {
        match self.inner.next().await {
            Some(event) => serde_json::to_value(&event)
                .map(Some)
                .map_err(|error| Error::from_reason(error.to_string())),
            None => Ok(None),
        }
    }

    /// Wait for the Deploy Outcome. Progress events are still produced.
    ///
    /// # Errors
    ///
    /// Returns when the outcome cannot be encoded as JSON.
    #[napi]
    pub async fn finished(&self) -> Result<serde_json::Value> {
        let outcome = self.inner.finished().await;
        serde_json::to_value(&outcome).map_err(|error| Error::from_reason(error.to_string()))
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

fn volume_fate(destroy_volumes: bool) -> ployz::deploy::VolumeFate {
    if destroy_volumes {
        ployz::deploy::VolumeFate::Destroy
    } else {
        ployz::deploy::VolumeFate::Preserve
    }
}

fn parse_intent(intent: serde_json::Value) -> Result<DeployIntent> {
    serde_json::from_value(intent).map_err(|error| {
        rpc_to_napi(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: error.to_string(),
            details: serde_json::Value::Null,
        })
    })
}

fn rpc_to_napi(error: RpcError) -> Error {
    match serde_json::to_string(&error) {
        Ok(json) => Error::from_reason(json),
        Err(_) => Error::from_reason(error.to_string()),
    }
}
