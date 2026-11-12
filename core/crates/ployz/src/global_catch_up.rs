//! Global catch-up: place observed eligible Globals onto this Machine only.
//!
//! It plans from a complete Live Observation, re-observing a partial one a
//! few times within the run before failing, reads fresh storage evidence,
//! reports unknown eligibility or incomplete placement per slot, and fails
//! once at the end; nothing retries it in the background. Deleted slots,
//! later eligibility changes and transient failures wait for an explicit
//! redeploy.
//!
//! A new copy carries its source's deploy label and gen, starts Pending and
//! is brought In through the owner, which refuses a copy older than an In
//! one. The Ingress Proxy is never copied: the joiner's own daemon builds its
//! holder and Caddy through SetIngressDoors, once, for the observed proxy id. The
//! Private Access router is never copied either: the joiner's own daemon builds it
//! through SetPrivateAccess from the sealed auth key an observed router carries.

use crate::cluster::Call;
use ployz_core::error_chain;
use ployz_core::{
    AnonymousVolumes, BridgeEndpointCapacity, Ciphertext, ContainerKind, ContainerNow,
    ContainerObservation, ContainerRuntimeObservation, ContainerWant, CopyRotation, CreationKey,
    EnvironmentValues, HealthFailure, HealthVerdict, Hint, IngressDoors, IngressDoorsSet,
    ListContainersRequest, LiveServices, Machine, MachineId, MachineTarget, NewContainer,
    PartialResult, PrivateAccessSet, QualifiedService, ResolvedServiceSpec, RpcError, RunRotation,
    RunTarget, ServiceId, ServiceMode, ServiceObservation, SetIngressDoorsRequest,
    SetPrivateAccessRequest, SlotKey, op,
};
use tokio_util::sync::CancellationToken;

use crate::deploy::cleanup::{
    self, BroughtIn, Copy, EmptyPolicy, NotIn, Removal, Rotated, RotationFailure,
};
use crate::global_slot::{
    Eligibility, Live, Sentence, ServerNames, participating, retire_if_ineligible,
    retire_ingress_proxy,
};
use crate::ingress::{ClusterDoors, DoorMachine};
use crate::private_access::AccessMachine;
use crate::{
    connect::{Client, unreadable_answer},
    deploy::endpoint_capacity_error,
    failure::Failure,
};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    cause: Failure,
    unresolved: Vec<QualifiedService>,
}

impl CatchUpError {
    /// Record the failure and Globals whose eligibility or running slot is unresolved.
    pub(crate) fn new(cause: Failure, unresolved: Vec<QualifiedService>) -> Self {
        Self { cause, unresolved }
    }
}

impl std::fmt::Display for CatchUpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}

/// What every Server runs, with each Server's name.
pub(crate) struct Observed {
    live: Live,
    /// Omitted Servers that are Unarrived Machines, which hold no copy to wait on.
    unarrived: ServerNames,
    /// The Doors the Cluster declares, as the entry Machine's replica holds them.
    declared: Option<IngressDoors>,
}

/// What the policy of one Global slot needs from the joiner: its own listing, its own
/// record, and the copies it holds.
pub(crate) trait SlotIo {
    /// `machine_id`'s Containers, as that Machine lists them.
    async fn list_target(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, RpcError>;
    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure>;
    /// Retire the slot's copies on `machine_id` when fresh evidence says it is ineligible
    /// there, answering whether it was.
    async fn retire_if_ineligible(
        &mut self,
        machine_id: &MachineId,
        new: &NewContainer,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError>;
    async fn retire_copy(
        &mut self,
        copy: &Copy,
        cancel: &CancellationToken,
    ) -> Result<Rotated, RpcError>;
    /// Ask `container` run Pending on `target`, resending until it answers.
    async fn ensure_pending(
        &mut self,
        target: &MachineTarget,
        container: RunTarget,
    ) -> Result<ContainerNow, RpcError>;
}

pub(crate) trait CatchUpClient: SlotIo {
    async fn live_services(&mut self) -> Result<Observed, Failure>;
    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
        cancel: &CancellationToken,
    ) -> Result<Option<Copy>, RpcError>;
    /// Set `machine_id`'s Doors to the Cluster's for the Ingress Proxy `proxy_service`,
    /// or remove its Ingress Proxy when it doesn't hold the ingress role, answering `None`.
    async fn run_doors(
        &mut self,
        machine_id: &MachineId,
        proxy_service: ServiceId,
        doors: ClusterDoors,
        cancel: &CancellationToken,
    ) -> Result<Option<IngressDoorsSet>, RpcError>;
    /// Place `machine_id`'s Private Access router with the auth key `sealed`.
    async fn run_private_access(
        &mut self,
        machine_id: &MachineId,
        sealed: Ciphertext,
    ) -> Result<PrivateAccessSet, RpcError>;
    async fn bring_in(
        &mut self,
        copy: &Copy,
        spec: &ResolvedServiceSpec,
    ) -> Result<BroughtIn, RpcError>;
    async fn remove_copy(&mut self, copy: &Copy) -> Result<Removal, RpcError>;
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl SlotIo for Client {
    async fn list_target(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        self.call::<op::ListContainers>(
            ListContainersRequest {
                environment: EnvironmentValues::Included,
            },
            Call::read()
                .to(&MachineTarget::from(machine_id))
                .budget(crate::connect::target_read_timeout()),
        )
        .await
        .map(|list| list.containers)
        .map_err(crate::connect::rpc_error)
    }

    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        let details = self
            .call::<op::Inspect>(
                ployz_core::InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                Call::read()
                    .to(&MachineTarget::from(machine_id))
                    .budget(crate::connect::target_read_timeout()),
            )
            .await
            .map_err(crate::connect::rpc_error)
            .map_err(Failure::from)?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }

    async fn retire_if_ineligible(
        &mut self,
        machine_id: &MachineId,
        new: &NewContainer,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError> {
        let retired = retire_if_ineligible(
            self,
            machine_id,
            &new.slot.namespace,
            &new.resolved_spec,
            cancel,
            &Sentence::CatchUp,
        )
        .await?;
        Ok(retired == Eligibility::Retired)
    }

    async fn retire_copy(
        &mut self,
        copy: &Copy,
        cancel: &CancellationToken,
    ) -> Result<Rotated, RpcError> {
        cleanup::retire(
            &*self,
            copy,
            EmptyPolicy::RequireOtherIn,
            AnonymousVolumes::Remove,
            cancel,
        )
        .await
    }

    async fn ensure_pending(
        &mut self,
        target: &MachineTarget,
        container: RunTarget,
    ) -> Result<ContainerNow, RpcError> {
        let want = ContainerWant::Running {
            container,
            rotation: RunRotation::Pending,
        };
        self.ensure_retried(target, want).await?.into_result()
    }
}

/// Place `new`, the copy of one observed Global, on `machine_id`: nothing when the Machine
/// rules the slot out, the copy already there when one of the same shape runs, and
/// otherwise a keyed create.
async fn place_slot<C: SlotIo>(
    client: &mut C,
    machine_id: &MachineId,
    new: NewContainer,
    cancel: &CancellationToken,
) -> Result<Option<Copy>, RpcError> {
    let target = MachineTarget::from(machine_id);
    if client
        .retire_if_ineligible(machine_id, &new, cancel)
        .await?
    {
        return Ok(None);
    }
    let existing = client
        .list_target(machine_id)
        .await?
        .into_iter()
        .find(|observed| {
            observed.machine_id == *machine_id
                && observed.kind == ContainerKind::ServiceContainer
                && observed.namespace == new.slot.namespace
                && observed.resolved_spec == new.resolved_spec
        });
    // The keyed create below answers with a Container that already holds the
    // creation key's name, so an Out copy must go before a new one is made.
    let existing = match existing {
        Some(existing) if matches!(existing.rotation(), CopyRotation::Held(_)) => {
            return Err(RpcError::conflict(format!(
                "copy {} is stopped on this Machine by a user; start or remove it there first",
                existing.container_id.as_str()
            )));
        }
        Some(existing) if *existing.rotation() == CopyRotation::Out => {
            step_error(
                client
                    .retire_copy(&Copy::observed(&existing), cancel)
                    .await?,
            )?;
            None
        }
        existing => existing,
    };
    if let Some(existing) = existing {
        let now = client
            .ensure_pending(
                &target,
                RunTarget::Existing(ployz_core::ContainerLocator::Id(existing.container_id)),
            )
            .await?;
        return Ok(Some(
            now.present()
                .map_or_else(|| Copy::observed(&existing), Copy::observed),
        ));
    }
    let capacity = client
        .bridge_capacity(machine_id)
        .await
        .map_err(|error| error_chain::rpc_error(ployz_core::RpcErrorCode::Unavailable, &error))?;
    if let Some(error) = endpoint_capacity_error(1, capacity.as_ref()) {
        return Err(RpcError::conflict(error.to_string()));
    }
    let now = client.ensure_pending(&target, RunTarget::New(new)).await?;
    Ok(now.present().map(Copy::observed))
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<Observed, Failure> {
        let list = self.machine_list().await.map_err(Failure::from)?;
        let declared = list.enrollment.and_then(|snapshot| snapshot.doors);
        let machines = list.machines;
        let services = self
            .live_services_from(&machines, EnvironmentValues::Included)
            .await
            .map_err(Failure::from)?;
        Ok(Observed {
            live: Live {
                services,
                names: ServerNames::of(&machines),
            },
            unarrived: ServerNames::unarrived(&machines),
            declared,
        })
    }

    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
        cancel: &CancellationToken,
    ) -> Result<Option<Copy>, RpcError> {
        place_slot(self, machine_id, new, cancel).await
    }

    async fn run_doors(
        &mut self,
        machine_id: &MachineId,
        proxy_service: ServiceId,
        doors: ClusterDoors,
        cancel: &CancellationToken,
    ) -> Result<Option<IngressDoorsSet>, RpcError> {
        if !participating(self, machine_id, false)
            .await?
            .machine
            .accepts_ingress
        {
            retire_ingress_proxy(&*self, machine_id, cancel).await?;
            return Ok(None);
        }
        self.set_ingress_doors(
            machine_id,
            SetIngressDoorsRequest {
                doors: doors.doors,
                proxy_service,
                sealed_token: doors.sealed_token,
            },
        )
        .await
        .map(Some)
    }

    async fn run_private_access(
        &mut self,
        machine_id: &MachineId,
        sealed: Ciphertext,
    ) -> Result<PrivateAccessSet, RpcError> {
        self.set_private_access(
            machine_id,
            SetPrivateAccessRequest {
                enabled: true,
                sealed_auth_key: Some(sealed),
                approval: None,
            },
        )
        .await
    }

    async fn bring_in(
        &mut self,
        copy: &Copy,
        spec: &ResolvedServiceSpec,
    ) -> Result<BroughtIn, RpcError> {
        cleanup::bring_in(&*self, copy, spec, None).await
    }

    async fn remove_copy(&mut self, copy: &Copy) -> Result<Removal, RpcError> {
        cleanup::remove(&*self, copy, AnonymousVolumes::Remove).await
    }

    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        self.list_target(machine_id).await.map_err(Failure::from)
    }
}

fn step_error(rotated: Rotated) -> Result<(), RpcError> {
    rotated
        .failure()
        .map_err(|failure| Sentence::CatchUp.error(failure))
}

pub(crate) fn joined_catch_up_error(
    error: CatchUpError,
    server: &Machine,
    command: impl Fn(&[&str]) -> String,
) -> Failure {
    let mut message = if error.cause.is_interrupted() {
        "Server joined; Global catch-up was interrupted. It remains a Cluster member."
    } else {
        "Server joined, but Global catch-up is incomplete; it remains a Cluster member."
    }
    .to_owned();
    for identity in &error.unresolved {
        if *identity != QualifiedService::system_ingress() && !identity.is_private_access_router() {
            message.push_str(&format!(
                " To finish, redeploy Namespace Service `{identity}`."
            ));
        }
    }
    let mut failure = error.cause.context(message);
    for identity in error.unresolved {
        let hint = if identity == QualifiedService::system_ingress() {
            Hint::Retry(command(&[
                "server",
                "set",
                &server.id.to_string(),
                "--accepts-ingress=true",
            ]))
        } else if identity.is_private_access_router() {
            Hint::Retry(command(&["private-access", "on", "--auth-key-stdin"]))
        } else {
            Hint::Inspect(command(&[
                "logs",
                &identity.to_string(),
                "--machine",
                &server.id.to_string(),
            ]))
        };
        failure = failure.hint(hint);
    }
    failure
}

/// A factual stage of target-only catch-up, adapted by the owning command.
pub(crate) enum CatchUpFact {
    Identified(Vec<QualifiedService>),
    Creating(QualifiedService),
    Excluded(QualifiedService),
    Running(QualifiedService),
    Failed(QualifiedService),
    /// Omitted Unarrived Machines the plan did not wait on.
    NotArrived(ServerNames),
}

/// Follow target-only catch-up with the shared progress block. Returns the Unarrived
/// Servers it reported, whether or not catch-up then succeeded.
pub(crate) async fn follow_globals(
    client: &mut Client,
    assigned: &Machine,
    command: impl Fn(&[&str]) -> String,
) -> (Vec<serde_json::Value>, Result<(), CatchUpError>) {
    use crate::ui::progress::{Disposition, Frame, Progress, Row, Run, State, Subject, Timing};
    let signal = match crate::cancellation::interrupted() {
        Ok(signal) => signal,
        Err(error) => return (Vec::new(), Err(CatchUpError::new(error.into(), Vec::new()))),
    };
    let mut frame = Frame {
        run: Run::CatchUp(assigned.id),
        title: format!("Starting Globals on {}", assigned.name),
        rows: Vec::new(),
        notices: Vec::new(),
    };
    let mut progress = Progress::start(frame.clone());
    let mut not_arrived = ServerNames::default();
    let result = catch_up_globals(client, assigned, &signal, |fact| {
        let (identity, state) = match fact {
            CatchUpFact::NotArrived(names) => {
                not_arrived = names;
                return;
            }
            CatchUpFact::Identified(services) => {
                frame.rows = services
                    .into_iter()
                    .map(|service| Row {
                        subject: Subject::ServiceOnServer {
                            service,
                            machine: assigned.id,
                            server: assigned.name.to_string(),
                        },
                        state: State::Pending,
                        detail: None,
                        timing: Timing::Unavailable,
                    })
                    .collect();
                progress.update(frame.clone());
                return;
            }
            CatchUpFact::Creating(service) => (
                service,
                State::Running(ployz_store::RowPhase::CreatingContainer),
            ),
            CatchUpFact::Excluded(service) => (service, State::Excluded),
            CatchUpFact::Running(service) => (service, State::ObservedRunning),
            CatchUpFact::Failed(service) => (service, State::Failed),
        };
        if let Some(row) = frame.rows.iter_mut().find(|row| {
            matches!(&row.subject, Subject::ServiceOnServer { service, .. } if service == &identity)
        }) {
            if matches!(row.timing, Timing::Unavailable) {
                row.timing = Timing::Started(std::time::SystemTime::now());
            }
            if !matches!(state, State::Running(_) | State::Pending)
                && let Timing::Started(at) = row.timing
            {
                row.timing = Timing::Finished(at.elapsed().unwrap_or_default());
            }
            row.state = state;
            row.detail = None;
        }
        progress.update(frame.clone());
    })
    .await;
    if result.is_err() {
        for row in &mut frame.rows {
            if matches!(row.state, State::Pending | State::Running(_)) {
                row.state = State::NotAttempted;
            }
        }
    }
    progress.finish(
        frame,
        if signal.is_cancelled() {
            Disposition::LocalInterrupted
        } else {
            Disposition::Settled
        },
    );
    let mut unarrived = Vec::new();
    // A Server enrolled moments ago looks the same as one that never will join,
    // so forgetting it is offered only with its condition.
    let list = command(&["server", "ls"]);
    for (id, name) in not_arrived {
        let next = command(&["server", "forget", id.as_str(), "--confirm", "dead"]);
        crate::ui::warn(format!(
            "Server {name} may still be joining, so Global catch-up did not wait on it; if `{}` \
             keeps showing it down with no version, remove it with `{}`.",
            crate::ui::pasteable(&list),
            crate::ui::pasteable(&next)
        ));
        unarrived.push(serde_json::json!({ "id": id, "name": name, "next": next }));
    }
    if !unarrived.is_empty() {
        crate::ui::hint(&Hint::Inspect(list));
    }
    (unarrived, result)
}

/// Add the Unarrived Servers catch-up skipped to a join result, when there are any.
pub(crate) fn with_unarrived(
    mut result: serde_json::Value,
    unarrived: &[serde_json::Value],
) -> serde_json::Value {
    if !unarrived.is_empty()
        && let Some(fields) = result.as_object_mut()
    {
        fields.insert("unarrived".into(), unarrived.into());
    }
    result
}

const OBSERVATION_ATTEMPTS: u32 = 3;
const OBSERVATION_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

const OUTDATED: &str = "a newer deploy replaced it while catching up; redeploy";

struct Source {
    identity: QualifiedService,
    work: CatchUpWork,
}

/// What catch-up places for one observed Global.
enum CatchUpWork {
    /// A copy of the newest In or Pending copy; none when no copy is either.
    Service(Option<Box<ContainerObservation>>),
    /// The holder, Caddy and Tunnel Connector, which the joiner's daemon builds for this
    /// Ingress Proxy Service with the Cluster's Doors. No door identity is ever copied.
    Ingress(ServiceId, ClusterDoors),
    /// The Private Access router, which the joiner's daemon builds from this sealed auth
    /// key. The router is never copied.
    PrivateAccess(Ciphertext),
}

struct Started {
    identity: QualifiedService,
    copy: Copy,
    spec: ResolvedServiceSpec,
}

fn is_running(observed: &ContainerObservation) -> bool {
    matches!(
        observed.runtime,
        ContainerRuntimeObservation::Running { .. }
    )
}

/// How long catch-up waits for the copies it started to run before it calls one not
/// running.
const STARTING_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Whether `target`, this Machine's Containers, shows `started` running, and a
/// running copy of its Service in its serving shape.
fn runs_here(target: &[ContainerObservation], started: &Started, machine: &MachineId) -> bool {
    let runs = target
        .iter()
        .any(|observed| observed.container_id == started.copy.container_id && is_running(observed));
    let shape_runs = target.iter().any(|observed| {
        observed.identity() == started.identity
            && observed.machine_id == *machine
            && observed.resolved_spec.serving_shape() == started.spec.serving_shape()
            && is_running(observed)
    });
    runs && shape_runs
}

/// Whether `started` may yet run: its Container is still there and isn't on its way
/// out. An exited one may, under Docker's restart policy.
fn may_still_run(target: &[ContainerObservation], started: &Started) -> bool {
    target.iter().any(|observed| {
        observed.container_id == started.copy.container_id
            && !matches!(
                observed.runtime,
                ContainerRuntimeObservation::Removing | ContainerRuntimeObservation::Dead
            )
    })
}

/// One source per observed Global, the Ingress Proxy's door identities as one and the
/// Private Access router as its sealed auth key. A joiner with the ingress role gets the
/// Ingress Proxy even when none runs anywhere.
fn sources(
    services: &[ServiceObservation],
    takes_ingress: bool,
    declared: Option<IngressDoors>,
) -> Vec<Source> {
    let mut sources: Vec<Source> = services
        .iter()
        .filter(|service| {
            !service.identity.is_ingress_door() && !service.identity.is_private_access_router()
        })
        .filter_map(|service| {
            let copies = || service.containers.iter().map(|c| c.as_observation());
            let newest = service.newest_container()?.as_observation();
            if newest.resolved_spec.mode != ServiceMode::Global {
                return None;
            }
            let copy = copies()
                .filter(|observed| {
                    matches!(
                        observed.rotation(),
                        CopyRotation::In | CopyRotation::Pending
                    )
                })
                .max_by_key(|observed| (observed.copy_gen(), observed.recency_key()));
            Some(Source {
                identity: service.identity.clone(),
                work: CatchUpWork::Service(copy.cloned().map(Box::new)),
            })
        })
        .collect();
    let doors_run = services
        .iter()
        .any(|service| service.identity.is_ingress_door() && !service.containers.is_empty());
    if doors_run || takes_ingress {
        sources.push(Source {
            identity: QualifiedService::system_ingress(),
            work: CatchUpWork::Ingress(
                crate::ingress::proxy_service(services),
                crate::ingress::cluster_doors(declared, services),
            ),
        });
    }
    if let Some(sealed) = crate::private_access::cluster_sealed_key(services) {
        sources.push(Source {
            identity: QualifiedService::system_tailscale(),
            work: CatchUpWork::PrivateAccess(sealed),
        });
    }
    sources
}

fn new_copy(source: &ContainerObservation) -> NewContainer {
    NewContainer {
        slot: SlotKey {
            namespace: source.namespace.clone(),
            service: source.resolved_spec.name.clone(),
            kind: ContainerKind::ServiceContainer,
            creation_key: CreationKey::try_from(crate::cluster::global_creation_key(
                &source.resolved_spec,
            ))
            .expect("a Global creation key stays within 512 bytes"),
        },
        resolved_spec: source.resolved_spec.clone(),
        r#gen: source.copy_gen(),
        deployment_id: source.deploy_label(),
        registry_auth: None,
        image_prepared: false,
        answer: ployz_core::AnswerWhen::Started,
    }
}

/// A copy that did not come In is removed; `message` is why, and what the removal did
/// follows it when it did not finish.
async fn remove_started<C: CatchUpClient>(client: &mut C, copy: &Copy, message: String) -> String {
    match client.remove_copy(copy).await {
        Ok(Removal::Removed) => message,
        Ok(removal) => format!("{message}; the started copy stays: {removal:?}"),
        Err(error) => format!("{message}; {}", error_chain::row(&error)),
    }
}

fn not_in_message(health: Option<HealthVerdict>) -> String {
    health
        .and_then(HealthVerdict::into_failure)
        .unwrap_or(HealthFailure::TimedOut {
            deadline_millis: None,
            last_check: None,
        })
        .to_string()
}

/// What fixes a partial listing. An answer this ployz can't read comes from a newer
/// release, so connectivity is named only for the Servers that didn't answer.
fn partial_next_step<T>(
    result: &PartialResult<T, RpcError>,
    server: impl Fn(&MachineId) -> String,
) -> String {
    let (unreadable, unanswered): (Vec<_>, Vec<_>) = result
        .failures
        .iter()
        .partition(|failure| unreadable_answer(&failure.error));
    let reconnect = "restore peer connectivity and redeploy";
    if unreadable.is_empty() {
        return reconnect.to_owned();
    }
    let names: Vec<_> = unreadable
        .iter()
        .map(|failure| server(&failure.machine_id))
        .collect();
    let update = format!(
        "this ployz can't read {}'s answer, so update ployz on this host",
        names.join(", ")
    );
    if unanswered.is_empty() && result.omissions.is_empty() {
        format!("{update} and redeploy")
    } else {
        format!("{update}; {reconnect}")
    }
}

async fn observe_complete<C: CatchUpClient>(
    client: &mut C,
    this_machine: &MachineId,
    cancel: &CancellationToken,
    observe: &mut impl FnMut(CatchUpFact),
) -> Result<(LiveServices<RpcError>, Option<IngressDoors>), CatchUpError> {
    let cancelled = || CatchUpError::new(Failure::cancelled(), Vec::new());
    for attempt in 1..=OBSERVATION_ATTEMPTS {
        let Observed {
            mut live,
            mut unarrived,
            declared,
        } = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            live = client.live_services() => {
                live.map_err(|error| CatchUpError::new(error, Vec::new()))?
            }
        };
        // The joiner's own row may still be its Register row; it is arriving now.
        unarrived.without(this_machine);
        live.services
            .containers
            .omissions
            .retain(|id| !unarrived.contains(id));
        let Some(details) = live.partial() else {
            observe(CatchUpFact::NotArrived(unarrived));
            return Ok((live.services, declared));
        };
        if attempt == OBSERVATION_ATTEMPTS {
            observe(CatchUpFact::NotArrived(unarrived));
            let next = partial_next_step(&live.services.containers, |id| live.names.name(id));
            return Err(CatchUpError::new(
                Failure::unavailable(format!(
                    "Global catch-up cannot plan from partial Service observations: {details}; \
                     {next}"
                )),
                Vec::new(),
            ));
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            () = tokio::time::sleep(OBSERVATION_RETRY) => {}
        }
    }
    unreachable!("the last attempt returns")
}

/// What one slot came to.
enum SlotOutcome {
    Resolved,
    /// This Machine takes no part in the slot.
    Excluded,
    Failed(SlotFailure),
}

/// Why a slot is unresolved: each cause, as the sentence a person reads.
struct SlotFailure {
    chains: Vec<String>,
}

impl SlotOutcome {
    fn failed(chain: String) -> Self {
        Self::Failed(SlotFailure {
            chains: vec![chain],
        })
    }
}

/// Every slot's outcome, folded into the facts catch-up reports and what is left to say
/// when it ends.
struct Tally<F> {
    observe: F,
    unresolved: Vec<QualifiedService>,
    failures: Vec<(QualifiedService, String)>,
}

impl<F: FnMut(CatchUpFact)> Tally<F> {
    fn new(identities: Vec<QualifiedService>, mut observe: F) -> Self {
        observe(CatchUpFact::Identified(identities.clone()));
        Self {
            observe,
            unresolved: identities,
            failures: Vec::new(),
        }
    }

    fn creating(&mut self, identity: &QualifiedService) {
        (self.observe)(CatchUpFact::Creating(identity.clone()));
    }

    fn record(&mut self, identity: QualifiedService, outcome: SlotOutcome) {
        let fact = match outcome {
            SlotOutcome::Resolved => {
                self.unresolved.retain(|service| service != &identity);
                CatchUpFact::Running(identity)
            }
            SlotOutcome::Excluded => {
                self.unresolved.retain(|service| service != &identity);
                CatchUpFact::Excluded(identity)
            }
            SlotOutcome::Failed(failure) => {
                for chain in failure.chains {
                    self.failures.push((identity.clone(), chain));
                }
                CatchUpFact::Failed(identity)
            }
        };
        (self.observe)(fact);
    }

    fn error(&self, cause: Failure) -> CatchUpError {
        CatchUpError::new(cause, self.unresolved.clone())
    }

    fn finish(self, cancel: &CancellationToken) -> Result<(), CatchUpError> {
        if cancel.is_cancelled() {
            return Err(CatchUpError::new(Failure::cancelled(), self.unresolved));
        }
        if self.unresolved.is_empty() {
            return Ok(());
        }
        let details = self
            .failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::unavailable("eligible Globals are not running after catch-up")
        } else {
            Failure::unavailable(format!("Global catch-up incomplete: {details}"))
        };
        Err(CatchUpError::new(cause, self.unresolved))
    }
}

async fn place_private_access<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    sealed: Ciphertext,
) -> SlotOutcome {
    let answer = match AccessMachine::of(this_machine).refusal() {
        Some(refusal) => Err(refusal),
        None => client.run_private_access(&this_machine.id, sealed).await,
    };
    let failure = match answer {
        Ok(set) => crate::private_access::failure_of(&set),
        Err(error) => Some(error_chain::row(&error)),
    };
    failure.map_or(SlotOutcome::Resolved, SlotOutcome::failed)
}

async fn place_ingress<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    proxy_service: ServiceId,
    doors: ClusterDoors,
    cancel: &CancellationToken,
) -> SlotOutcome {
    let refusal = this_machine
        .accepts_ingress
        .then(|| DoorMachine::of(this_machine).refusal(&doors.doors))
        .flatten();
    let answer = match refusal {
        Some(refusal) => Err(refusal),
        None => {
            client
                .run_doors(&this_machine.id, proxy_service, doors, cancel)
                .await
        }
    };
    let failure = match answer {
        Ok(None) => return SlotOutcome::Excluded,
        // The daemon answers once both run, so nothing is left to wait on.
        Ok(Some(set)) => crate::ingress::failure_of(&set).map(|failure| failure.cause().to_owned()),
        Err(error) => Some(error_chain::row(&error)),
    };
    failure.map_or(SlotOutcome::Resolved, SlotOutcome::failed)
}

/// Create `origin`'s copy on this Machine and bring it In, so it is `Started`; the slot's
/// outcome otherwise.
async fn start_copy<C: CatchUpClient>(
    client: &mut C,
    machine_id: &MachineId,
    identity: QualifiedService,
    origin: &ContainerObservation,
    cancel: &CancellationToken,
) -> Result<Started, SlotOutcome> {
    let spec = origin.resolved_spec.clone();
    let copy = match client.run_slot(machine_id, new_copy(origin), cancel).await {
        Ok(Some(copy)) => copy,
        Ok(None) => return Err(SlotOutcome::Excluded),
        Err(error) => return Err(SlotOutcome::failed(error_chain::row(&error))),
    };
    if copy.rotation != CopyRotation::In {
        let message = match client.bring_in(&copy, &spec).await.map(BroughtIn::not_in) {
            Ok(Ok(())) => None,
            Ok(Err(NotIn::Health(health))) => Some(not_in_message(health)),
            Ok(Err(NotIn::OlderThanIn(_))) => {
                Some(remove_started(client, &copy, OUTDATED.to_owned()).await)
            }
            Ok(Err(NotIn::Refused(refusal))) => {
                Some(remove_started(client, &copy, refusal.message).await)
            }
            Err(error) => Some(error_chain::row(&error)),
        };
        if let Some(message) = message {
            return Err(SlotOutcome::failed(message));
        }
    }
    Ok(Started {
        identity,
        copy,
        spec,
    })
}

/// What `target`, this Machine's Containers after the wait, makes of `started`: running,
/// and with its superseded copies retired.
async fn settle<C: CatchUpClient>(
    client: &mut C,
    machine_id: &MachineId,
    started: &Started,
    target: &[ContainerObservation],
    cancel: &CancellationToken,
) -> SlotOutcome {
    let Started { identity, copy, .. } = started;
    if !runs_here(target, started, machine_id) {
        let seen = target
            .iter()
            .find(|observed| observed.container_id == copy.container_id)
            .map_or_else(
                || "gone".to_owned(),
                |observed| observed.runtime.to_string(),
            );
        return SlotOutcome::failed(format!(
            "copy {} is not running on this Machine after catch-up; it is {seen}",
            copy.container_id.as_str()
        ));
    }
    let superseded = target.iter().filter(|observed| {
        observed.machine_id == *machine_id
            && observed.kind == ContainerKind::ServiceContainer
            && observed.identity() == *identity
            && observed.container_id != copy.container_id
    });
    let mut chains = Vec::new();
    for old in superseded {
        if cancel.is_cancelled() {
            break;
        }
        let left = match client
            .retire_copy(&Copy::observed(old), cancel)
            .await
            .map(Rotated::failure)
        {
            Ok(Ok(())) => None,
            Ok(Err(RotationFailure::Cancelled)) => break,
            Ok(Err(RotationFailure::Refused(refusal))) => Some(refusal.message),
            Ok(Err(RotationFailure::NotConfirmed(machines))) => Some(format!(
                "{} did not confirm it left the routes",
                machines
                    .machines
                    .iter()
                    .map(MachineId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(error) => Some(error_chain::row(&error)),
        };
        if let Some(message) = left {
            chains.push(format!(
                "superseded copy {} stays: {message}",
                old.container_id.as_str()
            ));
        }
    }
    if chains.is_empty() {
        SlotOutcome::Resolved
    } else {
        SlotOutcome::Failed(SlotFailure { chains })
    }
}

pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    cancel: &CancellationToken,
    mut observe: impl FnMut(CatchUpFact),
) -> Result<(), CatchUpError> {
    let (live, declared) = observe_complete(client, &this_machine.id, cancel, &mut observe).await?;
    let services = live.services();
    let sources = sources(&services, this_machine.accepts_ingress, declared);
    let mut tally = Tally::new(
        sources
            .iter()
            .map(|source| source.identity.clone())
            .collect(),
        observe,
    );
    let mut started: Vec<Started> = Vec::new();
    for source in sources {
        if cancel.is_cancelled() {
            break;
        }
        let identity = source.identity;
        let outcome = match source.work {
            CatchUpWork::PrivateAccess(sealed) => {
                tally.creating(&identity);
                place_private_access(client, this_machine, sealed).await
            }
            CatchUpWork::Ingress(proxy_service, doors) => {
                tally.creating(&identity);
                place_ingress(client, this_machine, proxy_service, doors, cancel).await
            }
            CatchUpWork::Service(None) => {
                SlotOutcome::failed(format!("no copy of it is In or Pending; {OUTDATED}"))
            }
            CatchUpWork::Service(Some(origin)) => {
                tally.creating(&identity);
                match start_copy(client, &this_machine.id, identity.clone(), &origin, cancel).await
                {
                    Ok(copy) => {
                        started.push(copy);
                        continue;
                    }
                    Err(outcome) => outcome,
                }
            }
        };
        tally.record(identity, outcome);
    }
    // A copy that started may have stopped again until Docker's restart policy starts it:
    // give each a moment to run before calling it not running.
    let deadline = tokio::time::Instant::now() + STARTING_WAIT;
    let target = loop {
        let target = client
            .target_containers(&this_machine.id)
            .await
            .map_err(|error| {
                tally.error(if cancel.is_cancelled() {
                    error.interrupted()
                } else {
                    error
                })
            })?;
        let starting = started.iter().any(|copy| {
            !runs_here(&target, copy, &this_machine.id) && may_still_run(&target, copy)
        });
        if !starting || tokio::time::Instant::now() >= deadline || cancel.is_cancelled() {
            break target;
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            () = tokio::time::sleep(OBSERVATION_RETRY) => {}
        }
    };
    for started in started {
        let outcome = settle(client, &this_machine.id, &started, &target, cancel).await;
        tally.record(started.identity, outcome);
    }
    tally.finish(cancel)
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
