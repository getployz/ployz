use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use pingora::prelude::*;
use ployz_routing::{BackendView, GatewaySnapshot, match_http_route};

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

pub struct GatewayApp {
    snapshot: SharedSnapshot,
    rr_state: Arc<Mutex<HashMap<String, usize>>>,
}

#[derive(Default)]
pub struct RequestCtx {
    backend: Option<BackendView>,
}

impl GatewayApp {
    #[must_use]
    pub fn new(snapshot: SharedSnapshot) -> Self {
        Self {
            snapshot,
            rr_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn select_backend(&self, route_id: &str, backends: &[BackendView]) -> Option<BackendView> {
        let len = (!backends.is_empty()).then_some(backends.len())?;
        let mut rr_state = self
            .rr_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = if let Some(value) = rr_state.get_mut(route_id) {
            value
        } else {
            rr_state.entry(route_id.to_string()).or_insert(0)
        };
        let backend = backends.get(*next % len).cloned()?;
        *next = next.wrapping_add(1);
        Some(backend)
    }
}

#[async_trait]
impl ProxyHttp for GatewayApp {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let request = session.req_header();
        let host = request
            .headers
            .get("host")
            .and_then(|value| value.to_str().ok());
        let path = request.uri.path();
        let snapshot = self.snapshot.load();
        let Some(route) = match_http_route(&snapshot, host, path) else {
            session.respond_error(404).await?;
            return Ok(true);
        };
        let Some(backend) = self.select_backend(&route.route_id, &route.backends) else {
            session.respond_error(503).await?;
            return Ok(true);
        };
        ctx.backend = Some(backend);
        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let Some(backend) = ctx.backend.as_ref() else {
            return Err(Error::explain(
                ErrorType::HTTPStatus(503),
                "backend was not selected",
            ));
        };
        Ok(Box::new(HttpPeer::new(backend.address, false, String::new())))
    }
}
