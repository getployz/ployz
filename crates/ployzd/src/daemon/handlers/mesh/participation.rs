use crate::mesh_state::network::NetworkConfig;
use ployz_store_api::MachineStore;

use super::{DaemonState, restore_network_config_subnet};

impl DaemonState {
    pub(crate) async fn handle_mesh_set_participation(
        &mut self,
        participation: ployz_types::model::Participation,
    ) -> ployz_api::DaemonResponse {
        let forced_participation = match participation {
            ployz_types::model::Participation::Disabled => {
                Some(ployz_types::model::Participation::Disabled)
            }
            ployz_types::model::Participation::Enabled
            | ployz_types::model::Participation::Draining => None,
        };
        if let Err(error) = self
            .set_local_participation_override(forced_participation)
            .await
        {
            return self.err(
                "PARTICIPATION_OVERRIDE_FAILED",
                format!("failed to apply participation override: {error}"),
            );
        }
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let now = ployz_types::time::now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = participation;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        match active.mesh.store.upsert_self_machine(&record).await {
            Ok(()) => self.ok(format!("participation set to {}", record.participation)),
            Err(error) => self.err(
                "STORE_UPDATE_FAILED",
                format!("failed to persist participation: {error}"),
            ),
        }
    }

    pub(crate) async fn handle_mesh_standby(&mut self, force: bool) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let network_name = active.config.name.0.clone();
        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        if !force && self_record.participation != ployz_types::model::Participation::Draining {
            let has_local_workloads = match self
                .runtime_has_local_workloads(&self.identity.machine_id)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    return self.err(
                        "WORKLOAD_INSPECTION_FAILED",
                        format!("failed to inspect local workloads before standby: {error}"),
                    );
                }
            };
            if has_local_workloads {
                return self.err(
                    "MACHINE_NOT_DRAINED",
                    "machine must be draining before standby; rerun with --force to bypass",
                );
            }
        }
        if let Err(error) = self
            .set_local_participation_override(Some(ployz_types::model::Participation::Disabled))
            .await
        {
            return self.err(
                "PARTICIPATION_OVERRIDE_FAILED",
                format!("failed to freeze standby participation: {error}"),
            );
        }
        let now = ployz_types::time::now_unix_secs();
        {
            let Some(active) = self.active.as_ref() else {
                let _ = self.set_local_participation_override(None).await;
                return self.err("NO_RUNNING_NETWORK", "no mesh running");
            };
            let Some(record) = active
                .mesh
                .update_authoritative_self_record(|record| {
                    record.participation = ployz_types::model::Participation::Disabled;
                    record.updated_at = now;
                })
                .await
            else {
                let _ = self.set_local_participation_override(None).await;
                return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
            };
            if let Err(error) = active.mesh.store.upsert_self_machine(&record).await {
                let _ = self.set_local_participation_override(None).await;
                return self.err(
                    "STORE_UPDATE_FAILED",
                    format!("failed to persist pre-standby participation: {error}"),
                );
            }
        }
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => {
                let _ = self.set_local_participation_override(None).await;
                return self.err("IO_ERROR", format!("load network config: {error}"));
            }
        };
        let previous_subnet = config.subnet;
        config.subnet = None;
        if let Err(error) = config.save(&config_path) {
            let _ = self.set_local_participation_override(None).await;
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }
        if let Err(error) = self.restart_active_runtime_from_config(&network_name).await {
            let _ = self.set_local_participation_override(None).await;
            let rollback_error =
                restore_network_config_subnet(&config_path, &mut config, previous_subnet).err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                match rollback_error {
                    Some(rollback_error) => {
                        format!(
                            "failed to enter standby: {error}; failed to restore config: {rollback_error}"
                        )
                    }
                    None => format!("failed to enter standby: {error}"),
                },
            );
        }
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = ployz_types::model::Participation::Disabled;
                record.subnet = None;
                record.status = ployz_types::model::MachineStatus::Up;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        active.config.subnet = None;
        match active.mesh.store.upsert_self_machine(&record).await {
            Ok(()) => self.ok("machine entered standby"),
            Err(error) => self.err(
                "STORE_UPDATE_FAILED",
                format!("failed to persist standby record: {error}"),
            ),
        }
    }

    pub(crate) async fn handle_mesh_promote(
        &mut self,
        assigned_subnet: ipnet::Ipv4Net,
    ) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let network_name = active.config.name.0.clone();
        let config_path = NetworkConfig::path(&self.data_dir, &network_name);
        let mut config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(error) => {
                return self.err("IO_ERROR", format!("load network config: {error}"));
            }
        };
        let previous_subnet = config.subnet;
        config.subnet = Some(assigned_subnet);
        if let Err(error) = config.save(&config_path) {
            return self.err("IO_ERROR", format!("save network config: {error}"));
        }
        if let Err(error) = self.restart_active_runtime_from_config(&network_name).await {
            let rollback_error =
                restore_network_config_subnet(&config_path, &mut config, previous_subnet).err();
            return self.err(
                "NETWORK_RESTART_FAILED",
                match rollback_error {
                    Some(rollback_error) => {
                        format!(
                            "failed to promote machine: {error}; failed to restore config: {rollback_error}"
                        )
                    }
                    None => format!("failed to promote machine: {error}"),
                },
            );
        }
        let Some(active) = self.active.as_mut() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };
        let now = ployz_types::time::now_unix_secs();
        let Some(record) = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.participation = ployz_types::model::Participation::Disabled;
                record.subnet = Some(assigned_subnet);
                record.status = ployz_types::model::MachineStatus::Up;
                record.updated_at = now;
            })
            .await
        else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        active.config.subnet = Some(assigned_subnet);
        match active.mesh.store.upsert_self_machine(&record).await {
            Ok(()) => {}
            Err(error) => {
                return self.err(
                    "STORE_UPDATE_FAILED",
                    format!("failed to persist promoted machine: {error}"),
                );
            }
        }
        self.ok(format!("machine promoted with subnet {}", assigned_subnet))
    }
}
