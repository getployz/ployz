//! Pingora-backed HTTP gateway serving.

use crate::roles::gateway::projection::GatewayProjection;
use async_trait::async_trait;
use bytes::Bytes;
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
use pingora::http::ResponseHeader;
use pingora::lb::health_check::TcpHealthCheck;
use pingora::lb::selection::RoundRobin;
use pingora::lb::{Backend, LoadBalancer};
use pingora::listeners::TlsAccept;
use pingora::protocols::l4::socket::SocketAddr as PingoraSocketAddr;
use pingora::protocols::tls::TlsRef;
use pingora::proxy::{FailToProxy, ProxyHttp, Session};
use pingora::upstreams::peer::HttpPeer;
use ployz_core::cert::{AcmeHttp01Challenge, CertificateChallengeApplicationStatus};
use ployz_core::ingress::{CertificateOwner, RouteBindingOrigin};
use ployz_core::ops::{RouteHostname, RouteHostnameError, RoutePort, RouteTarget};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct PingoraRouteRegistry {
    inner: Arc<RwLock<PingoraGatewaySnapshot>>,
}

struct PingoraGatewaySnapshot {
    routes: BTreeMap<RouteTarget, Arc<PingoraRoutePool>>,
    tls_by_hostname: BTreeMap<String, PreparedGatewayTls>,
    https_hostnames: BTreeSet<String>,
    challenges: BTreeMap<(String, String), AppliedHttp01Challenge>,
}

struct AppliedHttp01Challenge {
    value: String,
    expires_at_unix_seconds: u64,
}

#[derive(Clone)]
struct PreparedGatewayTls {
    certificates: Vec<pingora::tls::x509::X509>,
    private_key: pingora::tls::pkey::PKey<pingora::tls::pkey::Private>,
}

impl PingoraRouteRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(PingoraGatewaySnapshot {
                routes: BTreeMap::new(),
                tls_by_hostname: BTreeMap::new(),
                https_hostnames: BTreeSet::new(),
                challenges: BTreeMap::new(),
            })),
        }
    }

    pub fn replace_projection(
        &self,
        projection: &GatewayProjection,
    ) -> Result<(), PingoraRouteRegistryError> {
        let mut snapshot = prepare_gateway_snapshot(projection)?;
        let mut current = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        snapshot.challenges = std::mem::take(&mut current.challenges);
        *current = snapshot;
        Ok(())
    }

    pub fn apply_challenge(&self, challenge: &AcmeHttp01Challenge) {
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .challenges
            .insert(
                (
                    challenge.hostname().as_str().to_owned(),
                    challenge.token().as_str().to_owned(),
                ),
                AppliedHttp01Challenge {
                    value: challenge.value().as_str().to_owned(),
                    expires_at_unix_seconds: current_unix_seconds()
                        .saturating_add(challenge.ttl_seconds().get()),
                },
            );
    }

    pub fn remove_challenge(&self, challenge: &AcmeHttp01Challenge) {
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .challenges
            .remove(&(
                challenge.hostname().as_str().to_owned(),
                challenge.token().as_str().to_owned(),
            ));
    }

    #[must_use]
    pub fn is_https_hostname(&self, hostname: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .https_hostnames
            .contains(&hostname.to_ascii_lowercase())
    }

    #[must_use]
    pub fn http01_challenge(&self, hostname: &str, token: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .challenges
            .get(&(hostname.to_ascii_lowercase(), token.to_owned()))
            .filter(|challenge| current_unix_seconds() < challenge.expires_at_unix_seconds)
            .map(|challenge| challenge.value.clone())
    }

    #[must_use]
    pub fn challenge_application_status(
        &self,
        challenge: &AcmeHttp01Challenge,
    ) -> CertificateChallengeApplicationStatus {
        if self.http01_challenge(challenge.hostname().as_str(), challenge.token().as_str())
            == Some(challenge.value().as_str().to_owned())
        {
            CertificateChallengeApplicationStatus::Applied
        } else {
            CertificateChallengeApplicationStatus::NotApplied
        }
    }

    #[must_use]
    pub fn backend_count(&self, target: &RouteTarget) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .routes
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .routes
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .routes
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

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PingoraRouteRegistryError {
    #[error("invalid gateway backend address: {message}")]
    InvalidBackendAddress { message: String },
    #[error("invalid gateway certificate chain: {message}")]
    InvalidCertificateChain { message: String },
    #[error("invalid gateway private key: {message}")]
    InvalidPrivateKey { message: String },
    #[error("gateway certificate does not match its private key")]
    CertificateKeyMismatch,
}

#[derive(Clone)]
pub struct PloyzGatewayProxy {
    registry: PingoraRouteRegistry,
    listener_port: RoutePort,
    failure_recorder: GatewayPingoraFailureRecorder,
    redirect_managed_http: bool,
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
            redirect_managed_http: false,
        }
    }

    #[must_use]
    pub fn redirecting_managed_http(
        registry: PingoraRouteRegistry,
        listener_port: RoutePort,
        failure_recorder: GatewayPingoraFailureRecorder,
    ) -> Self {
        Self {
            registry,
            listener_port,
            failure_recorder,
            redirect_managed_http: true,
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

        if self.redirect_managed_http {
            let hostname = authority.split(':').next().unwrap_or(authority);
            let path = session.downstream_session.req_header().uri.path();
            if let Some(value) =
                http01_token(path).and_then(|token| self.registry.http01_challenge(hostname, token))
            {
                let mut response = ResponseHeader::build(200, Some(2))?;
                response.insert_header("content-type", "application/octet-stream")?;
                response.insert_header("content-length", value.len().to_string())?;
                session
                    .write_response_header(Box::new(response), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from(value)), true)
                    .await?;
                return Ok(true);
            }
            if self.registry.is_https_hostname(hostname) {
                let location = format!(
                    "https://{}{}",
                    hostname,
                    session.downstream_session.req_header().uri
                );
                let mut response = ResponseHeader::build(301, Some(1))?;
                response.insert_header("location", location)?;
                session
                    .write_response_header(Box::new(response), true)
                    .await?;
                return Ok(true);
            }
        }

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

pub struct GatewayTlsAccept {
    snapshot: Arc<RwLock<PingoraGatewaySnapshot>>,
}

impl GatewayTlsAccept {
    #[must_use]
    pub fn new(registry: &PingoraRouteRegistry) -> Self {
        Self {
            snapshot: Arc::clone(&registry.inner),
        }
    }
}

#[async_trait]
impl TlsAccept for GatewayTlsAccept {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        let Some(server_name) = ssl.servername(pingora::tls::ssl::NameType::HOST_NAME) else {
            return;
        };
        let prepared = {
            let snapshot = self
                .snapshot
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot
                .tls_by_hostname
                .get(&server_name.to_ascii_lowercase())
                .map(|tls| (tls.certificates.clone(), tls.private_key.clone()))
        };
        let Some((certificates, private_key)) = prepared else {
            return;
        };
        let Some(certificate) = certificates.first() else {
            return;
        };
        if pingora::tls::ext::ssl_use_certificate(ssl, certificate).is_err()
            || pingora::tls::ext::ssl_use_private_key(ssl, &private_key).is_err()
        {
            return;
        }
        for certificate in certificates.into_iter().skip(1) {
            if ssl.add_chain_cert(certificate).is_err() {
                return;
            }
        }
    }
}

fn prepare_gateway_snapshot(
    projection: &GatewayProjection,
) -> Result<PingoraGatewaySnapshot, PingoraRouteRegistryError> {
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

    let mut tls_by_hostname = BTreeMap::new();
    let mut https_hostnames = BTreeSet::new();
    for certificate in &projection.certificate_bundles {
        let tls = prepare_gateway_tls(
            certificate.bundle.certificate_chain_pem(),
            certificate.bundle.private_key_pem(),
        )?;
        for route in projection
            .routes
            .iter()
            .filter(|route| match &certificate.owner {
                CertificateOwner::PloyzAutomaticNamespace => {
                    route.origin == RouteBindingOrigin::Automatic
                }
                CertificateOwner::RouteBinding { route_binding_id } => {
                    &route.id == route_binding_id
                }
            })
        {
            let hostname = route.target.hostname.as_str().to_owned();
            tls_by_hostname.insert(hostname.clone(), tls.clone());
            https_hostnames.insert(hostname);
        }
    }

    let challenges = BTreeMap::new();

    Ok(PingoraGatewaySnapshot {
        routes,
        tls_by_hostname,
        https_hostnames,
        challenges,
    })
}

fn prepare_gateway_tls(
    certificate_chain_pem: &str,
    private_key_pem: &str,
) -> Result<PreparedGatewayTls, PingoraRouteRegistryError> {
    let certificates = pingora::tls::x509::X509::stack_from_pem(certificate_chain_pem.as_bytes())
        .map_err(|error| PingoraRouteRegistryError::InvalidCertificateChain {
        message: error.to_string(),
    })?;
    if certificates.is_empty() {
        return Err(PingoraRouteRegistryError::InvalidCertificateChain {
            message: "certificate chain is empty".to_owned(),
        });
    }
    let private_key = pingora::tls::pkey::PKey::private_key_from_pem(private_key_pem.as_bytes())
        .map_err(|error| PingoraRouteRegistryError::InvalidPrivateKey {
            message: error.to_string(),
        })?;
    let Some(leaf) = certificates.first() else {
        return Err(PingoraRouteRegistryError::InvalidCertificateChain {
            message: "certificate chain is empty".to_owned(),
        });
    };
    let leaf_public_key =
        leaf.public_key()
            .map_err(|error| PingoraRouteRegistryError::InvalidCertificateChain {
                message: error.to_string(),
            })?;
    if !leaf_public_key.public_eq(&private_key) {
        return Err(PingoraRouteRegistryError::CertificateKeyMismatch);
    }
    Ok(PreparedGatewayTls {
        certificates,
        private_key,
    })
}

fn http01_token(path: &str) -> Option<&str> {
    let token = path.strip_prefix("/.well-known/acme-challenge/")?;
    (!token.is_empty() && !token.contains('/')).then_some(token)
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

    let (hostname, _port) = host_and_port(authority, listener_port)?;
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|source| HttpRouteTargetError::InvalidHostname { source })?;

    Ok(RouteTarget::new(hostname))
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
