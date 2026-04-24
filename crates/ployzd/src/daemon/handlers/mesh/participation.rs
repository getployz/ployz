use ipnet::Ipv4Net;
use ployz_api::MachineTransitionGoal;
use ployz_types::model::MachineLifecycle;

use crate::mesh_state::network::NetworkConfig;

use super::{DaemonState, restore_network_config_subnet};

#[derive(Debug)]
pub(crate) struct TransitionError {
    pub(super) code: &'static str,
    pub(super) message: String,
}

impl TransitionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl DaemonState {
    pub(crate) async fn handle_machine_transition_self(
        &mut self,
        goal: MachineTransitionGoal,
        assigned_subnet: Option<Ipv4Net>,
        force: bool,
    ) -> ployz_api::DaemonResponse {
        match self
            .transition_local_machine(goal, assigned_subnet, force)
            .await
        {
            Ok(message) => self.ok(message),
            Err(error) => self.err(error.code, error.message),
        }
    }

    pub(crate) async fn transition_local_machine(
        &mut self,
        goal: MachineTransitionGoal,
        assigned_subnet: Option<Ipv4Net>,
        force: bool,
    ) -> Result<String, TransitionError> {
        let (network_name, current) = {
            let Some(active) = self.active.as_ref() else {
                return Err(TransitionError::new("NO_RUNNING_NETWORK", "no mesh running"));
            };
            let Some(self_record) = active.mesh.authoritative_self_record().await else {
                return Err(TransitionError::new(
                    "SELF_RECORD_MISSING",
                    "mesh self record unavailable",
                ));
            };
            (active.config.name.0.clone(), self_record)
        };

        match goal {
            MachineTransitionGoal::Activate => {
                let Some(assigned_subnet) = assigned_subnet else {
                    return Err(TransitionError::new(
                        "INVALID_ARGUMENT",
                        "machine activate requires an assigned subnet",
                    ));
                };
                if current.lifecycle == MachineLifecycle::Active
                    && current.subnet == Some(assigned_subnet)
                {
                    return Ok(format!("machine already active with subnet {assigned_subnet}"));
                }
                if current.lifecycle == MachineLifecycle::Draining {
                    return Err(TransitionError::new(
                        "INVALID_TRANSITION",
                        "cannot activate a draining machine without first entering standby",
                    ));
                }

                let config_path = NetworkConfig::path(&self.data_dir, &network_name);
                let mut config = NetworkConfig::load(&config_path).map_err(|error| {
                    TransitionError::new("IO_ERROR", format!("load network config: {error}"))
                })?;
                let previous_subnet = config.subnet;
                config.subnet = Some(assigned_subnet);
                config.save(&config_path).map_err(|error| {
                    TransitionError::new("IO_ERROR", format!("save network config: {error}"))
                })?;
                if previous_subnet != Some(assigned_subnet)
                    && let Err(error) = self.restart_active_runtime_from_config(&network_name).await
                {
                    let rollback_error =
                        restore_network_config_subnet(&config_path, &mut config, previous_subnet)
                            .err();
                    return Err(TransitionError::new(
                        "NETWORK_RESTART_FAILED",
                        match rollback_error {
                            Some(rollback_error) => format!(
                                "failed to activate machine: {error}; failed to restore config: {rollback_error}"
                            ),
                            None => format!("failed to activate machine: {error}"),
                        },
                    ));
                }

                let now = ployz_types::time::now_unix_secs();
                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .update_authoritative_self_record(|record| {
                        record.lifecycle = MachineLifecycle::Active;
                        record.subnet = Some(assigned_subnet);
                        record.updated_at = now;
                    })
                    .await
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                active.config.subnet = Some(assigned_subnet);
                Ok(format!(
                    "machine '{}' activated with subnet {}",
                    record.id, assigned_subnet
                ))
            }
            MachineTransitionGoal::Drain => {
                if current.lifecycle == MachineLifecycle::Draining {
                    return Ok(format!("machine '{}' already draining", current.id));
                }
                if current.lifecycle == MachineLifecycle::Standby {
                    return Err(TransitionError::new(
                        "INVALID_TRANSITION",
                        "cannot drain a standby machine",
                    ));
                }

                let now = ployz_types::time::now_unix_secs();
                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .update_authoritative_self_record(|record| {
                        record.lifecycle = MachineLifecycle::Draining;
                        record.updated_at = now;
                    })
                    .await
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                Ok(format!("machine '{}' draining", record.id))
            }
            MachineTransitionGoal::Standby => {
                if current.lifecycle == MachineLifecycle::Standby && current.subnet.is_none() {
                    return Ok(format!("machine '{}' already standby", current.id));
                }
                if !force && current.lifecycle != MachineLifecycle::Draining {
                    return Err(TransitionError::new(
                        "INVALID_TRANSITION",
                        "machine must be draining before standby; rerun with --force to bypass",
                    ));
                }

                let config_path = NetworkConfig::path(&self.data_dir, &network_name);
                let mut config = NetworkConfig::load(&config_path).map_err(|error| {
                    TransitionError::new("IO_ERROR", format!("load network config: {error}"))
                })?;
                let previous_subnet = config.subnet;
                config.subnet = None;
                config.save(&config_path).map_err(|error| {
                    TransitionError::new("IO_ERROR", format!("save network config: {error}"))
                })?;
                if previous_subnet.is_some()
                    && let Err(error) = self.restart_active_runtime_from_config(&network_name).await
                {
                    let rollback_error =
                        restore_network_config_subnet(&config_path, &mut config, previous_subnet)
                            .err();
                    return Err(TransitionError::new(
                        "NETWORK_RESTART_FAILED",
                        match rollback_error {
                            Some(rollback_error) => format!(
                                "failed to enter standby: {error}; failed to restore config: {rollback_error}"
                            ),
                            None => format!("failed to enter standby: {error}"),
                        },
                    ));
                }

                let now = ployz_types::time::now_unix_secs();
                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .update_authoritative_self_record(|record| {
                        record.lifecycle = MachineLifecycle::Standby;
                        record.subnet = None;
                        record.updated_at = now;
                    })
                    .await
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                active.config.subnet = None;
                Ok(format!("machine '{}' entered standby", record.id))
            }
        }
    }
}
