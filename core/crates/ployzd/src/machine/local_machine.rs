//! Local Machine operations as Live Observation and membership changes
//! from this Machine's Local Machine Phase.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use ployz_core::{
    ContainerDetails, ContainerId, DeviceCode, InspectRequest, LocalMachinePhase, MachineDetails,
    MachineId, MachineName, MachineToken, MachineUpdated, ManagementClientLabel, PublicIpDiscovery,
    RpcError, UpdateMachineRequest,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    LocalMachineRecord, RecordOwner, RecordOwnerStopped, StoreError, has_cluster_identity,
    store::Rediscovered,
};

use crate::{
    corrosion::{ClusterContext, ClusterSealing, ReplicatedStore},
    docker::ContainerRuntime,
    host_capacity, host_facts,
    network::{NetworkError, discover_network},
    sealing_key::{KeyFile, SealingKey},
};

const REDISCOVERY_RETRY_MIN: Duration = Duration::from_millis(250);
const REDISCOVERY_RETRY_MAX: Duration = Duration::from_secs(5);

/// Live Observation and membership operations for this Machine.
#[derive(Clone)]
pub struct LocalMachine {
    owner: RecordOwner,
    /// The call this handle serves. An Operator's starts are checked against who admitted
    /// each Container.
    call: Call,
    cluster: Option<ClusterContext>,
    containers: Option<ContainerRuntime>,
    plugin: crate::storage::Plugin,
    zpool: PathBuf,
    proc_root: PathBuf,
    settled: container::SettledCreations,
    approvals: Arc<ConsumptionLog>,
    pacing: private_access::Pacing,
    /// The lock `SetVolumeCopy`, `Volume.Seed` and `SetVolumeSize` hold through each
    /// switch, and every create or start of a Provisioned Volume's holder waits out.
    switch: Arc<tokio::sync::Mutex<()>>,
    /// This handle runs inside a switch whose guard its caller holds, so it takes no
    /// switch lock of its own. A clone never carries it.
    in_switch: switch::InSwitch,
    /// The work management keys started that outlives its call, which each key change ends
    /// for a key it leaves short of Full scope.
    bound: bound::BoundWork,
    /// How many calls have asked for the switch lock, which a test awaits to know a call
    /// queued behind it.
    #[cfg(test)]
    switch_asked: Arc<tokio::sync::watch::Sender<usize>>,
    /// How many floor raises and key changes have asked for the floor lock, which a test
    /// awaits to know a call queued behind it.
    #[cfg(test)]
    floor_asked: Arc<tokio::sync::watch::Sender<usize>>,
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
        ) -> futures_util::future::BoxFuture<'static, Option<ContainerDetails>>
        + Send
        + Sync,
>;

mod admission;
mod bound;
mod call;
mod departure;
mod enroll;
mod membership;
pub(crate) use membership::RuntimeWatchTelemetry;
use membership::machine_rtts;
mod consumption;
mod container;
mod forget;
mod ingress_doors;
mod preview_close;
mod private_access;
mod register;
mod reserved;
pub(crate) use admission::{Admit, AdmittedBy, AdmittedSpec};
#[cfg(test)]
pub(crate) use bound::STOP_WITHIN;
pub use bound::{Bound, Cause, Recheck, Seen, Tie, Work};
pub use call::FullCaller;
pub(crate) use call::{Call, Caller, Checked, Scope};
pub(crate) use consumption::ConsumptionLog;
#[cfg(test)]
pub(crate) use container::names_content;
pub(crate) use container::{CutRefusal, PlacedHolder};
#[cfg(test)]
pub(crate) use container::{EnsurePause, EnsurePausePoint};
#[cfg(test)]
pub(crate) use private_access::Pacing;
pub(crate) use switch::{KeyChangeHeld, SwitchGuard};
mod join_approval;
mod management_client;
mod push_target;
pub(crate) mod rule_deploy;
pub(crate) mod rule_grant;
mod sealing;
pub(crate) use sealing::Error as SealingError;
mod sealing_recovery;
mod switch;
pub(crate) use sealing_recovery::Error as RecoveryError;
#[cfg(test)]
pub(crate) use sealing_recovery::back_up;
mod turns;
mod upgrade;
mod volume_copy;

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
    #[error("passkey {0} has the same key as an approved Device")]
    PasskeyIsDevice(DeviceCode),
    #[error("device {0} has the same key as a trusted passkey")]
    DeviceIsPasskey(DeviceCode),
    #[error("passkey {held} already holds this passkey's credential ID")]
    CredentialHeld { held: DeviceCode },
    /// A key one kind holds is never trusted as another, so a CI or agent key never reads
    /// as a Device or passkey.
    #[error("key {code} is already trusted here as a {held} key")]
    KeyHeld {
        code: DeviceCode,
        held: ployz_core::authority::KeyKind,
    },
    /// A key a Management Client slot holds keeps its connection, so it is never trusted as
    /// a CI or agent key.
    #[error("key {0} already holds a management client slot here")]
    KeyIsClient(DeviceCode),
    #[error("This Server refused the key change: {0}")]
    KeyRefused(ployz_core::authority::AuthorityRefusal),
    /// A SeedCopy's Copy step did not open here. The refusal names
    /// [`ployz_core::SEED_COPY_CAPABILITY`] in its details.
    #[error("This Server refused the copy of a Volume: {0}")]
    CopyRefused(ployz_core::authority::AuthorityRefusal),
    /// A preview this Server may not hold. The refusal names
    /// [`ployz_core::PREVIEW_HOME_CAPABILITY`] in its details.
    #[error("{0}")]
    PreviewRefused(ployz_core::PreviewRefusal),
    /// No version of a forget opened here. The refusal names
    /// [`ployz_core::FORGET_VERSIONS_CAPABILITY`] in its details.
    #[error("This Server refused to forget older versions: {}", forget::refusal_text(.0))]
    ForgetRefused(ployz_core::authority::AuthorityRefusal),
    /// A join for a Member this Machine saw removed. A removed Member comes back only with a
    /// new identity.
    #[error("Server {0} was removed from this Cluster. It can only join again as a new Server.")]
    MemberRemoved(MachineId),
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
    #[error(transparent)]
    WorkNotStopped(#[from] bound::WorkNotStopped),
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
    /// Device enforcement refused an Operator's creation, or a step of one under a signed
    /// version. The refusal names [`ployz_core::SIGNED_VERSION_CAPABILITY`] in its details.
    #[error("{}", admission::refusal_text(.0))]
    DeployRefused(ployz_core::authority::AuthorityRefusal),
    /// A rule build's grant failed a check before anything opened. The refusal names
    /// [`ployz_core::BUILD_GRANT_RULE_CAPABILITY`] in its details.
    #[error("This Server refused the rule build's grant: {0}")]
    RuleGrantRefused(ployz_core::authority::AuthorityRefusal),
    /// Device enforcement refused an Operator's Container change under an automatic
    /// deploy rule. The refusal names [`ployz_core::DEPLOY_RULE_CAPABILITY`] in its
    /// details.
    #[error("{0}")]
    RuleDeployRefused(rule_deploy::RuleDeployRefusal),
    /// GitHub did not prove a rule build: its token failed to verify or match the rule.
    #[error("This Server refused the rule build's GitHub token: {0}")]
    RuleProofRefused(crate::github::oidc::ProofRefusal),
    /// This Server could not read the approved branch's head from GitHub.
    #[error("This Server could not read the approved branch's head from GitHub")]
    RuleHeadUnread(#[source] crate::github::head::HeadError),
    /// Device enforcement refused an Operator's `Build` or `MintBuildGrant`. The refusal
    /// names `capability` in its details.
    #[error("{}", ployz_core::BUILD_REFUSED_BY_ENFORCEMENT)]
    BuildRefused { capability: &'static str },
    /// Device enforcement refused an Operator's `SetPrivateAccess` turning Private Access on
    /// without an approval of its auth key.
    #[error("This Server joins a tailnet only with your approval: {0}")]
    PrivateAccessRefused(ployz_core::authority::AuthorityRefusal),
    /// Device enforcement refused an Operator's `RestoreTrash` or `EmptyTrash` without a
    /// person's approval of those entries on this Server.
    #[error("This Server restores or empties its Trash only with your approval: {0}")]
    TrashRefused(ployz_core::authority::AuthorityRefusal),
    #[error("ployz.machine.join.device-approval.v1 needs a signed join approval: {0}")]
    JoinRefused(ployz_core::authority::AuthorityRefusal),
    #[error("could not keep the record of join approvals")]
    JoinRecord(#[source] std::io::Error),
    #[error("the join of Machine {0} has already been admitted")]
    JoinConsumed(MachineId),
    #[error("could not keep the record of consumed approvals")]
    ApprovalRecord(#[source] std::io::Error),
    #[error(transparent)]
    RecordOwner(#[from] RecordOwnerStopped),
    #[error(transparent)]
    Cluster(#[from] crate::corrosion::Error),
    #[error(transparent)]
    IngressProxyServiceSpec(#[from] ployz_core::IngressProxyServiceSpecError),
    #[error("This isn't a router spec Ployz builds.")]
    PrivateAccessRouterSpec,
    #[error("Private Access needs a Tailscale auth key.")]
    PrivateAccessKeyMissing,
    #[error(transparent)]
    PrivateAccessKey(crate::sealed_env::OpenError),
    #[error("Could not write the Tailscale auth key for the router.")]
    PrivateAccessKeyFile(#[source] std::io::Error),
    #[error("Could not change the Private Access node state on this Server.")]
    PrivateAccessState(#[source] std::io::Error),
    #[error(
        "Could not remove Private Access router {container_id}, so this Server is not off yet."
    )]
    PrivateAccessRouterStays {
        container_id: ContainerId,
        #[source]
        source: Box<crate::docker::Error>,
    },
    #[error("this Server opens only the Public and Tunnel Doors")]
    UnsupportedDoors,
    #[error("the Tunnel Door needs a sealed tunnel token")]
    TunnelTokenMissing,
    #[error(transparent)]
    TunnelToken(crate::sealed_env::OpenError),
    #[error("could not write the tunnel token for the Tunnel Connector")]
    TunnelTokenFile(#[source] std::io::Error),
    #[error("could not record whether the Tunnel Door is closed")]
    TunnelDoorMarker(#[source] std::io::Error),
    #[error("Ingress Proxy container {container_id} is {runtime} right after starting")]
    DoorNotRunning {
        container_id: ContainerId,
        runtime: ployz_core::ContainerRuntimeObservation,
    },
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error(transparent)]
    Docker(#[from] crate::docker::Error),
    #[error("{0}")]
    Cleanup(String),
    /// An Operator's pull asked for what this Server does only for a caller with Full
    /// access: write an image tag, or pull a free image with a registry login. The refusal
    /// names the route's capability in its details.
    #[error(transparent)]
    PullRefused(RpcError),
    #[error("local operation task failed")]
    OperationTask(#[from] tokio::task::JoinError),
    #[error("Register requires a client-selected Machine Subnet")]
    MissingAssignment,
    /// The Cluster declares Doors the client running this joiner's catch-up can't
    /// follow, or a declaration this Machine can't read.
    #[error(
        "Server {joiner} needs a newer Ployz. It cannot serve {capability}.",
        capability = ployz_core::SET_CLUSTER_DOORS_CAPABILITY
    )]
    JoinerCannotFollowDoors { joiner: MachineName },
    #[error("could not read the Cluster's declared Doors")]
    DeclaredDoorsUnread(#[source] crate::corrosion::Error),
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
            bound: bound::BoundWork::new(owner.watch()),
            approvals: owner.restart_approvals(),
            owner,
            call: Call::daemon(),
            cluster: None,
            containers: None,
            plugin: crate::storage::Plugin::default(),
            zpool: PathBuf::from("zpool"),
            proc_root: PathBuf::from("/proc"),
            settled: container::SettledCreations::default(),
            pacing: private_access::Pacing::default(),
            switch: Arc::default(),
            in_switch: switch::InSwitch::default(),
            #[cfg(test)]
            switch_asked: Arc::new(tokio::sync::watch::channel(0).0),
            #[cfg(test)]
            floor_asked: Arc::new(tokio::sync::watch::channel(0).0),
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
    pub(crate) fn with_pacing(mut self, pacing: private_access::Pacing) -> Self {
        self.pacing = pacing;
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

    #[must_use]
    pub(crate) fn with_cluster_context(mut self, cluster: Option<ClusterContext>) -> Self {
        self.cluster = cluster;
        self
    }

    /// The Cluster `replicated` and `admin` reach, answering for this Machine's record.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_cluster(
        self,
        cluster: Option<(ReplicatedStore, crate::corrosion::AdminClient)>,
    ) -> Self {
        let owner = self.owner.clone();
        self.with_cluster_context(
            cluster.map(|(replicated, admin)| ClusterContext::new(replicated, admin, owner)),
        )
    }

    #[must_use]
    pub(crate) fn with_containers(mut self, containers: Option<ContainerRuntime>) -> Self {
        self.containers = containers;
        self
    }

    /// Whether `other` keeps the creations that settled in the same place this one does.
    #[cfg(test)]
    pub(crate) fn shares_settled_creations_with(&self, other: &Self) -> bool {
        self.settled.is_shared_with(&other.settled)
    }

    pub(crate) fn has_cluster(&self) -> bool {
        self.cluster.is_some()
    }

    pub(crate) fn containers(&self) -> Option<&ContainerRuntime> {
        self.containers.as_ref()
    }

    /// The Docker this Machine runs containers on, which a Machine without it refuses.
    pub(crate) fn docker(&self) -> Result<&ContainerRuntime, Error> {
        self.containers.as_ref().ok_or(Error::DockerUnavailable)
    }

    /// Place again the Private Access router and the Ingress doors this Machine held before
    /// the daemon stopped. A step that fails is warned about and the other still runs, and
    /// what it could not place stays as it is until the operator asks again.
    pub(crate) async fn restore_held_at_boot(&self) {
        // The daemon's first firewall install closed the preview sets; no two previews meet
        // until this writes them again.
        if let Ok(containers) = self.docker()
            && let Err(error) = containers.lifecycle().sync_preview_fence().await
        {
            tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "could not write the preview fence; each preview's own Containers on this Server can't reach each other until one of its Containers starts"
            );
        }
        if let Err(error) = self.restore_private_access().await {
            tracing::warn!(
                error = %ployz_core::error_chain::inline(&error),
                "could not place the Private Access router this Machine held again; it stays as it is until the next `ployz private-access on`"
            );
        }
        let unplaced = match self.restore_doors().await {
            Ok(Some(set)) => set.failure.map(|failure| failure.error.message),
            Ok(None) => None,
            Err(error) => Some(ployz_core::error_chain::inline(&error)),
        };
        if let Some(error) = unplaced {
            tracing::warn!(
                %error,
                "could not place the Ingress doors this Machine held again; they stay as they are until the next `ployz ingress use`"
            );
        }
    }

    /// The persisted Local Machine record as last published by its owner.
    #[must_use]
    pub fn record(&self) -> Arc<LocalMachineRecord> {
        self.owner.record()
    }

    #[must_use]
    pub fn owner(&self) -> &RecordOwner {
        &self.owner
    }

    pub(crate) fn require_management_access(&self) -> Result<(), Error> {
        if let Caller::Management(remote) = self.call.caller()
            && !self.record().accepts_management_client(&remote)
        {
            return Err(Error::ManagementRevoked);
        }
        Ok(())
    }

    /// Refuse a slot change from any management client other than Cloud's.
    /// Local-socket callers carry no client key and keep full authority.
    pub(crate) fn require_slot_authority(
        caller: Caller,
        record: &LocalMachineRecord,
    ) -> Result<(), Error> {
        let Caller::Management(remote) = caller else {
            return Ok(());
        };
        let label = record.accepted_label(&remote);
        if label.is_some_and(ManagementClientLabel::is_cloud) {
            return Ok(());
        }
        tracing::warn!(
            slot = label.map_or("none", ManagementClientLabel::as_str),
            "refused SetManagementClient: only Cloud manages slots"
        );
        Err(Error::OnlyCloudManagesSlots)
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
    ) -> Option<ContainerDetails> {
        #[cfg(test)]
        if let Some(peers) = &self.peers {
            return peers(machine, container).await;
        }
        let inspected = async {
            let machines = self.replicated().ok()?.machines().await.ok()?;
            crate::mesh_peer::inspect_peer(&machines, machine, container, PEER_INSPECT_TIMEOUT)
                .await
        };
        tokio::time::timeout(PEER_INSPECT_TIMEOUT, inspected)
            .await
            .ok()
            .flatten()
    }

    /// The Cluster network, from this Machine's record or, once joined, from the
    /// replicated Cluster record.
    ///
    /// # Errors
    ///
    /// Returns when the Cluster has not published its network to this Machine.
    pub(crate) async fn cluster_network(&self) -> Result<ipnet::Ipv4Net, Error> {
        let record = self.record();
        if let Some(network) = record.cluster_network() {
            return Ok(network);
        }
        published_network(&record, self.replicated()?).await
    }

    pub(crate) fn replicated(&self) -> Result<&ReplicatedStore, Error> {
        self.cluster
            .as_ref()
            .map(|cluster| &cluster.replicated)
            .ok_or(Error::ClusterStoreUnavailable)
    }

    /// The Doors the Cluster declares on this Machine's own replica. `None` when it
    /// declares none, the value is unreadable, or the replica can't be asked: the
    /// Doors this Machine held then stay as they were.
    pub(crate) async fn declared_doors(&self) -> Option<ployz_core::IngressDoors> {
        let replicated = self.replicated().ok()?;
        match replicated.cluster_doors().await {
            Ok(declared) => declared.followed(),
            Err(error) => {
                tracing::warn!(
                    error = %ployz_core::error_chain::inline(&error),
                    "could not read the Cluster's declared Doors"
                );
                None
            }
        }
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
                let containers = self.docker()?;
                Some(ployz_core::TelemetryObservation::BridgeCapacity {
                    bridge: containers.bridge_capacity().await?,
                })
            }
            ployz_core::InspectTelemetry::Full => {
                let containers = self.docker()?;
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
            devices: record.devices().collect(),
            removed_keys: record.removed_keys().to_vec(),
            removed_grants: record.removed_grants().to_vec(),
            passkeys: record.passkeys().to_vec(),
            grants: record.grants().to_vec(),
            founding: record.founding(),
            telemetry,
            storage,
            upgrade,
            token,
            sealing: self.sealing(record.phase()).await,
        })
    }

    /// The Sealing Key this Machine holds as a member of its Cluster, read from its data dir
    /// alone: what a confirmation can carry without a replicated read. A key file that holds
    /// no key, or that could not be read, names none.
    pub(crate) fn held_sealing(&self, phase: LocalMachinePhase) -> Option<ployz_core::Sealing> {
        held_key(&self.owner, phase).map(|public_key| ployz_core::Sealing::Held { public_key })
    }

    /// The build attest key this Machine derives from the Sealing Key its file holds, on
    /// the phases [`Self::held_sealing`] reports one.
    pub(crate) fn held_attest(
        &self,
        phase: LocalMachinePhase,
    ) -> Option<ployz_core::authority::AttestKey> {
        self.held_sealing_key(phase)
            .map(|key| key.attest().attest())
    }

    /// The Sealing Key this Machine's file holds, on the phases [`Self::held_sealing`]
    /// reports one.
    pub(crate) fn held_sealing_key(&self, phase: LocalMachinePhase) -> Option<SealingKey> {
        if !has_cluster_identity(&phase) {
            return None;
        }
        SealingKey::load(self.owner.data_dir()).ok().flatten()
    }

    /// The key this Machine's file holds, or, when it holds none, the key a recovery
    /// awaits as Generate reads it, and otherwise that it needs access. `None` when the
    /// file or the Cluster's Sealing could not be read.
    async fn sealing(&self, phase: LocalMachinePhase) -> Option<ployz_core::Sealing> {
        if !has_cluster_identity(&phase) {
            return None;
        }
        SealingKey::observe(self.owner.data_dir())?
            .sealing(self.cluster_sealing())
            .await
            .inspect_err(|error| {
                tracing::warn!(%error, "could not read whether the Cluster awaits recovery");
            })
            .ok()
    }

    /// The Cluster's Sealing as Generate reads it, or `None` with no Cluster store.
    async fn cluster_sealing(&self) -> Result<Option<ClusterSealing>, crate::corrosion::Error> {
        let Ok(replicated) = self.replicated() else {
            return Ok(None);
        };
        replicated
            .cluster_sealing(self.record().recovery_seed())
            .await
            .map(Some)
    }

    async fn token(
        &self,
        public_ip: PublicIpDiscovery,
        requested_endpoints: &[ployz_core::AdvertisedEndpoint],
    ) -> Result<MachineToken, Error> {
        let record = self.record();
        let discovered = discover_network(public_ip).await?;
        let capacity = host_capacity::observe();
        // A hint for whoever approves this Machine's join; unread, it names nothing.
        let first_key = ployz_core::first_key::FirstKeyFile::beside(self.owner.data_dir())
            .peek()
            .ok()
            .flatten()
            .and_then(|state| state.waiting().map(ployz_core::first_key::LineKey::hint));
        Ok(MachineToken {
            id: record.id(),
            public_key: record.wireguard_private_key.public_key(),
            public_ip: discovered.public_ip,
            advertised_endpoints: if requested_endpoints.is_empty() {
                discovered.endpoints
            } else {
                requested_endpoints.to_vec()
            },
            runtime: host_facts::local_runtime(self.owner.data_dir()).await,
            memory_total_bytes: capacity.memory_total_bytes,
            disk_total_bytes: capacity.disk_total_bytes,
            disk_available_bytes: capacity.disk_available_bytes,
            first_key,
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
            let discovered = discovered.clone();
            match self
                .finish_mutation(move |local| async move {
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

    /// Apply a local Machine update and best-effort publish it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyUpdate`] when no field is set,
    /// [`Error::ClusterStoreUnavailable`] when the Cluster store is missing,
    /// [`Error::RecordOwner`] when the record owner has stopped,
    /// [`Error::Store`] when the update is not legal, and [`Error::Cluster`]
    /// when listing visible Machines fails.
    pub async fn update_machine(
        &self,
        request: UpdateMachineRequest,
    ) -> Result<MachineUpdated, Error> {
        // Policy edits run beside other shared work, and a Build takes no turn, so they
        // affect new admission without waiting for one. The shared turn still checks the
        // caller under a lock every key change takes. Ordinary mutations remain serialized;
        // all updates exclude installation.
        if request.update.is_policy_edit() {
            return self
                .finish_shared(move |local| async move { local.update_admitted(request).await })
                .await;
        }
        self.finish_mutation(move |local| async move { local.update_admitted(request).await })
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

/// The public half of the Sealing Key `owner`'s file holds, on the phases a Machine has a
/// Cluster identity in.
pub(super) fn held_key(
    owner: &RecordOwner,
    phase: LocalMachinePhase,
) -> Option<ployz_core::SealingPublicKey> {
    if !has_cluster_identity(&phase) {
        return None;
    }
    SealingKey::observe(owner.data_dir()).and_then(KeyFile::held)
}

#[cfg(test)]
mod tests {
    use ployz_core::{AdvertisedEndpoint, MachineName};

    fn owner() -> (tempfile::TempDir, crate::machine::RecordOwner) {
        let dir = tempfile::tempdir().unwrap();
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(dir.path()).unwrap(),
        )
        .unwrap();
        (dir, owner)
    }

    #[tokio::test]
    async fn a_confirmation_tells_only_a_sealing_key_a_member_holds() {
        use ployz_core::LocalMachinePhase;
        let (dir, owner) = owner();
        let machine = super::LocalMachine::new(owner);
        assert_eq!(machine.held_sealing(LocalMachinePhase::Participating), None);

        let key = crate::sealing_key::SealingKey::generate();
        crate::sealing_key::SealingKey::set(dir.path(), Some(&key)).unwrap();
        let held = Some(ployz_core::Sealing::Held {
            public_key: key.public_key(),
        });
        assert_eq!(machine.held_sealing(LocalMachinePhase::Participating), held);
        assert_eq!(machine.held_sealing(LocalMachinePhase::Joining), held);
        assert_eq!(machine.held_sealing(LocalMachinePhase::Uninitialized), None);
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
                    grant: None,
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
}
