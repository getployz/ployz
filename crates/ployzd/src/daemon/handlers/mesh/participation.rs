use ployz_api::MachineSelfTransition;
use ployz_types::model::{
    MachineLifecycleGoal, MachineLifecycleTransition, MachineTransitionEvidence,
    MachineTransitionOutcome, StandbyTransitionClearance,
};

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
        transition: MachineSelfTransition,
    ) -> ployz_api::DaemonResponse {
        match self.transition_local_machine(transition).await {
            Ok(message) => self.ok(message),
            Err(error) => self.err(error.code, error.message),
        }
    }

    pub(crate) async fn transition_local_machine(
        &mut self,
        transition: MachineSelfTransition,
    ) -> Result<String, TransitionError> {
        let (network_name, current) = {
            let Some(active) = self.active.as_ref() else {
                return Err(TransitionError::new(
                    "NO_RUNNING_NETWORK",
                    "no mesh running",
                ));
            };
            let Some(self_record) = active.mesh.authoritative_self_record().await else {
                return Err(TransitionError::new(
                    "SELF_RECORD_MISSING",
                    "mesh self record unavailable",
                ));
            };
            (active.config.name.0.clone(), self_record)
        };

        match transition {
            MachineSelfTransition::Activate { assigned_subnet } => {
                let transition = MachineLifecycleTransition {
                    goal: MachineLifecycleGoal::Activate { assigned_subnet },
                    evidence: MachineTransitionEvidence::OperatorCommand {
                        command: "machine transition activate".into(),
                    },
                    at_unix_secs: ployz_types::time::now_unix_secs(),
                };
                let mut validated = current.clone();
                let outcome = validated
                    .apply_lifecycle_transition(transition.clone())
                    .map_err(|error| TransitionError::new(error.code(), error.message()))?;
                if outcome == MachineTransitionOutcome::AlreadyInState {
                    return Ok(format!(
                        "machine already active with subnet {assigned_subnet}"
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

                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .transition_authoritative_self_record(transition)
                    .await
                    .map_err(|error| TransitionError::new(error.code(), error.message()))?
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                active.config.subnet = Some(assigned_subnet);
                active.retained_subnet.record_activation(assigned_subnet);
                Ok(format!(
                    "machine '{}' activated with subnet {}",
                    record.id, assigned_subnet
                ))
            }
            MachineSelfTransition::Drain => {
                let transition = MachineLifecycleTransition {
                    goal: MachineLifecycleGoal::Drain,
                    evidence: MachineTransitionEvidence::OperatorCommand {
                        command: "machine transition drain".into(),
                    },
                    at_unix_secs: ployz_types::time::now_unix_secs(),
                };
                let mut validated = current.clone();
                let outcome = validated
                    .apply_lifecycle_transition(transition.clone())
                    .map_err(|error| TransitionError::new(error.code(), error.message()))?;
                if outcome == MachineTransitionOutcome::AlreadyInState {
                    return Ok(format!("machine '{}' already draining", current.id));
                }

                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .transition_authoritative_self_record(transition)
                    .await
                    .map_err(|error| TransitionError::new(error.code(), error.message()))?
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                Ok(format!("machine '{}' draining", record.id))
            }
            MachineSelfTransition::Standby { force } => {
                let clearance = if force {
                    StandbyTransitionClearance::OperatorForced
                } else {
                    StandbyTransitionClearance::DrainingComplete
                };
                let transition = MachineLifecycleTransition {
                    goal: MachineLifecycleGoal::Standby { clearance },
                    evidence: MachineTransitionEvidence::OperatorCommand {
                        command: "machine transition standby".into(),
                    },
                    at_unix_secs: ployz_types::time::now_unix_secs(),
                };
                let mut validated = current.clone();
                let outcome = validated
                    .apply_lifecycle_transition(transition.clone())
                    .map_err(|error| {
                        let message = if !force {
                            format!("{}; rerun with --force to bypass", error.message())
                        } else {
                            error.message().to_string()
                        };
                        TransitionError::new(error.code(), message)
                    })?;
                if outcome == MachineTransitionOutcome::AlreadyInState {
                    return Ok(format!("machine '{}' already standby", current.id));
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

                let Some(active) = self.active.as_mut() else {
                    return Err(TransitionError::new(
                        "NO_RUNNING_NETWORK",
                        "no mesh running",
                    ));
                };
                let Some(record) = active
                    .mesh
                    .transition_authoritative_self_record(transition)
                    .await
                    .map_err(|error| TransitionError::new(error.code(), error.message()))?
                else {
                    return Err(TransitionError::new(
                        "SELF_RECORD_MISSING",
                        "mesh self record unavailable",
                    ));
                };
                active.config.subnet = None;
                active.retained_subnet.record_standby(previous_subnet);
                Ok(format!("machine '{}' entered standby", record.id))
            }
        }
    }
}
