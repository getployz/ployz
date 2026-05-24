//! Product-neutral peer bootstrap, identity, and probe primitives.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    str::FromStr,
    thread,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use iroh::{EndpointAddr, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use irpc::{Client, WithChannels, channel::oneshot, rpc::RemoteService, rpc_requests};
use irpc_iroh::{IrohLazyRemoteConnection, IrohProtocol};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::IrohEndpointId;

pub const PLOYZ_PEER_ALPN: &[u8] = b"ployz/peer-rpc/0";
pub type PeerEndpoint = iroh::Endpoint;

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer ticket is malformed")]
    MalformedTicket,
    #[error("peer identity is malformed")]
    MalformedIdentity,
    #[error("peer identity storage failed")]
    IdentityIo { source: std::io::Error },
    #[error("peer endpoint bind failed: {message}")]
    EndpointBind { message: String },
    #[error("peer endpoint did not become online before deadline")]
    EndpointOnlineTimeout,
    #[error("peer probe failed for {endpoint}: {reason}")]
    ProbeFailed {
        endpoint: IrohEndpointId,
        reason: String,
    },
    #[error("peer rpc timed out")]
    RpcTimeout,
    #[error("peer rpc transport failed: {message}")]
    RpcTransport { message: String },
    #[error("peer rpc runtime failed: {message}")]
    RpcRuntime { message: String },
    #[error("peer rpc service is not local")]
    RemoteServiceCannotListen,
}

pub type PeerProbeResult<T> = std::result::Result<T, PeerError>;

#[derive(Debug, Clone)]
pub struct PeerTicket {
    ticket: EndpointTicket,
}

impl PeerTicket {
    pub fn parse(value: &str) -> PeerProbeResult<Self> {
        import_ticket(value)
    }

    #[must_use]
    pub fn encoded(&self) -> String {
        self.ticket.to_string()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> IrohEndpointId {
        IrohEndpointId::parse(self.ticket.endpoint_addr().id.to_string())
            .expect("iroh endpoint ids are non-empty")
    }

    #[must_use]
    pub fn endpoint_addr(&self) -> &EndpointAddr {
        self.ticket.endpoint_addr()
    }

    #[must_use]
    pub fn path(&self) -> PeerTicketPath {
        let has_direct_ip = self.ticket.endpoint_addr().ip_addrs().next().is_some();
        let has_relay = self.ticket.endpoint_addr().relay_urls().next().is_some();

        match (has_direct_ip, has_relay) {
            (true, true) => PeerTicketPath::DirectOrRelay,
            (true, false) => PeerTicketPath::DirectOnly,
            (false, true) => PeerTicketPath::RelayOnly,
            (false, false) => PeerTicketPath::DiscoveryOnly,
        }
    }

    #[must_use]
    pub fn redacted(&self) -> String {
        redact_ascii(&self.encoded(), 10, 8, "<redacted-ticket>")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTicketPath {
    DirectOnly,
    RelayOnly,
    DirectOrRelay,
    DiscoveryOnly,
}

#[must_use]
pub fn issue_ticket(endpoint_addr: EndpointAddr) -> PeerTicket {
    PeerTicket {
        ticket: EndpointTicket::new(endpoint_addr),
    }
}

pub fn import_ticket(value: &str) -> PeerProbeResult<PeerTicket> {
    EndpointTicket::from_str(value)
        .map(|ticket| PeerTicket { ticket })
        .map_err(|_| PeerError::MalformedTicket)
}

pub async fn issue_endpoint_ticket(
    endpoint: &PeerEndpoint,
    deadline: PeerProbeDeadline,
) -> PeerProbeResult<PeerTicket> {
    tokio::time::timeout(deadline.duration(), endpoint.online())
        .await
        .map_err(|_| PeerError::EndpointOnlineTimeout)?;
    Ok(issue_ticket(endpoint.addr()))
}

#[derive(Debug, Clone)]
pub struct PeerIdentity {
    secret_key: SecretKey,
}

impl PeerIdentity {
    #[must_use]
    pub fn generate() -> Self {
        Self {
            secret_key: SecretKey::generate(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> PeerProbeResult<Self> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| PeerError::MalformedIdentity)?;

        Ok(Self {
            secret_key: SecretKey::from_bytes(&bytes),
        })
    }

    #[must_use]
    pub fn endpoint_id(&self) -> IrohEndpointId {
        IrohEndpointId::parse(self.secret_key.public().to_string())
            .expect("iroh endpoint ids are non-empty")
    }

    #[must_use]
    pub fn endpoint_addr(&self) -> EndpointAddr {
        EndpointAddr::new(self.secret_key.public())
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.secret_key.to_bytes()
    }
}

pub async fn bind_peer_endpoint(identity: &PeerIdentity) -> PeerProbeResult<PeerEndpoint> {
    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(identity.secret_key.clone())
        .bind()
        .await
        .map_err(|error| PeerError::EndpointBind {
            message: error.to_string(),
        })
}

pub fn load_or_create_identity(path: &Path) -> PeerProbeResult<PeerIdentity> {
    match fs::read(path) {
        Ok(bytes) => parse_existing_identity(path, &bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let identity = PeerIdentity::generate();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| PeerError::IdentityIo { source })?;
            }
            match write_identity_file(path, &identity.to_bytes()) {
                Ok(()) => Ok(identity),
                Err(PeerError::IdentityIo { source })
                    if source.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    let bytes =
                        fs::read(path).map_err(|source| PeerError::IdentityIo { source })?;
                    parse_existing_identity(path, &bytes)
                }
                Err(error) => Err(error),
            }
        }
        Err(source) => Err(PeerError::IdentityIo { source }),
    }
}

fn parse_existing_identity(path: &Path, bytes: &[u8]) -> PeerProbeResult<PeerIdentity> {
    restrict_identity_permissions(path)?;
    PeerIdentity::from_bytes(bytes)
}

fn write_identity_file(path: &Path, bytes: &[u8; 32]) -> PeerProbeResult<()> {
    #[cfg(unix)]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| PeerError::IdentityIo { source })?;

    #[cfg(not(unix))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| PeerError::IdentityIo { source })?;

    file.write_all(bytes)
        .map_err(|source| PeerError::IdentityIo { source })?;
    Ok(())
}

fn restrict_identity_permissions(path: &Path) -> PeerProbeResult<()> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|source| PeerError::IdentityIo { source })?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerProbeDeadline(Duration);

impl PeerProbeDeadline {
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self(duration)
    }

    #[must_use]
    pub fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerProbeReceipt {
    endpoint: IrohEndpointId,
    alpn: &'static [u8],
    observed_path: PeerTicketPath,
}

impl PeerProbeReceipt {
    #[must_use]
    pub fn new(endpoint: IrohEndpointId, observed_path: PeerTicketPath) -> Self {
        Self {
            endpoint,
            alpn: PLOYZ_PEER_ALPN,
            observed_path,
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &IrohEndpointId {
        &self.endpoint
    }

    #[must_use]
    pub fn observed_path(&self) -> PeerTicketPath {
        self.observed_path
    }

    #[must_use]
    pub fn alpn(&self) -> &'static [u8] {
        self.alpn
    }
}

pub trait PeerProbe {
    fn probe(
        &self,
        target: &IrohEndpointId,
        deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<PeerProbeReceipt>;
}

#[derive(Debug, Default)]
pub struct FakePeerProbe {
    reachable: BTreeSet<IrohEndpointId>,
    failures: BTreeMap<IrohEndpointId, String>,
}

impl FakePeerProbe {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn reachable(mut self, endpoint: IrohEndpointId) -> Self {
        self.reachable.insert(endpoint);
        self
    }

    #[must_use]
    pub fn failing(mut self, endpoint: IrohEndpointId, reason: impl Into<String>) -> Self {
        self.failures.insert(endpoint, reason.into());
        self
    }
}

impl PeerProbe for FakePeerProbe {
    fn probe(
        &self,
        target: &IrohEndpointId,
        _deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<PeerProbeReceipt> {
        if let Some(reason) = self.failures.get(target) {
            return Err(PeerError::ProbeFailed {
                endpoint: target.clone(),
                reason: reason.clone(),
            });
        }

        if self.reachable.contains(target) {
            return Ok(PeerProbeReceipt::new(
                target.clone(),
                PeerTicketPath::DiscoveryOnly,
            ));
        }

        Err(PeerError::ProbeFailed {
            endpoint: target.clone(),
            reason: "endpoint is not reachable".to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerPreflightRequest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PeerPreflightReceipt;

#[rpc_requests(message = PeerRpcMessage, no_spans)]
#[derive(Debug, Serialize, Deserialize)]
enum PeerRpcProtocol {
    #[rpc(tx=oneshot::Sender<PeerPreflightReceipt>)]
    Preflight(PeerPreflightRequest),
}

pub struct PeerRpcService {
    client: Client<PeerRpcProtocol>,
    _actor: Option<thread::JoinHandle<()>>,
}

pub struct PeerRpcListener {
    _service: PeerRpcService,
    router: iroh::protocol::Router,
}

impl PeerRpcListener {
    pub fn start(endpoint: PeerEndpoint) -> PeerProbeResult<Self> {
        let service = PeerRpcService::spawn()?;
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(Self::ALPN, service.protocol_handler()?)
            .spawn();

        Ok(Self {
            _service: service,
            router,
        })
    }

    pub const ALPN: &'static [u8] = PLOYZ_PEER_ALPN;

    pub async fn shutdown(self, deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        tokio::time::timeout(deadline.duration(), self.router.shutdown())
            .await
            .map_err(|_| PeerError::RpcTimeout)?
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })
    }
}

pub struct PeerRpcProbe {
    endpoint: IrohEndpointId,
    observed_path: PeerTicketPath,
    service: PeerRpcService,
    runtime: tokio::runtime::Runtime,
}

impl PeerRpcProbe {
    pub fn connect(endpoint: PeerEndpoint, ticket: &PeerTicket) -> PeerProbeResult<Self> {
        Self::from_service(
            ticket.endpoint_id(),
            ticket.path(),
            PeerRpcService::connect(endpoint, ticket.endpoint_addr().clone()),
        )
    }

    fn from_service(
        endpoint: IrohEndpointId,
        observed_path: PeerTicketPath,
        service: PeerRpcService,
    ) -> PeerProbeResult<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        Ok(Self {
            endpoint,
            observed_path,
            service,
            runtime,
        })
    }
}

impl PeerProbe for PeerRpcProbe {
    fn probe(
        &self,
        target: &IrohEndpointId,
        deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<PeerProbeReceipt> {
        if target != &self.endpoint {
            return Err(PeerError::ProbeFailed {
                endpoint: target.clone(),
                reason: "peer rpc probe was built for a different endpoint".to_string(),
            });
        }

        self.runtime.block_on(self.service.preflight(deadline))?;
        Ok(PeerProbeReceipt::new(target.clone(), self.observed_path))
    }
}

impl PeerRpcService {
    pub const ALPN: &'static [u8] = PLOYZ_PEER_ALPN;

    pub fn spawn() -> PeerProbeResult<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        let actor = thread::Builder::new()
            .name("polis-peer-rpc".to_string())
            .spawn(move || {
                runtime.block_on(run_peer_rpc_actor(rx));
            })
            .map_err(|error| PeerError::RpcRuntime {
                message: error.to_string(),
            })?;
        Ok(Self {
            client: Client::local(tx),
            _actor: Some(actor),
        })
    }

    #[must_use]
    pub fn connect(endpoint: iroh::Endpoint, addr: impl Into<iroh::EndpointAddr>) -> Self {
        let connection = IrohLazyRemoteConnection::new(endpoint, addr.into(), Self::ALPN.to_vec());
        Self {
            client: Client::boxed(connection),
            _actor: None,
        }
    }

    pub fn protocol_handler(&self) -> PeerProbeResult<impl iroh::protocol::ProtocolHandler> {
        let Some(local) = self.client.as_local() else {
            return Err(PeerError::RemoteServiceCannotListen);
        };
        Ok(IrohProtocol::new(PeerRpcProtocol::remote_handler(local)))
    }

    pub async fn preflight(&self, deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        tokio::time::timeout(deadline.duration(), self.client.rpc(PeerPreflightRequest))
            .await
            .map_err(|_| PeerError::RpcTimeout)?
            .map(|_| ())
            .map_err(|error| PeerError::RpcTransport {
                message: error.to_string(),
            })
    }
}

async fn run_peer_rpc_actor(mut rx: tokio::sync::mpsc::Receiver<PeerRpcMessage>) {
    while let Some(message) = rx.recv().await {
        match message {
            PeerRpcMessage::Preflight(message) => {
                let WithChannels { tx, .. } = message;
                let _ = tx.send(PeerPreflightReceipt).await;
            }
        }
    }
}

pub fn preflight_membership<P>(
    probe: &P,
    target: &IrohEndpointId,
    deadline: PeerProbeDeadline,
) -> PeerProbeResult<PeerProbeReceipt>
where
    P: PeerProbe,
{
    probe.probe(target, deadline)
}

fn redact_ascii(value: &str, prefix: usize, suffix: usize, fallback: &str) -> String {
    if value.len() <= prefix + suffix {
        return fallback.to_string();
    }

    format!("{}...{}", &value[..prefix], &value[value.len() - suffix..])
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::SocketAddr,
        sync::atomic::{AtomicU64, Ordering},
    };

    use iroh::TransportAddr;

    use super::*;

    fn deterministic_identity(byte: u8) -> PeerIdentity {
        PeerIdentity::from_bytes(&[byte; 32]).expect("identity")
    }

    #[test]
    fn tickets_round_trip_without_becoming_machine_truth() {
        let identity = deterministic_identity(7);
        let addr = identity
            .endpoint_addr()
            .with_ip_addr(SocketAddr::from(([127, 0, 0, 1], 1777)));
        let ticket = issue_ticket(addr);
        let encoded = ticket.encoded();
        let parsed = import_ticket(&encoded).expect("ticket");

        assert_eq!(parsed.endpoint_id(), identity.endpoint_id());
        assert_eq!(parsed.path(), PeerTicketPath::DirectOnly);
        assert_ne!(parsed.redacted(), encoded);
        assert!(!parsed.redacted().contains(identity.endpoint_id().as_str()));
    }

    #[test]
    fn malformed_ticket_is_typed() {
        assert!(matches!(
            import_ticket("not-a-ticket"),
            Err(PeerError::MalformedTicket)
        ));
    }

    #[test]
    fn fake_peer_probe_succeeds_and_fails_with_typed_errors() {
        let reachable = IrohEndpointId::parse("reachable-endpoint").expect("endpoint");
        let failed = IrohEndpointId::parse("failed-endpoint").expect("endpoint");
        let probe = FakePeerProbe::new()
            .reachable(reachable.clone())
            .failing(failed.clone(), "membership denied");
        let deadline = PeerProbeDeadline::new(Duration::from_millis(250));

        let receipt = probe.probe(&reachable, deadline).expect("reachable");
        assert_eq!(receipt.endpoint(), &reachable);
        assert_eq!(receipt.alpn(), PLOYZ_PEER_ALPN);

        assert!(matches!(
            probe.probe(&failed, deadline),
            Err(PeerError::ProbeFailed { endpoint, reason })
                if endpoint == failed && reason == "membership denied"
        ));
    }

    #[test]
    fn load_or_create_identity_reuses_existing_key() {
        let path = temp_identity_path();
        let first = load_or_create_identity(&path).expect("first identity");
        let second = load_or_create_identity(&path).expect("second identity");

        assert_eq!(first.endpoint_id(), second.endpoint_id());

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn bound_endpoint_uses_loaded_identity_for_bootstrap_ticket() {
        let path = temp_identity_path();
        let identity = load_or_create_identity(&path).expect("identity");
        let endpoint = bind_peer_endpoint(&identity).await.expect("endpoint");
        let ticket =
            issue_endpoint_ticket(&endpoint, PeerProbeDeadline::new(Duration::from_secs(5)))
                .await
                .expect("ticket");

        assert_eq!(endpoint.id().to_string(), identity.endpoint_id().as_str());
        assert_eq!(ticket.endpoint_id(), identity.endpoint_id());

        endpoint.close().await;
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn created_identity_file_is_owner_only() {
        let path = temp_identity_path();
        load_or_create_identity(&path).expect("identity");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);

        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn existing_identity_file_is_tightened_before_reuse() {
        let path = temp_identity_path();
        fs::write(&path, [13; 32]).expect("write identity");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("loosen");

        load_or_create_identity(&path).expect("identity");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn ticket_path_reports_relay_and_direct_addr() {
        let identity = deterministic_identity(9);
        let relay = "https://relay.example./".parse().expect("relay url");
        let addr = identity.endpoint_addr().with_addrs([
            TransportAddr::Relay(relay),
            TransportAddr::Ip(SocketAddr::from(([127, 0, 0, 1], 1778))),
        ]);
        let ticket = issue_ticket(addr);

        assert_eq!(ticket.path(), PeerTicketPath::DirectOrRelay);
    }

    #[test]
    fn preflight_membership_uses_probe_result() {
        let endpoint = IrohEndpointId::parse("reachable-endpoint").expect("endpoint");
        let probe = FakePeerProbe::new().reachable(endpoint.clone());

        let receipt = preflight_membership(
            &probe,
            &endpoint,
            PeerProbeDeadline::new(Duration::from_millis(100)),
        )
        .expect("preflight");

        assert_eq!(receipt.endpoint(), &endpoint);
    }

    #[test]
    fn peer_rpc_probe_binds_receipt_to_configured_endpoint() {
        let endpoint = IrohEndpointId::parse("node-a-endpoint").expect("endpoint");
        let probe = PeerRpcProbe::from_service(
            endpoint.clone(),
            PeerTicketPath::DiscoveryOnly,
            PeerRpcService::spawn().expect("service"),
        )
        .expect("probe");

        let receipt = probe
            .probe(&endpoint, PeerProbeDeadline::new(Duration::from_secs(1)))
            .expect("preflight");

        assert_eq!(receipt.endpoint(), &endpoint);
        assert_eq!(receipt.alpn(), PLOYZ_PEER_ALPN);
    }

    #[test]
    fn peer_rpc_probe_rejects_unconfigured_endpoint_without_rpc_call() {
        let endpoint = IrohEndpointId::parse("node-a-endpoint").expect("endpoint");
        let other = IrohEndpointId::parse("node-b-endpoint").expect("endpoint");
        let probe = PeerRpcProbe::from_service(
            endpoint,
            PeerTicketPath::DiscoveryOnly,
            PeerRpcService::spawn().expect("service"),
        )
        .expect("probe");

        let error = probe
            .probe(&other, PeerProbeDeadline::new(Duration::from_secs(1)))
            .expect_err("wrong endpoint");

        assert!(matches!(
            error,
            PeerError::ProbeFailed { endpoint, reason }
                if endpoint == other && reason == "peer rpc probe was built for a different endpoint"
        ));
    }

    #[tokio::test]
    async fn iroh_irpc_preflight_round_trips_between_endpoints() {
        let server_endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .expect("server endpoint");
        let listener = PeerRpcListener::start(server_endpoint.clone()).expect("listener");
        let ticket = issue_endpoint_ticket(
            &server_endpoint,
            PeerProbeDeadline::new(Duration::from_secs(5)),
        )
        .await
        .expect("ticket");

        let client_endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0)
            .await
            .expect("client endpoint");
        let client = PeerRpcService::connect(client_endpoint, ticket.endpoint_addr().clone());
        client
            .preflight(PeerProbeDeadline::new(Duration::from_secs(5)))
            .await
            .expect("preflight");

        listener
            .shutdown(PeerProbeDeadline::new(Duration::from_secs(5)))
            .await
            .expect("router shutdown");
    }

    fn temp_identity_path() -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "polis-peer-identity-{}-{id}.key",
            std::process::id()
        ))
    }
}
