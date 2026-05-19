use std::path::PathBuf;

use mvp_deploy::{InstanceId, RevisionId};
use mvp_projection::ServiceName;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRuntimeConfig {
    pub root: PathBuf,
    pub managed_http: ManagedHttpCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedHttpCommand {
    pub program: PathBuf,
}

impl ManagedHttpCommand {
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl ProcessRuntimeConfig {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, managed_http: ManagedHttpCommand) -> Self {
        Self {
            root: root.into(),
            managed_http,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInstanceSpec {
    pub instance_id: InstanceId,
    pub service: ServiceName,
    pub revision: RevisionId,
}

impl ProcessInstanceSpec {
    #[must_use]
    pub fn new(instance_id: InstanceId, service: ServiceName, revision: RevisionId) -> Self {
        Self {
            instance_id,
            service,
            revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInstance {
    pub instance_id: InstanceId,
    pub service: ServiceName,
    pub revision: RevisionId,
    pub address: String,
    pub pid: Option<u32>,
    pub state: RuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeState {
    Prepared,
    Running,
    Draining,
    Stopped,
}
