//! The Server check of `ployz status`: no Server answering its one connection, each Down
//! Machine, each Service serving fewer copies than it should, and each live Machine whose
//! proxy lacks a Service's In copies or still routes its Out or Held ones. What the check
//! could not see is a warning, never an all-clear.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use clap::ArgMatches;
use futures_util::{StreamExt, stream};
use ipnet::Ipv4Net;
use ployz_core::{
    ContainerAddress, ContainerId, ContainerObservation, ContainerRuntimeObservation, CopyRotation,
    EnvironmentValues, LiveServices, MachineId, MachineName, MachineObservation, MachineTarget,
    MembershipObservation, Namespace, QualifiedService, ReadRoutedAddressesRequest,
    RoutedAddresses, RoutesRead, RpcError, RpcErrorCode, ServiceContainer, ServiceMode,
    ServiceObservation, Unacked, op, serving_containers,
};
use ployz_store::{DeploymentStatus, DeploymentSummary};
use serde::Serialize;

use super::link::Attention;
use super::server::{self, Reach, Unreached};
use super::{Error, leaf_matches};
use crate::cloud_login::LoginError;
use crate::context::ContextError;

pub(super) const UNCHECKED: &str =
    "Could not check which copies serve or which Servers picked up the new copies";

const READS_AT_ONCE: usize = 8;
/// A routed-address read answers from memory. A slow one warns rather than holding up the listing.
const ROUTED_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Subject {
    NotPickedUp {
        service: QualifiedService,
        /// Each such Machine by name, else by ID when the Machine list lacks it.
        machines: Vec<String>,
    },
    NotServing {
        service: QualifiedService,
        serving: usize,
        wanted: usize,
        copies: Vec<NotServingCopy>,
    },
    Down {
        machine: MachineName,
    },
    StaleAddress {
        service: QualifiedService,
        machine: String,
        addresses: Vec<ContainerAddress>,
    },
    /// The Servers that didn't answer, by name where known.
    Unanswered {
        servers: Vec<String>,
    },
}

/// A copy its owner keeps that does not serve.
#[derive(Serialize)]
pub(super) struct NotServingCopy {
    /// Its Machine by name, else by ID when the Machine list lacks it.
    machine: String,
    container: ContainerId,
    rotation: CopyRotation,
    runtime: ContainerRuntimeObservation,
}

pub(super) fn attention(
    root: &ArgMatches,
    namespace: &Namespace,
    deploying: &[DeploymentSummary],
) -> Vec<Attention> {
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    let named = context.is_some() || leaf.get_one::<String>("connect").is_some();
    let rolling_out = rolling_out(deploying);
    let observed = super::runtime()
        .map_err(Unreached::from)
        .and_then(|runtime| {
            runtime.block_on(async {
                match server::reach_or_silent(leaf, context).await? {
                    Some(Reach::Dial(mut client)) => {
                        Ok(Some(observe(&mut client, namespace, rolling_out).await?))
                    }
                    Some(Reach::Ask(_)) | None => Ok(None),
                }
            })
        });
    match observed {
        Ok(Some(observed)) => {
            let check = judge(observed, namespace, |words| super::store::next(leaf, words));
            check.warn();
            check.attention
        }
        Ok(None) => Vec::new(),
        Err(Unreached {
            silent: Some(silent),
            ..
        }) => vec![Attention {
            reason: "no_server_answers".to_owned(),
            message: format!("{silent}. {}", silent.next()),
            deployment: None,
            next: None,
            subject: Some(Subject::Unanswered {
                servers: silent.names().to_vec(),
            }),
        }],
        Err(Unreached { error, .. }) if !named && nothing_set_up(&error) => Vec::new(),
        Err(Unreached { error, .. }) => {
            crate::ui::warn_cause(UNCHECKED, &error);
            Vec::new()
        }
    }
}

fn rolling_out(deploying: &[DeploymentSummary]) -> Option<String> {
    deploying
        .iter()
        .find(|deployment| {
            matches!(
                deployment.status,
                DeploymentStatus::Running | DeploymentStatus::Cancelling
            )
        })
        .map(DeploymentSummary::label)
}

fn nothing_set_up(error: &Error) -> bool {
    let Some(source) = std::error::Error::source(error) else {
        return false;
    };
    source
        .downcast_ref::<ContextError>()
        .is_some_and(|error| *error == ContextError::NoConfig)
        || source
            .downcast_ref::<LoginError>()
            .is_some_and(|error| matches!(error, LoginError::SignedOut))
}

struct Observed {
    machines: Vec<MachineObservation>,
    copies: Copies,
}

enum Copies {
    RollingOut(String),
    Unlisted(Error),
    Listed {
        live: LiveServices<RpcError>,
        /// Each Service of the namespace that has serving copies, with its routes as
        /// one Server reads them.
        reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
        /// Each routed-address read, taken before the listing from every Machine it asks.
        routed: BTreeMap<MachineId, Result<RoutedAddresses, RpcError>>,
    },
}

async fn observe(
    client: &mut crate::connect::Client,
    namespace: &Namespace,
    rolling_out: Option<String>,
) -> Result<Observed, Error> {
    let machines = client.machines().await?;
    if let Some(deployment) = rolling_out {
        return Ok(Observed {
            machines,
            copies: Copies::RollingOut(deployment),
        });
    }
    // Read before listing, so a routed copy the listing lacks was removed, not created since.
    let routed = {
        let client = &*client;
        let up = machines
            .iter()
            .filter(|observed| observed.invites_rpc())
            .map(|observed| observed.machine.id);
        stream::iter(up)
            .map(|machine| async move {
                let routed = client
                    .invoke::<op::ReadRoutedAddresses>(
                        ReadRoutedAddressesRequest {},
                        &MachineTarget::from(&machine),
                        Some(ROUTED_READ_TIMEOUT),
                    )
                    .await;
                (machine, routed)
            })
            .buffer_unordered(READS_AT_ONCE)
            .collect()
            .await
    };
    let live = match client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await
    {
        Ok(live) => live,
        Err(error) => {
            return Ok(Observed {
                machines,
                copies: Copies::Unlisted(error.into()),
            });
        }
    };
    let reader = live
        .containers
        .successes
        .first()
        .map(|answered| answered.machine_id);
    let services = live.services().into_iter().filter(|service| {
        service.identity.namespace == *namespace
            && !serving_containers(&service.containers).is_empty()
    });
    let client = &*client;
    let reads = match reader {
        Some(reader) => {
            stream::iter(services)
                .map(|service| async move {
                    let read = crate::deploy::MachineOperations::read_routes(
                        client,
                        &reader,
                        &service.identity,
                    )
                    .await;
                    (service, read)
                })
                .buffered(READS_AT_ONCE)
                .collect()
                .await
        }
        None => Vec::new(),
    };
    Ok(Observed {
        machines,
        copies: Copies::Listed {
            live,
            reads,
            routed,
        },
    })
}

struct Check {
    attention: Vec<Attention>,
    unseen: Vec<Unseen>,
    names: HashMap<MachineId, MachineName>,
}

enum Unseen {
    RollingOut(String),
    Unlisted(Error),
    Unanswered(Vec<MachineId>),
    Unreported(BTreeSet<MachineId>),
    /// Live Machines whose daemon can't confirm what their proxy serves.
    Unconfirmed(BTreeSet<MachineId>),
    Unread {
        services: Vec<QualifiedService>,
        error: RpcError,
    },
    /// Live Machines the routes read did not judge; `None` when the Server read did not
    /// know every Machine yet.
    Unjudged {
        services: Vec<QualifiedService>,
        machines: Option<BTreeSet<MachineId>>,
    },
    DownToReader {
        services: Vec<QualifiedService>,
        machines: BTreeSet<MachineId>,
    },
}

struct ServicesByFact<Fact>(Vec<(Fact, Vec<QualifiedService>)>);

impl<Fact: PartialEq> ServicesByFact<Fact> {
    fn add(&mut self, fact: Fact, service: QualifiedService) {
        match self.0.iter_mut().find(|(known, _)| *known == fact) {
            Some((_, services)) => services.push(service),
            None => self.0.push((fact, vec![service])),
        }
    }
}

fn judge(observed: Observed, namespace: &Namespace, next: impl Fn(&[&str]) -> String) -> Check {
    let Observed { machines, copies } = observed;
    let down: BTreeSet<MachineId> = machines
        .iter()
        .filter(|observed| observed.membership == MembershipObservation::Down)
        .map(|observed| observed.machine.id)
        .collect();
    let mut check = Check {
        attention: machines
            .iter()
            .filter(|observed| down.contains(&observed.machine.id))
            .map(|observed| Attention {
                reason: "machine_down".to_owned(),
                message: "Down. Its copies serve again once it's back.".to_owned(),
                deployment: None,
                next: None,
                subject: Some(Subject::Down {
                    machine: observed.machine.name.clone(),
                }),
            })
            .collect(),
        unseen: Vec::new(),
        names: machines
            .iter()
            .map(|observed| (observed.machine.id, observed.machine.name.clone()))
            .collect(),
    };
    let (live, reads, routed) = match copies {
        Copies::RollingOut(deployment) => {
            check.unseen.push(Unseen::RollingOut(deployment));
            return check;
        }
        Copies::Unlisted(error) => {
            check.unseen.push(Unseen::Unlisted(error));
            return check;
        }
        Copies::Listed {
            live,
            reads,
            routed,
        } => (live, reads, routed),
    };
    let unanswered: Vec<MachineId> = live
        .containers
        .failures
        .iter()
        .map(|failure| failure.machine_id)
        .chain(live.containers.omissions.iter().copied())
        .filter(|machine| !down.contains(machine))
        .collect();
    let complete = down.is_empty()
        && live.containers.failures.is_empty()
        && live.containers.omissions.is_empty();
    for service in live.services() {
        if service.identity.namespace == *namespace
            && let Some(short) = check.not_serving(service, &down, complete, &next)
        {
            check.attention.push(short);
        }
    }
    if !unanswered.is_empty() {
        check.unseen.push(Unseen::Unanswered(unanswered.clone()));
    }
    let mut unconfirmed = BTreeSet::new();
    let mut confirmed = BTreeMap::new();
    for (machine, read) in routed {
        if down.contains(&machine) || unanswered.contains(&machine) {
            continue;
        }
        match read {
            Ok(RoutedAddresses {
                routes: Some(routes),
            }) => {
                confirmed.insert(machine, routes);
            }
            // An older Server lacks the check, and status claims nothing for it.
            Err(error) if error.code == RpcErrorCode::Unsupported => {}
            Ok(RoutedAddresses { routes: None }) | Err(_) => {
                unconfirmed.insert(machine);
            }
        }
    }
    let routed = confirmed;
    if !unconfirmed.is_empty() {
        check.unseen.push(Unseen::Unconfirmed(unconfirmed));
    }
    let mut unreported = BTreeSet::new();
    let mut unread = ServicesByFact(Vec::new());
    let mut unjudged = ServicesByFact(Vec::new());
    let mut down_to_reader = ServicesByFact(Vec::new());
    for (service, read) in reads {
        let unchecked: BTreeSet<MachineId> = match &read {
            Ok(read) => down.union(&read.down).copied().collect(),
            Err(_) => down.clone(),
        };
        let subnets: Vec<(Ipv4Net, bool)> = machines
            .iter()
            .map(|observed| {
                let id = &observed.machine.id;
                (
                    observed.machine.subnet.into(),
                    !unchecked.contains(id) && !unanswered.contains(id),
                )
            })
            .collect();
        for (machine, addresses) in stale_addresses(&service, &routed, &unchecked, &subnets) {
            let attention =
                stale_address(service.identity.clone(), check.name(&machine), addresses);
            check.attention.push(attention);
        }
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                unread.add(error, service.identity);
                continue;
            }
        };
        let up_copies: Vec<(MachineId, ContainerId)> = serving_containers(&service.containers)
            .iter()
            .map(|serving| serving.as_observation())
            .filter(|copy| !down.contains(&copy.machine_id))
            .map(|copy| (copy.machine_id, copy.container_id))
            .collect();
        if up_copies.is_empty() {
            continue;
        }
        let (reader_down, missing): (BTreeSet<MachineId>, BTreeSet<MachineId>) = machines
            .iter()
            .map(|observed| observed.machine.id)
            .filter(|machine| !down.contains(machine) && !read.routes.contains_key(machine))
            .partition(|machine| read.down.contains(machine));
        if !reader_down.is_empty() {
            down_to_reader.add(reader_down, service.identity.clone());
        }
        if !read.complete || !missing.is_empty() {
            unjudged.add(read.complete.then_some(missing), service.identity.clone());
        }
        let serving: BTreeSet<ContainerId> = up_copies
            .into_iter()
            .filter(|(machine, _)| !read.down.contains(machine))
            .map(|(_, container)| container)
            .collect();
        if serving.is_empty() {
            continue;
        }
        let retired: BTreeSet<ContainerId> = service
            .containers
            .iter()
            .map(ServiceContainer::as_observation)
            .filter(|copy| {
                matches!(copy.rotation(), CopyRotation::Out | CopyRotation::Held(_))
                    && !down.contains(&copy.machine_id)
                    && !read.down.contains(&copy.machine_id)
            })
            .map(|copy| copy.container_id)
            .collect();
        let mut waiting = Vec::new();
        for unacked in read
            .unacked(|routed| serving.is_subset(routed) && routed.is_disjoint(&retired))
            .filter(|unacked| !down.contains(&unacked.machine()))
        {
            match unacked {
                Unacked::Stale(machine) => waiting.push(check.name(&machine)),
                Unacked::Unanswered(machine) => {
                    unreported.insert(machine);
                }
            }
        }
        if !waiting.is_empty() {
            check
                .attention
                .push(not_picked_up(service.identity, waiting));
        }
    }
    if !unreported.is_empty() {
        check.unseen.push(Unseen::Unreported(unreported));
    }
    check.unseen.extend(
        unread
            .0
            .into_iter()
            .map(|(error, services)| Unseen::Unread { services, error }),
    );
    check.unseen.extend(
        unjudged
            .0
            .into_iter()
            .map(|(machines, services)| Unseen::Unjudged { services, machines }),
    );
    check.unseen.extend(
        down_to_reader
            .0
            .into_iter()
            .map(|(machines, services)| Unseen::DownToReader { services, machines }),
    );
    check
}

/// Addresses each live Server routes for this Service that no owner reports. A copy no
/// Machine lists counts only when every Machine whose subnet holds its address answered
/// and is up, since any of them could still report it. `subnets` pairs each Machine's
/// subnet with whether it answered and is up.
fn stale_addresses(
    service: &ServiceObservation,
    routed: &BTreeMap<
        MachineId,
        BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>>,
    >,
    down: &BTreeSet<MachineId>,
    subnets: &[(Ipv4Net, bool)],
) -> BTreeMap<MachineId, BTreeSet<ContainerAddress>> {
    let reported: BTreeMap<ContainerId, ContainerAddress> = serving_containers(&service.containers)
        .iter()
        .filter(|serving| !down.contains(&serving.as_observation().machine_id))
        .map(|serving| (serving.as_observation().container_id, serving.address()))
        .collect();
    let listed: BTreeSet<ContainerId> = service
        .containers
        .iter()
        .map(|copy| copy.as_observation().container_id)
        .collect();
    let owners_answered = |address: &ContainerAddress| {
        let mut holders = subnets
            .iter()
            .filter(|(subnet, _)| subnet.contains(&address.0))
            .peekable();
        holders.peek().is_some() && holders.all(|(_, answered)| *answered)
    };
    let removed = |copy: &ContainerId, address: &ContainerAddress| {
        !listed.contains(copy)
            && !reported.values().any(|reported| reported == address)
            && owners_answered(address)
    };
    routed
        .iter()
        .filter(|(machine, _)| !down.contains(machine))
        .filter_map(|(machine, routed)| {
            let stale: BTreeSet<ContainerAddress> = routed
                .get(&service.identity)?
                .iter()
                .filter(|(copy, address)| match reported.get(*copy) {
                    Some(reported) => reported != *address,
                    None => removed(copy, address),
                })
                .map(|(_, address)| *address)
                .collect();
            (!stale.is_empty()).then_some((*machine, stale))
        })
        .collect()
}

fn stale_address(
    service: QualifiedService,
    machine: String,
    addresses: BTreeSet<ContainerAddress>,
) -> Attention {
    let addresses: Vec<ContainerAddress> = addresses.into_iter().collect();
    let (copies, owners) = match addresses.as_slice() {
        [_] => ("a copy", "its owner does"),
        _ => ("copies", "their owners do"),
    };
    let listed = addresses
        .iter()
        .map(|address| address.0.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Attention {
        reason: "routes_stale_address".to_owned(),
        message: format!(
            "{machine} routes {copies} to {listed}, which {owners} not report. Check its network."
        ),
        deployment: None,
        next: None,
        subject: Some(Subject::StaleAddress {
            service,
            machine,
            addresses,
        }),
    }
}

fn not_picked_up(service: QualifiedService, machines: Vec<String>) -> Attention {
    let (have, its) = match machines.as_slice() {
        [_] => ("has", "its"),
        _ => ("have", "their"),
    };
    Attention {
        reason: "routes_not_acked".to_owned(),
        message: format!(
            "{} {have} not picked up the new copies. Check {its} network.",
            machines.join(", ")
        ),
        deployment: None,
        next: None,
        subject: Some(Subject::NotPickedUp { service, machines }),
    }
}

fn service_names(services: &[QualifiedService]) -> String {
    services
        .iter()
        .map(|service| service.name.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// How many of a Service's seen copies serve, how many should, and the kept copies that
/// do not, when fewer serve than should. A copy is kept when it is In and not stopped. An
/// Out copy still running counts as kept only for a Replicated Service in a `complete`
/// view: it stands in for a missing In copy only when every copy was seen. Each Held copy
/// explains one missing In copy.
fn shortfall<'service>(
    service: &'service ServiceObservation,
    down: &BTreeSet<MachineId>,
    complete: bool,
) -> Option<(usize, usize, Vec<&'service ContainerObservation>)> {
    let up: Vec<&ServiceContainer> = service
        .containers
        .iter()
        .filter(|copy| !down.contains(&copy.as_observation().machine_id))
        .collect();
    let serving: BTreeSet<ContainerId> = serving_containers(up.iter().copied())
        .iter()
        .map(|serving| serving.as_observation().container_id)
        .collect();
    let held = up
        .iter()
        .filter(|copy| matches!(copy.as_observation().rotation(), CopyRotation::Held(_)))
        .count();
    let candidates: Vec<&ContainerObservation> = up
        .iter()
        .map(|copy| copy.as_observation())
        .filter(|copy| match copy.rotation() {
            CopyRotation::In => !copy.stopped,
            CopyRotation::Out => {
                matches!(copy.runtime, ContainerRuntimeObservation::Running { .. })
            }
            CopyRotation::Pending | CopyRotation::Held(_) => false,
        })
        .collect();
    let newest = |copies: &[&'service ContainerObservation]| {
        copies
            .iter()
            .copied()
            .max_by_key(|copy| (copy.created_at_unix_nanos, copy.container_id))
    };
    let replicated = matches!(
        newest(&candidates)?.resolved_spec.mode,
        ServiceMode::Replicated { .. }
    );
    let mut kept: Vec<&ContainerObservation> = candidates
        .into_iter()
        .filter(|copy| *copy.rotation() == CopyRotation::In || (complete && replicated))
        .collect();
    let wanted = match newest(&kept)?.resolved_spec.mode {
        ServiceMode::Replicated { replicas } => usize::try_from(replicas.get())
            .unwrap_or(usize::MAX)
            .saturating_sub(held)
            .min(kept.len()),
        ServiceMode::Global => kept.len(),
    };
    if serving.len() >= wanted {
        return None;
    }
    kept.retain(|copy| !serving.contains(&copy.container_id));
    kept.sort_by_key(|copy| (copy.machine_id, copy.container_id));
    Some((serving.len(), wanted, kept))
}

impl Check {
    /// The row for a Service serving fewer copies than it should, per [`shortfall`].
    fn not_serving(
        &self,
        service: ServiceObservation,
        down: &BTreeSet<MachineId>,
        complete: bool,
        next: impl Fn(&[&str]) -> String,
    ) -> Option<Attention> {
        let (serving, wanted, kept) = shortfall(&service, down, complete)?;
        let states: Vec<String> = kept
            .iter()
            .map(|copy| {
                let words = match copy.rotation() {
                    CopyRotation::Out => "draining".to_owned(),
                    CopyRotation::In | CopyRotation::Pending | CopyRotation::Held(_) => {
                        super::service::process_state(copy)
                    }
                };
                format!("{} {words}", self.name(&copy.machine_id))
            })
            .collect();
        let name = service.identity.name.as_str();
        let hint = if kept.iter().any(|copy| *copy.rotation() == CopyRotation::In) {
            next(&["logs", name])
        } else {
            next(&["deploy", name])
        };
        Some(Attention {
            reason: "copies_not_serving".to_owned(),
            message: format!(
                "Serving {serving} of {wanted} {}. {}.",
                if wanted == 1 { "copy" } else { "copies" },
                states.join(", ")
            ),
            deployment: None,
            next: Some(hint),
            subject: Some(Subject::NotServing {
                service: service.identity.clone(),
                serving,
                wanted,
                copies: kept
                    .into_iter()
                    .map(|copy| NotServingCopy {
                        machine: self.name(&copy.machine_id),
                        container: copy.container_id,
                        rotation: copy.rotation().clone(),
                        runtime: copy.runtime.clone(),
                    })
                    .collect(),
            }),
        })
    }

    fn name(&self, machine: &MachineId) -> String {
        self.names
            .get(machine)
            .map_or_else(|| machine.to_string(), ToString::to_string)
    }

    fn names<'machine>(&self, machines: impl IntoIterator<Item = &'machine MachineId>) -> String {
        machines
            .into_iter()
            .map(|machine| self.name(machine))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn warn(&self) {
        for unseen in &self.unseen {
            match unseen {
                Unseen::RollingOut(deployment) => crate::ui::warn(format!(
                    "Not checking which copies serve or which Servers picked up the new copies while deployment {deployment} is in progress."
                )),
                Unseen::Unlisted(error) => crate::ui::warn_cause(UNCHECKED, error),
                Unseen::Unanswered(machines) => crate::ui::warn(format!(
                    "{} did not answer, so status may miss copies that do not serve and Servers that have not picked up the new copies.",
                    self.names(machines)
                )),
                Unseen::Unreported(machines) => {
                    let (has, it, routes) = match machines.len() {
                        1 => ("has", "it", "routes"),
                        _ => ("have", "they", "route"),
                    };
                    crate::ui::warn(format!(
                        "{} {has} not reported which copies {it} {routes}.",
                        self.names(machines)
                    ));
                }
                Unseen::Unconfirmed(machines) => {
                    let (has, its, proxy, it) = match machines.len() {
                        1 => ("has", "its", "proxy serves", "it"),
                        _ => ("have", "their", "proxies serve", "them"),
                    };
                    crate::ui::warn(format!(
                        "{} {has} not confirmed what {its} {proxy}, so status is not checking {it} for stale addresses.",
                        self.names(machines)
                    ));
                }
                Unseen::Unread { services, error } => crate::ui::warn_cause(
                    format_args!(
                        "Could not check which Servers picked up the new copies of {}",
                        service_names(services)
                    ),
                    error,
                ),
                Unseen::Unjudged { services, machines } => crate::ui::warn(format!(
                    "Could not check whether {} picked up the new copies of {}.",
                    machines.as_ref().map_or_else(
                        || "every Server".to_owned(),
                        |machines| self.names(machines)
                    ),
                    service_names(services)
                )),
                Unseen::DownToReader { services, machines } => {
                    let (it, routes, its) = match machines.len() {
                        1 => ("it", "routes", "its"),
                        _ => ("they", "route", "their"),
                    };
                    crate::ui::warn(format!(
                        "The Server status asked sees {} Down, so status is not checking what {it} {routes} or who routes {its} copies of {}.",
                        self.names(machines),
                        service_names(services)
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "status_servers_tests.rs"]
mod tests;
