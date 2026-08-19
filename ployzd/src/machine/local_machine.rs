//! Local Machine operations as Live Observation and membership changes
//! from this Machine's Local Machine Phase.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
    sync::{Arc, Mutex, MutexGuard},
};

use ployz_core::{
    InitializeRequest, Initialized, InspectRequest, JoinAccepted, JoinRequest, LocalMachinePhase,
    LocalMachineRemoved, Machine, MachineDetails, MachineId, MachineIdentity, MachineList,
    MachineObservation, MachineRemoved, MachineToken, MachineTokenRequest, MachineUpdated,
    ManagementAddress, MembershipObservation, PublicIpDiscovery, RegisterRequest, Registered,
    RemoveLocalMachineRequest, RemoveMachineRequest, ResetAccepted, RttObservation, RttStatistics,
    SelectedEndpoint, UpdateMachineRequest, WireGuardInspected, associate_wireguard_peers,
    synthesize_membership,
};
use thiserror::Error;
use tokio::sync::watch;

use super::{LocalMachineRecord, LocalMachineStore, StoreError, local_runtime};
use crate::{
    corrosion::{AdminClient, MembershipState, ReplicatedStore},
    docker::ContainerRuntime,
    network::{
        NetworkError, allocate_machine_subnet, discover_network, inspect_wireguard_device,
        management_address,
    },
};

/// Live Observation and membership operations for this Machine.
#[derive(Clone)]
pub struct LocalMachine {
    store: Arc<Mutex<LocalMachineStore>>,
    restart: watch::Sender<bool>,
    cluster: Option<ClusterContext>,
    containers: Option<ContainerRuntime>,
}

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
    pub states: BTreeMap<ManagementAddress, MembershipObservation>,
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
    Store(#[from] StoreError),
    #[error("Machine is not participating")]
    NotParticipating,
    #[error("Cluster store is not available")]
    ClusterStoreUnavailable,
    #[error("Cluster is not available")]
    ClusterUnavailable,
    #[error("Docker is not available")]
    DockerUnavailable,
    #[error("Machine name or public key already exists")]
    DuplicateMachine,
    #[error("at least one Machine update is required")]
    EmptyUpdate,
    #[error("local Machine record lock poisoned")]
    LockPoisoned,
    #[error(transparent)]
    Cluster(#[from] crate::corrosion::Error),
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error(transparent)]
    Docker(#[from] crate::docker::Error),
    #[error("{0}")]
    Cleanup(String),
    #[error("Allocator is not quiet")]
    AllocatorNotQuiet,
    #[error("this Machine is not the Allocator")]
    NotAllocator,
}

impl LocalMachine {
    /// An uninitialized Local Machine with no Cluster or Docker collaborators.
    #[must_use]
    pub fn new(store: Arc<Mutex<LocalMachineStore>>, restart: watch::Sender<bool>) -> Self {
        Self {
            store,
            restart,
            cluster: None,
            containers: None,
        }
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

    /// The persisted Local Machine record.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] when the local record lock is poisoned.
    pub fn record(&self) -> Result<LocalMachineRecord, Error> {
        Ok(self.lock_store()?.record().clone())
    }

    fn lock_store(&self) -> Result<MutexGuard<'_, LocalMachineStore>, Error> {
        self.store.lock().map_err(|_| Error::LockPoisoned)
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
        let Ok(local) = self.record() else {
            return None;
        };
        let (states, rtts) = read_admin(&cluster.admin).await?;
        Some(RuntimeWatchTelemetry {
            states: membership_states_by_address(states),
            rtts,
            selected_endpoints: local.selected_endpoints,
        })
    }

    /// Live Observation of this Machine's identity, Local Machine Phase, and
    /// advertised reachability.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] when the local record lock is poisoned,
    /// [`Error::Network`] when endpoint discovery fails, [`Error::Cluster`]
    /// when store version or RTT lookup fails, and [`Error::ClusterUnavailable`]
    /// when RTTs are requested without a Cluster.
    pub async fn inspect(&self, request: InspectRequest) -> Result<MachineDetails, Error> {
        let record = self.record()?;
        let advertised_endpoints = if !request.advertised_endpoints.is_empty() {
            request.advertised_endpoints
        } else if let Some(machine) = record.machine() {
            machine.advertised_endpoints.clone()
        } else {
            discover_network(
                request.wireguard_port,
                request
                    .public_ip_override
                    .map_or(PublicIpDiscovery::Auto, PublicIpDiscovery::Override),
            )
            .await?
            .endpoints
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
        Ok(MachineDetails {
            id: record.id(),
            phase: record.phase(),
            machine: record.machine().cloned(),
            public_key: record.wireguard_private_key.public_key(),
            advertised_endpoints,
            store_version,
            rtts,
        })
    }

    /// Live Observation of the token a peer uses to register this Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] when the local record lock is poisoned
    /// and [`Error::Network`] when endpoint discovery fails.
    pub async fn machine_token(&self, request: MachineTokenRequest) -> Result<MachineToken, Error> {
        let record = self.record()?;
        let private_key = record.wireguard_private_key;
        let discovered = discover_network(request.wireguard_port, request.public_ip).await?;
        Ok(MachineToken {
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

    /// Live Observation of the WireGuard device, enriched with Membership
    /// Observation when a Cluster is available.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Network`] when the device cannot be inspected.
    pub async fn inspect_wireguard(&self) -> Result<WireGuardInspected, Error> {
        let device = inspect_wireguard_device()?;
        let Some(cluster) = &self.cluster else {
            return Ok(WireGuardInspected { device });
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
        Ok(WireGuardInspected {
            device: associate_wireguard_peers(device, &machines, &rtts),
        })
    }

    /// Persist this Machine as the first participant and request restart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] when the local record lock is poisoned
    /// and [`Error::Store`] when initialize is not legal in the current phase.
    pub fn initialize(&self, request: InitializeRequest) -> Result<Initialized, Error> {
        let machine = self.lock_store()?.initialize(
            request.name,
            request.cluster_network,
            request.public_ip,
            request.advertised_endpoints,
            request.wireguard_mtu,
            request.cloud_pairing,
        )?;
        tracing::info!(
            name = machine.name.as_str(),
            id = machine.id.as_str(),
            "initialize accepted"
        );
        self.restart.send_replace(true);
        Ok(Initialized { machine })
    }

    /// Assign a new Machine into the Cluster from this participating Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when endpoints are missing, [`Error::NotParticipating`]
    /// when this Machine is not participating, [`Error::ClusterStoreUnavailable`]
    /// when the Cluster store is missing, [`Error::AllocatorNotQuiet`] when this
    /// Machine is named Allocator but the row is younger than 5s,
    /// [`Error::NotAllocator`] when the row names another Machine or is missing,
    /// [`Error::DuplicateMachine`] when the name or public key already exists,
    /// [`Error::Network`] when subnet allocation fails, and [`Error::Cluster`]
    /// when replicated I/O fails.
    pub async fn register(&self, request: RegisterRequest) -> Result<Registered, Error> {
        if request.advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints.into());
        }
        let record = self.record()?;
        if record.phase() != LocalMachinePhase::Participating {
            return Err(Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let assigned_machine = {
            let publication = replicated.machine_publication().await;
            match replicated.allocator().await? {
                Some(row) if row.machine_id == record.id() && row.quiet => {}
                Some(row) if row.machine_id == record.id() => {
                    return Err(Error::AllocatorNotQuiet);
                }
                Some(_) | None => return Err(Error::NotAllocator),
            }
            let snapshot = replicated.machines().await?;
            if snapshot.observations.iter().any(|machine| {
                machine.name == request.name || machine.public_key == request.public_key
            }) {
                return Err(Error::DuplicateMachine);
            }
            let network = replicated.cluster_network().await?;
            let assigned_machine = Machine {
                id: MachineId::random(),
                name: request.name,
                subnet: allocate_machine_subnet(
                    network,
                    snapshot.observations.iter().map(|machine| machine.subnet),
                )?,
                management_address: management_address(request.public_key),
                public_key: request.public_key,
                public_ip: request.public_ip,
                advertised_endpoints: request.advertised_endpoints,
                runtime: request.runtime,
            };
            // TODO(UT-140): cross-process registration stays unfenced and has no rollback.
            publication.publish(&assigned_machine).await?;
            assigned_machine
        };
        let target_versions = replicated.version().await?;
        let visible_peers = replicated
            .machines()
            .await?
            .observations
            .into_iter()
            .filter(|machine| machine.id != assigned_machine.id)
            .collect();
        Ok(Registered {
            assigned_machine,
            visible_peers,
            target_versions,
        })
    }

    /// Persist a join assignment and request restart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LockPoisoned`] when the local record lock is poisoned
    /// and [`Error::Store`] when join is not legal in the current phase.
    pub fn join(&self, request: JoinRequest) -> Result<JoinAccepted, Error> {
        let mut store = self.lock_store()?;
        store.join(
            request.registration.assigned_machine,
            request.registration.visible_peers,
            request.registration.target_versions,
            request.wireguard_mtu,
            request.cloud_pairing,
        )?;
        let machine = store
            .record()
            .machine()
            .expect("join persisted the assigned Machine");
        tracing::info!(
            name = machine.name.as_str(),
            id = machine.id.as_str(),
            "join accepted"
        );
        drop(store);
        self.restart.send_replace(true);
        Ok(JoinAccepted {})
    }

    /// Membership Observation of Machines visible from this participating Machine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotParticipating`] when this Machine is not
    /// participating, [`Error::ClusterStoreUnavailable`] when the Cluster
    /// store is missing, [`Error::LockPoisoned`] when the local record lock is
    /// poisoned, and [`Error::Cluster`] when replicated I/O fails.
    pub async fn list_machines(&self) -> Result<MachineList, Error> {
        let local = self.record()?;
        if local.phase() != LocalMachinePhase::Participating {
            return Err(Error::NotParticipating);
        }
        let replicated = self.replicated()?;
        let machines = replicated.machines().await?.observations;
        let states = match &self.cluster {
            Some(cluster) => cluster.admin.membership_states().await?,
            None => Vec::new(),
        };
        let states = membership_states_by_address(states);
        let entry_id = local.id();
        Ok(MachineList {
            machines: RuntimeWatchTelemetry {
                states,
                selected_endpoints: local.selected_endpoints,
                rtts: Vec::new(),
            }
            .overlay(machines, &entry_id),
        })
    }

    /// Apply a local Machine update and best-effort publish it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyUpdate`] when no field is set,
    /// [`Error::ClusterStoreUnavailable`] when the Cluster store is missing,
    /// [`Error::LockPoisoned`] when the local record lock is poisoned,
    /// [`Error::Store`] when the update is not legal, and [`Error::Cluster`]
    /// when listing visible Machines fails.
    pub async fn update(&self, request: UpdateMachineRequest) -> Result<MachineUpdated, Error> {
        if request.update.is_empty() {
            return Err(Error::EmptyUpdate);
        }
        let replicated = self.replicated()?;
        let visible = replicated.machines().await?.observations;
        let publication = replicated.machine_publication().await;
        let machine = self.lock_store()?.update(request.update, &visible)?;
        if let Err(error) = publication.publish(&machine).await {
            eprintln!("failed to publish updated local Machine: {error}");
        }
        Ok(MachineUpdated { machine })
    }

    /// Remove a peer Machine from the Cluster. Restarting when the target is
    /// this Machine in the resetting Local Machine Phase.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] when the Cluster store is
    /// missing, [`Error::Cluster`] when the replicated delete fails, and
    /// [`Error::LockPoisoned`] when the local record lock is poisoned.
    pub async fn remove_peer(
        &self,
        request: RemoveMachineRequest,
    ) -> Result<MachineRemoved, Error> {
        self.replicated()?
            .remove_machine(&request.machine_id)
            .await?;
        let local = self.record()?;
        if local.id() == request.machine_id && local.phase() == LocalMachinePhase::Resetting {
            self.restart.send_replace(true);
        }
        Ok(MachineRemoved {})
    }

    /// Remove this Machine: clean managed containers, persist reset, and
    /// best-effort delete the replicated row.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClusterStoreUnavailable`] when the Cluster store is
    /// missing, [`Error::DockerUnavailable`] when Docker is missing,
    /// [`Error::Cleanup`] when managed container removal fails,
    /// [`Error::LockPoisoned`] when the local record lock is poisoned, and
    /// [`Error::Store`] when reset cannot be prepared or committed.
    pub async fn remove_local(
        &self,
        request: RemoveLocalMachineRequest,
    ) -> Result<LocalMachineRemoved, Error> {
        let machine_id = self.record()?.id();
        let replicated = self.replicated()?;
        let containers = self.containers.as_ref().ok_or(Error::DockerUnavailable)?;
        let publication = replicated.machine_publication().await;
        let prepared_reset = {
            let store = self.lock_store()?;
            if store.record().phase() == LocalMachinePhase::Resetting {
                None
            } else {
                Some(store.prepare_reset()?)
            }
        };
        if let Err(error) = containers.remove_all_managed().await {
            return Err(Error::Cleanup(error.to_string()));
        }
        if let Some(prepared_reset) = prepared_reset {
            let mut store = self.lock_store()?;
            prepared_reset.commit(&mut store)?;
        }
        let reset_warning = publication
            .remove(&machine_id)
            .await
            .err()
            .map(|error| error.to_string());
        Ok(local_removal_response(
            &self.restart,
            reset_warning,
            request.restart_on_cleanup_failure,
        ))
    }

    /// Begin a Local Machine Phase reset and request daemon restart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Docker`] when managed container cleanup fails,
    /// [`Error::LockPoisoned`] when the local record lock is poisoned, and
    /// [`Error::Store`] when reset is not legal in the current phase.
    pub async fn reset(&self) -> Result<ResetAccepted, Error> {
        if let Some(containers) = &self.containers {
            containers.remove_all_managed().await?;
        }
        self.lock_store()?.begin_reset()?;
        self.restart.send_replace(true);
        Ok(ResetAccepted {})
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
            .map(|machine| (IpAddr::V6(machine.management_address.0), machine.id)),
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

fn membership_states_by_address(
    states: impl IntoIterator<Item = MembershipState>,
) -> BTreeMap<ManagementAddress, MembershipObservation> {
    states
        .into_iter()
        .filter_map(|state| match state.address.ip() {
            IpAddr::V6(address) => Some((ManagementAddress(address), state.membership)),
            IpAddr::V4(_) => None,
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{RuntimeWatchTelemetry, local_removal_response, read_admin, unique_identities};
    use crate::corrosion::AdminClient;
    use ployz_core::{
        AdvertisedEndpoint, Machine, MachineId, MachineIdentity, MachineName, MachineRuntime,
        ManagementAddress, MembershipObservation, RttObservation, RttStatistics, SelectedEndpoint,
        WireGuardPublicKey,
    };

    const ENTRY_ID: &str = "0123456789abcdef0123456789abcdef";
    const PEER_ID: &str = "fedcba9876543210fedcba9876543210";

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
            states: BTreeMap::from([(peer.management_address, MembershipObservation::Suspect)]),
            selected_endpoints: BTreeMap::from([(entry.id, endpoint)]),
            rtts: vec![RttObservation {
                peer_id: "peer".into(),
                address: format!("[{}]:51001", peer.management_address.0)
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
            id: MachineId::parse(id).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint(
                format!("203.0.113.{seed}:51820").parse().unwrap(),
            )],
            runtime: MachineRuntime::default(),
        }
    }
}
