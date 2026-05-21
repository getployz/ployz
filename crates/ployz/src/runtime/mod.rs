//! Runtime participant product ports.

use std::time::SystemTime;

use crate::error::RuntimeFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkloadId(String);

impl WorkloadId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineId(String);

impl MachineId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActivationRequest {
    pub workload: WorkloadId,
    pub machine: MachineId,
    pub deadline: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantReceipt {
    pub workload: WorkloadId,
    pub machine: MachineId,
    pub revision: RuntimeRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeRevision(u64);

impl RuntimeRevision {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeActivationOutcome {
    Activated(ParticipantReceipt),
    Failed(RuntimeFailure),
}

pub trait RuntimePort {
    fn activate_participant(
        &self,
        request: RuntimeActivationRequest,
    ) -> Result<RuntimeActivationOutcome, RuntimeFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_timeout_is_structured() {
        let outcome = RuntimeActivationOutcome::Failed(RuntimeFailure::Timeout);

        assert!(matches!(
            outcome,
            RuntimeActivationOutcome::Failed(RuntimeFailure::Timeout)
        ));
    }
}
