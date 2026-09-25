//! Napi package `@ployz/sdk`. Public payloads are derived from the Rust wire types
//! (`ployz::sdk::typescript_declarations`).
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).
//! The handwritten façade is connect / session observation and registration /
//! about / runtime.watch / preview / run / previewProjectRemoval /
//! remove_volumes / pruneImages / dataLossIfMachineRemoved / removeMachine /
//! dataLossIfProjectDestroyed / destroyProject / dataLossIfClusterDestroyed /
//! destroyCluster / close.
use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz::sdk;
use ployz_core::{
    DataLossConfirmation, ManagementClientLabel, ProjectName, RemoveVolumesRequest, RpcError,
    RpcErrorCode,
};

/// One cancellable connection attempt.
#[napi]
pub struct PendingConnection {
    connections: std::sync::Mutex<Option<Vec<ployz::context::Connection>>>,
    cancel: tokio_util::sync::CancellationToken,
}

/// Parse the shared descriptor shape without reflecting capabilities in errors.
///
/// # Errors
/// Returns InvalidArgument for malformed or empty connections.
#[napi]
pub fn start_connections(connections: serde_json::Value) -> Result<PendingConnection> {
    let connections: Vec<ployz::context::Connection> = serde_json::from_value(connections)
        .map_err(|_| invalid_argument("invalid management connections"))?;
    if connections.is_empty() {
        return Err(invalid_argument("connections must not be empty"));
    }
    Ok(PendingConnection {
        connections: std::sync::Mutex::new(Some(connections)),
        cancel: tokio_util::sync::CancellationToken::new(),
    })
}

#[napi]
impl PendingConnection {
    /// Cancel this attempt; dropping the dial future closes its transport.
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
        let connector = std::sync::Arc::new(ployz::connect::SystemConnector::default());
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

/// Native cancellable preparation retaining opaque image resources.
#[napi]
pub struct PreparationHandle {
    inner: sdk::RunningPreparation,
}

#[napi]
impl PreparationHandle {
    /// Request cancellation and await finished for termination evidence.
    #[napi]
    pub fn abort(&self) {
        self.inner.abort();
    }
    /// Bounded progress; lagging readers receive a truncation frame.
    #[napi]
    pub async fn next(&self) -> Option<serde_json::Value> {
        self.inner.next().await
    }
    /// Await preparation independently of progress consumption.
    ///
    /// # Errors
    /// Returns structured preparation failure or unknown outcome.
    #[napi]
    pub async fn finished(&self) -> Result<DeployPreviewHandle> {
        Ok(DeployPreviewHandle {
            inner: self.inner.finished().await.map_err(rpc_to_napi)?,
        })
    }
}

/// In-flight execution of one Deploy Preview.
#[napi]
pub struct RunningDeployHandle {
    inner: sdk::RunningDeploy,
}

#[napi]
impl Client {
    /// Clear this Machine's Management Client slot named `label`.
    ///
    /// # Errors
    /// Returns an invalid label or uncertain mutation failures.
    #[napi]
    pub async fn clear_management_client(&self, label: String) -> Result<()> {
        let label = ManagementClientLabel::parse(label).map_err(invalid_argument)?;
        self.inner
            .clear_management_client(label)
            .await
            .map_err(rpc_to_napi)
    }

    /// Inspect identity and Management Client labels on this session.
    ///
    /// # Errors
    /// Returns transport or inspection errors.
    #[napi]
    pub async fn inspect(&self) -> Result<serde_json::Value> {
        to_json(&self.inner.inspect().await.map_err(rpc_to_napi)?)
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
        let assignment = serde_json::from_value(assignment).map_err(invalid_argument)?;
        to_json(
            &self
                .inner
                .register(&assignment)
                .await
                .map_err(rpc_to_napi)?,
        )
    }

    /// Publish or clear Certificate Material for a hostname or single-level wildcard.
    ///
    /// # Errors
    /// Returns malformed input, transport failures, or the Machine's refusal of the material.
    #[napi]
    pub async fn publish_certificate_material(
        &self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = serde_json::from_value(request).map_err(invalid_argument)?;
        to_json(
            &self
                .inner
                .publish_certificate_material(request)
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

    /// Read one Container's output through its owning Machine.
    /// # Errors
    /// Returns malformed input and Machine transport failures.
    #[napi]
    pub async fn container_logs(&self, input: serde_json::Value) -> Result<ContainerLogStream> {
        let input = serde_json::from_value(input).map_err(invalid_argument)?;
        Ok(ContainerLogStream {
            inner: self
                .inner
                .container_logs(input)
                .await
                .map_err(rpc_to_napi)?,
        })
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

    /// Start preparation from frozen Cloud settings and checked-out source paths.
    ///
    /// # Errors
    /// Rejects malformed input or closed sessions.
    #[napi]
    pub fn prepare(&self, input: serde_json::Value) -> Result<PreparationHandle> {
        let input = serde_json::from_value(input).map_err(invalid_argument)?;
        Ok(PreparationHandle {
            inner: self.inner.prepare(input).map_err(rpc_to_napi)?,
        })
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
        let intent = serde_json::from_value(intent).map_err(invalid_argument)?;
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
        let project_name = ProjectName::parse(project_name).map_err(invalid_argument)?;
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
            serde_json::from_value(request).map_err(invalid_argument)?;
        let result = self
            .inner
            .remove_volumes(request)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&result)
    }

    /// Remove superseded build images from each target Machine; per-Machine results.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when `targets` is not a
    /// `PruneTarget` list or the session is closed.
    #[napi]
    pub async fn prune_images(&self, targets: serde_json::Value) -> Result<serde_json::Value> {
        let targets: Vec<ployz_core::PruneTarget> =
            serde_json::from_value(targets).map_err(invalid_argument)?;
        let report = self
            .inner
            .prune_images(&targets)
            .await
            .map_err(rpc_to_napi)?;
        to_json(&report)
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
            serde_json::from_value(confirm_data_loss).map_err(invalid_argument)?;
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
            serde_json::from_value(confirm_data_loss).map_err(invalid_argument)?;
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
            serde_json::from_value(confirm_data_loss).map_err(invalid_argument)?;
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
    /// Release unconfirmed retained images.
    #[napi]
    pub fn close(&self) {
        self.inner.close();
    }

    /// Private completed Build evidence; availability is rechecked before reuse.
    /// # Errors
    /// Returns when evidence cannot be encoded as JSON.
    #[napi]
    pub fn build_receipts(&self) -> Result<serde_json::Value> {
        to_json(self.inner.build_receipts())
    }

    /// Machines and repositories Image Cleanup covers, as plain data.
    /// # Errors
    /// Returns when targets cannot be encoded as JSON.
    #[napi]
    pub fn prune_targets(&self) -> Result<serde_json::Value> {
        to_json(&self.inner.prune_targets())
    }

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
    /// `image_cleanup` is `"auto"` (default) or `"manual"`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] JSON payload when this preview already
    /// confirmed or `image_cleanup` is unknown.
    #[napi]
    pub fn confirm(
        &self,
        deployment_id: Option<String>,
        image_cleanup: Option<String>,
    ) -> Result<RunningDeployHandle> {
        let deployment_id = deployment_id
            .map(|id| id.parse::<ployz_core::DeploymentLogId>())
            .transpose()
            .map_err(|error| Error::from_reason(error.to_string()))?;
        let cleanup = image_cleanup
            .as_deref()
            .map(str::parse::<sdk::ImageCleanup>)
            .transpose()
            .map_err(rpc_to_napi)?
            .unwrap_or_default();
        let inner = self
            .inner
            .confirm_with_log_id(deployment_id, cleanup)
            .map_err(rpc_to_napi)?;
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

fn invalid_argument(error: impl std::fmt::Display) -> Error {
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
    let request = serde_json::from_value(request).map_err(invalid_argument)?;
    let snapshot = serde_json::from_value(snapshot).map_err(invalid_argument)?;
    let saved: Vec<ployz_core::EnrollmentAssignment> =
        serde_json::from_value(saved).map_err(invalid_argument)?;
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

/// Cancellable Container log reader.
#[napi]
pub struct ContainerLogStream {
    inner: sdk::ContainerLogStream,
}
#[napi]
impl ContainerLogStream {
    /// Next output, or null at EOF/cancellation.
    /// # Errors
    /// Returns Machine transport or encoding failures.
    #[napi]
    pub async fn next(&self) -> Result<Option<serde_json::Value>> {
        self.inner
            .next()
            .await
            .map_err(rpc_to_napi)?
            .map(|row| to_json(&row))
            .transpose()
    }
    /// Stop this reader without closing its session.
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}
