//! Local Machine RPC. Listeners take [`super::MachineApi`], not this type.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    CapabilityAdvertisement, CloudPairing, CloudPairingSet, ContainerList, ContainerObservationMap,
    ContractDescription, Domain, DomainRecords, IngressProxyConfig, LocalMachinePhase, LogMetadata,
    LogOrigin, MachineId, MachineLogService, MachineRpc, MachineRpcClient, OpaquePayload,
    PROTOCOL_MAJOR, Rpc, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse, op,
};
use serde_json::Value;
use tokio::{sync::watch, time::Instant};
use tonic::transport::Endpoint;
use tonic::{Request, Response, Status};

use crate::{
    corrosion::{AdminClient, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    logs::{RpcStream, open_journal_logs, serve_logs},
    machine::{LocalMachine, LocalMachineError, LocalMachineStore, StoreError},
    network::MACHINE_API_PORT,
    runtime_watch::{RuntimeWatch, RuntimeWatchStream},
};

/// Metadata on a forwarded Machine-to-Machine Register. The named Allocator
/// admits locally and does not forward again.
pub(crate) const REGISTER_FORWARDED_METADATA: &str = "x-ployz-register-forwarded";
const MAX_CONTAINER_OBSERVATION_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct MachineService {
    local: LocalMachine,
    hosted_dns: crate::hosted_dns::HostedDns,
    ingress_data_dir: Option<PathBuf>,
    ingest: Arc<ImageIngest>,
    machine_api_port: u16,
    #[cfg(test)]
    allocator_endpoint: Option<(MachineId, std::net::SocketAddr)>,
    cloud_pairing: Option<watch::Sender<Option<CloudPairing>>>,
    runtime_watch: Arc<RuntimeWatch>,
    pub(crate) builds: Arc<crate::build::Runner>,
}

impl MachineService {
    #[must_use]
    pub fn with_cluster(
        store: Arc<Mutex<LocalMachineStore>>,
        restart: watch::Sender<bool>,
        cluster: Option<(ReplicatedStore, AdminClient)>,
    ) -> Self {
        Self {
            local: LocalMachine::new(store, restart).with_cluster(cluster),
            hosted_dns: crate::hosted_dns::HostedDns::new(),
            ingress_data_dir: None,
            ingest: ImageIngest::new(None, None),
            machine_api_port: MACHINE_API_PORT,
            #[cfg(test)]
            allocator_endpoint: None,
            cloud_pairing: None,
            runtime_watch: Arc::default(),
            builds: crate::build::Runner::new(Default::default(), Default::default())
                .expect("default Build policy"),
        }
    }

    #[must_use]
    pub(super) fn with_participation(mut self, participating: watch::Sender<bool>) -> Self {
        self.local = self.local.with_participation(participating);
        self
    }

    #[must_use]
    pub(super) fn with_cluster_option(
        mut self,
        cluster: Option<(ReplicatedStore, AdminClient)>,
    ) -> Self {
        self.local = self.local.with_cluster(cluster);
        self
    }

    #[must_use]
    pub fn with_optional_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.local = self.local.with_containers(containers);
        self
    }

    #[must_use]
    /// Make exact Ingress Proxy configuration available through the Machine RPC.
    pub fn with_ingress_data_dir(mut self, path: PathBuf) -> Self {
        self.ingress_data_dir = Some(path);
        self
    }

    /// Image ingest started on first Ensure and stopped with the daemon.
    #[must_use]
    pub fn with_image_ingest(mut self, ingest: Arc<ImageIngest>) -> Self {
        self.ingest = ingest;
        self
    }

    #[must_use]
    pub fn with_cloud_pairing(mut self, pairing: watch::Sender<Option<CloudPairing>>) -> Self {
        self.cloud_pairing = Some(pairing);
        self
    }

    /// Local Machine used to construct routing from this Machine's record and store.
    #[must_use]
    pub(crate) fn local(&self) -> LocalMachine {
        self.local.clone()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_machine_api_port(mut self, port: u16) -> Self {
        self.machine_api_port = port;
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_allocator_endpoint(
        mut self,
        machine: MachineId,
        address: std::net::SocketAddr,
    ) -> Self {
        self.allocator_endpoint = Some((machine, address));
        self
    }

    pub(super) fn machine_api_port(&self) -> u16 {
        self.machine_api_port
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
            .phase()
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

    async fn forward_register(
        &self,
        payload: OpaquePayload,
    ) -> Result<Response<OpaquePayload>, Status> {
        let replicated = match self.local.replicated() {
            Ok(store) => store,
            Err(error) => return local_error(error),
        };
        let first = match replicated.allocator().await {
            Ok(Some(row)) => row.machine_id,
            Ok(None) => return local_error(LocalMachineError::NotAllocator),
            Err(error) => return local_error(error.into()),
        };
        if let Some(response) = self.dial_allocator(first, payload.clone()).await? {
            return Ok(response);
        }
        let named = match replicated.allocator().await {
            Ok(row) => row.map(|row| row.machine_id),
            Err(error) => return local_error(error.into()),
        };
        if named != Some(first)
            && let Some(allocator) = named
            && let Some(response) = self.dial_allocator(allocator, payload).await?
        {
            return Ok(response);
        }
        let me = match self.local_record() {
            Ok(record) => record.id(),
            Err(error) => return Err(error),
        };
        match self.local.isolation_locked().await {
            Ok(true) => return local_error(LocalMachineError::IsolationLocked),
            Ok(false) => {}
            Err(error) => return local_error(error),
        }
        match replicated.steal_allocator(&me).await {
            Ok(()) => local_error(LocalMachineError::AllocatorNotQuiet),
            Err(error) => local_error(error.into()),
        }
    }

    async fn dial_allocator(
        &self,
        allocator: MachineId,
        payload: OpaquePayload,
    ) -> Result<Option<Response<OpaquePayload>>, Status> {
        let replicated = match self.local.replicated() {
            Ok(store) => store,
            Err(error) => return local_error(error).map(Some),
        };
        let Some(target) = (match replicated.machine(allocator.as_str()).await {
            Ok(machine) => machine,
            Err(error) => return local_error(error.into()).map(Some),
        }) else {
            return Ok(None);
        };
        let address =
            std::net::SocketAddr::new(target.management_address().0.into(), self.machine_api_port);
        #[cfg(test)]
        let address = self
            .allocator_endpoint
            .filter(|(id, _)| *id == target.id)
            .map_or(address, |(_, address)| address);
        let endpoint = Endpoint::from_shared(format!("http://{address}"))
            .map_err(|error| Status::internal(error.to_string()))?
            .connect_timeout(Duration::from_secs(10));
        let mut client = MachineRpcClient::new(match endpoint.connect().await {
            Ok(channel) => channel,
            Err(_) => return Ok(None),
        });
        let mut outbound = Request::new(payload);
        outbound.metadata_mut().insert(
            REGISTER_FORWARDED_METADATA,
            "1".parse().expect("ASCII metadata"),
        );
        match client.register(outbound).await {
            Ok(response) => Ok(Some(response)),
            Err(_) => Ok(None),
        }
    }
}

#[tonic::async_trait]
impl MachineRpc for MachineService {
    type ExecStream = RpcStream;
    type BuildStream = RpcStream;
    type ContainerLogsStream = RpcStream;
    type MachineLogsStream = RpcStream;
    type RuntimeWatchStream = RuntimeWatchStream;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::DescribeContract>(request)?;
        let machine_id = self.local_record()?.id();
        let mut capabilities: BTreeSet<_> =
            CapabilityAdvertisement::Always.capabilities().collect();
        if self.local.containers().is_some() {
            capabilities.extend(CapabilityAdvertisement::Container.capabilities());
        }
        if self.ingress_data_dir.is_some() {
            capabilities.extend(CapabilityAdvertisement::Ingress.capabilities());
        }
        if self.local.has_cluster() {
            capabilities.extend(CapabilityAdvertisement::Cluster.capabilities());
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
        finish(
            self.local
                .initialize(expect::<op::Initialize>(request)?)
                .await,
        )
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let forwarded = request
            .metadata()
            .get(REGISTER_FORWARDED_METADATA)
            .is_some();
        let payload = request.into_inner();
        let decoded = op::Register::from_request_body(
            payload.decode_request().map_err(invalid_request)?.body,
        )
        .map_err(invalid_request)?;
        if forwarded {
            return finish(self.local.register(decoded).await);
        }
        match self.local.register(decoded).await {
            Err(LocalMachineError::NotAllocator) => self.forward_register(payload).await,
            other => finish(other),
        }
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.join(expect::<op::Join>(request)?).await)
    }

    async fn set_cloud_pairing(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::SetCloudPairing>(request)?;
        if let Err(error) = self
            .local
            .set_cloud_pairing(request.cloud_pairing.clone())
            .await
        {
            return local_error(error);
        }
        if let Some(sender) = &self.cloud_pairing {
            sender.send_replace(request.cloud_pairing);
        }
        respond(CloudPairingSet {})
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
        let machine_id = self.local_record()?.id();
        match containers.list_managed(&machine_id).await {
            Ok(observations) => respond(ContainerList {
                containers: observations,
            }),
            Err(error) => respond(RpcError::from(&error)),
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
        let machine_id = self.local_record()?.id();
        match containers
            .inspect_managed_details(&request.container_id, &machine_id)
            .await
        {
            Ok(details) => respond(details),
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn get_container_observations(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::GetContainerObservations>(request)?;
        let wait = Duration::from_millis(request.wait_millis);
        if wait > MAX_CONTAINER_OBSERVATION_WAIT {
            return respond(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "container observation wait exceeds 5 seconds".into(),
                details: Value::Null,
            });
        }
        let replicated = match self.replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        let response = container_observations(replicated, &request.container_ids, wait).await;
        match response {
            Ok(response) => respond(response),
            Err(error) => respond(cluster_store_error(error)),
        }
    }

    async fn create_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::CreateContainer>(request)?;
        finish(
            self.local
                .create_container(request.kind, &request.project_name, &request.resolved_spec)
                .await,
        )
    }

    async fn ensure_global_slot(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::EnsureGlobalSlot>(request)?;
        let result = self
            .local
            .converge_global_slot(&request.project_name, &request.resolved_spec)
            .await
            .and_then(|outcome| outcome.into_container().map_err(Into::into));
        finish(result)
    }

    async fn start_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::StartContainer>(request)?;
        finish(self.local.start_container(request.container_id).await)
    }

    async fn stop_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::StopContainer>(request)?;
        finish(
            self.local
                .stop_container(
                    request.container_id,
                    request.signal,
                    request.grace_period_seconds,
                )
                .await,
        )
    }

    async fn remove_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveContainer>(request)?;
        finish(
            self.local
                .remove_container(request.container_id, request.remove_volumes, request.force)
                .await,
        )
    }

    async fn create_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::CreateVolume>(request)?;
        finish(self.local.create_volume(request).await)
    }

    async fn inspect_storage(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::InspectStorage>(request)?;
        match crate::storage::plugin::<ployz_core::StorageCapacity>("Storage.Inspect", &()).await {
            Ok(capacity) => respond(capacity),
            Err(error) => respond(error),
        }
    }

    async fn prepare_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::PrepareVolumes>(request)?;
        finish(self.local.prepare_volumes(request.specs).await)
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::ListVolumes>(request)?;
        let machine_id = self.local_record()?.id();
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.list_volumes(&machine_id).await {
            Ok(inventory) => respond(inventory),
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn inspect_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::InspectVolume>(request)?;
        let machine_id = self.local_record()?.id();
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.inspect_volume(&machine_id, &request.name).await {
            Ok(volume) => respond(volume),
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::RemoveVolume>(request)?;
        finish(self.local.remove_volume(request.name, request.force).await)
    }

    async fn build(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        self.containers()
            .map_err(|error| Status::unavailable(error.message))?;
        Ok(Response::new(crate::build::start(
            self.local.clone(),
            request.into_inner(),
            self.builds.clone(),
        )))
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
            .machine()
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        containers
            .container_logs(&record.id(), &machine.name, request)
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
            .machine()
            .cloned()
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        let metadata = LogMetadata {
            origin: LogOrigin::Machine {
                service: request.service,
            },
            machine_id: record.id(),
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

    async fn runtime_watch(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        op::RuntimeWatch::from_request_body(request_body(request)?).map_err(invalid_request)?;
        let store = self
            .ready_replicated()
            .map_err(|error| Status::unavailable(error.message))?
            .clone();
        let entry_id = self.local_record()?.id();
        let stream = self
            .runtime_watch
            .subscribe(store, self.local.clone(), entry_id)
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        Ok(Response::new(stream))
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

    async fn request_machine_upgrade(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .request_upgrade(expect::<op::RequestMachineUpgrade>(request)?)
                .await,
        )
    }

    async fn inspect_machine_upgrade(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .inspect_upgrade(expect::<op::InspectMachineUpgrade>(request)?)
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

    async fn ensure_image_ingest(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::EnsureImageIngest>(request)?;
        finish(
            self.local
                .ensure_image_ingest(Arc::clone(&self.ingest))
                .await,
        )
    }

    async fn pull_image_from_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::PullImageFromMachine>(request)?;
        if request.pull.image().is_empty() {
            return respond(RpcError {
                code: RpcErrorCode::InvalidArgument,
                message: "image is required".into(),
                details: Value::Null,
            });
        }
        finish(self.local.pull_image_from_machine(request).await)
    }

    async fn get_ingress_proxy_config(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::GetIngressProxyConfig>(request)?;
        if let Err(error) = self.ready_replicated() {
            return respond(error);
        }
        let Some(data_dir) = &self.ingress_data_dir else {
            return respond(unavailable("Ingress Proxy configuration is not available"));
        };
        let path = crate::ingress::caddy::config_path(data_dir);
        let generated = std::fs::read_to_string(&path);
        match generated {
            Ok(config) => respond(IngressProxyConfig { config }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                respond(ingress_config_missing(&path))
            }
            Err(error) => Err(Status::internal(format!(
                "read Ingress Proxy configuration {}: {error}",
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

async fn container_observations(
    replicated: &ReplicatedStore,
    container_ids: &[ployz_core::ContainerId],
    wait: Duration,
) -> Result<ContainerObservationMap, crate::corrosion::Error> {
    if wait.is_zero() || container_ids.is_empty() {
        return read_container_observations(replicated, container_ids).await;
    }
    let mut changes = replicated.subscribe_container_changes().await?;
    let initial = read_container_observations(replicated, container_ids).await?;
    let deadline = Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, changes.changed()).await {
            Ok(Ok(())) => {
                let current = read_container_observations(replicated, container_ids).await?;
                if current != initial {
                    return Ok(current);
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => return read_container_observations(replicated, container_ids).await,
        }
    }
}

async fn read_container_observations(
    replicated: &ReplicatedStore,
    container_ids: &[ployz_core::ContainerId],
) -> Result<ContainerObservationMap, crate::corrosion::Error> {
    let mut containers = BTreeMap::new();
    for container_id in container_ids {
        containers.insert(*container_id, replicated.container(container_id).await?);
    }
    Ok(ContainerObservationMap { containers })
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
        LocalMachineError::KeyAlreadyNamed
        | LocalMachineError::NameTaken
        | LocalMachineError::InitialPolicyMismatch => respond(RpcError {
            code: RpcErrorCode::Conflict,
            message: error.to_string(),
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
        LocalMachineError::OperationTask(error) => Err(Status::internal(error.to_string())),
        LocalMachineError::Cluster(error) => Err(Status::internal(error.to_string())),
        LocalMachineError::IngressProxyServiceSpec(error) => respond(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: error.to_string(),
            details: Value::Null,
        }),
        LocalMachineError::Network(error) => Err(Status::internal(error.to_string())),
        LocalMachineError::Docker(error) => respond(RpcError::from(&error)),
        LocalMachineError::StoragePreparation(error) => respond(error),
        LocalMachineError::Cleanup(message) => respond(RpcError {
            code: RpcErrorCode::Internal,
            message,
            details: Value::Null,
        }),
        LocalMachineError::AllocatorNotQuiet
        | LocalMachineError::NotAllocator
        | LocalMachineError::IsolationLocked => respond(unavailable(&error.to_string())),
        LocalMachineError::Admission(crate::mutation::Error::Busy) => respond(RpcError {
            code: RpcErrorCode::Conflict,
            message: "a Ployz installation or upgrade is active".into(),
            details: Value::Null,
        }),
        LocalMachineError::Admission(crate::mutation::Error::Io(error)) => {
            Err(Status::internal(error.to_string()))
        }
        LocalMachineError::Upgrade(error) => respond(upgrade_error(error)),
    }
}

fn upgrade_error(error: crate::installer::upgrade::Error) -> RpcError {
    use crate::{installer::upgrade::Error, mutation};
    let code = match &error {
        Error::NotFound => RpcErrorCode::NotFound,
        Error::AttemptConflict(_) | Error::Busy | Error::Admission(mutation::Error::Busy) => {
            RpcErrorCode::Conflict
        }
        Error::Resolve(_) | Error::NonstandardPaths(_) => RpcErrorCode::InvalidArgument,
        Error::Read(_)
        | Error::Decode(_)
        | Error::Write(_)
        | Error::Encode(_)
        | Error::QualificationSource(_)
        | Error::Launch(_)
        | Error::InspectWorker(_)
        | Error::WorkerEvidence(_)
        | Error::NotActive(_)
        | Error::Installation(_)
        | Error::Admission(mutation::Error::Io(_)) => RpcErrorCode::Internal,
    };
    RpcError {
        code,
        message: error.to_string(),
        details: Value::Null,
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

/// The generated Ingress Proxy configuration is absent; `details.path` names the file.
fn ingress_config_missing(path: &Path) -> RpcError {
    RpcError {
        code: RpcErrorCode::NotFound,
        message: format!(
            "Ingress Proxy configuration {} does not exist",
            path.display()
        ),
        details: serde_json::json!({ "path": path.display().to_string() }),
    }
}

fn hosted_dns_error(error: crate::hosted_dns::Error) -> RpcError {
    use crate::hosted_dns::Error;
    let (code, details) = match error {
        Error::AlreadyReserved => (RpcErrorCode::Conflict, Value::Null),
        Error::NotFound => (RpcErrorCode::NotFound, Value::Null),
        Error::AuthNoDomain => (
            RpcErrorCode::Unauthenticated,
            serde_json::json!({ "no_domain": true }),
        ),
        Error::Authentication => (RpcErrorCode::Unauthenticated, Value::Null),
        Error::InvalidEndpoint(_) => (RpcErrorCode::InvalidArgument, Value::Null),
        // InvalidReservation is an unusable upstream response, not caller input; a
        // malformed stored record surfaces as corrosion::InvalidDomainReservation instead.
        Error::Http(_) | Error::Json(_) | Error::InvalidReservation(_) => {
            (RpcErrorCode::Unavailable, Value::Null)
        }
        // Hosted DNS speaks HTTP; its status class is the taxonomy kind.
        Error::Status(status, _) => (
            match status {
                403 => RpcErrorCode::Unauthenticated,
                404 => RpcErrorCode::NotFound,
                409 => RpcErrorCode::Conflict,
                _ => RpcErrorCode::Unavailable,
            },
            serde_json::json!({ "status": status }),
        ),
        Error::Store(_) | Error::InvalidReservationCleared => (RpcErrorCode::Internal, Value::Null),
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
        | StoreError::NotParticipating
        | StoreError::NotResetting
        | StoreError::NotJoining
        | StoreError::AlreadyRunning(_) => RpcErrorCode::Conflict,
        StoreError::MissingEndpoints
        | StoreError::MissingPeers
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

fn cluster_store_error(error: crate::corrosion::Error) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
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
#[path = "local_tests.rs"]
mod tests;
