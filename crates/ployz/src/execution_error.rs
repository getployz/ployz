//! Command-level aggregation of shared and feature-owned execution failures.

use crate::core::runtime::CoreRuntimeError;
use crate::deploy::runtime::DeployExecutionError;
use crate::execution_support::ExecutionSupportError;
use crate::machine::runtime::MachineExecutionError;
use crate::namespace::runtime::NamespaceExecutionError;
use crate::volume::runtime::VolumeExecutionError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PloyzctlExecutionError {
    #[error("telemetry preferences are handled locally by the ployz binary")]
    LocalCommand,
    #[error(
        "ployz {command} is reserved for Ployz Cloud and no Cloud connection is configured; configure a Ployz Cloud connection before using Cloud verbs"
    )]
    CloudUnconfigured { command: &'static str },
    #[error(transparent)]
    Support(#[from] ExecutionSupportError),
    #[error(transparent)]
    Core(CoreRuntimeError),
    #[error(transparent)]
    Machine(MachineExecutionError),
    #[error(transparent)]
    Deploy(DeployExecutionError),
    #[error(transparent)]
    Namespace(NamespaceExecutionError),
    #[error(transparent)]
    Volume(VolumeExecutionError),
}
