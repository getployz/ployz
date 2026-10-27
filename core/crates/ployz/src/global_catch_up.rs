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
//! one. System ingress copies stay Pending for life and skip rotation.

use ployz_core::{
    AnonymousVolumes, BridgeEndpointCapacity, ContainerKind, ContainerObservation,
    ContainerRefusalReason, ContainerRuntimeObservation, ContainerWant, CopyRotation, CreationKey,
    EnvironmentValues, HealthFailure, HealthVerdict, InspectRequest, ListContainersRequest,
    LiveServices, Machine, MachineId, MachineTarget, NewContainer, PartialResult, QualifiedService,
    ResolvedServiceSpec, RpcError, RunRotation, RunTarget, ServiceMode, ServiceObservation,
    ServicePlacementEligibility, SlotKey, op,
};
use tokio_util::sync::CancellationToken;

use crate::deploy::cleanup::{self, BroughtIn, Copy, Step};
use crate::drain::retirement::{Names, server_name};
use crate::failure::partial_failure_details;
use crate::global_slot::{slot_eligibility, unknown_eligibility};
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
    live: LiveServices<RpcError>,
    names: Names,
    /// Omitted Servers that are Unarrived Machines, which hold no copy to wait on.
    unarrived: Names,
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<Observed, Failure>;
    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure>;
    async fn retire_if_ineligible(
        &mut self,
        machine_id: &MachineId,
        new: &NewContainer,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError>;
    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
        cancel: &CancellationToken,
    ) -> Result<Option<Copy>, RpcError>;
    async fn bring_in(
        &mut self,
        copy: &Copy,
        spec: &ResolvedServiceSpec,
    ) -> Result<BroughtIn, RpcError>;
    async fn retire_copy(
        &mut self,
        copy: &Copy,
        cancel: &CancellationToken,
    ) -> Result<Step, RpcError>;
    async fn remove_copy(&mut self, copy: &Copy) -> Result<Step, RpcError>;
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<Observed, Failure> {
        let machines = self.machines().await.map_err(Failure::from)?;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Included)
            .await
            .map_err(Failure::from)?;
        let named = |machine: &ployz_core::MachineObservation| {
            (machine.machine.id, machine.machine.name.clone())
        };
        Ok(Observed {
            live,
            names: machines.iter().map(named).collect(),
            unarrived: machines
                .iter()
                .filter(|machine| machine.unarrived())
                .map(named)
                .collect(),
        })
    }

    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        let details = self
            .read::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                &MachineTarget::from(machine_id),
            )
            .await
            .map_err(Failure::from)?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }

    async fn retire_if_ineligible(
        &mut self,
        machine_id: &MachineId,
        new: &NewContainer,
        cancel: &CancellationToken,
    ) -> Result<bool, RpcError> {
        match slot_eligibility(self, machine_id, &new.slot.namespace, &new.resolved_spec).await? {
            ServicePlacementEligibility::Eligible => Ok(false),
            ServicePlacementEligibility::Ineligible(_) => {
                let service = QualifiedService::new(
                    new.slot.namespace.clone(),
                    new.resolved_spec.name.clone(),
                );
                let listed = self
                    .read::<op::ListContainers>(
                        ListContainersRequest {
                            environment: EnvironmentValues::Included,
                        },
                        &MachineTarget::from(machine_id),
                    )
                    .await?
                    .containers;
                let own = listed.iter().filter(|observed| {
                    observed.machine_id == *machine_id
                        && observed.kind == ContainerKind::ServiceContainer
                        && observed.identity() == service
                });
                for observed in own {
                    let copy = Copy::observed(observed);
                    let step = if is_proxy(&service) {
                        cleanup::remove(&*self, &copy, AnonymousVolumes::Remove).await?
                    } else {
                        cleanup::retire(&*self, &copy, false, AnonymousVolumes::Remove, cancel)
                            .await?
                    };
                    step_error(step)?;
                }
                Ok(true)
            }
            ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        }
    }

    async fn run_slot(
        &mut self,
        machine_id: &MachineId,
        new: NewContainer,
        cancel: &CancellationToken,
    ) -> Result<Option<Copy>, RpcError> {
        let target = MachineTarget::from(machine_id);
        if self.retire_if_ineligible(machine_id, &new, cancel).await? {
            return Ok(None);
        }
        let listed = self
            .read::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Included,
                },
                &target,
            )
            .await?;
        let existing = listed.containers.into_iter().find(|observed| {
            observed.machine_id == *machine_id
                && observed.kind == ContainerKind::ServiceContainer
                && observed.namespace == new.slot.namespace
                && observed.resolved_spec == new.resolved_spec
        });
        // The keyed create below answers with a Container that already holds the
        // creation key's name, so an Out copy must go before a new one is made.
        let existing = match existing {
            Some(existing) if matches!(existing.rotation(), CopyRotation::Held(_)) => {
                return Err(RpcError {
                    code: ployz_core::RpcErrorCode::Conflict,
                    message: format!(
                        "copy {} is stopped on this Machine by a user; start or remove it there first",
                        existing.container_id.as_str()
                    ),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                });
            }
            Some(existing) if *existing.rotation() == CopyRotation::Out => {
                step_error(self.retire_copy(&Copy::observed(&existing), cancel).await?)?;
                None
            }
            existing => existing,
        };
        if let Some(existing) = existing {
            let now = ensure_retried(
                self,
                &target,
                RunTarget::Existing(ployz_core::ContainerLocator::Id(existing.container_id)),
            )
            .await?;
            return Ok(Some(
                now.present()
                    .map_or_else(|| Copy::observed(&existing), Copy::observed),
            ));
        }
        let capacity = self
            .bridge_capacity(machine_id)
            .await
            .map_err(|error| crate::ui::rpc_error(ployz_core::RpcErrorCode::Unavailable, &error))?;
        if let Some(error) = endpoint_capacity_error(1, capacity.as_ref()) {
            return Err(RpcError {
                code: ployz_core::RpcErrorCode::Conflict,
                message: error.to_string(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            });
        }
        let now = ensure_retried(self, &target, RunTarget::New(new)).await?;
        Ok(now.present().map(Copy::observed))
    }

    async fn bring_in(
        &mut self,
        copy: &Copy,
        spec: &ResolvedServiceSpec,
    ) -> Result<BroughtIn, RpcError> {
        cleanup::bring_in(&*self, copy, spec, None).await
    }

    async fn retire_copy(
        &mut self,
        copy: &Copy,
        cancel: &CancellationToken,
    ) -> Result<Step, RpcError> {
        cleanup::retire(&*self, copy, false, AnonymousVolumes::Remove, cancel).await
    }

    async fn remove_copy(&mut self, copy: &Copy) -> Result<Step, RpcError> {
        cleanup::remove(&*self, copy, AnonymousVolumes::Remove).await
    }

    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        self.read::<op::ListContainers>(
            ListContainersRequest {
                environment: EnvironmentValues::Included,
            },
            &MachineTarget::from(machine_id),
        )
        .await
        .map(|list| list.containers)
        .map_err(Failure::from)
    }
}

fn step_error(step: Step) -> Result<(), RpcError> {
    match step {
        Step::Done => Ok(()),
        Step::Refused(refusal) => Err(refusal.into_error()),
        Step::NotConfirmed(machines) => Err(RpcError {
            code: ployz_core::RpcErrorCode::Unavailable,
            message: format!(
                "{} did not confirm the copy left its routes",
                machines
                    .machines
                    .iter()
                    .map(MachineId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        Step::Cancelled => Err(crate::ui::rpc_error(
            ployz_core::RpcErrorCode::Internal,
            &Failure::cancelled(),
        )),
    }
}

async fn ensure_retried(
    client: &mut Client,
    target: &MachineTarget,
    container: RunTarget,
) -> Result<ployz_core::ContainerNow, RpcError> {
    let want = ContainerWant::Running {
        container,
        rotation: RunRotation::Pending,
    };
    crate::sealed_compat::admit(client, target, &want).await?;
    client
        .call::<op::EnsureContainer>(ployz_core::EnsureContainerRequest { want }, Some(target))
        .await
        .map_err(RpcError::from)?
        .into_result()
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
        if *identity != QualifiedService::system_ingress() {
            message.push_str(&format!(
                " To finish, redeploy Namespace Service `{identity}`."
            ));
        }
    }
    let mut failure = error.cause.context(message);
    for identity in error.unresolved {
        let hint = if identity == QualifiedService::system_ingress() {
            crate::ui::Hint::Retry(command(&[
                "server",
                "set",
                &server.id.to_string(),
                "--accepts-ingress=true",
            ]))
        } else {
            crate::ui::Hint::Inspect(command(&[
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
    NotArrived(Names),
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
    let mut not_arrived = Names::new();
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
    for (id, name) in not_arrived {
        crate::ui::warn(format!(
            "Server {name} shows no sign it ever joined, so Global catch-up did not wait on it."
        ));
        let next = command(&["server", "forget", id.as_str(), "--confirm", "dead"]);
        crate::ui::hint(&crate::ui::Hint::Next(next.clone()));
        unarrived.push(serde_json::json!({ "id": id, "name": name, "next": next }));
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
    copy: Option<ContainerObservation>,
}

struct Started {
    identity: QualifiedService,
    copy: Copy,
    spec: ResolvedServiceSpec,
}

fn is_proxy(service: &QualifiedService) -> bool {
    *service == QualifiedService::system_ingress()
}

fn is_running(observed: &ContainerObservation) -> bool {
    matches!(
        observed.runtime,
        ContainerRuntimeObservation::Running { .. }
    )
}

/// How long, in [`OBSERVATION_RETRY`]s, catch-up waits for the copies it started to
/// run before it calls one not running.
const STARTING_ATTEMPTS: u32 = 30;

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

fn sources(services: &[ServiceObservation]) -> Vec<Source> {
    services
        .iter()
        .filter_map(|service| {
            let copies = || service.containers.iter().map(|c| c.as_observation());
            let newest = copies().max_by(|a, b| {
                (a.created_at_unix_nanos, a.container_id.as_str())
                    .cmp(&(b.created_at_unix_nanos, b.container_id.as_str()))
            })?;
            if newest.resolved_spec.mode != ServiceMode::Global {
                return None;
            }
            let identity = service.identity.clone();
            let copy = if is_proxy(&identity) {
                Some(newest)
            } else {
                copies()
                    .filter(|observed| {
                        matches!(
                            observed.rotation(),
                            CopyRotation::In | CopyRotation::Pending
                        )
                    })
                    .max_by(|a, b| {
                        (
                            a.copy_gen(),
                            a.created_at_unix_nanos,
                            a.container_id.as_str(),
                        )
                            .cmp(&(
                                b.copy_gen(),
                                b.created_at_unix_nanos,
                                b.container_id.as_str(),
                            ))
                    })
            };
            Some(Source {
                identity,
                copy: copy.cloned(),
            })
        })
        .collect()
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
) -> Result<LiveServices<RpcError>, CatchUpError> {
    let cancelled = || CatchUpError::new(Failure::cancelled(), Vec::new());
    for attempt in 1..=OBSERVATION_ATTEMPTS {
        let Observed {
            mut live,
            names,
            mut unarrived,
        } = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            live = client.live_services() => {
                live.map_err(|error| CatchUpError::new(error, Vec::new()))?
            }
        };
        // The joiner's own row may still be its Register row; it is arriving now.
        unarrived.retain(|(id, _)| id != this_machine);
        live.containers
            .omissions
            .retain(|id| !unarrived.iter().any(|(unarrived, _)| unarrived == id));
        if live.containers.all_targets_succeeded() {
            observe(CatchUpFact::NotArrived(unarrived));
            return Ok(live);
        }
        if attempt == OBSERVATION_ATTEMPTS {
            observe(CatchUpFact::NotArrived(unarrived));
            let details = partial_failure_details(&live.containers, |id| server_name(&names, id));
            let next = partial_next_step(&live.containers, |id| server_name(&names, id));
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

pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    cancel: &CancellationToken,
    mut observe: impl FnMut(CatchUpFact),
) -> Result<(), CatchUpError> {
    let live = observe_complete(client, &this_machine.id, cancel, &mut observe).await?;
    let services = live.services();
    let sources = sources(&services);
    let mut unresolved: Vec<QualifiedService> = sources
        .iter()
        .map(|source| source.identity.clone())
        .collect();
    observe(CatchUpFact::Identified(unresolved.clone()));
    let mut failures: Vec<(QualifiedService, String)> = Vec::new();
    let mut started: Vec<Started> = Vec::new();
    for source in sources {
        if cancel.is_cancelled() {
            break;
        }
        let identity = source.identity;
        let Some(origin) = source.copy else {
            failures.push((
                identity.clone(),
                format!("no copy of it is In or Pending; {OUTDATED}"),
            ));
            observe(CatchUpFact::Failed(identity));
            continue;
        };
        let spec = origin.resolved_spec.clone();
        observe(CatchUpFact::Creating(identity.clone()));
        let copy = match client
            .run_slot(&this_machine.id, new_copy(&origin), cancel)
            .await
        {
            Ok(Some(copy)) => copy,
            Ok(None) => {
                unresolved.retain(|service| service != &identity);
                observe(CatchUpFact::Excluded(identity));
                continue;
            }
            Err(error) => {
                failures.push((identity.clone(), crate::ui::row(&error)));
                observe(CatchUpFact::Failed(identity));
                continue;
            }
        };
        if !is_proxy(&identity) && copy.rotation != CopyRotation::In {
            let outcome = match client.bring_in(&copy, &spec).await {
                Ok(BroughtIn::In) => None,
                Ok(BroughtIn::NotIn { health }) => Some(not_in_message(health)),
                Ok(BroughtIn::Refused(refusal)) => {
                    let message = if refusal.reason == ContainerRefusalReason::OlderThanIn {
                        OUTDATED.to_owned()
                    } else {
                        refusal.message
                    };
                    Some(match client.remove_copy(&copy).await {
                        Ok(Step::Done) => message,
                        Ok(step) => format!("{message}; the started copy stays: {step:?}"),
                        Err(error) => format!("{message}; {}", crate::ui::row(&error)),
                    })
                }
                Err(error) => Some(crate::ui::row(&error)),
            };
            if let Some(message) = outcome {
                failures.push((identity.clone(), message));
                observe(CatchUpFact::Failed(identity));
                continue;
            }
        }
        started.push(Started {
            identity,
            copy,
            spec,
        });
    }
    // A copy the Machine created may still be starting: give each a moment to run
    // before calling it not running.
    let mut waited = 0;
    let target = loop {
        let target = client
            .target_containers(&this_machine.id)
            .await
            .map_err(|error| {
                let error = if cancel.is_cancelled() {
                    error.interrupted()
                } else {
                    error
                };
                CatchUpError::new(error, unresolved.clone())
            })?;
        let starting = started
            .iter()
            .any(|copy| !runs_here(&target, copy, &this_machine.id));
        if !starting || waited == STARTING_ATTEMPTS || cancel.is_cancelled() {
            break target;
        }
        waited += 1;
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            () = tokio::time::sleep(OBSERVATION_RETRY) => {}
        }
    };
    for started in started {
        let runs = runs_here(&target, &started, &this_machine.id);
        let Started { identity, copy, .. } = started;
        if !runs {
            failures.push((
                identity.clone(),
                format!(
                    "copy {} is not running on this Machine after catch-up",
                    copy.container_id.as_str()
                ),
            ));
            observe(CatchUpFact::Failed(identity));
            continue;
        }
        if !is_proxy(&identity) {
            let superseded = target.iter().filter(|observed| {
                observed.machine_id == this_machine.id
                    && observed.kind == ContainerKind::ServiceContainer
                    && observed.identity() == identity
                    && observed.container_id != copy.container_id
            });
            for old in superseded {
                if cancel.is_cancelled() {
                    break;
                }
                let left = match client.retire_copy(&Copy::observed(old), cancel).await {
                    Ok(Step::Done) => None,
                    Ok(Step::Cancelled) => break,
                    Ok(Step::Refused(refusal)) => Some(refusal.message),
                    Ok(Step::NotConfirmed(machines)) => Some(format!(
                        "{} did not confirm it left the routes",
                        machines
                            .machines
                            .iter()
                            .map(MachineId::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    Err(error) => Some(crate::ui::row(&error)),
                };
                if let Some(message) = left {
                    failures.push((
                        identity.clone(),
                        format!(
                            "superseded copy {} stays: {message}",
                            old.container_id.as_str()
                        ),
                    ));
                }
            }
        }
        if failures.iter().any(|(failed, _)| failed == &identity) {
            observe(CatchUpFact::Failed(identity));
            continue;
        }
        unresolved.retain(|service| service != &identity);
        observe(CatchUpFact::Running(identity));
    }
    if cancel.is_cancelled() {
        return Err(CatchUpError::new(Failure::cancelled(), unresolved));
    }
    if !unresolved.is_empty() {
        let details = failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::unavailable("eligible Globals are not running after catch-up")
        } else {
            Failure::unavailable(format!("Global catch-up incomplete: {details}"))
        };
        return Err(CatchUpError::new(cause, unresolved));
    }
    Ok(())
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
