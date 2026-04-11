//! Runtime-facing contracts for service supervision, networking, and deploy sessions.
//!
//! The orchestrator depends on these traits and value types instead of importing
//! backend-specific process, network, or transport implementations upward.

mod deploy;
mod network;

use async_trait::async_trait;
use thiserror::Error;

pub use deploy::{
    DeployFrame, DeploySession, DeploySessionFactory, PreDeployHookRequest, StartCandidateRequest,
};
pub use network::{
    AttachedDataplane, ContainerNetwork, ContainerNetworkBackend, DataplaneFactory, DevicePeer,
    DisconnectMode, EndpointDiscovery, MeshDataplane, MeshNetwork, ObserveMode, WireGuardDevice,
    WireguardBackend, WireguardBackendMode, WireguardDriver,
};

/// Result type used by runtime and service lifecycle contracts.
pub type Result<T> = std::result::Result<T, RuntimeError>;

/// Error returned by runtime-facing service and process control seams.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("{operation}: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
    #[error(transparent)]
    Domain(#[from] ployz_types::Error),
}

impl RuntimeError {
    #[must_use]
    pub fn operation(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            message: message.into(),
        }
    }
}

/// Runtime-managed workload that should be restarted or reconnected after repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartableWorkload {
    /// Runtime-local container name.
    pub container_name: String,
    /// Whether the workload was running before being interrupted.
    pub was_running: bool,
}

/// Start/stop/health contract for a managed service runtime.
#[async_trait]
pub trait ServiceRuntime: Send + Sync {
    /// Start the managed service if it is not already running.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the underlying runtime cannot start or
    /// adopt the managed service.
    async fn start(&self) -> Result<()>;

    /// Stop the managed service if it is running.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when the underlying runtime cannot stop the
    /// managed service cleanly.
    async fn stop(&self) -> Result<()>;

    /// Report whether the service is currently healthy enough for orchestration.
    async fn healthy(&self) -> bool;
}

/// Owned handle for a long-running managed process or listener.
#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    /// Shut the managed resource down.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when shutdown fails.
    async fn shutdown(self: Box<Self>) -> Result<()>;

    /// Detach from the managed resource without necessarily stopping it.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when detach fails.
    async fn detach(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

/// No-op runtime handle used when no managed process exists in the current mode.
pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}
