//! The Ingress Proxy: each Server with the ingress role asks its own daemon to bring the
//! holder, the Caddy in its network and the Tunnel Connector to the wiring that daemon
//! pins, through SetIngressDoors. Nothing here builds a spec or names an image.

use ployz_core::error_chain;
use std::collections::BTreeMap;

use futures_util::{StreamExt, stream};
use ployz_core::{
    ContainerId, ContainerObservation, ContainerRuntimeObservation, DescribeContractRequest,
    DoorChange, DoorOutcome, EnvironmentValues, IngressDoor, IngressDoors, IngressDoorsFailure,
    IngressDoorsSet, IngressPorts, IngressStage, IngressWiring, Machine, MachineFailure, MachineId,
    MachineName, MachineObservation, MachineTarget, PartialResult, PlatformPurpose,
    QualifiedService, RowStanding, RpcError, RpcErrorCode, SET_CLUSTER_DOORS_CAPABILITY,
    SET_INGRESS_DOORS_CAPABILITY, SET_INGRESS_TUNNEL_CAPABILITY, Sealing, SealingPublicKey,
    ServiceId, ServiceObservation, SetClusterDoorsRequest, op, seal_platform,
    validate_ingress_service_spec,
};
use ployz_store::{IngressAction, IngressFailure, IngressRepair};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::{
    cluster::Call,
    connect::{Client, ConnectError},
    deploy::{MachineOperations, RestartTolerant, SlotAction},
    failure::Failure,
    ui::progress::{Detail, Diagnostic, StartFailure},
};

/// How many Servers a role change sets the Ingress Proxy on at once. A cutover or pin
/// change costs each running Server a short gap on 80 and 443, so one at a time keeps
/// every other Server serving through it.
const FOLLOW_AT_ONCE: usize = 1;

/// How many Servers a Deploy repairs at once. Each one's Ingress Proxy is already down,
/// so a slow Server must not hold back the others.
const REPAIRS_AT_ONCE: usize = 8;

/// A Server whose Ingress Proxy a command sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DoorMachine {
    pub(crate) id: MachineId,
    pub(crate) name: MachineName,
    /// Whether its daemon may serve SetIngressDoors. One that advertises other
    /// capabilities but not this one keeps the Ingress Proxy it runs, and is named as
    /// needing a newer Ployz.
    pub(crate) supported: bool,
    /// Whether its daemon opens the Tunnel Door. One that doesn't keeps its Doors and is
    /// named as needing a newer Ployz when the Cluster uses the tunnel.
    pub(crate) tunnels: bool,
}

impl DoorMachine {
    pub(crate) fn of(machine: &Machine) -> Self {
        let capabilities = &machine.runtime.capabilities;
        Self {
            id: machine.id,
            name: machine.name.clone(),
            // An empty list is a row that predates it, which claims nothing, so the
            // daemon is asked and answers for itself.
            supported: capabilities.is_empty()
                || capabilities.contains(SET_INGRESS_DOORS_CAPABILITY),
            tunnels: capabilities.is_empty()
                || capabilities.contains(SET_INGRESS_TUNNEL_CAPABILITY),
        }
    }

    /// The refusal for this Server when its daemon can't set `doors`, if any.
    pub(crate) fn refusal(&self, doors: &IngressDoors) -> Option<RpcError> {
        if !self.supported {
            return Some(unsupported(&self.name, SET_INGRESS_DOORS_CAPABILITY));
        }
        if doors.has(IngressDoor::Tunnel) && !self.tunnels {
            return Some(unsupported(&self.name, SET_INGRESS_TUNNEL_CAPABILITY));
        }
        None
    }
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
        .map(|machine| DoorMachine::of(&machine.machine))
        .collect()
}

pub(crate) use ployz_core::{ClusterDoors, cluster_doors};

/// The Doors each Server holds open now: the Public Door when a holder with the host
/// ports, or an older Caddy publishing them, runs there, and the Tunnel Door when a
/// Tunnel Connector runs there. A Server with no running door Container holds none.
pub(crate) fn machine_doors(services: &[ServiceObservation]) -> BTreeMap<MachineId, IngressDoors> {
    let mut doors: BTreeMap<MachineId, IngressDoors> = BTreeMap::new();
    for service in services
        .iter()
        .filter(|service| service.identity.is_ingress_door())
    {
        for copy in service.containers.iter().map(|copy| copy.as_observation()) {
            if !matches!(copy.runtime, ContainerRuntimeObservation::Running { .. }) {
                continue;
            }
            let door = match validate_ingress_service_spec(&copy.resolved_spec).ok() {
                Some(IngressWiring::LegacyProxy | IngressWiring::Holder(IngressPorts::Public)) => {
                    IngressDoor::Public
                }
                Some(IngressWiring::Tunnel) => IngressDoor::Tunnel,
                Some(IngressWiring::JoinedProxy | IngressWiring::Holder(IngressPorts::Closed))
                | None => continue,
            };
            doors
                .entry(copy.machine_id)
                .or_insert_with(|| IngressDoors(Default::default()))
                .0
                .insert(door);
        }
    }
    doors
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
                .map(|copy| copy.as_observation().recency_key())
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

/// A Server that serves SetClusterDoors, reached through the entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Declarer {
    pub(crate) machine: MachineId,
    pub(crate) server: MachineName,
}

/// Where the command declares the Cluster's Doors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Declaration {
    /// The entry Machine serves SetClusterDoors.
    Entry,
    /// The entry predates it, and these Servers, each online and not leaving, serve it.
    /// A rolling upgrade has the entry and its peers on different versions, and the
    /// declaration still has to land in the Cluster's one key.
    Through(Vec<Declarer>),
    /// No Server that could be asked serves it, so a command sets each Server's Doors as
    /// it did before the declaration existed.
    Unsupported,
}

/// The Servers' Ingress Proxies observed in full, with every Server that should run one.
pub(crate) struct Doors {
    pub(crate) machines: Vec<DoorMachine>,
    pub(crate) proxy: ServiceId,
    /// The Doors a Server's Ingress Proxy follows: the Cluster's declared Doors, or what
    /// the Servers run when none is declared.
    pub(crate) cluster: ClusterDoors,
    /// The Doors the Cluster declares, as the entry Machine's replica holds them.
    pub(crate) declared: Option<IngressDoors>,
    /// Who declares Doors for the Cluster.
    pub(crate) declaration: Declaration,
    /// What each Server holds open now, by Server; one that listed nothing is absent.
    pub(crate) open: BTreeMap<MachineId, IngressDoors>,
    /// The listing the rest derives from, with the Servers that didn't answer it.
    pub(crate) listed: PartialResult<Vec<ContainerObservation>, RpcError>,
    /// The Cluster's Sealing Key, from the first Server that holds it.
    pub(crate) sealing: Option<SealingPublicKey>,
}

/// The Doors to open: `doors`, with `token` sealed to the Cluster when the Tunnel Door is
/// among them, or the token the Cluster already runs when none is given.
///
/// # Errors
///
/// Fails when the Tunnel Door needs a token and none is given or observed, or when no
/// Server holds the Sealing Key to seal it for.
pub(crate) fn cluster_doors_for(
    observed: &Doors,
    doors: IngressDoors,
    token: Option<&str>,
) -> Result<ClusterDoors, Failure> {
    if !doors.has(IngressDoor::Tunnel) {
        return Ok(ClusterDoors {
            doors,
            sealed_token: None,
        });
    }
    let sealed_token = match token {
        Some(token) => {
            let Some(sealing) = &observed.sealing else {
                return Err(Failure::coded(
                    RpcErrorCode::Unavailable,
                    "No online Server holds the Cluster's Sealing Key, so the tunnel token can't \
                     be sealed.",
                ));
            };
            seal_platform(sealing, PlatformPurpose::IngressTunnel, token.as_bytes()).map_err(
                |error| {
                    Failure::caused(
                        RpcErrorCode::Internal,
                        "Could not seal the tunnel token for this Cluster",
                        error,
                    )
                },
            )?
        }
        None => observed.cluster.sealed_token.clone().ok_or_else(|| {
            Failure::usage(
                "The Tunnel Door needs the tunnel's Cloudflare token. Pipe it with --token-stdin.",
            )
        })?,
    };
    Ok(ClusterDoors {
        doors,
        sealed_token: Some(sealed_token),
    })
}

pub(crate) async fn observe_doors(client: &mut Client) -> Result<Doors, Failure> {
    let list = client.machine_list().await?;
    let declared = list
        .enrollment
        .as_ref()
        .and_then(|snapshot| snapshot.doors.clone());
    let machines = list.machines;
    let declaration = declaration_of(client, &machines).await?;
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
        return Err(Failure::caused(
            RpcErrorCode::Unavailable,
            format!(
                "Server {} didn't list its containers, so which Ingress Proxy the Cluster runs is \
                 unknown.",
                failure.machine_id
            ),
            failure.error.clone(),
        ));
    }
    let sealing = machines
        .iter()
        .find_map(|machine| match &machine.machine.runtime.sealing {
            Some(Sealing::Held { public_key }) => Some(*public_key),
            Some(Sealing::NeedsAccess | Sealing::AwaitsRecovery { .. }) | None => None,
        });
    Ok(Doors {
        machines: door_machines(&machines),
        proxy: proxy_service(&services),
        cluster: cluster_doors(declared.clone(), &services),
        declared,
        declaration,
        open: machine_doors(&services),
        listed: live.containers,
        sealing,
    })
}

/// Where the Cluster's Doors are declared: the entry when it serves SetClusterDoors, else
/// the Servers that advertise it and can be asked. An entry that can't be asked fails the
/// command: reading it as unsupported would set Doors the declaration then disagrees with.
async fn declaration_of(
    client: &mut Client,
    machines: &[MachineObservation],
) -> Result<Declaration, Failure> {
    let contract = client
        .call::<op::DescribeContract>(DescribeContractRequest {}, Call::read())
        .await?;
    if contract.supports(SET_CLUSTER_DOORS_CAPABILITY) {
        return Ok(Declaration::Entry);
    }
    let declarers = declarers_of(machines);
    Ok(if declarers.is_empty() {
        Declaration::Unsupported
    } else {
        Declaration::Through(declarers)
    })
}

/// The Servers that advertise SetClusterDoors and can be asked: online, and not leaving,
/// since a leaving or silent Server's advertisement is no route.
fn declarers_of(machines: &[MachineObservation]) -> Vec<Declarer> {
    machines
        .iter()
        .filter(|observation| {
            observation.invites_rpc()
                && !matches!(observation.machine.standing(), RowStanding::Leaving { .. })
                && observation
                    .machine
                    .runtime
                    .capabilities
                    .contains(SET_CLUSTER_DOORS_CAPABILITY)
        })
        .map(|observation| Declarer {
            machine: observation.machine.id,
            server: observation.machine.name.clone(),
        })
        .collect()
}

/// Declare `doors` as the Cluster's Doors, so a joiner, a restart and every reader follow
/// them. It writes even when the entry already shows them, since another operator's
/// declaration may not have reached it yet. An entry that can't keep a declaration hands
/// it to a Server that can, through the route the Doors themselves take. Nothing is
/// written when no Server can.
///
/// # Errors
///
/// Fails when the Server asked refuses or can't be reached. A reply lost after the Server
/// committed leaves the Doors declared with no Server switched, which a rerun converges.
async fn declare(client: &Client, observed: &Doors, doors: &IngressDoors) -> Result<(), Failure> {
    let request = || SetClusterDoorsRequest {
        doors: doors.clone(),
    };
    match &observed.declaration {
        Declaration::Unsupported => {
            crate::ui::warn(
                "No Server can keep the Cluster's Doors, so each Server's Doors are set without \
                 declaring them. Upgrade a Server to have a joiner and a restart follow them.",
            );
            Ok(())
        }
        Declaration::Entry => {
            client
                .call_shared::<op::SetClusterDoors>(request(), Call::once())
                .await?;
            Ok(())
        }
        Declaration::Through(declarers) => {
            let mut last = None;
            for declarer in declarers {
                match client
                    .call_shared::<op::SetClusterDoors>(
                        request(),
                        Call::once().to(&MachineTarget::from(&declarer.machine)),
                    )
                    .await
                {
                    Ok(_) => return Ok(()),
                    Err(error) => last = Some(error),
                }
            }
            let error = last.expect("a Through declaration names a Server");
            Err(Failure::caused(
                RpcErrorCode::Unavailable,
                "No Server confirmed the Cluster's Doors, so none were switched. Run the command \
                 again, or upgrade the Server you connect through.",
                error,
            ))
        }
    }
}

/// Ask each of `machines` to set `doors` for `proxy`, `at_once` at a time, in order.
/// A call whose reply was lost, or that met a restarting daemon, is sent again, since
/// the daemon converges on a resend.
async fn set_doors<C: MachineOperations>(
    client: &C,
    machines: &[DoorMachine],
    proxy: ServiceId,
    doors: &ClusterDoors,
    at_once: usize,
    cancel: &CancellationToken,
) -> Vec<Answer> {
    let client = &RestartTolerant::new(client, cancel);
    // Each future owns its Server, since one borrowing from the slice leaves the
    // caller's future Send for one lifetime only, and a spawned Deploy runner needs all.
    stream::iter(machines.to_vec())
        .map(|machine| set_door(client, machine, proxy, doors, cancel))
        .buffered(at_once)
        .collect()
        .await
}

async fn set_door<C: MachineOperations>(
    client: &C,
    machine: DoorMachine,
    proxy: ServiceId,
    doors: &ClusterDoors,
    cancel: &CancellationToken,
) -> Answer {
    if let Some(refusal) = machine.refusal(&doors.doors) {
        return Answer::Refused(refusal);
    }
    let request = doors.request(proxy);
    tokio::select! {
        answer = client.set_ingress_doors(&machine.id, request) => match answer {
            Ok(set) => Answer::Set(Box::new(set)),
            Err(error) => Answer::Refused(error),
        },
        () = cancel.cancelled() => Answer::Cancelled,
    }
}

/// The refusal for a Server whose daemon predates `capability`.
pub(crate) fn unsupported(server: &MachineName, capability: &str) -> RpcError {
    let details = serde_json::json!({
        "target": server.to_string(),
        "capability": capability,
    });
    let refusal = ployz_core::CapabilityRefusal::new(&RpcErrorCode::Unsupported, details.clone())
        .expect("the details name a target and a capability");
    RpcError::unsupported(refusal.to_string()).with_details(details)
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
    let cluster = doors.cluster.clone();
    set_roles(client, doors, &cluster).await
}

/// Declare the Cluster closed when it declares nothing yet, as a Cluster founded closed
/// starts. A Cluster that already declares Doors keeps them, so a rerun of the founding
/// follows what the operator set since. An entry that can't keep a declaration declares
/// through an online Server that can, and nothing when none can. Through such an entry the
/// snapshot carries no declaration, so a Cluster that already declares Doors is declared
/// closed again.
///
/// # Errors
///
/// Fails when the entry can't be read or refuses the declaration.
pub(crate) async fn declare_founding_closed(client: &mut Client) -> Result<(), Failure> {
    let observed = observe_doors(client).await?;
    if observed.declared.is_none() {
        declare(client, &observed, &IngressDoors::closed()).await?;
    }
    Ok(())
}

/// Set the Ingress Proxy on every Server that holds the ingress role with no Door open,
/// as a Cluster founded closed starts: Caddy runs, and ports 80 and 443 never open, until
/// `ployz ingress use tunnel` opens the Tunnel Door. A Cluster that declares Doors keeps
/// them instead.
///
/// # Errors
///
/// As [`follow_roles`].
pub(crate) async fn found_closed(client: &mut Client) -> Result<Vec<Followed>, NotFollowed> {
    let doors = observe_doors(client).await?;
    let cluster = if doors.declared.is_some() {
        doors.cluster.clone()
    } else {
        ClusterDoors::closed()
    };
    set_roles(client, doors, &cluster).await
}

async fn set_roles(
    client: &Client,
    doors: Doors,
    cluster: &ClusterDoors,
) -> Result<Vec<Followed>, NotFollowed> {
    let answers = set_doors(
        client,
        &doors.machines,
        doors.proxy,
        cluster,
        FOLLOW_AT_ONCE,
        &CancellationToken::new(),
    )
    .await;
    followed(doors.machines, answers)
}

/// One Server's Doors after a command set or read them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineDoors {
    pub machine: MachineId,
    pub server: String,
    /// The Doors open on it, by name; empty when it holds none open.
    pub doors: Vec<String>,
}

/// What [`use_doors`] left on every Server with the ingress role.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct DoorsChanged {
    pub machines: Vec<MachineDoors>,
    /// Each Server whose Doors aren't as asked after, with why.
    pub failures: Vec<MachineFailure<RpcError>>,
}

/// Open `doors` on every Server with the ingress role, one at a time. A switch to the
/// Tunnel Door opens the tunnel beside the Public Door on every Server first, so the
/// host ports close only once every Server's connector can take the traffic. With no
/// Public Door open anywhere, there is nothing to hand over, so a rerun leaves the
/// doors that already run.
///
/// # Errors
///
/// Fails when observation fails. A Server's own refusal or Docker failure is one of the
/// result's failures, never an error.
pub(crate) async fn use_doors(
    client: &mut Client,
    observed: Doors,
    doors: ClusterDoors,
    cancel: &CancellationToken,
) -> Result<DoorsChanged, Failure> {
    declare(client, &observed, &doors.doors).await?;
    let mut machines = observed.machines;
    let mut failures = Vec::new();
    machines.retain(|machine| match machine.refusal(&doors.doors) {
        Some(error) => {
            failures.push(MachineFailure {
                machine_id: machine.id,
                error,
            });
            false
        }
        None => true,
    });
    let steps = switch_steps(doors, &observed.open);
    let mut changed = DoorsChanged::default();
    for step in &steps {
        let answers = set_doors(
            &*client,
            &machines,
            observed.proxy,
            step,
            FOLLOW_AT_ONCE,
            cancel,
        )
        .await;
        let mut kept = Vec::new();
        changed.machines.clear();
        for (machine, answer) in machines.into_iter().zip(answers) {
            match answer {
                Answer::Set(set) => match failure_of(&set) {
                    None => {
                        changed.machines.push(MachineDoors {
                            machine: machine.id,
                            server: machine.name.to_string(),
                            doors: doors_of(&set),
                        });
                        kept.push(machine);
                    }
                    Some(failure) => failures.push(MachineFailure {
                        machine_id: machine.id,
                        error: RpcError::unavailable(format!(
                            "The Ingress Proxy on {} isn't running: {}",
                            machine.name,
                            failure.cause()
                        )),
                    }),
                },
                Answer::Refused(error) => failures.push(MachineFailure {
                    machine_id: machine.id,
                    error,
                }),
                Answer::Cancelled => failures.push(MachineFailure {
                    machine_id: machine.id,
                    error: RpcError::unavailable(format!(
                        "{} was still setting its Doors when the command was cancelled",
                        machine.name
                    )),
                }),
            }
        }
        machines = kept;
        if cancel.is_cancelled() {
            break;
        }
    }
    changed.failures = failures;
    Ok(changed)
}

/// The Doors each Server is asked for in turn: through both Doors first when a switch to
/// the Tunnel Door closes a Public Door some Server holds open now.
fn switch_steps(
    doors: ClusterDoors,
    open: &BTreeMap<MachineId, IngressDoors>,
) -> Vec<ClusterDoors> {
    let public_open = open.values().any(|open| open.has(IngressDoor::Public));
    if doors.doors == IngressDoors::tunnel() && public_open {
        vec![
            ClusterDoors {
                doors: IngressDoors::both(),
                sealed_token: doors.sealed_token.clone(),
            },
            doors,
        ]
    } else {
        vec![doors]
    }
}

/// The Doors `set` left open, by name: the Public Door when the holder publishes the
/// host ports, the Tunnel Door when a connector runs.
fn doors_of(set: &IngressDoorsSet) -> Vec<String> {
    let mut doors = Vec::new();
    let public = set.holder.as_ref().is_some_and(|holder| {
        matches!(
            validate_ingress_service_spec(&holder.now.resolved_spec).ok(),
            Some(IngressWiring::Holder(IngressPorts::Public) | IngressWiring::LegacyProxy)
        )
    });
    if public {
        doors.push(IngressDoor::Public.as_str().to_owned());
    }
    if set.tunnel.is_some() {
        doors.push(IngressDoor::Tunnel.as_str().to_owned());
    }
    doors
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
                said: error_chain::row(&error),
                cause: error_chain::row(&error),
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
            IngressStage::Tunnel => "Tunnel Connector",
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

/// Bring back the Ingress Proxy on every Server with the ingress role whose observation
/// shows none Docker can start, or one at rest. A Server whose Ingress Proxy runs is not
/// asked, so a Deploy never moves a running one to new pins; `ployz server upgrade` and
/// `ployz server set` do. Each Server's answer stands on its own, and a failure only
/// reports.
///
/// # Errors
///
/// Fails when observation fails.
pub(crate) async fn fill_slots(
    client: &mut Client,
    cancel: &CancellationToken,
) -> Result<IngressCheck, Failure> {
    let Planned { check, missing } = plan_slots(client).await?;
    if cancel.is_cancelled() {
        return Ok(IngressCheck::default());
    }
    if missing.is_empty() {
        return Ok(check);
    }
    let doors = observe_doors(client).await?;
    let answers = set_doors(
        &*client,
        &missing,
        doors.proxy,
        &doors.cluster,
        REPAIRS_AT_ONCE,
        cancel,
    )
    .await;
    let mut answered = missing
        .iter()
        .zip(answers)
        .map(|(machine, answer)| (machine.id, checked(machine, answer)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut filled = IngressCheck {
        repairs: Vec::new(),
        unchecked: check.unchecked,
    };
    for planned in check.repairs {
        match answered.remove(&planned.machine) {
            None => filled.repairs.push(planned),
            Some(Ok(Some(repair))) => filled.repairs.push(repair),
            Some(Ok(None)) => {}
            Some(Err(cause)) => filled.unchecked.push(cause),
        }
    }
    Ok(filled)
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
            Err(error_chain::row(&error))
        }
        Answer::Refused(error) if error.target_unanswered() => Err(format!(
            "Server {server} isn't answering ({})",
            error_chain::row(&error)
        )),
        Answer::Refused(error) => repair(
            IngressAction::Create,
            Some(IngressFailure::Failed {
                cause: error_chain::row(&error),
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

/// What the daemon did: a create when it made any component, a start when it only
/// started or rejoined one, and nothing when all already ran as pinned.
fn action_of(set: &IngressDoorsSet) -> Option<IngressAction> {
    let changes = [&set.holder, &set.proxy, &set.tunnel]
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
/// both run, and the Tunnel Connector when the daemon set one.
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
    if let Some(DoorOutcome { now, .. }) = &set.tunnel
        && let Some(cause) = not_running(now)
    {
        return Some(IngressFailure::Failed { cause });
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
    let cause = error_chain::row(&failure.error);
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
/// nothing. A Server whose daemon predates SetIngressDoors and needs a repair is
/// unchecked, since it keeps the Ingress Proxy it runs.
///
/// # Errors
///
/// Fails when observation fails.
pub(crate) async fn planned_slots(client: &mut Client) -> Result<IngressCheck, Failure> {
    Ok(plan_slots(client).await?.check)
}

/// What each ingress Server's observation asks of its Ingress Proxy.
struct Planned {
    check: IngressCheck,
    /// The Servers with none Docker can start, or one at rest, to set the doors on.
    missing: Vec<DoorMachine>,
}

async fn plan_slots(client: &mut Client) -> Result<Planned, Failure> {
    let machines = client.machines().await?;
    let doors = door_machines(&machines);
    let mut check = IngressCheck::default();
    let mut missing = Vec::new();
    for repair in client.plan_ingress_repairs().await? {
        let Some(machine) = doors.iter().find(|machine| machine.id == repair.machine_id) else {
            continue;
        };
        let server = machine.name.to_string();
        if !machine.supported && !matches!(repair.action, SlotAction::Unchecked(_)) {
            check.unchecked.push(error_chain::row(&unsupported(
                &machine.name,
                SET_INGRESS_DOORS_CAPABILITY,
            )));
            continue;
        }
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
            SlotAction::Create => {
                missing.push(machine.clone());
                shown(IngressAction::Create, None)
            }
            SlotAction::Start(_) => {
                missing.push(machine.clone());
                shown(IngressAction::Start, None)
            }
            SlotAction::Restarting(container_id) => shown(
                IngressAction::Start,
                Some(IngressFailure::Failed {
                    cause: crash_looping(&container_id),
                }),
            ),
        });
    }
    Ok(Planned { check, missing })
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
        return error_chain::row(failure);
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
        (Some(_), _) | (None, None) => error_chain::row(failure),
        (None, Some(server)) => format!("{} (on Server {server})", error_chain::row(failure)),
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
