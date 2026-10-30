use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    BridgeEndpointCapacity, ByteCapacity, ContainerAction, ContainerId, ContainerNow,
    ContainerObservation, ContainerWant, CopyRole, DataLoss, DataLossConfirmation,
    DescribeContractRequest, DockerVolumeId, DockerVolumeName, EnsureContainerRequest,
    EnvironmentValues, ForgetRequest, Forgotten, InspectRequest, InspectStorageRequest,
    InspectTelemetry, LeaveRequest, Left, ListContainersRequest, ListImagesRequest,
    ListMachinesRequest, ListVolumesRequest, LiveServices, LocalMachineRemoved,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineFailure, MachineId, MachineImages,
    MachineLabelKey, MachineName, MachineObservation, MachineRpcClient, MachineStorageObservation,
    MachineSuccess, MachineTarget, MachineUpdate, NameMatches, Namespace, ObservedDataLoss,
    OpaquePayload, PartialResult, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, RemoveVolumeRequest,
    RemoveVolumesRequest, ResolvedServiceSpec, Rpc, RpcError, RpcErrorCode, RpcResponseBody,
    RuntimeWatchFrame, RuntimeWatchRequest, StopOptions, StorageCapacity, TelemetryObservation,
    UnconfirmedDataLoss, VolumeInventory, VolumeRemoval, VolumeRemovalOutcome,
    decode_runtime_watch_frame, derive_live_services, op,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::{
    Streaming,
    codec::{CompressionEncoding, ProstCodec},
    codegen::http::uri::PathAndQuery,
    transport::Channel,
};

use crate::{
    connect::{
        BoxProxyStream, CONNECT_CONFIRM_TIMEOUT, ConnectError, Connector, TARGET_RPC_TIMEOUT,
        TransportError, UNARY_RETRY_DELAYS, apply_timeout, rpc_error, target_request,
    },
    context::{Connection, ConnectionSource},
    deploy::{
        DeploySnapshot, VolumeSnapshot,
        cleanup::{self, UserStarted},
    },
    service::ContainerOperationFailure,
};

mod capacity;
mod container_health;

pub(crate) use container_health::HealthWaitError;

const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(3);
/// Reply timeout for mutations on a live Machine, which may be mid-Deploy and
/// answer only once that work lets it through.
pub(crate) const LIVE_MUTATION_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Client {
    pub(crate) deployment_id: Option<ployz_core::DeploymentLogId>,
    /// Pull credentials by Service for the Deploy this client executes.
    pub(crate) registry_auth:
        std::collections::BTreeMap<ployz_core::ServiceName, ployz_core::RegistryAuth>,
    /// Shared by every clone, so a redial on any of them reaches the rest.
    channel: Arc<std::sync::RwLock<Channel>>,
    connection: Connection,
    source: ConnectionSource,
    connector: Arc<dyn Connector>,
    /// The Machine the entry described itself as when it was last confirmed.
    entry: Option<MachineId>,
    /// The Machine `channel` was last confirmed to reach. Unlike `entry`, a failed redial
    /// keeps it, so a later one can tell the endpoint now reaches another Machine.
    confirmed: Option<MachineId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MachineImagesObservation {
    pub machine_name: MachineName,
    pub images: MachineImages,
}

impl Client {
    pub(crate) fn new(
        channel: Channel,
        connection: Connection,
        source: ConnectionSource,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            deployment_id: None,
            registry_auth: std::collections::BTreeMap::new(),
            channel: Arc::new(std::sync::RwLock::new(channel)),
            connection,
            source,
            connector,
            entry: None,
            confirmed: None,
        }
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    #[must_use]
    pub fn connection_source(&self) -> &ConnectionSource {
        &self.source
    }

    /// The entry Machine this client reaches, as the daemon on its current channel
    /// described itself when confirmed. None when it didn't.
    pub(crate) fn entry_machine(&self) -> Option<MachineId> {
        self.entry
    }

    /// The Server this client enters, as people read it: its connection's name, else its
    /// Machine ID.
    pub(crate) fn server(&self) -> Option<String> {
        self.connection
            .server()
            .or_else(|| self.entry.map(|machine| machine.to_string()))
    }

    /// Replace the channel, and the entry it confirmed, with a freshly confirmed one. An
    /// entry that now describes itself as another Machine, as after a reset, moves only
    /// this client: its clones, a Deploy's among them, keep the Machine they confirmed.
    async fn redial(&mut self) -> Result<(), ConnectError> {
        self.entry = None;
        let (channel, entry) = self.dial_confirmed().await?;
        match (self.confirmed, entry) {
            (Some(confirmed), Some(entry)) if confirmed != entry => {
                self.channel = Arc::new(std::sync::RwLock::new(channel));
            }
            _ => self.share(channel),
        }
        self.confirmed = entry.or(self.confirmed);
        self.entry = entry;
        Ok(())
    }

    /// [`Self::redial`] for a client shared by reference, as a Deploy's is. Its entry
    /// stays the Machine it confirmed, so an endpoint that now reaches another is refused,
    /// even after a failed redial made this client forget its entry.
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`], when the
    /// entry doesn't answer on the new channel.
    pub(crate) async fn redial_shared(&self) -> Result<(), ConnectError> {
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
    async fn dial_confirmed(&self) -> Result<(Channel, Option<MachineId>), ConnectError> {
        let candidate = Self {
            channel: Arc::new(std::sync::RwLock::new(
                self.connector.connect(&self.connection).await?,
            )),
            ..self.clone()
        };
        let entry = candidate.confirmed_entry().await?;
        Ok((candidate.channel(), entry))
    }

    /// Replace the channel every clone shares.
    fn share(&self, channel: Channel) {
        *self
            .channel
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = channel;
    }

    /// One-shot check that this channel reaches a daemon. No unary retry — a
    /// down daemon must not stall the connection walk.
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`]
    /// when the daemon does not answer within [`CONNECT_CONFIRM_TIMEOUT`].
    pub(crate) async fn confirm_entry(&mut self) -> Result<(), ConnectError> {
        self.entry = self.confirmed_entry().await?;
        self.confirmed = self.entry.or(self.confirmed);
        Ok(())
    }

    /// The Machine this channel's daemon describes itself as, checked against the
    /// connection. None when neither the connection nor a Machine this client confirmed
    /// names one and the daemon doesn't say.
    async fn confirmed_entry(&self) -> Result<Option<MachineId>, ConnectError> {
        let confirm = async {
            let payload =
                op::DescribeContract::into_request(DescribeContractRequest {}).encode()?;
            match self.call_once::<op::DescribeContract>(payload, None).await {
                Ok(description) => {
                    if let Some(expected) = self.connection.machine_id()
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
                    Ok(Some(description.machine_id))
                }
                Err(ConnectError::Remote(_))
                    if self.connection.machine_id().is_none() && self.confirmed.is_none() =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        };
        tokio::time::timeout(CONNECT_CONFIRM_TIMEOUT, confirm)
            .await
            .map_err(|_| ConnectError::EntryNotReady)?
    }

    /// The id of the Machine this client enters by.
    ///
    /// # Errors
    ///
    /// Returns a transport or remote error when the entry does not describe itself.
    pub(crate) async fn entry_id(&self) -> Result<MachineId, ConnectError> {
        let payload = op::DescribeContract::into_request(DescribeContractRequest {}).encode()?;
        apply_timeout(
            Some(CONNECT_CONFIRM_TIMEOUT),
            self.call_once::<op::DescribeContract>(payload, None),
        )
        .await
        .map(|description| description.machine_id)
    }

    /// Issue one unary RPC. The response type is derived from the RPC, so a request
    /// cannot be paired with the wrong response.
    ///
    /// Transient transport drops (`Attempt`, tonic `Unavailable` / `DeadlineExceeded`)
    /// redial the same connection and retry up to four times; a failure the entry's router
    /// reported retries on the channel it came back on. Domain `Remote` errors and other gRPC
    /// status codes are not retried.
    ///
    /// # Errors
    ///
    /// Returns an encoding, transport, or remote RPC error when the call does
    /// not succeed within the bounded retry policy.
    pub async fn call<T: Rpc>(
        &mut self,
        request: T::Request,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        self.call_retried::<T>(payload, target, None).await
    }

    /// Retry only reads or known-idempotent setup RPCs with short attempts.
    /// Callers must use `call_unretried` for mutations with uncertain outcomes.
    pub(crate) async fn call_repeatable<T: Rpc>(
        &mut self,
        request: T::Request,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        self.call_repeatable_for::<T>(request, target, None, None, crate::setup_retry::WAIT)
            .await
            .map_err(|error| match error {
                crate::setup_retry::Error::Permanent(error) => error,
                exhausted => ConnectError::Exhausted(Box::new(exhausted)),
            })
    }

    /// Retry a read or stable-identity request for the caller's remaining time budget.
    /// `server` names the target in the connectivity warning; `expected` announces an
    /// anticipated outage in its place.
    pub(crate) async fn call_repeatable_for<T: Rpc>(
        &mut self,
        request: T::Request,
        target: Option<&MachineTarget>,
        server: Option<&str>,
        expected: Option<crate::setup_retry::Expected>,
        wait: Duration,
    ) -> Result<T::Response, crate::setup_retry::Error<ConnectError>> {
        let payload = T::into_request(request)
            .encode()
            .map_err(ConnectError::from)
            .map_err(crate::setup_retry::Error::Permanent)?;
        let mut redial = false;
        let progress = match (server, target) {
            (Some(server), _) => format!("Could not reach Server {server}"),
            (None, Some(target)) => format!(
                "Could not reach Server {} via {}",
                target.as_str().escape_debug(),
                self.connection
            ),
            (None, None) => format!("Could not reach {}", self.connection),
        };
        crate::setup_retry::run_expecting(
            self,
            &progress,
            expected,
            wait,
            ConnectError::is_setup_retryable,
            async |client| {
                let reconnect = redial;
                redial = true;
                client
                    .setup_read_attempt::<T>(payload.clone(), target, reconnect)
                    .await
            },
        )
        .await
    }

    /// One read with no retry or notice of its own, for a readiness check whose
    /// caller's wait retries it and owns what the user sees (#3620).
    pub(crate) async fn read_once<T: Rpc>(
        &mut self,
        request: T::Request,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        self.setup_read_attempt::<T>(payload, target, false).await
    }

    async fn setup_read_attempt<T: Rpc>(
        &mut self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        redial: bool,
    ) -> Result<T::Response, ConnectError> {
        tokio::time::timeout(
            Duration::from_secs(5),
            self.unary_attempt::<T>(payload, target, redial),
        )
        .await
        .unwrap_or_else(|_| {
            Err(tonic::Status::deadline_exceeded("Machine setup read timed out").into())
        })
    }

    /// Mutations that must not be replayed after a lost response. `reply_timeout` is
    /// usually [`LIVE_MUTATION_REPLY_TIMEOUT`].
    pub(crate) async fn call_unretried<T: Rpc>(
        &self,
        request: T::Request,
        target: Option<&MachineTarget>,
        reply_timeout: Duration,
    ) -> Result<T::Response, ConnectError> {
        tokio::time::timeout(
            reply_timeout,
            self.call_once::<T>(T::into_request(request).encode()?, target),
        )
        .await
        .unwrap_or_else(|_| {
            Err(tonic::Status::deadline_exceeded("Machine setup mutation reply timed out").into())
        })
    }

    /// Issue a retryable read-only targeted RPC with a deadline per attempt.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when request encoding fails or the targeted read
    /// does not succeed within the bounded retry policy.
    pub(crate) async fn read<T: Rpc>(
        &mut self,
        request: T::Request,
        target: &MachineTarget,
    ) -> Result<T::Response, RpcError> {
        self.try_read::<T>(request, target).await.map_err(rpc_error)
    }

    pub(crate) async fn try_read<T: Rpc>(
        &mut self,
        request: T::Request,
        target: &MachineTarget,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request)
            .encode()
            .map_err(ConnectError::Codec)?;
        let timeout = crate::connect::target_read_timeout();
        self.call_retried::<T>(payload, Some(target), Some(timeout))
            .await
    }

    async fn call_retried<T: Rpc>(
        &mut self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        timeout: Option<Duration>,
    ) -> Result<T::Response, ConnectError> {
        let mut delays = UNARY_RETRY_DELAYS.iter().copied();
        let mut redial = false;
        // One budget across retries: fast failures retry inside it, and a vanished
        // Machine costs the budget once instead of once per attempt.
        let deadline = timeout.map(|timeout| tokio::time::Instant::now() + timeout);
        // A reply on an earlier attempt means the target was not silent throughout.
        let mut reply: Option<ConnectError> = None;
        // An earlier attempt that went out and lost its answer may have been done.
        let mut lost: Option<ConnectError> = None;
        loop {
            let outcome = match deadline {
                Some(deadline) => {
                    self.bounded_attempt::<T>(&payload, target, redial, deadline)
                        .await
                }
                None => {
                    self.unary_attempt::<T>(payload.clone(), target, redial)
                        .await
                }
            };
            let error = match outcome {
                Ok(response) => return Ok(response),
                Err(error) => error,
            };
            // The entry's router made this failure, so the channel to the entry works.
            let entry_answered = error.routed_by_entry();
            let error = crate::connect::keep_target_reply(&mut reply, error);
            if error.retry() != ployz_core::Retry::Now {
                return Err(crate::connect::keep_lost_call(lost, error));
            }
            let Some(delay) = delays.next() else {
                return Err(crate::connect::keep_lost_call(lost, error));
            };
            if deadline.is_some_and(|deadline| tokio::time::Instant::now() + delay >= deadline) {
                return Err(crate::connect::keep_lost_call(lost, error));
            }
            tokio::time::sleep(delay).await;
            redial = !entry_answered;
            if error.target_replied() {
                // The target answered after the loss: that answer is the news now.
                lost = None;
                reply = Some(error);
            } else if error.unreplied() {
                lost = Some(error);
            }
        }
    }

    /// Only a deadline spent waiting on a confirmed entry is evidence the target is silent;
    /// one spent reconnecting to the entry says nothing about it. An entry that just reported
    /// the target silent on this channel counts as confirmed, as one a redial checked does.
    async fn bounded_attempt<T: Rpc>(
        &mut self,
        payload: &OpaquePayload,
        target: Option<&MachineTarget>,
        redial: bool,
        deadline: tokio::time::Instant,
    ) -> Result<T::Response, ConnectError> {
        if redial {
            match tokio::time::timeout_at(deadline, self.redial()).await {
                Ok(confirmed) => confirmed?,
                Err(_) => {
                    return Err(tonic::Status::deadline_exceeded(
                        "entry Machine reconnect timed out",
                    )
                    .into());
                }
            }
        }
        match tokio::time::timeout_at(deadline, self.call_once::<T>(payload.clone(), target)).await
        {
            Ok(outcome) => outcome,
            Err(_) => Err(ConnectError::Rpc(TransportError::budget_expired(
                "target Machine RPC timed out",
            ))),
        }
    }

    async fn unary_attempt<T: Rpc>(
        &mut self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        redial: bool,
    ) -> Result<T::Response, ConnectError> {
        if redial {
            self.redial().await?;
        }
        self.call_once::<T>(payload, target).await
    }

    /// One-shot targeted RPC. No retry: a mutating operation that is not keyed
    /// must not be re-issued after a dropped response.
    pub(crate) async fn invoke<T: Rpc>(
        &self,
        request: T::Request,
        target: &MachineTarget,
        timeout: Option<Duration>,
    ) -> Result<T::Response, RpcError> {
        self.invoke_answered::<T>(request, target, timeout)
            .await
            .map_err(rpc_error)
    }

    /// [`Self::invoke`] on the entry Machine itself, which this client always reaches.
    pub(crate) async fn invoke_entry<T: Rpc>(
        &self,
        request: T::Request,
        timeout: Option<Duration>,
    ) -> Result<T::Response, RpcError> {
        let call = async {
            let payload = T::into_request(request).encode()?;
            apply_timeout(timeout, self.call_once::<T>(payload, None)).await
        };
        call.await.map_err(rpc_error)
    }

    /// [`Self::invoke`], keeping whether the failure came back from a daemon or no reply came.
    pub(crate) async fn invoke_answered<T: Rpc>(
        &self,
        request: T::Request,
        target: &MachineTarget,
        timeout: Option<Duration>,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        apply_timeout(timeout, self.call_once::<T>(payload, Some(target))).await
    }

    /// One unary RPC from an already-shaped request body. No retry, as [`Self::invoke`].
    pub(crate) async fn invoke_raw(
        &self,
        request: &ployz_core::RpcRequest,
        path: &'static str,
        target: Option<&MachineTarget>,
        timeout: Option<Duration>,
    ) -> Result<ployz_core::RpcResponse, ConnectError> {
        let payload = request.encode().map_err(ConnectError::Codec)?;
        apply_timeout(timeout, async {
            let mut grpc = self.grpc();
            grpc.ready().await?;
            Ok(grpc
                .unary(
                    target_request(payload, target),
                    PathAndQuery::from_static(path),
                    ProstCodec::<OpaquePayload, OpaquePayload>::default(),
                )
                .await
                .map_err(|status| status_error(status, path, target))?
                .into_inner()
                .decode_response()?)
        })
        .await
    }

    /// The channel the latest dial opened.
    fn channel(&self) -> Channel {
        self.channel
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Specs carry Config bytes as JSON number arrays, so a 1 MB Config alone nears
    /// tonic's 4 MiB default, and stored log lines reach 8 MiB.
    fn grpc(&self) -> tonic::client::Grpc<Channel> {
        tonic::client::Grpc::new(self.channel())
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
    }

    fn machine_rpc(&self) -> MachineRpcClient<Channel> {
        MachineRpcClient::new(self.channel())
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT)
    }

    pub(crate) async fn build_stream(
        &self,
        target: &MachineTarget,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        // Submission is one-shot. A lost response must never replay a Build.
        Ok(self
            .machine_rpc()
            .build(target_request(input, Some(target)))
            .await
            .map_err(|status| TransportError::from(status).name_refusal(op::Build::PATH, target))?
            .into_inner())
    }

    pub(crate) async fn exec_stream(
        &self,
        target: &MachineTarget,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .exec(target_request(input, Some(target)))
            .await
            .map_err(|status| TransportError::from(status).name_refusal(op::Exec::PATH, target))?
            .into_inner())
    }

    pub(crate) async fn tail_logs_stream(
        &self,
        target: &MachineTarget,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .tail_logs(target_request(request, Some(target)))
            .await
            .map_err(|status| {
                TransportError::from(status).name_refusal(op::TailLogs::PATH, target)
            })?
            .into_inner())
    }

    pub(crate) async fn log_history_stream(
        &self,
        target: &MachineTarget,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .log_history(target_request(request, Some(target)))
            .await
            .map_err(|status| {
                TransportError::from(status).name_refusal(op::LogHistory::PATH, target)
            })?
            .into_inner())
    }

    pub(crate) async fn runtime_watch_stream(
        &self,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self
            .machine_rpc()
            .accept_compressed(CompressionEncoding::Gzip);
        Ok(rpc
            .runtime_watch(tonic::Request::new(request))
            .await?
            .into_inner())
    }

    /// The Cluster as the first Runtime Watch frame shows it.
    pub(crate) async fn runtime_frame(&self) -> Result<RuntimeWatchFrame, RpcError> {
        let payload = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
            .encode()
            .map_err(|error| RpcError::from(ConnectError::from(error)))?;
        let mut stream = self
            .runtime_watch_stream(payload)
            .await
            .map_err(|error| error.to_rpc_error())?;
        let Some(frame) = stream
            .message()
            .await
            .map_err(|status| TransportError::from_stream_status(status).to_rpc_error())?
        else {
            return Err(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Runtime Watch ended before its first frame".into(),
                details: Value::Null,
                cause: Vec::new(),
            });
        };
        decode_runtime_watch_frame(&frame)
            .map_err(|error| crate::ui::rpc_error(RpcErrorCode::Internal, &error))
    }

    async fn call_once<T: Rpc>(
        &self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        let mut grpc = self.grpc();
        grpc.ready().await?;
        let response = grpc
            .unary(
                target_request(payload, target),
                PathAndQuery::from_static(T::PATH),
                ProstCodec::<OpaquePayload, OpaquePayload>::default(),
            )
            .await
            .map_err(|status| status_error(status, T::PATH, target))?
            .into_inner()
            .decode_response()?;
        if let RpcResponseBody::Error(error) = &response.body {
            return Err(ConnectError::Remote(error.clone()));
        }
        response.decode::<T>().map_err(ConnectError::Codec)
    }

    /// List Docker Volumes on each Machine that invites an RPC.
    ///
    /// Down, Unknown, and Unrecognized are omitted. Up and Suspect are read
    /// independently with bounded transient retries.
    /// Keep each probed target's success or typed failure.
    pub async fn list_volumes(
        &self,
        machines: &[MachineObservation],
    ) -> PartialResult<VolumeInventory, RpcError> {
        let mut requests = Vec::new();
        let mut omissions = Vec::new();
        for machine in machines {
            if !machine.invites_rpc() {
                omissions.push(machine.machine.id);
                continue;
            }
            let machine_id = machine.machine.id;
            let client = self.clone();
            requests.push(async move { list_volumes_on_machine(client, machine_id).await });
        }
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions,
        };
        for outcome in join_all(requests).await {
            match outcome {
                Ok(success) => result.successes.push(success),
                Err(failure) => result.failures.push(failure),
            }
        }
        result
    }

    /// Inspect storage on each Machine that invites an RPC.
    pub async fn inspect_storage(
        &self,
        machines: &[MachineObservation],
    ) -> PartialResult<StorageCapacity, RpcError> {
        let mut requests = Vec::new();
        let mut omissions = Vec::new();
        for machine in machines {
            if !machine.invites_rpc() {
                omissions.push(machine.machine.id);
                continue;
            }
            let mut client = self.clone();
            let machine_id = machine.machine.id;
            requests.push(async move {
                let result = client
                    .read::<op::InspectStorage>(
                        InspectStorageRequest {},
                        &MachineTarget::from(&machine_id),
                    )
                    .await;
                (machine_id, result)
            });
        }
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions,
        };
        for (machine_id, outcome) in join_all(requests).await {
            match outcome {
                Ok(value) => result.successes.push(MachineSuccess { machine_id, value }),
                Err(error) => result.failures.push(MachineFailure { machine_id, error }),
            }
        }
        result
    }

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// Each volume is identified by Machine plus name. Fan-out is a Partial
    /// Result: each requested identity carries removal, failure, or omission
    /// evidence. Machines that do not invite RPC are omitted. Forced removal is off
    /// unless `force` is set.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn remove_volumes(
        &mut self,
        request: RemoveVolumesRequest,
    ) -> Result<Vec<VolumeRemoval>, RpcError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        Ok(remove_volumes_on(self, &machines.machines, request).await)
    }

    /// Which Machines hold each of the Docker Volumes `sought`, as the Docker Volume
    /// or as a mirror slot: the evidence a Deploy that deletes Volume data is reviewed
    /// against. Every Machine that did not answer, or could not read one of them, is
    /// named in `unanswered`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_volumes(
        &mut self,
        sought: Vec<DockerVolumeName>,
    ) -> Result<ployz_store::VolumeObservation, RpcError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        let inspector = self.clone();
        // Seeking nothing asks only who answers: storage holds nothing sought.
        let storage: &[MachineObservation] = match sought.is_empty() {
            true => &[],
            false => &machines.machines,
        };
        let (listed, inspected) = tokio::join!(
            self.list_volumes(&machines.machines),
            inspector.inspect_storage(storage)
        );
        let mut unanswered: BTreeSet<MachineId> = listed
            .failures
            .iter()
            .chain(&inspected.failures)
            .map(|failure| failure.machine_id)
            .chain(listed.omissions.iter().copied())
            .collect();
        let mut held = BTreeSet::new();
        for success in listed.successes {
            for failure in &success.value.failures {
                if sought.contains(&failure.id.name) {
                    unanswered.insert(failure.id.machine_id);
                }
            }
            held.extend(
                success
                    .value
                    .volumes
                    .into_iter()
                    .map(|volume| volume.id)
                    .filter(|id| sought.contains(&id.name)),
            );
        }
        for success in inspected.successes {
            held.extend(
                success
                    .value
                    .roles()
                    .filter(|(name, role)| *role == CopyRole::Slot && sought.contains(name))
                    .map(|(name, _)| DockerVolumeId {
                        machine_id: success.machine_id,
                        name: name.clone(),
                    }),
            );
        }
        Ok(ployz_store::VolumeObservation {
            sought,
            held: held.into_iter().collect(),
            unanswered: unanswered.into_iter().collect(),
            failure: None,
        })
    }

    /// Every Service with a Container on a Machine that answered, by Namespace: what a
    /// full Deploy's prune may remove. Every Machine that should answer and did not is
    /// named in `unanswered`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_running(&mut self) -> Result<ployz_store::RunningObservation, RpcError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        let live = self
            .live_services_from(&machines.machines, EnvironmentValues::Redacted)
            .await
            .map_err(RpcError::from)?;
        let services: BTreeSet<_> = live
            .containers
            .successes
            .iter()
            .flat_map(|success| &success.value)
            .map(ContainerObservation::identity)
            .collect();
        // A Machine already marked down is left out, as the Deploy's prune leaves it.
        let unanswered: BTreeSet<MachineId> = live
            .containers
            .failures
            .iter()
            .map(|failure| failure.machine_id)
            .collect();
        // The entry's own path to a Server that didn't answer says why, when large
        // packets to it are dropped (#3490). A failed dial sends only small packets, so
        // a Server the entry couldn't reach is left out, as the router leaves it.
        let dropped = machines
            .machines
            .iter()
            .filter(|machine| {
                machine
                    .path
                    .is_some_and(|path| path.large_packets == ployz_core::LargePackets::Dropped)
                    && live.containers.failures.iter().any(|failure| {
                        failure.machine_id == machine.machine.id
                            && failure.error.target_unanswered()
                            && !failure.error.undialed()
                    })
            })
            .map(|machine| machine.machine.name.clone())
            .collect();
        Ok(ployz_store::RunningObservation {
            services: services.into_iter().collect(),
            unanswered: unanswered.into_iter().collect(),
            failure: None,
            dropped,
        })
    }

    /// Every copy of every Volume on every Machine, by role, slots included. Every
    /// Machine that did not answer is named in `unanswered`, never assumed empty.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_copies(&mut self) -> Result<crate::sdk::CopyObservation, RpcError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        let inspected = self.inspect_storage(&machines.machines).await;
        let unanswered: BTreeSet<MachineId> = inspected
            .failures
            .iter()
            .map(|failure| failure.machine_id)
            .chain(inspected.omissions.iter().copied())
            .collect();
        let copies = inspected
            .successes
            .iter()
            .flat_map(|success| {
                success
                    .value
                    .roles()
                    .map(|(name, role)| crate::sdk::ObservedCopy {
                        machine_id: success.machine_id,
                        name: name.clone(),
                        role,
                    })
            })
            .collect();
        Ok(crate::sdk::CopyObservation {
            copies,
            unanswered: unanswered.into_iter().collect(),
        })
    }

    /// Live Observation of Data Loss that removing `machine` would cause.
    ///
    /// This is not a complete Cluster view. Mutates nothing: it is safe to
    /// call when the operator then cancels.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible or is
    /// ambiguous, or when the Machine did not respond so Data Loss cannot be
    /// listed.
    pub async fn data_loss_if_machine_removed(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<ObservedDataLoss, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let observation = visible_machine(machine, &machines)?;
        data_loss_on_machine(self, observation).await
    }

    /// Remove `machine` after an exact Data Loss confirmation.
    ///
    /// Re-reads Data Loss at execute time. Fresh Data Loss the confirmation does
    /// not cover refuse the removal. Confirmed identities that disappeared
    /// are ignored.
    /// Resets the Machine. A reset warning is returned, not swallowed.
    /// Refused before reset or membership mutation when this is the last Machine
    /// and a Management Client other than Cloud holds a key, or Cloud holds it and
    /// `remover` isn't Cloud.
    ///
    /// Sending it again is the retry: every guard runs again while the Machine is listed.
    /// A Machine ID the list no longer holds was already forgotten, so this answers removed,
    /// with a warning that its reset can't be confirmed: only a Leave reply confirms one.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible or is
    /// the current entry while another Machine is visible, when the Machine
    /// is the last Machine and its holders refuse it or can't be read, when the Machine
    /// did not respond so Data Loss cannot be listed, when the confirmation does not cover the
    /// fresh Data Loss, or when reset or shared-row removal fails.
    pub async fn remove_machine(
        &mut self,
        machine: &MachineTarget,
        confirm_data_loss: &DataLossConfirmation,
        remover: Remover,
    ) -> Result<LocalMachineRemoved, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let Some(observation) = listed_unless_forgotten(machine, &machines)? else {
            return Ok(LocalMachineRemoved {
                reset_warning: Some(RESET_UNCONFIRMED.into()),
            });
        };
        let selected = observation.machine.id;
        let current = self
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .map_err(RpcError::from)?
            .machine_id;
        if selected == current && machines.len() > 1 {
            return Err(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message:
                    "the current entry Machine cannot be removed while another Machine is visible"
                        .into(),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
        let hold = refuse_last_managed(self, &machines, selected).await?;
        if hold == CloudHold::Last && remover == Remover::Operator {
            return Err(cloud_holds_last(selected));
        }
        evict_machine(self, observation, confirm_data_loss, current).await
    }

    /// Remove Cluster membership for `machine` without resetting it.
    ///
    /// Refused before membership mutation when this is the last Machine and any
    /// Management Client holds a key: Cloud lets go of its last Machine only by resetting it.
    /// Also refused while the Machine answers with a copy of any Volume, which only its
    /// reset demotes, or replies with an error; a Machine that sends no reply goes.
    ///
    /// Sending it again is the retry: every guard runs again while the Machine is listed,
    /// and a Machine ID the list no longer holds was already forgotten.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible, when
    /// it is the last Machine and a Management Client holds a key or its holders
    /// can't be read, when it answers holding a Volume copy or replies with an error,
    /// or when shared-row removal fails.
    pub async fn remove_machine_membership(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<(), RpcError> {
        self.remove_membership(machine, DeadConfirmed::Yes).await
    }

    /// As [`Self::remove_machine_membership`], but a Server confirmed while it answered
    /// that stops answering before it goes is refused: dead was never typed for it.
    pub(crate) async fn remove_membership(
        &mut self,
        machine: &MachineTarget,
        dead: DeadConfirmed,
    ) -> Result<(), RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let Some(observation) = listed_unless_forgotten(machine, &machines)? else {
            return Ok(());
        };
        let selected = observation.machine.id;
        if refuse_last_managed(self, &machines, selected).await? == CloudHold::Last {
            return Err(cloud_holds_last(selected));
        }
        let held = match (self.held_copies(observation).await?, dead) {
            (HeldCopies::Dead, DeadConfirmed::Yes) => BTreeSet::new(),
            (HeldCopies::Dead, DeadConfirmed::No) => {
                return Err(stopped_answering(&observation.machine));
            }
            (HeldCopies::Answered(held), _) => held,
            (HeldCopies::Gone, _) => return Ok(()),
        };
        if !held.is_empty() {
            return Err(copies_left_behind(
                &observation.machine.name,
                held.iter()
                    .map(|name| Namespace::declared_volume_name(name).unwrap_or(name.as_str())),
            ));
        }
        forget(self, selected).await
    }

    pub(crate) async fn held_copies(
        &self,
        machine: &MachineObservation,
    ) -> Result<HeldCopies, RpcError> {
        if !machine.invites_rpc() {
            return Ok(HeldCopies::Dead);
        }
        let mut client = self.clone();
        match client
            .try_read::<op::InspectStorage>(
                InspectStorageRequest {},
                &MachineTarget::from(&machine.machine.id),
            )
            .await
        {
            Ok(storage) => Ok(HeldCopies::Answered(
                storage.roles().map(|(name, _)| name.clone()).collect(),
            )),
            Err(error) if error.target_unanswered() => Ok(HeldCopies::Dead),
            Err(error) => {
                let error = rpc_error(error);
                if error.server_unlisted(&machine.machine.id) {
                    return Ok(HeldCopies::Gone);
                }
                Err(RpcError {
                    code: RpcErrorCode::Unavailable,
                    // Often only the entry was busy, as when several forgets share it.
                    message: format!(
                        "Couldn't check whether Server {} holds Volume copies. No changes made. \
                         Run the command again, one Server at a time if you're forgetting several.",
                        machine.machine.name
                    ),
                    details: Value::Null,
                    cause: std::iter::once(error.message).chain(error.cause).collect(),
                })
            }
        }
    }

    /// List images on each target Machine with independent bounded retries.
    pub async fn list_images(
        &self,
        reference: Option<String>,
        targets: &[Machine],
    ) -> PartialResult<MachineImagesObservation, RpcError> {
        let tasks = targets.iter().map(|machine| {
            let mut client = self.clone();
            let machine_id = machine.id;
            let machine_name = machine.name.clone();
            let reference = reference.clone();
            async move {
                let response = client
                    .read::<op::ListImages>(
                        ListImagesRequest {
                            reference,
                            last_tagged: false,
                        },
                        &MachineTarget::from(&machine_id),
                    )
                    .await;
                (machine_id, machine_name, response)
            }
        });
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for (machine_id, machine_name, outcome) in join_all(tasks).await {
            match outcome {
                Ok(images) => result.successes.push(MachineSuccess {
                    machine_id,
                    value: MachineImagesObservation {
                        machine_name,
                        images,
                    },
                }),
                Err(error) => result.failures.push(MachineFailure { machine_id, error }),
            }
        }
        result
    }

    pub async fn dial_proxy(
        &self,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        self.connector
            .dial_proxy(&self.connection, network, address)
            .await
    }

    /// Membership Observation of Machines visible from this entry Machine.
    ///
    /// # Errors
    ///
    /// Returns a connection or remote RPC error from `ListMachines`. The whole
    /// call, including unary retries, is capped at 10 seconds and surfaces as
    /// deadline exceeded when that cap elapses.
    pub async fn machines(&mut self) -> Result<Vec<MachineObservation>, ConnectError> {
        match tokio::time::timeout(
            TARGET_RPC_TIMEOUT,
            self.call::<op::ListMachines>(ListMachinesRequest {}, None),
        )
        .await
        {
            Ok(result) => result.map(|list| list.machines),
            Err(_) => Err(tonic::Status::deadline_exceeded("target Machine RPC timed out").into()),
        }
    }

    /// Fill in each reachable Machine's storage; returns the Machines left without it.
    pub(crate) async fn observe_machine_storage(
        &self,
        machines: &mut [MachineObservation],
    ) -> StorageGaps {
        let mut tasks = JoinSet::new();
        for (index, machine) in machines.iter().enumerate() {
            if machine.invites_rpc() {
                tasks.spawn(observe_machine_storage(
                    self.clone(),
                    index,
                    machine.machine.id,
                ));
            }
        }
        let mut result = StorageGaps::default();
        while let Some(observed) = tasks.join_next().await {
            let (index, observed) = observed.expect("Machine storage observation does not panic");
            let Some(machine) = machines.get_mut(index) else {
                continue;
            };
            let machine_id = machine.machine.id;
            match observed {
                Some(Ok(storage)) => machine.storage = storage,
                Some(Err(error)) => result.failures.push(MachineFailure { machine_id, error }),
                None => result.omissions.push(machine_id),
            }
        }
        result
    }

    /// Read the space under each reachable Machine's Docker root. A Machine that fails or is
    /// slow to answer is left out, since the reading only ever adds a warning.
    pub(crate) async fn observe_docker_disks(
        &self,
        machines: &[MachineObservation],
    ) -> Vec<(MachineId, ByteCapacity)> {
        let reads = machines
            .iter()
            .filter(|observed| observed.invites_rpc())
            .map(|observed| async move {
                let machine = observed.machine.id;
                let request = InspectRequest {
                    telemetry: InspectTelemetry::Full,
                    ..Default::default()
                };
                let target = MachineTarget::from(&machine);
                let read = self.invoke::<op::Inspect>(request, &target, None);
                match tokio::time::timeout(STORAGE_OBSERVATION_TIMEOUT, read).await {
                    Ok(Ok(details)) => match details.telemetry {
                        Some(TelemetryObservation::Full { host, .. }) => {
                            Some((machine, host.docker_root()))
                        }
                        _ => None,
                    },
                    Ok(Err(_)) | Err(_) => None,
                }
            });
        join_all(reads).await.into_iter().flatten().collect()
    }

    /// List Containers on every visible Machine and derive the observed Services.
    ///
    /// `environment` chooses whether observations carry real environment values
    /// or redacted ones.
    ///
    /// # Errors
    /// Returns an error when Machines cannot be listed or a listing task fails.
    pub async fn live_services(
        &mut self,
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = self.machines().await?;
        self.live_services_from(&machines, environment).await
    }

    pub(crate) async fn live_services_from(
        &self,
        machines: &[MachineObservation],
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        self.live_services_until(machines, environment, |_| false)
            .await
    }

    /// [`Self::live_services_from`], returning early once `found` holds for the
    /// answers so far. An early result leaves out the Machines yet to answer, as
    /// neither a failure nor an omission, so only what `found` checked is complete.
    pub(crate) async fn live_services_until(
        &self,
        machines: &[MachineObservation],
        environment: EnvironmentValues,
        found: impl Fn(&LiveServices<RpcError>) -> bool,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let mut live = derive_live_services(PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        let mut tasks = JoinSet::new();
        for machine in machines {
            if machine.invites_rpc() {
                tasks.spawn(list_on_machine(
                    self.clone(),
                    machine.machine.id,
                    environment,
                ));
            } else {
                live.containers.omissions.push(machine.machine.id);
            }
        }
        // TODO: retain target failures and omissions beside successful container
        // observations; an entry-relative answer remains partial evidence.
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(success)) => live.containers.successes.push(success),
                Ok(Err(failure)) => live.containers.failures.push(failure),
                Err(error) => return Err(error.into()),
            }
            if found(&live) {
                return Ok(live);
            }
        }
        Ok(live)
    }

    /// Converge one Container on `machine_id` to `want`. A refusal answers as the error
    /// the replaced Container verbs answered.
    pub async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<ContainerNow, RpcError> {
        self.ensure_settled(machine_id, want, timeout)
            .await
            .map(|(now, _)| now)
    }

    pub(crate) async fn ensure_settled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<(ContainerNow, Option<ployz_core::HealthVerdict>), RpcError> {
        self.ensure_unsettled(machine_id, want, timeout)
            .await?
            .into_settled()
    }

    /// Converge one Container and keep a refusal as the owner typed it, for
    /// callers that act on which rotation rule refused.
    pub(crate) async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<ployz_core::EnsuredContainer, RpcError> {
        let target = MachineTarget::from(machine_id);
        crate::sealed_compat::admit(self, &target, &want).await?;
        let payload = op::EnsureContainer::into_request(EnsureContainerRequest { want })
            .encode()
            .map_err(|error| rpc_error(ConnectError::Codec(error)))?;
        apply_timeout(
            timeout,
            self.call_once::<op::EnsureContainer>(payload, Some(&target)),
        )
        .await
        .map_err(|error| answered_by(machine_id, error))
    }

    pub async fn change_observed_service(
        &self,
        service: &ployz_core::ServiceObservation,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<u16>,
        cancellation: &CancellationToken,
    ) -> PartialResult<ContainerId, ContainerOperationFailure> {
        let mut tasks = JoinSet::new();
        let mut task_targets = HashMap::new();
        let mut queued = HashMap::<MachineId, u32>::new();
        for container in service.containers_for(action) {
            *queued
                .entry(container.as_observation().machine_id)
                .or_default() += 1;
        }
        for container in service.containers_for(action) {
            let observation = container.as_observation();
            let machine_id = observation.machine_id;
            let container_id = observation.container_id;
            let handle = tasks.spawn(change_on_machine(
                self.clone(),
                machine_id,
                container_id,
                action,
                StopOptions {
                    signal: signal.clone(),
                    grace_period_seconds,
                },
                queued.get(&machine_id).copied().unwrap_or(1),
                cancellation.clone(),
            ));
            task_targets.insert(handle.id(), (machine_id, container_id));
        }
        let mut outcomes = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        while let Some(joined) = tasks.join_next_with_id().await {
            match joined {
                Ok((id, Ok(success))) => {
                    task_targets.remove(&id);
                    outcomes.successes.push(success);
                }
                Ok((id, Err(failure))) => {
                    task_targets.remove(&id);
                    outcomes.failures.push(failure);
                }
                Err(error) => {
                    if let Some((machine_id, container_id)) = task_targets.remove(&error.id()) {
                        outcomes.failures.push(MachineFailure {
                            machine_id,
                            error: ContainerOperationFailure {
                                container_id,
                                error: RpcError {
                                    code: RpcErrorCode::Unavailable,
                                    message: error.to_string(),
                                    details: Value::Null,
                                    cause: Vec::new(),
                                },
                            },
                        });
                    }
                }
            }
        }
        outcomes
    }

    /// Gather an observer-relative Deploy Snapshot from the given Machines.
    /// Target-specific Container and Docker Volume failures and omissions stay
    /// on the snapshot so planning can derive observer-relative completeness.
    pub(crate) async fn deploy_snapshot(
        &self,
        machines: Vec<MachineObservation>,
    ) -> Result<DeploySnapshot, ConnectError> {
        let (live, volumes, capacity) = tokio::join!(
            self.live_services_from(&machines, EnvironmentValues::Included),
            self.list_volumes(&machines),
            capacity::observe(self, &machines),
        );
        snapshot_from_partial(machines, live?.containers, volumes, capacity)
            .map_err(ConnectError::Remote)
    }
}

/// A call the router or this client's own deadline shows went unanswered names the
/// Machine in the deepest line, which is the one progress shows. Every other error
/// keeps its own words.
pub(crate) fn answered_by(machine_id: &MachineId, error: ConnectError) -> RpcError {
    if !error.target_unanswered() {
        // Only a failure a retry may fix is marked unreached: a refused key or another
        // lasting failure is what the user acts on, so it keeps its own words.
        let (lost, unreached) = (
            error.unreplied(),
            error.unreached() && error.retry() != ployz_core::Retry::Never,
        );
        let error = rpc_error(error);
        return if lost {
            error.reply_lost_from(machine_id)
        } else if unreached {
            error.unreached()
        } else {
            error
        };
    }
    unanswered_by(machine_id, rpc_error(error))
}

/// `error`, from a call `machine` left unanswered, naming it in the deepest line.
pub(crate) fn unanswered_by(machine: impl std::fmt::Display, error: RpcError) -> RpcError {
    let transport = std::iter::once(error.message)
        .chain(error.cause)
        .collect::<Vec<_>>()
        .join(": ");
    RpcError::unanswered_target(
        error.code,
        format!("Machine {machine} did not answer: {transport}"),
    )
}

fn status_error(status: tonic::Status, path: &str, target: Option<&MachineTarget>) -> ConnectError {
    let error = TransportError::from(status);
    ConnectError::Rpc(match target {
        Some(target) => error.name_refusal(path, target),
        None => error,
    })
}

pub(crate) fn snapshot_from_partial(
    machines: Vec<MachineObservation>,
    containers: PartialResult<Vec<ContainerObservation>, RpcError>,
    volumes: PartialResult<VolumeInventory, RpcError>,
    capacity: BTreeMap<MachineId, BridgeEndpointCapacity>,
) -> Result<DeploySnapshot, RpcError> {
    let PartialResult {
        successes: container_successes,
        failures: container_failures,
        omissions: container_omissions,
    } = containers;
    Ok(DeploySnapshot {
        storage_capacity: Default::default(),
        machines,
        containers: container_successes
            .into_iter()
            .flat_map(|success| success.value)
            .collect(),
        volume_snapshot: VolumeSnapshot::from_partial(volumes)?,
        container_failures,
        container_omissions,
        capacity: Some(capacity),
    })
}

async fn remove_volumes_on(
    client: &Client,
    machines: &[MachineObservation],
    request: RemoveVolumesRequest,
) -> Vec<VolumeRemoval> {
    join_all(request.volumes.into_iter().map(|id| async move {
        let outcome = if machines
            .iter()
            .any(|machine| machine.machine.id == id.machine_id && machine.invites_rpc())
        {
            match client
                .invoke::<op::RemoveVolume>(
                    RemoveVolumeRequest {
                        name: id.name.clone(),
                        force: request.force,
                    },
                    &MachineTarget::from(&id.machine_id),
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await
            {
                Ok(_)
                | Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    ..
                }) => VolumeRemovalOutcome::Removed,
                Err(error) => VolumeRemovalOutcome::Failed { error },
            }
        } else {
            VolumeRemovalOutcome::Omitted
        };
        VolumeRemoval { id, outcome }
    }))
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeadConfirmed {
    Yes,
    No,
}

fn stopped_answering(machine: &Machine) -> RpcError {
    RpcError {
        code: RpcErrorCode::ConfirmationRequired,
        message: format!(
            "Server {} stopped answering after its name was typed. Forgetting it leaves any Volume copies on it behind, so it needs dead typed. No changes made.",
            machine.name
        ),
        details: json!({
            "server": { "id": machine.id, "name": machine.name },
            "answered": false,
        }),
        cause: Vec::new(),
    }
}

#[derive(Debug)]
pub(crate) enum HeldCopies {
    Dead,
    Answered(BTreeSet<DockerVolumeName>),
    /// The entry no longer lists the Server, as when Cloud's removal finished first.
    Gone,
}

/// Whether Cloud holds the last Machine. Cloud lets go of it by removing it itself:
/// the reset takes its keys, and Cloud forgets the pairing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudHold {
    None,
    Last,
}

/// Who removes a Machine. Only Cloud may remove the last Machine it holds: it sees the
/// reset, and only then lets go of the Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Remover {
    /// Cloud, through its own Management Client.
    Cloud,
    /// Anyone else: the CLI, whatever it connects through.
    Operator,
}

/// Forgetting `machine` leaves its copies of `volumes` behind: only its
/// reset demotes them.
pub(crate) fn copies_left_behind<'a>(
    machine: &MachineName,
    volumes: impl IntoIterator<Item = &'a str>,
) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "Server {machine} answers and holds copies of Volumes {}; forgetting it leaves them \
             behind. No changes made. Remove it with ployz server rm so its reset demotes them.",
            volumes.into_iter().collect::<Vec<_>>().join(", ")
        ),
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// The last Machine is Cloud's: anyone else removing it would leave Cloud paired
/// with a Cluster that no longer exists.
fn cloud_holds_last(machine: MachineId) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "Machine {machine} is the last Machine and Cloud manages it; only Cloud's removal \
             lets go of it too. No changes made."
        ),
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// Refuse removing the last Machine while a Management Client other than Cloud still
/// holds a key: that holder would be left managing a Cluster that no longer exists.
/// Cloud's own hold (`cloud` and the `cli-*` device keys it provisions) goes with the
/// Machine when Cloud removes it, so it only shows in the returned [`CloudHold`].
/// Holders that can't be read refuse too: they may be anyone's.
///
/// # Errors
/// Returns `conflict` naming the other holders, or `unavailable` when the last
/// Machine's holders can't be read.
pub(crate) async fn refuse_last_managed(
    client: &Client,
    machines: &[MachineObservation],
    selected: MachineId,
) -> Result<CloudHold, RpcError> {
    if machines.len() != 1 {
        return Ok(CloudHold::None);
    }
    let holders = client
        .invoke::<op::Inspect>(
            InspectRequest::default(),
            &MachineTarget::from(&selected),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| RpcError {
            code: RpcErrorCode::Unavailable,
            message: crate::ui::then_sentence(
                &format!(
                    "cannot read who manages Machine {selected}, the last Machine: {}",
                    crate::ui::row(&error)
                ),
                "No changes made.",
            ),
            details: Value::Null,
            cause: Vec::new(),
        })?
        .management_clients;
    let cloud = holders.iter().any(|label| label.as_str() == "cloud");
    let names: Vec<String> = holders
        .iter()
        .filter(|label| {
            !cloud || (label.as_str() != "cloud" && !label.as_str().starts_with("cli-"))
        })
        .map(|label| format!("`{label}`"))
        .collect();
    let who = match names.split_last() {
        None if cloud => return Ok(CloudHold::Last),
        None => return Ok(CloudHold::None),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    };
    Err(RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "this is the last Machine in the Cluster and it is still managed by {who}; \
             removing it would leave {who} managing a Cluster that no longer exists. \
             No changes made. Disconnect {who} from this Machine first."
        ),
        details: Value::Null,
        cause: Vec::new(),
    })
}

/// Resolve a Machine Target without hiding Name Ambiguity in the visible observations.
///
/// # Errors
/// Returns NotFound or Ambiguous when the target does not resolve to one Machine.
pub(crate) fn visible_machine<'list>(
    machine: &MachineTarget,
    machines: &'list [MachineObservation],
) -> Result<&'list MachineObservation, RpcError> {
    let selected = match machine.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => {
            return Err(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("Server {} was not found", machine.as_str().escape_debug()),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
        matches @ NameMatches::Ambiguous { .. } => {
            return Err(RpcError {
                code: RpcErrorCode::Ambiguous,
                message: format!(
                    "Server name {} is ambiguous: {}",
                    machine.as_str().escape_debug(),
                    matches
                        .iter()
                        .map(|row| row.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
        NameMatches::One(row) => row.id,
    };
    Ok(machines
        .iter()
        .find(|entry| entry.machine.id == selected)
        .expect("resolved Machine came from this list"))
}

/// The reset warning of a removal that found its Machine already gone: a missing row proves
/// Forget ran, not that the reset did.
const RESET_UNCONFIRMED: &str =
    "its reset couldn't be confirmed: it was already gone from the Cluster";

/// `machine`'s listed observation, or `None` when it is a Machine ID the list no longer
/// holds: an earlier removal already forgot it. A name that doesn't resolve stays NotFound.
///
/// # Errors
/// Returns NotFound or Ambiguous when the target does not resolve to one Machine.
fn listed_unless_forgotten<'list>(
    machine: &MachineTarget,
    machines: &'list [MachineObservation],
) -> Result<Option<&'list MachineObservation>, RpcError> {
    match visible_machine(machine, machines) {
        Ok(observation) => Ok(Some(observation)),
        Err(error)
            if error.code == RpcErrorCode::NotFound
                && MachineId::parse(machine.as_str()).is_ok() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A role setting a caller waits for this entry to observe.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RoleSetting {
    ServicesOff,
    IngressOn,
    IngressOff,
}

impl RoleSetting {
    fn role(self) -> &'static str {
        match self {
            Self::ServicesOff => "services",
            Self::IngressOn | Self::IngressOff => "ingress",
        }
    }

    fn holds(self, machine: &Machine) -> bool {
        match self {
            Self::ServicesOff => !machine.accepts_services,
            Self::IngressOn => machine.accepts_ingress,
            Self::IngressOff => !machine.accepts_ingress,
        }
    }
}

/// Why [`wait_for_role`] gave up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoleWaitError {
    #[error("this entry Server has not yet observed the new {} role", .0.role())]
    NotObserved(RoleSetting),
    #[error("this entry Server has not yet listed the change")]
    ChangeNotObserved,
    #[error("cancelled while waiting for the new role")]
    Cancelled,
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

/// Wait until this entry sees Machine `id` with `setting`, so what plans next plans from it.
///
/// # Errors
/// Fails when the entry hasn't observed it within 30 s plus one list call,
/// `cancellation` fires first, or listing Machines fails.
pub(crate) async fn wait_for_role(
    client: &mut Client,
    id: &MachineId,
    setting: RoleSetting,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    wait_until(
        client,
        id,
        |machine| setting.holds(machine),
        RoleWaitError::NotObserved(setting),
        cancellation,
    )
    .await
}

/// Whether this entry lists Machine `id` with `setting` now, in one list call.
///
/// # Errors
/// Fails when listing Machines fails.
pub(crate) async fn lists_role(
    client: &mut Client,
    id: &MachineId,
    setting: RoleSetting,
) -> Result<bool, RoleWaitError> {
    Ok(client
        .machines()
        .await?
        .iter()
        .any(|entry| entry.machine.id == *id && setting.holds(&entry.machine)))
}

/// The fields of a Machine update that a later plan reads: placement Labels and the roles.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlannedChange {
    labels: Vec<MachineLabelKey>,
    accepts_builds: bool,
    accepts_services: bool,
    accepts_ingress: bool,
}

impl PlannedChange {
    pub(crate) fn of(update: &MachineUpdate) -> Self {
        Self {
            labels: update.label_changes.keys().cloned().collect(),
            accepts_builds: update.accepts_builds.is_some(),
            accepts_services: update.accepts_services.is_some(),
            accepts_ingress: update.accepts_ingress.is_some(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && !self.accepts_builds
            && !self.accepts_services
            && !self.accepts_ingress
    }

    /// Whether `listed` carries every changed field as the target saved it.
    fn listed(&self, saved: &Machine, listed: &Machine) -> bool {
        self.labels
            .iter()
            .all(|key| saved.labels.get(key) == listed.labels.get(key))
            && (!self.accepts_builds || saved.accepts_builds == listed.accepts_builds)
            && (!self.accepts_services || saved.accepts_services == listed.accepts_services)
            && (!self.accepts_ingress || saved.accepts_ingress == listed.accepts_ingress)
    }
}

/// Wait until this entry lists `saved` with every field in `change`, in one poll.
///
/// # Errors
/// As [`wait_for_role`].
pub(crate) async fn wait_for_change(
    client: &mut Client,
    saved: &Machine,
    change: &PlannedChange,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    wait_until(
        client,
        &saved.id,
        |listed| change.listed(saved, listed),
        RoleWaitError::ChangeNotObserved,
        cancellation,
    )
    .await
}

async fn wait_until(
    client: &mut Client,
    id: &MachineId,
    holds: impl Fn(&Machine) -> bool,
    not_observed: RoleWaitError,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    // ponytail: 30 s deadline plus one ListMachines call, capped at 10 s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let machines = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RoleWaitError::Cancelled),
            machines = client.machines() => machines?,
        };
        if machines
            .iter()
            .any(|entry| entry.machine.id == *id && holds(&entry.machine))
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(not_observed);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RoleWaitError::Cancelled),
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

pub(crate) async fn evict_machine(
    client: &mut Client,
    observation: &MachineObservation,
    confirm_data_loss: &DataLossConfirmation,
    current: MachineId,
) -> Result<LocalMachineRemoved, RpcError> {
    let selected = observation.machine.id;
    data_loss_on_machine(client, observation)
        .await?
        .require(confirm_data_loss)
        .map_err(UnconfirmedDataLoss::into_rpc_error)?;
    depart(client, selected, current).await
}

/// Leave on `machine`, then Forget its row through the entry unless it is the entry. Both
/// are idempotent. A Leave the entry can't route finds no row: Forget already ran, and
/// whether the reset did is unknown, so the reply carries a warning saying so.
///
/// A Forget that fails after the target answered Left carries that reply as
/// [`LeftNotForgotten`]: a retry can no longer reach the target, and the reply is the only
/// proof that it reset.
async fn depart(
    client: &mut Client,
    machine: MachineId,
    current: MachineId,
) -> Result<LocalMachineRemoved, RpcError> {
    let (reset_warning, left) = match client
        .call::<op::Leave>(LeaveRequest {}, Some(&MachineTarget::from(&machine)))
        .await
    {
        Ok(Left::Left { reset_warning, .. }) => (reset_warning, true),
        Ok(Left::Refused { error, .. }) => return Err(error),
        Err(error) if error.is_unreachable() => (
            Some(format!(
                "target is unreachable; removing shared rows: {}",
                crate::ui::row(&error)
            )),
            false,
        ),
        Err(error) => match RpcError::from(error) {
            gone if gone.code == RpcErrorCode::NotFound => (Some(RESET_UNCONFIRMED.into()), false),
            error => return Err(error),
        },
    };
    if machine != current
        && let Err(error) = forget(client, machine).await
    {
        return Err(if left {
            LeftNotForgotten::carried_by(error, reset_warning)
        } else {
            error
        });
    }
    Ok(LocalMachineRemoved { reset_warning })
}

/// A Forget that failed after its target answered Left. The failed Forget's error carries
/// this in `details`, so a caller that retries with Forget alone keeps the reset's warning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LeftNotForgotten {
    left: bool,
    pub(crate) reset_warning: Option<String>,
}

impl LeftNotForgotten {
    fn carried_by(error: RpcError, reset_warning: Option<String>) -> RpcError {
        RpcError {
            details: serde_json::to_value(Self {
                left: true,
                reset_warning,
            })
            .expect("LeftNotForgotten is JSON"),
            ..error
        }
    }

    /// The Leave reply a failed removal carries, when it carries one.
    pub(crate) fn from_rpc_error(error: &RpcError) -> Option<Self> {
        Self::deserialize(&error.details)
            .ok()
            .filter(|carried| carried.left)
    }
}

/// Delete `machine_id`'s shared row through the entry Machine.
pub(crate) async fn forget(client: &mut Client, machine_id: MachineId) -> Result<(), RpcError> {
    match client
        .call::<op::Forget>(ForgetRequest { machine_id }, None)
        .await
        .map_err(RpcError::from)?
    {
        Forgotten::Forgotten => Ok(()),
        Forgotten::Refused { error, .. } => Err(error),
    }
}

async fn data_loss_on_machine(
    client: &Client,
    observation: &MachineObservation,
) -> Result<ObservedDataLoss, RpcError> {
    let selected = observation.machine.id;
    let name = &observation.machine.name;
    if !observation.invites_rpc() {
        return Err(server_did_not_respond(name));
    }
    let volumes = list_volumes_on_machine(client.clone(), selected)
        .await
        .map_err(|failure| RpcError {
            message: format!("Server {name}: {}", crate::ui::row(&failure.error)),
            ..failure.error
        })?;
    Ok(ObservedDataLoss {
        data_loss: volumes_lost(volumes.value),
    })
}

/// The Docker Volumes a reset loses: every one listed or failing to list, but the
/// builder's cache, which is no one's data.
fn volumes_lost(inventory: VolumeInventory) -> Vec<DataLoss> {
    inventory
        .volumes
        .into_iter()
        .map(|volume| volume.id)
        .chain(inventory.failures.into_iter().map(|failure| failure.id))
        .filter(|id| !ployz_build::is_builder_cache(id.name.as_str()))
        .map(|id| DataLoss::DockerVolume { id })
        .collect()
}

fn server_did_not_respond(name: &MachineName) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!("Server {name} did not respond"),
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// Machines whose storage observation failed (`failures`) or did not answer in
/// time (`omissions`).
#[derive(Debug, Default)]
pub(crate) struct StorageGaps {
    pub failures: Vec<MachineFailure<RpcError>>,
    pub omissions: Vec<MachineId>,
}

/// `None` when the Machine did not answer within the storage budget.
async fn observe_machine_storage(
    client: Client,
    index: usize,
    machine_id: MachineId,
) -> (
    usize,
    Option<Result<Option<MachineStorageObservation>, RpcError>>,
) {
    let target = MachineTarget::from(&machine_id);
    let observation = async {
        let description = client
            .invoke::<op::DescribeContract>(DescribeContractRequest {}, &target, None)
            .await?;
        if !description.supports(MACHINE_STORAGE_OBSERVATION_CAPABILITY) {
            return Ok(None);
        }
        Ok(client
            .invoke::<op::Inspect>(
                InspectRequest {
                    include_storage: true,
                    ..Default::default()
                },
                &target,
                None,
            )
            .await?
            .storage)
    };
    let storage = tokio::time::timeout(STORAGE_OBSERVATION_TIMEOUT, observation)
        .await
        .ok();
    (index, storage)
}

async fn list_volumes_on_machine(
    mut client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<VolumeInventory>, MachineFailure<RpcError>> {
    client
        .read::<op::ListVolumes>(ListVolumesRequest {}, &MachineTarget::from(&machine_id))
        .await
        .and_then(|inventory| validate_volume_inventory(machine_id, inventory))
        .map(|list| MachineSuccess {
            machine_id,
            value: list,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

fn validate_volume_inventory(
    machine_id: MachineId,
    inventory: VolumeInventory,
) -> Result<VolumeInventory, RpcError> {
    let mut names = BTreeSet::new();
    for id in inventory
        .volumes
        .iter()
        .map(|volume| &volume.id)
        .chain(inventory.failures.iter().map(|failure| &failure.id))
    {
        if id.machine_id != machine_id {
            return Err(RpcError {
                code: RpcErrorCode::Internal,
                message: format!(
                    "targeted ListVolumes response from Machine {machine_id} contained Docker Volume '{}' for Machine {}",
                    id.name, id.machine_id
                ),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
        if !names.insert(&id.name) {
            return Err(RpcError {
                code: RpcErrorCode::Internal,
                message: format!(
                    "targeted ListVolumes response from Machine {machine_id} repeated Docker Volume '{}'",
                    id.name
                ),
                details: Value::Null,
                cause: Vec::new(),
            });
        }
    }
    Ok(inventory)
}

async fn list_on_machine(
    mut client: Client,
    machine_id: MachineId,
    environment: EnvironmentValues,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    client
        .try_read::<op::ListContainers>(
            ListContainersRequest { environment },
            &MachineTarget::from(&machine_id),
        )
        .await
        .map(|list| MachineSuccess {
            machine_id,
            value: list.containers,
        })
        .map_err(|error| MachineFailure {
            machine_id,
            error: unanswered_kept(error),
        })
}

/// [`rpc_error`] in its own words, marked unanswered when the router or this client's
/// deadline shows the target never answered, and undialed as well when the router
/// couldn't dial it.
fn unanswered_kept(error: ConnectError) -> RpcError {
    if !error.target_unanswered() {
        return rpc_error(error);
    }
    let unreached = error.target_unreached();
    let error = rpc_error(error);
    let kept = RpcError {
        cause: error.cause,
        ..RpcError::unanswered_target(error.code, error.message)
    };
    match unreached {
        true => kept.undialed_target(),
        false => kept,
    }
}

async fn change_on_machine(
    client: Client,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    stop: StopOptions,
    queued: u32,
    cancellation: CancellationToken,
) -> Result<MachineSuccess<ContainerId>, MachineFailure<ContainerOperationFailure>> {
    match change_container_rpc(
        &client,
        &machine_id,
        &container_id,
        action,
        stop,
        queued,
        &cancellation,
    )
    .await
    {
        Ok(()) => Ok(MachineSuccess {
            machine_id,
            value: container_id,
        }),
        Err(error) => Err(MachineFailure {
            machine_id,
            error: ContainerOperationFailure {
                container_id,
                error,
            },
        }),
    }
}

async fn change_container_rpc(
    client: &Client,
    machine_id: &MachineId,
    container_id: &ContainerId,
    action: ContainerAction,
    stop: StopOptions,
    queued: u32,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    match action {
        ContainerAction::Start => cleanup::user_start(client, machine_id, container_id)
            .await
            .and_then(admitted),
        ContainerAction::Restart => {
            cleanup::user_restart(client, machine_id, container_id, stop, queued, cancellation)
                .await
                .and_then(admitted)
        }
        ContainerAction::Stop => {
            cleanup::user_stop(client, machine_id, container_id, stop, queued, cancellation).await
        }
        ContainerAction::Remove => {
            cleanup::user_remove(client, machine_id, container_id, cancellation).await
        }
    }
}

fn admitted(started: UserStarted) -> Result<(), RpcError> {
    match started {
        UserStarted::In | UserStarted::Running => Ok(()),
        UserStarted::NotIn(health) => Err(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "The Container started but did not pass its healthcheck, so proxies do not route to it.".into(),
            details: health
                .and_then(ployz_core::HealthVerdict::into_failure)
                .map_or(Value::Null, |failure| json!(failure)),
            cause: Vec::new(),
        }),
    }
}

#[cfg(test)]
#[path = "cluster_tests.rs"]
mod tests;

/// One Global revision per target; Namespace and kind are scoped by the SlotKey.
pub(crate) fn global_creation_key(spec: &ResolvedServiceSpec) -> String {
    format!(
        "global:{}:{}",
        spec.service_id,
        spec.serving_shape().token()
    )
}
