//! Local Machine RPC. Listeners take [`super::MachineApi`], not this type.
#![expect(
    clippy::result_large_err,
    reason = "a handler fails with tonic::Status, the RPC error type"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ployz_core::{
    CapabilityAdvertisement, CertificateMaterialChange, CertificateMaterialPublished, ClientKey,
    ClusterDoorsDeclared, ClusterNamed, ContainerId, ContainerList, ContainerObservation,
    ContractDescription, EndpointSelected, EnvironmentValues, Evidence, Gate, GateReason,
    LiveLogTarget, LocalMachinePhase, LogMetadata, LogOrigin, MachineLogService, MachineRpc,
    OpaquePayload, PROTOCOL_MAJOR, RoutedAddresses, RoutesRead, Rpc, RpcError, RpcErrorCode,
    RpcRequestBody, RpcResponse, SelectEndpointRequest, metrics::MetricsLiveReply, op,
};
use tonic::{Request, Response, Status};

use super::provenance::Provenance;
use crate::{
    corrosion::{CertificateMaterial, ReplicatedStore},
    docker::{ContainerRuntime, ImageIngest},
    logs::{RpcStream, open_journal_logs, serve_logs},
    machine::{
        AdmittedSpec, Caller, LocalMachine, LocalMachineError, LocalMachineRecord, RecoveryError,
        Scope, SealingError, StoreError,
    },
    management::build_grant::Minter,
    network::{ChoiceRefused, EndpointChoice, MACHINE_API_PORT},
    route_acks::{Ackers, routed},
    runtime_watch::{RuntimeWatch, RuntimeWatchStream},
    sealed_env::SealedRecord,
};

#[derive(Clone)]
pub struct MachineService {
    local: LocalMachine,
    ingest: Arc<ImageIngest>,
    machine_api_port: u16,
    runtime_watch: Arc<RuntimeWatch>,
    pub(crate) builds: Arc<crate::build::Runner>,
    pub(crate) grants: Arc<crate::management::BuildGrants>,
    pub(crate) endpoint_chooser: Option<crate::network::EndpointChooser>,
    pub(crate) published_routes: tokio::sync::watch::Receiver<crate::ingress::PublishedRoutes>,
    /// This Machine's observation of its mesh path to each peer.
    pub(crate) paths: crate::network::PathBook,
    /// GitHub as a test fixes it, in place of `api.github.com` and its issuer.
    #[cfg(test)]
    pub(crate) github: Option<Arc<crate::machine::local_machine::rule_grant::FixedGithub>>,
}

impl MachineService {
    /// Run every check of a rule build's grant against GitHub, or `None` when this daemon
    /// cannot reach GitHub at all.
    async fn admit_rule_grant(
        &self,
        grant: &ployz_core::RuleGrant,
        holder: Option<&ployz_core::BuildGrantId>,
        sealed: Option<&ployz_core::SealedRecord>,
    ) -> Option<Result<crate::machine::local_machine::rule_grant::AdmittedRule, LocalMachineError>>
    {
        let now = ployz_core::authority::Timestamp::now();
        #[cfg(test)]
        if let Some(github) = &self.github {
            return Some(
                self.local
                    .admit_rule_grant(grant, holder, sealed, github.as_ref(), github.now(now))
                    .await,
            );
        }
        let github = crate::machine::local_machine::rule_grant::GithubOnline::shared()?;
        Some(
            self.local
                .admit_rule_grant(grant, holder, sealed, github, now)
                .await,
        )
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_cluster(
        owner: crate::machine::RecordOwner,
        cluster: Option<(ReplicatedStore, crate::corrosion::AdminClient)>,
    ) -> Self {
        Self::over(LocalMachine::new(owner).with_cluster(cluster))
    }

    /// A Machine API service over `local`, with the defaults a builder starts from.
    #[must_use]
    pub fn over(local: LocalMachine) -> Self {
        Self {
            local,
            ingest: ImageIngest::new(None, None),
            machine_api_port: MACHINE_API_PORT,
            runtime_watch: Arc::default(),
            builds: crate::build::Runner::new(Default::default(), Default::default())
                .expect("default Build policy"),
            grants: Arc::default(),
            endpoint_chooser: None,
            paths: crate::network::PathBook::default(),
            #[cfg(test)]
            github: None,
            published_routes: tokio::sync::watch::channel(
                crate::ingress::PublishedRoutes::default(),
            )
            .1,
        }
    }

    /// Serve the calls of the management connection `remote` authenticated. Until a call's
    /// own marks say otherwise, its work is an Operator's that Cloud routed.
    pub(super) fn with_management_client(mut self, remote: ClientKey) -> Self {
        self.local = self.local.at(crate::machine::Call::new(
            Caller::Management(remote),
            ployz_core::ManagementScope::Operator,
            true,
        ));
        self
    }

    /// This Machine serving `request`: at the scope and Cloud routing its edge marked, from
    /// this Server's local socket when it came over it, else from the connection this
    /// service serves. A routed call reaches its target over the mesh, so only the Server
    /// the caller sits at sees its local socket.
    fn at<T>(&self, request: &Request<T>) -> LocalMachine {
        let caller = if request
            .extensions()
            .get::<tonic::transport::server::UdsConnectInfo>()
            .is_some()
        {
            Caller::LocalSocket
        } else {
            self.local.call().caller()
        };
        self.local.clone().at(Provenance::of(request).call(caller))
    }

    /// Serve through `local` itself, so everything else cloned from it shares its state.
    #[must_use]
    pub(super) fn with_local(mut self, local: LocalMachine) -> Self {
        self.local = local;
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_optional_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.local = self.local.with_containers(containers);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_volume_plugin(mut self, plugin: crate::storage::Plugin) -> Self {
        self.local = self.local.with_plugin(plugin);
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

    /// An Operator's request to write an image tag here, refused before anything is
    /// pulled: a Full creation runs whatever a tag names, with the Service's Sealed
    /// values and Volumes, so only a caller with Full access here writes one.
    fn replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        self.local
            .replicated()
            .map_err(|_| RpcError::unavailable("Cluster store is not available"))
    }

    fn ready_replicated(&self) -> Result<&ReplicatedStore, RpcError> {
        if self.local_record().phase() != LocalMachinePhase::Participating {
            return Err(RpcError::unavailable("Machine is not participating"));
        }
        self.replicated()
    }

    /// Hand an operator's endpoint for a peer to this Machine's WireGuard. Only this
    /// Machine's own route changes; the peer's record does not.
    async fn select(&self, request: SelectEndpointRequest) -> Result<EndpointSelected, RpcError> {
        let SelectEndpointRequest { peer, endpoint } = request;
        if peer == self.local_record().id() {
            return Err(RpcError::invalid_argument(
                "a Server does not choose an endpoint for itself",
            ));
        }
        let chooser = self
            .endpoint_chooser
            .as_ref()
            .ok_or_else(|| RpcError::unavailable("WireGuard is not running on this Server"))?;
        let (applied, outcome) = tokio::sync::oneshot::channel();
        chooser
            .send(EndpointChoice {
                peer,
                endpoint,
                applied,
            })
            .await
            .map_err(|_| RpcError::unavailable("WireGuard is not running on this Server"))?;
        match outcome.await {
            Ok(Ok(())) => Ok(EndpointSelected { peer, endpoint }),
            Ok(Err(refused @ ChoiceRefused::UnknownPeer(_))) => {
                Err(RpcError::caused(RpcErrorCode::NotFound, &refused))
            }
            Ok(Err(refused @ ChoiceRefused::WireGuard)) => {
                Err(RpcError::caused(RpcErrorCode::Internal, &refused))
            }
            Err(_) => Err(RpcError::unavailable(
                "WireGuard is not running on this Server",
            )),
        }
    }

    async fn sealed_record(&self, id: ContainerId) -> Result<Option<SealedRecord>, Status> {
        self.containers()
            .map_err(|error| Status::unavailable(error.message))?
            .observation()
            .sealed_record(&id)
            .await
            .map_err(|error| Status::unavailable(ployz_core::error_chain::inline(&error)))
    }

    async fn opened_keys(&self, id: ContainerId) -> Result<BTreeSet<String>, Status> {
        Ok(self
            .sealed_record(id)
            .await?
            .iter()
            .flat_map(SealedRecord::keys)
            .map(str::to_owned)
            .collect())
    }

    fn containers(&self) -> Result<&ContainerRuntime, RpcError> {
        self.local
            .containers()
            .ok_or_else(|| RpcError::unavailable("Docker is not available"))
    }
}

/// What a unary handler answers.
type Answer = Result<Response<OpaquePayload>, Status>;

macro_rules! catalog_ops {
    (package $package:literal unary { $($variant:ident: ($method:ident, $($row:tt)*),)+ } $($rest:tt)*) => {
        /// The catalog's op for a unary method.
        macro_rules! op_of {
            $(($method) => { op::$variant };)+
        }
    };
}
ployz_core::rpc_catalog!(catalog_ops);

/// Handlers that decode the request, pass it to the [`LocalMachine`] method of the same
/// name, and answer its result. Written as `async_trait` expands
/// an `async fn`, because the attribute leaves a macro in the impl unexpanded.
macro_rules! pass_through {
    ($($method:ident,)+) => {
        $(
            fn $method<'life0, 'async_trait>(
                &'life0 self,
                request: Request<OpaquePayload>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Answer> + Send + 'async_trait>>
            where
                'life0: 'async_trait,
                Self: 'async_trait,
            {
                Box::pin(async move {
                    let local = self.at(&request);
                    let request = expect::<op_of!($method)>(request)?;
                    finish(local.$method(request).await)
                })
            }
        )+
    };
}

#[tonic::async_trait]
impl MachineRpc for MachineService {
    type ExecStream = RpcStream;
    type BuildStream = RpcStream;
    type BuildFreeStream = RpcStream;
    type TailLogsStream = RpcStream;
    type RuntimeWatchStream = RuntimeWatchStream;

    pass_through! {
        enroll,
        enroll_for_recovery,
        recover_sealing_key,
        approve_join,
        register,
        set_management_client,
        set_device,
        key_step,
        set_key,
        forget_versions,
        set_push_target,
        update_machine,
        share_sealing_key,
        request_machine_upgrade,
        leave,
        forget,
        prune_images,
        list_kept_volumes,
        remove_kept_volumes,
        set_ingress_doors,
        set_private_access,
    }

    async fn describe_contract(&self, request: Request<OpaquePayload>) -> Answer {
        expect::<op::DescribeContract>(request)?;
        let record = self.local_record();
        let machine_id = record.id();
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
            sealing: self.local.held_sealing(record.phase()),
            attest: self.local.held_attest(record.phase()),
        })
    }

    async fn inspect(&self, request: Request<OpaquePayload>) -> Answer {
        let details = self
            .at(&request)
            .inspect(expect::<op::Inspect>(request)?)
            .await;
        finish(details.map(|mut details| {
            for rtt in &mut details.rtts {
                let Some(machine) = &rtt.machine else {
                    continue;
                };
                rtt.path = self.paths.get(&machine.id);
                rtt.wireguard = self.paths.wireguard(&machine.id);
                // The mesh echo's round trip, when there is one, over Corrosion's (#3502).
                if let Some(statistics) = self.paths.rtt(&machine.id) {
                    rtt.statistics = statistics;
                }
            }
            details
        }))
    }

    async fn wrap_sealing_key(&self, request: Request<OpaquePayload>) -> Answer {
        finish(
            self.at(&request)
                .wrap_sealing_key(expect::<op::WrapSealingKey>(request)?),
        )
    }

    async fn pending_joins(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        expect::<op::PendingJoins>(request)?;
        finish(local.pending_joins())
    }

    async fn confirm_join(&self, request: Request<OpaquePayload>) -> Answer {
        finish(
            self.local
                .confirm_join(expect::<op::ConfirmJoin>(request)?)
                .await,
        )
    }

    async fn select_endpoint(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::SelectEndpoint>(request)?;
        match self.select(request).await {
            Ok(selected) => respond(selected),
            Err(error) => respond(error),
        }
    }

    async fn list_machines(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        expect::<op::ListMachines>(request)?;
        finish(local.list_machines().await.map(|mut list| {
            for machine in &mut list.machines {
                machine.path = self.paths.get(&machine.machine.id);
                machine.wireguard = self.paths.wireguard(&machine.machine.id);
                if let Some(rtt) = self.paths.rtt(&machine.machine.id) {
                    machine.rtt = Some(rtt);
                }
            }
            list
        }))
    }

    async fn list_containers(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::ListContainers>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let machine_id = self.local_record().id();
        match containers.lifecycle().list_managed(&machine_id).await {
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

    async fn inspect_container(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::InspectContainer>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let machine_id = self.local_record().id();
        match containers
            .observation()
            .inspect_managed_details(&request.container_id, &machine_id)
            .await
        {
            Ok(mut details) => {
                // A read, so the scope as the keys stand now.
                if !matches!(local.scope_now(), Ok(Scope::Full(_))) {
                    match self.opened_keys(request.container_id).await {
                        Ok(opened) => withhold_opened(&mut details.environment, &opened),
                        Err(_) => details.environment = None,
                    }
                }
                respond(details)
            }
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn ensure_container(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::EnsureContainer>(request)?;
        if let Some(refusal) = pin_on_ensure(&request.want) {
            return respond(refusal);
        }
        self.ensure(local, request).await
    }

    async fn ensure_pinned_container(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::EnsurePinnedContainer>(request)?;
        if let Some(refusal) = pinned_shape(&request.want) {
            return respond(refusal);
        }
        self.ensure(local, request).await
    }

    async fn update_container_limits(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::UpdateContainerLimits>(request)?;
        finish(
            local
                .update_container_limits(request.container_id, request.spec)
                .await,
        )
    }

    async fn update_container_placement(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::UpdateContainerPlacement>(request)?;
        finish(
            local
                .update_container_placement(request.container_id, request.spec)
                .await,
        )
    }

    async fn inspect_storage(&self, request: Request<OpaquePayload>) -> Answer {
        expect::<op::InspectStorage>(request)?;
        match self
            .local
            .plugin()
            .call::<ployz_core::StorageCapacity>(crate::volume_copy::STORAGE_INSPECT_ROUTE, &())
            .await
        {
            Ok(capacity) => respond(capacity),
            Err(error) => respond(error),
        }
    }

    async fn prepare_volumes(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::PrepareVolumes>(request)?;
        finish(local.prepare_volumes(request.specs).await)
    }

    async fn list_volumes(&self, request: Request<OpaquePayload>) -> Answer {
        expect::<op::ListVolumes>(request)?;
        let machine_id = self.local_record().id();
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        match containers.volumes().list_volumes(&machine_id).await {
            Ok(inventory) => respond(inventory),
            Err(error) => respond(RpcError::from(&error)),
        }
    }

    async fn remove_volume(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::RemoveVolume>(request)?;
        finish(local.remove_volume(request.name, request.force).await)
    }

    async fn inspect_volume_copy(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::InspectVolumeCopy>(request)?;
        finish(
            self.local
                .volume_verb::<ployz_core::VolumeCopyView>(
                    "InspectVolumeCopy",
                    crate::volume_copy::VOLUME_INSPECT_ROUTE,
                    &request,
                )
                .await,
        )
    }

    async fn set_volume_copy(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::SetVolumeCopy>(request)?;
        let copy = local.set_volume_copy(request).await;
        finish(copy.map(|copy| ployz_core::SetVolumeCopyReply { copy }))
    }

    async fn seed_volume(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::SeedVolume>(request)?;
        finish(local.seed_volume(&request).await)
    }

    async fn seed_copy(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::SeedCopy>(request)?;
        finish(local.seed_copy(request).await)
    }

    async fn set_volume_size(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::SetVolumeSize>(request)?;
        finish(local.set_volume_size(&request).await)
    }

    async fn list_trash(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::ListTrash>(request)?;
        finish(
            self.local
                .volume_verb::<ployz_core::TrashList>(
                    "ListTrash",
                    crate::volume_copy::TRASH_LIST_ROUTE,
                    &request,
                )
                .await,
        )
    }

    async fn restore_trash(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::RestoreTrash>(request)?;
        finish(self.local.restore_trash(&request).await)
    }

    async fn empty_trash(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::EmptyTrash>(request)?;
        finish(
            self.local
                .volume_verb::<ployz_core::TrashEmptied>(
                    "EmptyTrash",
                    crate::volume_copy::TRASH_EMPTY_ROUTE,
                    &request,
                )
                .await,
        )
    }

    async fn close_preview(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::ClosePreview>(request)?;
        finish(local.close_preview(request.namespace).await)
    }

    async fn list_previews(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        expect::<op::ListPreviews>(request)?;
        finish(local.list_previews())
    }

    async fn build(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        Ok(Response::new(crate::build::start(
            self.at(&request),
            request.into_inner(),
            self.builds.clone(),
            containers.sealing().clone(),
            ployz_build::Access::Full,
        )))
    }

    async fn build_free(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::BuildFreeStream>, Status> {
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        Ok(Response::new(crate::build::start(
            self.at(&request),
            request.into_inner(),
            self.builds.clone(),
            containers.sealing().clone(),
            ployz_build::Access::Free,
        )))
    }

    async fn exec(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?;
        let bound = self
            .at(&request)
            .bind_full(crate::machine::Work::Exec)
            .map_err(local_status)?;
        containers
            .exec(request.into_inner(), bound)
            .await
            .map(Response::new)
    }

    async fn metrics_live(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::MetricsLive>(request)?;
        let containers = self
            .containers()
            .map_err(|error| Status::unavailable(error.message))?
            .local_docker()
            .live_stats(&request.scope)
            .await
            .map_err(|error| Status::unavailable(ployz_core::error_chain::inline(&error)))?;
        respond(MetricsLiveReply { containers })
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
            .subscribe(store, self.local.clone(), self.paths.clone(), entry_id)
            .await
            .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Unavailable, &error))?;
        Ok(Response::new(stream))
    }

    async fn generate_sealing_key(&self, request: Request<OpaquePayload>) -> Answer {
        sealing_refused(
            self.at(&request)
                .generate_sealing_key(expect::<op::GenerateSealingKey>(request)?)
                .await,
            ployz_core::GENERATE_SEALING_KEY_CAPABILITY,
        )
    }

    async fn install_sealing_key(&self, request: Request<OpaquePayload>) -> Answer {
        sealing_refused(
            self.at(&request)
                .install_sealing_key(expect::<op::InstallSealingKey>(request)?)
                .await,
            ployz_core::INSTALL_SEALING_KEY_CAPABILITY,
        )
    }

    async fn list_images(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::ListImages>(request)?;
        let containers = match self.containers() {
            Ok(containers) => containers,
            Err(error) => return respond(error),
        };
        let images = containers
            .images()
            .list_images(request.reference.as_deref(), request.last_tagged)
            .await
            .map_err(|error| ployz_core::rpc::caused_status(tonic::Code::Internal, &error))?;
        respond(images)
    }

    async fn ensure_image_ingest(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        expect::<op::EnsureImageIngest>(request)?;
        finish(local.ensure_image_ingest(Arc::clone(&self.ingest)).await)
    }

    async fn pull_image(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::PullImage>(request)?;
        if request.image.is_empty() {
            return respond(RpcError::invalid_argument("image is required"));
        }
        finish(local.pull_image(request).await)
    }

    async fn pull_free_image(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::PullFreeImage>(request)?;
        if request.image.is_empty() {
            return respond(RpcError::invalid_argument("image is required"));
        }
        finish(local.pull_free_image(request).await)
    }

    async fn pull_image_from_machine(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::PullImageFromMachine>(request)?;
        if request.pull.image().is_empty() {
            return respond(RpcError::invalid_argument("image is required"));
        }
        finish(local.pull_image_from_machine(request).await)
    }

    async fn mint_build_grant(&self, request: Request<OpaquePayload>) -> Answer {
        let local = self.at(&request);
        let request = expect::<op::MintBuildGrant>(request)?;
        // Refused before the mint, which opens the Sealed record to check it. A rule
        // build's grant passes enforcement only once every check of its proof passed.
        let scope = match local.scope_now() {
            Ok(scope) => scope,
            Err(error) => return local_error(error),
        };
        let minter = match (&request.rule, scope) {
            (None, Scope::Operator) => {
                return local_error(LocalMachineError::BuildRefused {
                    capability: ployz_core::MINT_BUILD_GRANT_CAPABILITY,
                });
            }
            // Bound before the mint opens the Sealed record, which it does only while a key
            // change would wait for it. A key change once it opened leaves the grant serving
            // nothing.
            (None, Scope::Full(_)) => match local.bind_full(crate::machine::Work::BuildGrant) {
                Ok(bound) => Minter::Full(bound),
                Err(error) => return local_error(error),
            },
            (Some(grant), _) => {
                let admitted =
                    self.admit_rule_grant(grant, request.holder.as_ref(), request.sealed.as_ref());
                let Some(admitted) = admitted.await else {
                    return respond(RpcError::unavailable(
                        "This Server cannot reach GitHub to check a rule build",
                    ));
                };
                match admitted {
                    Ok(admitted) => Minter::Rule(admitted),
                    Err(error) => return local_error(error),
                }
            }
        };
        match crate::management::build_grant::mint(
            &self.grants,
            &local,
            Arc::clone(&self.ingest),
            request,
            minter,
        )
        .await
        {
            Ok(Ok(minted)) => respond(minted),
            Ok(Err(refused)) => respond(refused),
            Err(error) => local_error(error),
        }
    }

    async fn inspect_build_grant(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::InspectBuildGrant>(request)?;
        match self.grants.inspect(&request.id) {
            Ok(inspected) => respond(inspected),
            Err(error) => respond(error),
        }
    }

    async fn publish_certificate_material(&self, request: Request<OpaquePayload>) -> Answer {
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
                        return respond(RpcError::invalid_argument(format!(
                            "refused Certificate Material for {hostname}: {error}",
                            error = ployz_core::error_chain::inline(&error),
                        )));
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

    async fn read_routes(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::ReadRoutes>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        let rows = match replicated.service_ingress_routes(&request.service).await {
            Ok(rows) => rows,
            Err(error) => return respond(cluster_store_error(error)),
        };
        let membership = match self.local.route_membership().await {
            Ok(membership) => membership,
            Err(error) => return local_error(error),
        };
        let mut routed = routed(rows, &request.service);
        let routes = listed(&membership.ackers)
            .map(|machine| (machine, routed.remove(&machine)))
            .collect();
        respond(RoutesRead {
            routes,
            complete: matches!(membership.ackers, Ackers::Known(_)),
            down: membership.down,
        })
    }

    async fn read_routed_addresses(&self, request: Request<OpaquePayload>) -> Answer {
        expect::<op::ReadRoutedAddresses>(request)?;
        respond(RoutedAddresses {
            routes: self.published_routes.borrow().addresses.clone(),
        })
    }

    async fn read_cluster_name(&self, request: Request<OpaquePayload>) -> Answer {
        expect::<op::ReadClusterName>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match replicated.cluster_name().await {
            Ok(name) => respond(ClusterNamed { name }),
            Err(error) => respond(cluster_store_error(error)),
        }
    }

    async fn set_cluster_name(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::SetClusterName>(request)?;
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match replicated.publish_cluster_name(&request.name).await {
            Ok(()) => respond(ClusterNamed {
                name: Some(request.name),
            }),
            Err(error) => respond(cluster_store_error(error)),
        }
    }

    async fn set_cluster_doors(&self, request: Request<OpaquePayload>) -> Answer {
        let request = expect::<op::SetClusterDoors>(request)?;
        if !request.doors.is_known() {
            return respond(
                RpcError::invalid_argument(
                    "This Server declares only the Public and Tunnel Doors.",
                )
                .with_details(serde_json::json!({
                    "capability": ployz_core::SET_CLUSTER_DOORS_CAPABILITY,
                })),
            );
        }
        let replicated = match self.ready_replicated() {
            Ok(replicated) => replicated,
            Err(error) => return respond(error),
        };
        match replicated.publish_cluster_doors(&request.doors).await {
            Ok(()) => respond(ClusterDoorsDeclared {
                doors: request.doors,
            }),
            Err(error) => respond(cluster_store_error(error)),
        }
    }
}

fn listed(ackers: &Ackers) -> impl Iterator<Item = ployz_core::MachineId> + '_ {
    let (named, unnamed) = match ackers {
        Ackers::Known(named) => (Some(named), None),
        Ackers::Incomplete { named, unnamed } => (Some(named), Some(unnamed)),
        Ackers::Unread => (None, None),
    };
    named
        .into_iter()
        .flat_map(BTreeMap::keys)
        .chain(unnamed.into_iter().flatten())
        .copied()
}

impl MachineService {
    /// Admit and converge one Container, for ensure.v1 and the pinned route alike. Admission
    /// reads the spec alone, never the pin.
    async fn ensure(
        &self,
        local: LocalMachine,
        request: ployz_core::EnsureContainerRequest,
    ) -> Answer {
        let admitted = match local
            .admit_spec(
                &request.want,
                request.rule.as_deref(),
                &request.versions,
                crate::machine::Admit::Record,
            )
            .await
        {
            Ok(admitted) => admitted,
            Err(error) => return local_error(error),
        };
        let issued = admitted.as_ref().and_then(AdmittedSpec::verified);
        let ensured = local.ensure_container(request.want, admitted).await;
        finish(ensured.map(|ensured| match ensured {
            ployz_core::EnsuredContainer::Ensured { now, health, .. } => {
                ployz_core::EnsuredContainer::Ensured {
                    now,
                    health,
                    verified: issued,
                }
            }
            refused @ ployz_core::EnsuredContainer::Refused { .. } => refused,
        }))
    }
}

/// ensure.v1 refuses a pin, now and later: a daemon from before the pinned route ignores
/// one, so a pin travels only on the route that requires it.
fn pin_on_ensure(want: &ployz_core::ContainerWant) -> Option<RpcError> {
    let ployz_core::ContainerWant::Running {
        container: ployz_core::RunTarget::New(new),
        ..
    } = want
    else {
        return None;
    };
    new.image_id
        .is_some()
        .then(|| pinned_refusal("ensure.v1 takes no image ID; send it on the pinned route"))
}

/// Why the pinned route refuses `want` before admission, if it does. It takes one
/// Pending Service Container whose spec names its image by tag, with the image ID it
/// must run, answered once health settles, and nothing a Deploy's pull would carry.
fn pinned_shape(want: &ployz_core::ContainerWant) -> Option<RpcError> {
    let ployz_core::ContainerWant::Running {
        container: ployz_core::RunTarget::New(new),
        rotation: ployz_core::RunRotation::Pending,
    } = want
    else {
        return Some(pinned_refusal(
            "the pinned route only creates a new Pending Container",
        ));
    };
    let spec = &new.resolved_spec;
    let why = if new.image_id.is_none() {
        "a pinned creation names the image ID it runs"
    } else if ployz_core::authority::deploy::content_addressed(&spec.container.image) {
        "the spec's image already names its content; send it on ensure.v1"
    } else if new.slot.kind != ployz_core::ContainerKind::ServiceContainer {
        "the pinned route creates only Service Containers"
    } else if new.answer != ployz_core::AnswerWhen::Settled {
        "a pinned creation answers once its health settles"
    } else if new.slot.service != spec.name {
        "the slot names another Service than the spec"
    } else if ployz_core::QualifiedService::new(new.slot.namespace.clone(), spec.name.clone())
        .reserved()
        .is_some()
    {
        "the pinned route creates no reserved Service"
    } else if new.registry_auth.is_some() || new.registry_login.is_some() || !new.image_prepared {
        "a pinned creation pulls nothing for the caller"
    } else {
        return None;
    };
    Some(pinned_refusal(why))
}

/// Whether every name `pull` writes is the retention name of the content it copies, which
/// the copy checks against the content before writing. Image Cleanup alone reads those
/// names, so an Operator may write them; any other name a Full creation may run.
fn pinned_refusal(message: &str) -> RpcError {
    RpcError::invalid_argument(message).with_details(
        serde_json::json!({ "capability": ployz_core::ENSURE_PINNED_CONTAINER_CAPABILITY }),
    )
}

fn local_error(error: LocalMachineError) -> Answer {
    respond(local_failure(error)?)
}

/// The status a stream fails with when its caller may not open it: a key this Machine no
/// longer accepts, or one short of Full scope.
fn local_status(error: LocalMachineError) -> Status {
    match local_failure(error) {
        Ok(error) => Status::internal(error.message),
        Err(status) => status,
    }
}

/// `error` as the [`RpcError`] it answers, or the [`Status`] it fails the call with.
fn local_failure(error: LocalMachineError) -> Result<RpcError, Status> {
    match error {
        // A source that fails the call outright has no details to mark.
        LocalMachineError::Departing { server, source } => {
            Ok(local_failure(*source)?.overlapping_leave(&server))
        }
        LocalMachineError::ManagementRevoked => Err(ployz_core::rpc::caused_status(
            tonic::Code::Unauthenticated,
            &error,
        )),
        LocalMachineError::OnlyCloudManagesSlots | LocalMachineError::NeedsFullScope => Err(
            ployz_core::rpc::caused_status(tonic::Code::PermissionDenied, &error),
        ),
        LocalMachineError::DeployRefused(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::SIGNED_VERSION_CAPABILITY).next("ployz deploy from an approved device"),
        )),
        LocalMachineError::RuleDeployRefused(ref refusal) => {
            let gate = Gate::new(ployz_core::DEPLOY_RULE_CAPABILITY);
            let gate = match refusal.next() {
                Some(next) => gate.next(next),
                None => gate,
            };
            Err(gated(tonic::Code::PermissionDenied, &error, &gate))
        }
        LocalMachineError::BuildRefused { capability } => Err(build_refused(&error, capability)),
        LocalMachineError::PrivateAccessRefused(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::PRIVATE_ACCESS_APPROVAL_CAPABILITY).next(
                "ployz private-access on from an approved device, or Connect in the dashboard with your passkey",
            ),
        )),
        LocalMachineError::RuleGrantRefused(_)
        | LocalMachineError::RuleProofRefused(_)
        | LocalMachineError::RuleHeadUnread(_) => Err(build_refused(
            &error,
            ployz_core::BUILD_GRANT_RULE_CAPABILITY,
        )),
        LocalMachineError::KeyRefused(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::KEY_STEP_CAPABILITY),
        )),
        LocalMachineError::CopyRefused(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::SEED_COPY_CAPABILITY),
        )),
        LocalMachineError::PreviewRefused(_)
        | LocalMachineError::Docker(crate::docker::Error::PreviewRefused(_)) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::PREVIEW_HOME_CAPABILITY),
        )),
        LocalMachineError::ForgetRefused(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::FORGET_VERSIONS_CAPABILITY),
        )),
        LocalMachineError::MemberRemoved(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::JOIN_APPROVAL_CAPABILITY)
                .because(GateReason::MemberRemoved)
                .next(
                    "ployz server add --reset with a new token; reinstall first if it never finished joining",
                ),
        )),
        LocalMachineError::JoinConsumed(_) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::JOIN_APPROVAL_CAPABILITY)
                .because(GateReason::JoinAlreadyAdmitted)
                .inspecting(&["ployz server ls"]),
        )),
        LocalMachineError::Store(StoreError::FirstKeySpent) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::ENROLL_MACHINE_CAPABILITY)
                .because(GateReason::FirstKeySpent)
                .next("sudo ployz key add LINE on this Server, with the key line from your install command"),
        )),
        LocalMachineError::Store(StoreError::NoFirstKey) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::ENROLL_MACHINE_CAPABILITY)
                .because(GateReason::NoFirstKey)
                .next("ployz server add --command, then run the line it prints on this Server"),
        )),
        LocalMachineError::Store(StoreError::JoinUnanchored) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::JOIN_APPROVAL_CAPABILITY).next(
                "copy the install command again, or run sudo ployz key add LINE on this Server",
            ),
        )),
        LocalMachineError::Store(StoreError::JoinTrustMissing { .. }) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::JOIN_TRUST_CAPABILITY),
        )),
        LocalMachineError::JoinRefused(_)
        | LocalMachineError::Store(StoreError::JoinRefused(_) | StoreError::NotFirstKey { .. }) => Err(gated(
            tonic::Code::PermissionDenied,
            &error,
            &Gate::new(ployz_core::JOIN_APPROVAL_CAPABILITY)
                .next("ployz device approve --join CODE from an approved device"),
        )),
        LocalMachineError::Store(error) => Ok(store_error(error)),
        LocalMachineError::JoinRecord(_) => Err(internal_response(error)),
        LocalMachineError::ApprovalRecord(_) => Err(internal_response(error)),
        LocalMachineError::NotParticipating => {
            Ok(RpcError::unavailable("Machine is not participating"))
        }
        LocalMachineError::ClusterStoreUnavailable => {
            Ok(RpcError::unavailable("Cluster store is not available"))
        }
        LocalMachineError::ClusterNetworkUnpublished | LocalMachineError::ClusterStoreEmpty => {
            Ok(RpcError::unavailable(error.to_string()))
        }
        LocalMachineError::ClusterUnavailable => {
            Err(Status::unavailable("Cluster is not available"))
        }
        LocalMachineError::WorkNotStopped(_) => {
            Ok(RpcError::caused(RpcErrorCode::Internal, &error))
        }
        LocalMachineError::DockerUnavailable => {
            Ok(RpcError::unavailable("Docker is not available"))
        }
        LocalMachineError::PasskeyIsDevice(_)
        | LocalMachineError::DeviceIsPasskey(_)
        | LocalMachineError::KeyHeld { .. }
        | LocalMachineError::KeyIsClient(_)
        | LocalMachineError::CredentialHeld { .. } => {
            Ok(RpcError::caused(RpcErrorCode::Conflict, &error))
        }
        LocalMachineError::JoinerCannotFollowDoors { ref joiner } => Ok(RpcError::unsupported(
            error.to_string(),
        )
        .with_details(serde_json::json!({
            "target": joiner.to_string(),
            "capability": ployz_core::SET_CLUSTER_DOORS_CAPABILITY,
            "next": "upgrade ployz where the join runs, then rerun it",
        }))),
        LocalMachineError::DeclaredDoorsUnread(_) => {
            Ok(RpcError::caused(RpcErrorCode::Unavailable, &error))
        }
        LocalMachineError::MissingAssignment
        | LocalMachineError::DeviceSlot
        | LocalMachineError::InvalidPushTarget(_)
        | LocalMachineError::Seal(_) => Ok(RpcError::caused(RpcErrorCode::InvalidArgument, &error)),
        LocalMachineError::Enrollment(error) => {
            Ok(RpcError::caused(RpcErrorCode::Conflict, &error))
        }
        LocalMachineError::EmptyUpdate => Ok(RpcError::invalid_argument(
            "at least one Machine update is required",
        )),
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
            Ok(RpcError::caused(RpcErrorCode::InvalidArgument, &error))
        }
        LocalMachineError::PrivateAccessRouterSpec | LocalMachineError::PrivateAccessKey(_) => {
            Ok(RpcError::invalid_argument(error.to_string()))
        }
        LocalMachineError::PrivateAccessKeyMissing => {
            Ok(RpcError::invalid_argument(error.to_string()).with_details(
                serde_json::json!({ "capability": ployz_core::SET_PRIVATE_ACCESS_CAPABILITY }),
            ))
        }
        LocalMachineError::PrivateAccessKeyFile(_)
        | LocalMachineError::PrivateAccessState(_)
        | LocalMachineError::PrivateAccessRouterStays { .. } => {
            Ok(RpcError::caused(RpcErrorCode::Internal, &error))
        }
        LocalMachineError::Network(error) => Err(ployz_core::rpc::caused_status(
            tonic::Code::Internal,
            &error,
        )),
        // Both ask the caller for the doors route, so its capability is named.
        LocalMachineError::UnsupportedDoors
        | LocalMachineError::Docker(
            crate::docker::Error::IngressHolderNotRunning
            | crate::docker::Error::TunnelOriginNotRunning,
        ) => Ok(RpcError::invalid_argument(error.to_string()).with_details(
            serde_json::json!({ "capability": ployz_core::SET_INGRESS_DOORS_CAPABILITY }),
        )),
        LocalMachineError::TunnelTokenMissing => Ok(RpcError::invalid_argument(error.to_string())
            .with_details(
                serde_json::json!({ "capability": ployz_core::SET_INGRESS_TUNNEL_CAPABILITY }),
            )),
        LocalMachineError::TunnelToken(_) => Ok(RpcError::invalid_argument(error.to_string())),
        LocalMachineError::DoorNotRunning { .. }
        | LocalMachineError::TunnelTokenFile(_)
        | LocalMachineError::TunnelDoorMarker(_) => {
            Ok(RpcError::caused(RpcErrorCode::Internal, &error))
        }
        LocalMachineError::Docker(error) => Ok(RpcError::from(&error)),
        LocalMachineError::StoragePreparation(error)
        | LocalMachineError::FenceRefused(error)
        | LocalMachineError::PullRefused(error) => Ok(error),
        LocalMachineError::Cleanup(message) => Ok(RpcError::internal(message)),
        LocalMachineError::Admission(crate::mutation::Error::Busy) => Ok(RpcError::conflict(
            "a Ployz installation or upgrade is active",
        )),
        LocalMachineError::Admission(crate::mutation::Error::Io(error)) => Err(
            ployz_core::rpc::caused_status(tonic::Code::Internal, &error),
        ),
        LocalMachineError::Upgrade(error) => upgrade_error(error),
        LocalMachineError::Sealing(error) => Ok(sealing_error(error)),
        LocalMachineError::Recovery(error) => Ok(recovery_error(error)),
    }
}

fn sealing_error(error: SealingError) -> RpcError {
    use ployz_core::SealingRefusal as Refusal;
    let (code, refusal) = match &error {
        SealingError::Unreadable(_) | SealingError::Unwritable(_) => {
            (RpcErrorCode::Internal, Refusal::Unreadable)
        }
        SealingError::Published(_) => (RpcErrorCode::Conflict, Refusal::Published),
        SealingError::AwaitsRecovery(_) => (RpcErrorCode::Conflict, Refusal::AwaitsRecovery),
        SealingError::NotHeld => (RpcErrorCode::Conflict, Refusal::NotHeld),
        SealingError::HeldAnother { .. } => (RpcErrorCode::Conflict, Refusal::HeldAnother),
        SealingError::UnknownMachine(_) => (RpcErrorCode::NotFound, Refusal::UnknownMachine),
        SealingError::WireGuardKeyMismatch(_) => {
            (RpcErrorCode::Conflict, Refusal::WireGuardKeyMismatch)
        }
        SealingError::Unopenable { .. } => (RpcErrorCode::InvalidArgument, Refusal::Unopenable),
        SealingError::SealedAnother { .. } => {
            (RpcErrorCode::InvalidArgument, Refusal::SealedAnother)
        }
    };
    RpcError::caused(code, &error).with_evidence(&refusal)
}

/// A refusal that keeps a second Sealing Key out of the Cluster also names the route's
/// capability, as every refusal the coverage table expects does.
fn sealing_refused(
    result: Result<ployz_core::SealingKeyHeld, LocalMachineError>,
    capability: &'static str,
) -> Answer {
    match result {
        Err(LocalMachineError::Sealing(
            error @ (SealingError::Published(_)
            | SealingError::AwaitsRecovery(_)
            | SealingError::Unopenable { .. }
            | SealingError::SealedAnother { .. }),
        )) => {
            let refused = sealing_error(error);
            let mut details = refused.details.clone();
            if let Some(details) = details.as_object_mut() {
                details.insert("capability".into(), serde_json::json!(capability));
            }
            respond(refused.with_details(details))
        }
        result => finish(result),
    }
}

/// Refusals name themselves in `details` so the CLI tells a wrong code from a
/// wrong backup; a key file this Machine cannot use is its own fault.
fn recovery_error(error: RecoveryError) -> RpcError {
    use ployz_core::RecoveryRefusal as Refusal;
    let (code, refusal) = match &error {
        RecoveryError::Unreadable(_) | RecoveryError::Unwritable(_) => {
            return RpcError::caused(RpcErrorCode::Internal, &error);
        }
        RecoveryError::CloudRouted => (RpcErrorCode::Unsupported, Refusal::CloudRouted),
        RecoveryError::NotHeld => (RpcErrorCode::Conflict, Refusal::NotHeld),
        RecoveryError::NothingAwaited => (RpcErrorCode::Conflict, Refusal::NothingAwaited),
        RecoveryError::NoBackup => (RpcErrorCode::Conflict, Refusal::NoBackup),
        RecoveryError::Malformed => (RpcErrorCode::InvalidArgument, Refusal::Malformed),
        RecoveryError::OtherKey(_) => (RpcErrorCode::InvalidArgument, Refusal::OtherKey),
        RecoveryError::WrongCode => (RpcErrorCode::InvalidArgument, Refusal::WrongCode),
        RecoveryError::HeldAnother { .. } => (RpcErrorCode::Conflict, Refusal::HeldAnother),
    };
    RpcError::caused(code, &error).with_evidence(&refusal)
}

fn upgrade_error(error: crate::installer::upgrade::Error) -> Result<RpcError, Status> {
    use crate::{installer::upgrade::Error, mutation};
    let code = match &error {
        Error::OffChannel { .. } => {
            return Err(ployz_core::rpc::hinted_status(
                tonic::Code::PermissionDenied,
                &error,
                serde_json::json!({
                    "capability": ployz_core::UPGRADE_EXACT_CAPABILITY,
                    "next": "ployz device request",
                }),
            ));
        }
        Error::NotFound => RpcErrorCode::NotFound,
        Error::AttemptConflict(_) | Error::Busy | Error::Admission(mutation::Error::Busy) => {
            RpcErrorCode::Conflict
        }
        Error::Resolve(_) | Error::NonstandardPaths(_) | Error::Downgrade { .. } => {
            RpcErrorCode::InvalidArgument
        }
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
    Ok(RpcError::caused(code, &error))
}

fn finish(result: Result<impl Into<RpcResponse>, LocalMachineError>) -> Answer {
    match result {
        Ok(value) => respond(value),
        Err(error) => local_error(error),
    }
}

fn store_error(error: StoreError) -> RpcError {
    if let StoreError::GrantMoved(key) = &error {
        let details = serde_json::json!({ ployz_core::authority::GRANT_MOVED: key });
        return RpcError::caused(RpcErrorCode::Conflict, &error).with_details(details);
    }
    let code = match error {
        StoreError::JoinRefused(_)
        | StoreError::NotFirstKey { .. }
        | StoreError::FirstKeySpent
        | StoreError::NoFirstKey
        | StoreError::JoinUnanchored
        | StoreError::JoinTrustMissing { .. } => RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        StoreError::JoinRestrictionsPending => RpcErrorCode::Conflict,
        StoreError::FirstKey(_) => RpcErrorCode::Internal,
        StoreError::KeyRemoved(_)
        | StoreError::GrantRemoved(_)
        | StoreError::LastKey(_)
        | StoreError::AmbiguousCode(_)
        | StoreError::CodeHeld(_)
        | StoreError::GrantKind { .. }
        | StoreError::GrantHeld(_)
        | StoreError::GrantMoved(_)
        | StoreError::NotGrant(_) => RpcErrorCode::Conflict,
        StoreError::AlreadyResetting
        | StoreError::AlreadyInitialized
        | StoreError::NotParticipating
        | StoreError::NotResetting
        | StoreError::NotJoining
        | StoreError::AlreadyRunning(_) => RpcErrorCode::Conflict,
        StoreError::MissingEndpoints
        | StoreError::RecoveryWithoutFounding
        | StoreError::MissingPeers
        | StoreError::IdentityMismatch
        | StoreError::KeyMismatch
        | StoreError::SealingKey(_)
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

fn respond(response: impl Into<RpcResponse>) -> Answer {
    Ok(Response::new(
        response.into().encode().map_err(internal_response)?,
    ))
}

fn invalid_request(error: impl std::error::Error + 'static) -> Status {
    ployz_core::rpc::caused_status(tonic::Code::InvalidArgument, &error)
}

fn request_body(request: Request<OpaquePayload>) -> Result<RpcRequestBody, Status> {
    request
        .into_inner()
        .decode_request()
        .map(|request| request.body)
        .map_err(invalid_request)
}

/// The refusal an enforcing Machine answers an Operator's `Build` or `MintBuildGrant` with.
pub(super) fn build_refused(error: &LocalMachineError, capability: &'static str) -> Status {
    gated(tonic::Code::PermissionDenied, error, &Gate::new(capability))
}

/// `error` as the status a gate answers with: its message and causes, and `gate` as the
/// details.
fn gated(code: tonic::Code, error: &LocalMachineError, gate: &Gate) -> Status {
    ployz_core::rpc::hinted_status(code, error, gate.to_details())
}

fn withhold_opened(environment: &mut Option<BTreeMap<String, String>>, opened: &BTreeSet<String>) {
    if let Some(environment) = environment {
        environment.retain(|key, _| !opened.contains(key));
    }
}

fn expect<T: Rpc>(request: Request<OpaquePayload>) -> Result<T::Request, Status> {
    T::from_request_body(request_body(request)?).map_err(invalid_request)
}

fn internal_response(error: impl std::error::Error + 'static) -> Status {
    ployz_core::rpc::caused_status(tonic::Code::Internal, &error)
}

#[cfg(test)]
#[path = "authorization_tests.rs"]
mod authorization_tests;
#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;
#[cfg(test)]
#[path = "rotation_fleet_tests.rs"]
mod rotation_fleet_tests;
#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
