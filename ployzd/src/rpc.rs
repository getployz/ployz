use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use ployz_core::{
    CONTAINER_LOGS_CAPABILITY, CREATE_CONTAINER_CAPABILITY, CREATE_DOMAIN_RECORDS_CAPABILITY,
    CREATE_VOLUME_CAPABILITY, CaddyConfig, CapabilityName, ContainerChanged, ContainerDetails,
    ContainerList, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, Domain, DomainRecords,
    EXEC_CONTAINER_CAPABILITY, GET_CADDY_CONFIG_CAPABILITY, GET_DOMAIN_CAPABILITY,
    INITIALIZE_MACHINE_CAPABILITY, INSPECT_CONTAINER_CAPABILITY, INSPECT_MACHINE_CAPABILITY,
    INSPECT_VOLUME_CAPABILITY, INSPECT_WIREGUARD_CAPABILITY, JOIN_MACHINE_CAPABILITY,
    LIST_CONTAINERS_CAPABILITY, LIST_IMAGES_CAPABILITY, LIST_MACHINES_CAPABILITY,
    LIST_VOLUMES_CAPABILITY, LocalMachinePhase, LogMetadata, LogOrigin, MACHINE_LOGS_CAPABILITY,
    MACHINE_TOKEN_CAPABILITY, MachineLogService, MachineRpc, OpaquePayload, PROTOCOL_MAJOR,
    REGISTER_MACHINE_CAPABILITY, RELEASE_DOMAIN_CAPABILITY, REMOVE_CONTAINER_CAPABILITY,
    REMOVE_LOCAL_MACHINE_CAPABILITY, REMOVE_MACHINE_CAPABILITY, REMOVE_VOLUME_CAPABILITY,
    RESERVE_DOMAIN_CAPABILITY, RESET_MACHINE_CAPABILITY, Rpc, RpcError, RpcErrorCode,
    RpcRequestBody, RpcResponse, START_CONTAINER_CAPABILITY, STOP_CONTAINER_CAPABILITY,
    UPDATE_MACHINE_CAPABILITY, VolumeList, VolumeRemoved, op,
};
use serde_json::Value;
use tokio::sync::watch;
use tonic::{Request, Response, Status};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, Error as DockerError},
    logs::{RpcStream, open_journal_logs, serve_logs},
    machine::{LocalMachine, LocalMachineError, LocalMachineStore, StoreError},
};

#[derive(Clone)]
pub struct MachineService {
    local: LocalMachine,
    hosted_dns: crate::hosted_dns::HostedDns,
    caddyfile: Option<PathBuf>,
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
            local: LocalMachine::new(store, restart).with_cluster(cluster),
            hosted_dns: crate::hosted_dns::HostedDns::new(),
            caddyfile: None,
        }
    }

    #[must_use]
    pub fn with_containers(self, containers: ContainerRuntime) -> Self {
        self.with_optional_containers(Some(containers))
    }

    #[must_use]
    pub fn with_optional_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.local = self.local.with_containers(containers);
        self
    }

    #[must_use]
    pub fn with_caddyfile(mut self, path: PathBuf) -> Self {
        self.caddyfile = Some(path);
        self
    }

    #[allow(clippy::result_large_err)]
    fn local_record(&self) -> Result<crate::machine::LocalMachineRecord, Status> {
        self.local
            .record()
            .map_err(|_| Status::internal("local Machine record lock poisoned"))
    }

    fn replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        self.local
            .replicated()
            .map_err(|_| unavailable("Cluster store is not available"))
    }

    fn ready_replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        let participating = self
            .local_record()
            .map_err(|error| unavailable(error.message()))?
            .phase
            == LocalMachinePhase::Participating;
        if !participating {
            return Err(unavailable("Machine is not participating"));
        }
        self.replicated()
    }

    fn containers(&self) -> Result<&ContainerRuntime, RpcError> {
        self.local
            .containers()
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
        if self.local.containers().is_some() {
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
        if self.local.has_cluster() {
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
        finish(self.local.inspect(expect::<op::Inspect>(request)?).await)
    }

    async fn machine_token(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .machine_token(expect::<op::MachineToken>(request)?)
                .await,
        )
    }

    async fn initialize(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.initialize(expect::<op::Initialize>(request)?))
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.register(expect::<op::Register>(request)?).await)
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.join(expect::<op::Join>(request)?))
    }

    async fn list_machines(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ListMachines>(request)?;
        finish(self.local.list_machines().await)
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
        let gateway = machine.subnet.gateway();
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
            Ok(volume) => respond(volume),
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
            Ok(volume) => respond(volume),
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
        finish(
            self.local
                .update(expect::<op::UpdateMachine>(request)?)
                .await,
        )
    }

    async fn remove_local_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .remove_local(expect::<op::RemoveLocalMachine>(request)?)
                .await,
        )
    }

    async fn remove_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .remove_peer(expect::<op::RemoveMachine>(request)?)
                .await,
        )
    }

    async fn inspect_wireguard(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::InspectWireguard>(request)?;
        finish(self.local.inspect_wireguard().await)
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
        finish(self.local.reset().await)
    }
}

#[allow(clippy::result_large_err)]
fn local_error(error: LocalMachineError) -> Result<Response<OpaquePayload>, Status> {
    match error {
        LocalMachineError::Store(error) => respond(store_error(error)),
        LocalMachineError::NotParticipating => respond(unavailable("Machine is not participating")),
        LocalMachineError::ClusterStoreUnavailable => {
            respond(unavailable("Cluster store is not available"))
        }
        LocalMachineError::ClusterUnavailable => {
            Err(Status::unavailable("Cluster is not available"))
        }
        LocalMachineError::DockerUnavailable => respond(unavailable("Docker is not available")),
        LocalMachineError::DuplicateMachine => respond(RpcError {
            code: RpcErrorCode::Conflict,
            message: "Machine name or public key already exists".into(),
            details: Value::Null,
        }),
        LocalMachineError::EmptyUpdate => respond(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: "at least one Machine update is required".into(),
            details: Value::Null,
        }),
        LocalMachineError::LockPoisoned => {
            Err(Status::internal("local Machine record lock poisoned"))
        }
        LocalMachineError::Cluster(error) => Err(Status::internal(error.to_string())),
        LocalMachineError::Network(error) => Err(Status::internal(error.to_string())),
        LocalMachineError::Docker(error) => respond(docker_rpc_error(error)),
        LocalMachineError::Cleanup(message) => respond(RpcError {
            code: RpcErrorCode::Internal,
            message,
            details: Value::Null,
        }),
    }
}

#[allow(clippy::result_large_err)]
fn finish(
    result: Result<impl Into<RpcResponse>, LocalMachineError>,
) -> Result<Response<OpaquePayload>, Status> {
    match result {
        Ok(value) => respond(value),
        Err(error) => local_error(error),
    }
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
    use super::store_error;
    use crate::machine::StoreError;
    use ployz_core::RpcErrorCode;

    #[test]
    fn non_participating_update_is_a_typed_conflict() {
        assert_eq!(
            store_error(StoreError::NotParticipating).code,
            RpcErrorCode::Conflict
        );
    }
}
