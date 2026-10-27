//! Local Machine operations as Live Observation and membership changes
//! from this Machine's Local Machine Phase.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use ployz_core::{
    ContainerId, ContainerObservation, EnrollRequest, Enrolled, ForgetRequest, Forgotten,
    InspectRequest, LeaveRequest, Left, LeftPhase, LocalMachinePhase, Machine, MachineDetails,
    MachineId, MachineIdentity, MachineList, MachineName, MachineObservation, MachineToken,
    MachineUpdated, ManagementAddress, ManagementClientLabel, MembershipEvidence,
    PublicIpDiscovery, RecoverySeed, RegisterRequest, Registered, RpcError, RpcErrorCode,
    RttObservation, RttStatistics, SelectedEndpoint, StorageDeparture, UpdateMachineRequest,
    synthesize_membership,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    EnrollPlan, LocalMachineRecord, RecordOwner, RecordOwnerStopped, StoreError, local_runtime,
    store::{Installation, PreparedReset, Rediscovered},
};

use crate::{
    corrosion::{AdminClient, MembershipState, ReplicatedStore, membership_states_by_address},
    docker::ContainerRuntime,
    host_capacity,
    network::{NetworkError, discover_network},
    sealing_key::SealingKey,
};

const REDISCOVERY_RETRY_MIN: Duration = Duration::from_millis(250);
const REDISCOVERY_RETRY_MAX: Duration = Duration::from_secs(5);

/// Live Observation and membership operations for this Machine.
#[derive(Clone)]
pub struct LocalMachine {
    owner: RecordOwner,
    management_client: Option<[u8; 32]>,
    cluster: Option<ClusterContext>,
    containers: Option<ContainerRuntime>,
    plugin: crate::storage::Plugin,
    zpool: PathBuf,
    proc_root: PathBuf,
    settled: container::SettledCreations,
    #[cfg(test)]
    peers: Option<PeerInspect>,
    #[cfg(test)]
    ensure_pause: Option<container::EnsurePause>,
}

const PEER_INSPECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// How a test answers another Machine's copy in place of the mesh.
#[cfg(test)]
pub(crate) type PeerInspect = std::sync::Arc<
    dyn Fn(
            MachineId,
            ContainerId,
        ) -> futures_util::future::BoxFuture<'static, Option<ContainerObservation>>
        + Send
        + Sync,
>;

mod container;
pub(crate) use container::CutRefusal;
#[cfg(test)]
pub(crate) use container::{EnsurePause, EnsurePausePoint};
mod management_client;
mod push_target;
mod sealing;
pub(crate) use sealing::Error as SealingError;
mod sealing_recovery;
pub(crate) use sealing_recovery::Error as RecoveryError;
mod upgrade;

#[derive(Clone)]
struct ClusterContext {
    replicated: ReplicatedStore,
    admin: AdminClient,
}

/// Entry-local admin membership, RTT samples, and selected endpoints.
///
/// Projected onto the current replicated Machine snapshot at assemble time.
/// Missing telemetry is not a delete of the replicated Machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimeWatchTelemetry {
    pub states: BTreeMap<ManagementAddress, MembershipEvidence>,
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    pub rtts: Vec<RttObservation>,
}

impl RuntimeWatchTelemetry {
    /// Membership, selected endpoints, and RTT for the current replicated Machines.
    pub(crate) fn overlay(
        &self,
        machines: Vec<Machine>,
        entry_id: &MachineId,
    ) -> Vec<MachineObservation> {
        let rtt = rtts_by_machine(&machines, &self.rtts);
        let mut observations = synthesize_membership(machines, entry_id, &self.states);
        for observation in &mut observations {
            observation.selected_endpoint = self
                .selected_endpoints
                .get(&observation.machine.id)
                .copied();
            observation.rtt = rtt.get(&observation.machine.id).cloned();
        }
        observations
    }
}

/// Failures from Local Machine operations. The RPC adapter maps these once.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Enrollment(#[from] ployz_core::EnrollmentError),
    /// Machine-local storage admission or preparation failed with structured details.
    #[error(transparent)]
    StoragePreparation(#[from] ployz_core::RpcError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("management credential has been revoked")]
    ManagementRevoked,
    #[error("only Cloud manages slots")]
    OnlyCloudManagesSlots,
    #[error("device- slots change only through `ployz device`")]
    DeviceSlot,
    #[error("only an approved Device manages Devices")]
    NeedsFullScope,
    #[error("Machine is not participating")]
    NotParticipating,
    #[error("Cluster store is not available")]
    ClusterStoreUnavailable,
    #[error("Cluster network is not published yet")]
    ClusterNetworkUnpublished,
    #[error(
        "this Server's Cluster store holds no other Server: it was recreated and can't reach them yet"
    )]
    ClusterStoreEmpty,
    #[error("Cluster is not available")]
    ClusterUnavailable,
    #[error("Docker is not available")]
    DockerUnavailable,
    /// A Leave was removing this Machine from the Cluster as the call failed with
    /// `source`, so its answer is `source`'s, marked with the Server.
    #[error("Server {server} is being removed from the Cluster")]
    Departing {
        server: MachineName,
        #[source]
        source: Box<Error>,
    },
    #[error("at least one Machine update is required")]
    EmptyUpdate,
    #[error("push target {0}")]
    InvalidPushTarget(&'static str),
    #[error(transparent)]
    RecordOwner(#[from] RecordOwnerStopped),
    #[error(transparent)]
    Cluster(#[from] crate::corrosion::Error),
    #[error(transparent)]
    IngressProxyServiceSpec(#[from] ployz_core::IngressProxyServiceSpecError),
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error(transparent)]
    Docker(#[from] crate::docker::Error),
    #[error("{0}")]
    Cleanup(String),
    /// An upgrade is running; the fence is retried once it ends.
    #[error(transparent)]
    FenceRefused(RpcError),
    #[error("local operation task failed")]
    OperationTask(#[from] tokio::task::JoinError),
    #[error("Register requires a client-selected Machine Subnet")]
    MissingAssignment,
    #[error(transparent)]
    Admission(#[from] crate::mutation::Error),
    #[error(transparent)]
    Upgrade(#[from] crate::installer::upgrade::Error),
    #[error(transparent)]
    Seal(#[from] crate::sealing_key::SealError),
    #[error(transparent)]
    Sealing(#[from] SealingError),
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
}

impl LocalMachine {
    /// A Local Machine over its record owner, with no Cluster or Docker collaborators.
    #[must_use]
    pub fn new(owner: RecordOwner) -> Self {
        Self {
            owner,
            management_client: None,
            cluster: None,
            containers: None,
            plugin: crate::storage::Plugin::default(),
            zpool: PathBuf::from("zpool"),
            proc_root: PathBuf::from("/proc"),
            settled: container::SettledCreations::default(),
            #[cfg(test)]
            peers: None,
            #[cfg(test)]
            ensure_pause: None,
        }
    }

    /// Hold every `ensure_container` on this handle at `pause`'s point until the test
    /// releases it; clones made before this call never pause.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn pausing_ensure_for_test(mut self, pause: container::EnsurePause) -> Self {
        self.ensure_pause = Some(pause);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_zpool(mut self, program: impl Into<PathBuf>) -> Self {
        self.zpool = program.into();
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_peers(mut self, peers: PeerInspect) -> Self {
        self.peers = Some(peers);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_proc_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.proc_root = root.into();
        self
    }

    #[must_use]
    pub(crate) fn with_plugin(mut self, plugin: crate::storage::Plugin) -> Self {
        self.plugin = plugin;
        self
    }

    pub(crate) fn plugin(&self) -> &crate::storage::Plugin {
        &self.plugin
    }

    /// Bind subsequent mutation admission to this authenticated management client.
    #[must_use]
    pub(crate) fn with_management_client(mut self, remote: [u8; 32]) -> Self {
        self.management_client = Some(remote);
        self
    }

    #[must_use]
    pub(crate) fn with_cluster(mut self, cluster: Option<(ReplicatedStore, AdminClient)>) -> Self {
        self.cluster = cluster.map(|(replicated, admin)| ClusterContext { replicated, admin });
        self
    }

    #[must_use]
    pub(crate) fn with_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.containers = containers;
        self
    }

    pub(crate) fn has_cluster(&self) -> bool {
        self.cluster.is_some()
    }

    pub(crate) fn containers(&self) -> Option<&ContainerRuntime> {
        self.containers.as_ref()
    }

    /// The persisted Local Machine record as last published by its owner.
    #[must_use]
    pub fn record(&self) -> Arc<LocalMachineRecord> {
        self.owner.record()
    }

    /// The owner of this Machine's durable record.
    #[must_use]
    pub fn owner(&self) -> &RecordOwner {
        &self.owner
    }

    pub(crate) fn require_management_access(&self) -> Result<(), Error> {
        if let Some(remote) = self.management_client
            && !self.record().accepts_management_client(&remote)
        {
            return Err(Error::ManagementRevoked);
        }
        Ok(())
    }

    /// Refuse a slot change from any management client other than Cloud's.
    /// Local-socket callers carry no client key and keep full authority.
    pub(crate) fn require_slot_authority(
        client: Option<[u8; 32]>,
        record: &LocalMachineRecord,
    ) -> Result<(), Error> {
        let Some(remote) = client else {
            return Ok(());
        };
        let label = record.accepted_label(&remote);
        if label.is_some_and(|label| label.as_str() == "cloud") {
            return Ok(());
        }
        tracing::warn!(
            slot = label.map_or("none", ManagementClientLabel::as_str),
            "refused SetManagementClient: only Cloud manages slots"
        );
        Err(Error::OnlyCloudManagesSlots)
    }

    /// Admit work under installation exclusion only, taking no turn in Machine
    /// admission. Builds and image ingest use it alone; admitted work adds its turn.
    pub(crate) fn admit_installation(&self) -> Result<crate::mutation::MutationGuard, Error> {
        self.require_management_access()?;
        Ok(self.owner.mutation_gate().try_mutation()?)
    }

    /// Run `work` alone: no other admitted work runs on this Machine meanwhile.
    async fn finish_mutation<T, F>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let exclusive = self.owner.admission_lock().write_owned().await;
        self.finish_admitted(exclusive, work).await
    }

    /// Run `work` holding `turn`, an exclusive or shared admission guard. Admitted
    /// work is detached from its RPC: a caller that stops waiting does not abandon it
    /// halfway.
    async fn finish_admitted<T, F, G>(&self, turn: G, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, Error>> + Send + 'static,
        G: Send + 'static,
    {
        let installation = self.admit_installation()?;
        tokio::spawn(async move {
            let _admission = (turn, installation);
            work.await
        })
        .await?
    }

    /// Run `work` holding the installation guard and no turn. `work` takes the turns it
    /// needs itself, so a drain inside it never holds one. Detached like admitted work.
    async fn finish_installed<T, F, Fut>(&self, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(crate::mutation::MutationGuard) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, Error>> + Send + 'static,
    {
        let installation = self.admit_installation()?;
        tokio::spawn(work(installation)).await?
    }

    #[cfg(test)]
    pub(crate) async fn hold_mutation_for_test(
        &self,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> Result<(), Error> {
        self.finish_mutation(async move {
            started.notify_one();
            release.notified().await;
            Ok(())
        })
        .await
    }

    /// The Machines whose proxies must ack a route change, and those reported Down.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] without a Cluster store, and
    /// [`Error::Cluster`] when the Machine list cannot be read.
    pub(crate) async fn route_membership(&self) -> Result<crate::route_acks::Membership, Error> {
        let cluster = self
            .cluster
            .as_ref()
            .ok_or(Error::ClusterStoreUnavailable)?;
        let machines = cluster.replicated.machines().await?;
        Ok(crate::route_acks::sample_ackers(machines, &self.record().id(), &cluster.admin).await)
    }

    /// `container` as `machine` observes it now, with the environment and Config content
    /// a store row drops. `None` when it didn't answer within [`PEER_INSPECT_TIMEOUT`], so
    /// the two guard reads of one In fit the caller's wait.
    pub(crate) async fn inspect_peer(
        &self,
        machine: MachineId,
        container: ContainerId,
    ) -> Option<ContainerObservation> {
        #[cfg(test)]
        if let Some(peers) = &self.peers {
            return peers(machine, container).await;
        }
        let machines = self.replicated().ok()?.machines().await.ok()?;
        let peer = machines
            .observations
            .iter()
            .find(|peer| peer.id == machine)?;
        crate::boot_pass::inspect_peer(peer, container, PEER_INSPECT_TIMEOUT).await
    }

    pub(crate) fn replicated(&self) -> Result<&ReplicatedStore, Error> {
        self.cluster
            .as_ref()
            .map(|cluster| &cluster.replicated)
            .ok_or(Error::ClusterStoreUnavailable)
    }

    /// Entry-local membership and RTT from the same admin source as ListMachines
    /// and inspect RTT.
    ///
    /// Returns `None` when the admin socket cannot be read so Watch can keep
    /// replicated rows.
    pub(crate) async fn runtime_watch_telemetry(&self) -> Option<RuntimeWatchTelemetry> {
        let Some(cluster) = &self.cluster else {
            return None;
        };
        let local = self.record();
        let (states, rtts) = read_admin(&cluster.admin).await?;
        Some(RuntimeWatchTelemetry {
            states: membership_states_by_address(states),
            rtts,
            selected_endpoints: local.selected_endpoints.clone(),
        })
    }

    /// Live Observation of this Machine's identity, Local Machine Phase, and
    /// advertised reachability.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Network`] when endpoint discovery fails, [`Error::Cluster`]
    /// when store version or RTT lookup fails, and [`Error::ClusterUnavailable`]
    /// when RTTs are requested without a Cluster. Returns
    /// [`Error::DockerUnavailable`] when requested telemetry cannot reach Docker,
    /// or a Docker error when a requested telemetry probe fails.
    pub async fn inspect(&self, request: InspectRequest) -> Result<MachineDetails, Error> {
        let record = self.record();
        let token = match request.token {
            Some(token) => Some(
                self.token(token.public_ip, &request.advertised_endpoints)
                    .await?,
            ),
            None => None,
        };
        let advertised_endpoints = if !request.advertised_endpoints.is_empty() {
            request.advertised_endpoints
        } else if let Some(machine) = record.machine() {
            machine.advertised_endpoints.clone()
        } else if let Some(token) = &token {
            token.advertised_endpoints.clone()
        } else {
            discover_network(PublicIpDiscovery::Auto).await?.endpoints
        };
        let store_version = match &self.cluster {
            Some(cluster) => cluster.replicated.version().await?,
            None => BTreeMap::new(),
        };
        let rtts = if request.include_rtts && record.phase() == LocalMachinePhase::Participating {
            let cluster = self.cluster.as_ref().ok_or(Error::ClusterUnavailable)?;
            machine_rtts(&cluster.admin, &cluster.replicated).await?
        } else {
            Vec::new()
        };
        let telemetry = match request.telemetry {
            ployz_core::InspectTelemetry::None => None,
            ployz_core::InspectTelemetry::BridgeCapacity => {
                let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                Some(ployz_core::TelemetryObservation::BridgeCapacity {
                    bridge: containers.bridge_capacity().await?,
                })
            }
            ployz_core::InspectTelemetry::Full => {
                let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
                let (host, bridge) =
                    tokio::try_join!(containers.telemetry(), containers.bridge_capacity())?;
                Some(ployz_core::TelemetryObservation::Full { host, bridge })
            }
        };
        let storage = if request.include_storage {
            self.observe_storage().await
        } else {
            None
        };
        let upgrade =
            match crate::installer::upgrade::latest(self.owner.data_dir(), self.owner.run_dir())
                .await
            {
                Ok(upgrade) => upgrade,
                Err(error) => {
                    tracing::warn!(%error, "could not read the latest upgrade attempt");
                    None
                }
            };
        Ok(MachineDetails {
            id: record.id(),
            phase: record.phase(),
            machine: record.machine().cloned(),
            public_key: record.wireguard_private_key.public_key(),
            advertised_endpoints,
            store_version,
            rtts,
            management_clients: record.management_clients().cloned().collect(),
            telemetry,
            storage,
            upgrade,
            token,
            sealing: self.sealing(record.phase()).await,
        })
    }

    async fn sealing(&self, phase: LocalMachinePhase) -> Option<ployz_core::Sealing> {
        if !matches!(
            phase,
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return None;
        }
        match SealingKey::observe(self.owner.data_dir()) {
            Some(ployz_core::Sealing::NeedsAccess) => self.lacking_key().await,
            observed => observed,
        }
    }

    /// A published key overrides the recovery marker, as Generate reads them.
    async fn lacking_key(&self) -> Option<ployz_core::Sealing> {
        let Ok(replicated) = self.replicated() else {
            return Some(ployz_core::Sealing::NeedsAccess);
        };
        match replicated.awaited_sealing_key().await {
            Ok(Some(public_key)) => Some(ployz_core::Sealing::AwaitsRecovery { public_key }),
            Ok(None) => Some(ployz_core::Sealing::NeedsAccess),
            Err(error) => {
                tracing::warn!(%error, "could not read whether the Cluster awaits recovery");
                None
            }
        }
    }

    async fn token(
        &self,
        public_ip: PublicIpDiscovery,
        requested_endpoints: &[ployz_core::AdvertisedEndpoint],
    ) -> Result<MachineToken, Error> {
        let record = self.record();
        let discovered = discover_network(public_ip).await?;
        let capacity = host_capacity::observe();
        Ok(MachineToken {
            id: record.id(),
            public_key: record.wireguard_private_key.public_key(),
            public_ip: discovered.public_ip,
            advertised_endpoints: if requested_endpoints.is_empty() {
                discovered.endpoints
            } else {
                requested_endpoints.to_vec()
            },
            runtime: local_runtime(self.owner.data_dir()),
            memory_total_bytes: capacity.memory_total_bytes,
            disk_total_bytes: capacity.disk_total_bytes,
            disk_available_bytes: capacity.disk_available_bytes,
        })
    }

    /// Refresh this participating Machine's unpinned public IP and Advertised
    /// Endpoints from discovery, waiting out busy installation admission until
    /// `shutdown`. A failure keeps the stored row.
    pub async fn rediscover_addresses(&self, shutdown: CancellationToken) {
        match self.try_rediscover_addresses(&shutdown).await {
            Ok(Rediscovered::Unchanged) => {}
            Ok(Rediscovered::Updated(machine)) => tracing::info!(
                public_ip = ?machine.public_ip,
                advertised_endpoints = ?machine.advertised_endpoints,
                "rediscovered this Machine's addresses"
            ),
            Ok(Rediscovered::PublicIpLookupFailed) => tracing::warn!(
                "public IP lookup failed; keeping the stored public IP and Advertised Endpoints"
            ),
            Err(error) => tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "could not rediscover this Machine's addresses; keeping the stored ones"
            ),
        }
    }

    async fn try_rediscover_addresses(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<Rediscovered, Error> {
        let record = self.record();
        let pinned = record.pinned_addresses();
        let (LocalMachinePhase::Participating, Some(machine)) = (record.phase(), record.machine())
        else {
            return Ok(Rediscovered::Unchanged);
        };
        if pinned.public_ip && pinned.advertised_endpoints {
            return Ok(Rediscovered::Unchanged);
        }
        let public_ip = match (pinned.public_ip, machine.public_ip) {
            (false, _) => PublicIpDiscovery::Auto,
            (true, Some(address)) => PublicIpDiscovery::Override(address),
            (true, None) => PublicIpDiscovery::Disabled,
        };
        let discovered = discover_network(public_ip).await?;
        let mut retry = REDISCOVERY_RETRY_MIN;
        loop {
            let local = self.clone();
            let discovered = discovered.clone();
            match self
                .finish_mutation(async move {
                    Ok(local
                        .owner
                        .mutate(move |store| store.rediscover(discovered))
                        .await??)
                })
                .await
            {
                Err(Error::Admission(crate::mutation::Error::Busy)) => {}
                outcome => return outcome,
            }
            tokio::select! {
                () = shutdown.cancelled() => return Ok(Rediscovered::Unchanged),
                () = tokio::time::sleep(retry) => {}
            }
            retry = (retry * 2).min(REDISCOVERY_RETRY_MAX);
        }
    }

    /// Enroll this Machine as a founder or member and request restart. A resend
    /// that names the stored enrollment replays it without restarting.
    ///
    /// # Errors
    ///
    /// Returns when mutation admission is busy, the record owner has stopped, the
    /// request is invalid, or storage departure fails.
    pub async fn enroll(&self, request: EnrollRequest) -> Result<Enrolled, Error> {
        let local = self.clone();
        self.finish_mutation(async move { local.enroll_admitted(request, None).await })
            .await
    }

    async fn enroll_admitted(
        &self,
        request: EnrollRequest,
        recovery: Option<RecoverySeed>,
    ) -> Result<Enrolled, Error> {
        let (plan, installation) = self
            .owner
            .mutate(move |store| store.plan_enrollment(request, recovery))
            .await??;
        let machine = match plan {
            EnrollPlan::Fresh => {
                let machine = installation
                    .machine()
                    .cloned()
                    .expect("an enrollment owns its Machine");
                self.install(installation).await?;
                self.owner.request_restart();
                machine
            }
            EnrollPlan::Replay(machine) => machine,
            EnrollPlan::Conflict(now) => {
                return Ok(Enrolled::Refused {
                    error: RpcError {
                        code: RpcErrorCode::Conflict,
                        message: enroll_conflict(&now.phase).into(),
                        details: serde_json::Value::Null,
                        cause: Vec::new(),
                    },
                    now,
                });
            }
        };
        tracing::info!(
            name = machine.name.as_str(),
            id = machine.id.as_str(),
            "enrolled"
        );
        Ok(Enrolled::Enrolled { machine })
    }

    /// Publish a client-selected assignment from a participating Entry Machine.
    ///
    /// # Errors
    /// Rejects absent assignments, invalid assignments, unavailable storage, or
    /// a Cluster network not yet published.
    pub async fn register(&self, request: RegisterRequest) -> Result<Registered, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            if local.record().phase() != LocalMachinePhase::Participating {
                return Err(Error::NotParticipating);
            }
            if request.assigned_subnet.is_none() {
                return Err(Error::MissingAssignment);
            }
            local.publish_assignment(request).await
        })
        .await
    }

    async fn publish_assignment(&self, request: RegisterRequest) -> Result<Registered, Error> {
        let replicated = self.replicated()?;
        let publication = replicated.machine_publication().await;
        // Other Machines' rows are not consulted: Cloud owns concurrency, and a
        // Register for an existing id overwrites its row.
        let snapshot = ployz_core::EnrollmentSnapshot {
            network: published_network(&self.record(), replicated).await?,
            machines: Vec::new(),
            target_versions: BTreeMap::new(),
        };
        let assignment = ployz_core::allocate_enrollment(&request, &snapshot, &[])?;
        let sealing_key = SealingKey::load(self.owner.data_dir())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "could not read the Sealing Key; registering without it");
                None
            })
            .map(|key| key.seal_to(&assignment.machine.public_key))
            .transpose()?;
        publication.publish(&assignment.machine).await?;
        Ok(registered(
            assignment.machine,
            replicated.machines().await?.observations,
            replicated.version().await?,
            sealing_key,
            self.record().devices().collect(),
        ))
    }

    /// Membership Observation of Machines visible from this participating Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotParticipating`] when this Machine is not
    /// participating, [`Error::ClusterStoreUnavailable`] when the Cluster
    /// store is missing, [`Error::ClusterNetworkUnpublished`] before the
    /// founder publishes the Cluster network, [`Error::ClusterStoreEmpty`] when
    /// this joined Machine's store never held another Machine, and [`Error::Cluster`] when
    /// replicated I/O fails.
    pub async fn list_machines(&self) -> Result<MachineList, Error> {
        let local = self.record();
        if local.phase() != LocalMachinePhase::Participating {
            return Err(Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let network = published_network(&local, replicated).await?;
        let machines = replicated.machines().await?.observations;
        let states = self.membership_states().await?;
        let entry_id = local.id();
        Ok(MachineList {
            enrollment: Some(ployz_core::EnrollmentSnapshot {
                network,
                machines: machines.clone(),
                target_versions: replicated.version().await?,
            }),
            machines: RuntimeWatchTelemetry {
                states,
                selected_endpoints: local.selected_endpoints.clone(),
                rtts: Vec::new(),
            }
            .overlay(machines, &entry_id),
        })
    }

    async fn membership_states(
        &self,
    ) -> Result<BTreeMap<ManagementAddress, MembershipEvidence>, Error> {
        let states = match &self.cluster {
            Some(cluster) => cluster.admin.membership_states().await?,
            None => Vec::new(),
        };
        Ok(membership_states_by_address(states))
    }

    /// Apply a local Machine update and best-effort publish it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyUpdate`] when no field is set,
    /// [`Error::ClusterStoreUnavailable`] when the Cluster store is missing,
    /// [`Error::RecordOwner`] when the record owner has stopped,
    /// [`Error::Store`] when the update is not legal, and [`Error::Cluster`]
    /// when listing visible Machines fails.
    pub async fn update(&self, request: UpdateMachineRequest) -> Result<MachineUpdated, Error> {
        let local = self.clone();
        // Policy edits affect new admission without waiting for active Builds.
        // Ordinary mutations remain serialized; all updates exclude installation.
        if request.update.name.is_none()
            && request.update.public_ip == ployz_core::PublicIpUpdate::Keep
            && request.update.advertised_endpoints.is_none()
        {
            self.require_management_access()?;
            let installation = self.owner.mutation_gate().try_mutation()?;
            return tokio::spawn(async move {
                let _installation = installation;
                local.update_admitted(request).await
            })
            .await?;
        }
        self.finish_mutation(async move { local.update_admitted(request).await })
            .await
    }

    async fn update_admitted(
        &self,
        request: UpdateMachineRequest,
    ) -> Result<MachineUpdated, Error> {
        if request.update.is_empty() {
            return Err(Error::EmptyUpdate);
        }
        let replicated = self.replicated()?;
        let visible = replicated.machines().await?.observations;
        let update = request.update;
        // The Machine publisher wakes on the changed record and publishes it.
        let machine = self
            .owner
            .mutate(move |store| store.update(update, &visible))
            .await??;
        Ok(MachineUpdated { machine })
    }

    /// Forget another Machine: delete its replicated row. A missing row is success.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] when the Cluster store is
    /// missing and [`Error::Cluster`] when the replicated delete fails.
    pub async fn forget(&self, request: ForgetRequest) -> Result<Forgotten, Error> {
        let local = self.clone();
        self.finish_mutation(async move {
            let now = local.record().now();
            if now.id == request.machine_id {
                return Ok(Forgotten::Refused {
                    error: refusal(
                        RpcErrorCode::InvalidArgument,
                        "this is the Machine itself; send Leave to it instead",
                    ),
                    now,
                });
            }
            local
                .replicated()?
                .forget_machine(&request.machine_id)
                .await?;
            Ok(Forgotten::Forgotten)
        })
        .await
    }

    /// Leave the Cluster: clean managed containers, depart storage, persist the
    /// reset, best-effort delete this Machine's replicated row, and restart the
    /// daemon into the reset. Once admitted, the leave continues if the caller
    /// disconnects. A resend on a Machine already resetting repeats the cleanup.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Upgrade`] when the upgrade receipt cannot be read,
    /// [`Error::Cleanup`] when managed container removal fails,
    /// [`Error::RecordOwner`] when the record owner has stopped,
    /// [`Error::Store`] when reset cannot be prepared or committed, and
    /// [`Error::OperationTask`] if the admitted operation task fails.
    pub async fn leave(&self, _: LeaveRequest) -> Result<Left, Error> {
        self.depart(StorageExit::Depart).await
    }

    /// Fence this Machine after the Cluster forgot it (#2927): the same reset as
    /// [`Self::leave`], except that storage keeps every copy, since a forgotten Machine
    /// cannot know what a copy elsewhere still holds.
    ///
    /// # Errors
    /// As [`Self::leave`], and [`Error::FenceRefused`] where Leave would answer
    /// [`Left::Refused`].
    pub async fn fence_forgotten(&self) -> Result<(), Error> {
        match self.depart(StorageExit::KeepEveryCopy).await? {
            Left::Left { .. } => Ok(()),
            Left::Refused { error, .. } => Err(Error::FenceRefused(error)),
        }
    }

    async fn depart(&self, exit: StorageExit) -> Result<Left, Error> {
        if self.record().phase() == LocalMachinePhase::Uninitialized {
            return Ok(Left::Left {
                phase: LeftPhase::Uninitialized,
                reset_warning: None,
            });
        }
        match crate::installer::upgrade::latest(self.owner.data_dir(), self.owner.run_dir()).await {
            Ok(Some(attempt)) if !attempt.is_terminal() => {
                return Ok(Left::Refused {
                    error: refusal(
                        RpcErrorCode::Conflict,
                        "an upgrade is running on this Machine; resend Leave once it finishes",
                    ),
                    now: self.record().now(),
                });
            }
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
        let local = self.clone();
        self.finish_mutation(async move { local.leave_admitted(exit).await })
            .await
    }

    async fn leave_admitted(&self, exit: StorageExit) -> Result<Left, Error> {
        let machine_id = self.record().id();
        let publication = match &self.cluster {
            Some(cluster) => Some(cluster.replicated.machine_publication().await),
            None => None,
        };
        // From here until the row goes, a deploy plans no Ingress Proxy here and the
        // ingress check doesn't call this Machine unchecked. Best effort: a Machine
        // whose Cluster can't take the mark still leaves.
        if let Some(publication) = &publication
            && let Err(error) = publication.mark_leaving(&machine_id, true).await
        {
            tracing::warn!(%error, "could not mark this Machine's row as leaving");
        }
        if let Err(error) = self.depart_admitted(exit).await {
            if let Some(publication) = &publication
                && let Err(error) = publication.mark_leaving(&machine_id, false).await
            {
                tracing::warn!(%error, "could not unmark this Machine's row after a failed Leave");
            }
            return Err(error);
        }
        let reset_warning = match publication {
            Some(publication) => publication
                .remove(&machine_id)
                .await
                .err()
                .map(|error| ployz_core::error_chain::inline(&error)),
            None => None,
        };
        Ok(leave_response(&self.owner, reset_warning))
    }

    /// Reset this Machine: its Containers, its storage, then its phase. Any failure
    /// leaves it Participating.
    async fn depart_admitted(&self, exit: StorageExit) -> Result<(), Error> {
        let prepared_reset = self
            .owner
            .mutate(|store| {
                if store.record().phase() == LocalMachinePhase::Resetting {
                    Ok(None)
                } else {
                    store.prepare_reset().map(Some)
                }
            })
            .await??;
        // A resend on a Machine already resetting leaves it leaving whatever happens.
        let began_reset = prepared_reset.is_some();
        let departed = self.clear_out(prepared_reset, exit).await;
        if departed.is_err()
            && began_reset
            && let Some(containers) = &self.containers
        {
            containers.departure_failed();
        }
        departed
    }

    /// Remove every managed Container, depart storage and commit the reset.
    async fn clear_out(
        &self,
        prepared_reset: Option<PreparedReset>,
        exit: StorageExit,
    ) -> Result<(), Error> {
        if let Some(containers) = &self.containers
            && let Err(error) = containers.remove_all_managed().await
        {
            return Err(Error::Cleanup(ployz_core::error_chain::inline(&error)));
        }
        self.depart_storage(exit).await?;
        if let Some(prepared_reset) = prepared_reset {
            self.owner
                .mutate(move |store| prepared_reset.commit(store))
                .await??;
        }
        Ok(())
    }
}

impl LocalMachine {
    async fn install(&self, installation: Installation) -> Result<(), Error> {
        self.depart_storage(StorageExit::Depart).await?;
        self.owner
            .mutate(move |store| store.install(installation))
            .await??;
        Ok(())
    }

    /// Demote every root through the plugin, make Docker forget each demoted name, then
    /// forget every Volume floor, so a lane from a new cluster starts at any `gen`.
    /// A Machine without Docker has no volume driver and nothing to depart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cleanup`] when the plugin cannot demote, a Volume's copy would not
    /// stop or its demote failed, or Docker keeps a name.
    async fn depart_storage(&self, exit: StorageExit) -> Result<(), Error> {
        let Some(containers) = &self.containers else {
            return Ok(());
        };
        let departure: StorageDeparture = self
            .plugin
            .call(exit.route(), &())
            .await
            .map_err(|error| Error::Cleanup(format!("storage departure failed: {error}")))?;
        // A retry answers only what it changes, so the demoted are forgotten now even
        // when a stuck Volume or another kept name fails this departure.
        let mut left = Vec::new();
        for name in &departure.demoted {
            if let Err(error) = containers.forget_volume(name).await {
                left.push(format!("Docker kept Volume {name}: {error}"));
            }
        }
        left.extend(
            departure
                .stuck
                .iter()
                .map(|name| format!("Volume {name} did not stop copying")),
        );
        left.extend(
            departure
                .failed
                .iter()
                .map(|failed| format!("Volume {} did not depart: {}", failed.name, failed.reason)),
        );
        if !left.is_empty() {
            return Err(Error::Cleanup(format!(
                "{}; retry the departure",
                left.join("; ")
            )));
        }
        containers.clear_floors().await.map_err(|error| {
            Error::Cleanup(format!(
                "the Volume floors stayed: {}",
                ployz_core::error_chain::inline(&error)
            ))
        })
    }
}

/// What a departure does with this Machine's mirrors: a leave or a fresh install
/// destroys those a writer elsewhere holds, a forgotten Machine's fence keeps every copy.
#[derive(Clone, Copy)]
enum StorageExit {
    Depart,
    KeepEveryCopy,
}

impl StorageExit {
    fn route(self) -> &'static str {
        match self {
            Self::Depart => "Storage.Demote",
            Self::KeepEveryCopy => "Storage.Seal",
        }
    }
}

/// The founder's publisher writes the Cluster network after it starts
/// answering as Participating, so on the founder a missing row means not ready
/// yet. A joined Machine whose store never held another Machine lost that store.
async fn published_network(
    record: &LocalMachineRecord,
    replicated: &ReplicatedStore,
) -> Result<ipnet::Ipv4Net, Error> {
    if let Some(network) = replicated.cluster_network().await? {
        return Ok(network);
    }
    if record.cluster_network().is_none()
        && matches!(
            replicated.held_another_machine(&record.id()).await,
            Ok(false)
        )
    {
        return Err(Error::ClusterStoreEmpty);
    }
    Err(Error::ClusterNetworkUnpublished)
}

fn registered(
    assigned_machine: Machine,
    machines: Vec<Machine>,
    target_versions: BTreeMap<String, i64>,
    sealing_key: Option<ployz_core::SealedSealingKey>,
    devices: Vec<ployz_core::DeviceKey>,
) -> Registered {
    let assigned_id = assigned_machine.id;
    Registered {
        assigned_machine,
        visible_peers: machines
            .into_iter()
            .filter(|machine| machine.id != assigned_id)
            .collect(),
        target_versions,
        sealing_key,
        devices,
    }
}

async fn machine_rtts(
    admin: &AdminClient,
    replicated: &ReplicatedStore,
) -> Result<Vec<RttObservation>, crate::corrosion::Error> {
    let machines = replicated.machines().await?.observations;
    let identities = unique_identities(machines.into_iter().map(|machine| {
        (
            IpAddr::V6(machine.management_address().0),
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

/// Read membership and RTT from the local admin socket.
///
/// Both reads must succeed; otherwise Watch keeps replicated rows.
async fn read_admin(admin: &AdminClient) -> Option<(Vec<MembershipState>, Vec<RttObservation>)> {
    let Ok((states, rtts)) = tokio::try_join!(admin.membership_states(), admin.member_rtts())
    else {
        return None;
    };
    Some((states, rtts))
}

fn rtts_by_machine(
    machines: &[Machine],
    rtts: &[RttObservation],
) -> BTreeMap<MachineId, RttStatistics> {
    let identities = unique_identities(
        machines
            .iter()
            .map(|machine| (IpAddr::V6(machine.management_address().0), machine.id)),
    );
    rtts.iter()
        .filter_map(|observation| {
            identities
                .get(&observation.address.ip())
                .copied()
                .map(|id| (id, observation.statistics.clone()))
        })
        .collect()
}

fn enroll_conflict(phase: &LocalMachinePhase) -> &'static str {
    match phase {
        LocalMachinePhase::Resetting => {
            "this Machine is resetting; resend Enroll once it is uninitialized"
        }
        LocalMachinePhase::Uninitialized
        | LocalMachinePhase::Joining
        | LocalMachinePhase::Participating
        | LocalMachinePhase::Unrecognized(_) => {
            "this Machine already holds a different enrollment; send Leave before enrolling it again"
        }
    }
}

/// Associate one value per management address; duplicate addresses are dropped.
fn unique_identities<T>(entries: impl IntoIterator<Item = (IpAddr, T)>) -> BTreeMap<IpAddr, T> {
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

fn leave_response(owner: &RecordOwner, reset_warning: Option<String>) -> Left {
    owner.request_restart();
    Left::Left {
        phase: LeftPhase::Resetting,
        reset_warning,
    }
}

fn refusal(code: RpcErrorCode, message: &str) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
#[path = "local_machine/departure_tests.rs"]
mod departure_tests;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RuntimeWatchTelemetry, leave_response, read_admin, unique_identities};
    use crate::corrosion::AdminClient;
    use ployz_core::{
        AdvertisedEndpoint, CORROSION_GOSSIP_PORT, Machine, MachineId, MachineIdentity,
        MachineName, MachineRuntime, MembershipEvidence, MembershipObservation, RttObservation,
        RttStatistics, SelectedEndpoint, WireGuardPublicKey,
    };
    const ENTRY_ID: &str = "0123456789abcdef0123456789abcdef";
    const PEER_ID: &str = "fedcba9876543210fedcba9876543210";

    fn owner() -> (tempfile::TempDir, crate::machine::RecordOwner) {
        let dir = tempfile::tempdir().unwrap();
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(dir.path()).unwrap(),
        )
        .unwrap();
        (dir, owner)
    }

    #[tokio::test]
    async fn inspect_answers_when_upgrade_evidence_cannot_be_read() {
        let (dir, owner) = owner();
        std::fs::write(dir.path().join("upgrade-attempt.json"), b"not json").unwrap();
        let details = super::LocalMachine::new(owner)
            .inspect(ployz_core::InspectRequest {
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                token: None,
                include_rtts: false,
                include_storage: false,
                telemetry: ployz_core::InspectTelemetry::None,
            })
            .await
            .unwrap();

        assert_eq!(details.upgrade, None);
    }

    #[tokio::test]
    async fn rediscovery_waits_for_installation_admission_then_applies() {
        let dir = tempfile::tempdir().unwrap();
        let public_ip: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let stale = AdvertisedEndpoint("192.0.2.250:51820".parse().unwrap());
        let mut store = crate::machine::LocalMachineStore::open(dir.path()).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("edge").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: Some(public_ip),
                    advertised_endpoints: vec![stale],
                },
                wireguard_mtu: None,
                pinned: ployz_core::PinnedAddresses {
                    public_ip: true,
                    advertised_endpoints: false,
                },
            })
            .unwrap();
        let owner = crate::machine::RecordOwner::spawn(store).unwrap();
        let local = super::LocalMachine::new(owner.clone());
        let upgrade = owner.mutation_gate().try_installation().unwrap();
        let shutdown = tokio_util::sync::CancellationToken::new();

        let rediscovery = tokio::spawn({
            let shutdown = shutdown.clone();
            async move { local.rediscover_addresses(shutdown).await }
        });
        // Paused across the Busy retries at 250ms and 750ms, which never leave the
        // runtime; resumed before the apply waits on the record owner's thread.
        tokio::time::pause();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let held = owner
            .record()
            .machine()
            .unwrap()
            .advertised_endpoints
            .clone();
        drop(upgrade);
        tokio::time::resume();
        tokio::time::timeout(std::time::Duration::from_secs(10), rediscovery)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(held, vec![stale]);
        let endpoints = owner
            .record()
            .machine()
            .unwrap()
            .advertised_endpoints
            .clone();
        assert!(
            endpoints.contains(&AdvertisedEndpoint(std::net::SocketAddr::new(
                public_ip, 51820
            ))),
            "{endpoints:?}"
        );
        assert!(!endpoints.contains(&stale), "{endpoints:?}");
    }

    #[test]
    fn leave_with_a_publication_warning_still_restarts() {
        let (_dir, owner) = owner();
        let restart_rx = owner.restart_requested();
        let left = leave_response(&owner, Some("replicated delete failed".into()));

        assert_eq!(
            left,
            ployz_core::Left::Left {
                phase: ployz_core::LeftPhase::Resetting,
                reset_warning: Some("replicated delete failed".into()),
            }
        );
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

    #[test]
    fn overlay_marks_entry_up_and_maps_inspect_rtt() {
        let entry = machine("edge", ENTRY_ID, 1);
        let peer = machine("peer", PEER_ID, 2);
        let endpoint = SelectedEndpoint("203.0.113.10:51820".parse().unwrap());
        let rtt = RttStatistics {
            median_ns: 1_500_000,
            population_stddev_ns: 250_000,
        };
        let observations = RuntimeWatchTelemetry {
            states: BTreeMap::from([(peer.management_address(), MembershipEvidence::Suspect)]),
            selected_endpoints: BTreeMap::from([(entry.id, endpoint)]),
            rtts: vec![RttObservation {
                peer_id: "peer".into(),
                address: format!("[{}]:{CORROSION_GOSSIP_PORT}", peer.management_address().0)
                    .parse()
                    .unwrap(),
                machine: None,
                statistics: rtt.clone(),
                path: None,
            }],
        }
        .overlay(vec![entry.clone(), peer.clone()], &entry.id);

        let [entry_row, peer_row] = observations.as_slice() else {
            panic!("expected entry and peer observations");
        };
        assert_eq!(entry_row.membership, MembershipObservation::Up);
        assert_eq!(entry_row.selected_endpoint, Some(endpoint));
        assert_eq!(peer_row.membership, MembershipObservation::Suspect);
        assert_eq!(peer_row.rtt, Some(rtt));
    }

    #[test]
    fn overlay_uses_down_for_peers_missing_from_admin() {
        let entry = machine("edge", ENTRY_ID, 1);
        let peer = machine("peer", PEER_ID, 2);
        let observations =
            RuntimeWatchTelemetry::default().overlay(vec![entry.clone(), peer.clone()], &entry.id);

        let [entry_row, peer_row] = observations.as_slice() else {
            panic!("expected entry and peer observations");
        };
        assert_eq!(entry_row.membership, MembershipObservation::Up);
        assert_eq!(peer_row.membership, MembershipObservation::Down);
        assert_eq!(peer_row.rtt, None);
    }

    #[tokio::test]
    async fn missing_admin_socket_is_unavailable_telemetry() {
        let admin = AdminClient::new("/no/such/ployz-admin.sock");
        assert!(read_admin(&admin).await.is_none());
    }

    fn machine(name: &str, id: &str, seed: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(id).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("203.0.113.{seed}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
            build_concurrency: None,
        }
    }
}
