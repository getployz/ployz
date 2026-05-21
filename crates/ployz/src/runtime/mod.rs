//! Runtime participant product ports.

use std::time::SystemTime;

use crate::error::RuntimeFailure;
use crate::operation::MutationContext;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkloadId(String);

impl WorkloadId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RuntimeFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineId(String);

impl MachineId {
    pub fn parse(value: impl Into<String>) -> Result<Self, RuntimeFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeActivationRequest {
    pub workload: WorkloadId,
    pub machine: MachineId,
    pub context: MutationContext,
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

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, RuntimeFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(RuntimeFailure::PayloadInvalid);
    }
    Ok(build(value))
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

    #[test]
    fn empty_workload_id_is_payload_invalid() {
        assert_eq!(WorkloadId::parse(""), Err(RuntimeFailure::PayloadInvalid));
    }
}
