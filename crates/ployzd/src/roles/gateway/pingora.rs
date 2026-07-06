//! Pingora-backed HTTP gateway serving.

use crate::roles::gateway::projection::GatewayProjection;
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
use pingora::lb::health_check::TcpHealthCheck;
use pingora::lb::selection::RoundRobin;
use pingora::lb::{Backend, LoadBalancer};
use pingora::protocols::l4::socket::SocketAddr as PingoraSocketAddr;
use pingora::proxy::{FailToProxy, ProxyHttp, Session};
use pingora::upstreams::peer::HttpPeer;
use ployz_core::ops::{RouteHostname, RouteHostnameError, RoutePort, RouteTarget};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct PingoraRouteRegistry {
    inner: Arc<RwLock<BTreeMap<RouteTarget, Arc<PingoraRoutePool>>>>,
}

impl PingoraRouteRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub fn replace_projection(
        &self,
        projection: &GatewayProjection,
    ) -> Result<(), PingoraRouteRegistryError> {
        let mut routes = BTreeMap::new();
        for route in &projection.routes {
            let upstreams = route
                .upstreams
                .iter()
                .map(|upstream| upstream.address)
                .collect::<Vec<_>>();
            routes.insert(
                route.target.clone(),
                Arc::new(PingoraRoutePool::new(upstreams)?),
            );
        }

        *self
            .inner
            .write()
            .expect("pingora route registry lock is not poisoned") = routes;
        Ok(())
    }

    #[must_use]
    pub fn backend_count(&self, target: &RouteTarget) -> usize {
        self.inner
            .read()
            .expect("pingora route registry lock is not poisoned")
            .get(target)
            .map_or(0, |pool| pool.backend_count)
    }

    pub fn select_backend(
        &self,
        target: &RouteTarget,
        tried: &BTreeSet<SocketAddr>,
    ) -> Result<Backend, PingoraRouteSelectionError> {
        let pool = self
            .inner
            .read()
            .expect("pingora route registry lock is not poisoned")
            .get(target)
            .cloned()
            .ok_or_else(|| PingoraRouteSelectionError::NoRoute {
                target: target.clone(),
            })?;

        if pool.backend_count == 0 {
            return Err(PingoraRouteSelectionError::NoHealthyUpstream {
                target: target.clone(),
            });
        }

        pool.load_balancer
            .select_with(b"", pool.backend_count, |backend, healthy| {
                healthy && backend_inet_addr(backend).is_some_and(|addr| !tried.contains(&addr))
            })
            .ok_or_else(|| PingoraRouteSelectionError::NoHealthyUpstream {
                target: target.clone(),
            })
    }

    pub async fn run_health_checks(&self) {
        let pools = self
            .inner
            .read()
            .expect("pingora route registry lock is not poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pool in pools {
            pool.load_balancer
                .backends()
                .run_health_check(pool.load_balancer.parallel_health_check)
                .await;
        }
    }
}

impl Default for PingoraRouteRegistry {
    fn default() -> Self {
        Self::new()
    }
}

struct PingoraRoutePool {
    load_balancer: LoadBalancer<RoundRobin>,
    backend_count: usize,
}

impl PingoraRoutePool {
    fn new(upstreams: Vec<SocketAddr>) -> Result<Self, PingoraRouteRegistryError> {
        let backend_count = upstreams.len();
        let mut load_balancer = LoadBalancer::try_from_iter(upstreams).map_err(|source| {
            PingoraRouteRegistryError::InvalidBackendAddress {
                message: source.to_string(),
            }
        })?;
        load_balancer.set_health_check(TcpHealthCheck::new());
        load_balancer.parallel_health_check = true;

        Ok(Self {
            load_balancer,
            backend_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingoraRouteRegistryError {
    InvalidBackendAddress { message: String },
}

impl fmt::Display for PingoraRouteRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBackendAddress { message } => {
                write!(formatter, "invalid gateway backend address: {message}")
            }
        }
    }
}

impl std::error::Error for PingoraRouteRegistryError {}

#[derive(Clone)]
pub struct PloyzGatewayProxy {
    registry: PingoraRouteRegistry,
    listener_port: RoutePort,
    failure_recorder: GatewayPingoraFailureRecorder,
}

impl PloyzGatewayProxy {
    #[must_use]
    pub fn new(
        registry: PingoraRouteRegistry,
        listener_port: RoutePort,
        failure_recorder: GatewayPingoraFailureRecorder,
    ) -> Self {
        Self {
            registry,
            listener_port,
            failure_recorder,
        }
    }

    fn record_failure(&self, message: String) {
        (self.failure_recorder)(message);
    }
}

pub type GatewayPingoraFailureRecorder = Arc<dyn Fn(String) + Send + Sync>;

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
    ) -> PingoraResult<bool>
    where
        Self::CTX: Send + Sync,
    {
        let Some(authority) = request_authority(session) else {
            self.record_failure("HTTP request is missing authority".to_owned());
            session.respond_error(400).await?;
            return Ok(true);
        };

        let target = match route_target_from_authority(authority, self.listener_port) {
            Ok(target) => target,
            Err(error) => {
                self.record_failure(format!("invalid HTTP authority: {error:?}"));
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
        let backend = self
            .registry
            .select_backend(target, &ctx.tried)
            .map_err(|error| {
                let status = match error {
                    PingoraRouteSelectionError::NoRoute { .. } => 404,
                    PingoraRouteSelectionError::NoHealthyUpstream { .. } => 503,
                };
                Error::explain(HTTPStatus(status), format!("{error:?}"))
            })?;
        let Some(addr) = backend_inet_addr(&backend) else {
            return Error::e_explain(InternalError, "gateway backend is not TCP");
        };
        ctx.tried.insert(addr);
        let mut peer = HttpPeer::new(addr, false, String::new());
        peer.options.connection_timeout = Some(UPSTREAM_CONNECT_TIMEOUT);
        Ok(Box::new(peer))
    }

    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        e: Box<Error>,
    ) -> Box<Error> {
        let Some(target) = ctx.target.as_ref() else {
            return e;
        };
        let mut error = e;
        if ctx.tried.len() < self.registry.backend_count(target) {
            error.set_retry(true);
        }
        error
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &Error,
        _ctx: &mut Self::CTX,
    ) -> FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        self.record_failure(error.to_string());
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingoraRouteSelectionError {
    NoRoute { target: RouteTarget },
    NoHealthyUpstream { target: RouteTarget },
}

fn backend_inet_addr(backend: &Backend) -> Option<SocketAddr> {
    match &backend.addr {
        PingoraSocketAddr::Inet(addr) => Some(*addr),
        PingoraSocketAddr::Unix(_) => None,
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

pub fn route_target_from_authority(
    authority: &str,
    listener_port: RoutePort,
) -> Result<RouteTarget, HttpRouteTargetError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(HttpRouteTargetError::EmptyHost);
    }
    if authority.contains('@') {
        return Err(HttpRouteTargetError::UserInfo);
    }

    let (hostname, port) = host_and_port(authority, listener_port)?;
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|source| HttpRouteTargetError::InvalidHostname { source })?;

    Ok(RouteTarget::new(hostname, port))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRouteTargetError {
    EmptyHost,
    UserInfo,
    MissingHost,
    MissingPort,
    InvalidHostname { source: RouteHostnameError },
    InvalidPort,
    UnsupportedIpv6,
}

fn host_and_port(
    authority: &str,
    listener_port: RoutePort,
) -> Result<(&str, RoutePort), HttpRouteTargetError> {
    let Some((hostname, port)) = authority.rsplit_once(':') else {
        return Ok((authority, listener_port));
    };
    if hostname.contains(':') {
        return Err(HttpRouteTargetError::UnsupportedIpv6);
    }
    if hostname.is_empty() {
        return Err(HttpRouteTargetError::MissingHost);
    }
    if port.is_empty() {
        return Err(HttpRouteTargetError::MissingPort);
    }
    let port = port
        .parse::<u16>()
        .ok()
        .and_then(|value| RoutePort::try_new(value).ok())
        .ok_or(HttpRouteTargetError::InvalidPort)?;

    Ok((hostname, port))
}
