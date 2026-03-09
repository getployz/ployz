use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use http::Method;
use pingora::prelude::*;
use ployz_routes::{BackendView, match_http_route};
use ployz_sdk::model::InstanceId;
use tracing::info;

use crate::snapshot::SharedSnapshot;

// ---------------------------------------------------------------------------
// GatewayApp — pingora HTTP proxy
// ---------------------------------------------------------------------------

pub struct GatewayApp {
    snapshot: SharedSnapshot,
    rr_state: Arc<Mutex<HashMap<String, usize>>>,
}

#[derive(Default)]
pub struct RequestCtx {
    route_id: Option<String>,
    backends: Vec<BackendView>,
    backend: Option<BackendView>,
    failed_instances: HashSet<InstanceId>,
    retry_count: usize,
    retry_allowed: bool,
    upstream_host: Option<String>,
}

impl GatewayApp {
    #[must_use]
    pub fn new(snapshot: SharedSnapshot) -> Self {
        Self {
            snapshot,
            rr_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn select_backend(
        &self,
        route_id: &str,
        backends: &[BackendView],
        failed_instances: &HashSet<InstanceId>,
    ) -> Option<BackendView> {
        let eligible: Vec<_> = backends
            .iter()
            .filter(|b| !failed_instances.contains(&b.instance_id))
            .collect();
        let len = eligible.len();
        if len == 0 {
            return None;
        }
        let mut rr_state = self
            .rr_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let next = rr_state.entry(route_id.to_string()).or_insert(0);
        let idx = *next % len;
        *next = next.wrapping_add(1);
        Some(eligible[idx].clone())
    }
}

#[async_trait]
impl ProxyHttp for GatewayApp {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<bool> {
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

        *ctx = RequestCtx {
            route_id: Some(route.route_id.clone()),
            backends: route.backends.clone(),
            backend: None,
            failed_instances: HashSet::new(),
            retry_count: 0,
            retry_allowed: is_retryable_method(&request.method),
            upstream_host: host.map(ToOwned::to_owned),
        };

        let Some(backend) = select_backend_for_ctx(self, ctx) else {
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
    ) -> pingora::Result<Box<HttpPeer>> {
        if ctx.backend.is_none() {
            ctx.backend = Some(select_backend_for_ctx(self, ctx).ok_or_else(|| {
                Error::explain(ErrorType::HTTPStatus(503), "no eligible backend")
            })?);
        }
        let Some(backend) = ctx.backend.as_ref() else {
            unreachable!("backend is guaranteed Some by the block above");
        };

        let mut peer = HttpPeer::new(backend.address, false, String::new());
        peer.options.connection_timeout = Some(Duration::from_secs(2));
        peer.options.total_connection_timeout = Some(Duration::from_secs(5));
        peer.options.read_timeout = Some(Duration::from_secs(30));
        peer.options.write_timeout = Some(Duration::from_secs(30));
        peer.options.idle_timeout = Some(Duration::from_secs(60));
        Ok(Box::new(peer))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        if let Some(host) = ctx.upstream_host.as_deref() {
            upstream_request
                .insert_header("Host", host)
                .map_err(|err| {
                    Error::because(ErrorType::InternalError, "insert Host header", err)
                })?;
        }
        if let Some(client_addr) = session.client_addr()
            && let Some(address) = client_addr.as_inet() {
                upstream_request
                    .insert_header("X-Forwarded-For", address.ip().to_string())
                    .map_err(|err| {
                        Error::because(
                            ErrorType::InternalError,
                            "insert X-Forwarded-For header",
                            err,
                        )
                    })?;
            }
        upstream_request
            .insert_header("X-Forwarded-Proto", "http")
            .map_err(|err| {
                Error::because(
                    ErrorType::InternalError,
                    "insert X-Forwarded-Proto header",
                    err,
                )
            })?;
        Ok(())
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        if !ctx.retry_allowed || ctx.retry_count >= 1 {
            return e;
        }
        let Some(previous) = ctx.backend.take() else {
            return e;
        };
        ctx.failed_instances.insert(previous.instance_id.clone());
        let Some(route_id) = ctx.route_id.as_deref() else {
            ctx.backend = Some(previous);
            return e;
        };
        let Some(next_backend) =
            self.select_backend(route_id, &ctx.backends, &ctx.failed_instances)
        else {
            ctx.backend = Some(previous);
            return e;
        };

        ctx.backend = Some(next_backend);
        ctx.retry_count = ctx.retry_count.saturating_add(1);
        let mut retry_error = e;
        retry_error.set_retry(true);
        retry_error
    }

    async fn logging(&self, session: &mut Session, e: Option<&Error>, ctx: &mut Self::CTX) {
        let request = session.req_header();
        let route_id = ctx.route_id.as_deref().unwrap_or("-");
        let backend = ctx
            .backend
            .as_ref()
            .map(|backend| backend.instance_id.0.as_str())
            .unwrap_or("-");
        info!(
            method = %request.method,
            path = %request.uri.path(),
            route_id,
            backend,
            retry_count = ctx.retry_count,
            failed = e.is_some(),
            "gateway request completed"
        );
    }
}

fn select_backend_for_ctx(app: &GatewayApp, ctx: &RequestCtx) -> Option<BackendView> {
    let route_id = ctx.route_id.as_deref()?;
    app.select_backend(route_id, &ctx.backends, &ctx.failed_instances)
}

fn is_retryable_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}
