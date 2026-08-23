use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use ployz_core::{
    BridgeEndpointCapacity, ContainerAction, ContainerCreated, ContainerId, ContainerKind,
    ContainerObservation, CreateContainerRequest, DataLoss, DescribeContractRequest, DockerVolume,
    DockerVolumeName, GetDomainRequest, InspectRequest, ListContainersRequest, ListImagesRequest,
    ListMachinesRequest, ListVolumesRequest, LiveServices, LocalMachineRemoved,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineFailure, MachineId, MachineImages,
    MachineName, MachineObservation, MachineRpcClient, MachineStorageObservation, MachineSuccess,
    MachineTarget, NameMatches, ObservedDataLoss, OpaquePayload, PartialResult, ProjectName,
    RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, RemoveContainerRequest, RemoveLocalMachineRequest,
    RemoveMachineRequest, RemoveVolumeRequest, RemoveVolumesRequest, ResolvedServiceSpec, Rpc,
    RpcError, RpcErrorCode, RpcResponseBody, StartContainerRequest, StopContainerRequest,
    UnconfirmedDataLoss, derive_live_services, op,
};
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinSet;
use tonic::{Streaming, codec::ProstCodec, codegen::http::uri::PathAndQuery, transport::Channel};

use crate::{
    connect::{
        BoxProxyStream, ConnectError, Connector, TARGET_RPC_TIMEOUT, TransportError,
        UNARY_RETRY_DELAYS, apply_timeout, is_unary_retryable, rpc_error, stop_rpc_timeout,
        target_request,
    },
    context::{Connection, ConnectionSource, Transport},
    deploy::{DeploySnapshot, ObservedDockerVolume},
    service::ContainerOperationFailure,
};

mod capacity;

const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Client {
    channel: Channel,
    connection: Connection,
    source: ConnectionSource,
    connector: Arc<dyn Connector>,
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
            channel,
            connection,
            source,
            connector,
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

    /// One-shot check that this channel reaches a daemon. No unary retry — a
    /// down daemon must not stall the connection walk.
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error when the daemon does not answer
    /// within five seconds.
    pub(crate) async fn confirm_entry(&self) -> Result<(), ConnectError> {
        let confirm = async {
            let payload =
                op::DescribeContract::into_request(DescribeContractRequest {}).encode()?;
            match self.call_once::<op::DescribeContract>(payload, None).await {
                Ok(_) | Err(ConnectError::Remote(_)) => Ok(()),
                Err(error) => Err(error),
            }
        };
        tokio::time::timeout(Duration::from_secs(5), confirm)
            .await
            .map_err(|_| ConnectError::Attempt("entry Machine did not become ready".into()))?
    }

    /// Issue one unary RPC. The response type is derived from the RPC, so a request
    /// cannot be paired with the wrong response.
    ///
    /// Transient transport drops (`Attempt`, tonic `Unavailable` / `DeadlineExceeded`)
    /// redial the same connection and retry up to four times. Domain `Remote` errors
    /// and other gRPC status codes are not retried.
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
        let payload = T::into_request(request)
            .encode()
            .map_err(|error| rpc_error(ConnectError::Codec(error)))?;
        self.call_retried::<T>(payload, Some(target), Some(TARGET_RPC_TIMEOUT))
            .await
            .map_err(rpc_error)
    }

    async fn call_retried<T: Rpc>(
        &mut self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        timeout: Option<Duration>,
    ) -> Result<T::Response, ConnectError> {
        let mut delays = UNARY_RETRY_DELAYS.iter().copied();
        let mut redial = false;
        loop {
            let attempt = self.unary_attempt::<T>(payload.clone(), target, redial);
            let outcome = match timeout {
                Some(timeout) => match tokio::time::timeout(timeout, attempt).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        Err(tonic::Status::deadline_exceeded("target Machine RPC timed out").into())
                    }
                },
                None => attempt.await,
            };
            match outcome {
                Ok(response) => return Ok(response),
                Err(error) if is_unary_retryable(&error) => {
                    let Some(delay) = delays.next() else {
                        return Err(error);
                    };
                    tokio::time::sleep(delay).await;
                    redial = true;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn unary_attempt<T: Rpc>(
        &mut self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
        redial: bool,
    ) -> Result<T::Response, ConnectError> {
        if redial {
            self.channel = self.connector.connect(&self.connection).await?;
        }
        self.call_once::<T>(payload, target).await
    }

    /// One-shot targeted RPC. No retry — mutating operations must not
    /// re-issue CreateContainer or CreateVolume after a dropped response.
    pub(crate) async fn invoke<T: Rpc>(
        &self,
        request: T::Request,
        target: &MachineTarget,
        timeout: Option<Duration>,
    ) -> Result<T::Response, RpcError> {
        let payload = T::into_request(request)
            .encode()
            .map_err(|error| rpc_error(ConnectError::Codec(error)))?;
        apply_timeout(timeout, self.call_once::<T>(payload, Some(target))).await
    }

    fn machine_rpc(&self) -> MachineRpcClient<Channel> {
        MachineRpcClient::new(self.channel.clone())
    }

    pub(crate) async fn exec_stream(
        &self,
        target: &MachineTarget,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .exec(target_request(input, Some(target)))
            .await?
            .into_inner())
    }

    pub(crate) async fn container_logs_stream(
        &self,
        target: &MachineTarget,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .container_logs(target_request(request, Some(target)))
            .await?
            .into_inner())
    }

    pub(crate) async fn machine_logs_stream(
        &self,
        target: &MachineTarget,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.machine_rpc();
        Ok(rpc
            .machine_logs(target_request(request, Some(target)))
            .await?
            .into_inner())
    }

    pub(crate) async fn runtime_watch_stream(
        &self,
        request: OpaquePayload,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self
            .machine_rpc()
            .max_decoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT);
        Ok(rpc
            .runtime_watch(tonic::Request::new(request))
            .await?
            .into_inner())
    }

    async fn call_once<T: Rpc>(
        &self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready().await?;
        let response = grpc
            .unary(
                target_request(payload, target),
                PathAndQuery::from_static(T::PATH),
                ProstCodec::<OpaquePayload, OpaquePayload>::default(),
            )
            .await?
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
    /// UT-028: keep each probed target's success or typed failure.
    pub async fn list_volumes(
        &mut self,
        machines: &[MachineObservation],
    ) -> PartialResult<Vec<DockerVolume>, RpcError> {
        let mut requests = JoinSet::new();
        let mut omissions = Vec::new();
        for (index, machine) in machines.iter().enumerate() {
            if !machine.membership.invites_rpc() {
                omissions.push(machine.machine.id);
                continue;
            }
            let machine_id = machine.machine.id;
            let client = self.clone();
            requests
                .spawn(async move { (index, list_volumes_on_machine(client, machine_id).await) });
        }
        let mut outcomes = Vec::with_capacity(requests.len());
        while let Some(outcome) = requests.join_next().await {
            outcomes.push(outcome.expect("Volume listing task does not panic"));
        }
        outcomes.sort_by_key(|(index, _)| *index);
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions,
        };
        for (_, outcome) in outcomes {
            match outcome {
                Ok(success) => result.successes.push(success),
                Err(failure) => result.failures.push(failure),
            }
        }
        result
    }

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// Each volume is identified by Machine plus name. Fan-out is a Partial
    /// Result: destroyed names, per-Machine failures (including not-found),
    /// and omissions for Machines that do not invite RPC. Forced removal is
    /// off unless `force` is set.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn remove_volumes(
        &mut self,
        request: RemoveVolumesRequest,
    ) -> Result<PartialResult<DockerVolumeName, RpcError>, RpcError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(RpcError::from)?;
        Ok(remove_volumes_on(self, &machines.machines, request).await)
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

    /// Remove `machine` after a named Data Loss confirmation.
    ///
    /// Re-reads Data Loss at execute time. Fresh names the confirmation does
    /// not cover refuse the removal. Extra confirmed names are ignored.
    /// Resets the Machine. A reset warning is returned, not swallowed.
    /// The last Cloud-paired Machine is refused before reset or membership
    /// mutation; tear down that Cluster from Cloud.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible or is
    /// the current entry while another Machine is visible, when the Machine
    /// is the last Cloud-paired Machine, when the Machine did not respond so
    /// Data Loss cannot be listed, when the confirmation does not cover the
    /// fresh Data Loss, or when reset or shared-row removal fails.
    pub async fn remove_machine(
        &mut self,
        machine: &MachineTarget,
        confirm_data_loss: &[DataLoss],
    ) -> Result<LocalMachineRemoved, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let observation = visible_machine(machine, &machines)?;
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
            });
        }
        refuse_last_cloud_paired(self, &machines, selected).await?;
        evict_machine(self, observation, confirm_data_loss, current).await
    }

    /// Remove Cluster membership for `machine` without resetting it.
    ///
    /// The last Cloud-paired Machine is refused before membership mutation;
    /// tear down that Cluster from Cloud.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible, when
    /// it is the last Cloud-paired Machine, or when shared-row removal fails.
    pub async fn remove_machine_membership(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<(), RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let observation = visible_machine(machine, &machines)?;
        let selected = observation.machine.id;
        refuse_last_cloud_paired(self, &machines, selected).await?;
        self.call::<op::RemoveMachine>(
            RemoveMachineRequest {
                machine_id: selected,
            },
            None,
        )
        .await
        .map_err(RpcError::from)?;
        Ok(())
    }

    /// List images on each target Machine with independent bounded retries.
    pub async fn list_images(
        &self,
        reference: Option<String>,
        targets: &[Machine],
    ) -> PartialResult<MachineImagesObservation, RpcError> {
        let mut tasks = JoinSet::new();
        for (index, machine) in targets.iter().enumerate() {
            let mut client = self.clone();
            let machine_id = machine.id;
            let machine_name = machine.name.clone();
            let reference = reference.clone();
            tasks.spawn(async move {
                let response = client
                    .read::<op::ListImages>(
                        ListImagesRequest { reference },
                        &MachineTarget::from(&machine_id),
                    )
                    .await;
                (index, machine_id, machine_name, response)
            });
        }
        let mut outcomes = Vec::with_capacity(tasks.len());
        while let Some(outcome) = tasks.join_next().await {
            outcomes.push(outcome.expect("Image listing task does not panic"));
        }
        outcomes.sort_by_key(|(index, _, _, _)| *index);
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for (_, machine_id, machine_name, outcome) in outcomes {
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
    /// Returns a connection or remote RPC error from `ListMachines`.
    pub async fn machines(&mut self) -> Result<Vec<MachineObservation>, ConnectError> {
        self.call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map(|list| list.machines)
    }

    pub(crate) async fn observe_machine_storage(&self, machines: &mut [MachineObservation]) {
        let mut tasks = JoinSet::new();
        for (index, machine) in machines.iter().enumerate() {
            if machine.membership.invites_rpc() {
                tasks.spawn(observe_machine_storage(
                    self.clone(),
                    index,
                    machine.machine.id,
                ));
            }
        }
        while let Some(observed) = tasks.join_next().await {
            let (index, storage) = observed.expect("Machine storage observation does not panic");
            if let Some(machine) = machines.get_mut(index) {
                machine.storage = storage;
            }
        }
    }

    pub async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = self.machines().await?;
        self.live_services_from(&machines).await
    }

    pub(crate) async fn live_services_from(
        &self,
        machines: &[MachineObservation],
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let mut tasks = JoinSet::new();
        let mut omissions = Vec::new();
        for machine in machines {
            // TODO(UT-102): the entry Machine's observer-relative Membership Observation is the
            // current trust boundary; it can be stale and is not an authority or freshness proof.
            if machine.membership.invites_rpc() {
                tasks.spawn(list_on_machine(self.clone(), machine.machine.id));
            } else {
                omissions.push(machine.machine.id);
            }
        }
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions,
        };
        // TODO(UT-015, UT-017): retain target failures and omissions beside successful container
        // observations; an entry-relative answer remains partial evidence.
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(success)) => result.successes.push(success),
                Ok(Err(failure)) => result.failures.push(failure),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(derive_live_services(result))
    }

    pub async fn create_container(
        &self,
        machine_id: MachineId,
        kind: ContainerKind,
        project_name: ProjectName,
        resolved_spec: ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        self.invoke::<op::CreateContainer>(
            CreateContainerRequest {
                kind,
                project_name,
                resolved_spec,
            },
            &MachineTarget::from(&machine_id),
            None,
        )
        .await
    }

    pub async fn change_observed_service(
        &self,
        service: &ployz_core::ServiceObservation,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> PartialResult<ContainerId, ContainerOperationFailure> {
        let mut tasks = JoinSet::new();
        let mut task_targets = HashMap::new();
        for container in service.containers_for(action) {
            let observation = container.as_observation();
            let machine_id = observation.machine_id;
            let container_id = observation.container_id;
            let handle = tasks.spawn(change_on_machine(
                self.clone(),
                machine_id,
                container_id,
                action,
                signal.clone(),
                grace_period_seconds,
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
                                },
                            },
                        });
                    }
                }
            }
        }
        outcomes
    }

    pub async fn domain_if_reserved(&mut self) -> Result<Option<String>, ConnectError> {
        match self.call::<op::GetDomain>(GetDomainRequest {}, None).await {
            Ok(domain) => Ok(Some(domain.name)),
            Err(ConnectError::Remote(error)) if error.code == RpcErrorCode::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Gather an observer-relative Deploy Snapshot from the given Machines.
    /// Target-specific Container and Docker Volume failures and omissions stay
    /// on the snapshot so planning can derive observer-relative completeness.
    pub(crate) async fn deploy_snapshot(
        &mut self,
        machines: Vec<MachineObservation>,
    ) -> Result<DeploySnapshot, ConnectError> {
        let containers = self.live_services_from(&machines).await?.containers;
        let volumes = self.list_volumes(&machines).await;
        let capacity = capacity::observe(self, &machines).await;
        Ok(snapshot_from_partial(
            machines, containers, volumes, capacity,
        ))
    }
}

pub(crate) fn snapshot_from_partial(
    machines: Vec<MachineObservation>,
    containers: PartialResult<Vec<ContainerObservation>, RpcError>,
    volumes: PartialResult<Vec<DockerVolume>, RpcError>,
    capacity: BTreeMap<MachineId, BridgeEndpointCapacity>,
) -> DeploySnapshot {
    let PartialResult {
        successes: container_successes,
        failures: container_failures,
        omissions: container_omissions,
    } = containers;
    let PartialResult {
        successes: volume_successes,
        failures: volume_failures,
        omissions: volume_omissions,
    } = volumes;
    DeploySnapshot {
        machines,
        containers: container_successes
            .into_iter()
            .flat_map(|success| success.value)
            .collect(),
        volumes: volume_successes
            .into_iter()
            .flat_map(|success| success.value)
            .map(|volume| ObservedDockerVolume {
                id: volume.id,
                driver: volume.driver,
                options: volume.options,
                labels: volume.labels,
            })
            .collect(),
        container_failures,
        container_omissions,
        volume_failures,
        volume_omissions,
        capacity: Some(capacity),
    }
}

async fn remove_volumes_on(
    client: &Client,
    machines: &[MachineObservation],
    request: RemoveVolumesRequest,
) -> PartialResult<DockerVolumeName, RpcError> {
    let mut removals = JoinSet::new();
    let mut omissions = Vec::new();
    for (index, volume) in request.volumes.iter().enumerate() {
        if !machines.iter().any(|machine| {
            machine.machine.id == volume.machine_id && machine.membership.invites_rpc()
        }) {
            if !omissions.contains(&volume.machine_id) {
                omissions.push(volume.machine_id);
            }
            continue;
        }
        let client = client.clone();
        let id = volume.clone();
        let force = request.force;
        removals.spawn(async move {
            let outcome = client
                .invoke::<op::RemoveVolume>(
                    RemoveVolumeRequest {
                        name: id.name.clone(),
                        force,
                    },
                    &MachineTarget::from(&id.machine_id),
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await;
            (index, id, outcome)
        });
    }
    let mut outcomes = Vec::with_capacity(removals.len());
    while let Some(outcome) = removals.join_next().await {
        outcomes.push(outcome.expect("Volume removal task does not panic"));
    }
    outcomes.sort_by_key(|(index, _, _)| *index);
    let mut result = PartialResult {
        successes: Vec::new(),
        failures: Vec::new(),
        omissions,
    };
    for (_, id, outcome) in outcomes {
        match outcome {
            Ok(_) => result.successes.push(MachineSuccess {
                machine_id: id.machine_id,
                value: id.name,
            }),
            Err(error) => result.failures.push(MachineFailure {
                machine_id: id.machine_id,
                error,
            }),
        }
    }
    result
}

async fn refuse_last_cloud_paired(
    client: &Client,
    machines: &[MachineObservation],
    selected: MachineId,
) -> Result<(), RpcError> {
    if machines.len() != 1 {
        return Ok(());
    }
    let paired = match client.connection.transport() {
        Transport::Relay { .. } => true,
        Transport::Ssh { .. } | Transport::Tcp(_) | Transport::Unix(_) => {
            // Inspect errors must not block unpaired last-Machine removal.
            client
                .invoke::<op::Inspect>(
                    InspectRequest::default(),
                    &MachineTarget::from(&selected),
                    Some(TARGET_RPC_TIMEOUT),
                )
                .await
                .map(|details| details.cloud_paired)
                .unwrap_or(false)
        }
    };
    if !paired {
        return Ok(());
    }
    Err(RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: "the last Cloud-paired Machine cannot be removed with machine rm; tear down this Cluster from Cloud".into(),
        details: Value::Null,
    })
}

fn visible_machine<'list>(
    machine: &MachineTarget,
    machines: &'list [MachineObservation],
) -> Result<&'list MachineObservation, RpcError> {
    let selected = match machine.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => {
            return Err(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("Machine {machine:?} was not found"),
                details: Value::Null,
            });
        }
        NameMatches::Ambiguous(matches) => {
            return Err(RpcError {
                code: RpcErrorCode::Ambiguous,
                message: format!(
                    "Machine name {machine:?} is ambiguous: {}",
                    matches
                        .into_iter()
                        .map(|row| row.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                details: Value::Null,
            });
        }
        NameMatches::One(row) => row.id,
    };
    Ok(machines
        .iter()
        .find(|entry| entry.machine.id == selected)
        .expect("resolved Machine came from this list"))
}

pub(crate) async fn evict_machine(
    client: &mut Client,
    observation: &MachineObservation,
    confirm_data_loss: &[DataLoss],
    current: MachineId,
) -> Result<LocalMachineRemoved, RpcError> {
    let selected = observation.machine.id;
    let missing = data_loss_on_machine(client, observation)
        .await?
        .uncovered_by(confirm_data_loss);
    if !missing.is_empty() {
        return Err(UnconfirmedDataLoss { missing }.into_rpc_error());
    }
    let selected_target = MachineTarget::from(&selected);
    let reset_warning = match client
        .call::<op::RemoveLocalMachine>(
            RemoveLocalMachineRequest {
                restart_on_cleanup_failure: selected != current,
            },
            Some(&selected_target),
        )
        .await
    {
        Ok(removed) => removed.reset_warning,
        Err(error) if error.is_unreachable() => Some(format!(
            "target is unreachable; removing shared rows: {error}"
        )),
        Err(error) => return Err(error.into()),
    };
    if reset_warning.is_some() || selected != current {
        client
            .call::<op::RemoveMachine>(
                RemoveMachineRequest {
                    machine_id: selected,
                },
                None,
            )
            .await
            .map_err(RpcError::from)?;
    }
    Ok(LocalMachineRemoved { reset_warning })
}

async fn data_loss_on_machine(
    client: &Client,
    observation: &MachineObservation,
) -> Result<ObservedDataLoss, RpcError> {
    let selected = observation.machine.id;
    if !observation.membership.invites_rpc() {
        return Err(machine_did_not_respond(selected));
    }
    let volumes = list_volumes_on_machine(client.clone(), selected)
        .await
        .map_err(|failure| {
            if failure.error.code == RpcErrorCode::Unavailable {
                machine_did_not_respond(selected)
            } else {
                failure.error
            }
        })?;
    Ok(ObservedDataLoss {
        data_loss: volumes
            .value
            .into_iter()
            .map(|volume| DataLoss::DockerVolume(volume.id))
            .collect(),
    })
}

fn machine_did_not_respond(machine_id: MachineId) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!("Machine {machine_id} did not respond"),
        details: Value::Null,
    }
}

async fn observe_machine_storage(
    client: Client,
    index: usize,
    machine_id: MachineId,
) -> (usize, Option<MachineStorageObservation>) {
    let target = MachineTarget::from(&machine_id);
    let observation = async {
        let description = client
            .invoke::<op::DescribeContract>(DescribeContractRequest {}, &target, None)
            .await
            .ok()?;
        if !description.supports(MACHINE_STORAGE_OBSERVATION_CAPABILITY) {
            return None;
        }
        client
            .invoke::<op::Inspect>(
                InspectRequest {
                    include_storage: true,
                    ..Default::default()
                },
                &target,
                None,
            )
            .await
            .ok()?
            .storage
    };
    let storage = tokio::time::timeout(STORAGE_OBSERVATION_TIMEOUT, observation)
        .await
        .ok()
        .flatten();
    (index, storage)
}

async fn list_volumes_on_machine(
    mut client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<DockerVolume>>, MachineFailure<RpcError>> {
    client
        .read::<op::ListVolumes>(ListVolumesRequest {}, &MachineTarget::from(&machine_id))
        .await
        .map(|list| MachineSuccess {
            machine_id,
            value: list.volumes,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

async fn list_on_machine(
    mut client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    client
        .read::<op::ListContainers>(ListContainersRequest {}, &MachineTarget::from(&machine_id))
        .await
        .map(|list| MachineSuccess {
            machine_id,
            value: list.containers,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

async fn change_on_machine(
    client: Client,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<MachineSuccess<ContainerId>, MachineFailure<ContainerOperationFailure>> {
    match change_container_rpc(
        &client,
        &machine_id,
        &container_id,
        action,
        signal,
        grace_period_seconds,
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
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<(), RpcError> {
    let target = MachineTarget::from(machine_id);
    if matches!(action, ContainerAction::Stop | ContainerAction::Remove) {
        accept_stop_result(
            action,
            client
                .invoke::<op::StopContainer>(
                    StopContainerRequest {
                        container_id: *container_id,
                        signal,
                        grace_period_seconds,
                    },
                    &target,
                    stop_rpc_timeout(grace_period_seconds),
                )
                .await
                .map(|_| ()),
        )?;
    }
    match action {
        ContainerAction::Start => client
            .invoke::<op::StartContainer>(
                StartContainerRequest {
                    container_id: *container_id,
                },
                &target,
                Some(TARGET_RPC_TIMEOUT),
            )
            .await
            .map(|_| ()),
        ContainerAction::Stop => Ok(()),
        ContainerAction::Remove => remove_container_rpc(client, machine_id, container_id).await,
    }
}

async fn remove_container_rpc(
    client: &Client,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<(), RpcError> {
    client
        .invoke::<op::RemoveContainer>(
            RemoveContainerRequest {
                container_id: *container_id,
                remove_volumes: true,
                force: false,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
}

fn accept_stop_result(
    action: ContainerAction,
    result: Result<(), RpcError>,
) -> Result<(), RpcError> {
    match result {
        Err(error) if action == ContainerAction::Remove && error.code == RpcErrorCode::NotFound => {
            Ok(())
        }
        result => result,
    }
}

#[cfg(test)]
#[path = "cluster_tests.rs"]
mod tests;
