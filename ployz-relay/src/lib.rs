//! In-process HTTP/1.1 Cloud Relay. TLS belongs to the terminator.

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
use tokio::{sync::mpsc, task::JoinHandle, time::MissedTickBehavior};

mod client;
mod serve;

pub use client::{ClientError, RelayClient, RelayWs, TunnelIo};

/// In-flight tunnels drain for this long after GOAWAY, then remaining holds close.
const DRAIN: Duration = Duration::from_secs(30);
/// An Open must rendezvous with Attach within this bound, even if Dial stays open.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);
/// Register path RTT ping after hello, then this often.
const PING_INTERVAL: Duration = Duration::from_secs(5);

/// Dial header for the target Machine ID.
pub const MACHINE_ID_HEADER: &str = "machine-id";
/// Attach header for the Tunnel ID from Open.
pub const TUNNEL_ID_HEADER: &str = "tunnel-id";
/// Dial, List, and Revoke header for the Pairing Credential slot.
pub const PAIRING_HEADER: &str = "pairing";

pub(crate) const REGISTER_PATH: &str = "/register";
pub(crate) const DIAL_PATH: &str = "/dial";
pub(crate) const ATTACH_PATH: &str = "/attach";
pub(crate) const LIST_PATH: &str = "/list";
pub(crate) const REVOKE_PATH: &str = "/revoke";

const TUNNEL_BUFFER: usize = 16;

/// Fail-closed Relay response (HTTP status before WebSocket upgrade).
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct Status {
    status: http::StatusCode,
    message: String,
}

impl Status {
    /// HTTP status sent on the wire.
    #[must_use]
    pub fn status(&self) -> http::StatusCode {
        self.status
    }

    /// Reason phrase sent on the wire.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn unauthenticated(message: impl Into<String>) -> Self {
        Self {
            status: http::StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            status: http::StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: http::StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    pub(crate) fn from_http(status: http::StatusCode, body: &[u8]) -> Self {
        let message = String::from_utf8_lossy(body);
        let message = if message.is_empty() {
            status
                .canonical_reason()
                .unwrap_or("relay error")
                .to_owned()
        } else {
            message.into_owned()
        };
        Self { status, message }
    }
}

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

/// Starts GOAWAY drain on a running [`Relay::serve`] task.
pub struct Goaway {
    phase: tokio::sync::watch::Sender<Phase>,
}

impl Goaway {
    /// Refuse new Register/Dial/Attach/List and drain in-flight tunnels, then close.
    pub fn start(self) {
        self.phase.send_replace(Phase::Draining);
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Live,
    Draining,
}

/// HTTP/1.1 Cloud Relay: Register, Dial, Attach, List, Revoke, opaque splice.
///
/// Slots are keyed by Pairing Credential and Machine ID. One process Dial
/// Credential authenticates Cloud. Pairings are not loaded at boot.
#[derive(Clone)]
pub struct Relay {
    dial: DialCredential,
    state: Arc<Mutex<State>>,
    phase: tokio::sync::watch::Sender<Phase>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct Slot {
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
    open_tx: mpsc::Sender<Open>,
    // Dropped when this Register is displaced; Dial/Attach forwarders abort.
    cancel: tokio::sync::watch::Sender<()>,
    register_rtt: Option<Duration>,
}

struct Pending {
    slot: Slot,
    expires_at: tokio::time::Instant,
    // Removing pending state also stops its expiry task.
    _expiry_cancel: tokio::sync::oneshot::Sender<()>,
    to_machine: mpsc::Receiver<TunnelFrame>,
    from_machine: mpsc::Sender<TunnelFrame>,
    cancel: tokio::sync::watch::Receiver<()>,
}

pub(crate) struct StartedRegister {
    pub inbound: mpsc::Sender<RegisterRequest>,
    pub opens: mpsc::Receiver<Open>,
}

pub(crate) struct StartedTunnel {
    pub inbound: mpsc::Sender<TunnelFrame>,
    pub outbound: mpsc::Receiver<TunnelFrame>,
    pub cancel: tokio::sync::watch::Receiver<()>,
    pending: Option<PendingDial>,
}

// Dial owns rendezvous cleanup before, during, and after WebSocket upgrade.
// Attach consumes the entry; this guard then has nothing left to remove.
struct PendingDial {
    state: Arc<Mutex<State>>,
    tunnel_id: TunnelId,
}

impl Drop for PendingDial {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.pending.remove(&self.tunnel_id);
        }
    }
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
            phase: tokio::sync::watch::channel(Phase::Live).0,
        }
    }

    /// Serve HTTP/1.1 on `listen` (WebSocket Register/Dial/Attach, POST List/Revoke).
    ///
    /// [`Goaway::start`] refuses new methods, drains in-flight tunnels for 30
    /// seconds, then force-closes remaining holds.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from binding the listener.
    pub async fn serve(
        &self,
        listen: SocketAddr,
    ) -> std::io::Result<(SocketAddr, JoinHandle<std::io::Result<()>>, Goaway)> {
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
    ) -> std::io::Result<(SocketAddr, JoinHandle<std::io::Result<()>>, Goaway)> {
        serve::bind(self.clone(), listen, drain).await
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

    pub(crate) fn register_pairing(
        &self,
        bearer: Option<&str>,
    ) -> Result<PairingCredential, Status> {
        let token = bearer.ok_or_else(|| Status::unauthenticated("invalid Pairing Credential"))?;
        if token == self.dial.as_str() {
            return Err(Status::unauthenticated("invalid Pairing Credential"));
        }
        let pairing = PairingCredential::parse(token)
            .map_err(|_| Status::unauthenticated("invalid Pairing Credential"))?;
        if self.lock().revoked.contains(&pairing) {
            return Err(Status::unauthenticated("invalid Pairing Credential"));
        }
        Ok(pairing)
    }

    fn is_dial(&self, bearer: Option<&str>) -> bool {
        bearer == Some(self.dial.as_str())
    }

    pub(crate) fn cloud_pairing(
        &self,
        bearer: Option<&str>,
        pairing: Option<&str>,
    ) -> Result<PairingCredential, Status> {
        if !self.is_dial(bearer) {
            return Err(Status::unauthenticated("invalid Dial Credential"));
        }
        pairing
            .and_then(|value| PairingCredential::parse(value).ok())
            .ok_or_else(|| Status::invalid_argument("missing or invalid pairing"))
    }

    pub(crate) fn cloud_pairing_live(
        &self,
        bearer: Option<&str>,
        pairing: Option<&str>,
    ) -> Result<PairingCredential, Status> {
        let pairing = self.cloud_pairing(bearer, pairing)?;
        if !self.accepting() {
            return Err(Status::unavailable("GOAWAY"));
        }
        Ok(pairing)
    }

    pub(crate) fn require_accepting(&self) -> Result<(), Status> {
        if self.accepting() {
            Ok(())
        } else {
            Err(Status::unavailable("GOAWAY"))
        }
    }

    pub(crate) fn start_register(
        &self,
        pairing: PairingCredential,
        machine_id: MachineId,
    ) -> Result<StartedRegister, Status> {
        self.require_accepting()?;
        let slot = Slot {
            pairing,
            machine_id,
        };
        let (open_tx, open_rx) = mpsc::channel::<Open>(TUNNEL_BUFFER);
        let (inbound_tx, inbound_rx) = mpsc::channel::<RegisterRequest>(TUNNEL_BUFFER);
        let (cancel, _) = tokio::sync::watch::channel(());
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
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut inbound = inbound_rx;
            let mut nonce = 0;
            let mut ping_sent = None;
            loop {
                tokio::select! {
                    message = inbound.recv() => {
                        let Some(message) = message else { break };
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
                        if open_tx.send(Open::ping(nonce)).await.is_err() {
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
        Ok(StartedRegister {
            inbound: inbound_tx,
            opens: open_rx,
        })
    }

    pub(crate) async fn start_dial(
        &self,
        pairing: PairingCredential,
        machine_id: MachineId,
    ) -> Result<StartedTunnel, Status> {
        let slot = Slot {
            pairing,
            machine_id,
        };
        let tunnel_id = TunnelId::random();
        let expires_at = tokio::time::Instant::now() + ATTACH_TIMEOUT;
        let (expiry_cancel, cancelled) = tokio::sync::oneshot::channel();
        let (to_machine_tx, to_machine_rx) = mpsc::channel::<TunnelFrame>(TUNNEL_BUFFER);
        let (from_machine_tx, from_machine_rx) = mpsc::channel::<TunnelFrame>(TUNNEL_BUFFER);
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
                    expires_at,
                    _expiry_cancel: expiry_cancel,
                    to_machine: to_machine_rx,
                    from_machine: from_machine_tx,
                    cancel: cancel.clone(),
                },
            );
            (open_tx, cancel)
        };
        let pending = PendingDial {
            state: Arc::clone(&self.state),
            tunnel_id,
        };
        // One bounded task per pending rendezvous; Attach, Dial cancellation,
        // Register turnover, and shutdown all cancel it by dropping the sender.
        let state = Arc::downgrade(&self.state);
        tokio::spawn(async move {
            tokio::select! {
                _ = cancelled => {}
                _ = tokio::time::sleep_until(expires_at) => {
                    if let Some(state) = state.upgrade()
                        && let Ok(mut state) = state.lock()
                    {
                        state.pending.remove(&tunnel_id);
                    }
                }
            }
        });
        tokio::time::timeout_at(expires_at, open_tx.send(Open::new(&tunnel_id)))
            .await
            .map_err(|_| Status::unavailable("Attach deadline elapsed"))?
            .map_err(|_| Status::unavailable("Register closed"))?;
        Ok(StartedTunnel {
            inbound: to_machine_tx,
            outbound: from_machine_rx,
            cancel,
            pending: Some(pending),
        })
    }

    pub(crate) fn start_attach(&self, tunnel_id: TunnelId) -> Result<StartedTunnel, Status> {
        self.require_accepting()?;
        let pending = self
            .lock()
            .pending
            .remove(&tunnel_id)
            .filter(|pending| pending.expires_at > tokio::time::Instant::now())
            .ok_or_else(|| Status::not_found("unknown Tunnel ID"))?;
        Ok(StartedTunnel {
            inbound: pending.from_machine,
            outbound: pending.to_machine,
            cancel: pending.cancel,
            pending: None,
        })
    }

    pub(crate) fn list(&self, pairing: &PairingCredential) -> ListResponse {
        let registers = self
            .lock()
            .machines
            .iter()
            .filter(|(slot, _)| slot.pairing == *pairing)
            .map(|(slot, registration)| HeldRegister {
                machine_id: slot.machine_id.to_string(),
                register_rtt_ns: registration.register_rtt.and_then(rtt_ns),
            })
            .collect();
        ListResponse { registers }
    }

    pub(crate) fn revoke(&self, pairing: PairingCredential) {
        self.lock().revoked.insert(pairing);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn next_tunnel(register: &mut StartedRegister) -> TunnelId {
        loop {
            let open = register.opens.recv().await.unwrap();
            if open.ping_nonce().is_none() {
                return open.tunnel_id().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn abandoned_dial_cannot_be_attached() {
        let relay = Relay::new(DialCredential::parse("cloud").unwrap());
        let pairing = PairingCredential::parse("pairing").unwrap();
        let machine = MachineId::random();
        let mut register = relay.start_register(pairing.clone(), machine).unwrap();
        let dial = relay.start_dial(pairing.clone(), machine).await.unwrap();
        let tunnel = next_tunnel(&mut register).await;
        drop(dial); // Includes dropping the handle before the upgrade callback runs.
        assert!(relay.start_attach(tunnel).is_err());
        assert_eq!(relay.list(&pairing).registers().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn missing_attach_expires_while_register_and_dial_survive() {
        let relay = Relay::new(DialCredential::parse("cloud").unwrap());
        let pairing = PairingCredential::parse("pairing").unwrap();
        let machine = MachineId::random();
        let mut register = relay.start_register(pairing.clone(), machine).unwrap();
        let mut dial = relay.start_dial(pairing.clone(), machine).await.unwrap();
        let tunnel = next_tunnel(&mut register).await;
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert!(dial.inbound.is_closed());
        assert!(dial.outbound.recv().await.is_none());
        assert!(relay.start_attach(tunnel).is_err());
        assert_eq!(relay.list(&pairing).registers().len(), 1);

        // Expiry does not revoke the surviving Register's ability to serve.
        let fresh = relay.start_dial(pairing, machine).await.unwrap();
        let mut attached = relay
            .start_attach(next_tunnel(&mut register).await)
            .unwrap();
        fresh
            .inbound
            .send(TunnelFrame::new(vec![42]))
            .await
            .unwrap();
        assert_eq!(attached.outbound.recv().await.unwrap().data, vec![42]);
    }

    #[tokio::test(start_paused = true)]
    async fn attached_tunnels_outlive_deadline_and_old_dial_cleanup() {
        let relay = Relay::new(DialCredential::parse("cloud").unwrap());
        let pairing = PairingCredential::parse("pairing").unwrap();
        let machine = MachineId::random();
        let mut register = relay.start_register(pairing.clone(), machine).unwrap();
        let old = relay.start_dial(pairing.clone(), machine).await.unwrap();
        let mut old_attach = relay
            .start_attach(next_tunnel(&mut register).await)
            .unwrap();
        let mut dial = relay.start_dial(pairing, machine).await.unwrap();
        let tunnel = next_tunnel(&mut register).await;
        drop(old);
        assert!(old_attach.outbound.recv().await.is_none());
        let mut attach = relay.start_attach(tunnel).unwrap();
        assert!(relay.start_attach(tunnel).is_err());
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        dial.inbound.send(TunnelFrame::new(vec![1])).await.unwrap();
        assert_eq!(attach.outbound.recv().await.unwrap().data, vec![1]);
        attach
            .inbound
            .send(TunnelFrame::new(vec![2]))
            .await
            .unwrap();
        assert_eq!(dial.outbound.recv().await.unwrap().data, vec![2]);
    }

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
