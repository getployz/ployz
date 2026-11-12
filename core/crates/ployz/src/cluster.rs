use ployz_core::error_chain;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

use futures_util::future::join_all;
pub(crate) use ployz_core::global_creation_key;
use ployz_core::{
    BridgeEndpointCapacity, ByteCapacity, ContainerAction, ContainerId, ContainerNow,
    ContainerObservation, ContainerWant, CopyRole, DataLoss, DataLossConfirmation, Departed,
    DescribeContractRequest, DockerVolumeId, DockerVolumeName, EnsureContainerRequest,
    EnvironmentValues, ForgetRequest, Forgotten, IngressDoorsSet, InspectRequest,
    InspectStorageRequest, InspectTelemetry, LeaveRequest, Left, ListContainersRequest,
    ListImagesRequest, ListMachinesRequest, ListVolumesRequest, LiveServices, LocalMachineRemoved,
    MACHINE_STORAGE_OBSERVATION_CAPABILITY, Machine, MachineFailure, MachineId, MachineImages,
    MachineLabelKey, MachineName, MachineObservation, MachineStorageObservation, MachineSuccess,
    MachineTarget, MachineUpdate, NameMatches, Namespace, ObservedDataLoss, OpaquePayload,
    PartialResult, PrivateAccessSet, RemoveVolumeRequest, RemoveVolumesRequest, Rpc, RpcError,
    RpcErrorCode, RuntimeWatchFrame, RuntimeWatchRequest, SetIngressDoorsRequest,
    SetPrivateAccessRequest, StopOptions, StorageCapacity, TelemetryObservation,
    UnconfirmedDataLoss, VolumeInventory, VolumeRemoval, VolumeRemovalOutcome,
    decode_runtime_watch_frame, derive_live_services, op,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tonic::{Streaming, transport::Channel};

use crate::{
    connect::{
        BoxProxyStream, CONNECT_CONFIRM_TIMEOUT, ConnectError, Connector, TARGET_RPC_TIMEOUT,
        TransportError, apply_timeout, rpc_error, target_request,
    },
    context::{Connection, ConnectionSource},
    deploy::{
        DeploySnapshot, VolumeSnapshot,
        cleanup::{self, UserStarted},
    },
    service::ContainerOperationFailure,
};

mod call;
#[cfg(test)]
mod call_audit;
mod capacity;
mod container_health;
mod deploy_context;
mod fan_out;
#[cfg(test)]
mod retry;
mod routes;
#[cfg(test)]
mod routing_tests;
mod session;

pub(crate) use call::setup_outcome;
pub use call::{Budgeted, Call, Deadline, Once, Pacing, Repeat, Replay, Wait, Waiting};
pub(crate) use container_health::HealthWaitError;
use deploy_context::DeployContext;
pub use deploy_context::RuleProofs;
pub(crate) use fan_out::{fan_out, fan_out_entry_last};
pub(crate) use routes::FullRoutes;
pub(crate) use session::Refusal;
use session::Session;

const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(3);
/// Reply timeout for mutations on a live Machine, which may be mid-Deploy and
/// answer only once that work lets it through.
pub(crate) const LIVE_MUTATION_REPLY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Client {
    /// The Deploy this client executes: its log, pull credentials and approvals.
    pub(crate) deploy: DeployContext,
    /// The connection to the entry and what was confirmed behind it.
    session: Session,
    /// The Full-scope connections a Full call may dial.
    routes: FullRoutes,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MachineImagesObservation {
    pub machine_name: MachineName,
    pub images: MachineImages,
}

impl Client {
    pub(crate) fn new(
        channel: Channel,
        connection: Connection,
        source: ConnectionSource,
        connector: Arc<dyn Connector>,
    ) -> Self {
        Self {
            deploy: DeployContext::default(),
            session: Session::new(channel, connection, source, connector),
            routes: FullRoutes::default(),
        }
    }

    #[must_use]
    pub(crate) fn with_full_routes(mut self, routes: FullRoutes) -> Self {
        self.routes = routes;
        self
    }

    /// The Full-scope connections a Full call may dial.
    pub(crate) fn full_routes(&self) -> &FullRoutes {
        &self.routes
    }

    pub(crate) fn full_machines(&self) -> BTreeSet<MachineId> {
        self.routes.machines(self.session.entry)
    }

    /// Whether this client reaches `machine` with Full scope: over SSH or the local
    /// socket, or over a Management connection dialled as a Device when `device` says one
    /// is held. Only a client [`Self::with_full_routes`] built can.
    pub(crate) fn reaches_full(&self, machine: &MachineId, device: bool) -> bool {
        self.routes.reach(
            machine,
            device,
            self.session.entry.as_ref(),
            self.session.connection(),
        )
    }

    async fn full_client(&self, target: &MachineTarget) -> Result<Option<Self>, ConnectError> {
        self.session.ended()?;
        if self.routes.is_empty()
            || self
                .session
                .entry
                .is_some_and(|id| id.as_str() == target.as_str())
        {
            return Ok(None);
        }
        let Some(connection) = self.routes.to(target.as_str()) else {
            return Ok(None);
        };
        let mut client = Box::pin(crate::connect::connect_one(
            connection,
            self.session.source(),
            self.session.connector(),
            false,
            self.session.cluster.as_ref().map(|cluster| &cluster.expect),
        ))
        .await?;
        client.session.ending_with(&self.session);
        self.session.ended()?;
        Ok(Some(client))
    }

    #[must_use]
    pub(crate) fn with_restart_approval(
        mut self,
        approval: Option<ployz_core::authority::Signed<ployz_core::authority::RestartApproval>>,
    ) -> Self {
        self.deploy.set_restart(approval);
        self
    }

    /// Hand every Server the approval of the Deploy this client runs.
    #[must_use]
    pub(crate) fn with_deploy_approval(
        mut self,
        approval: Option<ployz_core::authority::Signed<ployz_core::authority::DeployApproval>>,
    ) -> Self {
        self.deploy.set_deploy(approval);
        self
    }

    /// Hand every Server the rule's proof for the Service each change belongs to, in
    /// place of a person's deploy approval.
    #[must_use]
    pub(crate) fn with_rule_proofs(mut self, proofs: RuleProofs) -> Self {
        self.deploy.set_proofs(proofs);
        self
    }

    /// Hand every Server the teardown of what this client's Deploy takes off it.
    #[must_use]
    pub(crate) fn with_teardown(
        mut self,
        teardown: Option<ployz_core::authority::TeardownApproval>,
    ) -> Self {
        self.deploy.set_teardown(teardown);
        self
    }

    /// What the first dial confirmed about the entry's Cluster; None when it expected none.
    /// A redial checks the Cluster again but leaves this as the first dial found it.
    #[must_use]
    pub fn cluster(&self) -> Option<&crate::connect::ClusterCheck> {
        self.session.cluster.as_ref().map(|cluster| &cluster.check)
    }

    /// Record the Cluster the dial expected and what it confirmed about it.
    pub(crate) fn expect_cluster(&mut self, cluster: crate::connect::ExpectedCluster) {
        self.session.cluster = Some(Box::new(cluster));
    }

    /// What the dial confirmed about the Cluster, for the connection walk to complete.
    pub(crate) fn cluster_check_mut(&mut self) -> Option<&mut crate::connect::ClusterCheck> {
        self.session
            .cluster
            .as_deref_mut()
            .map(|cluster| &mut cluster.check)
    }

    /// Whether the confirmed entry holds `expect`'s Sealing Key. Inspect is a read every
    /// supported daemon answers; one that predates the field reports no key. Like the
    /// confirmation, one try within [`CONNECT_CONFIRM_TIMEOUT`], else
    /// [`ConnectError::EntryNotReady`].
    pub(crate) async fn check_cluster(
        &mut self,
        expect: &crate::connect::ClusterExpectation,
        confirmed: Option<ployz_core::Sealing>,
    ) -> Result<crate::connect::ClusterCheck, ConnectError> {
        // A confirmation that carried the held key answers without Inspect.
        let (sealing, machine_id, predates) =
            if let (Some(held @ ployz_core::Sealing::Held { .. }), Some(id)) =
                (confirmed, self.session.entry)
            {
                (Some(held), id, None)
            } else {
                let inspect = async {
                    let payload = op::Inspect::into_request(ployz_core::InspectRequest::default())
                        .encode()?;
                    self.session.call_once::<op::Inspect>(payload).await
                };
                let details = tokio::time::timeout(CONNECT_CONFIRM_TIMEOUT, inspect)
                    .await
                    .map_err(|_| ConnectError::EntryNotReady)??;
                let predates = crate::connect::predates_sealing(&details);
                (
                    details.sealing,
                    self.session.entry.unwrap_or(details.id),
                    predates,
                )
            };
        expect.check(machine_id, sealing, predates)
    }

    #[must_use]
    pub fn connection(&self) -> &Connection {
        self.session.connection()
    }

    #[must_use]
    pub fn connection_source(&self) -> &ConnectionSource {
        self.session.source()
    }

    /// The entry Machine this client reaches, as the daemon on its current channel
    /// described itself when confirmed. None when it didn't.
    pub(crate) fn entry_machine(&self) -> Option<MachineId> {
        self.session.entry
    }

    /// The Server this client enters, as people read it: its connection's name, else its
    /// Machine ID.
    pub(crate) fn server(&self) -> Option<String> {
        self.session
            .connection()
            .server()
            .or_else(|| self.session.entry.map(|machine| machine.to_string()))
    }

    /// The transition a Deploy's shared client makes: see [`Session::redial_shared`].
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`], when the
    /// entry doesn't answer on the new channel.
    pub(crate) async fn redial_shared(&self) -> Result<(), ConnectError> {
        self.session.redial_shared().await
    }

    /// What ends this client, for a stream it opened to check as it reads.
    pub(crate) fn refusal(&self) -> Refusal {
        self.session.refusal()
    }

    /// One-shot check that this channel reaches a daemon, as [`Session::confirm_entry`].
    ///
    /// # Errors
    ///
    /// Returns a transport or codec error, or [`ConnectError::EntryNotReady`]
    /// when the daemon does not answer within [`CONNECT_CONFIRM_TIMEOUT`].
    pub(crate) async fn confirm_entry(
        &mut self,
    ) -> Result<Option<ployz_core::Sealing>, ConnectError> {
        self.session.confirm_entry().await
    }

    /// Whether the Machine at `target`, the entry when there is none, promises `capability` in
    /// the contract it describes. A caller words what a Machine that doesn't means to it.
    ///
    /// # Errors
    ///
    /// Returns the transport or remote error the describe met.
    pub(crate) async fn supports(
        &self,
        target: Option<&MachineTarget>,
        capability: &str,
    ) -> Result<bool, ConnectError> {
        Ok(self
            .call_shared::<op::DescribeContract>(
                DescribeContractRequest {},
                Call::once().toward(target),
            )
            .await?
            .supports(capability))
    }

    /// The id of the Machine this client enters by.
    ///
    /// # Errors
    ///
    /// Returns a transport or remote error when the entry does not describe itself.
    pub(crate) async fn entry_id(&self) -> Result<MachineId, ConnectError> {
        self.session.entry_id().await
    }

    /// Open the stream `Op` names, on the connection its scope routes to.
    ///
    /// A Full RPC with a target dials that target directly, as [`Self::send`] does for a
    /// unary one; the rest go through the entry with the target named. `input` is the one
    /// request of a server stream, or the whole feed of a client stream. A client stream
    /// stops reading its feed once this client ends.
    ///
    /// # Errors
    ///
    /// Returns the transport error the open met, naming `target` when the Server refused it.
    pub(crate) async fn open_stream<Op: StreamRpc>(
        &self,
        target: Option<&MachineTarget>,
        input: impl tokio_stream::Stream<Item = OpaquePayload> + Send + 'static,
    ) -> Result<Streaming<OpaquePayload>, TransportError> {
        let direct = if Op::ENTRY_ONLY {
            None
        } else {
            self.direct_client(Op::PATH, target)
                .await
                .map_err(TransportError::client_ended)?
        };
        let sent_to = if direct.is_some() { None } else { target };
        let input: std::pin::Pin<Box<dyn tokio_stream::Stream<Item = OpaquePayload> + Send>> =
            if Op::CLIENT_STREAM {
                Box::pin(self.session.until_ended(input))
            } else {
                Box::pin(input)
            };
        let opened = direct
            .as_ref()
            .unwrap_or(self)
            .session
            .open(Op::PATH, Op::GZIP, target_request(input, sent_to))
            .await;
        match target {
            Some(target) => opened.map_err(|error| error.name_refusal(Op::PATH, target)),
            None => opened,
        }
    }

    /// The Cluster as the first Runtime Watch frame shows it.
    pub(crate) async fn runtime_frame(&self) -> Result<RuntimeWatchFrame, RpcError> {
        let payload = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
            .encode()
            .map_err(|error| RpcError::from(ConnectError::from(error)))?;
        let mut stream = self
            .open_stream::<op::RuntimeWatch>(None, tokio_stream::once(payload))
            .await
            .map_err(|error| error.to_rpc_error())?;
        let Some(frame) = stream
            .message()
            .await
            .map_err(|status| TransportError::from_stream_status(status).to_rpc_error())?
        else {
            return Err(RpcError::unavailable(
                "Runtime Watch ended before its first frame",
            ));
        };
        decode_runtime_watch_frame(&frame)
            .map_err(|error| error_chain::rpc_error(RpcErrorCode::Internal, &error))
    }

    /// [`Self::send_path`] for an RPC of the catalog.
    async fn send<T: Rpc>(
        &self,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
    ) -> Result<T::Response, ConnectError> {
        self.send_path(T::PATH, payload, target)
            .await?
            .decode::<T>()
            .map_err(ConnectError::Codec)
    }

    /// The one attempt every unary call makes, answered by the daemon's own error when it
    /// sent one. A Full RPC with a target goes straight to it; the rest enter by this
    /// client's own channel with the target named.
    async fn send_path(
        &self,
        path: &'static str,
        payload: OpaquePayload,
        target: Option<&MachineTarget>,
    ) -> Result<ployz_core::RpcResponse, ConnectError> {
        let direct = self.direct_client(path, target).await?;
        let target = if direct.is_some() { None } else { target };
        direct
            .as_ref()
            .unwrap_or(self)
            .session
            .unary(path, payload, target)
            .await
    }

    /// The client that dials `target` itself, when `path` is an RPC only a Full connection
    /// may send and one is held for it.
    async fn direct_client(
        &self,
        path: &str,
        target: Option<&MachineTarget>,
    ) -> Result<Option<Self>, ConnectError> {
        match target {
            Some(target) if dials_direct(path) => self.full_client(target).await,
            _ => Ok(None),
        }
    }

    /// List Docker Volumes on each Machine that invites an RPC.
    ///
    /// Down, Unknown, and Unrecognized are omitted. Up and Suspect are read
    /// independently with bounded transient retries.
    /// Keep each probed target's success or typed failure.
    pub async fn list_volumes(
        &self,
        machines: &[MachineObservation],
    ) -> PartialResult<VolumeInventory, RpcError> {
        fan_out(machines, |machine| {
            let client = self.clone();
            async move {
                list_volumes_on_machine(client, machine.machine.id)
                    .await
                    .map(Some)
            }
        })
        .await
    }

    /// Inspect storage on each Machine that invites an RPC.
    pub async fn inspect_storage(
        &self,
        machines: &[MachineObservation],
    ) -> PartialResult<StorageCapacity, RpcError> {
        fan_out(machines, |machine| {
            let mut client = self.clone();
            async move {
                client
                    .call::<op::InspectStorage>(
                        InspectStorageRequest {},
                        Call::read()
                            .to(&MachineTarget::from(&machine.machine.id))
                            .budget(crate::connect::target_read_timeout()),
                    )
                    .await
                    .map(Some)
                    .map_err(crate::connect::rpc_error)
            }
        })
        .await
    }

    /// Destroy named Docker Volumes. The list is the confirmation.
    ///
    /// Each volume is identified by Machine plus name. Fan-out is a Partial
    /// Result: each requested identity carries removal, failure, or omission
    /// evidence. Machines that do not invite RPC are omitted. Forced removal is off
    /// unless `force` is set.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn remove_volumes(
        &mut self,
        request: RemoveVolumesRequest,
    ) -> Result<Vec<VolumeRemoval>, RpcError> {
        let machines = self
            .machine_list_uncapped()
            .await
            .map_err(RpcError::from)?
            .machines;
        Ok(remove_volumes_on(self, &machines, request).await)
    }

    /// Which Machines hold each of the Docker Volumes `sought`, as the Docker Volume
    /// or as a mirror slot: the evidence a Deploy that deletes Volume data is reviewed
    /// against. Every Machine that did not answer, or could not read one of them, is
    /// named in `unanswered`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_volumes(
        &mut self,
        sought: Vec<DockerVolumeName>,
    ) -> Result<ployz_store::VolumeObservation, RpcError> {
        let machines = self
            .machine_list_uncapped()
            .await
            .map_err(RpcError::from)?
            .machines;
        let inspector = self.clone();
        // Seeking nothing asks only who answers: storage holds nothing sought.
        let storage: &[MachineObservation] = match sought.is_empty() {
            true => &[],
            false => &machines,
        };
        let (listed, inspected) = tokio::join!(
            self.list_volumes(&machines),
            inspector.inspect_storage(storage)
        );
        let mut unanswered: BTreeSet<MachineId> = listed
            .failures
            .iter()
            .chain(&inspected.failures)
            .map(|failure| failure.machine_id)
            .chain(listed.omissions.iter().copied())
            .collect();
        let mut held = BTreeSet::new();
        for success in listed.successes {
            for failure in &success.value.failures {
                if sought.contains(&failure.id.name) {
                    unanswered.insert(failure.id.machine_id);
                }
            }
            held.extend(
                success
                    .value
                    .volumes
                    .into_iter()
                    .map(|volume| volume.id)
                    .filter(|id| sought.contains(&id.name)),
            );
        }
        for success in inspected.successes {
            held.extend(
                success
                    .value
                    .roles()
                    .filter(|(name, role)| *role == CopyRole::Slot && sought.contains(name))
                    .map(|(name, _)| DockerVolumeId {
                        machine_id: success.machine_id,
                        name: name.clone(),
                    }),
            );
        }
        Ok(ployz_store::VolumeObservation {
            sought,
            held: held.into_iter().collect(),
            unanswered: unanswered.into_iter().collect(),
            failure: None,
        })
    }

    /// Every Service with a Container on a Machine that answered, by Namespace: what a
    /// full Deploy's prune may remove. Every Machine that should answer and did not is
    /// named in `unanswered`.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_running(&mut self) -> Result<ployz_store::RunningObservation, RpcError> {
        let machines = self
            .machine_list_uncapped()
            .await
            .map_err(RpcError::from)?
            .machines;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(RpcError::from)?;
        let services: BTreeSet<_> = live
            .containers
            .successes
            .iter()
            .flat_map(|success| &success.value)
            .map(ContainerObservation::identity)
            .collect();
        // A Machine already marked down is left out, as the Deploy's prune leaves it.
        let unanswered: BTreeSet<MachineId> = live
            .containers
            .failures
            .iter()
            .map(|failure| failure.machine_id)
            .collect();
        // The entry's own path to a Server that didn't answer says why, when large
        // packets to it are dropped (#3490). A failed dial sends only small packets, so
        // a Server the entry couldn't reach is left out, as the router leaves it.
        let dropped = machines
            .iter()
            .filter(|machine| {
                machine
                    .path
                    .is_some_and(|path| path.large_packets == ployz_core::LargePackets::Dropped)
                    && live.containers.failures.iter().any(|failure| {
                        failure.machine_id == machine.machine.id
                            && failure.error.target_unanswered()
                            && !failure.error.undialed()
                    })
            })
            .map(|machine| machine.machine.name.clone())
            .collect();
        Ok(ployz_store::RunningObservation {
            services: services.into_iter().collect(),
            unanswered: unanswered.into_iter().collect(),
            failure: None,
            dropped,
        })
    }

    /// Every copy of every Volume on every Machine, by role, slots included. Every
    /// Machine that did not answer is named in `unanswered`, never assumed empty.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when listing Machines fails.
    pub async fn observe_copies(&mut self) -> Result<crate::sdk::CopyObservation, RpcError> {
        let machines = self
            .machine_list_uncapped()
            .await
            .map_err(RpcError::from)?
            .machines;
        let inspected = self.inspect_storage(&machines).await;
        let unanswered: BTreeSet<MachineId> = inspected
            .failures
            .iter()
            .map(|failure| failure.machine_id)
            .chain(inspected.omissions.iter().copied())
            .collect();
        let copies = inspected
            .successes
            .iter()
            .flat_map(|success| {
                success
                    .value
                    .roles()
                    .map(|(name, role)| crate::sdk::ObservedCopy {
                        machine_id: success.machine_id,
                        name: name.clone(),
                        role,
                    })
            })
            .collect();
        Ok(crate::sdk::CopyObservation {
            copies,
            unanswered: unanswered.into_iter().collect(),
        })
    }

    /// Live Observation of Data Loss that removing `machine` would cause.
    ///
    /// This is not a complete Cluster view. Mutates nothing: it is safe to
    /// call when the operator then cancels.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible or is
    /// ambiguous, or when the Machine did not respond so Data Loss cannot be
    /// listed.
    pub async fn data_loss_if_machine_removed(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<ObservedDataLoss, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let observation = visible_machine(machine, &machines)?;
        data_loss_on_machine(self, observation).await
    }

    /// Remove `machine` after an exact Data Loss confirmation.
    ///
    /// Re-reads Data Loss at execute time. Fresh Data Loss the confirmation does
    /// not cover refuse the removal. Confirmed identities that disappeared
    /// are ignored.
    /// Resets the Machine. A reset warning is returned, not swallowed.
    /// Refused before reset or membership mutation when this is the last Machine
    /// and a Management Client other than Cloud holds a key, or Cloud holds it and
    /// `remover` isn't Cloud.
    ///
    /// Sending it again is the retry: every guard runs again while the Machine is listed.
    /// A Machine ID the list no longer holds was already forgotten, so this answers removed,
    /// with a warning that its reset can't be confirmed: only a Leave reply confirms one.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible or is
    /// the current entry while another Machine is visible, when the Machine
    /// is the last Machine and its holders refuse it or can't be read, when the Machine
    /// did not respond so Data Loss cannot be listed, when the confirmation does not cover the
    /// fresh Data Loss, or when reset or shared-row removal fails.
    pub async fn remove_machine(
        &mut self,
        machine: &MachineTarget,
        confirm_data_loss: &DataLossConfirmation,
        remover: Remover,
    ) -> Result<LocalMachineRemoved, RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let Some(observation) = listed_unless_forgotten(machine, &machines)? else {
            return Ok(Departed::AlreadyForgotten.into());
        };
        let selected = observation.machine.id;
        let current = self
            .call::<op::DescribeContract>(DescribeContractRequest {}, Call::read())
            .await
            .map_err(RpcError::from)?
            .machine_id;
        if selected == current && machines.len() > 1 {
            return Err(RpcError::invalid_argument(
                "the current entry Machine cannot be removed while another Machine is visible",
            ));
        }
        let hold = refuse_last_managed(self, &machines, selected).await?;
        if hold == CloudHold::Last && remover == Remover::Operator {
            return Err(cloud_holds_last(selected));
        }
        evict_machine(self, observation, confirm_data_loss, current).await
    }

    /// Remove Cluster membership for `machine` without resetting it.
    ///
    /// Refused before membership mutation when this is the last Machine and any
    /// Management Client holds a key: Cloud lets go of its last Machine only by resetting it.
    /// Also refused while the Machine answers with a copy of any Volume, which only its
    /// reset demotes, or replies with an error; a Machine that sends no reply goes.
    ///
    /// Sending it again is the retry: every guard runs again while the Machine is listed,
    /// and a Machine ID the list no longer holds was already forgotten.
    ///
    /// # Errors
    ///
    /// Returns a generated [`RpcError`] when the Machine is not visible, when
    /// it is the last Machine and a Management Client holds a key or its holders
    /// can't be read, when it answers holding a Volume copy or replies with an error,
    /// or when shared-row removal fails.
    pub async fn remove_machine_membership(
        &mut self,
        machine: &MachineTarget,
    ) -> Result<(), RpcError> {
        self.remove_membership(machine, DeadConfirmed::Yes).await
    }

    /// As [`Self::remove_machine_membership`], but a Server confirmed while it answered
    /// that stops answering before it goes is refused: dead was never typed for it.
    pub(crate) async fn remove_membership(
        &mut self,
        machine: &MachineTarget,
        dead: DeadConfirmed,
    ) -> Result<(), RpcError> {
        let machines = self.machines().await.map_err(RpcError::from)?;
        let Some(observation) = listed_unless_forgotten(machine, &machines)? else {
            return Ok(());
        };
        let selected = observation.machine.id;
        if refuse_last_managed(self, &machines, selected).await? == CloudHold::Last {
            return Err(cloud_holds_last(selected));
        }
        let held = match (self.held_copies(observation).await?, dead) {
            (HeldCopies::Dead, DeadConfirmed::Yes) => BTreeSet::new(),
            (HeldCopies::Dead, DeadConfirmed::No) => {
                return Err(stopped_answering(&observation.machine));
            }
            (HeldCopies::Answered(held), _) => held,
            (HeldCopies::Gone(_), _) => return Ok(()),
        };
        if !held.is_empty() {
            return Err(copies_left_behind(
                &observation.machine.name,
                held.iter()
                    .map(|name| Namespace::declared_volume_name(name).unwrap_or(name.as_str())),
            ));
        }
        forget(self, selected).await
    }

    pub(crate) async fn held_copies(
        &self,
        machine: &MachineObservation,
    ) -> Result<HeldCopies, RpcError> {
        if !machine.invites_rpc() {
            return Ok(HeldCopies::Dead);
        }
        let mut client = self.clone();
        match client
            .call::<op::InspectStorage>(
                InspectStorageRequest {},
                Call::read()
                    .to(&MachineTarget::from(&machine.machine.id))
                    .budget(crate::connect::target_read_timeout()),
            )
            .await
        {
            Ok(storage) => Ok(HeldCopies::Answered(
                storage.roles().map(|(name, _)| name.clone()).collect(),
            )),
            Err(error) if error.target_unanswered() => Ok(HeldCopies::Dead),
            Err(error) => {
                let error = rpc_error(error);
                let unlisted = error.server_unlisted(&machine.machine.id);
                // Often only the entry was busy, as when several forgets share it.
                let unchecked = RpcError::unavailable(format!(
                    "Couldn't check whether Server {} holds Volume copies. No changes made. \
                     Run the command again, one Server at a time if you're forgetting several.",
                    machine.machine.name
                ))
                .with_causes(std::iter::once(error.message).chain(error.cause).collect());
                if unlisted {
                    Ok(HeldCopies::Gone(unchecked))
                } else {
                    Err(unchecked)
                }
            }
        }
    }

    /// List images on each target Machine with independent bounded retries.
    pub async fn list_images(
        &self,
        reference: Option<String>,
        targets: &[Machine],
    ) -> PartialResult<MachineImagesObservation, RpcError> {
        fan_out(targets, |machine| {
            let mut client = self.clone();
            let reference = reference.clone();
            async move {
                let images = client
                    .call::<op::ListImages>(
                        ListImagesRequest {
                            reference,
                            last_tagged: false,
                        },
                        Call::read()
                            .to(&MachineTarget::from(&machine.id))
                            .budget(crate::connect::target_read_timeout()),
                    )
                    .await
                    .map_err(crate::connect::rpc_error)?;
                Ok(Some(MachineImagesObservation {
                    machine_name: machine.name.clone(),
                    images,
                }))
            }
        })
        .await
    }

    pub async fn dial_proxy(
        &self,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        self.session.ended()?;
        let stream = self
            .session
            .connector()
            .dial_proxy(self.session.connection(), network, address)
            .await;
        self.session.ended()?;
        stream
    }

    /// Membership Observation of Machines visible from this entry Machine.
    ///
    /// # Errors
    ///
    /// Returns a connection or remote RPC error from `ListMachines`. The whole
    /// call, including unary retries, is capped at 10 seconds and surfaces as
    /// deadline exceeded when that cap elapses.
    pub async fn machines(&mut self) -> Result<Vec<MachineObservation>, ConnectError> {
        self.machine_list().await.map(|list| list.machines)
    }

    /// The entry Machine's listing of the Cluster, with the enrollment snapshot it carries.
    pub async fn machine_list(&mut self) -> Result<ployz_core::MachineList, ConnectError> {
        match tokio::time::timeout(TARGET_RPC_TIMEOUT, self.machine_list_uncapped()).await {
            Ok(result) => result,
            Err(_) => Err(tonic::Status::deadline_exceeded("target Machine RPC timed out").into()),
        }
    }

    /// [`Self::machine_list`] with only the unary retries to bound it. The reads that listed
    /// Machines this way before the Machine list had a deadline keep it: a slow daemon still
    /// answers them.
    pub(crate) async fn machine_list_uncapped(
        &mut self,
    ) -> Result<ployz_core::MachineList, ConnectError> {
        self.call::<op::ListMachines>(ListMachinesRequest {}, Call::read())
            .await
    }

    /// Fill in each reachable Machine's storage; returns the Machines left without it.
    pub(crate) async fn observe_machine_storage(
        &self,
        machines: &mut [MachineObservation],
    ) -> StorageGaps {
        let observed = fan_out(
            machines.iter().filter(|machine| machine.invites_rpc()),
            |machine| observe_machine_storage(self.clone(), machine.machine.id),
        )
        .await;
        for success in observed.successes {
            if let Some(machine) = machines
                .iter_mut()
                .find(|machine| machine.machine.id == success.machine_id)
            {
                machine.storage = success.value;
            }
        }
        StorageGaps {
            failures: observed.failures,
            omissions: observed.omissions,
        }
    }

    /// Read the space under each reachable Machine's Docker root. A Machine that fails or is
    /// slow to answer is left out, since the reading only ever adds a warning.
    pub(crate) async fn observe_docker_disks(
        &self,
        machines: &[MachineObservation],
    ) -> Vec<(MachineId, ByteCapacity)> {
        let read = fan_out(machines, |observed| async move {
            let request = InspectRequest {
                telemetry: InspectTelemetry::Full,
                ..Default::default()
            };
            let target = MachineTarget::from(&observed.machine.id);
            let read = self.call_shared::<op::Inspect>(request, Call::once().to(&target));
            Ok::<_, ()>(
                match tokio::time::timeout(STORAGE_OBSERVATION_TIMEOUT, read).await {
                    Ok(Ok(details)) => match details.telemetry {
                        Some(TelemetryObservation::Full { host, .. }) => Some(host.docker_root()),
                        _ => None,
                    },
                    Ok(Err(_)) | Err(_) => None,
                },
            )
        })
        .await;
        read.successes
            .into_iter()
            .map(|success| (success.machine_id, success.value))
            .collect()
    }

    /// List Containers on every visible Machine and derive the observed Services.
    ///
    /// `environment` chooses whether observations carry real environment values
    /// or redacted ones.
    ///
    /// # Errors
    /// Returns an error when Machines cannot be listed or a listing task fails.
    pub async fn live_services(
        &mut self,
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = self.machines().await?;
        self.live_services_from(&machines, environment).await
    }

    pub(crate) async fn live_services_from(
        &self,
        machines: &[MachineObservation],
        environment: EnvironmentValues,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        self.live_services_until(machines, environment, |_| false)
            .await
    }

    /// [`Self::live_services_from`], returning early once `found` holds for the
    /// answers so far. An early result leaves out the Machines yet to answer, as
    /// neither a failure nor an omission, so only what `found` checked is complete.
    pub(crate) async fn live_services_until(
        &self,
        machines: &[MachineObservation],
        environment: EnvironmentValues,
        found: impl Fn(&LiveServices<RpcError>) -> bool,
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let mut live = derive_live_services(PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        let mut tasks = JoinSet::new();
        for machine in machines {
            if machine.invites_rpc() {
                tasks.spawn(list_on_machine(
                    self.clone(),
                    machine.machine.id,
                    environment,
                ));
            } else {
                live.containers.omissions.push(machine.machine.id);
            }
        }
        // TODO: retain target failures and omissions beside successful container
        // observations; an entry-relative answer remains partial evidence.
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(success)) => live.containers.successes.push(success),
                Ok(Err(failure)) => live.containers.failures.push(failure),
                Err(error) => return Err(error.into()),
            }
            if found(&live) {
                return Ok(live);
            }
        }
        Ok(live)
    }

    /// Converge one Container on `machine_id` to `want`. A refusal answers as the error
    /// the replaced Container verbs answered.
    pub async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<ContainerNow, RpcError> {
        self.ensure_settled(machine_id, want, timeout)
            .await
            .map(|(now, _)| now)
    }

    /// Bring `machine`'s Ingress Proxy, its holder and Caddy, to its daemon's pinned
    /// wiring for `request`'s Doors. The daemon may pull both images first, so no client
    /// deadline applies, as for a Deploy's create. A daemon from before the route
    /// answers Unsupported, naming `machine`.
    ///
    /// # Errors
    ///
    /// Returns the daemon's refusal, or the transport failure naming `machine`. A Docker
    /// stage that failed after an earlier one completed answers in the result's `failure`.
    pub async fn set_ingress_doors(
        &self,
        machine: &MachineId,
        request: SetIngressDoorsRequest,
    ) -> Result<IngressDoorsSet, RpcError> {
        self.call_shared::<op::SetIngressDoors>(
            request,
            Call::once().to(&MachineTarget::from(machine)),
        )
        .await
        .map_err(|error| answered_by(machine, error))
    }

    /// Turn Private Access on or off on `machine`: place the Tailscale router with the
    /// sealed auth key, or remove it. The router's start is awaited, with no deadline
    /// beyond the daemon's own settle window. A daemon from before the route answers
    /// Unsupported, naming `machine`.
    ///
    /// # Errors
    ///
    /// Returns the daemon's refusal, or the transport failure naming `machine`. A router
    /// that was placed and did not serve answers in the result's `failure`.
    pub async fn set_private_access(
        &self,
        machine: &MachineId,
        request: SetPrivateAccessRequest,
    ) -> Result<PrivateAccessSet, RpcError> {
        self.call_shared::<op::SetPrivateAccess>(
            request,
            Call::once().to(&MachineTarget::from(machine)),
        )
        .await
        .map_err(|error| answered_by(machine, error))
    }

    pub(crate) async fn ensure_settled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<(ContainerNow, Option<ployz_core::HealthVerdict>), RpcError> {
        self.ensure_unsettled(machine_id, want, timeout)
            .await?
            .into_settled()
    }

    /// Converge one Container and keep a refusal as the owner typed it, for
    /// callers that act on which rotation rule refused.
    pub(crate) async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<ployz_core::EnsuredContainer, RpcError> {
        let target = MachineTarget::from(machine_id);
        crate::sealed_compat::admit(self, &target, &want).await?;
        let carried = self.deploy.carried(&want);
        let payload = op::EnsureContainer::into_request(EnsureContainerRequest {
            teardown: carried.teardown,
            preview: carried.preview,
            want,
            approval: carried.restart,
            deploy: carried.deploy,
            rule: carried.rule,
        })
        .encode()
        .map_err(|error| rpc_error(ConnectError::Codec(error)))?;
        apply_timeout(
            timeout,
            self.send::<op::EnsureContainer>(payload, Some(&target)),
        )
        .await
        .map_err(|error| answered_by(machine_id, error))
    }

    pub async fn change_observed_service(
        &self,
        service: &ployz_core::ServiceObservation,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<u16>,
        approvals: &HashMap<
            ContainerId,
            ployz_core::authority::Signed<ployz_core::authority::RestartApproval>,
        >,
        cancellation: &CancellationToken,
    ) -> PartialResult<ContainerId, ContainerOperationFailure> {
        let mut tasks = JoinSet::new();
        let mut task_targets = HashMap::new();
        let mut queued = HashMap::<MachineId, u32>::new();
        for container in service.containers_for(action) {
            *queued
                .entry(container.as_observation().machine_id)
                .or_default() += 1;
        }
        for container in service.containers_for(action) {
            let observation = container.as_observation();
            let machine_id = observation.machine_id;
            let container_id = observation.container_id;
            let handle = tasks.spawn(change_on_machine(
                self.clone()
                    .with_restart_approval(approvals.get(&container_id).cloned()),
                machine_id,
                container_id,
                action,
                StopOptions {
                    signal: signal.clone(),
                    grace_period_seconds,
                },
                queued.get(&machine_id).copied().unwrap_or(1),
                cancellation.clone(),
            ));
            task_targets.insert(handle.id(), (machine_id, container_id));
        }
        let mut outcomes = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        while let Some(joined) = tasks.join_next_with_id().await {
            match joined {
                Ok((id, Ok(success))) => {
                    task_targets.remove(&id);
                    outcomes.successes.push(success);
                }
                Ok((id, Err(failure))) => {
                    task_targets.remove(&id);
                    outcomes.failures.push(failure);
                }
                Err(error) => {
                    if let Some((machine_id, container_id)) = task_targets.remove(&error.id()) {
                        outcomes.failures.push(MachineFailure {
                            machine_id,
                            error: ContainerOperationFailure {
                                container_id,
                                error: RpcError::unavailable(error.to_string()),
                            },
                        });
                    }
                }
            }
        }
        outcomes
    }

    /// Gather an observer-relative Deploy Snapshot from the given Machines.
    /// Target-specific Container and Docker Volume failures and omissions stay
    /// on the snapshot so planning can derive observer-relative completeness.
    pub(crate) async fn deploy_snapshot(
        &self,
        machines: Vec<MachineObservation>,
    ) -> Result<DeploySnapshot, ConnectError> {
        let (live, volumes, capacity) = tokio::join!(
            self.live_services_from(&machines, EnvironmentValues::Included),
            self.list_volumes(&machines),
            capacity::observe(self, &machines),
        );
        snapshot_from_partial(machines, live?.containers, volumes, capacity)
            .map_err(ConnectError::Remote)
    }
}

/// A call the router or this client's own deadline shows went unanswered names the
/// Machine in the deepest line, which is the one progress shows. Every other error
/// keeps its own words.
pub(crate) fn answered_by(machine_id: &MachineId, error: ConnectError) -> RpcError {
    if !error.target_unanswered() {
        // Only a failure a retry may fix is marked unreached here: a refused key or another
        // lasting failure is what the user acts on, so it keeps its own words. A refused
        // entry comes marked by `rpc_error`, and keeps its words too.
        let (lost, unreached) = (
            error.unreplied(),
            error.unreached() && error.retry() != ployz_core::Retry::Never,
        );
        let error = rpc_error(error);
        return if lost {
            error.reply_lost_from(machine_id)
        } else if unreached {
            error.unreached()
        } else {
            error
        };
    }
    // Named in the details too, as a lost reply is, so a refusal after it can still say
    // which Machine may have done the call.
    let mut silent = unanswered_by(machine_id, rpc_error(error));
    if let serde_json::Value::Object(details) = &mut silent.details {
        details.insert("machine_id".into(), machine_id.to_string().into());
    }
    silent
}

/// `error`, from a call `machine` left unanswered, naming it in the deepest line.
pub(crate) fn unanswered_by(machine: impl std::fmt::Display, error: RpcError) -> RpcError {
    let transport = std::iter::once(error.message)
        .chain(error.cause)
        .collect::<Vec<_>>()
        .join(": ");
    RpcError::unanswered_target(
        error.code,
        format!("Machine {machine} did not answer: {transport}"),
    )
}

fn status_error(status: tonic::Status, path: &str, target: Option<&MachineTarget>) -> ConnectError {
    let error = TransportError::from(status);
    ConnectError::Rpc(match target {
        Some(target) => error.name_refusal(path, target),
        None => error,
    })
}

pub(crate) fn snapshot_from_partial(
    machines: Vec<MachineObservation>,
    containers: PartialResult<Vec<ContainerObservation>, RpcError>,
    volumes: PartialResult<VolumeInventory, RpcError>,
    capacity: BTreeMap<MachineId, BridgeEndpointCapacity>,
) -> Result<DeploySnapshot, RpcError> {
    let PartialResult {
        successes: container_successes,
        failures: container_failures,
        omissions: container_omissions,
    } = containers;
    Ok(DeploySnapshot {
        storage_capacity: Default::default(),
        machines,
        containers: container_successes
            .into_iter()
            .flat_map(|success| success.value)
            .collect(),
        volume_snapshot: VolumeSnapshot::from_partial(volumes)?,
        container_failures,
        container_omissions,
        capacity: Some(capacity),
    })
}

async fn remove_volumes_on(
    client: &Client,
    machines: &[MachineObservation],
    request: RemoveVolumesRequest,
) -> Vec<VolumeRemoval> {
    let RemoveVolumesRequest {
        volumes,
        force,
        teardown,
    } = request;
    let teardown = &teardown;
    join_all(volumes.into_iter().map(|id| async move {
        let outcome = if machines
            .iter()
            .any(|machine| machine.machine.id == id.machine_id && machine.invites_rpc())
        {
            match client
                .call_shared::<op::RemoveVolume>(
                    RemoveVolumeRequest {
                        teardown: teardown.clone(),
                        name: id.name.clone(),
                        force,
                    },
                    Call::once()
                        .to(&MachineTarget::from(&id.machine_id))
                        .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
                )
                .await
                .map_err(crate::connect::rpc_error)
            {
                Ok(_)
                | Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    ..
                }) => VolumeRemovalOutcome::Removed,
                Err(error) => VolumeRemovalOutcome::Failed { error },
            }
        } else {
            VolumeRemovalOutcome::Omitted
        };
        VolumeRemoval { id, outcome }
    }))
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeadConfirmed {
    Yes,
    No,
}

fn stopped_answering(machine: &Machine) -> RpcError {
    RpcError::confirmation_required(format!(
            "Server {} stopped answering after its name was typed. Forgetting it leaves any Volume copies on it behind, so it needs dead typed. No changes made.",
            machine.name
        )).with_details(json!({
            "server": { "id": machine.id, "name": machine.name },
            "answered": false,
        }))
}

#[derive(Debug)]
pub(crate) enum HeldCopies {
    Dead,
    Answered(BTreeSet<DockerVolumeName>),
    /// The entry refused the read because it no longer lists the Server, as when Cloud's
    /// removal finished first. Only the entry that listed it proves it gone; from another
    /// entry, which may not have learned of a fresh join yet, it stays this refusal.
    Gone(RpcError),
}

/// Whether Cloud holds the last Machine. Cloud lets go of it by removing it itself:
/// the reset takes its keys, and Cloud forgets the pairing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloudHold {
    None,
    Last,
}

/// Who removes a Machine. Only Cloud may remove the last Machine it holds: it sees the
/// reset, and only then lets go of the Cluster.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Remover {
    /// Cloud, through its own Management Client.
    Cloud,
    /// Anyone else: the CLI, whatever it connects through.
    Operator,
}

/// Forgetting `machine` leaves its copies of `volumes` behind: only its
/// reset demotes them.
pub(crate) fn copies_left_behind<'a>(
    machine: &MachineName,
    volumes: impl IntoIterator<Item = &'a str>,
) -> RpcError {
    RpcError::conflict(format!(
        "Server {machine} answers and holds copies of Volumes {}; forgetting it leaves them \
             behind. No changes made. Remove it with ployz server rm so its reset demotes them.",
        volumes.into_iter().collect::<Vec<_>>().join(", ")
    ))
}

/// The last Machine is Cloud's: anyone else removing it would leave Cloud paired
/// with a Cluster that no longer exists.
fn cloud_holds_last(machine: MachineId) -> RpcError {
    RpcError::conflict(format!(
        "Machine {machine} is the last Machine and Cloud manages it; only Cloud's removal \
             lets go of it too. No changes made."
    ))
}

/// Refuse removing the last Machine while a Management Client other than Cloud still
/// holds a key: that holder would be left managing a Cluster that no longer exists.
/// Cloud's own hold (`cloud` and the `cli-*` device keys it provisions) goes with the
/// Machine when Cloud removes it, so it only shows in the returned [`CloudHold`].
/// Holders that can't be read refuse too: they may be anyone's.
///
/// # Errors
/// Returns `conflict` naming the other holders, or `unavailable` when the last
/// Machine's holders can't be read.
pub(crate) async fn refuse_last_managed(
    client: &Client,
    machines: &[MachineObservation],
    selected: MachineId,
) -> Result<CloudHold, RpcError> {
    if machines.len() != 1 {
        return Ok(CloudHold::None);
    }
    let holders = client
        .call_shared::<op::Inspect>(
            InspectRequest::default(),
            Call::once()
                .to(&MachineTarget::from(&selected))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map_err(|error| {
            RpcError::unavailable(error_chain::then_sentence(
                &format!(
                    "cannot read who manages Machine {selected}, the last Machine: {}",
                    error_chain::row(&error)
                ),
                "No changes made.",
            ))
        })?
        .management_clients;
    let cloud = holders
        .iter()
        .any(ployz_core::ManagementClientLabel::is_cloud);
    let names: Vec<String> = holders
        .iter()
        .filter(|label| {
            !cloud
                || !matches!(
                    label.kind(),
                    ployz_core::ManagementClientKind::Cloud | ployz_core::ManagementClientKind::Cli
                )
        })
        .map(|label| format!("`{label}`"))
        .collect();
    let who = match names.split_last() {
        None if cloud => return Ok(CloudHold::Last),
        None => return Ok(CloudHold::None),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    };
    Err(RpcError::conflict(format!(
        "this is the last Machine in the Cluster and it is still managed by {who}; \
             removing it would leave {who} managing a Cluster that no longer exists. \
             No changes made. Disconnect {who} from this Machine first."
    )))
}

/// Resolve a Machine Target without hiding Name Ambiguity in the visible observations.
///
/// # Errors
/// Returns NotFound or Ambiguous when the target does not resolve to one Machine.
pub(crate) fn visible_machine<'list>(
    machine: &MachineTarget,
    machines: &'list [MachineObservation],
) -> Result<&'list MachineObservation, RpcError> {
    let selected = match machine.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => {
            return Err(RpcError::not_found(format!(
                "Server {} was not found",
                machine.as_str().escape_debug()
            )));
        }
        matches @ NameMatches::Ambiguous { .. } => {
            return Err(RpcError::ambiguous(format!(
                "Server name {} is ambiguous: {}",
                machine.as_str().escape_debug(),
                matches
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        NameMatches::One(row) => row.id,
    };
    Ok(machines
        .iter()
        .find(|entry| entry.machine.id == selected)
        .expect("resolved Machine came from this list"))
}

/// `machine`'s listed observation, or `None` when it is a Machine ID the list no longer
/// holds: an earlier removal already forgot it. A name that doesn't resolve stays NotFound.
///
/// # Errors
/// Returns NotFound or Ambiguous when the target does not resolve to one Machine.
fn listed_unless_forgotten<'list>(
    machine: &MachineTarget,
    machines: &'list [MachineObservation],
) -> Result<Option<&'list MachineObservation>, RpcError> {
    match visible_machine(machine, machines) {
        Ok(observation) => Ok(Some(observation)),
        Err(error)
            if error.code == RpcErrorCode::NotFound
                && MachineId::parse(machine.as_str()).is_ok() =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A role setting a caller waits for this entry to observe.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RoleSetting {
    ServicesOff,
    IngressOn,
    IngressOff,
}

impl RoleSetting {
    fn role(self) -> &'static str {
        match self {
            Self::ServicesOff => "services",
            Self::IngressOn | Self::IngressOff => "ingress",
        }
    }

    fn holds(self, machine: &Machine) -> bool {
        match self {
            Self::ServicesOff => !machine.accepts_services,
            Self::IngressOn => machine.accepts_ingress,
            Self::IngressOff => !machine.accepts_ingress,
        }
    }
}

/// Why [`wait_for_role`] gave up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoleWaitError {
    #[error("this entry Server has not yet observed the new {} role", .0.role())]
    NotObserved(RoleSetting),
    #[error("this entry Server has not yet listed the change")]
    ChangeNotObserved,
    #[error("cancelled while waiting for the new role")]
    Cancelled,
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

/// Wait until this entry sees Machine `id` with `setting`, so what plans next plans from it.
///
/// # Errors
/// Fails when the entry hasn't observed it within 30 s plus one list call,
/// `cancellation` fires first, or listing Machines fails.
pub(crate) async fn wait_for_role(
    client: &mut Client,
    id: &MachineId,
    setting: RoleSetting,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    wait_until(
        client,
        id,
        |machine| setting.holds(machine),
        RoleWaitError::NotObserved(setting),
        cancellation,
    )
    .await
}

/// Whether this entry lists Machine `id` with `setting` now, in one list call.
///
/// # Errors
/// Fails when listing Machines fails.
pub(crate) async fn lists_role(
    client: &mut Client,
    id: &MachineId,
    setting: RoleSetting,
) -> Result<bool, RoleWaitError> {
    Ok(client
        .machines()
        .await?
        .iter()
        .any(|entry| entry.machine.id == *id && setting.holds(&entry.machine)))
}

/// The fields of a Machine update that a later plan reads: placement Labels and the roles.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlannedChange {
    labels: Vec<MachineLabelKey>,
    accepts_builds: bool,
    accepts_services: bool,
    accepts_ingress: bool,
}

impl PlannedChange {
    pub(crate) fn of(update: &MachineUpdate) -> Self {
        Self {
            labels: update.label_changes.keys().cloned().collect(),
            accepts_builds: update.accepts_builds.is_some(),
            accepts_services: update.accepts_services.is_some(),
            accepts_ingress: update.accepts_ingress.is_some(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.labels.is_empty()
            && !self.accepts_builds
            && !self.accepts_services
            && !self.accepts_ingress
    }

    /// Whether `listed` carries every changed field as the target saved it.
    fn listed(&self, saved: &Machine, listed: &Machine) -> bool {
        self.labels
            .iter()
            .all(|key| saved.labels.get(key) == listed.labels.get(key))
            && (!self.accepts_builds || saved.accepts_builds == listed.accepts_builds)
            && (!self.accepts_services || saved.accepts_services == listed.accepts_services)
            && (!self.accepts_ingress || saved.accepts_ingress == listed.accepts_ingress)
    }
}

/// Wait until this entry lists `saved` with every field in `change`, in one poll.
///
/// # Errors
/// As [`wait_for_role`].
pub(crate) async fn wait_for_change(
    client: &mut Client,
    saved: &Machine,
    change: &PlannedChange,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    wait_until(
        client,
        &saved.id,
        |listed| change.listed(saved, listed),
        RoleWaitError::ChangeNotObserved,
        cancellation,
    )
    .await
}

async fn wait_until(
    client: &mut Client,
    id: &MachineId,
    holds: impl Fn(&Machine) -> bool,
    not_observed: RoleWaitError,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), RoleWaitError> {
    // ponytail: 30 s deadline plus one ListMachines call, capped at 10 s.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let machines = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RoleWaitError::Cancelled),
            machines = client.machines() => machines?,
        };
        if machines
            .iter()
            .any(|entry| entry.machine.id == *id && holds(&entry.machine))
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(not_observed);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(RoleWaitError::Cancelled),
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

pub(crate) async fn evict_machine(
    client: &mut Client,
    observation: &MachineObservation,
    confirm_data_loss: &DataLossConfirmation,
    current: MachineId,
) -> Result<LocalMachineRemoved, RpcError> {
    let selected = observation.machine.id;
    data_loss_on_machine(client, observation)
        .await?
        .require(confirm_data_loss)
        .map_err(UnconfirmedDataLoss::into_rpc_error)?;
    depart(client, selected, current).await
}

/// A streaming RPC of the catalog, as [`Client::open_stream`] opens it.
pub(crate) trait StreamRpc {
    const PATH: &'static str;
    /// Whether the caller feeds many requests, not one.
    const CLIENT_STREAM: bool;
    /// Whether the stream is submitted through the entry even where a Full connection to
    /// its target is held: a Build's submission is one-shot.
    const ENTRY_ONLY: bool = false;
    /// Whether the frames come gzip-compressed.
    const GZIP: bool = false;
}

impl StreamRpc for op::Build {
    const PATH: &'static str = op::Build::PATH;
    const CLIENT_STREAM: bool = true;
    const ENTRY_ONLY: bool = true;
}

impl StreamRpc for op::Exec {
    const PATH: &'static str = op::Exec::PATH;
    const CLIENT_STREAM: bool = true;
}

impl StreamRpc for op::TailLogs {
    const PATH: &'static str = op::TailLogs::PATH;
    const CLIENT_STREAM: bool = false;
}

impl StreamRpc for op::LogHistory {
    const PATH: &'static str = op::LogHistory::PATH;
    const CLIENT_STREAM: bool = false;
}

impl StreamRpc for op::RuntimeWatch {
    const PATH: &'static str = op::RuntimeWatch::PATH;
    const CLIENT_STREAM: bool = false;
    const GZIP: bool = true;
}

/// Whether a call on `path` to a named target goes to that target itself when a Full
/// connection reaches it: a Full route, or one an enforcing Machine refuses every Operator,
/// such as Leave. A call forwarded over the mesh arrives as an Operator's.
fn dials_direct(path: &str) -> bool {
    ployz_core::scope_of(path) == Some(ployz_core::ManagementScope::Full)
        || ployz_core::capability_of(path)
            .is_some_and(ployz_core::authority::coverage::needs_full_caller)
}

/// Leave on `machine`, then Forget its row through the entry unless it is the entry. Both
/// are idempotent. A Leave the entry can't route finds no row: Forget already ran, and
/// whether the reset did is unknown, so it departed [`Departed::AlreadyForgotten`].
///
/// A Forget that fails after the target answered Left carries that reply as
/// [`LeftNotForgotten`]: a retry can no longer reach the target, and the reply is the only
/// proof that it reset.
async fn depart(
    client: &mut Client,
    machine: MachineId,
    current: MachineId,
) -> Result<LocalMachineRemoved, RpcError> {
    let departed = match client
        .call::<op::Leave>(
            LeaveRequest {
                teardown: client.deploy.teardown(),
            },
            Call::keyed().to(&MachineTarget::from(&machine)),
        )
        .await
    {
        Ok(Left::Left { reset_warning, .. }) => Departed::Reset {
            row_warning: reset_warning,
        },
        Ok(Left::Refused { error, .. }) => return Err(error),
        Err(error) if error.is_unreachable() => Departed::Unreachable {
            cause: error_chain::row(&error),
        },
        Err(error) => match RpcError::from(error) {
            gone if gone.code == RpcErrorCode::NotFound => Departed::AlreadyForgotten,
            error => return Err(error),
        },
    };
    if machine != current
        && let Err(error) = forget(client, machine).await
    {
        return Err(match departed {
            Departed::Reset { row_warning } => LeftNotForgotten::carried_by(error, row_warning),
            Departed::Unreachable { .. } | Departed::AlreadyForgotten => error,
        });
    }
    Ok(departed.into())
}

/// A Forget that failed after its target answered Left. The failed Forget's error carries
/// this in `details`, so a caller that retries with Forget alone keeps the reset's warning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LeftNotForgotten {
    left: bool,
    pub(crate) reset_warning: Option<String>,
}

impl LeftNotForgotten {
    fn carried_by(error: RpcError, reset_warning: Option<String>) -> RpcError {
        error.with_details(
            serde_json::to_value(Self {
                left: true,
                reset_warning,
            })
            .expect("LeftNotForgotten is JSON"),
        )
    }

    /// The Leave reply a failed removal carries, when it carries one.
    pub(crate) fn from_rpc_error(error: &RpcError) -> Option<Self> {
        Self::deserialize(&error.details)
            .ok()
            .filter(|carried| carried.left)
    }
}

/// Delete `machine_id`'s shared row through the entry Machine, under the teardown the
/// client carries.
pub(crate) async fn forget(client: &mut Client, machine_id: MachineId) -> Result<(), RpcError> {
    match client
        .call::<op::Forget>(
            ForgetRequest {
                teardown: client.deploy.teardown(),
                machine_id,
            },
            Call::keyed(),
        )
        .await
        .map_err(RpcError::from)?
    {
        Forgotten::Forgotten => Ok(()),
        Forgotten::Refused { error, .. } => Err(error),
    }
}

async fn data_loss_on_machine(
    client: &Client,
    observation: &MachineObservation,
) -> Result<ObservedDataLoss, RpcError> {
    let selected = observation.machine.id;
    let name = &observation.machine.name;
    if !observation.invites_rpc() {
        return Err(server_did_not_respond(name));
    }
    let volumes = list_volumes_on_machine(client.clone(), selected)
        .await
        .map_err(|error| {
            let message = format!("Server {name}: {}", error_chain::row(&error));
            error.with_message(message)
        })?;
    Ok(ObservedDataLoss {
        data_loss: volumes_lost(volumes),
    })
}

/// The Docker Volumes a reset loses: every one listed or failing to list, but the
/// builder's cache, which is no one's data.
fn volumes_lost(inventory: VolumeInventory) -> Vec<DataLoss> {
    inventory
        .volumes
        .into_iter()
        .map(|volume| volume.id)
        .chain(inventory.failures.into_iter().map(|failure| failure.id))
        .filter(|id| !ployz_build::is_builder_cache(id.name.as_str()))
        .map(|id| DataLoss::DockerVolume { id })
        .collect()
}

fn server_did_not_respond(name: &MachineName) -> RpcError {
    RpcError::unavailable(format!("Server {name} did not respond"))
}

/// Machines whose storage observation failed (`failures`) or did not answer in
/// time (`omissions`).
#[derive(Debug, Default)]
pub(crate) struct StorageGaps {
    pub failures: Vec<MachineFailure<RpcError>>,
    pub omissions: Vec<MachineId>,
}

/// `Ok(None)` when the Machine did not answer within the storage budget, and `Some(None)`
/// when it answered that it cannot report storage.
async fn observe_machine_storage(
    client: Client,
    machine_id: MachineId,
) -> Result<Option<Option<MachineStorageObservation>>, RpcError> {
    let target = MachineTarget::from(&machine_id);
    let observation = async {
        let description = client
            .call_shared::<op::DescribeContract>(
                DescribeContractRequest {},
                Call::once().to(&target),
            )
            .await
            .map_err(crate::connect::rpc_error)?;
        if !description.supports(MACHINE_STORAGE_OBSERVATION_CAPABILITY) {
            return Ok(None);
        }
        Ok(client
            .call_shared::<op::Inspect>(
                InspectRequest {
                    include_storage: true,
                    ..Default::default()
                },
                Call::once().to(&target),
            )
            .await
            .map_err(crate::connect::rpc_error)?
            .storage)
    };
    match tokio::time::timeout(STORAGE_OBSERVATION_TIMEOUT, observation).await {
        Ok(observed) => observed.map(Some),
        Err(_) => Ok(None),
    }
}

async fn list_volumes_on_machine(
    mut client: Client,
    machine_id: MachineId,
) -> Result<VolumeInventory, RpcError> {
    client
        .call::<op::ListVolumes>(
            ListVolumesRequest {},
            Call::read()
                .to(&MachineTarget::from(&machine_id))
                .budget(crate::connect::target_read_timeout()),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .and_then(|inventory| validate_volume_inventory(machine_id, inventory))
}

fn validate_volume_inventory(
    machine_id: MachineId,
    inventory: VolumeInventory,
) -> Result<VolumeInventory, RpcError> {
    let mut names = BTreeSet::new();
    for id in inventory
        .volumes
        .iter()
        .map(|volume| &volume.id)
        .chain(inventory.failures.iter().map(|failure| &failure.id))
    {
        if id.machine_id != machine_id {
            return Err(RpcError::internal(format!(
                "targeted ListVolumes response from Machine {machine_id} contained Docker Volume '{}' for Machine {}",
                id.name, id.machine_id
            )));
        }
        if !names.insert(&id.name) {
            return Err(RpcError::internal(format!(
                "targeted ListVolumes response from Machine {machine_id} repeated Docker Volume '{}'",
                id.name
            )));
        }
    }
    Ok(inventory)
}

async fn list_on_machine(
    mut client: Client,
    machine_id: MachineId,
    environment: EnvironmentValues,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    client
        .call::<op::ListContainers>(
            ListContainersRequest { environment },
            Call::read()
                .to(&MachineTarget::from(&machine_id))
                .budget(crate::connect::target_read_timeout()),
        )
        .await
        .map(|list| MachineSuccess {
            machine_id,
            value: list.containers,
        })
        .map_err(|error| MachineFailure {
            machine_id,
            error: unanswered_kept(error),
        })
}

/// [`rpc_error`] in its own words, marked unanswered when the router or this client's
/// deadline shows the target never answered, and undialed as well when the router
/// couldn't dial it.
fn unanswered_kept(error: ConnectError) -> RpcError {
    if !error.target_unanswered() {
        return rpc_error(error);
    }
    let unreached = error.target_unreached();
    let error = rpc_error(error);
    let kept = RpcError::unanswered_target(error.code, error.message).with_causes(error.cause);
    match unreached {
        true => kept.undialed_target(),
        false => kept,
    }
}

async fn change_on_machine(
    client: Client,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    stop: StopOptions,
    queued: u32,
    cancellation: CancellationToken,
) -> Result<MachineSuccess<ContainerId>, MachineFailure<ContainerOperationFailure>> {
    match change_container_rpc(
        &client,
        &machine_id,
        &container_id,
        action,
        stop,
        queued,
        &cancellation,
    )
    .await
    {
        Ok(()) => Ok(MachineSuccess {
            machine_id,
            value: container_id,
        }),
        Err(error) => Err(MachineFailure {
            machine_id,
            error: ContainerOperationFailure {
                container_id,
                error,
            },
        }),
    }
}

async fn change_container_rpc(
    client: &Client,
    machine_id: &MachineId,
    container_id: &ContainerId,
    action: ContainerAction,
    stop: StopOptions,
    queued: u32,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    match action {
        ContainerAction::Start => cleanup::user_start(client, machine_id, container_id)
            .await
            .and_then(admitted),
        ContainerAction::Restart => {
            cleanup::user_restart(client, machine_id, container_id, stop, queued, cancellation)
                .await
                .and_then(admitted)
        }
        ContainerAction::Stop => {
            cleanup::user_stop(client, machine_id, container_id, stop, queued, cancellation).await
        }
        ContainerAction::Remove => {
            cleanup::user_remove(client, machine_id, container_id, cancellation).await
        }
    }
}

fn admitted(started: UserStarted) -> Result<(), RpcError> {
    match started {
        UserStarted::In | UserStarted::Running => Ok(()),
        UserStarted::NotIn(health) => Err(RpcError::unavailable("The Container started but did not pass its healthcheck, so proxies do not route to it.").with_details(health
                .and_then(ployz_core::HealthVerdict::into_failure)
                .map_or(Value::Null, |failure| json!(failure)))),
    }
}

#[cfg(test)]
#[path = "cluster_tests.rs"]
mod tests;
