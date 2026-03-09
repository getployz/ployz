use std::sync::{Arc, RwLock};

use ployz_routes::GatewaySnapshot;

#[derive(Clone)]
pub struct SharedSnapshot {
    inner: Arc<RwLock<Arc<GatewaySnapshot>>>,
}

impl SharedSnapshot {
    #[must_use]
    pub fn new(snapshot: GatewaySnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    #[must_use]
    pub fn load(&self) -> Arc<GatewaySnapshot> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace(&self, snapshot: GatewaySnapshot) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(snapshot);
    }
}
