//! Typed requests and durable outcomes for one Machine upgrade.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Request one exact or channel-selected Machine release under a stable retry identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestMachineUpgradeRequest {
    /// Identity reused by every retry of the same request.
    pub attempt_id: crate::MachineUpgradeAttemptId,
    /// Exact version or supported release channel to resolve once.
    pub release: crate::MachineRelease,
}

/// Read the latest attempt, optionally requiring one exact retry identity.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectMachineUpgradeRequest {
    /// Required attempt identity, or the latest local attempt when absent.
    #[serde(default)]
    pub attempt_id: Option<crate::MachineUpgradeAttemptId>,
}

crate::value::open_string_enum!(MachineUpgradeStage, Unknown {
    Launching => "launching",
    Preparing => "preparing",
    Acquiring => "acquiring",
    Verifying => "verifying",
    Activating => "activating",
    Restarting => "restarting",
    Readiness => "readiness",
});

/// Durable local evidence for one bounded Machine upgrade attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineUpgradeAttempt {
    /// Stable identity assigned before the request is dispatched.
    pub attempt_id: crate::MachineUpgradeAttemptId,
    /// Exact version resolved before any host mutation.
    pub target: crate::MachineVersion,
    /// State-specific evidence for the attempt.
    #[serde(flatten)]
    #[ts(flatten)]
    pub outcome: MachineUpgradeOutcome,
}

/// State-specific evidence for a Machine upgrade attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MachineUpgradeOutcome {
    /// The durable receipt and active marker exist and worker launch was accepted.
    Accepted,
    /// The independent worker reached this installation stage.
    Running {
        /// Latest stage durably recorded by the worker.
        stage: MachineUpgradeStage,
    },
    /// The target daemon was observed running and ready through its local API.
    Succeeded {
        /// Exact version reported by the ready, activated daemon.
        version: crate::MachineVersion,
    },
    /// The worker recorded a terminal installation failure.
    Failed {
        /// First installation stage that did not complete.
        stage: MachineUpgradeStage,
        /// Operator-facing failure evidence.
        error: String,
    },
    /// A positively stopped worker left no terminal result.
    Interrupted {
        /// Last durable stage before the worker stopped.
        stage: MachineUpgradeStage,
    },
}

impl MachineUpgradeAttempt {
    /// Whether the attempt has durable terminal evidence.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.outcome,
            MachineUpgradeOutcome::Succeeded { .. }
                | MachineUpgradeOutcome::Failed { .. }
                | MachineUpgradeOutcome::Interrupted { .. }
        )
    }
}
