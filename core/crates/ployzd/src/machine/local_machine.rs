//! Local Machine operations as Live Observation and membership changes
//! from this Machine's Local Machine Phase.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    path::PathBuf,
    sync::Arc,
};

use ployz_core::{
    EnrollRequest, Enrolled, ForgetRequest, Forgotten, InspectRequest, LeaveRequest, Left,
    LeftPhase, LocalMachinePhase, Machine, MachineDetails, MachineId, MachineIdentity, MachineList,
    MachineObservation, MachineToken, MachineUpdated, ManagementAddress, MembershipEvidence,
    PublicIpDiscovery, RegisterRequest, Registered, RpcError, RpcErrorCode, RttObservation,
    RttStatistics, SelectedEndpoint, StorageDeparture, UpdateMachineRequest, synthesize_membership,
};
use thiserror::Error;

use super::{
    EnrollPlan, LocalMachineRecord, RecordOwner, RecordOwnerStopped, StoreError, local_runtime,
    store::Installation,
};

use crate::{
    corrosion::{AdminClient, MembershipState, ReplicatedStore, membership_states_by_address},
    docker::ContainerRuntime,
    host_capacity,
    network::{NetworkError, discover_network},
};

/// Live Observation and membership operations for this Machine.
#[derive(Clone)]
pub struct LocalMachine {
    owner: RecordOwner,
    management_client: Option<[u8; 32]>,
    cluster: Option<ClusterContext>,
    containers: Option<ContainerRuntime>,
    plugin: crate::storage::Plugin,
    zpool: PathBuf,
}

mod container;
pub(crate) use container::CutRefusal;
mod management_client;
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
    #[error("Machine is not participating")]
    NotParticipating,
    #[error("Cluster store is not available")]
    ClusterStoreUnavailable,
    #[error("Cluster is not available")]
    ClusterUnavailable,
    #[error("Docker is not available")]
    DockerUnavailable,
    #[error("the roster row of lower Machine {0} is unreadable")]
    UnreadableLowerMachine(MachineId),
    #[error("the Machine at {} is active but has no readable roster row", .0.0)]
    MissingPeerRow(ManagementAddress),
    #[error("at least one Machine update is required")]
    EmptyUpdate,
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
    #[error("local operation task failed")]
    OperationTask(#[from] tokio::task::JoinError),
    #[error("Register requires a client-selected Machine Subnet")]
    MissingAssignment,
    #[error(transparent)]
    Admission(#[from] crate::mutation::Error),
    #[error(transparent)]
    Upgrade(#[from] crate::installer::upgrade::Error),
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
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_zpool(mut self, program: impl Into<PathBuf>) -> Self {
        self.zpool = program.into();
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

    /// Machines whose ingress must route a Serving Pointer before a Deploy relies on
    /// it, or `None` while the Machine list is partly replicated.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] without a Cluster store, and
    /// [`Error::Cluster`] when the Machine list cannot be read.
    pub(crate) async fn ingress_machines(&self) -> Result<Option<BTreeSet<MachineId>>, Error> {
        let cluster = self
            .cluster
            .as_ref()
            .ok_or(Error::ClusterStoreUnavailable)?;
        let machines = cluster.replicated.machines().await?;
        let membership =
            crate::retire::sample_ackers(machines, &self.record().id(), &cluster.admin).await;
        Ok(match membership.ackers {
            crate::retire::Ackers::Known(machines) if !machines.is_empty() => {
                Some(machines.into_keys().collect())
            }
            crate::retire::Ackers::Known(_)
            | crate::retire::Ackers::Incomplete { .. }
            | crate::retire::Ackers::Unread => None,
        })
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
        })
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
            runtime: local_runtime(),
            memory_total_bytes: capacity.memory_total_bytes,
            disk_total_bytes: capacity.disk_total_bytes,
            disk_available_bytes: capacity.disk_available_bytes,
        })
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
        self.finish_mutation(async move { local.enroll_admitted(request).await })
            .await
    }

    async fn enroll_admitted(&self, request: EnrollRequest) -> Result<Enrolled, Error> {
        let (plan, installation) = self
            .owner
            .mutate(move |store| store.plan_enrollment(request))
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
    /// Rejects absent assignments, invalid assignments, or unavailable storage.
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
            network: replicated.cluster_network().await?,
            machines: Vec::new(),
            target_versions: BTreeMap::new(),
        };
        let assignment = ployz_core::allocate_enrollment(&request, &snapshot, &[])?;
        publication.publish(&assignment.machine).await?;
        Ok(registered(
            assignment.machine,
            replicated.machines().await?.observations,
            replicated.version().await?,
        ))
    }

    /// Membership Observation of Machines visible from this participating Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotParticipating`] when this Machine is not
    /// participating, [`Error::ClusterStoreUnavailable`] when the Cluster
    /// store is missing, and [`Error::Cluster`] when replicated I/O fails.
    pub async fn list_machines(&self) -> Result<MachineList, Error> {
        let local = self.record();
        if local.phase() != LocalMachinePhase::Participating {
            return Err(Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let machines = replicated.machines().await?.observations;
        let states = self.membership_states().await?;
        let entry_id = local.id();
        Ok(MachineList {
            enrollment: Some(ployz_core::EnrollmentSnapshot {
                network: replicated.cluster_network().await?,
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

    /// The lowest-id Machine that invites RPC in this Machine's Membership Observation.
    /// Only that Machine answers Serving Pointer reads and publishes, so every
    /// compare-and-set it sees runs on one Corrosion node.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] when the Cluster store is missing,
    /// [`Error::Cluster`] when replicated or admin I/O fails, and
    /// [`Error::UnreadableLowerMachine`] when a Machine below the publisher has a row this
    /// Machine cannot decode, and [`Error::MissingPeerRow`] when an admin-active peer has no
    /// readable row at all, since either Machine may be the publisher another Machine sees.
    pub async fn pointer_publisher(&self) -> Result<MachineId, Error> {
        let machines = self.replicated()?.machines().await?;
        let states = self.membership_states().await?;
        let record = self.record();
        let local_id = record.id();
        let known = machines
            .observations
            .iter()
            .map(Machine::management_address)
            .chain(record.machine().map(Machine::management_address))
            .collect::<BTreeSet<_>>();
        let publisher = synthesize_membership(machines.observations, &local_id, &states)
            .iter()
            .filter(|observation| observation.invites_rpc())
            .map(|observation| observation.machine.id)
            .min()
            .unwrap_or(local_id);
        if let Some(unreadable) = machines
            .incomplete_ids
            .into_iter()
            .min()
            .filter(|unreadable| *unreadable < publisher)
        {
            return Err(Error::UnreadableLowerMachine(unreadable));
        }
        if let Some((missing, _)) = states
            .iter()
            .find(|(address, evidence)| evidence.invites_rpc() && !known.contains(address))
        {
            return Err(Error::MissingPeerRow(*missing));
        }
        Ok(publisher)
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
                .remove_machine(&request.machine_id)
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
        self.finish_mutation(async move { local.leave_admitted().await })
            .await
    }

    async fn leave_admitted(&self) -> Result<Left, Error> {
        let machine_id = self.record().id();
        let publication = match &self.cluster {
            Some(cluster) => Some(cluster.replicated.machine_publication().await),
            None => None,
        };
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
        if let Some(containers) = &self.containers
            && let Err(error) = containers.remove_all_managed().await
        {
            return Err(Error::Cleanup(ployz_core::error_chain::inline(&error)));
        }
        self.depart_storage().await?;
        if let Some(prepared_reset) = prepared_reset {
            self.owner
                .mutate(move |store| prepared_reset.commit(store))
                .await??;
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
}

impl LocalMachine {
    async fn install(&self, installation: Installation) -> Result<(), Error> {
        self.depart_storage().await?;
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
    async fn depart_storage(&self) -> Result<(), Error> {
        let Some(containers) = &self.containers else {
            return Ok(());
        };
        let departure: StorageDeparture = self
            .plugin
            .call("Storage.Demote", &())
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

fn registered(
    assigned_machine: Machine,
    machines: Vec<Machine>,
    target_versions: BTreeMap<String, i64>,
) -> Registered {
    let assigned_id = assigned_machine.id;
    Registered {
        assigned_machine,
        visible_peers: machines
            .into_iter()
            .filter(|machine| machine.id != assigned_id)
            .collect(),
        target_versions,
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
