use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    sync::{Arc, Mutex},
};

use ployz_core::{
    CONTAINER_LOGS_CAPABILITY, CREATE_CONTAINER_CAPABILITY, CREATE_VOLUME_CAPABILITY,
    CapabilityName, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, EXEC_CONTAINER_CAPABILITY,
    INITIALIZE_MACHINE_CAPABILITY, INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY,
    INSPECT_VOLUME_CAPABILITY, INSPECT_WIREGUARD_CAPABILITY, JOIN_MACHINE_CAPABILITY,
    LIST_CONTAINERS_CAPABILITY, LIST_MACHINES_CAPABILITY, LIST_VOLUMES_CAPABILITY,
    LocalMachinePhase, LocalMachineRemoved, LogMetadata, LogOrigin, MACHINE_LOGS_CAPABILITY,
    MACHINE_TOKEN_CAPABILITY, Machine, MachineDetails, MachineId, MachineIdentity,
    MachineLogService, MachineRpc, MachineToken, OpaquePayload, PROTOCOL_MAJOR, PublicIpDiscovery,
    REGISTER_MACHINE_CAPABILITY, REMOVE_CONTAINER_CAPABILITY, REMOVE_LOCAL_MACHINE_CAPABILITY,
    REMOVE_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY, RESET_MACHINE_CAPABILITY, Registered,
    RpcError, RpcErrorCode, RpcRequestBody, RpcResponse, RttObservation,
    START_CONTAINER_CAPABILITY, STOP_CONTAINER_CAPABILITY, UPDATE_MACHINE_CAPABILITY,
    associate_wireguard_peers, synthesize_membership,
};
use serde_json::Value;
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{Error as DockerError, LocalDocker, MachineSpecStore},
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
    cluster: Option<ClusterContext>,
    containers: Option<ContainerContext>,
}

#[derive(Clone)]
struct ClusterContext {
    replicated: ReplicatedStore,
    admin: AdminClient,
}

#[derive(Clone)]
struct ContainerContext {
    docker: LocalDocker,
    specs: MachineSpecStore,
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
            cluster: cluster.map(|(replicated, admin)| ClusterContext { replicated, admin }),
            containers: None,
        }
    }

    #[must_use]
    pub fn with_containers(mut self, docker: LocalDocker, specs: MachineSpecStore) -> Self {
        self.containers = Some(ContainerContext { docker, specs });
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

    fn containers(&self) -> Result<&ContainerContext, RpcError> {
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
        if !matches!(request_body(request)?, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument(
                "expected describe_contract request",
            ));
        }
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
                ]
                .into_iter()
                .map(|name| CapabilityName::parse(name).expect("static capability name is valid")),
            );
        }
        respond(RpcResponse::contract_description(ContractDescription {
            machine_id,
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities,
        }))
    }

    async fn inspect(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::Inspect(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected inspect request"));
        };
        let record = self.local_record()?;
        let Some(private_key) = record.wireguard_private_key.as_ref() else {
            return respond(RpcResponse::error(store_error(
                StoreError::MissingPrivateKey,
            )));
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
        respond(RpcResponse::machine_details(MachineDetails {
            id: record.id,
            phase: record.phase,
            machine: record.machine,
            public_key: private_key.public_key(),
            advertised_endpoints,
            store_version,
            rtts,
        }))
    }

    async fn machine_token(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::MachineToken(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected machine_token request"));
        };
        let record = self.local_record()?;
        let Some(private_key) = record.wireguard_private_key else {
            return respond(RpcResponse::error(store_error(
                StoreError::MissingPrivateKey,
            )));
        };
        let discovered = discover_network(request.wireguard_port, request.public_ip)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        respond(RpcResponse::machine_token(MachineToken {
            public_key: private_key.public_key(),
            public_ip: discovered.public_ip,
            advertised_endpoints: if request.advertised_endpoints.is_empty() {
                discovered.endpoints
            } else {
                request.advertised_endpoints
            },
            runtime: local_runtime(),
        }))
    }

    async fn initialize(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::Initialize(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected initialize request"));
        };
        let result = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .initialize(
                request.name,
                request.cluster_network,
                request.advertised_endpoints,
                request.wireguard_mtu,
            );
        match result {
            Ok(machine) => {
                self.restart.send_replace(true);
                respond(RpcResponse::initialized(machine))
            }
            Err(error) => respond(RpcResponse::error(store_error(error))),
        }
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::Register(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected register request"));
        };
        if request.advertised_endpoints.is_empty() {
            return respond(RpcResponse::error(store_error(
                StoreError::MissingEndpoints,
            )));
        }
        if self.local_record()?.phase != LocalMachinePhase::Participating {
            return respond(RpcResponse::error(unavailable(
                "Machine is not participating",
            )));
        }
        let replicated = match self.replicated() {
            Ok(store) => store,
            Err(error) => return respond(RpcResponse::error(error)),
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
            return respond(RpcResponse::error(RpcError {
                code: RpcErrorCode::Conflict,
                message: "Machine name or public key already exists".into(),
                details: Value::Null,
            }));
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
        respond(RpcResponse::registered(Registered {
            assigned_machine,
            visible_peers,
            target_versions,
        }))
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::Join(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected join request"));
        };
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
                respond(RpcResponse::join_accepted())
            }
            Err(error) => respond(RpcResponse::error(store_error(error))),
        }
    }

    async fn list_machines(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !matches!(request_body(request)?, RpcRequestBody::ListMachines(_)) {
            return Err(Status::invalid_argument("expected list_machines request"));
        }
        let local = self.local_record()?;
        if local.phase != LocalMachinePhase::Participating {
            return respond(RpcResponse::error(unavailable(
                "Machine is not participating",
            )));
        }
        let replicated = match self.replicated() {
            Ok(store) => store,
            Err(error) => return respond(RpcResponse::error(error)),
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
        respond(RpcResponse::machine_list(observations))
    }

    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !matches!(request_body(request)?, RpcRequestBody::ListContainers(_)) {
            return Err(Status::invalid_argument("expected list_containers request"));
        }
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        let machine_id = self.local_record()?.id;
        match containers
            .docker
            .list_managed(&machine_id, &containers.specs)
            .await
        {
            Ok(observations) => respond(RpcResponse::container_list(observations)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn inspect_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::InspectContainer(request) = request_body(request)? else {
            return Err(Status::invalid_argument(
                "expected inspect_container request",
            ));
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        let machine_id = self.local_record()?.id;
        match containers
            .docker
            .inspect_managed(&request.container_id, &machine_id, &containers.specs)
            .await
        {
            Ok(observation) => respond(RpcResponse::container_details(observation)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn create_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::CreateContainer(request) = request_body(request)? else {
            return Err(Status::invalid_argument(
                "expected create_container request",
            ));
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        let record = self.local_record()?;
        let machine = record
            .machine
            .as_ref()
            .ok_or_else(|| Status::unavailable("Machine network is not configured"))?;
        let gateway =
            machine_gateway(machine.subnet).map_err(|error| Status::internal(error.to_string()))?;
        match containers
            .docker
            .create(
                &record.id,
                gateway,
                &containers.specs,
                request.kind,
                &request.resolved_spec,
            )
            .await
        {
            Ok(created) => respond(RpcResponse::container_created(created)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn start_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::StartContainer(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected start_container request"));
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match containers
            .docker
            .start(&containers.specs, &request.container_id)
            .await
        {
            Ok(()) => respond(RpcResponse::container_changed(request.container_id)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn stop_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::StopContainer(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected stop_container request"));
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match containers
            .docker
            .stop(
                &containers.specs,
                &request.container_id,
                request.signal.as_deref(),
                request.grace_period_seconds,
            )
            .await
        {
            Ok(()) => respond(RpcResponse::container_changed(request.container_id)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn remove_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::RemoveContainer(request) = request_body(request)? else {
            return Err(Status::invalid_argument(
                "expected remove_container request",
            ));
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match containers
            .docker
            .remove(
                &containers.specs,
                &request.container_id,
                request.remove_volumes,
                request.force,
            )
            .await
        {
            Ok(()) => respond(RpcResponse::container_changed(request.container_id)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn create_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::CreateVolume(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected create_volume request"));
        };
        let machine_id = self.local_record()?.id;
        let docker = match self.containers() {
            Ok(docker) => docker,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match docker.docker.create_volume(&machine_id, request).await {
            Ok(volume) => respond(RpcResponse::volume_created(volume)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !matches!(request_body(request)?, RpcRequestBody::ListVolumes(_)) {
            return Err(Status::invalid_argument("expected list_volumes request"));
        }
        let machine_id = self.local_record()?.id;
        let docker = match self.containers() {
            Ok(docker) => docker,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match docker.docker.list_volumes(&machine_id).await {
            Ok(volumes) => respond(RpcResponse::volume_list(volumes)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn inspect_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::InspectVolume(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected inspect_volume request"));
        };
        let machine_id = self.local_record()?.id;
        let docker = match self.containers() {
            Ok(docker) => docker,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match docker
            .docker
            .inspect_volume(&machine_id, &request.name)
            .await
        {
            Ok(volume) => respond(RpcResponse::volume_details(volume)),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
        }
    }

    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::RemoveVolume(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected remove_volume request"));
        };
        let docker = match self.containers() {
            Ok(docker) => docker,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        match docker
            .docker
            .remove_volume(&request.name, request.force)
            .await
        {
            Ok(()) => respond(RpcResponse::volume_removed()),
            Err(error) => respond(RpcResponse::error(docker_rpc_error(error))),
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
            .docker
            .exec(request.into_inner())
            .await
            .map(Response::new)
    }

    async fn container_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        let RpcRequestBody::ContainerLogs(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected container_logs request"));
        };
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        let record = self.local_record()?;
        let machine = record
            .machine
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        containers
            .docker
            .container_logs(&record.id, &machine.name, &containers.specs, request)
            .await
            .map(Response::new)
    }

    async fn machine_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        let RpcRequestBody::MachineLogs(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected machine_logs request"));
        };
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
                .docker
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
        let RpcRequestBody::UpdateMachine(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected update_machine request"));
        };
        if request.update.is_empty() {
            return respond(RpcResponse::error(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "at least one Machine update is required".into(),
                details: Value::Null,
            }));
        }
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(RpcResponse::error(error)),
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
                respond(RpcResponse::machine_updated(machine))
            }
            Err(error) => respond(RpcResponse::error(store_error(error))),
        }
    }

    async fn remove_local_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::RemoveLocalMachine(request) = request_body(request)? else {
            return Err(Status::invalid_argument(
                "expected remove_local_machine request",
            ));
        };
        let machine_id = self.local_record()?.id;
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        if let Err(error) = containers
            .docker
            .remove_all_managed(&containers.specs)
            .await
        {
            return respond(RpcResponse::error(RpcError {
                code: RpcErrorCode::Internal,
                message: error.to_string(),
                details: Value::Null,
            }));
        }
        let publication = replicated.machine_publication().await;
        let reset = {
            let mut store = self
                .store
                .lock()
                .map_err(|_| Status::internal("local Machine record lock poisoned"))?;
            if store.record().phase == LocalMachinePhase::Resetting {
                Ok(())
            } else {
                store.begin_reset()
            }
        };
        if let Err(error) = reset {
            return respond(RpcResponse::error(store_error(error)));
        }
        let reset_warning = publication
            .remove(&machine_id)
            .await
            .err()
            .map(|error| error.to_string());
        respond(RpcResponse::local_machine_removed(local_removal_response(
            &self.restart,
            reset_warning,
            request.restart_on_cleanup_failure,
        )))
    }

    async fn remove_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let RpcRequestBody::RemoveMachine(request) = request_body(request)? else {
            return Err(Status::invalid_argument("expected remove_machine request"));
        };
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(RpcResponse::error(error)),
        };
        replicated
            .remove_machine(&request.machine_id)
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        let local = self.local_record()?;
        if local.id == request.machine_id && local.phase == LocalMachinePhase::Resetting {
            self.restart.send_replace(true);
        }
        respond(RpcResponse::machine_removed())
    }

    async fn inspect_wireguard(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !matches!(request_body(request)?, RpcRequestBody::InspectWireguard(_)) {
            return Err(Status::invalid_argument(
                "expected inspect_wireguard request",
            ));
        }
        let device =
            inspect_wireguard_device().map_err(|error| Status::internal(error.to_string()))?;
        let Some(cluster) = &self.cluster else {
            return respond(RpcResponse::wireguard_inspected(device));
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
        respond(RpcResponse::wireguard_inspected(associate_wireguard_peers(
            device, &machines, &rtts,
        )))
    }

    async fn reset(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !matches!(request_body(request)?, RpcRequestBody::Reset(_)) {
            return Err(Status::invalid_argument("expected reset request"));
        }
        if let Some(containers) = &self.containers
            && let Err(error) = containers
                .docker
                .remove_all_managed(&containers.specs)
                .await
        {
            return respond(RpcResponse::error(docker_rpc_error(error)));
        }
        let reset = self
            .store
            .lock()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))?
            .begin_reset();
        match reset {
            Ok(()) => {
                self.restart.send_replace(true);
                respond(RpcResponse::reset_accepted())
            }
            Err(error) => respond(RpcResponse::error(store_error(error))),
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
        | StoreError::OwnershipLost(_) => RpcErrorCode::Internal,
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
fn respond(response: RpcResponse) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(response.encode().map_err(internal_response)?))
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
