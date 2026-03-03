use crate::dataplane::traits::{PortError, PortResult, ServiceControl};
use std::sync::atomic::{AtomicBool, Ordering};

pub struct SystemdService {
    pub unit: String,
    started: AtomicBool,
}

impl SystemdService {
    pub fn new(unit: impl Into<String>) -> Self {
        Self {
            unit: unit.into(),
            started: AtomicBool::new(false),
        }
    }
}

impl ServiceControl for SystemdService {
    async fn start(&self) -> PortResult<()> {
        if self.unit.is_empty() {
            return Err(PortError::operation("systemd start", "missing unit name"));
        }
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> PortResult<()> {
        self.started.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn healthy(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }
}
