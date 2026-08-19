//! In-process plaintext HTTP/2 Cloud Relay.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ployz_core::{MachineId, TunnelId, ValueError};
use prost::Message;
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_stream::{
    StreamExt,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::{Request, Response, Status, Streaming, metadata::MetadataMap, transport::Server};

/// In-flight tunnels drain for this long after GOAWAY, then remaining streams close.
const DRAIN: Duration = Duration::from_secs(30);
/// Register path RTT ping after hello, then this often.
const PING_INTERVAL: Duration = Duration::from_secs(5);

/// Bearer metadata key (`authorization`).
pub const AUTHORIZATION_METADATA: &str = "authorization";
/// Dial metadata key for the target Machine ID.
pub const MACHINE_ID_METADATA: &str = "machine-id";
/// Attach metadata key for the Tunnel ID from Open.
pub const TUNNEL_ID_METADATA: &str = "tunnel-id";
/// Dial, List, and Revoke metadata key for the Pairing Credential slot.
pub const PAIRING_METADATA: &str = "pairing";

const BEARER_PREFIX: &str = "Bearer ";
const TUNNEL_BUFFER: usize = 16;

mod transport {
    include!(concat!(env!("OUT_DIR"), "/ployz.relay.v1.CloudRelay.rs"));
}

use transport::cloud_relay_server::CloudRelay;
pub use transport::{cloud_relay_client::CloudRelayClient, cloud_relay_server::CloudRelayServer};

mod tunnel;
pub use tunnel::TunnelIo;

/// Machine identity on hello (`echo` 0) or a pong (`echo` n) on Register.
#[derive(Clone, PartialEq, Message)]
pub struct RegisterRequest {
    #[prost(string, tag = "1")]
    machine_id: String,
    #[prost(uint64, tag = "2")]
    echo: u64,
}

impl RegisterRequest {
    /// Build a Register hello for this Machine ID.
    #[must_use]
    pub fn new(machine_id: &MachineId) -> Self {
        Self {
            machine_id: machine_id.to_string(),
            echo: 0,
        }
    }

    /// Build a pong for this Register ping nonce.
    #[must_use]
    pub fn pong(nonce: u64) -> Self {
        Self {
            machine_id: String::new(),
            echo: nonce,
        }
    }

    /// Parse the Machine ID from a hello.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the wire string is not a Machine ID.
    pub fn machine_id(&self) -> Result<MachineId, ValueError> {
        MachineId::parse(&self.machine_id)
    }

    /// Echo nonce. `0` is hello; non-zero is a pong.
    #[must_use]
    pub fn echo(&self) -> u64 {
        self.echo
    }
}

/// `Open(id)` (`ping` 0) or a Register ping (`ping` n).
#[derive(Clone, PartialEq, Message)]
pub struct Open {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(uint64, tag = "2")]
    ping: u64,
}

impl Open {
    /// Build an Open carrying this Tunnel ID.
    #[must_use]
    pub fn new(tunnel_id: &TunnelId) -> Self {
        Self {
            id: tunnel_id.to_string(),
            ping: 0,
        }
    }

    /// Build a Register ping for this nonce. `nonce` must be non-zero.
    #[must_use]
    pub fn ping(nonce: u64) -> Self {
        Self {
            id: String::new(),
            ping: nonce,
        }
    }

    /// Ping nonce when this message is a ping, not an Open.
    #[must_use]
    pub fn ping_nonce(&self) -> Option<u64> {
        (self.ping != 0).then_some(self.ping)
    }

    /// Parse the Tunnel ID from an Open.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the wire string is not a Tunnel ID.
    pub fn tunnel_id(&self) -> Result<TunnelId, ValueError> {
        TunnelId::parse(&self.id)
    }
}

/// One opaque chunk on Dial or Attach. The Relay does not parse Machine RPC.
#[derive(Clone, PartialEq, Message)]
pub struct TunnelFrame {
    #[prost(bytes = "vec", tag = "1")]
    pub data: Vec<u8>,
}

impl TunnelFrame {
    /// Wrap opaque tunnel bytes. The Relay does not parse them.
    #[must_use]
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

/// Dial-authenticated request to revoke a Pairing Credential.
#[derive(Clone, PartialEq, Message)]
pub struct RevokeRequest {}

/// Pairing Credential is no longer accepted on Register.
#[derive(Clone, PartialEq, Message)]
pub struct RevokeResponse {}

/// Dial-authenticated request to list held Registers for a pairing.
#[derive(Clone, PartialEq, Message)]
pub struct ListRequest {}

/// One held Register as listed for a pairing.
#[derive(Clone, PartialEq, Message)]
pub struct HeldRegister {
    #[prost(string, tag = "1")]
    machine_id: String,
    #[prost(int64, optional, tag = "2")]
    pub register_rtt_ns: Option<i64>,
}

impl HeldRegister {
    /// Machine ID as listed on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.machine_id
    }

    /// Parse the Machine ID from this row.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the wire string is not a Machine ID.
    pub fn machine_id(&self) -> Result<MachineId, ValueError> {
        MachineId::parse(&self.machine_id)
    }
}

/// Live Observation of currently held Registers for one pairing.
#[derive(Clone, PartialEq, Message)]
pub struct ListResponse {
    #[prost(message, repeated, tag = "1")]
    registers: Vec<HeldRegister>,
}

impl ListResponse {
    /// Held Registers for the requested pairing. Empty is success.
    #[must_use]
    pub fn registers(&self) -> &[HeldRegister] {
        &self.registers
    }
}

/// Bearer that authenticates Register and is the Dial/List/Revoke slot key.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PairingCredential(String);

/// Process-wide bearer Cloud presents on Dial, List, and Revoke.
#[derive(Clone, Eq, PartialEq)]
pub struct DialCredential(String);

/// Failures constructing a credential.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RelayError {
    #[error("credential must be a non-empty bearer")]
    EmptyCredential,
}

impl PairingCredential {
    /// Parse a non-empty Pairing Credential.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::EmptyCredential`] when `value` is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_credential(value).map(Self)
    }

    /// Pairing Credential bearer.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl DialCredential {
    /// Parse a non-empty Dial Credential.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::EmptyCredential`] when `value` is empty.
    pub fn parse(value: impl Into<String>) -> Result<Self, RelayError> {
        parse_credential(value).map(Self)
    }

    /// Dial Credential bearer.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PairingCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingCredential(..)")
    }
}

impl fmt::Debug for DialCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DialCredential(..)")
    }
}

fn parse_credential(value: impl Into<String>) -> Result<String, RelayError> {
    let value = value.into();
    if value.is_empty() {
        Err(RelayError::EmptyCredential)
    } else {
        Ok(value)
    }
}

/// Starts HTTP/2 GOAWAY drain on a running [`Relay::serve`] task.
pub struct Goaway {
    phase: watch::Sender<Phase>,
}

impl Goaway {
    /// Refuse new Attaches and drain in-flight tunnels, then close.
    pub fn start(self) {
        self.phase.send_replace(Phase::Draining);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Live,
    Draining,
}

/// Plaintext Cloud Relay: Register, Dial, Attach, List, Revoke, opaque splice.
///
/// Slots are keyed by Pairing Credential and Machine ID. One process Dial
/// Credential authenticates Cloud. Pairings are not loaded at boot.
#[derive(Clone)]
pub struct Relay {
    dial: DialCredential,
    state: Arc<Mutex<State>>,
    phase: watch::Sender<Phase>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct Slot {
    pairing: PairingCredential,
    machine_id: MachineId,
}

struct State {
    machines: HashMap<Slot, Registration>,
    pending: HashMap<TunnelId, Pending>,
    next_generation: u64,
    revoked: HashSet<PairingCredential>,
}

struct Registration {
    // Distinguishes this Register from a later replacement so the old inbound
    // task cannot delete the winner.
    generation: u64,
    open_tx: mpsc::Sender<Result<Open, Status>>,
    // Dropped when this Register is displaced; Dial/Attach forwarders abort.
    cancel: watch::Sender<()>,
    register_rtt: Option<Duration>,
}

struct Pending {
    slot: Slot,
    to_machine: mpsc::Receiver<Result<TunnelFrame, Status>>,
    from_machine: mpsc::Sender<Result<TunnelFrame, Status>>,
    cancel: watch::Receiver<()>,
}

impl Relay {
    /// Construct a Relay with one process-wide Dial Credential.
    #[must_use]
    pub fn new(dial: DialCredential) -> Self {
        Self {
            dial,
            state: Arc::new(Mutex::new(State {
                machines: HashMap::new(),
                pending: HashMap::new(),
                next_generation: 0,
                revoked: HashSet::new(),
            })),
            phase: watch::channel(Phase::Live).0,
        }
    }

    /// Serve plaintext HTTP/2 on `listen`.
    ///
    /// [`Goaway::start`] sends HTTP/2 GOAWAY: new Attaches are refused, in-flight
    /// tunnels drain for 30 seconds, then remaining streams close.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from binding the listener.
    pub async fn serve(
        &self,
        listen: SocketAddr,
    ) -> std::io::Result<(
        SocketAddr,
        JoinHandle<Result<(), tonic::transport::Error>>,
        Goaway,
    )> {
        self.serve_with_drain(listen, DRAIN).await
    }

    /// Bind with a drain timeout. Production uses [`serve`] (30s).
    ///
    /// # Errors
    ///
    /// Returns I/O errors from binding the listener.
    #[doc(hidden)]
    pub async fn serve_with_drain(
        &self,
        listen: SocketAddr,
        drain: Duration,
    ) -> std::io::Result<(
        SocketAddr,
        JoinHandle<Result<(), tonic::transport::Error>>,
        Goaway,
    )> {
        let listener = TcpListener::bind(listen).await?;
        let address = listener.local_addr()?;
        let server = CloudRelayServer::new(self.clone());
        let closer = self.clone();
        let mut shutdown_phase = self.phase.subscribe();
        let mut expire_phase = closer.phase.subscribe();
        let handle = tokio::spawn(async move {
            let shutdown = async move {
                let _ = shutdown_phase.wait_for(|phase| *phase != Phase::Live).await;
            };
            let serve = Server::builder()
                .add_service(server)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown);
            let expire = async move {
                if expire_phase
                    .wait_for(|phase| *phase != Phase::Live)
                    .await
                    .is_ok()
                {
                    tokio::time::sleep(drain).await;
                    // tonic waits forever on the held Register stream.
                    closer.force_close();
                }
            };
            tokio::select! {
                result = serve => result,
                _ = expire => Ok(()),
            }
        });
        Ok((
            address,
            handle,
            Goaway {
                phase: self.phase.clone(),
            },
        ))
    }

    fn force_close(&self) {
        let mut state = self.lock();
        state.machines.clear();
        state.pending.clear();
    }

    fn accepting(&self) -> bool {
        match *self.phase.borrow() {
            Phase::Live => true,
            Phase::Draining => false,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("relay state mutex poisoned")
    }

    fn register_pairing(&self, metadata: &MetadataMap) -> Option<PairingCredential> {
        let token = bearer(metadata)?;
        if token == self.dial.as_str() {
            return None;
        }
        let pairing = PairingCredential::parse(token).ok()?;
        if self.lock().revoked.contains(&pairing) {
            return None;
        }
        Some(pairing)
    }

    fn is_dial(&self, metadata: &MetadataMap) -> bool {
        bearer(metadata) == Some(self.dial.as_str())
    }

    #[expect(
        clippy::result_large_err,
        reason = "tonic Status is the Dial/List/Revoke error"
    )]
    fn cloud_pairing(&self, metadata: &MetadataMap) -> Result<PairingCredential, Status> {
        if !self.is_dial(metadata) {
            return Err(Status::unauthenticated("invalid Dial Credential"));
        }
        pairing_metadata(metadata)
            .ok_or_else(|| Status::invalid_argument("missing or invalid pairing"))
    }

    #[expect(
        clippy::result_large_err,
        reason = "tonic Status is the Dial/List error"
    )]
    fn cloud_pairing_live(&self, metadata: &MetadataMap) -> Result<PairingCredential, Status> {
        let pairing = self.cloud_pairing(metadata)?;
        if !self.accepting() {
            return Err(Status::unavailable("GOAWAY"));
        }
        Ok(pairing)
    }
}

fn pairing_metadata(metadata: &MetadataMap) -> Option<PairingCredential> {
    metadata_str(metadata, PAIRING_METADATA).and_then(|value| PairingCredential::parse(value).ok())
}

fn bearer(metadata: &MetadataMap) -> Option<&str> {
    metadata
        .get(AUTHORIZATION_METADATA)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_PREFIX)
}

fn metadata_str<'a>(metadata: &'a MetadataMap, key: &'static str) -> Option<&'a str> {
    metadata.get(key).and_then(|value| value.to_str().ok())
}

fn next_nonce(nonce: u64) -> u64 {
    match nonce.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

fn rtt_ns(rtt: Duration) -> Option<i64> {
    i64::try_from(rtt.as_nanos()).ok()
}

async fn forward_frames(
    mut inbound: Streaming<TunnelFrame>,
    tx: mpsc::Sender<Result<TunnelFrame, Status>>,
    mut cancel: watch::Receiver<()>,
) {
    loop {
        tokio::select! {
            frame = inbound.next() => {
                let Some(Ok(frame)) = frame else { break };
                if tx.send(Ok(frame)).await.is_err() {
                    break;
                }
            }
            _ = cancel.changed() => break,
        }
    }
}

#[tonic::async_trait]
impl CloudRelay for Relay {
    type RegisterStream = ReceiverStream<Result<Open, Status>>;
    type DialStream = ReceiverStream<Result<TunnelFrame, Status>>;
    type AttachStream = ReceiverStream<Result<TunnelFrame, Status>>;

    async fn register(
        &self,
        request: Request<Streaming<RegisterRequest>>,
    ) -> Result<Response<Self::RegisterStream>, Status> {
        let pairing = self
            .register_pairing(request.metadata())
            .ok_or_else(|| Status::unauthenticated("invalid Pairing Credential"))?;
        if !self.accepting() {
            return Err(Status::unavailable("GOAWAY"));
        }
        let mut inbound = request.into_inner();
        let first = inbound
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("Register requires a Machine ID"))??;
        if first.echo() != 0 {
            return Err(Status::invalid_argument("Register requires a Machine ID"));
        }
        let machine_id = first
            .machine_id()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let slot = Slot {
            pairing,
            machine_id,
        };
        let (open_tx, open_rx) = mpsc::channel(TUNNEL_BUFFER);
        let (cancel, _) = watch::channel(());
        let mut ping_cancel = cancel.subscribe();
        let generation = {
            let mut state = self.lock();
            let generation = state.next_generation;
            state.next_generation += 1;
            state.pending.retain(|_, pending| pending.slot != slot);
            state.machines.insert(
                slot.clone(),
                Registration {
                    generation,
                    open_tx: open_tx.clone(),
                    cancel,
                    register_rtt: None,
                },
            );
            generation
        };
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(PING_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut nonce = 0;
            let mut ping_sent = None;
            loop {
                tokio::select! {
                    message = inbound.next() => {
                        let Some(Ok(message)) = message else { break };
                        if message.echo() == 0 {
                            continue;
                        }
                        let Some((expected, sent)) = ping_sent else {
                            continue;
                        };
                        if expected != message.echo() {
                            continue;
                        }
                        let rtt = Instant::now().saturating_duration_since(sent);
                        let Ok(mut state) = state.lock() else { break };
                        let Some(registration) = state.machines.get_mut(&slot) else {
                            break;
                        };
                        if registration.generation != generation {
                            break;
                        }
                        registration.register_rtt = Some(rtt);
                    }
                    _ = interval.tick() => {
                        nonce = next_nonce(nonce);
                        ping_sent = Some((nonce, Instant::now()));
                        if open_tx.send(Ok(Open::ping(nonce))).await.is_err() {
                            break;
                        }
                    }
                    _ = ping_cancel.changed() => break,
                }
            }
            if let Ok(mut state) = state.lock()
                && state
                    .machines
                    .get(&slot)
                    .is_some_and(|current| current.generation == generation)
            {
                state.pending.retain(|_, pending| pending.slot != slot);
                state.machines.remove(&slot);
            }
        });
        Ok(Response::new(ReceiverStream::new(open_rx)))
    }

    async fn dial(
        &self,
        request: Request<Streaming<TunnelFrame>>,
    ) -> Result<Response<Self::DialStream>, Status> {
        let pairing = self.cloud_pairing_live(request.metadata())?;
        let machine_id = MachineId::parse(
            metadata_str(request.metadata(), MACHINE_ID_METADATA)
                .ok_or_else(|| Status::invalid_argument("missing or invalid machine-id"))?,
        )
        .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let slot = Slot {
            pairing,
            machine_id,
        };
        let tunnel_id = TunnelId::random();
        let (to_machine_tx, to_machine_rx) = mpsc::channel(TUNNEL_BUFFER);
        let (from_machine_tx, from_machine_rx) = mpsc::channel(TUNNEL_BUFFER);
        let (open_tx, cancel) = {
            let mut state = self.lock();
            let registration = state
                .machines
                .get(&slot)
                .ok_or_else(|| Status::not_found("unknown Machine ID"))?;
            let open_tx = registration.open_tx.clone();
            let cancel = registration.cancel.subscribe();
            state.pending.insert(
                tunnel_id,
                Pending {
                    slot,
                    to_machine: to_machine_rx,
                    from_machine: from_machine_tx,
                    cancel: cancel.clone(),
                },
            );
            (open_tx, cancel)
        };
        if open_tx.send(Ok(Open::new(&tunnel_id))).await.is_err() {
            self.lock().pending.remove(&tunnel_id);
            return Err(Status::unavailable("Register closed"));
        }
        tokio::spawn(forward_frames(request.into_inner(), to_machine_tx, cancel));
        Ok(Response::new(ReceiverStream::new(from_machine_rx)))
    }

    async fn attach(
        &self,
        request: Request<Streaming<TunnelFrame>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        if !self.accepting() {
            return Err(Status::unavailable("GOAWAY"));
        }
        let tunnel_id = TunnelId::parse(
            metadata_str(request.metadata(), TUNNEL_ID_METADATA)
                .ok_or_else(|| Status::invalid_argument("missing or invalid tunnel-id"))?,
        )
        .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let pending = self
            .lock()
            .pending
            .remove(&tunnel_id)
            .ok_or_else(|| Status::not_found("unknown Tunnel ID"))?;
        tokio::spawn(forward_frames(
            request.into_inner(),
            pending.from_machine,
            pending.cancel,
        ));
        Ok(Response::new(ReceiverStream::new(pending.to_machine)))
    }

    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let pairing = self.cloud_pairing_live(request.metadata())?;
        let registers = self
            .lock()
            .machines
            .iter()
            .filter(|(slot, _)| slot.pairing == pairing)
            .map(|(slot, registration)| HeldRegister {
                machine_id: slot.machine_id.to_string(),
                register_rtt_ns: registration.register_rtt.and_then(rtt_ns),
            })
            .collect();
        Ok(Response::new(ListResponse { registers }))
    }

    async fn revoke(
        &self,
        request: Request<RevokeRequest>,
    ) -> Result<Response<RevokeResponse>, Status> {
        let pairing = self.cloud_pairing(request.metadata())?;
        self.lock().revoked.insert(pairing);
        Ok(Response::new(RevokeResponse {}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_credentials_are_rejected() {
        let Err(pairing) = PairingCredential::parse("") else {
            panic!("expected empty Pairing Credential to fail");
        };
        let Err(dial) = DialCredential::parse("") else {
            panic!("expected empty Dial Credential to fail");
        };
        assert_eq!(pairing, RelayError::EmptyCredential);
        assert_eq!(dial, RelayError::EmptyCredential);
    }

    #[test]
    fn register_request_rejects_a_non_machine_id() {
        assert!(RegisterRequest::default().machine_id().is_err());
    }

    #[test]
    fn held_register_rejects_a_non_machine_id() {
        assert!(HeldRegister::default().machine_id().is_err());
    }

    #[test]
    fn open_rejects_a_non_tunnel_id() {
        assert!(Open::default().tunnel_id().is_err());
    }

    #[test]
    fn ping_zero_is_an_open_and_nonzero_is_a_ping() {
        let tunnel_id = TunnelId::random();
        let open = Open::new(&tunnel_id);
        assert_eq!(open.ping_nonce(), None);
        assert_eq!(open.tunnel_id().unwrap(), tunnel_id);
        assert_eq!(Open::ping(7).ping_nonce(), Some(7));
    }
}
