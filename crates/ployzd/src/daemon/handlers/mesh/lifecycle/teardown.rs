use crate::daemon::{ActiveMesh, DaemonState};
use crate::mesh_state::network::NetworkConfig;
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;
use tracing::warn;

struct MeshControlHandles {
    nats_control: Box<dyn ployz_runtime_api::RuntimeHandle>,
}

pub(in crate::daemon::handlers::mesh::lifecycle) async fn perform_mesh_teardown(
    data_dir: PathBuf,
    active: ActiveMesh,
) -> Result<(), String> {
    let (result, control_handles) =
        perform_mesh_teardown_before_control_shutdown(&data_dir, active).await;
    let _ = control_handles.nats_control.shutdown().await;
    result
}

async fn perform_mesh_teardown_before_control_shutdown(
    data_dir: &Path,
    active: ActiveMesh,
) -> (Result<(), String>, MeshControlHandles) {
    let ActiveMesh {
        config,
        mut mesh,
        mut runtime,
        ..
    } = active;
    let network_name = config.name.0;
    let control_handles = MeshControlHandles {
        nats_control: runtime.take_nats_control(),
    };

    let result = async move {
        runtime.shutdown_background_tasks().await;
        if let Err(error) = mesh.destroy_and_wipe_store_data().await {
            return Err(format!("mesh runtime destroy and wipe failed: {error}"));
        }
        let gateway_error = runtime
            .shutdown_edge_and_image_receiver("mesh destroy")
            .await;
        runtime.shutdown_zfs_transfer("mesh destroy").await;
        if let Some(error) = gateway_error {
            return Err(format!("gateway stop failed: {error}"));
        }

        NetworkConfig::delete(data_dir, &network_name)
            .map_err(|error| format!("failed to delete network config: {error}"))
    }
    .await;

    (result, control_handles)
}

pub(in crate::daemon::handlers::mesh::lifecycle) fn spawn_teardown_after_response<T, F>(
    response_flushed: Option<oneshot::Receiver<()>>,
    teardown: T,
    log_error: F,
) where
    T: std::future::Future<Output = Result<(), String>> + Send + 'static,
    F: FnOnce(String) + Send + 'static,
{
    tokio::spawn(async move {
        if let Some(response_flushed) = response_flushed {
            let _ = response_flushed.await;
        }
        if let Err(error) = teardown.await {
            log_error(error);
        }
    });
}

impl DaemonState {
    pub(in crate::daemon::handlers::mesh) async fn stop_started_mesh_after_transition_failure(
        &mut self,
    ) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.stop_background_tasks().await;
        if let Err(error) = active.mesh.destroy().await {
            warn!(?error, "failed to stop mesh after transition error");
        }
        active.runtime.shutdown_nats_control().await;
        let gateway_error = active
            .runtime
            .shutdown_edge_and_image_receiver("transition failure")
            .await;
        if let Some(error) = gateway_error {
            warn!(%error, "failed to stop gateway after transition error");
        }
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, "image receive session cleanup failed after transition error");
        }
        active
            .runtime
            .shutdown_zfs_transfer("transition failure")
            .await;
    }
}
