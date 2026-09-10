//! Napi package `@ployz/sdk`. Public payloads are derived from the Rust wire types
//! (`ployz::sdk::typescript_declarations`).
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).
//! The handwritten façade is connect / session observation and registration /
//! about / runtime.watch / preview / run / previewProjectRemoval /
//! remove_volumes / dataLossIfMachineRemoved / removeMachine /
//! dataLossIfProjectDestroyed / destroyProject / dataLossIfClusterDestroyed /
//! destroyCluster / close.
use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz::sdk;
use ployz_core::{DataLossConfirmation, ProjectName, RemoveVolumesRequest, RpcError, RpcErrorCode};

/// npm package name.
#[must_use]
#[napi]
pub fn package_name() -> &'static str {
    "@ployz/sdk"
}

/// Pure authored configuration parsing, comparison, and restore.
///
/// # Errors
/// Rejects invalid requests or configuration through the shared configuration error boundary.
#[napi]
pub fn config_request(input: serde_json::Value) -> Result<serde_json::Value> {
    ployz_core::config::config_request(input).map_err(|error| Error::from_reason(error.to_string()))
}

/// One cancellable connection attempt. Owns no shared helper manager.
#[napi]
pub struct PendingConnection {
    connections: std::sync::Mutex<Option<Vec<ployz::context::Connection>>>,
    helper: String,
    cancel: tokio_util::sync::CancellationToken,
}

/// Parse the shared descriptor shape without reflecting capabilities in errors.
///
/// # Errors
/// Returns InvalidArgument for malformed or empty connections.
#[napi]
pub fn start_connections(
    connections: serde_json::Value,
    helper: String,
) -> Result<PendingConnection> {
    let connections: Vec<ployz::context::Connection> = serde_json::from_value(connections)
        .map_err(|_| {
            rpc_to_napi(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "invalid management connections".into(),
                details: serde_json::Value::Null,
            })
        })?;
    if connections.is_empty() {
        return Err(rpc_to_napi(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: "connections must not be empty".into(),
            details: serde_json::Value::Null,
        }));
    }
    Ok(PendingConnection {
        connections: std::sync::Mutex::new(Some(connections)),
        helper,
        cancel: tokio_util::sync::CancellationToken::new(),
    })
}

#[napi]
impl PendingConnection {
    /// Cancel this attempt; dropping the dial future reaps its helper.
    #[napi]
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Await one confirmed session.
    ///
    /// # Errors
    /// Returns cancellation, connection failure, or an already-consumed attempt.
    #[napi]
    pub async fn wait(&self) -> Result<Client> {
        let connections = self
            .connections
            .lock()
            .map_err(|_| Error::from_reason("connection lock failed"))?
            .take()
            .ok_or_else(|| Error::from_reason("connection already awaited"))?;
        let connector = std::sync::Arc::new(
            ployz::connect::SystemConnector::default().with_tailcat_program(&self.helper),
        );
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(rpc_to_napi(RpcError { code: RpcErrorCode::Unavailable, message: "connection cancelled".into(), details: serde_json::Value::Null })),
            result = sdk::connect_connections(connections, connector) => Ok(Client { inner: result.map_err(rpc_to_napi)? }),
        }
    }
}

/// Session over one confirmed management connection.
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
    /// Request endpoint revocation. Confirm separately through the successor.
    ///
    /// # Errors
    /// Returns invalid input or uncertain mutation failures.
    #[napi]
    pub async fn remove_cloud_pairing(&self, removal: serde_json::Value) -> Result<()> {
        let removal = serde_json::from_value(removal)
            .map_err(|_| Error::from_reason("invalid Tailcat removal"))?;
        self.inner
            .remove_cloud_pairing(removal)
            .await
            .map_err(rpc_to_napi)
    }

    /// Inspect identity and Cloud Pairing presence on this session.
    ///
    /// # Errors
    /// Returns transport or inspection errors.
    #[napi]
    pub async fn inspect(&self) -> Result<serde_json::Value> {
        serde_json::to_value(self.inner.inspect().await.map_err(rpc_to_napi)?).map_err(invalid_json)
    }

    /// Read enrollment facts from this confirmed Entry Machine.
    ///
    /// # Errors
    /// Returns cancellation, transport errors, or missing enrollment facts.
    #[napi]
    pub async fn observe_enrollment(&self) -> Result<serde_json::Value> {
        to_json(&self.inner.observe_enrollment().await.map_err(rpc_to_napi)?)
    }

    /// Send Register on this confirmed session without mutation replay.
    ///
    /// # Errors
    /// Returns invalid input, transport failures or Register domain errors.
    #[napi]
    pub async fn register(&self, assignment: serde_json::Value) -> Result<serde_json::Value> {
        let assignment = serde_json::from_value(assignment).map_err(invalid_json)?;
        to_json(
            &self
                .inner
                .register(&assignment)
                .await
                .map_err(rpc_to_napi)?,
        )
    }

    /// Describe the entry Machine contract.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the session is closed
    /// or `DescribeContract` fails.
    #[napi]
    pub async fn about(&self) -> Result<serde_json::Value> {
        let description = self.inner.about().await.map_err(rpc_to_napi)?;
        to_json(&description)
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
    /// [`DeployIntent`](ployz_core::DeployIntent) data, the session is closed, or planning fails.
    #[napi]
    pub async fn preview(&self, intent: serde_json::Value) -> Result<DeployPreviewHandle> {
        let intent = serde_json::from_value(intent).map_err(invalid_json)?;
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
    /// Machines fails. Already-absent Volumes count as successful removals;
    /// every failure or omission retains its Docker Volume identity.
    #[napi]
    pub async fn remove_volumes(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let request: RemoveVolumesRequest =
            serde_json::from_value(request).map_err(invalid_json)?;
        let result = self
            .inner
            .remove_volumes(request)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&result)
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
        to_json(&observed)
    }

    /// Remove `machine` after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` must be a DataLossConfirmation object, not a bare
    /// Data Loss list or an ObservedDataLoss read.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a DataLossConfirmation object, the session is closed, the
    /// Machine cannot be removed, or the confirmation does not cover the fresh
    /// Data Loss.
    #[napi]
    pub async fn remove_machine(
        &self,
        machine: String,
        confirm_data_loss: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: DataLossConfirmation =
            serde_json::from_value(confirm_data_loss).map_err(invalid_json)?;
        let removed = self
            .inner
            .remove_machine(&machine, &confirm_data_loss)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&removed)
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
        to_json(&observed)
    }

    /// Destroy `project_name` after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` must be a DataLossConfirmation object, not a bare
    /// Data Loss list or an ObservedDataLoss read. Confirmed identities that
    /// disappeared are ignored.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a DataLossConfirmation object, the session is closed, the
    /// Project cannot be destroyed, or the confirmation does not cover the
    /// fresh Data Loss.
    #[napi]
    pub async fn destroy_project(
        &self,
        project_name: String,
        confirm_data_loss: serde_json::Value,
        destroy_volumes: bool,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: DataLossConfirmation =
            serde_json::from_value(confirm_data_loss).map_err(invalid_json)?;
        let outcome = self
            .inner
            .destroy_project(
                &project_name,
                &confirm_data_loss,
                volume_fate(destroy_volumes),
            )
            .await
            .map_err(rpc_to_napi)?;
        to_json(&outcome)
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
        to_json(&observed)
    }

    /// Destroy this Cluster after an exact Data Loss confirmation.
    ///
    /// `confirm_data_loss` must be a DataLossConfirmation object, not a bare
    /// Data Loss list or an ObservedDataLoss read. Confirmed identities that
    /// disappeared are ignored.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `confirm_data_loss`
    /// is not a DataLossConfirmation object, the session is closed, or the
    /// confirmation does not cover the fresh Data Loss.
    #[napi]
    pub async fn destroy_cluster(
        &self,
        confirm_data_loss: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let confirm_data_loss: DataLossConfirmation =
            serde_json::from_value(confirm_data_loss).map_err(invalid_json)?;
        let teardown = self
            .inner
            .destroy_cluster(&confirm_data_loss)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&teardown)
    }

    /// Drop the Client and transport session. Aborts in-flight Watch and Deploy.
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
        to_json(self.inner.preview())
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
            Some(event) => to_json(&event).map(Some),
            None => Ok(None),
        }
    }

    /// Wait for the Deploy Outcome. Progress events are still produced.
    ///
    /// # Errors
    ///
    /// Returns a typed error if session closure interrupts execution, or if JSON encoding fails.
    #[napi]
    pub async fn finished(&self) -> Result<serde_json::Value> {
        let outcome = self.inner.finished().await.map_err(rpc_to_napi)?;
        to_json(&outcome)
    }
}

#[napi]
impl WatchStream {
    /// Next complete `RuntimeWatchView`, or `null` when this stream was cancelled.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when the daemon, store, or
    /// RPC fails, including when the stream ends without cancellation.
    #[napi]
    pub async fn next(&self) -> Result<Option<serde_json::Value>> {
        match self.inner.next().await {
            Ok(Some(frame)) => to_json(&sdk::RuntimeWatchView::from(frame)).map(Some),
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

fn volume_fate(destroy_volumes: bool) -> ployz::deploy::VolumeFate {
    if destroy_volumes {
        ployz::deploy::VolumeFate::Destroy
    } else {
        ployz::deploy::VolumeFate::Preserve
    }
}

fn to_json(value: &impl serde::Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| Error::from_reason(error.to_string()))
}

fn invalid_json(error: serde_json::Error) -> Error {
    rpc_to_napi(RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: error.to_string(),
        details: serde_json::Value::Null,
    })
}

const RPC_ERROR_PREFIX: &str = "PLOYZ_RPC_ERROR:";

fn rpc_to_napi(error: RpcError) -> Error {
    match serde_json::to_string(&error) {
        Ok(json) => Error::from_reason(format!("{RPC_ERROR_PREFIX}{json}")),
        Err(_) => Error::from_reason(error.to_string()),
    }
}

/// Run the shared allocation policy inside the caller's storage transaction.
///
/// # Errors
/// Rejects invalid JSON, conflicting retry inputs or claims, invalid pools, and exhaustion.
#[napi]
pub fn allocate_enrollment(
    request: serde_json::Value,
    snapshot: serde_json::Value,
    saved: serde_json::Value,
) -> Result<serde_json::Value> {
    let request = serde_json::from_value(request).map_err(invalid_json)?;
    let snapshot = serde_json::from_value(snapshot).map_err(invalid_json)?;
    let saved: Vec<ployz_core::EnrollmentAssignment> =
        serde_json::from_value(saved).map_err(invalid_json)?;
    let assignment =
        ployz_core::allocate_enrollment(&request, &snapshot, &saved).map_err(|error| {
            rpc_to_napi(ployz_core::RpcError {
                code: ployz_core::RpcErrorCode::Conflict,
                message: error.to_string(),
                details: serde_json::Value::Null,
            })
        })?;
    to_json(&assignment)
}

/// Prepare an offline removal successor without exposing capabilities in argv.
///
/// # Errors
/// Returns a redacted error for invalid input or helper failure.
#[napi]
pub async fn prepare_tailcat_removal(expected: String, helper: String) -> Result<String> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let failure = || Error::from_reason("Tailcat successor preparation failed");
    let expected = ployz_core::TailcatCapability::parse(expected).map_err(|_| failure())?;
    let mut child = tokio::process::Command::new(helper)
        .arg("successor")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| failure())?;
    let mut input = child.stdin.take().ok_or_else(failure)?;
    input
        .write_all(format!("{}\n", expected.as_str()).as_bytes())
        .await
        .map_err(|_| failure())?;
    drop(input);
    let output = child.wait_with_output().await.map_err(|_| failure())?;
    if !output.status.success() {
        return Err(failure());
    }
    let successor = String::from_utf8(output.stdout).map_err(|_| failure())?;
    let successor = successor.strip_suffix('\n').ok_or_else(failure)?;
    let successor = ployz_core::TailcatCapability::parse(successor).map_err(|_| failure())?;
    Ok(successor.into())
}
