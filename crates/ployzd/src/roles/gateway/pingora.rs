//! Pingora request serving over one atomically replaceable row projection.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use pingora::Error;
use pingora::ErrorType::{
    AcceptError, BindError, ConnectError, ConnectNoRoute, ConnectProxyFailure, ConnectRefused,
    ConnectTimedout, ConnectionClosed, Custom, CustomCode, FileCreateError, FileOpenError,
    FileReadError, FileWriteError, H1Error, H2Downgrade, H2Error, HandshakeError, InvalidCert,
    InvalidH2, InvalidHTTPHeader, ReadError, ReadTimedout, SocketError, TLSHandshakeFailure,
    TLSHandshakeTimedout, TLSWantX509Lookup, UnknownError, WriteError, WriteTimedout,
};
use pingora::ErrorType::{HTTPStatus, InternalError};
use pingora::Result as PingoraResult;
use pingora::proxy::{FailToProxy, ProxyHttp, Session};
use pingora::upstreams::peer::HttpPeer;
use ployz_core::operation::{RouteHostname, RouteHostnameError, RouteTarget};

use super::projection::GatewayProjection;

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub struct PingoraRouteRegistry {
    inner: Arc<RwLock<Arc<GatewaySnapshot>>>,
}

#[derive(Default)]
struct GatewaySnapshot {
    routes: BTreeMap<RouteTarget, Arc<RoutePool>>,
}

struct RoutePool {
    upstreams: Vec<SocketAddr>,
    next: AtomicUsize,
}

impl PingoraRouteRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds off-lock and publishes one immutable all-route snapshot.
    pub fn replace_projection(&self, projection: &GatewayProjection) {
        let routes = projection
            .routes
            .iter()
            .map(|route| {
                (
                    route.target.clone(),
                    Arc::new(RoutePool {
                        upstreams: route
                            .upstreams
                            .iter()
                            .map(|upstream| upstream.address)
                            .collect(),
                        next: AtomicUsize::new(0),
                    }),
                )
            })
            .collect();
        *self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Arc::new(GatewaySnapshot { routes });
    }

    #[must_use]
    pub fn backend_count(&self, target: &RouteTarget) -> usize {
        self.snapshot()
            .routes
            .get(target)
            .map_or(0, |pool| pool.upstreams.len())
    }

    pub fn select_backend(
        &self,
        target: &RouteTarget,
        tried: &BTreeSet<SocketAddr>,
    ) -> Result<SocketAddr, PingoraRouteSelectionError> {
        let snapshot = self.snapshot();
        let Some(pool) = snapshot.routes.get(target) else {
            return Err(PingoraRouteSelectionError::NoRoute {
                target: target.clone(),
            });
        };
        if pool.upstreams.is_empty() {
            return Err(PingoraRouteSelectionError::NoUpstream {
                target: target.clone(),
            });
        }
        let start = pool.next.fetch_add(1, Ordering::Relaxed) % pool.upstreams.len();
        pool.upstreams
            .iter()
            .cycle()
            .skip(start)
            .take(pool.upstreams.len())
            .copied()
            .find(|address| !tried.contains(address))
            .ok_or_else(|| PingoraRouteSelectionError::NoUpstream {
                target: target.clone(),
            })
    }

    fn snapshot(&self) -> Arc<GatewaySnapshot> {
        Arc::clone(
            &self
                .inner
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}

#[derive(Clone)]
pub struct PloyzGatewayProxy {
    registry: PingoraRouteRegistry,
}

impl PloyzGatewayProxy {
    #[must_use]
    pub const fn new(registry: PingoraRouteRegistry) -> Self {
        Self { registry }
    }
}

#[derive(Default)]
pub struct GatewayPingoraContext {
    target: Option<RouteTarget>,
    tried: BTreeSet<SocketAddr>,
}

#[async_trait]
impl ProxyHttp for PloyzGatewayProxy {
    type CTX = GatewayPingoraContext;

    fn new_ctx(&self) -> Self::CTX {
        GatewayPingoraContext::default()
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<bool> {
        let Some(authority) = request_authority(session) else {
            session.respond_error(400).await?;
            return Ok(true);
        };
        let target = match route_target_from_authority(authority) {
            Ok(target) => target,
            Err(_) => {
                session.respond_error(400).await?;
                return Ok(true);
            }
        };
        ctx.target = Some(target);
        Ok(false)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> PingoraResult<Box<HttpPeer>> {
        let Some(target) = ctx.target.as_ref() else {
            return Error::e_explain(HTTPStatus(400), "missing gateway route target");
        };
        let address = self
            .registry
            .select_backend(target, &ctx.tried)
            .map_err(|error| {
                let status = selection_error_status(&error);
                Error::explain(HTTPStatus(status), error.to_string())
            })?;
        ctx.tried.insert(address);
        let mut peer = HttpPeer::new(address, false, String::new());
        peer.options.connection_timeout = Some(UPSTREAM_CONNECT_TIMEOUT);
        Ok(Box::new(peer))
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut error: Box<Error>,
    ) -> Box<Error> {
        if ctx
            .target
            .as_ref()
            .is_some_and(|target| ctx.tried.len() < self.registry.backend_count(target))
        {
            error.set_retry(true);
        }
        error
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &Error,
        _ctx: &mut Self::CTX,
    ) -> FailToProxy {
        let code = match error.etype() {
            HTTPStatus(code) => *code,
            ConnectTimedout | ConnectRefused | ConnectNoRoute | TLSWantX509Lookup
            | TLSHandshakeFailure | TLSHandshakeTimedout | InvalidCert | HandshakeError
            | ConnectError | BindError | AcceptError | SocketError | ConnectProxyFailure
            | InvalidHTTPHeader | H1Error | H2Error | H2Downgrade | InvalidH2 | ReadError
            | WriteError | ReadTimedout | WriteTimedout | ConnectionClosed | FileOpenError
            | FileCreateError | FileReadError | FileWriteError | InternalError | UnknownError
            | Custom(_) | CustomCode(..) => 503,
        };
        if code > 0 {
            let _ = session.respond_error(code).await;
        }
        FailToProxy {
            error_code: code,
            can_reuse_downstream: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PingoraRouteSelectionError {
    #[error("no route for {target:?}")]
    NoRoute { target: RouteTarget },
    #[error("no upstream for {target:?}")]
    NoUpstream { target: RouteTarget },
}

const fn selection_error_status(error: &PingoraRouteSelectionError) -> u16 {
    match error {
        PingoraRouteSelectionError::NoRoute { .. } => 404,
        PingoraRouteSelectionError::NoUpstream { .. } => 503,
    }
}

fn request_authority(session: &Session) -> Option<&str> {
    if let Some(authority) = session.req_header().uri.authority() {
        return Some(authority.as_str());
    }
    session
        .req_header()
        .headers
        .get("host")
        .and_then(|value| value.to_str().ok())
}

pub fn route_target_from_authority(authority: &str) -> Result<RouteTarget, HttpRouteTargetError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(HttpRouteTargetError::EmptyHost);
    }
    if authority.contains('@') {
        return Err(HttpRouteTargetError::UserInfo);
    }
    let hostname = match authority.rsplit_once(':') {
        Some((hostname, port)) if !hostname.contains(':') => {
            if hostname.is_empty() {
                return Err(HttpRouteTargetError::MissingHost);
            }
            if port.parse::<u16>().is_err() {
                return Err(HttpRouteTargetError::InvalidPort);
            }
            hostname
        }
        Some(_) => return Err(HttpRouteTargetError::UnsupportedIpv6),
        None => authority,
    };
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|source| HttpRouteTargetError::InvalidHostname { source })?;
    Ok(RouteTarget::new(hostname))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRouteTargetError {
    EmptyHost,
    UserInfo,
    MissingHost,
    InvalidHostname { source: RouteHostnameError },
    InvalidPort,
    UnsupportedIpv6,
}

#[cfg(test)]
#[path = "pingora_tests.rs"]
mod tests;
