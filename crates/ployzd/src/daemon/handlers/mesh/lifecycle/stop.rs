use crate::daemon::{ActiveMesh, DaemonState};
use crate::mesh_state::network::NetworkConfig;
use ployz_model::{
    MachineLifecycle, MachineLifecycleGoal, MachineLifecycleTransition, MachineMembership,
    MachineTransitionEvidence, NetworkLifecycleGoal, NetworkLifecycleTransition,
    NetworkTransitionEvidence, StandbyTransitionClearance,
};
use ployz_store_api::MachineMembershipStore;
use tracing::warn;

impl DaemonState {
    pub(crate) async fn handle_mesh_stop(&mut self, force: bool) -> ployz_api::DaemonResponse {
        let (network_name, overlay_ip, subnet_to_persist, current_lifecycle, previous_self_record) = {
            let Some(active) = self.active.as_ref() else {
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            let Some(self_record) = active.mesh.authoritative_self_record().await else {
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            (
                active.config.name.0.clone(),
                active.config.overlay_ip,
                active.subnet_to_persist_after_stop(),
                self_record.lifecycle,
                self_record,
            )
        };

        if !force && current_lifecycle != MachineLifecycle::Draining {
            return self.err(
                "INVALID_TRANSITION",
                "mesh stop requires the local machine to be draining; rerun with --force to bypass",
            );
        }

        let Some(mut active) = self.active.take() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        active.stop_background_tasks().await;
        if let Err(error) = active.mesh.destroy().await {
            self.active = Some(active);
            return self.err("NETWORK_STOP_FAILED", format!("mesh stop failed: {error}"));
        }
        if let Err(error) = persist_stopped_self_record(&mut active, &previous_self_record).await {
            warn!(%error, "failed to persist standby self record after mesh stop");
        }
        let mut persisted = active.config.clone();
        let persisted_network_name = persisted.name.clone();
        let _ = persisted
            .lifecycle
            .apply_transition(NetworkLifecycleTransition {
                goal: NetworkLifecycleGoal::Stop,
                evidence: NetworkTransitionEvidence::MeshTeardown {
                    network: persisted_network_name,
                },
                at_unix_secs: ployz_time::now_unix_secs(),
            });
        persisted.subnet = subnet_to_persist;
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        if let Err(error) = persisted.save(&config_path) {
            return self.err(
                "IO_ERROR",
                format!("failed to persist stopped network config: {error}"),
            );
        }

        active.runtime.shutdown_nats_control().await;
        let gateway_error = active
            .runtime
            .shutdown_edge_and_image_receiver("mesh stop")
            .await;
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, "image receive session cleanup failed during mesh stop");
        }
        active.runtime.shutdown_zfs_transfer("mesh stop").await;
        if let Some(error) = gateway_error {
            return self.err(
                "NETWORK_STOP_FAILED",
                format!("gateway stop failed: {error}"),
            );
        }

        self.ok(format!(
            "mesh '{}' stopped\n  overlay: {}\n  lifecycle: stopped",
            network_name, overlay_ip
        ))
    }
}

async fn persist_stopped_self_record(
    active: &mut ActiveMesh,
    previous_self_record: &MachineMembership,
) -> Result<(), String> {
    let mut standby = previous_self_record.clone();
    standby
        .apply_lifecycle_transition(MachineLifecycleTransition {
            goal: MachineLifecycleGoal::Standby {
                clearance: StandbyTransitionClearance::OperatorForced,
            },
            evidence: MachineTransitionEvidence::MeshStop {
                network: active.config.name.clone(),
            },
            at_unix_secs: ployz_time::now_unix_secs(),
        })
        .map_err(|err| format!("build standby self record: {err}"))?;

    if let Err(error) = active.mesh.store.upsert_self_machine(&standby).await {
        return Err(format!("persist standby self record in store: {error}"));
    }

    if active
        .mesh
        .update_authoritative_self_record(|record| {
            *record = standby.clone();
        })
        .await
        .is_none()
    {
        return Err("update in-memory self record after standby persistence".to_string());
    }

    Ok(())
}
