use async_trait::async_trait;
use ployz_runtime_api::{Result, RuntimeError, RuntimeHandle, ServiceRuntime};

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> Result<()> {
        Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToggleState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    #[must_use]
    pub fn is_started(&self) -> bool {
        self.started.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ServiceRuntime for MemoryServiceRuntime {
    async fn start(&self) -> Result<()> {
        if self.fail_start.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(RuntimeError::operation("service start", "injected failure"));
        }
        self.started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        if self.fail_stop.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(RuntimeError::operation("service stop", "injected failure"));
        }
        self.started
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.healthy.load(std::sync::atomic::Ordering::SeqCst)
    }
}
