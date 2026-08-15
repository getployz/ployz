use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use ployz_core::{
    ContainerAction, ContainerCreated, ContainerId, ContainerKind, ContainerLogsRequest,
    ContainerObservation, CreateContainerRequest, CreateDomainRecordsRequest, DnsRecordRequest,
    DockerVolume, ExecConfig, ExecOptions, ExecRequestFrame, FanoutOutcome, FanoutResponse,
    GetDomainRequest, InspectContainerRequest, ListContainersRequest, ListImagesRequest,
    ListMachinesRequest, ListVolumesRequest, LiveServices, LogsOptions, MachineFailure, MachineId,
    MachineImages, MachineLogService, MachineLogsRequest, MachineName, MachineObservation,
    MachineRpcClient, MachineSelector, MachineSuccess, MembershipObservation, OpaquePayload,
    PartialResult, ReleaseDomainRequest, RemoveContainerRequest, ReserveDomainRequest,
    ResolvedServiceSpec, Rpc, RpcError, RpcErrorCode, RpcResponseBody, StartContainerRequest,
    StopContainerRequest, apply_many_targets, derive_live_services, op, select_service,
};
use serde_json::Value;
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tonic::{Streaming, codec::ProstCodec, codegen::http::uri::PathAndQuery, transport::Channel};

use crate::{
    connect::{
        BoxProxyStream, ConnectError, Connector, TARGET_RPC_TIMEOUT, TransportError,
        UNARY_RETRY_DELAYS, apply_timeout, decode_fanout_failure, is_unary_retryable, rpc_error,
        target_request,
    },
    context::{Connection, ConnectionSource},
    deploy::{DeploySnapshot, ObservedDockerVolume},
    operator::{
        ExecSession, LogInput, OperatorError, ServiceArg, ServiceLogInputs, open_log_input,
        select_exec_container, select_log_containers, select_machines, stream_input,
    },
    service::{ContainerOperationFailure, LifecycleResult, ServiceClientError},
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
        target: Option<&MachineSelector>,
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
        target: Option<&MachineSelector>,
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
        target: &MachineSelector,
        timeout: Option<Duration>,
    ) -> Result<T::Response, RpcError> {
        let payload = T::into_request(request)
            .encode()
            .map_err(|error| rpc_error(ConnectError::Codec(error)))?;
        apply_timeout(timeout, self.call_once::<T>(payload, Some(target))).await
    }

    pub(crate) async fn exec_stream(
        &self,
        target: &MachineSelector,
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
        target: &MachineSelector,
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
        target: &MachineSelector,
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
        target: Option<&MachineSelector>,
    ) -> Result<T::Response, ConnectError> {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|error| ConnectError::Attempt(error.to_string()))?;
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
            let machine_id = machine.machine.id.clone();
            let mut client = self.clone();
            requests.spawn(async move {
                let target = MachineSelector::from(&machine_id);
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
            vec![MachineSelector::parse("*").expect("wildcard selector is valid")]
        } else {
            targets
                .iter()
                .map(MachineSelector::parse)
                .collect::<Result<Vec<_>, _>>()?
        };
        apply_many_targets(request.metadata_mut(), &selectors)
            .map_err(|error| ConnectError::Attempt(error.to_string()))?;
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        grpc.ready()
            .await
            .map_err(|error| ConnectError::Attempt(error.to_string()))?;
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

    pub async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await?;
        self.live_services_from(&machines.machines).await
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
                    tasks.spawn(list_on_machine(self.clone(), machine.machine.id.clone()));
                }
                MembershipObservation::Down
                | MembershipObservation::Unknown
                | MembershipObservation::Unrecognized(_) => {
                    omissions.push(machine.machine.id.clone());
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
                Err(error) => return Err(ConnectError::Attempt(error.to_string())),
            }
        }
        Ok(derive_live_services(result))
    }

    pub async fn inspect_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
    ) -> Result<ployz_core::ContainerObservation, RpcError> {
        self.invoke::<op::InspectContainer>(
            InspectContainerRequest { container_id },
            &MachineSelector::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|details| details.container)
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
            &MachineSelector::from(&machine_id),
            None,
        )
        .await
    }

    pub async fn change_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        change_container_rpc(
            self,
            &machine_id,
            &container_id,
            action,
            signal,
            grace_period_seconds,
        )
        .await
    }

    pub(crate) async fn remove_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
    ) -> Result<(), RpcError> {
        remove_container_rpc(self, &machine_id, &container_id).await
    }

    pub async fn change_service(
        &mut self,
        selector: &str,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> Result<LifecycleResult, ServiceClientError> {
        let live = self.live_services().await?;
        let outcomes = self
            .change_observed_service(
                select_service(&live.services, selector)?,
                action,
                signal,
                grace_period_seconds,
            )
            .await;
        Ok(LifecycleResult {
            observations: live.containers,
            outcomes,
        })
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
            let machine_id = container.machine_id.clone();
            let container_id = container.container_id.clone();
            let handle = tasks.spawn(change_on_machine(
                self.clone(),
                machine_id.clone(),
                container_id.clone(),
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

    pub async fn open_exec(
        &mut self,
        service_selector: &str,
        container_selector: Option<&str>,
        options: ExecOptions,
    ) -> Result<ExecSession, OperatorError> {
        let live = self
            .live_services()
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let service = select_service(&live.services, service_selector)
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let container = select_exec_container(service, container_selector)?;
        let machine_id = container.machine_id.clone();
        let config = ExecRequestFrame::Config(ExecConfig {
            container_id: container.container_id.clone(),
            options,
        })
        .encode()?;
        let (sender, receiver) = mpsc::channel(32);
        sender
            .send(config)
            .await
            .map_err(|_| OperatorError::Message("exec request stream closed".into()))?;
        let output = self
            .exec_stream(
                &MachineSelector::from(&machine_id),
                tokio_stream::wrappers::ReceiverStream::new(receiver),
            )
            .await?;
        Ok(ExecSession {
            input: sender,
            output,
        })
    }

    pub async fn open_service_logs(
        &mut self,
        args: &[ServiceArg],
        machine_selectors: &[String],
        options: LogsOptions,
        compose_selection: bool,
        cancellation: CancellationToken,
    ) -> Result<ServiceLogInputs, OperatorError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?
            .machines;
        let selected_machines = select_machines(&machines, machine_selectors)?;
        let machine_ids = selected_machines
            .iter()
            .map(|machine| machine.machine.id.clone())
            .collect::<HashSet<_>>();
        let live = self
            .live_services()
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?;
        let mut inputs = Vec::new();
        let mut skipped_services = Vec::new();
        for arg in args {
            let service = match select_service(&live.services, &arg.service) {
                Ok(service) => service,
                Err(ployz_core::ServiceSelectorError::NotFound { .. }) if compose_selection => {
                    skipped_services.push(arg.service.clone());
                    continue;
                }
                Err(error) => return Err(error.to_string().into()),
            };
            let containers = select_log_containers(service, &arg.containers)?;
            let containers = containers
                .into_iter()
                .filter(|container| machine_ids.contains(&container.machine_id))
                .collect::<Vec<_>>();
            if containers.is_empty() {
                return Err(format!(
                    "no containers for Service {:?} found on the selected Machines",
                    arg.service
                )
                .into());
            }
            for container in containers {
                let request = op::ContainerLogs::into_request(ContainerLogsRequest {
                    container_id: container.container_id.clone(),
                    options: options.clone(),
                })
                .encode()
                .map_err(|error| OperatorError::Message(error.to_string()))?;
                let identity = format!("{}/{}", arg.service, container.display_name);
                let target = MachineSelector::from(&container.machine_id);
                // TODO(UT-082): earlier Container log streams intentionally survive until the
                // parent cancellation token is cancelled.
                if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                    self.container_logs_stream(&target, request)
                        .await
                        .map(|stream| stream_input(identity, stream))
                })
                .await
                {
                    return Err(format!(
                        "open logs for Container {} on Machine {}: {error}",
                        container.container_id, container.machine_id
                    )
                    .into());
                }
            }
        }
        if inputs.is_empty() {
            return Err("none of the selected Services exist in the Cluster".into());
        }
        Ok(ServiceLogInputs {
            inputs,
            skipped_services,
        })
    }

    pub async fn open_machine_logs(
        &mut self,
        services: &[String],
        machine_selectors: &[String],
        options: LogsOptions,
        cancellation: CancellationToken,
    ) -> Result<Vec<LogInput>, OperatorError> {
        let services = if services.is_empty() {
            vec![MachineLogService::Ployz]
        } else {
            services
                .iter()
                .map(|service| {
                    service.parse::<MachineLogService>().map_err(|expected| {
                        OperatorError::Message(format!(
                            "unsupported Machine log service {service:?}; {expected}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await
            .map_err(|error| OperatorError::Message(error.to_string()))?
            .machines;
        let machines = select_machines(&machines, machine_selectors)?;
        let mut inputs = Vec::new();
        for service in services {
            for machine in &machines {
                let request = op::MachineLogs::into_request(MachineLogsRequest {
                    service,
                    options: options.clone(),
                })
                .encode()
                .map_err(|error| OperatorError::Message(error.to_string()))?;
                let identity = format!("{service}@{}", machine.machine.name);
                let target = MachineSelector::from(&machine.machine.id);
                // TODO(UT-083): earlier Machine log streams intentionally survive until the
                // parent cancellation token is cancelled.
                if let Err(error) = open_log_input(&mut inputs, &cancellation, async {
                    self.machine_logs_stream(&target, request)
                        .await
                        .map(|stream| stream_input(identity, stream))
                })
                .await
                {
                    return Err(format!(
                        "open {service} logs on Machine {}: {error}",
                        machine.machine.name
                    )
                    .into());
                }
            }
        }
        Ok(inputs)
    }

    pub async fn reserve_domain(&mut self, endpoint: String) -> Result<String, ConnectError> {
        self.call::<op::ReserveDomain>(ReserveDomainRequest { endpoint }, None)
            .await
            .map(|domain| domain.name)
    }

    pub async fn domain(&mut self) -> Result<String, ConnectError> {
        self.call::<op::GetDomain>(GetDomainRequest {}, None)
            .await
            .map(|domain| domain.name)
    }

    pub async fn domain_if_reserved(&mut self) -> Result<Option<String>, ConnectError> {
        match self.domain().await {
            Ok(domain) => Ok(Some(domain)),
            Err(ConnectError::Remote(error)) if error.code == RpcErrorCode::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn release_domain(&mut self) -> Result<String, ConnectError> {
        self.call::<op::ReleaseDomain>(ReleaseDomainRequest {}, None)
            .await
            .map(|domain| domain.name)
    }

    pub(crate) async fn create_domain_records(
        &mut self,
        records: Vec<DnsRecordRequest>,
    ) -> Result<(), ConnectError> {
        self.call::<op::CreateDomainRecords>(CreateDomainRecordsRequest { records }, None)
            .await
            .map(drop)
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
            &MachineSelector::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|list| MachineSuccess {
            machine_id: machine_id.clone(),
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
    let target = MachineSelector::from(machine_id);
    if matches!(action, ContainerAction::Stop | ContainerAction::Remove) {
        accept_stop_result(
            action,
            client
                .invoke::<op::StopContainer>(
                    StopContainerRequest {
                        container_id: container_id.clone(),
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
                    container_id: container_id.clone(),
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
                container_id: container_id.clone(),
                remove_volumes: true,
                force: false,
            },
            &MachineSelector::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
}

fn stop_rpc_timeout(grace_period_seconds: Option<i32>) -> Option<Duration> {
    match grace_period_seconds {
        Some(seconds) if seconds < 0 => None,
        Some(seconds) => Some(TARGET_RPC_TIMEOUT + Duration::from_secs(seconds as u64)),
        None => None,
    }
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
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn unspecified_or_negative_stop_timeout_has_no_rpc_deadline() {
        assert_eq!(stop_rpc_timeout(Some(-1)), None);
        assert_eq!(stop_rpc_timeout(None), None);
        assert_eq!(
            stop_rpc_timeout(Some(5)),
            Some(TARGET_RPC_TIMEOUT + Duration::from_secs(5))
        );
    }

    #[test]
    fn remove_tolerates_a_missing_preliminary_stop_target() {
        let missing = RpcError {
            code: RpcErrorCode::NotFound,
            message: "gone".into(),
            details: Value::Null,
        };

        assert!(accept_stop_result(ContainerAction::Remove, Err(missing.clone())).is_ok());
        assert_eq!(
            accept_stop_result(ContainerAction::Stop, Err(missing.clone()))
                .unwrap_err()
                .code,
            RpcErrorCode::NotFound
        );
        assert_eq!(
            accept_stop_result(
                ContainerAction::Remove,
                Err(RpcError {
                    code: RpcErrorCode::Internal,
                    ..missing
                })
            )
            .unwrap_err()
            .code,
            RpcErrorCode::Internal
        );
    }
}
