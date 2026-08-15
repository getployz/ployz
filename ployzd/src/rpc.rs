use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ployz_core::{
    CONTAINER_LOGS_CAPABILITY, CREATE_CONTAINER_CAPABILITY, CREATE_DOMAIN_RECORDS_CAPABILITY,
    CREATE_VOLUME_CAPABILITY, CaddyConfig, CapabilityName, ContainerChanged, ContainerDetails,
    ContainerList, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, Domain, DomainRecords,
    EXEC_CONTAINER_CAPABILITY, GET_CADDY_CONFIG_CAPABILITY, GET_DOMAIN_CAPABILITY,
    INITIALIZE_MACHINE_CAPABILITY, INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY,
    INSPECT_VOLUME_CAPABILITY, INSPECT_WIREGUARD_CAPABILITY, Initialized, JOIN_MACHINE_CAPABILITY,
    JoinAccepted, LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY, LIST_MACHINES_CAPABILITY,
    LIST_VOLUMES_CAPABILITY, LocalMachinePhase, LocalMachineRemoved, LogMetadata, LogOrigin,
    MACHINE_LOGS_CAPABILITY, MACHINE_TOKEN_CAPABILITY, Machine, MachineDetails, MachineId,
    MachineIdentity, MachineList, MachineLogService, MachineRemoved, MachineRpc, MachineToken,
    MachineUpdated, OpaquePayload, PROTOCOL_MAJOR, PublicIpDiscovery, REGISTER_MACHINE_CAPABILITY,
    RELEASE_DOMAIN_CAPABILITY, REMOVE_CONTAINER_CAPABILITY, REMOVE_LOCAL_MACHINE_CAPABILITY,
    REMOVE_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY, RESERVE_DOMAIN_CAPABILITY,
    RESET_MACHINE_CAPABILITY, Registered, ResetAccepted, Rpc, RpcError, RpcErrorCode,
    RpcRequestBody, RpcResponse, RttObservation, START_CONTAINER_CAPABILITY,
    STOP_CONTAINER_CAPABILITY, UPDATE_MACHINE_CAPABILITY, VolumeCreated, VolumeDetails, VolumeList,
    VolumeRemoved, WireGuardInspected, associate_wireguard_peers, op, synthesize_membership,
};
use serde_json::Value;
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, Error as DockerError},
    logs::{RpcStream, open_journal_logs, serve_logs},
    machine::local_runtime,
    machine::{LocalMachineStore, StoreError},
    network::{
        allocate_machine_subnet, discover_network, inspect_wireguard_device, machine_gateway,
        management_address,
    },
};

#[derive(Clone)]
pub struct MachineService {
    store: Arc<Mutex<LocalMachineStore>>,
    restart: watch::Sender<bool>,
    hosted_dns: crate::hosted_dns::HostedDns,
    cluster: Option<ClusterContext>,
    containers: Option<ContainerRuntime>,
    caddyfile: Option<PathBuf>,
}

#[derive(Clone)]
struct ClusterContext {
    replicated: ReplicatedStore,
    admin: AdminClient,
}

impl MachineService {
    #[must_use]
    pub fn new(store: Arc<Mutex<LocalMachineStore>>, restart: watch::Sender<bool>) -> Self {
        Self::with_cluster(store, restart, None)
    }

    #[must_use]
    pub fn with_cluster(
        store: Arc<Mutex<LocalMachineStore>>,
        restart: watch::Sender<bool>,
        cluster: Option<(ReplicatedStore, AdminClient)>,
    ) -> Self {
        Self {
            store,
            restart,
            hosted_dns: crate::hosted_dns::HostedDns::new(),
            cluster: cluster.map(|(replicated, admin)| ClusterContext { replicated, admin }),
            containers: None,
            caddyfile: None,
        }
    }

    #[must_use]
    pub fn with_containers(self, containers: ContainerRuntime) -> Self {
        self.with_optional_containers(Some(containers))
    }

    #[must_use]
    pub fn with_optional_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.containers = containers;
        self
    }

    #[must_use]
    pub fn with_caddyfile(mut self, path: PathBuf) -> Self {
        self.caddyfile = Some(path);
        self
    }

    #[allow(clippy::result_large_err)]
    fn local_record(&self) -> Result<crate::machine::LocalMachineRecord, Status> {
        self.store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))
            .map(|store| store.record().clone())
    }

    fn replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        self.cluster
            .as_ref()
            .map(|cluster| &cluster.replicated)
            .ok_or_else(|| RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Cluster store is not available".into(),
                details: Value::Null,
            })
    }

    fn ready_replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        let participating = self
            .store
            .lock()
            .map_err(|_| unavailable("local Machine record lock poisoned"))?
            .record()
            .phase
            == LocalMachinePhase::Participating;
        if !participating {
            return Err(unavailable("Machine is not participating"));
        }
        self.replicated()
    }

    fn containers(&self) -> Result<&ContainerRuntime, RpcError> {
        self.containers
            .as_ref()
            .ok_or_else(|| unavailable("Docker is not available"))
    }
}

#[tonic::async_trait]
impl MachineRpc for MachineService {
    type ExecStream = RpcStream;
    type ContainerLogsStream = RpcStream;
    type MachineLogsStream = RpcStream;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::DescribeContract>(request)?;
        let machine_id = self.local_record()?.id;
        let mut capabilities = [
            DESCRIBE_CONTRACT_CAPABILITY,
            INSPECT_MACHINE_CAPABILITY,
            MACHINE_TOKEN_CAPABILITY,
            INITIALIZE_MACHINE_CAPABILITY,
            REGISTER_MACHINE_CAPABILITY,
            JOIN_MACHINE_CAPABILITY,
            LIST_MACHINES_CAPABILITY,
            UPDATE_MACHINE_CAPABILITY,
            REMOVE_LOCAL_MACHINE_CAPABILITY,
            REMOVE_MACHINE_CAPABILITY,
            INSPECT_WIREGUARD_CAPABILITY,
            RESET_MACHINE_CAPABILITY,
        ]
        .into_iter()
        .map(|name| CapabilityName::parse(name).expect("static capability name is valid"))
        .collect::<BTreeSet<_>>();
        if self.containers.is_some() {
            capabilities.extend(
                [
                    LIST_CONTAINERS_CAPABILITY,
                    INSPECT_CONTAINER_CAPABILITY,
                    CREATE_CONTAINER_CAPABILITY,
                    START_CONTAINER_CAPABILITY,
                    STOP_CONTAINER_CAPABILITY,
                    REMOVE_CONTAINER_CAPABILITY,
                    CREATE_VOLUME_CAPABILITY,
                    LIST_VOLUMES_CAPABILITY,
                    INSPECT_VOLUME_CAPABILITY,
                    REMOVE_VOLUME_CAPABILITY,
                    EXEC_CONTAINER_CAPABILITY,
                    CONTAINER_LOGS_CAPABILITY,
                    MACHINE_LOGS_CAPABILITY,
                    LIST_IMAGES_CAPABILITY,
                ]
                .into_iter()
                .map(|name| CapabilityName::parse(name).expect("static capability name is valid")),
            );
        }
        if self.caddyfile.is_some() {
            capabilities.insert(
                CapabilityName::parse(GET_CADDY_CONFIG_CAPABILITY)
                    .expect("static capability name is valid"),
            );
        }
        if self.cluster.is_some() {
            capabilities.extend(
                [
                    RESERVE_DOMAIN_CAPABILITY,
                    GET_DOMAIN_CAPABILITY,
                    RELEASE_DOMAIN_CAPABILITY,
                    CREATE_DOMAIN_RECORDS_CAPABILITY,
                ]
                .into_iter()
                .map(|name| CapabilityName::parse(name).expect("static capability name is valid")),
            );
        }
        respond(ContractDescription {
            machine_id,
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities,
        })
    }

    async fn inspect(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::Inspect>(request)?;
        let record = self.local_record()?;
        let Some(private_key) = record.wireguard_private_key.as_ref() else {
            return respond(store_error(StoreError::MissingPrivateKey));
        };
        let advertised_endpoints = if !request.advertised_endpoints.is_empty() {
            request.advertised_endpoints
        } else if let Some(machine) = &record.machine {
            machine.advertised_endpoints.clone()
        } else {
            discover_network(
                request.wireguard_port,
                request
                    .public_ip_override
                    .map_or(PublicIpDiscovery::Auto, PublicIpDiscovery::Override),
            )
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .endpoints
        };
        let store_version = match &self.cluster {
            Some(cluster) => cluster
                .replicated
                .version()
                .await
                .map_err(|error| Status::internal(error.to_string()))?,
            None => Default::default(),
        };
        let rtts = if request.include_rtts && record.phase == LocalMachinePhase::Participating {
            machine_rtts(
                &self
                    .cluster
                    .as_ref()
                    .ok_or_else(|| Status::unavailable("Cluster is not available"))?
                    .admin,
                self.replicated()
                    .map_err(|error| Status::unavailable(error.message))?,
            )
            .await
            .map_err(|error| Status::internal(error.to_string()))?
        } else {
            Vec::new()
        };
        respond(MachineDetails {
            id: record.id,
            phase: record.phase,
            machine: record.machine,
            public_key: private_key.public_key(),
            advertised_endpoints,
            store_version,
            rtts,
        })
    }

    async fn machine_token(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::MachineToken>(request)?;
        let record = self.local_record()?;
        let Some(private_key) = record.wireguard_private_key else {
            return respond(store_error(StoreError::MissingPrivateKey));
        };
        let discovered = discover_network(request.wireguard_port, request.public_ip)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        respond(MachineToken {
            public_key: private_key.public_key(),
            public_ip: discovered.public_ip,
            advertised_endpoints: if request.advertised_endpoints.is_empty() {
                discovered.endpoints
            } else {
                request.advertised_endpoints
            },
            runtime: local_runtime(),
        })
    }

    async fn initialize(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::Initialize>(request)?;
        let result = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .initialize(
                request.name,
                request.cluster_network,
                request.public_ip,
                request.advertised_endpoints,
                request.wireguard_mtu,
            );
        match result {
            Ok(machine) => {
                self.restart.send_replace(true);
                respond(Initialized { machine })
            }
            Err(error) => respond(store_error(error)),
        }
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::Register>(request)?;
        if request.advertised_endpoints.is_empty() {
            return respond(store_error(StoreError::MissingEndpoints));
        }
        if self.local_record()?.phase != LocalMachinePhase::Participating {
            return respond(unavailable("Machine is not participating"));
        }
        let replicated = match self.replicated() {
            Ok(store) => store,
            Err(error) => return respond(error),
        };
        let snapshot = replicated
            .machines()
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        if snapshot
            .observations
            .iter()
            .any(|machine| machine.name == request.name || machine.public_key == request.public_key)
        {
            return respond(RpcError {
                code: RpcErrorCode::Conflict,
                message: "Machine name or public key already exists".into(),
                details: Value::Null,
            });
        }
        let network = replicated
            .cluster_network()
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let assigned_machine = Machine {
            id: MachineId::random(),
            name: request.name,
            subnet: allocate_machine_subnet(
                network,
                snapshot.observations.iter().map(|machine| machine.subnet),
            )
            .map_err(|error| Status::internal(error.to_string()))?,
            management_address: management_address(request.public_key),
            public_key: request.public_key,
            public_ip: request.public_ip,
            advertised_endpoints: request.advertised_endpoints,
            runtime: request.runtime,
        };
        // TODO(UT-140): the imperative registration is deliberately unfenced and has no rollback.
        replicated
            .publish_local_machine(&assigned_machine)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let target_versions = replicated
            .version()
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let visible_peers = replicated
            .machines()
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .observations
            .into_iter()
            .filter(|machine| machine.id != assigned_machine.id)
            .collect();
        respond(Registered {
            assigned_machine,
            visible_peers,
            target_versions,
        })
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::Join>(request)?;
        let result = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .join(
                request.registration.assigned_machine,
                request.registration.visible_peers,
                request.registration.target_versions,
                request.wireguard_mtu,
            );
        match result {
            Ok(()) => {
                self.restart.send_replace(true);
                respond(JoinAccepted {})
            }
            Err(error) => respond(store_error(error)),
        }
    }

    async fn list_machines(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ListMachines>(request)?;
        let local = self.local_record()?;
        if local.phase != LocalMachinePhase::Participating {
            return respond(unavailable("Machine is not participating"));
        }
        let replicated = match self.replicated() {
            Ok(store) => store,
            Err(error) => return respond(error),
        };
        let machines = replicated
            .machines()
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .observations;
        let states = match &self.cluster {
            Some(cluster) => cluster
                .admin
                .membership_states()
                .await
                .map_err(|error| Status::internal(error.to_string()))?,
            None => Vec::new(),
        };
        let states = states
            .into_iter()
            .filter_map(|state| match state.address.ip() {
                IpAddr::V6(address) => {
                    Some((ployz_core::ManagementAddress(address), state.membership))
                }
                IpAddr::V4(_) => None,
            })
            .collect();
        let mut observations = synthesize_membership(machines, &local.id, &states);
        for observation in &mut observations {
            observation.selected_endpoint = local
                .selected_endpoints
                .get(&observation.machine.id)
                .copied();
        }
        respond(MachineList {
            machines: observations,
        })
    }

    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ListContainers>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let machine_id = self.local_record()?.id;
        match containers.list_managed(&machine_id).await {
            Ok(observations) => respond(ContainerList {
                containers: observations,
            }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn inspect_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::InspectContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let machine_id = self.local_record()?.id;
        match containers
            .inspect_managed(&request.container_id, &machine_id)
            .await
        {
            Ok(observation) => respond(ContainerDetails {
                container: observation,
            }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn create_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::CreateContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let record = self.local_record()?;
        let machine = record
            .machine
            .as_ref()
            .ok_or_else(|| Status::unavailable("Machine network is not configured"))?;
        let gateway =
            machine_gateway(machine.subnet).map_err(|error| Status::internal(error.to_string()))?;
        match containers
            .create(&record.id, gateway, request.kind, &request.resolved_spec)
            .await
        {
            Ok(created) => respond(created),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn start_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::StartContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.start(&request.container_id).await {
            Ok(()) => respond(ContainerChanged {
                container_id: request.container_id,
            }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn stop_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::StopContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers
            .stop(
                &request.container_id,
                request.signal.as_deref(),
                request.grace_period_seconds,
            )
            .await
        {
            Ok(()) => respond(ContainerChanged {
                container_id: request.container_id,
            }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn remove_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers
            .remove(&request.container_id, request.remove_volumes, request.force)
            .await
        {
            Ok(()) => respond(ContainerChanged {
                container_id: request.container_id,
            }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn create_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::CreateVolume>(request)?;
        let machine_id = self.local_record()?.id;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.create_volume(&machine_id, request).await {
            Ok(volume) => respond(VolumeCreated { volume }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ListVolumes>(request)?;
        let machine_id = self.local_record()?.id;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.list_volumes(&machine_id).await {
            Ok(volumes) => respond(VolumeList { volumes }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn inspect_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::InspectVolume>(request)?;
        let machine_id = self.local_record()?.id;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.inspect_volume(&machine_id, &request.name).await {
            Ok(volume) => respond(VolumeDetails { volume }),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveVolume>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.remove_volume(&request.name, request.force).await {
            Ok(()) => respond(VolumeRemoved {}),
            Err(error) => respond(docker_rpc_error(error)),
        }
    }

    async fn exec(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        containers
            .exec(request.into_inner())
            .await
            .map(Response::new)
    }

    async fn container_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        let request = op::ContainerLogs::from_request_body(request_body(request)?)
            .map_err(invalid_request)?;
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        let record = self.local_record()?;
        let machine = record
            .machine
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        containers
            .container_logs(&record.id, &machine.name, request)
            .await
            .map(Response::new)
    }

    async fn machine_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        let request =
            op::MachineLogs::from_request_body(request_body(request)?).map_err(invalid_request)?;
        let record = self.local_record()?;
        let machine = record
            .machine
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        let metadata = LogMetadata {
            origin: LogOrigin::Machine {
                service: request.service,
            },
            machine_id: record.id,
            machine_name: machine.name,
        };
        let source = match request.service {
            MachineLogService::Ployz | MachineLogService::Docker => {
                open_journal_logs(request.service.as_str(), &request.options).await?
            }
            MachineLogService::Corrosion => self
                .containers()
                .map_err(|error| Status::unavailable(error.message))?
                .raw_logs(crate::corrosion::DEFAULT_CONTAINER_NAME, &request.options)?,
        };
        Ok(Response::new(serve_logs(
            source,
            metadata,
            request.options.follow,
        )))
    }

    async fn update_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::UpdateMachine>(request)?;
        if request.update.is_empty() {
            return respond(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "at least one Machine update is required".into(),
                details: Value::Null,
            });
        }
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        let visible = replicated
            .machines()
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .observations;
        let publication = replicated.machine_publication().await;
        let updated = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .update(request.update, &visible);
        match updated {
            Ok(machine) => {
                if let Err(error) = publication.publish(&machine).await {
                    eprintln!("failed to publish updated local Machine: {error}");
                }
                respond(MachineUpdated { machine })
            }
            Err(error) => respond(store_error(error)),
        }
    }

    async fn remove_local_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveLocalMachine>(request)?;
        let machine_id = self.local_record()?.id;
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let publication = replicated.machine_publication().await;
        let prepared_reset = {
            let store = self
                .store
                .lock()
                .map_err(|_| Status::internal("local Machine record lock poisoned"))?;
            if store.record().phase == LocalMachinePhase::Resetting {
                None
            } else {
                match store.prepare_reset() {
                    Ok(prepared) => Some(prepared),
                    Err(error) => return respond(store_error(error)),
                }
            }
        };
        if let Err(error) = containers.remove_all_managed().await {
            return respond(RpcError {
                code: RpcErrorCode::Internal,
                message: error.to_string(),
                details: Value::Null,
            });
        }
        if let Some(prepared_reset) = prepared_reset {
            let mut store = self
                .store
                .lock()
                .map_err(|_| Status::internal("local Machine record lock poisoned"))?;
            if let Err(error) = prepared_reset.commit(&mut store) {
                return respond(store_error(error));
            }
        }
        let reset_warning = publication
            .remove(&machine_id)
            .await
            .err()
            .map(|error| error.to_string());
        respond(local_removal_response(
            &self.restart,
            reset_warning,
            request.restart_on_cleanup_failure,
        ))
    }

    async fn remove_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveMachine>(request)?;
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        replicated
            .remove_machine(&request.machine_id)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let local = self.local_record()?;
        if local.id == request.machine_id && local.phase == LocalMachinePhase::Resetting {
            self.restart.send_replace(true);
        }
        respond(MachineRemoved {})
    }

    async fn inspect_wireguard(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::InspectWireguard>(request)?;
        let device =
            inspect_wireguard_device().map_err(|error| Status::internal(error.to_string()))?;
        let Some(cluster) = &self.cluster else {
            return respond(WireGuardInspected { device });
        };
        let machines = match cluster.replicated.machines().await {
            Ok(snapshot) => snapshot.observations,
            Err(error) => {
                eprintln!("WireGuard Machine enrichment is unavailable: {error}");
                Vec::new()
            }
        };
        let rtts = match machine_rtts(&cluster.admin, &cluster.replicated).await {
            Ok(rtts) => rtts,
            Err(error) => {
                eprintln!("WireGuard RTT enrichment is unavailable: {error}");
                Vec::new()
            }
        }
        .into_iter()
        .filter_map(|observation| {
            observation
                .machine
                .map(|machine| (machine.id, observation.statistics))
        })
        .collect();
        respond(WireGuardInspected {
            device: associate_wireguard_peers(device, &machines, &rtts),
        })
    }

    async fn list_images(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::ListImages>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let images = containers
            .list_images(request.reference.as_deref())
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        respond(images)
    }

    async fn get_caddy_config(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::GetCaddyConfig>(request)?;
        let Some(path) = &self.caddyfile else {
            return respond(unavailable("Caddy configuration is not available"));
        };
        match std::fs::read_to_string(path) {
            Ok(caddyfile) => respond(CaddyConfig { caddyfile }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => respond(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("Caddyfile {} does not exist", path.display()),
                details: Value::Null,
            }),
            Err(error) => Err(Status::internal(format!(
                "read Caddyfile {}: {error}",
                path.display()
            ))),
        }
    }

    async fn reserve_domain(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::ReserveDomain>(request)?;
        if request.endpoint.is_empty() {
            return respond(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "hosted DNS endpoint is required".into(),
                details: Value::Null,
            });
        }
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match self
            .hosted_dns
            .reserve_domain(replicated, &request.endpoint)
            .await
        {
            Ok(name) => respond(Domain { name }),
            Err(error) => respond(hosted_dns_error(error)),
        }
    }

    async fn get_domain(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::GetDomain>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match self.hosted_dns.domain(replicated).await {
            Ok(name) => respond(Domain { name }),
            Err(error) => respond(hosted_dns_error(error)),
        }
    }

    async fn release_domain(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ReleaseDomain>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match self.hosted_dns.release_domain(replicated).await {
            Ok(name) => respond(Domain { name }),
            Err(error) => respond(hosted_dns_error(error)),
        }
    }

    async fn create_domain_records(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::CreateDomainRecords>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match self
            .hosted_dns
            .create_records(replicated, &request.records)
            .await
        {
            Ok(records) => respond(DomainRecords { records }),
            Err(error) => respond(hosted_dns_error(error)),
        }
    }

    async fn reset(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::Reset>(request)?;
        if let Some(containers) = &self.containers
            && let Err(error) = containers.remove_all_managed().await
        {
            return respond(docker_rpc_error(error));
        }
        let reset = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .begin_reset();
        match reset {
            Ok(()) => {
                self.restart.send_replace(true);
                respond(ResetAccepted {})
            }
            Err(error) => respond(store_error(error)),
        }
    }
}

async fn machine_rtts(
    admin: &AdminClient,
    replicated: &ReplicatedStore,
) -> Result<Vec<RttObservation>, crate::corrosion::Error> {
    let machines = replicated.machines().await?.observations;
    let identities = unique_identities(machines.into_iter().map(|machine| {
        (
            IpAddr::V6(machine.management_address.0),
            MachineIdentity {
                id: machine.id,
                name: machine.name,
            },
        )
    }));
    Ok(admin
        .member_rtts()
        .await?
        .into_iter()
        .map(|mut observation| {
            observation.machine = identities.get(&observation.address.ip()).cloned();
            observation
        })
        .collect())
}

fn unique_identities(
    entries: impl IntoIterator<Item = (IpAddr, MachineIdentity)>,
) -> BTreeMap<IpAddr, MachineIdentity> {
    let mut identities = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (address, identity) in entries {
        if identities.insert(address, identity).is_some() {
            ambiguous.insert(address);
        }
    }
    identities.retain(|address, _| !ambiguous.contains(address));
    identities
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.into(),
        details: Value::Null,
    }
}

fn hosted_dns_error(error: crate::hosted_dns::Error) -> RpcError {
    let (code, details) = match error {
        crate::hosted_dns::Error::AlreadyReserved => (RpcErrorCode::Conflict, Value::Null),
        crate::hosted_dns::Error::NotFound => (RpcErrorCode::NotFound, Value::Null),
        crate::hosted_dns::Error::AuthNoDomain => (
            RpcErrorCode::Unauthenticated,
            serde_json::json!({ "no_domain": true }),
        ),
        crate::hosted_dns::Error::Authentication => (RpcErrorCode::Unauthenticated, Value::Null),
        crate::hosted_dns::Error::Store(_)
        | crate::hosted_dns::Error::Http(_)
        | crate::hosted_dns::Error::Json(_)
        | crate::hosted_dns::Error::InvalidEndpoint(_)
        | crate::hosted_dns::Error::Status(_) => (RpcErrorCode::Internal, Value::Null),
    };
    RpcError {
        code,
        message: error.to_string(),
        details,
    }
}

fn local_removal_response(
    restart: &watch::Sender<bool>,
    reset_warning: Option<String>,
    restart_on_warning: bool,
) -> LocalMachineRemoved {
    if reset_warning.is_none() || restart_on_warning {
        restart.send_replace(true);
    }
    LocalMachineRemoved { reset_warning }
}

fn store_error(error: StoreError) -> RpcError {
    let code = match error {
        StoreError::AlreadyResetting
        | StoreError::AlreadyInitialized
        | StoreError::NotParticipating => RpcErrorCode::Conflict,
        StoreError::MissingEndpoints
        | StoreError::MissingPeers
        | StoreError::MissingPrivateKey
        | StoreError::KeyMismatch
        | StoreError::InvalidNetwork(_) => RpcErrorCode::InvalidArgument,
        StoreError::MachineUpdate(ployz_core::MachineUpdateError::DuplicateName) => {
            RpcErrorCode::Conflict
        }
        StoreError::MachineUpdate(ployz_core::MachineUpdateError::MissingEndpoints) => {
            RpcErrorCode::InvalidArgument
        }
        StoreError::Io(_)
        | StoreError::Json(_)
        | StoreError::InvalidPhase
        | StoreError::MachineIdMismatch
        | StoreError::NotResetting
        | StoreError::NotJoining
        | StoreError::AlreadyRunning(_)
        | StoreError::UnsafeDataDirectory(_)
        | StoreError::UnownedDataDirectory(_)
        | StoreError::OwnershipLost(_)
        | StoreError::ResetPreparationLost(_) => RpcErrorCode::Internal,
    };
    RpcError {
        code,
        message: error.to_string(),
        details: Value::Null,
    }
}

fn docker_rpc_error(error: DockerError) -> RpcError {
    RpcError {
        code: error.rpc_code(),
        message: error.to_string(),
        details: Value::Null,
    }
}

#[allow(clippy::result_large_err)]
fn respond(response: impl Into<RpcResponse>) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(
        response.into().encode().map_err(internal_response)?,
    ))
}

fn invalid_request(error: impl std::fmt::Display) -> Status {
    Status::invalid_argument(error.to_string())
}

#[allow(clippy::result_large_err)]
fn request_body(request: Request<OpaquePayload>) -> Result<RpcRequestBody, Status> {
    request
        .into_inner()
        .decode_request()
        .map(|request| request.body)
        .map_err(invalid_request)
}

#[allow(clippy::result_large_err)]
fn expect<T: Rpc>(request: Request<OpaquePayload>) -> Result<T::Request, Status> {
    T::from_request_body(request_body(request)?).map_err(invalid_request)
}

fn internal_response(error: impl std::fmt::Display) -> Status {
    Status::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{local_removal_response, store_error, unique_identities};
    use crate::machine::StoreError;
    use ployz_core::{MachineId, MachineIdentity, MachineName, RpcErrorCode};

    #[test]
    fn non_participating_update_is_a_typed_conflict() {
        assert_eq!(
            store_error(StoreError::NotParticipating).code,
            RpcErrorCode::Conflict
        );
    }

    #[test]
    fn failed_local_removal_keeps_the_daemon_available_for_entry_fallback() {
        let (restart, restart_rx) = tokio::sync::watch::channel(false);
        let removed =
            local_removal_response(&restart, Some("replicated delete failed".into()), false);

        assert_eq!(
            removed.reset_warning.as_deref(),
            Some("replicated delete failed")
        );
        assert!(!*restart_rx.borrow());
    }

    #[test]
    fn failed_remote_removal_restarts_after_delegating_entry_fallback() {
        let (restart, restart_rx) = tokio::sync::watch::channel(false);
        local_removal_response(&restart, Some("replicated delete failed".into()), true);

        assert!(*restart_rx.borrow());
    }

    #[test]
    fn duplicate_management_addresses_have_no_identity_winner() {
        let duplicate = "192.0.2.1".parse().unwrap();
        let unique = "192.0.2.2".parse().unwrap();
        let identity = |seed: char| MachineIdentity {
            id: MachineId::parse(seed.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(seed.to_string()).unwrap(),
        };
        let identities = unique_identities([
            (duplicate, identity('1')),
            (duplicate, identity('2')),
            (unique, identity('3')),
        ]);

        assert!(!identities.contains_key(&duplicate));
        assert_eq!(identities.get(&unique), Some(&identity('3')));
    }
}
