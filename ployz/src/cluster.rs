use std::{collections::HashMap, sync::Arc, time::Duration};

use ployz_core::{
    ContainerAction, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    CreateContainerRequest, DataLoss, DescribeContractRequest, DockerVolume, DockerVolumeName,
    FanoutOutcome, FanoutResponse, FanoutSelector, GetDomainRequest, ListContainersRequest,
    ListImagesRequest, ListMachinesRequest, ListVolumesRequest, LiveServices, MachineFailure,
    MachineId, MachineImages, MachineName, MachineObservation, MachineRpcClient, MachineSuccess,
    MachineTarget, NameMatches, ObservedDataLoss, OpaquePayload, PartialResult,
    RemoveContainerRequest, RemoveVolumeRequest, RemoveVolumesRequest, ResolvedServiceSpec, Rpc,
    RpcError, RpcErrorCode, RpcResponseBody, StartContainerRequest, StopContainerRequest,
    apply_many_targets, derive_live_services, op,
};
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinSet;
use tonic::{Streaming, codec::ProstCodec, codegen::http::uri::PathAndQuery, transport::Channel};

use crate::{
    connect::{
        BoxProxyStream, ConnectError, Connector, TARGET_RPC_TIMEOUT, TransportError,
        UNARY_RETRY_DELAYS, apply_timeout, decode_fanout_failure, is_unary_retryable, rpc_error,
        stop_rpc_timeout, target_request,
    },
    context::{Connection, ConnectionSource},
    deploy::{DeploySnapshot, ObservedDockerVolume},
    service::ContainerOperationFailure,
};

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

pub(crate) struct DeploySnapshotGather {
    pub snapshot: DeploySnapshot,
    pub containers: PartialResult<Vec<ContainerObservation>, RpcError>,
    pub volumes: PartialResult<Vec<DockerVolume>, RpcError>,
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
    pub async fn call<T: Rpc>(
        &mut self,
        request: T::Request,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        let payload = T::into_request(request).encode()?;
        let mut delays = UNARY_RETRY_DELAYS.iter().copied();
        let mut redial = false;
        loop {
            match self
                .unary_attempt::<T>(payload.clone(), target, redial)
                .await
            {
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
        let mut rpc = self.machine_rpc();
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
    /// Down, Unknown, and Unrecognized are omitted. Up and Suspect get one
    /// `invoke` — `call` retries are for the entry connection, not peer fan-out.
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
    /// ambiguous, or when this observer cannot list Docker Volumes on that
    /// Machine.
    pub async fn data_loss_if_machine_removed(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<ObservedDataLoss, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
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
        let observation = machines
            .iter()
            .find(|entry| entry.machine.id == selected)
            .expect("resolved Machine came from this list");
        if !observation.membership.invites_rpc() {
            return Err(RpcError {
                code: RpcErrorCode::Unavailable,
                message: format!(
                    "Machine {selected} did not produce a Volume listing from this observer"
                ),
                details: Value::Null,
            });
        }
        let volumes = list_volumes_on_machine(self.clone(), selected)
            .await
            .map_err(|failure| failure.error)?;
        Ok(ObservedDataLoss {
            data_loss: volumes
                .value
                .into_iter()
                .map(|volume| DataLoss::DockerVolume(volume.id))
                .collect(),
        })
    }

    pub async fn list_images(
        &self,
        reference: Option<String>,
        targets: &[String],
    ) -> Result<PartialResult<MachineImagesObservation, RpcError>, ConnectError> {
        let mut request = tonic::Request::new(
            op::ListImages::into_request(ListImagesRequest { reference }).encode()?,
        );
        let selectors = if targets.is_empty() {
            vec![FanoutSelector::All]
        } else {
            targets
                .iter()
                .map(|target| FanoutSelector::parse(target.as_str()))
                .collect::<Result<Vec<_>, _>>()?
        };
        apply_many_targets(request.metadata_mut(), &selectors)?;
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready().await?;
        let response = grpc
            .server_streaming(
                request,
                PathAndQuery::from_static(op::ListImages::PATH),
                ProstCodec::<OpaquePayload, FanoutResponse>::default(),
            )
            .await?;
        let mut stream = response.into_inner();
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        while let Some(envelope) = stream.message().await? {
            let machine_id = envelope.machine_id()?;
            let machine_name = envelope.machine_name()?;
            match envelope.outcome {
                Some(FanoutOutcome::FramedPayload(frame)) => {
                    let response = OpaquePayload::decode_grpc_frame(&frame)?.decode_response()?;
                    if let RpcResponseBody::Error(error) = &response.body {
                        result.failures.push(MachineFailure {
                            machine_id,
                            error: error.clone(),
                        });
                    } else {
                        result.successes.push(MachineSuccess {
                            machine_id,
                            value: MachineImagesObservation {
                                machine_name,
                                images: response
                                    .decode::<op::ListImages>()
                                    .map_err(ConnectError::Codec)?,
                            },
                        });
                    }
                }
                Some(FanoutOutcome::Failure(failure)) => {
                    result.failures.push(MachineFailure {
                        machine_id,
                        error: decode_fanout_failure(failure),
                    });
                }
                None => result.omissions.push(machine_id),
            }
        }
        Ok(result)
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
        resolved_spec: ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        self.invoke::<op::CreateContainer>(
            CreateContainerRequest {
                kind,
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
    /// Container and volume fan-out failures stay in the returned Partial
    /// Results; the snapshot keeps successful observations.
    pub(crate) async fn deploy_snapshot(
        &mut self,
        machines: Vec<MachineObservation>,
    ) -> Result<DeploySnapshotGather, ConnectError> {
        let containers = self.live_services_from(&machines).await?.containers;
        let volumes = self.list_volumes(&machines).await;
        let snapshot = snapshot_from_partial(machines, &containers, &volumes);
        Ok(DeploySnapshotGather {
            snapshot,
            containers,
            volumes,
        })
    }
}

pub(crate) fn snapshot_from_partial(
    machines: Vec<MachineObservation>,
    containers: &PartialResult<Vec<ContainerObservation>, RpcError>,
    volumes: &PartialResult<Vec<DockerVolume>, RpcError>,
) -> DeploySnapshot {
    DeploySnapshot {
        machines,
        containers: containers
            .successes
            .iter()
            .flat_map(|success| success.value.iter().cloned())
            .collect(),
        volumes: volumes
            .successes
            .iter()
            .flat_map(|success| success.value.iter().cloned())
            .map(|volume| ObservedDockerVolume {
                id: volume.id,
                driver: volume.driver,
                options: volume.options,
            })
            .collect(),
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

async fn list_volumes_on_machine(
    client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<DockerVolume>>, MachineFailure<RpcError>> {
    client
        .invoke::<op::ListVolumes>(
            ListVolumesRequest {},
            &MachineTarget::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|list| MachineSuccess {
            machine_id,
            value: list.volumes,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

async fn list_on_machine(
    client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    client
        .invoke::<op::ListContainers>(
            ListContainersRequest {},
            &MachineTarget::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
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
