//! The Ingress Proxy: each Server with the ingress role asks its own daemon to bring the
//! holder and the Caddy in its network to the wiring that daemon pins, through
//! SetIngressDoors. Nothing here builds a spec or names an image.

use futures_util::{StreamExt, stream};
use ployz_core::{
    ContainerId, ContainerObservation, ContainerRuntimeObservation, DoorChange, DoorOutcome,
    EnvironmentValues, IngressDoors, IngressDoorsFailure, IngressDoorsSet, IngressStage, MachineId,
    MachineName, MachineObservation, QualifiedService, RowStanding, RpcError, RpcErrorCode,
    SET_INGRESS_DOORS_CAPABILITY, ServiceId, ServiceObservation, SetIngressDoorsRequest,
};
use ployz_store::{IngressAction, IngressFailure, IngressRepair};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::{
    connect::{Client, ConnectError},
    deploy::{MachineOperations, RestartTolerant, SlotAction},
    failure::Failure,
    ui::progress::{Detail, Diagnostic, StartFailure},
};

/// How many Servers one command sets the Ingress Proxy on at once.
const DOORS_AT_ONCE: usize = 8;

/// A Server whose Ingress Proxy a command sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorMachine {
    pub(crate) id: MachineId,
    pub(crate) name: MachineName,
    /// Whether its daemon may serve SetIngressDoors. One that advertises other
    /// capabilities but not this one keeps the Ingress Proxy it runs, and is named as
    /// needing a newer Ployz.
    pub(crate) supported: bool,
}

/// The Servers that should run the Ingress Proxy: reachable, holding the ingress role, and
/// not leaving.
pub(crate) fn door_machines(observed: &[MachineObservation]) -> Vec<DoorMachine> {
    observed
        .iter()
        .filter(|machine| {
            machine.invites_rpc()
                && machine.machine.accepts_ingress
                && match machine.machine.standing() {
                    RowStanding::Registered | RowStanding::Versioned => true,
                    RowStanding::Leaving { .. } => false,
                }
        })
        .map(|machine| {
            let capabilities = &machine.machine.runtime.capabilities;
            DoorMachine {
                id: machine.machine.id,
                name: machine.machine.name.clone(),
                // An empty list is a row that predates it, which claims nothing, so the
                // daemon is asked and answers for itself.
                supported: capabilities.is_empty()
                    || capabilities.contains(SET_INGRESS_DOORS_CAPABILITY),
            }
        })
        .collect()
}

/// The Ingress Proxy Service every Server's doors derive from: the observed one, its
/// newest copy's when copies disagree, else a fresh id. A command calls this once, so
/// every Server it sets derives its holder from one id.
pub(crate) fn proxy_service(services: &[ServiceObservation]) -> ServiceId {
    services
        .iter()
        .filter(|service| service.identity == QualifiedService::system_ingress())
        .filter_map(|service| {
            let newest = service
                .containers
                .iter()
                .map(|copy| {
                    let copy = copy.as_observation();
                    (copy.created_at_unix_nanos, copy.container_id)
                })
                .max()?;
            Some((newest, service.service_id))
        })
        .max()
        .map_or_else(ServiceId::random, |(_, service_id)| service_id)
}

/// What one Server's daemon answered, or why it wasn't asked to the end.
#[derive(Debug)]
enum Answer {
    Set(Box<IngressDoorsSet>),
    Refused(RpcError),
    Cancelled,
}

/// The Servers' Ingress Proxies observed in full, with every Server that should run one.
struct Doors {
    machines: Vec<DoorMachine>,
    proxy: ServiceId,
}

async fn observe_doors(client: &mut Client) -> Result<Doors, Failure> {
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    let services = live.services();
    let observed = services
        .iter()
        .any(|service| service.identity == QualifiedService::system_ingress());
    // A Server that failed its listing may run the Ingress Proxy under an id a fresh one
    // would replace. One that isn't reachable at all is past asking either way.
    if !observed && let Some(failure) = live.containers.failures.first() {
        return Err(Failure::unavailable(format!(
            "Server {} didn't list its containers, so which Ingress Proxy the Cluster runs is \
             unknown: {}",
            failure.machine_id,
            crate::ui::row(&failure.error)
        )));
    }
    Ok(Doors {
        machines: door_machines(&machines),
        proxy: proxy_service(&services),
    })
}

/// Ask each of `machines` to set its Public Door for `proxy`, a few at a time, in order.
/// A call whose reply was lost, or that met a restarting daemon, is sent again, since
/// the daemon converges on a resend.
async fn set_doors<C: MachineOperations>(
    client: &C,
    machines: &[DoorMachine],
    proxy: ServiceId,
    cancel: &CancellationToken,
) -> Vec<Answer> {
    let client = &RestartTolerant::new(client, cancel);
    // Each future owns its Server, since one borrowing from the slice leaves the
    // caller's future Send for one lifetime only, and a spawned Deploy runner needs all.
    stream::iter(machines.to_vec())
        .map(|machine| set_door(client, machine, proxy, cancel))
        .buffered(DOORS_AT_ONCE)
        .collect()
        .await
}

async fn set_door<C: MachineOperations>(
    client: &C,
    machine: DoorMachine,
    proxy: ServiceId,
    cancel: &CancellationToken,
) -> Answer {
    if !machine.supported {
        return Answer::Refused(unsupported(&machine.name));
    }
    let request = SetIngressDoorsRequest {
        doors: IngressDoors::public(),
        proxy_service: proxy,
    };
    tokio::select! {
        answer = client.set_ingress_doors(&machine.id, request) => match answer {
            Ok(set) => Answer::Set(Box::new(set)),
            Err(error) => Answer::Refused(error),
        },
        () = cancel.cancelled() => Answer::Cancelled,
    }
}

/// The refusal for a Server whose daemon predates SetIngressDoors.
fn unsupported(server: &MachineName) -> RpcError {
    let details = serde_json::json!({
        "target": server.to_string(),
        "capability": SET_INGRESS_DOORS_CAPABILITY,
    });
    let refusal = ployz_core::CapabilityRefusal::new(&RpcErrorCode::Unsupported, details.clone())
        .expect("the details name a target and a capability");
    RpcError {
        code: RpcErrorCode::Unsupported,
        message: refusal.to_string(),
        details,
        cause: Vec::new(),
    }
}

/// One Server's Ingress Proxy after [`follow_roles`] set it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Followed {
    pub(crate) machine: MachineId,
    pub(crate) server: MachineName,
    #[serde(flatten)]
    pub(crate) set: IngressDoorsSet,
}

/// Why [`follow_roles`] failed, with every Server whose Ingress Proxy isn't running after.
#[derive(Debug)]
pub(crate) struct NotFollowed {
    pub(crate) failure: Failure,
    /// Empty when observation failed, so no Server's outcome is known.
    failed: Vec<MachineId>,
}

impl NotFollowed {
    /// Whether every Server that failed is one other than `machine`, as when a resumed
    /// founding finds an Ingress Proxy down on a Server that joined since.
    pub(crate) fn only_elsewhere(&self, machine: &MachineId) -> bool {
        !self.failed.is_empty() && !self.failed.contains(machine)
    }
}

impl From<Failure> for NotFollowed {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            failed: Vec::new(),
        }
    }
}

impl From<NotFollowed> for Failure {
    fn from(not_followed: NotFollowed) -> Self {
        not_followed.failure
    }
}

/// Set the Ingress Proxy on every Server that holds the ingress role. Turning the role off
/// removes nothing here.
///
/// # Errors
///
/// Fails when observation fails, or when any Server's Ingress Proxy isn't running after,
/// naming each such Server. The first one's failure is the diagnostic's.
pub(crate) async fn follow_roles(client: &mut Client) -> Result<Vec<Followed>, NotFollowed> {
    let doors = observe_doors(client).await?;
    let answers = set_doors(
        &*client,
        &doors.machines,
        doors.proxy,
        &CancellationToken::new(),
    )
    .await;
    followed(doors.machines, answers)
}

/// [`follow_roles`]'s result from each Server's answer, in the same order.
fn followed(
    machines: Vec<DoorMachine>,
    answers: Vec<Answer>,
) -> Result<Vec<Followed>, NotFollowed> {
    let mut followed = Vec::new();
    let mut failed = Vec::new();
    for (machine, answer) in machines.into_iter().zip(answers) {
        match answer {
            Answer::Set(set) => match failure_of(&set) {
                None => followed.push(Followed {
                    machine: machine.id,
                    server: machine.name,
                    set: *set,
                }),
                Some(failure) => failed.push(Unfollowed::of_set(machine, &set, failure)),
            },
            Answer::Refused(error) => failed.push(Unfollowed {
                said: crate::ui::row(&error),
                cause: crate::ui::row(&error),
                machine,
                refused: None,
                start: None,
            }),
            Answer::Cancelled => unreachable!("following the roles is never cancelled"),
        }
    }
    let Some(first) = failed.first() else {
        return Ok(followed);
    };
    let lines = failed
        .iter()
        .map(|failed| format!("{}: {}", failed.machine.name, failed.said))
        .collect::<Vec<_>>();
    let diagnostic = Diagnostic {
        failed_machine: Some(first.machine.id),
        start_failure: first.start,
        failures: failed
            .iter()
            .map(|failed| Detail {
                message: format!(
                    "The Ingress Proxy on {} isn't running.",
                    failed.machine.name
                ),
                causes: vec![failed.cause.clone()],
            })
            .collect(),
        ..Diagnostic::default()
    };
    let failure = match &first.refused {
        // Docker's words stay a `cause:` line under ours, as a headline that quotes this
        // failure would swallow them otherwise.
        Some(error) => Failure::caused(RpcErrorCode::Unavailable, lines.join("; "), error.clone()),
        None => Failure::coded(RpcErrorCode::Unavailable, lines.join("; ")),
    };
    Err(NotFollowed {
        failure: failure.with_diagnostic(diagnostic),
        failed: failed.iter().map(|failed| failed.machine.id).collect(),
    })
}

/// One Server whose Ingress Proxy isn't running after [`follow_roles`].
struct Unfollowed {
    machine: DoorMachine,
    /// What failed, in our words.
    said: String,
    /// Why, on one line.
    cause: String,
    /// What Docker answered the stage that failed.
    refused: Option<RpcError>,
    start: Option<StartFailure>,
}

impl Unfollowed {
    fn of_set(machine: DoorMachine, set: &IngressDoorsSet, failure: IngressFailure) -> Self {
        let cause = failure.cause().to_owned();
        let Some(stage) = &set.failure else {
            // The daemon set both, and one isn't running, so the start is what failed.
            return Self {
                machine,
                said: cause.clone(),
                cause,
                refused: None,
                start: Some(StartFailure::Unhealthy),
            };
        };
        let component = match stage.stage {
            IngressStage::Holder => "holder",
            IngressStage::Proxy => "Caddy",
            IngressStage::Unrecognized => "Container",
        };
        Self {
            machine,
            said: format!("Docker refused its Ingress Proxy {component}"),
            cause,
            refused: Some(stage.error.clone()),
            start: match failure {
                IngressFailure::PortTaken { .. } => Some(StartFailure::PortTaken),
                // Any other refusal, such as a bad mount, is no reason to take the role
                // back: the role isn't what failed.
                IngressFailure::Failed { .. } => None,
            },
        }
    }
}

/// What an ingress start did, or would do, on each Server that holds the ingress role.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct IngressCheck {
    pub repairs: Vec<IngressRepair>,
    /// One cause per ingress Server whose containers went unobserved, naming it.
    pub unchecked: Vec<String>,
}

/// Set the Ingress Proxy on every Server with the ingress role. Each daemon brings back
/// a missing or stopped one, and moves an older one to the wiring it pins; one that
/// already runs as pinned is left alone and reported as nothing. Each Server's answer
/// stands on its own, and a failure only reports.
///
/// # Errors
///
/// Fails when observation fails.
pub(crate) async fn fill_slots(
    client: &mut Client,
    cancel: &CancellationToken,
) -> Result<IngressCheck, Failure> {
    let doors = observe_doors(client).await?;
    if cancel.is_cancelled() {
        return Ok(IngressCheck::default());
    }
    let answers = set_doors(&*client, &doors.machines, doors.proxy, cancel).await;
    let mut check = IngressCheck::default();
    for (machine, answer) in doors.machines.iter().zip(answers) {
        match checked(machine, answer) {
            Ok(Some(repair)) => check.repairs.push(repair),
            Ok(None) => {}
            Err(cause) => check.unchecked.push(cause),
        }
    }
    Ok(check)
}

/// One Server's repair from its answer: `None` when its Ingress Proxy already ran as
/// pinned, and the unchecked cause when nothing on it is known.
fn checked(machine: &DoorMachine, answer: Answer) -> Result<Option<IngressRepair>, String> {
    let server = machine.name.to_string();
    let repair = |action, failure| {
        Ok(Some(IngressRepair {
            machine: machine.id,
            server: server.clone(),
            action,
            failure,
        }))
    };
    match answer {
        Answer::Cancelled => Err(format!(
            "{server} was still starting its Ingress Proxy when the Deploy was cancelled"
        )),
        Answer::Refused(error) if error.code == RpcErrorCode::Unsupported => {
            Err(crate::ui::row(&error))
        }
        Answer::Refused(error) if error.target_unanswered() => Err(format!(
            "Server {server} isn't answering ({})",
            crate::ui::row(&error)
        )),
        Answer::Refused(error) => repair(
            IngressAction::Create,
            Some(IngressFailure::Failed {
                cause: crate::ui::row(&error),
            }),
        ),
        Answer::Set(set) => {
            let failure = failure_of(&set);
            match (action_of(&set), failure) {
                (None, None) => Ok(None),
                (action, failure) => repair(action.unwrap_or(IngressAction::Create), failure),
            }
        }
    }
}

/// What the daemon did: a create when it made either component, a start when it only
/// started or rejoined one, and nothing when both already ran as pinned.
fn action_of(set: &IngressDoorsSet) -> Option<IngressAction> {
    let changes = [&set.holder, &set.proxy]
        .into_iter()
        .flatten()
        .map(|outcome| outcome.change);
    let mut action = None;
    for change in changes {
        match change {
            DoorChange::Created => return Some(IngressAction::Create),
            DoorChange::Started | DoorChange::Rejoined => action = Some(IngressAction::Start),
            DoorChange::Unchanged | DoorChange::Unrecognized => {}
        }
    }
    action
}

/// Why the Ingress Proxy isn't running after `set`, or `None` when the holder and Caddy
/// both run.
pub(crate) fn failure_of(set: &IngressDoorsSet) -> Option<IngressFailure> {
    if let Some(failure) = &set.failure {
        return Some(stage_failure(failure));
    }
    for (component, outcome) in [("holder", &set.holder), ("Caddy", &set.proxy)] {
        let Some(DoorOutcome { now, .. }) = outcome else {
            return Some(IngressFailure::Failed {
                cause: format!("its daemon set no Ingress Proxy {component}"),
            });
        };
        if let Some(cause) = not_running(now) {
            return Some(IngressFailure::Failed { cause });
        }
    }
    None
}

fn not_running(now: &ContainerObservation) -> Option<String> {
    match now.runtime {
        ContainerRuntimeObservation::Running { .. } => None,
        ContainerRuntimeObservation::Restarting => Some(crash_looping(&now.container_id)),
        ref other @ (ContainerRuntimeObservation::Created
        | ContainerRuntimeObservation::Paused
        | ContainerRuntimeObservation::Exited { .. }
        | ContainerRuntimeObservation::Removing
        | ContainerRuntimeObservation::Dead
        | ContainerRuntimeObservation::Unknown { .. }) => {
            Some(format!("it stopped right after starting: it is {other}"))
        }
    }
}

fn crash_looping(container_id: &ContainerId) -> String {
    format!(
        "its Ingress Proxy keeps crashing and Docker keeps restarting it; see why there with \
         `docker logs {container_id}`"
    )
}

/// A port only the holder publishes, so only its stage can find one taken.
fn stage_failure(failure: &IngressDoorsFailure) -> IngressFailure {
    let cause = crate::ui::row(&failure.error);
    let held = failure.stage == IngressStage::Holder
        && std::iter::once(&failure.error.message)
            .chain(&failure.error.cause)
            .any(|cause| port_taken(cause));
    if held {
        IngressFailure::PortTaken { cause }
    } else {
        IngressFailure::Failed { cause }
    }
}

/// The repairs [`fill_slots`] would run, by Server, from what each one lists; runs
/// nothing. A Server whose daemon predates SetIngressDoors is unchecked, since it keeps
/// the Ingress Proxy it runs.
///
/// # Errors
///
/// Fails when observation fails.
pub(crate) async fn planned_slots(client: &mut Client) -> Result<IngressCheck, Failure> {
    let machines = client.machines().await?;
    let doors = door_machines(&machines);
    let mut check = IngressCheck::default();
    for machine in doors.iter().filter(|machine| !machine.supported) {
        check
            .unchecked
            .push(crate::ui::row(&unsupported(&machine.name)));
    }
    for repair in client.plan_ingress_repairs().await? {
        let Some(machine) = doors.iter().find(|machine| machine.id == repair.machine_id) else {
            continue;
        };
        if !machine.supported {
            continue;
        }
        let server = machine.name.to_string();
        let shown = |action, failure| IngressRepair {
            machine: machine.id,
            server: server.clone(),
            action,
            failure,
        };
        check.repairs.push(match repair.action {
            SlotAction::Unchecked(cause) => {
                check
                    .unchecked
                    .push(format!("{server} went unobserved ({cause})"));
                continue;
            }
            SlotAction::Create => shown(IngressAction::Create, None),
            SlotAction::Start(_) => shown(IngressAction::Start, None),
            SlotAction::Restarting(container_id) => shown(
                IngressAction::Start,
                Some(IngressFailure::Failed {
                    cause: crash_looping(&container_id),
                }),
            ),
        });
    }
    Ok(check)
}

/// Why the ingress check through `server` failed, on one line. A connection failure
/// names the Server it went to, and when no daemon replied, says it isn't answering
/// with the transport's last word. Any other failure is about no Server and keeps its
/// whole line.
pub(crate) fn unchecked(
    failure: &Failure,
    server: Option<String>,
    known: Option<(&MachineId, &MachineName)>,
) -> String {
    let Some(connect) = failure.connect_error() else {
        return crate::ui::row(failure);
    };
    let named = if let ConnectError::AllFailed { server, .. } = connect {
        server.clone()
    } else {
        None
    };
    let name = |server: String| match known {
        Some((id, machine_name)) if server == id.to_string() => machine_name.to_string(),
        Some(_) | None => server,
    };
    let unanswered = connect.unreached() || connect.unreplied();
    match (named.map(&name), server.map(name)) {
        (Some(server), _) | (None, Some(server)) if unanswered => {
            match ployz_core::error_chain::causes(failure).pop() {
                Some(why) => format!("Server {server} isn't answering ({why})"),
                None => format!("Server {server} isn't answering"),
            }
        }
        // The failure names the Server it tried itself.
        (Some(_), _) | (None, None) => crate::ui::row(failure),
        (None, Some(server)) => format!("{} (on Server {server})", crate::ui::row(failure)),
    }
}

/// Whether Docker's `cause` says something else holds a port the Ingress Proxy publishes.
pub(crate) fn port_taken(cause: &str) -> bool {
    cause.contains("address already in use") || cause.contains("port is already allocated")
}

/// True when this observation is the reserved Ingress Proxy Service.
#[must_use]
pub fn is_system_ingress(observation: &ContainerObservation) -> bool {
    observation.identity() == QualifiedService::system_ingress()
}

#[cfg(test)]
#[path = "ingress_tests.rs"]
mod tests;
