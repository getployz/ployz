use std::{collections::HashMap, sync::Arc, time::Duration};

use ployz_core::{
    ContainerAction, ContainerCreated, ContainerId, ContainerKind, ContainerObservation,
    CreateContainerRequest, DockerVolume, FanoutOutcome, FanoutResponse, FanoutSelector,
    GetDomainRequest, ListContainersRequest, ListImagesRequest, ListMachinesRequest,
    ListVolumesRequest, LiveServices, MachineFailure, MachineId, MachineImages, MachineName,
    MachineObservation, MachineRpcClient, MachineSuccess, MachineTarget, MembershipObservation,
    OpaquePayload, PartialResult, RemoveContainerRequest, ResolvedServiceSpec, Rpc, RpcError,
    RpcErrorCode, RpcResponseBody, StartContainerRequest, StopContainerRequest, apply_many_targets,
    derive_live_services, op,
};
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
    rpc: MachineRpcClient<Channel>,
    channel: Channel,
    connection: Connection,
    source: ConnectionSource,
    connector: Arc<dyn Connector>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
            rpc: MachineRpcClient::new(channel.clone()),
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
            let channel = self.connector.connect(&self.connection).await?;
            self.rpc = MachineRpcClient::new(channel.clone());
            self.channel = channel;
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

    pub(crate) async fn exec_stream(
        &self,
        target: &MachineTarget,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let mut rpc = self.rpc.clone();
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
        let mut rpc = self.rpc.clone();
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
        let mut rpc = self.rpc.clone();
        Ok(rpc
            .machine_logs(target_request(request, Some(target)))
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

    pub async fn list_volumes(
        &mut self,
        machines: &[MachineObservation],
    ) -> PartialResult<Vec<DockerVolume>, RpcError> {
        // UT-028: keep every target's success or typed failure instead of warning and omitting it.
        let mut requests = tokio::task::JoinSet::new();
        for (index, machine) in machines.iter().enumerate() {
            let machine_id = machine.machine.id;
            let mut client = self.clone();
            requests.spawn(async move {
                let target = MachineTarget::from(&machine_id);
                let outcome = client
                    .call::<op::ListVolumes>(ListVolumesRequest {}, Some(&target))
                    .await;
                (index, machine_id, outcome)
            });
        }
        let mut outcomes = Vec::with_capacity(machines.len());
        while let Some(outcome) = requests.join_next().await {
            outcomes.push(outcome.expect("Volume listing task does not panic"));
        }
        outcomes.sort_by_key(|(index, _, _)| *index);
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for (_, machine_id, outcome) in outcomes {
            match outcome {
                Ok(volumes) => result.successes.push(MachineSuccess {
                    machine_id,
                    value: volumes.volumes,
                }),
                Err(error) => result.failures.push(MachineFailure {
                    machine_id,
                    error: rpc_error(error),
                }),
            }
        }
        result
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
            match machine.membership {
                MembershipObservation::Up | MembershipObservation::Suspect => {
                    tasks.spawn(list_on_machine(self.clone(), machine.machine.id));
                }
                MembershipObservation::Down
                | MembershipObservation::Unknown
                | MembershipObservation::Unrecognized(_) => {
                    omissions.push(machine.machine.id);
                }
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
