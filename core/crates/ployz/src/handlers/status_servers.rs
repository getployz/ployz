use std::collections::{BTreeMap, BTreeSet, HashMap};

use clap::ArgMatches;
use futures_util::{StreamExt, stream};
use ipnet::Ipv4Net;
use ployz_core::{
    ContainerAddress, ContainerId, CopyRotation, EnvironmentValues, LiveServices, MachineId,
    MachineName, MachineObservation, MachineTarget, MembershipObservation, Namespace,
    QualifiedService, ReadRoutedAddressesRequest, RoutedAddresses, RoutesRead, RpcError,
    ServiceContainer, ServiceObservation, Unacked, op, serving_containers,
};
use ployz_store::{DeploymentStatus, DeploymentSummary};
use serde::Serialize;

use super::link::Attention;
use super::server::{self, Reach, Unreached};
use super::{Error, leaf_matches};
use crate::cloud_login::LoginError;
use crate::context::ContextError;

pub(super) const UNCHECKED: &str = "Could not check which Servers picked up the new copies";

const READS_AT_ONCE: usize = 8;

#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Subject {
    NotPickedUp {
        service: QualifiedService,
        /// Each such Machine by name, else by ID when the Machine list lacks it.
        machines: Vec<String>,
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
            let check = judge(observed);
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
        routed: BTreeMap<MachineId, RoutedAddresses>,
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
    let reads = async {
        match reader {
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
        }
    };
    let answered = live
        .containers
        .successes
        .iter()
        .map(|answered| answered.machine_id);
    let routed = stream::iter(answered)
        .map(|machine| async move {
            let routed = client
                .invoke::<op::ReadRoutedAddresses>(
                    ReadRoutedAddressesRequest {},
                    &MachineTarget::from(&machine),
                    Some(crate::connect::TARGET_RPC_TIMEOUT),
                )
                .await;
            (machine, routed)
        })
        .buffered(READS_AT_ONCE)
        .filter_map(|(machine, routed)| async move { Some((machine, routed.ok()?)) })
        .collect();
    let (reads, routed) = futures_util::join!(reads, routed);
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

fn judge(observed: Observed) -> Check {
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
    if !unanswered.is_empty() {
        check.unseen.push(Unseen::Unanswered(unanswered.clone()));
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
        let owners: Vec<(Ipv4Net, MachineId)> = machines
            .iter()
            .filter(|observed| {
                !unchecked.contains(&observed.machine.id)
                    && !unanswered.contains(&observed.machine.id)
            })
            .map(|observed| (observed.machine.subnet.into(), observed.machine.id))
            .collect();
        for (machine, addresses) in stale_addresses(&service, &routed, &unchecked, &owners) {
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
/// Machine lists counts only when its address falls in the subnet of a Machine that
/// answered and is up, since only that Machine could still report it.
fn stale_addresses(
    service: &ServiceObservation,
    routed: &BTreeMap<MachineId, RoutedAddresses>,
    down: &BTreeSet<MachineId>,
    owners: &[(Ipv4Net, MachineId)],
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
    let removed = |copy: &ContainerId, address: &ContainerAddress| {
        !listed.contains(copy)
            && !reported.values().any(|reported| reported == address)
            && owners.iter().any(|(subnet, _)| subnet.contains(&address.0))
    };
    routed
        .iter()
        .filter(|(machine, _)| !down.contains(machine))
        .filter_map(|(machine, routed)| {
            let stale: BTreeSet<ContainerAddress> = routed
                .routes
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

impl Check {
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
                    "Not checking which Servers picked up the new copies while deployment {deployment} is in progress."
                )),
                Unseen::Unlisted(error) => crate::ui::warn_cause(UNCHECKED, error),
                Unseen::Unanswered(machines) => crate::ui::warn(format!(
                    "{} did not answer, so status may miss Servers that have not picked up the new copies.",
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
                Unseen::Unread { services, error } => crate::ui::warn_cause(
                    format_args!("{UNCHECKED} of {}", service_names(services)),
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
mod tests {
    use std::net::Ipv4Addr;

    use ployz_core::{
        ContainerAddress, ContainerObservation, DeploymentLogId, Machine, MachineFailure,
        PartialResult, RpcErrorCode, service_containers,
    };
    use serde_json::json;

    use super::*;
    use crate::global_fixtures::{container_id, machine, qualified, running_on, service_id};

    fn machines() -> [Machine; 3] {
        [
            machine('a', "machine-1"),
            machine('b', "machine-2"),
            machine('c', "machine-3"),
        ]
    }

    fn rotated_on(machine: &Machine, hex: char, rotation: CopyRotation) -> ContainerObservation {
        let mut copy = running_on(machine, hex);
        copy.try_update(|parts| {
            parts.rotation = Some(rotation);
            parts.address = Some(ContainerAddress(Ipv4Addr::new(10, 210, 0, 1)));
        })
        .unwrap();
        copy
    }

    fn routed_on(machine: &Machine, hex: char) -> ContainerObservation {
        rotated_on(machine, hex, CopyRotation::In)
    }

    fn held() -> CopyRotation {
        CopyRotation::Held(DeploymentLogId::parse("run").unwrap())
    }

    fn service(name: &str, id: char, copies: [ContainerObservation; 2]) -> ServiceObservation {
        ServiceObservation {
            identity: qualified("app", name),
            service_id: service_id(id),
            containers: service_containers(copies),
            hook_containers: Vec::new(),
        }
    }

    fn web(copies: [ContainerObservation; 2]) -> ServiceObservation {
        service("web", 'a', copies)
    }

    fn routes_as_ployzd_answers(acked: &[(&Machine, &[char])], down: &[&Machine]) -> RoutesRead {
        RoutesRead {
            routes: acked
                .iter()
                .map(|(machine, copies)| {
                    let copies = copies.iter().copied().map(container_id).collect();
                    (machine.id, Some(copies))
                })
                .collect(),
            complete: true,
            down: down.iter().map(|machine| machine.id).collect(),
        }
    }

    fn listed(up: &[&Machine], down: &[&Machine]) -> Vec<MachineObservation> {
        let up = up
            .iter()
            .map(|machine| (machine, MembershipObservation::Up));
        let down = down
            .iter()
            .map(|machine| (machine, MembershipObservation::Down));
        up.chain(down)
            .map(|(machine, membership)| MachineObservation::new((*machine).clone(), membership))
            .collect()
    }

    fn unavailable(message: &str) -> RpcError {
        RpcError {
            code: RpcErrorCode::Unavailable,
            message: message.to_owned(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }
    }

    fn listing(failed: &[&Machine]) -> LiveServices<RpcError> {
        LiveServices {
            containers: PartialResult {
                successes: Vec::new(),
                failures: failed
                    .iter()
                    .map(|machine| MachineFailure {
                        machine_id: machine.id,
                        error: unavailable("connection refused"),
                    })
                    .collect(),
                omissions: Vec::new(),
            },
        }
    }

    fn checked(
        machines: Vec<MachineObservation>,
        reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
    ) -> Check {
        checked_routing(machines, reads, BTreeMap::new())
    }

    fn checked_routing(
        machines: Vec<MachineObservation>,
        reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
        routed: BTreeMap<MachineId, RoutedAddresses>,
    ) -> Check {
        judge(Observed {
            machines,
            copies: Copies::Listed {
                live: listing(&[]),
                reads,
                routed,
            },
        })
    }

    fn web_routed_at(copies: &[(char, [u8; 4])]) -> RoutedAddresses {
        RoutedAddresses {
            routes: BTreeMap::from([(
                qualified("app", "web"),
                copies
                    .iter()
                    .map(|(hex, address)| {
                        (
                            container_id(*hex),
                            ContainerAddress(Ipv4Addr::from(*address)),
                        )
                    })
                    .collect(),
            )]),
        }
    }

    fn warned(check: &Check) -> Vec<String> {
        check.warn();
        let (_, json) = crate::ui::captured(|| crate::ui::show(&json!({})));
        json.and_then(|json| json.get("warnings").cloned())
            .map(|warnings| serde_json::from_value(warnings).unwrap())
            .unwrap_or_default()
    }

    #[test]
    fn status_names_no_machine_when_every_proxy_acks_the_new_copies() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&two, '2')]),
                Ok(routes_as_ployzd_answers(
                    &[
                        (&one, &['1', '2']),
                        (&two, &['1', '2']),
                        (&three, &['1', '2']),
                    ],
                    &[],
                )),
            )],
        );

        assert!(check.attention.is_empty());
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_names_a_live_machine_whose_proxy_lacks_a_new_copy() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&two, '2')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1', '2']), (&two, &['1', '2']), (&three, &['0'])],
                    &[],
                )),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_not_acked",
                "message": "machine-3 has not picked up the new copies. Check its network.",
                "service": "app/web",
                "machines": ["machine-3"],
            }])
        );
        assert_eq!(
            super::super::link::rendered(check.attention),
            "organization  ?\nsigned in as  local\nweb           machine-3 has not picked up the new copies. Check its network.\n"
        );
    }

    #[test]
    fn status_names_a_machine_whose_proxy_routes_a_copy_at_an_address_its_owner_does_not_report() {
        let [one, two, three] = machines();
        let check = checked_routing(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&two, '2')]),
                Ok(routes_as_ployzd_answers(
                    &[
                        (&one, &['1', '2']),
                        (&two, &['1', '2']),
                        (&three, &['1', '2']),
                    ],
                    &[],
                )),
            )],
            BTreeMap::from([
                (
                    one.id,
                    web_routed_at(&[('1', [10, 210, 0, 1]), ('2', [10, 210, 0, 1])]),
                ),
                (
                    three.id,
                    web_routed_at(&[('1', [10, 210, 2, 2]), ('2', [10, 210, 0, 1])]),
                ),
            ]),
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_stale_address",
                "message": "machine-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.",
                "service": "app/web",
                "machine": "machine-3",
                "addresses": ["10.210.2.2"],
            }])
        );
        assert_eq!(
            super::super::link::rendered(check.attention),
            "organization  ?\nsigned in as  local\nweb           machine-3 routes a copy to 10.210.2.2, which its owner does not report. Check its network.\n"
        );
    }

    fn routing_a_removed_copy(
        listed_machines: Vec<MachineObservation>,
        unanswered: &[&Machine],
        removed_at: [u8; 4],
    ) -> Check {
        let [one, two, three] = machines();
        judge(Observed {
            machines: listed_machines,
            copies: Copies::Listed {
                live: listing(unanswered),
                reads: vec![(
                    web([routed_on(&one, '1'), routed_on(&three, '3')]),
                    Ok(routes_as_ployzd_answers(
                        &[
                            (&one, &['1', '3']),
                            (&two, &['1', '3']),
                            (&three, &['1', '3']),
                        ],
                        &[],
                    )),
                )],
                routed: BTreeMap::from([(
                    two.id,
                    web_routed_at(&[('1', [10, 210, 0, 1]), ('0', removed_at)]),
                )]),
            },
        })
    }

    #[test]
    fn status_names_a_machine_whose_proxy_routes_a_copy_its_owner_removed() {
        let [one, two, three] = machines();
        let check =
            routing_a_removed_copy(listed(&[&one, &two, &three], &[]), &[], [10, 210, 12, 5]);

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_stale_address",
                "message": "machine-2 routes a copy to 10.210.12.5, which its owner does not report. Check its network.",
                "service": "app/web",
                "machine": "machine-2",
                "addresses": ["10.210.12.5"],
            }])
        );
    }

    #[test]
    fn status_names_no_machine_for_a_removed_copy_whose_owner_did_not_answer() {
        let [one, two, three] = machines();
        let check = routing_a_removed_copy(
            listed(&[&one, &two, &three], &[]),
            &[&three],
            [10, 210, 12, 5],
        );

        assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
    }

    #[test]
    fn status_names_no_machine_for_a_removed_copy_whose_owner_is_down() {
        let [one, two, three] = machines();
        let check = routing_a_removed_copy(listed(&[&one, &two], &[&three]), &[], [10, 210, 12, 5]);

        assert!(
            check
                .attention
                .iter()
                .all(|attention| attention.reason == "machine_down"),
            "{}",
            serde_json::to_value(&check.attention).unwrap()
        );
    }

    #[test]
    fn status_names_no_machine_for_a_removed_copy_at_an_address_no_machine_owns() {
        let [one, two, three] = machines();
        let check =
            routing_a_removed_copy(listed(&[&one, &two, &three], &[]), &[], [10, 210, 99, 5]);

        assert_eq!(serde_json::to_value(&check.attention).unwrap(), json!([]));
    }

    fn checked_with_an_old_copy(rotation: CopyRotation) -> Check {
        let [one, two, three] = machines();
        checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), rotated_on(&two, '0', rotation)]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1']), (&three, &['1', '0'])],
                    &[],
                )),
            )],
        )
    }

    #[test]
    fn status_names_a_live_machine_whose_proxy_still_routes_an_out_copy() {
        let check = checked_with_an_old_copy(CopyRotation::Out);

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_not_acked",
                "message": "machine-3 has not picked up the new copies. Check its network.",
                "service": "app/web",
                "machines": ["machine-3"],
            }])
        );
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_names_a_live_machine_whose_proxy_still_routes_a_held_copy() {
        let check = checked_with_an_old_copy(held());

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_not_acked",
                "message": "machine-3 has not picked up the new copies. Check its network.",
                "service": "app/web",
                "machines": ["machine-3"],
            }])
        );
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_names_once_each_machine_that_lacks_a_new_copy_or_routes_an_old_one() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([
                    routed_on(&one, '1'),
                    rotated_on(&two, '0', CopyRotation::Out),
                ]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['0']), (&three, &['1', '0'])],
                    &[],
                )),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "routes_not_acked",
                "message": "machine-2, machine-3 have not picked up the new copies. Check their network.",
                "service": "app/web",
                "machines": ["machine-2", "machine-3"],
            }])
        );
    }

    #[test]
    fn status_names_no_machine_for_routing_an_out_copy_on_a_down_machine() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &three], &[&two]),
            vec![(
                web([
                    routed_on(&one, '1'),
                    rotated_on(&two, '0', CopyRotation::Out),
                ]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&three, &['1', '0'])],
                    &[],
                )),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-2",
            }])
        );
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn a_proxy_keeping_an_out_copy_while_every_in_copy_is_down_is_not_named() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &three], &[&two]),
            vec![(
                web([
                    routed_on(&two, '1'),
                    rotated_on(&one, '0', CopyRotation::Out),
                ]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['0']), (&three, &['0'])],
                    &[],
                )),
            )],
        );

        assert_eq!(check.attention.len(), 1, "only machine-2's Down row");
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_names_no_machine_when_every_proxy_routes_only_the_new_copy() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([
                    routed_on(&one, '1'),
                    rotated_on(&two, '0', CopyRotation::Out),
                ]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1']), (&three, &['1'])],
                    &[],
                )),
            )],
        );

        assert!(check.attention.is_empty());
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_shows_a_machine_down_on_the_entry_and_the_reader_as_down_only() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two], &[&three]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1'])],
                    &[&three],
                )),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-3",
            }])
        );
        assert!(warned(&check).is_empty());
        assert_eq!(
            super::super::link::rendered(check.attention),
            "organization  ?\nsigned in as  local\nmachine-3     Down. Its copies serve again once it's back.\n"
        );
    }

    #[test]
    fn status_shows_a_machine_down_only_on_the_entry_as_down_and_never_as_not_picking_up() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two], &[&three]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1']), (&three, &[])],
                    &[],
                )),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-3",
            }])
        );
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_warns_it_skips_a_machine_only_the_reader_calls_down() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1'])],
                    &[&three],
                )),
            )],
        );

        assert!(check.attention.is_empty(), "no routes_not_acked row");
        assert_eq!(
            warned(&check),
            [
                "The Server status asked sees machine-3 Down, so status is not checking what it routes or who routes its copies of web."
            ]
        );
    }

    #[test]
    fn status_names_no_stale_address_for_a_machine_only_the_reader_calls_down() {
        let [one, two, three] = machines();
        let check = checked_routing(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1']), (&two, &['1'])],
                    &[&three],
                )),
            )],
            BTreeMap::from([
                (
                    two.id,
                    web_routed_at(&[('1', [10, 210, 0, 1]), ('3', [10, 210, 2, 2])]),
                ),
                (
                    three.id,
                    web_routed_at(&[('1', [10, 210, 2, 2]), ('3', [10, 210, 0, 1])]),
                ),
            ]),
        );

        assert!(
            check.attention.is_empty(),
            "no routes_stale_address row for what machine-3 routes or for its copies"
        );
        assert_eq!(
            warned(&check),
            [
                "The Server status asked sees machine-3 Down, so status is not checking what it routes or who routes its copies of web."
            ]
        );
    }

    #[test]
    fn a_service_whose_copies_are_all_on_down_machines_warns_nothing() {
        let [one, two, three] = machines();
        let mut read = routes_as_ployzd_answers(&[(&one, &[])], &[&two, &three]);
        read.complete = false;
        let check = checked(
            listed(&[&one, &two], &[&three]),
            vec![(
                web([routed_on(&three, '3'), routed_on(&three, '4')]),
                Ok(read),
            )],
        );

        assert_eq!(check.attention.len(), 1, "only machine-3's Down row");
        assert!(warned(&check).is_empty());
    }

    #[test]
    fn status_shows_a_down_machine_when_no_service_has_copies() {
        let [one, two, three] = machines();

        let check = checked(listed(&[&one, &two], &[&three]), Vec::new());

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-3",
            }])
        );
    }

    #[test]
    fn status_says_the_check_is_partial_when_a_live_machine_did_not_list_its_copies() {
        let [one, two, three] = machines();
        let check = judge(Observed {
            machines: listed(&[&one, &two], &[&three]),
            copies: Copies::Listed {
                live: listing(&[&two, &three]),
                reads: Vec::new(),
                routed: BTreeMap::new(),
            },
        });

        assert_eq!(check.attention.len(), 1, "only machine-3's Down row");
        assert_eq!(
            warned(&check),
            [
                "machine-2 did not answer, so status may miss Servers that have not picked up the new copies."
            ]
        );
    }

    #[test]
    fn a_failed_routes_read_warns_for_its_service_and_keeps_the_down_row() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two], &[&three]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&two, '2')]),
                Err(unavailable("store not ready")),
            )],
        );

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-3",
            }])
        );
        assert_eq!(
            warned(&check),
            ["Could not check which Servers picked up the new copies of web: store not ready"]
        );
    }

    #[test]
    fn a_read_that_lacks_the_full_machine_list_is_never_an_all_clear() {
        let [one, two, three] = machines();
        let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
        read.complete = false;
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(web([routed_on(&one, '1'), routed_on(&two, '2')]), Ok(read))],
        );

        assert!(check.attention.is_empty());
        assert_eq!(
            warned(&check),
            ["Could not check whether every Server picked up the new copies of web."]
        );
    }

    #[test]
    fn a_machine_whose_routes_row_has_not_arrived_is_unreported_not_stale() {
        let [one, two, three] = machines();
        let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
        read.routes.insert(three.id, None);
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(web([routed_on(&one, '1'), routed_on(&two, '2')]), Ok(read))],
        );

        assert!(check.attention.is_empty(), "no routes_not_acked row");
        assert_eq!(
            warned(&check),
            ["machine-3 has not reported which copies it routes."]
        );
    }

    #[test]
    fn a_read_that_lacks_the_full_machine_list_warns_once_for_every_service() {
        let [one, two, three] = machines();
        let mut read = routes_as_ployzd_answers(&[(&one, &['1', '2']), (&two, &['1', '2'])], &[]);
        read.complete = false;
        let copies = || [routed_on(&one, '1'), routed_on(&two, '2')];
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![
                (service("web", 'a', copies()), Ok(read.clone())),
                (service("api", 'b', copies()), Ok(read.clone())),
                (service("worker", 'c', copies()), Ok(read)),
            ],
        );

        assert_eq!(
            warned(&check),
            ["Could not check whether every Server picked up the new copies of web, api, worker."]
        );
    }

    #[test]
    fn the_same_read_error_for_several_services_warns_once() {
        let [one, two, three] = machines();
        let copies = || [routed_on(&one, '1'), routed_on(&two, '2')];
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![
                (
                    service("web", 'a', copies()),
                    Err(unavailable("store not ready")),
                ),
                (
                    service("api", 'b', copies()),
                    Err(unavailable("store not ready")),
                ),
            ],
        );

        assert_eq!(
            warned(&check),
            ["Could not check which Servers picked up the new copies of web, api: store not ready"]
        );
    }

    fn deployment(number: u64, status: &str) -> DeploymentSummary {
        serde_json::from_value(json!({
            "id": "00000000-0000-4000-8000-000000000101",
            "environment_id": "00000000-0000-4000-8000-000000000002",
            "number": number, "status": status, "saved": 1, "services": [],
            "runner": null, "upload": null, "remove": false, "admitted_by": null,
            "admitted_at": 0, "started_at": null, "ended_at": null, "message": null,
            "push": null, "ci": null, "in_flight": true, "outcome": null,
        }))
        .unwrap()
    }

    #[test]
    fn a_queued_deployment_does_not_turn_the_check_off() {
        assert_eq!(rolling_out(&[deployment(12, "queued")]), None);
    }

    #[test]
    fn while_a_deployment_rolls_out_status_says_it_skips_the_check() {
        let [one, two, three] = machines();
        let deploying = [deployment(13, "queued"), deployment(12, "running")];
        let check = judge(Observed {
            machines: listed(&[&one, &two], &[&three]),
            copies: Copies::RollingOut(rolling_out(&deploying).unwrap()),
        });

        assert_eq!(
            serde_json::to_value(&check.attention).unwrap(),
            json!([{
                "reason": "machine_down",
                "message": "Down. Its copies serve again once it's back.",
                "machine": "machine-3",
            }])
        );
        assert_eq!(
            warned(&check),
            [
                "Not checking which Servers picked up the new copies while deployment #12 is in progress."
            ]
        );
    }

    fn status_warnings(config: &std::path::Path, flags: &[&str]) -> Vec<String> {
        let mut args = vec![
            "ployz",
            "status",
            "--ployz-config",
            config.to_str().unwrap(),
        ];
        args.extend(flags);
        let root = crate::cli::command().try_get_matches_from(args).unwrap();
        let namespace = qualified("app", "web").namespace;
        assert!(attention(&root, &namespace, &[]).is_empty());
        let (_, json) = crate::ui::captured(|| crate::ui::show(&json!({})));
        json.and_then(|json| json.get("warnings").cloned())
            .map(|warnings| serde_json::from_value(warnings).unwrap())
            .unwrap_or_default()
    }

    #[test]
    fn status_warns_when_the_context_it_was_given_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(&config, "contexts:\n  prod:\n    connections: []\n").unwrap();

        let warnings = status_warnings(&config, &["--context", "typo"]);

        let [warning] = warnings.as_slice() else {
            panic!("one warning: {warnings:?}");
        };
        assert!(
            warning.starts_with(&format!("{UNCHECKED}: context typo not found")),
            "{warning}"
        );
    }

    #[test]
    fn status_checks_nothing_and_says_nothing_with_no_server_set_up() {
        if std::path::Path::new(crate::connect::DEFAULT_LOCAL_SOCKET).exists()
            || std::env::var_os(crate::cli::env::TOKEN).is_some()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();

        assert!(status_warnings(&dir.path().join("config.yaml"), &[]).is_empty());
    }
}
