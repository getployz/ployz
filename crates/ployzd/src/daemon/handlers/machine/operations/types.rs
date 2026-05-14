use serde::{Deserialize, Serialize};

use ployz_api::MachineOperationInfo;
use ployz_model::MachineId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::daemon::handlers::machine) enum MachineOperationKind {
    Init,
    Add,
    Update,
    StoragePromote,
}

impl MachineOperationKind {
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Add => "add",
            Self::Update => "update",
            Self::StoragePromote => "storage-promote",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(in crate::daemon::handlers::machine) enum MachineOperationStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl MachineOperationStatus {
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineOperationTransition {
    status: MachineOperationStatus,
    last_error: Option<String>,
    at_unix_secs: u64,
}

impl MachineOperationTransition {
    pub(super) fn succeed(at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Succeeded,
            last_error: None,
            at_unix_secs,
        }
    }

    pub(super) fn fail(last_error: String, at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Failed,
            last_error: Some(last_error),
            at_unix_secs,
        }
    }

    pub(super) fn interrupt(last_error: Option<String>, at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Interrupted,
            last_error,
            at_unix_secs,
        }
    }

    pub(super) fn running(last_error: Option<String>, at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Running,
            last_error,
            at_unix_secs,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(in crate::daemon::handlers::machine) struct MachineOperationArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_subnet: Option<String>,
    #[serde(default)]
    pub uses_operation_identity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub(in crate::daemon::handlers::machine) enum MachineOperationState {
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Succeeded {},
    Failed {
        last_error: String,
    },
    Interrupted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
}

impl MachineOperationState {
    #[must_use]
    fn status(&self) -> MachineOperationStatus {
        match self {
            Self::Running { .. } => MachineOperationStatus::Running,
            Self::Succeeded { .. } => MachineOperationStatus::Succeeded,
            Self::Failed { .. } => MachineOperationStatus::Failed,
            Self::Interrupted { .. } => MachineOperationStatus::Interrupted,
        }
    }

    #[must_use]
    fn last_error(&self) -> Option<&str> {
        match self {
            Self::Running { last_error } | Self::Interrupted { last_error } => {
                last_error.as_deref()
            }
            Self::Failed { last_error } => Some(last_error.as_str()),
            Self::Succeeded { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::daemon::handlers::machine) struct MachineOperationRecord {
    pub id: String,
    pub kind: MachineOperationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    pub stage: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub state: MachineOperationState,
    #[serde(default)]
    pub artifacts: MachineOperationArtifacts,
}

impl MachineOperationRecord {
    #[must_use]
    pub(in crate::daemon::handlers::machine) fn status(&self) -> MachineOperationStatus {
        self.state.status()
    }

    #[must_use]
    pub(in crate::daemon::handlers::machine) fn last_error(&self) -> Option<&str> {
        self.state.last_error()
    }

    #[must_use]
    pub(in crate::daemon::handlers::machine) fn info(&self) -> MachineOperationInfo {
        MachineOperationInfo {
            id: self.id.clone(),
            kind: self.kind.as_str().into(),
            network_name: self.network_name.clone(),
            targets: self.targets.clone(),
            status: self.status().as_str().into(),
            stage: self.stage.clone(),
            started_at: self.started_at,
            updated_at: self.updated_at,
            last_error: self.last_error().map(str::to_string),
            machine_id: self.artifacts.machine_id.clone(),
            invite_id: self.artifacts.invite_id.clone(),
            allocated_subnet: self.artifacts.allocated_subnet.clone(),
        }
    }

    pub(super) fn apply_transition(&mut self, transition: MachineOperationTransition) {
        let MachineOperationTransition {
            status,
            last_error,
            at_unix_secs,
        } = transition;
        self.state = match status {
            MachineOperationStatus::Running => MachineOperationState::Running {
                last_error: last_error.or_else(|| self.last_error().map(str::to_string)),
            },
            MachineOperationStatus::Succeeded => MachineOperationState::Succeeded {},
            MachineOperationStatus::Failed => MachineOperationState::Failed {
                last_error: last_error.unwrap_or_else(|| "machine operation failed".into()),
            },
            MachineOperationStatus::Interrupted => {
                MachineOperationState::Interrupted { last_error }
            }
        };
        self.updated_at = at_unix_secs;
    }
}
