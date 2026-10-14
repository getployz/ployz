//! Local Machine RPC. Listeners take [`super::MachineApi`], not this type.

use std::{collections::BTreeSet, path::PathBuf, sync::Arc};

use ployz_core::{
    CapabilityAdvertisement, CertificateMaterialChange, CertificateMaterialPublished,
    ContainerList, ContainerObservation, ContractDescription, EnvironmentValues, LiveLogTarget,
    LocalMachinePhase, LogMetadata, LogOrigin, MachineLogService, MachineRpc, NotPointerPublisher,
    OpaquePayload, PROTOCOL_MAJOR, Rpc, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    ServingPointerRead, op,
};
use serde_json::Value;
use tonic::{Request, Response, Status};

use crate::{
    corrosion::{AdminClient, CertificateMaterial, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    logs::{RpcStream, open_journal_logs, serve_logs},
    machine::{LocalMachine, LocalMachineError, LocalMachineRecord, RecordOwner, StoreError},
    network::MACHINE_API_PORT,
    runtime_watch::{RuntimeWatch, RuntimeWatchStream},
};

const DEFAULT_OBSERVE_SOCKET: &str = "/run/ployz/observe.sock";

#[derive(Clone)]
pub struct MachineService {
    local: LocalMachine,
    observe: crate::observe::ObserveClient,
    ingest: Arc<ImageIngest>,
    machine_api_port: u16,
    runtime_watch: Arc<RuntimeWatch>,
    switch_mutation: Arc<tokio::sync::Mutex<()>>,
    pub(crate) builds: Arc<crate::build::Runner>,
    pub(crate) grants: Arc<crate::management::BuildGrants>,
}

impl MachineService {
    #[must_use]
    pub fn with_cluster(
        owner: RecordOwner,
        cluster: Option<(ReplicatedStore, AdminClient)>,
    ) -> Self {
        Self {
            local: LocalMachine::new(owner).with_cluster(cluster),
            observe: crate::observe::ObserveClient::new(PathBuf::from(DEFAULT_OBSERVE_SOCKET)),
            ingest: ImageIngest::new(None, None),
            machine_api_port: MACHINE_API_PORT,
            runtime_watch: Arc::default(),
            switch_mutation: Arc::default(),
            builds: crate::build::Runner::new(Default::default(), Default::default())
                .expect("default Build policy"),
            grants: Arc::default(),
        }
    }

    pub(super) fn with_management_client(mut self, remote: [u8; 32]) -> Self {
        self.local = self.local.with_management_client(remote);
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
    pub(crate) fn with_volume_plugin(mut self, plugin: crate::storage::Plugin) -> Self {
        self.local = self.local.with_plugin(plugin);
        self
    }

    /// Serve history from the Log Store whose ployz-observe listens on `socket`.
    #[must_use]
    pub fn with_observe_socket(mut self, socket: PathBuf) -> Self {
        self.observe = crate::observe::ObserveClient::new(socket);
        self
    }

    /// Image ingest started on first Ensure and stopped with the daemon.
    #[must_use]
    pub fn with_image_ingest(mut self, ingest: Arc<ImageIngest>) -> Self {
        self.ingest = ingest;
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

    pub(super) fn machine_api_port(&self) -> u16 {
        self.machine_api_port
    }

    fn local_record(&self) -> Arc<LocalMachineRecord> {
        self.local.record()
    }

    fn replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        self.local
            .replicated()
            .map_err(|_| unavailable("Cluster store is not available"))
    }

    fn ready_replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        if self.local_record().phase() != LocalMachinePhase::Participating {
            return Err(unavailable("Machine is not participating"));
        }
        self.replicated()
    }

    async fn pointer_replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        let replicated = self.ready_replicated()?;
        let publisher = match self.local.pointer_publisher().await {
            Ok(publisher) => publisher,
            Err(LocalMachineError::Cluster(error)) => return Err(cluster_store_error(error)),
            Err(
                error @ (LocalMachineError::UnreadableLowerMachine(_)
                | LocalMachineError::MissingPeerRow(_)),
            ) => {
                return Err(RpcError::caused(RpcErrorCode::Unavailable, &error));
            }
            Err(_) => return Err(unavailable("Cluster store is not available")),
        };
        if publisher != self.local_record().id() {
            return Err(NotPointerPublisher { publisher }.rpc_error());
        }
        Ok(replicated)
    }

    fn containers(&self) -> Result<&ContainerRuntime, RpcError> {
        self.local
            .containers()
            .ok_or_else(|| unavailable("Docker is not available"))
    }

    fn require_joined(&self) -> Result<(), RpcError> {
        match self.local_record().phase() {
            LocalMachinePhase::Joining | LocalMachinePhase::Participating => Ok(()),
            LocalMachinePhase::Uninitialized
            | LocalMachinePhase::Resetting
            | LocalMachinePhase::Unrecognized(_) => {
                Err(unavailable("Machine is not participating"))
            }
        }
    }

    #[allow(clippy::result_large_err)]
    async fn switch_verb<T: serde::de::DeserializeOwned + Into<RpcResponse>>(
        &self,
        verb: &'static str,
        route: &str,
        request: &impl serde::Serialize,
    ) -> Result<Response<OpaquePayload>, Status> {
        let _guard = self.switch_mutation.lock().await;
        self.volume_verb::<T>(verb, route, request).await
    }

    /// A Volume verb that takes no switch lock. A read may land beside a cut's drain: the
    /// write that follows it rechecks the copy under the lock, so a stale view ends in a
    /// refusal, never a wrong flip.
    #[allow(clippy::result_large_err)]
    async fn volume_verb<T: serde::de::DeserializeOwned + Into<RpcResponse>>(
        &self,
        verb: &'static str,
        route: &str,
        request: &impl serde::Serialize,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Err(error) = self.require_joined() {
            return respond(error);
        }
        crate::faults::apply(verb).await;
        match self.local.plugin().call::<T>(route, request).await {
            Ok(reply) => respond(reply),
            Err(error) => respond(error),
        }
    }
}

#[tonic::async_trait]
impl MachineRpc for MachineService {
    type ExecStream = RpcStream;
    type BuildStream = RpcStream;
    type LogHistoryStream = RpcStream;
    type TailLogsStream = RpcStream;
    type RuntimeWatchStream = RuntimeWatchStream;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::DescribeContract>(request)?;
        let machine_id = self.local_record().id();
        let mut capabilities: BTreeSet<_> =
            CapabilityAdvertisement::Always.capabilities().collect();
        if self.local.containers().is_some() {
            capabilities.extend(CapabilityAdvertisement::Container.capabilities());
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

    async fn enroll(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.enroll(expect::<op::Enroll>(request)?).await)
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.register(expect::<op::Register>(request)?).await)
    }

    async fn set_management_client(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::SetManagementClient>(request)?;
        finish(self.local.set_management_client(request).await)
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
        let request = expect::<op::ListContainers>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let machine_id = self.local_record().id();
        match containers.list_managed(&machine_id).await {
            Ok(mut observations) => {
                if request.environment == EnvironmentValues::Redacted {
                    observations
                        .iter_mut()
                        .for_each(ContainerObservation::redact_values);
                }
                respond(ContainerList {
                    containers: observations,
                })
            }
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
        let machine_id = self.local_record().id();
        match containers
            .inspect_managed_details(&request.container_id, &machine_id)
            .await
        {
            Ok(details) => respond(details),
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn ensure_container(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::EnsureContainer>(request)?;
        finish(self.local.ensure_container(request.want).await)
    }

    async fn inspect_storage(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        expect::<op::InspectStorage>(request)?;
        match self
            .local
            .plugin()
            .call::<ployz_core::StorageCapacity>("Storage.Inspect", &())
            .await
        {
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
        let machine_id = self.local_record().id();
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.list_volumes(&machine_id).await {
            Ok(inventory) => respond(inventory),
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

    async fn inspect_volume_copy(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::InspectVolumeCopy>(request)?;
        self.volume_verb::<ployz_core::VolumeCopyView>(
            "InspectVolumeCopy",
            "Volume.Inspect",
            &request,
        )
        .await
    }

    async fn set_volume_copy(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.set_copy(request).await
    }

    async fn seed_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::SeedVolume>(request)?;
        self.switch_verb::<ployz_core::VolumeCopyView>("SeedVolume", "Volume.Seed", &request)
            .await
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

    async fn log_history(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::LogHistoryStream>, Status> {
        let request =
            op::LogHistory::from_request_body(request_body(request)?).map_err(invalid_request)?;
        request.validate().map_err(Status::invalid_argument)?;
        self.observe.history(request).await.map(Response::new)
    }

    async fn metric_history(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::MetricHistory>(request)?;
        request.validate().map_err(Status::invalid_argument)?;
        respond(self.observe.metrics(request).await?)
    }

    async fn tail_logs(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::TailLogsStream>, Status> {
        let request =
            op::TailLogs::from_request_body(request_body(request)?).map_err(invalid_request)?;
        let record = self.local_record();
        let machine = record
            .machine()
            .ok_or_else(|| Status::unavailable("Machine is not participating"))?;
        let stream = match request.target {
            LiveLogTarget::Container(container_id) => {
                self.containers()
                    .map_err(|error| Status::unavailable(error.message))?
                    .container_logs(&record.id(), &machine.name, container_id, &request.options)
                    .await?
            }
            LiveLogTarget::Machine(service) => {
                let source = match service {
                    MachineLogService::Ployz | MachineLogService::Docker => {
                        open_journal_logs(service.as_str(), &request.options).await?
                    }
                    MachineLogService::Corrosion => self
                        .containers()
                        .map_err(|error| Status::unavailable(error.message))?
                        .raw_logs(crate::corrosion::DEFAULT_CONTAINER_NAME, &request.options)?,
                };
                let metadata = LogMetadata {
                    origin: LogOrigin::Machine { service },
                    machine_id: record.id(),
                    machine_name: machine.name.clone(),
                };
                serve_logs(source, metadata, request.options.follow)
            }
        };
        Ok(Response::new(stream))
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
        let entry_id = self.local_record().id();
        let stream = self
            .runtime_watch
            .subscribe(store, self.local.clone(), entry_id)
            .await
            .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Unavailable, &error))?;
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

    async fn leave(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.leave(expect::<op::Leave>(request)?).await)
    }

    async fn forget(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(self.local.forget(expect::<op::Forget>(request)?).await)
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
            .list_images(request.reference.as_deref(), request.last_tagged)
            .await
            .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Internal, &error))?;
        respond(images)
    }

    async fn prune_images(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        finish(
            self.local
                .prune_images(expect::<op::PruneImages>(request)?)
                .await,
        )
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
                cause: Vec::new(),
            });
        }
        finish(self.local.pull_image_from_machine(request).await)
    }

    async fn mint_build_grant(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::MintBuildGrant>(request)?;
        match crate::management::build_grant::mint_for(
            &self.grants,
            &self.local,
            Arc::clone(&self.ingest),
            request,
        )
        .await
        {
            Ok(minted) => respond(minted),
            Err(error) => local_error(error),
        }
    }

    async fn inspect_build_grant(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::InspectBuildGrant>(request)?;
        match self.grants.inspect(&request.id) {
            Ok(inspected) => respond(inspected),
            Err(error) => respond(error),
        }
    }

    async fn publish_certificate_material(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::PublishCertificateMaterial>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        let hostname = request.hostname;
        let result = match request.change {
            CertificateMaterialChange::Set {
                certificate_chain_pem,
                private_key_pem,
            } => {
                let material = CertificateMaterial::parse(certificate_chain_pem, private_key_pem)
                    .and_then(|material| material.covering(&hostname));
                let material = match material {
                    Ok(material) => material,
                    Err(error) => {
                        return respond(RpcError {
                            code: RpcErrorCode::InvalidArgument,
                            message: format!(
                                "refused Certificate Material for {hostname}: {error}",
                                error = ployz_core::error_chain::inline(&error),
                            ),
                            details: Value::Null,
                            cause: Vec::new(),
                        });
                    }
                };
                replicated
                    .publish_certificate_material(&hostname, material)
                    .await
            }
            CertificateMaterialChange::Clear => {
                replicated.clear_published_certificate(&hostname).await
            }
        };
        match result {
            Ok(()) => respond(CertificateMaterialPublished {}),
            Err(error) => respond(cluster_store_error(error)),
        }
    }

    async fn publish_serving_pointer(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::PublishServingPointer>(request)?;
        let replicated = match self.pointer_replicated().await {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match replicated
            .compare_and_set_serving_pointer(
                &request.service,
                request.read.as_ref(),
                &request.pointer,
            )
            .await
        {
            Ok(publication) => respond(publication),
            Err(error) => respond(cluster_store_error(error)),
        }
    }

    async fn read_serving_pointer(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = expect::<op::ReadServingPointer>(request)?;
        let replicated = match self.pointer_replicated().await {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match replicated.serving_pointer(&request.service).await {
            Ok(pointer) => respond(ServingPointerRead { pointer }),
            Err(error) => respond(cluster_store_error(error)),
        }
    }
}

#[allow(clippy::result_large_err)]
fn local_error(error: LocalMachineError) -> Result<Response<OpaquePayload>, Status> {
    match error {
        LocalMachineError::Store(error) => respond(store_error(error)),
        LocalMachineError::ManagementRevoked => Err(ployz_core::rpc::caused_status(
            tonic::Code::Unauthenticated,
            &error,
        )),
        LocalMachineError::NotParticipating => respond(unavailable("Machine is not participating")),
        LocalMachineError::ClusterStoreUnavailable => {
            respond(unavailable("Cluster store is not available"))
        }
        LocalMachineError::ClusterUnavailable => {
            Err(Status::unavailable("Cluster is not available"))
        }
        LocalMachineError::DockerUnavailable => respond(unavailable("Docker is not available")),
        LocalMachineError::UnreadableLowerMachine(_) | LocalMachineError::MissingPeerRow(_) => {
            respond(RpcError::caused(RpcErrorCode::Unavailable, &error))
        }
        LocalMachineError::MissingAssignment => {
            respond(RpcError::caused(RpcErrorCode::InvalidArgument, &error))
        }
        LocalMachineError::Enrollment(error) => {
            respond(RpcError::caused(RpcErrorCode::Conflict, &error))
        }
        LocalMachineError::EmptyUpdate => respond(RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: "at least one Machine update is required".into(),
            details: Value::Null,
            cause: Vec::new(),
        }),
        LocalMachineError::RecordOwner(error) => Err(ployz_core::rpc::caused_status(
            tonic::Code::Internal,
            &error,
        )),
        LocalMachineError::OperationTask(error) => Err(ployz_core::rpc::caused_status(
            tonic::Code::Internal,
            &error,
        )),
        LocalMachineError::Cluster(error) => Err(ployz_core::rpc::caused_status(
            tonic::Code::Internal,
            &error,
        )),
        LocalMachineError::IngressProxyServiceSpec(error) => {
            respond(RpcError::caused(RpcErrorCode::InvalidArgument, &error))
        }
        LocalMachineError::Network(error) => Err(ployz_core::rpc::caused_status(
            tonic::Code::Internal,
            &error,
        )),
        LocalMachineError::Docker(error) => respond(RpcError::from(&error)),
        LocalMachineError::StoragePreparation(error) => respond(error),
        LocalMachineError::Cleanup(message) => respond(RpcError {
            code: RpcErrorCode::Internal,
            message,
            details: Value::Null,
            cause: Vec::new(),
        }),
        LocalMachineError::Admission(crate::mutation::Error::Busy) => respond(RpcError {
            code: RpcErrorCode::Conflict,
            message: "a Ployz installation or upgrade is active".into(),
            details: Value::Null,
            cause: Vec::new(),
        }),
        LocalMachineError::Admission(crate::mutation::Error::Io(error)) => Err(
            ployz_core::rpc::caused_status(tonic::Code::Internal, &error),
        ),
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
        | Error::Launch(_)
        | Error::InspectWorker(_)
        | Error::WorkerEvidence(_)
        | Error::NotActive(_)
        | Error::Upgrade(_)
        | Error::Admission(mutation::Error::Io(_)) => RpcErrorCode::Internal,
    };
    RpcError::caused(code, &error)
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
        cause: Vec::new(),
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
        | StoreError::IdentityMismatch
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
        | StoreError::ResetPreparationLost(_)
        | StoreError::ResetInterrupted(_) => RpcErrorCode::Internal,
    };
    RpcError::caused(code, &error)
}

fn cluster_store_error(error: crate::corrosion::Error) -> RpcError {
    RpcError::caused(RpcErrorCode::Internal, &error)
}

#[allow(clippy::result_large_err)]
fn respond(response: impl Into<RpcResponse>) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(
        response.into().encode().map_err(internal_response)?,
    ))
}

fn invalid_request(error: impl std::error::Error + 'static) -> Status {
    ployz_core::rpc::caused_status(tonic::Code::InvalidArgument, &error)
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

fn internal_response(error: impl std::error::Error + 'static) -> Status {
    ployz_core::rpc::caused_status(tonic::Code::Internal, &error)
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
#[path = "local_volume_copy.rs"]
mod volume_copy;
