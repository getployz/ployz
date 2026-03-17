use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use pingora::lb::prelude::LoadBalancer;
use pingora::lb::selection::RoundRobin;
use ployz_routes::{BackendView, GatewaySnapshot};

pub struct SnapshotState {
    pub snapshot: Arc<GatewaySnapshot>,
    pub load_balancers: HashMap<String, Arc<LoadBalancer<RoundRobin>>>,
    pub backend_lookup: HashMap<SocketAddr, BackendView>,
}

impl SnapshotState {
    fn build(snapshot: GatewaySnapshot) -> Self {
        let mut load_balancers = HashMap::new();
        let mut backend_lookup = HashMap::new();

        for route in &snapshot.http_routes {
            if route.backends.is_empty() {
                continue;
            }

            let mut addrs = Vec::with_capacity(route.backends.len());
            for backend in &route.backends {
                addrs.push(backend.address.to_string());
                backend_lookup.insert(backend.address, backend.clone());
            }

            let Ok(lb) = LoadBalancer::try_from_iter(addrs) else {
                continue;
            };
            load_balancers.insert(route.route_id.clone(), Arc::new(lb));
        }

        Self {
            snapshot: Arc::new(snapshot),
            load_balancers,
            backend_lookup,
        }
    }
}

#[derive(Clone)]
pub struct SharedSnapshot {
    inner: Arc<RwLock<Arc<SnapshotState>>>,
}

impl SharedSnapshot {
    #[must_use]
    pub fn new(snapshot: GatewaySnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(SnapshotState::build(snapshot)))),
        }
    }

    #[must_use]
    pub fn load(&self) -> Arc<SnapshotState> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn replace(&self, snapshot: GatewaySnapshot) {
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Arc::new(SnapshotState::build(snapshot));
    }
}
