//! The Server check of `ployz status`: each Down Machine, and each live Machine whose
//! proxy has not picked up a Service's In copies. What the check could not see is a
//! warning, never an all-clear.

use std::collections::{BTreeSet, HashMap};

use clap::ArgMatches;
use futures_util::future::join_all;
use ployz_core::{
    ContainerId, EnvironmentValues, LiveServices, MachineId, MachineName, MachineObservation,
    MembershipObservation, Namespace, QualifiedService, RoutesRead, RpcError, ServiceObservation,
    serving_containers,
};
use serde::Serialize;

use super::link::Attention;
use super::server::{self, Reach};
use super::{Error, leaf_matches};
use crate::cloud_login::LoginError;
use crate::context::ContextError;

pub(super) const UNCHECKED: &str = "Could not check which Servers picked up the new copies";

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
}

pub(super) fn attention(
    root: &ArgMatches,
    namespace: &Namespace,
    rolling_out: bool,
) -> Vec<Attention> {
    let leaf = leaf_matches(root);
    let context = leaf.get_one::<String>("context").map(String::as_str);
    let named = context.is_some() || leaf.get_one::<String>("connect").is_some();
    let observed = super::runtime().and_then(|runtime| {
        runtime.block_on(async {
            match server::reach(leaf, context).await? {
                Some(Reach::Dial(mut client)) => {
                    observe(&mut client, namespace, rolling_out).await.map(Some)
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
        Err(error) if !named && nothing_set_up(&error) => Vec::new(),
        Err(error) => {
            crate::ui::warn_cause(UNCHECKED, &error);
            Vec::new()
        }
    }
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
    RollingOut,
    Unlisted(Error),
    Listed {
        live: LiveServices<RpcError>,
        /// Each Service of the namespace that has serving copies, with its routes as
        /// one Server reads them.
        reads: Vec<(ServiceObservation, Result<RoutesRead, RpcError>)>,
    },
}

async fn observe(
    client: &mut crate::connect::Client,
    namespace: &Namespace,
    rolling_out: bool,
) -> Result<Observed, Error> {
    let machines = client.machines().await?;
    if rolling_out {
        return Ok(Observed {
            machines,
            copies: Copies::RollingOut,
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
    let reads = match reader {
        Some(reader) => {
            join_all(services.map(|service| async move {
                let read = crate::deploy::MachineOperations::read_routes(
                    client,
                    &reader,
                    &service.identity,
                )
                .await;
                (service, read)
            }))
            .await
        }
        None => Vec::new(),
    };
    Ok(Observed {
        machines,
        copies: Copies::Listed { live, reads },
    })
}

struct Check {
    attention: Vec<Attention>,
    unseen: Vec<Unseen>,
    names: HashMap<MachineId, MachineName>,
}

enum Unseen {
    Unlisted(Error),
    Unanswered(Vec<MachineId>),
    Unread {
        service: QualifiedService,
        error: RpcError,
    },
    /// Live Machines the routes read did not judge; `None` when the Server read did not
    /// know every Machine yet.
    Unjudged {
        service: QualifiedService,
        machines: Option<Vec<MachineId>>,
    },
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
    let (live, reads) = match copies {
        Copies::RollingOut => return check,
        Copies::Unlisted(error) => {
            check.unseen.push(Unseen::Unlisted(error));
            return check;
        }
        Copies::Listed { live, reads } => (live, reads),
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
        check.unseen.push(Unseen::Unanswered(unanswered));
    }
    for (service, read) in reads {
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                check.unseen.push(Unseen::Unread {
                    service: service.identity,
                    error,
                });
                continue;
            }
        };
        let serving: BTreeSet<ContainerId> = serving_containers(&service.containers)
            .iter()
            .map(|serving| serving.as_observation())
            .filter(|copy| !down.contains(&copy.machine_id))
            .map(|copy| copy.container_id)
            .collect();
        if serving.is_empty() {
            continue;
        }
        let waiting: Vec<String> = read
            .unacked(&down, |routed| serving.is_subset(routed))
            .map(|(machine, _)| check.name(&machine))
            .collect();
        let missing: Vec<MachineId> = machines
            .iter()
            .map(|observed| observed.machine.id)
            .filter(|machine| !down.contains(machine) && !read.routes.contains_key(machine))
            .collect();
        if !read.complete || !missing.is_empty() {
            check.unseen.push(Unseen::Unjudged {
                service: service.identity.clone(),
                machines: read.complete.then_some(missing),
            });
        }
        if !waiting.is_empty() {
            check
                .attention
                .push(not_picked_up(service.identity, waiting));
        }
    }
    check
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

impl Check {
    fn name(&self, machine: &MachineId) -> String {
        self.names
            .get(machine)
            .map_or_else(|| machine.to_string(), ToString::to_string)
    }

    fn names(&self, machines: &[MachineId]) -> String {
        machines
            .iter()
            .map(|machine| self.name(machine))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn warn(&self) {
        for unseen in &self.unseen {
            match unseen {
                Unseen::Unlisted(error) => crate::ui::warn_cause(UNCHECKED, error),
                Unseen::Unanswered(machines) => crate::ui::warn(format!(
                    "{} did not answer, so status may miss Servers that have not picked up the new copies.",
                    self.names(machines)
                )),
                Unseen::Unread { service, error } => {
                    crate::ui::warn_cause(format_args!("{UNCHECKED} of {}", service.name), error)
                }
                Unseen::Unjudged { service, machines } => crate::ui::warn(format!(
                    "Could not check whether {} picked up the new copies of {}.",
                    machines.as_deref().map_or_else(
                        || "every Server".to_owned(),
                        |machines| self.names(machines)
                    ),
                    service.name
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use ployz_core::{
        ContainerAddress, ContainerObservation, CopyRotation, Machine, MachineFailure,
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

    fn routed_on(machine: &Machine, hex: char) -> ContainerObservation {
        let mut copy = running_on(machine, hex);
        copy.try_update(|parts| {
            parts.rotation = Some(CopyRotation::In);
            parts.address = Some(ContainerAddress(Ipv4Addr::new(10, 210, 0, 1)));
        })
        .unwrap();
        copy
    }

    fn web(copies: [ContainerObservation; 2]) -> ServiceObservation {
        ServiceObservation {
            identity: qualified("app", "web"),
            service_id: service_id('a'),
            containers: service_containers(copies),
            hook_containers: Vec::new(),
        }
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
        judge(Observed {
            machines,
            copies: Copies::Listed {
                live: listing(&[]),
                reads,
            },
        })
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
    fn status_judges_a_machine_the_reader_calls_down_but_the_entry_does_not() {
        let [one, two, three] = machines();
        let check = checked(
            listed(&[&one, &two, &three], &[]),
            vec![(
                web([routed_on(&one, '1'), routed_on(&three, '3')]),
                Ok(routes_as_ployzd_answers(
                    &[(&one, &['1', '3']), (&two, &['1', '3'])],
                    &[&three],
                )),
            )],
        );

        assert!(check.attention.is_empty());
        assert_eq!(
            warned(&check),
            ["Could not check whether machine-3 picked up the new copies of web."]
        );
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
    fn while_a_deployment_rolls_out_only_down_machines_show() {
        let [one, two, three] = machines();
        let check = judge(Observed {
            machines: listed(&[&one, &two], &[&three]),
            copies: Copies::RollingOut,
        });

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
        assert!(attention(&root, &namespace, false).is_empty());
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
        let dir = tempfile::tempdir().unwrap();

        assert!(status_warnings(&dir.path().join("config.yaml"), &[]).is_empty());
    }
}
