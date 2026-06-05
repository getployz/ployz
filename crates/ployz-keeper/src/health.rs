//! Health gates for keeper-managed local substrate changes.

use ployz_core::roles::DaemonProcessRole;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleHealthGate {
    pub role: DaemonProcessRole,
    pub timeout_seconds: u16,
}

impl RoleHealthGate {
    #[must_use]
    pub const fn new(role: DaemonProcessRole, timeout_seconds: u16) -> Self {
        Self {
            role,
            timeout_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperStepFailureReason {
    HostPrerequisiteFailed,
    ArtifactVerificationFailed,
    SupervisorWriteFailed,
    SupervisorStartFailed,
    HealthGateFailed,
    ProgressReportFailed,
}
