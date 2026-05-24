//! Product-neutral peer bootstrap, identity, and probe primitives.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    str::FromStr,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use iroh::{EndpointAddr, SecretKey};
use iroh_tickets::endpoint::EndpointTicket;
use thiserror::Error;

use crate::IrohEndpointId;

pub const PLOYZ_PEER_ALPN: &[u8] = b"ployz/peer-rpc/0";

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer ticket is malformed")]
    MalformedTicket,
    #[error("peer identity is malformed")]
    MalformedIdentity,
    #[error("peer identity storage failed")]
    IdentityIo { source: std::io::Error },
    #[error("peer probe failed for {endpoint}: {reason}")]
    ProbeFailed {
        endpoint: IrohEndpointId,
        reason: String,
    },
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

pub fn load_or_create_identity(path: &Path) -> PeerProbeResult<PeerIdentity> {
    match fs::read(path) {
        Ok(bytes) => {
            restrict_identity_permissions(path)?;
            PeerIdentity::from_bytes(&bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let identity = PeerIdentity::generate();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| PeerError::IdentityIo { source })?;
            }
            write_identity_file(path, &identity.to_bytes())?;
            Ok(identity)
        }
        Err(source) => Err(PeerError::IdentityIo { source }),
    }
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
    use std::{fs, net::SocketAddr, time::SystemTime};

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

    fn temp_identity_path() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("polis-peer-identity-{nanos}.key"))
    }
}
