//! In-process plaintext HTTP/2 Cloud Relay.

use std::{
    collections::HashMap,
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use ployz_core::{MachineId, TunnelId, ValueError};
use prost::Message;
use thiserror::Error;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_stream::{
    StreamExt,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::{Request, Response, Status, Streaming, metadata::MetadataMap, transport::Server};

/// Bearer metadata key (`authorization`).
pub const AUTHORIZATION_METADATA: &str = "authorization";
/// Dial metadata key for the target Machine ID.
pub const MACHINE_ID_METADATA: &str = "machine-id";
/// Attach metadata key for the Tunnel ID from Open.
pub const TUNNEL_ID_METADATA: &str = "tunnel-id";

const BEARER_PREFIX: &str = "Bearer ";
const TUNNEL_BUFFER: usize = 16;

mod transport {
    include!(concat!(env!("OUT_DIR"), "/ployz.relay.v1.CloudRelay.rs"));
}

use transport::cloud_relay_server::CloudRelay;
pub use transport::{cloud_relay_client::CloudRelayClient, cloud_relay_server::CloudRelayServer};

/// Machine identity sent on the held Register stream.
#[derive(Clone, PartialEq, Message)]
pub struct RegisterRequest {
    #[prost(string, tag = "1")]
    machine_id: String,
}

impl RegisterRequest {
    /// Build a Register message for this Machine ID.
    #[must_use]
    pub fn new(machine_id: &MachineId) -> Self {
        Self {
            machine_id: machine_id.to_string(),
        }
    }

    /// Parse the Machine ID from the wire string.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the wire string is not a Machine ID.
    pub fn machine_id(&self) -> Result<MachineId, ValueError> {
        MachineId::parse(&self.machine_id)
    }
}

/// `Open(id)` sent on Register when Cloud Dials.
#[derive(Clone, PartialEq, Message)]
pub struct Open {
    #[prost(string, tag = "1")]
    id: String,
}

impl Open {
    /// Build an Open carrying this Tunnel ID.
    #[must_use]
    pub fn new(tunnel_id: &TunnelId) -> Self {
        Self {
            id: tunnel_id.to_string(),
        }
    }

    /// Parse the Tunnel ID from the Open.
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

/// Bearer that authenticates Register and is rejected on Dial.
#[derive(Clone, Eq, PartialEq)]
pub struct PairingCredential(String);

/// Bearer Cloud presents on Dial.
#[derive(Clone, Eq, PartialEq)]
pub struct DialCredential(String);

/// Failures constructing a [`Relay`] or its credentials.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RelayError {
    #[error("credential must be a non-empty bearer")]
    EmptyCredential,
    #[error("Pairing Credential and Dial Credential must be distinct")]
    CredentialCollision,
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

/// Plaintext Cloud Relay: Register, Dial, Attach, opaque splice.
#[derive(Clone)]
pub struct Relay {
    pairing: PairingCredential,
    dial: DialCredential,
    state: Arc<Mutex<State>>,
}

struct State {
    machines: HashMap<MachineId, mpsc::Sender<Result<Open, Status>>>,
    pending: HashMap<TunnelId, Pending>,
}

struct Pending {
    to_machine: mpsc::Receiver<Result<TunnelFrame, Status>>,
    from_machine: mpsc::Sender<Result<TunnelFrame, Status>>,
}

impl Relay {
    /// Construct a Relay with distinct Pairing and Dial credentials.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::CredentialCollision`] when the two credentials are equal.
    pub fn new(pairing: PairingCredential, dial: DialCredential) -> Result<Self, RelayError> {
        if pairing.as_str() == dial.as_str() {
            return Err(RelayError::CredentialCollision);
        }
        Ok(Self {
            pairing,
            dial,
            state: Arc::new(Mutex::new(State {
                machines: HashMap::new(),
                pending: HashMap::new(),
            })),
        })
    }

    /// Serve plaintext HTTP/2 on an ephemeral local port.
    ///
    /// # Errors
    ///
    /// Returns I/O errors from binding the listener.
    pub async fn serve(
        &self,
    ) -> std::io::Result<(SocketAddr, JoinHandle<Result<(), tonic::transport::Error>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = CloudRelayServer::new(self.clone());
        let handle = tokio::spawn(
            Server::builder()
                .add_service(server)
                .serve_with_incoming(TcpListenerStream::new(listener)),
        );
        Ok((address, handle))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("relay state mutex poisoned")
    }
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

async fn forward_frames(
    mut inbound: Streaming<TunnelFrame>,
    tx: mpsc::Sender<Result<TunnelFrame, Status>>,
) {
    while let Some(Ok(frame)) = inbound.next().await {
        if tx.send(Ok(frame)).await.is_err() {
            break;
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
        match bearer(request.metadata()) {
            Some(bearer) if bearer == self.pairing.as_str() => {}
            _ => return Err(Status::unauthenticated("invalid Pairing Credential")),
        }
        let mut inbound = request.into_inner();
        let first = inbound
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("Register requires a Machine ID"))??;
        let machine_id = first
            .machine_id()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let (open_tx, open_rx) = mpsc::channel(TUNNEL_BUFFER);
        {
            let mut state = self.lock();
            if state.machines.contains_key(&machine_id) {
                return Err(Status::already_exists("Machine already registered"));
            }
            state.machines.insert(machine_id, open_tx.clone());
        }
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            while inbound.next().await.is_some() {}
            if let Ok(mut state) = state.lock()
                && state
                    .machines
                    .get(&machine_id)
                    .is_some_and(|current| current.same_channel(&open_tx))
            {
                state.machines.remove(&machine_id);
            }
        });
        Ok(Response::new(ReceiverStream::new(open_rx)))
    }

    async fn dial(
        &self,
        request: Request<Streaming<TunnelFrame>>,
    ) -> Result<Response<Self::DialStream>, Status> {
        match bearer(request.metadata()) {
            Some(bearer) if bearer == self.pairing.as_str() => {
                return Err(Status::permission_denied("Pairing Credential cannot Dial"));
            }
            Some(bearer) if bearer == self.dial.as_str() => {}
            _ => return Err(Status::unauthenticated("invalid Dial Credential")),
        }
        let machine_id = MachineId::parse(
            metadata_str(request.metadata(), MACHINE_ID_METADATA)
                .ok_or_else(|| Status::invalid_argument("missing or invalid machine-id"))?,
        )
        .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let tunnel_id = TunnelId::random();
        let (to_machine_tx, to_machine_rx) = mpsc::channel(TUNNEL_BUFFER);
        let (from_machine_tx, from_machine_rx) = mpsc::channel(TUNNEL_BUFFER);
        let open_tx = {
            let mut state = self.lock();
            let open_tx = state
                .machines
                .get(&machine_id)
                .cloned()
                .ok_or_else(|| Status::not_found("unknown Machine ID"))?;
            state.pending.insert(
                tunnel_id,
                Pending {
                    to_machine: to_machine_rx,
                    from_machine: from_machine_tx,
                },
            );
            open_tx
        };
        if open_tx.send(Ok(Open::new(&tunnel_id))).await.is_err() {
            self.lock().pending.remove(&tunnel_id);
            return Err(Status::unavailable("Register closed"));
        }
        tokio::spawn(forward_frames(request.into_inner(), to_machine_tx));
        Ok(Response::new(ReceiverStream::new(from_machine_rx)))
    }

    async fn attach(
        &self,
        request: Request<Streaming<TunnelFrame>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
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
        tokio::spawn(forward_frames(request.into_inner(), pending.from_machine));
        Ok(Response::new(ReceiverStream::new(pending.to_machine)))
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
    fn relay_rejects_identical_pairing_and_dial_credentials() {
        let pairing = PairingCredential::parse("same").unwrap();
        let dial = DialCredential::parse("same").unwrap();
        let Err(error) = Relay::new(pairing, dial) else {
            panic!("expected credential collision");
        };
        assert_eq!(error, RelayError::CredentialCollision);
    }

    #[test]
    fn register_request_rejects_a_non_machine_id() {
        assert!(RegisterRequest::default().machine_id().is_err());
    }

    #[test]
    fn open_rejects_a_non_tunnel_id() {
        assert!(Open::default().tunnel_id().is_err());
    }
}
