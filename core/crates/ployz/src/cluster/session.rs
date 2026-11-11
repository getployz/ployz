//! The Session: everything a Client knows about its connection to the entry Machine. The
//! channel every clone shares, the refusal that ends them all, the Machine this clone last
//! confirmed behind it, and the connector that dials it again.
//!
//! Two transitions replace the channel, and they differ on purpose. [`Session::redial`] is a
//! call's retry, taking `&mut self`: an entry that now describes itself as another Machine,
//! as after a reset, moves only this clone. [`Session::redial_shared`] is a Deploy's, taking
//! `&self`: that entry is refused, so the clones that share the channel never part.

use std::sync::{Arc, PoisonError, RwLock};

use ployz_core::{
    DescribeContractRequest, MachineId, MachineTarget, OpaquePayload,
    RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Rpc, RpcResponse, RpcResponseBody, op,
};
use tonic::{
    Streaming,
    codec::{CompressionEncoding, ProstCodec},
    codegen::http::uri::PathAndQuery,
    transport::Channel,
};

use super::status_error;
use crate::{
    connect::{
        CONNECT_CONFIRM_TIMEOUT, ConnectError, Connector, ExpectedCluster, TransportError,
        apply_timeout, target_request,
    },
    context::{Connection, ConnectionSource},
};

/// What dials the entry: the connection, how it was chosen, and the connector that opens it.
/// The same for every clone, which share one allocation of it.
#[derive(Clone)]
struct Endpoint {
    connection: Connection,
    source: ConnectionSource,
    connector: Arc<dyn Connector>,
}

#[derive(Clone)]
pub(crate) struct Session {
    /// Shared by every clone, so a redial on any of them reaches the rest.
    channel: Arc<RwLock<Channel>>,
    endpoint: Arc<Endpoint>,
    /// The Machine the entry described itself as when it was last confirmed.
    pub(super) entry: Option<MachineId>,
    /// The Machine `channel` was last confirmed to reach. Unlike `entry`, a failed redial
    /// keeps it, so a later one can tell the endpoint now reaches another Machine.
    pub(super) confirmed: Option<MachineId>,
    /// The Cluster the dial expected and what the first dial confirmed about it; None when
    /// it expected none. Boxed so a client stays small beside the enums that carry it.
    pub(super) cluster: Option<Box<ExpectedCluster>>,
    /// Set once a redial finds the entry in another Cluster. Every clone shares it.
    refused: Refusal,
}

/// The entry a redial found in another Cluster, shared by every clone of a client and
/// the streams it opened. The client ends then: no new call, stream or dial starts on any
/// clone. Work already started may still complete:
/// - a dial or tunnel in progress;
/// - a request already handed to the channel, including one waiting for its connection,
///   which tonic's own Tcp/Unix reconnect may carry;
/// - a send racing the refusal on another thread.
///
/// A dial, or a Watch or exec output read, that finishes afterwards returns the refusal
/// instead; a unary call or a stream open already under way returns what the daemon
/// answered. A request the daemon received isn't undone.
#[derive(Clone, Default)]
pub(crate) struct Refusal(Arc<std::sync::OnceLock<MachineId>>);

impl Refusal {
    /// Fails once a redial found the entry in another Cluster.
    pub(crate) fn check(&self) -> Result<(), ConnectError> {
        match self.0.get() {
            Some(&machine_id) => Err(ConnectError::ClusterMismatch { machine_id }),
            None => Ok(()),
        }
    }

    /// End the client. True for the first refusal only.
    fn set(&self, machine_id: MachineId) -> bool {
        self.0.set(machine_id).is_ok()
    }
}

impl Session {
    pub(super) fn new(
        channel: Channel,
        connection: Connection,
        source: ConnectionSource,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            channel: Arc::new(RwLock::new(channel)),
            endpoint: Arc::new(Endpoint {
                connection,
                source,
                connector,
            }),
            entry: None,
            confirmed: None,
            cluster: None,
            refused: Refusal::default(),
        }
    }

    pub(super) fn connection(&self) -> &Connection {
        &self.endpoint.connection
    }

    pub(super) fn source(&self) -> &ConnectionSource {
        &self.endpoint.source
    }

    pub(super) fn connector(&self) -> &Arc<dyn Connector> {
        &self.endpoint.connector
    }

    /// Dial through `connector` from now on, as a test's daemon that moved does.
    #[cfg(test)]
    pub(super) fn use_connector(&mut self, connector: Arc<dyn Connector>) {
        self.endpoint = Arc::new(Endpoint {
            connector,
            ..(*self.endpoint).clone()
        });
    }

    /// A Session to another entry of the same client: it ends when this one does.
    pub(super) fn ending_with(&mut self, other: &Self) {
        self.refused = other.refused.clone();
    }

    /// Fails once a redial found the entry in another Cluster.
    pub(super) fn ended(&self) -> Result<(), ConnectError> {
        self.refused.check()
    }

    /// What ends this client, for a stream it opened to check as it reads.
    pub(super) fn refusal(&self) -> Refusal {
        self.refused.clone()
    }

    /// The channel the latest dial opened.
    pub(super) fn channel(&self) -> Channel {
        self.channel
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replace the channel every clone shares.
    pub(super) fn share(&self, channel: Channel) {
        *self.channel.write().unwrap_or_else(PoisonError::into_inner) = channel;
    }

    /// The latest channel, once it can take a call, unless the client ended meanwhile.
    async fn ready_channel(&self) -> Result<Channel, ConnectError> {
        self.ended()?;
        let mut channel = self.channel();
        tower::ServiceExt::<http::Request<tonic::body::Body>>::ready(&mut channel).await?;
        self.ended()?;
        Ok(channel)
    }

    /// Specs carry Config bytes as JSON number arrays, so a 1 MB Config alone nears
    /// tonic's 4 MiB default, and stored log lines reach 8 MiB.
    /// Ready to send. Fails once the client has ended, also while it waited to be ready.
    pub(super) async fn grpc(&self) -> Result<tonic::client::Grpc<Channel>, ConnectError> {
        Ok(tonic::client::Grpc::new(self.ready_channel().await?)
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT))
    }

    /// Open the stream on `path` over this Session's own channel, with the status
    /// [`crate::connect::rpc_error`] gives a refusal. The channel stays ready, so the stream
    /// opens with no wait after the check. `request` is one message for a server stream or
    /// the whole feed of a client stream; both go out as the same gRPC call.
    pub(super) async fn open(
        &self,
        path: &'static str,
        gzip: bool,
        request: tonic::Request<impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static>,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let channel = self.ready_channel().await.map_err(|error| {
            if let ConnectError::Dial(error) = error {
                // Worded as the generated client words a channel that isn't ready.
                TransportError::from(tonic::Status::unknown(format!(
                    "Service was not ready: {error}"
                )))
            } else {
                TransportError::client_ended(error)
            }
        })?;
        let mut grpc = tonic::client::Grpc::new(channel)
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT);
        if gzip {
            grpc = grpc.accept_compressed(CompressionEncoding::Gzip);
        }
        grpc.ready()
            .await
            .map_err(|error| tonic::Status::unknown(format!("Service was not ready: {error}")))?;
        Ok(grpc
            .streaming(
                request,
                PathAndQuery::from_static(path),
                ProstCodec::<OpaquePayload, OpaquePayload>::default(),
            )
            .await?
            .into_inner())
    }

    /// `input` until the client ends, so a stream it opened takes no more input after that.
    pub(super) fn until_ended(
        &self,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static {
        let refused = self.refusal();
        tokio_stream::StreamExt::take_while(input, move |_| refused.check().is_ok())
    }

    /// One unary RPC on `path` over this Session's own channel, answered by the daemon's own
    /// error when it sent one.
    pub(super) async fn unary(
        &self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
    ) -> Result<RpcResponse, ConnectError> {
        let mut grpc = self.grpc().await?;
        let response = grpc
            .unary(
                target_request(payload, target),
                PathAndQuery::from_static(path),
                ProstCodec::<OpaquePayload, OpaquePayload>::default(),
            )
            .await
            .map_err(|status| status_error(status, path, target))?
            .into_inner()
            .decode_response()?;
        if let RpcResponseBody::Error(error) = &response.body {
            return Err(ConnectError::Remote(error.clone()));
        }
        Ok(response)
    }

    /// [`Self::unary`] for an RPC of the catalog, sent to the entry itself.
    pub(super) async fn call_once<T: Rpc>(
        &self,
        payload: OpaquePayload,
    ) -> Result<T::Response, ConnectError> {
        self.unary(T::PATH, payload, None)
            .await?
            .decode::<T>()
            .map_err(ConnectError::Codec)
    }

    /// Replace the channel, and the entry it confirmed, with a freshly confirmed one. An
    /// entry that now describes itself as another Machine, as after a reset, moves only
    /// this clone: its clones, a Deploy's among them, keep the Machine they confirmed.
    pub(super) async fn redial(&mut self) -> Result<(), ConnectError> {
        self.entry = None;
        let (channel, entry) = self.dial_confirmed().await?;
        match (self.confirmed, entry) {
            (Some(confirmed), Some(entry)) if confirmed != entry => {
                self.channel = Arc::new(RwLock::new(channel));
            }
            _ => self.share(channel),
        }
        self.confirmed = entry.or(self.confirmed);
        self.entry = entry;
        Ok(())
    }

    /// [`Self::redial`] for a session shared by reference, as a Deploy's is. Its entry
    /// stays the Machine it confirmed, so an endpoint that now reaches another is refused,
    /// even after a failed redial made this one forget its entry.
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`], when the
    /// entry doesn't answer on the new channel.
    pub(super) async fn redial_shared(&self) -> Result<(), ConnectError> {
        let (channel, entry) = self.dial_confirmed().await?;
        if let (Some(expected), Some(actual)) = (self.entry.or(self.confirmed), entry)
            && expected != actual
        {
            return Err(ConnectError::IdentityMismatch { expected, actual });
        }
        self.share(channel);
        Ok(())
    }

    /// Dial the connection again and confirm its entry on the new channel. Returns the
    /// channel unshared, with the entry it confirmed.
    /// Fails with the refusal when the client ended while it dialed, whatever the dial found.
    async fn dial_confirmed(&self) -> Result<(Channel, Option<MachineId>), ConnectError> {
        self.ended()?;
        let dialed = async {
            let candidate = Self {
                channel: Arc::new(RwLock::new(
                    self.connector().connect(self.connection()).await?,
                )),
                ..self.clone()
            };
            let entry = candidate.confirmed_entry().await?;
            Ok((candidate.channel(), entry))
        }
        .await;
        self.ended()?;
        dialed
    }

    /// One-shot check that this channel reaches a daemon. No unary retry — a
    /// down daemon must not stall the connection walk.
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`]
    /// when the daemon does not answer within [`CONNECT_CONFIRM_TIMEOUT`]. Resolves to the
    /// Sealing Key the confirmation said the entry holds, None when it said nothing.
    pub(super) async fn confirm_entry(
        &mut self,
    ) -> Result<Option<ployz_core::Sealing>, ConnectError> {
        let (entry, sealing) = self.confirmation().await?;
        self.entry = entry;
        self.confirmed = self.entry.or(self.confirmed);
        Ok(sealing)
    }

    /// The Machine this channel's daemon describes itself as, checked against the
    /// connection and, when the confirmation tells the key it holds, the Cluster this
    /// session's dial expected. None when neither the connection nor a Machine this session
    /// confirmed names one and the daemon doesn't say.
    async fn confirmed_entry(&self) -> Result<Option<MachineId>, ConnectError> {
        let (entry, sealing) = self.confirmation().await?;
        // Only a key tells the Cluster without Inspect; a confirmation without one leaves the
        // redial to the Machine ID, as before.
        if let (
            Some(cluster),
            Some(machine_id),
            Some(
                key @ (ployz_core::Sealing::Held { .. }
                | ployz_core::Sealing::AwaitsRecovery { .. }),
            ),
        ) = (&self.cluster, entry, sealing)
            && let Err(error) = cluster.expect.check(machine_id, Some(key), None)
        {
            if self.refused.set(machine_id) {
                tracing::warn!(
                    %machine_id,
                    "the entry now belongs to another Cluster; the session ends"
                );
            }
            return Err(error);
        }
        Ok(entry)
    }

    /// [`Self::confirmed_entry`], with the Sealing Key the daemon said it holds.
    async fn confirmation(
        &self,
    ) -> Result<(Option<MachineId>, Option<ployz_core::Sealing>), ConnectError> {
        let confirm = async {
            let payload =
                op::DescribeContract::into_request(DescribeContractRequest {}).encode()?;
            match self.call_once::<op::DescribeContract>(payload).await {
                Ok(description) => {
                    if let Some(expected) = self.connection().machine_id()
                        && *expected != description.machine_id
                    {
                        return Err(ConnectError::IdentityMismatch {
                            expected: *expected,
                            actual: description.machine_id,
                        });
                    }
                    if description.protocol_major != ployz_core::PROTOCOL_MAJOR {
                        return Err(ConnectError::Codec(
                            ployz_core::CodecError::UnsupportedProtocolMajor {
                                requested: description.protocol_major,
                                supported: ployz_core::PROTOCOL_MAJOR,
                            },
                        ));
                    }
                    Ok((Some(description.machine_id), description.sealing))
                }
                Err(ConnectError::Remote(_))
                    if self.connection().machine_id().is_none() && self.confirmed.is_none() =>
                {
                    Ok((None, None))
                }
                Err(error) => Err(error),
            }
        };
        tokio::time::timeout(CONNECT_CONFIRM_TIMEOUT, confirm)
            .await
            .map_err(|_| ConnectError::EntryNotReady)?
    }

    /// The id of the Machine this session enters by.
    ///
    /// # Errors
    ///
    /// Returns a transport or remote error when the entry does not describe itself.
    pub(super) async fn entry_id(&self) -> Result<MachineId, ConnectError> {
        let payload = op::DescribeContract::into_request(DescribeContractRequest {}).encode()?;
        apply_timeout(
            Some(CONNECT_CONFIRM_TIMEOUT),
            self.call_once::<op::DescribeContract>(payload),
        )
        .await
        .map(|description| description.machine_id)
    }
}
