use std::time::Duration;

use ployz_types::model::MachineId;

use crate::subjects;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCommandSubject {
    subject: String,
}

impl NodeCommandSubject {
    #[must_use]
    pub fn deploy_start_candidate(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.start_candidate")
    }

    #[must_use]
    pub fn deploy_drain_instance(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.drain_instance")
    }

    #[must_use]
    pub fn deploy_stop(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "deploy.stop")
    }

    #[must_use]
    pub fn volume_ensure(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.ensure")
    }

    #[must_use]
    pub fn volume_remove(machine_id: &MachineId) -> Self {
        Self::new(machine_id, "volume.remove")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.subject
    }

    fn new(machine_id: &MachineId, command: &str) -> Self {
        Self {
            subject: subjects::node_command(machine_id, command),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RpcPolicy {
    pub timeout: Duration,
}

impl Default for RpcPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15),
        }
    }
}
