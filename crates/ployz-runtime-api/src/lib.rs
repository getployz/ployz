mod deploy;
mod identity;
mod network;

use async_trait::async_trait;

pub use deploy::{DeployFrame, DeploySession, DeploySessionFactory, StartCandidateRequest};
pub use identity::{Identity, IdentityError};
pub use network::{
    AttachedDataplane, ContainerNetwork, ContainerNetworkBackend, DataplaneFactory, DevicePeer,
    DisconnectMode, EndpointDiscovery, MemoryWireGuard, MeshDataplane, MeshNetwork, ObserveMode,
    StaticEndpointDiscovery, WireGuardDevice, WireguardBackend, WireguardBackendMode,
    WireguardDriver, container_ip, machine_ip,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartableWorkload {
    pub container_name: String,
    pub was_running: bool,
}

#[async_trait]
pub trait ServiceRuntime: Send + Sync {
    async fn start(&self) -> std::result::Result<(), String>;
    async fn stop(&self) -> std::result::Result<(), String>;
    async fn healthy(&self) -> bool;
}

#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String>;

    async fn detach(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopServiceRuntime;

#[async_trait]
impl ServiceRuntime for NoopServiceRuntime {
    async fn start(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn stop(&self) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn healthy(&self) -> bool {
        true
    }
}

pub struct MemoryServiceRuntime {
    started: std::sync::atomic::AtomicBool,
    healthy: std::sync::atomic::AtomicBool,
    fail_start: std::sync::atomic::AtomicBool,
    fail_stop: std::sync::atomic::AtomicBool,
}

impl Default for MemoryServiceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub enum ToggleState {
    Enabled,
    Disabled,
}

pub enum ServiceHealth {
    Healthy,
    Unhealthy,
}

impl MemoryServiceRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: std::sync::atomic::AtomicBool::new(false),
            healthy: std::sync::atomic::AtomicBool::new(true),
            fail_start: std::sync::atomic::AtomicBool::new(false),
            fail_stop: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn set_healthy(&self, health: ServiceHealth) {
        self.healthy.store(
            matches!(health, ServiceHealth::Healthy),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub fn set_fail_start(&self, state: ToggleState) {
        self.fail_start.store(
            matches!(state, ToggleState::Enabled),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub fn set_fail_stop(&self, state: ToggleState) {
        self.fail_stop.store(
            matches!(state, ToggleState::Enabled),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    pub fn is_started(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ServiceRuntime for MemoryServiceRuntime {
    async fn start(&self) -> std::result::Result<(), String> {
        if self
            .fail_start
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ployz_types::Error::operation("service start", "injected failure").to_string());
        }
        self.started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> std::result::Result<(), String> {
        if self.fail_stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ployz_types::Error::operation("service stop", "injected failure").to_string());
        }
        self.started
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.healthy
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}
