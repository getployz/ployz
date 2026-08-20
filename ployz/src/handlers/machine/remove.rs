use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, LiveServices, Machine, MachineId, MachineName, MachineTarget,
    NameMatches, QualifiedService, RemoveMachineRequest, RpcError, RpcErrorCode, op,
};

use super::super::{connect_client, runtime, string_values};
use super::{ConnectionOptions, helpers, machine_list, target};
use crate::handlers::{Error, leaf_matches};

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let yes = matches.get_flag("yes");
    let named = string_values(matches, "data-loss");
    runtime()?.block_on(async {
        let mut client = connect_client(matches, options.context()).await?;
        let machines = machine_list(&mut client).await?;
        let selected = select_machine(&machines, &selector)?;
        let selected_target = MachineTarget::from(&selected.id);
        let current = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await?
            .machine_id;
        if selected.id == current && machines.len() > 1 {
            return Err(Error::usage(
                "the current entry Machine cannot be removed while another Machine is visible",
            ));
        }
        let selected_observation = machines
            .iter()
            .find(|entry| entry.machine.id == selected.id)
            .expect("selected Machine came from this list");
        let confirmation = if no_reset {
            None
        } else {
            let observed = client
                .data_loss_if_machine_removed(&selected_target)
                .await
                .map_err(machine_removal_refusal)?;
            Some(super::super::data_loss::collect_data_loss_confirmation(
                &observed, &named,
            )?)
        };
        let services = services_on(
            &selected.id,
            &client
                .live_services_from(std::slice::from_ref(selected_observation))
                .await?,
        );
        for line in service_warnings(&selected.name, &services) {
            eprintln!("{line}");
        }
        helpers::confirm(
            yes,
            &format!("Remove Machine {} ({})?", selected.name, selected.id),
        )?;

        // TODO(UT-055): do not reroute away from the current entry before removal.
        // TODO(UT-056): there is no drain or unschedulable phase before cleanup.
        match confirmation {
            None => {
                client
                    .call::<op::RemoveMachine>(
                        RemoveMachineRequest {
                            machine_id: selected.id,
                        },
                        None,
                    )
                    .await?;
            }
            Some(confirmation) => {
                let removed = client
                    .remove_machine(&selected_target, &confirmation)
                    .await
                    .map_err(crate::failure::refusal_from_rpc)?;
                if let Some(warning) = removed.reset_warning {
                    eprintln!("WARNING: target cleanup/reset failed: {warning}");
                }
            }
        }

        // Drop the local connection before DNS refresh. A refresh failure
        // must not leave the removed Machine named in the context (#249)
        // and must not fail the command (#449).
        let mut config = options.load_or_empty_config()?;
        if let Some(context_name) = config.context_name(options.context()).map(str::to_owned)
            && let Some(context) = config.contexts.get_mut(&context_name)
        {
            context.drop_machine(&selected.id);
            config.save()?;
        }
        if let Err(error) =
            crate::dns::update_records_after_removal(&mut client, machines, &selected.id).await
        {
            eprintln!(
                "{}",
                Error::warned(
                    "hosted DNS refresh failed after removing the Machine",
                    error,
                )
            );
        }
        println!("Removed Machine {} ({})", selected.name, selected.id);
        Ok::<_, Error>(())
    })
}

fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineTarget::parse(selector)?;
    match selector.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(Error::usage(format!("Machine {selector:?} was not found"))),
        NameMatches::One(machine) => Ok(machine.clone()),
        NameMatches::Ambiguous(matches) => Err(Error::usage(format!(
            "Machine name {selector:?} is ambiguous: {}",
            matches
                .into_iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn machine_removal_refusal(error: RpcError) -> Error {
    if error.code == RpcErrorCode::Unavailable {
        Error::usage(format!(
            "{error}; use --no-reset to remove it from the Cluster without resetting"
        ))
    } else {
        error.into()
    }
}

#[must_use]
fn services_on(machine_id: &MachineId, live: &LiveServices<RpcError>) -> Vec<QualifiedService> {
    live.services()
        .into_iter()
        .filter(|service| {
            service
                .containers
                .iter()
                .any(|container| container.as_observation().machine_id == *machine_id)
        })
        .map(|service| service.identity)
        .collect()
}

#[must_use]
fn service_warnings(machine: &MachineName, services: &[QualifiedService]) -> Vec<String> {
    if services.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "WARNING: Machine {machine} is running Services: {}",
        services
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )]
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
        LiveServices, MachineId, MachineName, MachineSuccess, PartialResult, QualifiedService,
        RpcError, RpcErrorCode, ServiceId, ServiceName, derive_live_services,
    };
    use serde_json::{Value, json};

    use super::{machine_removal_refusal, service_warnings, services_on};

    #[test]
    fn service_warnings_are_silent_when_nothing_is_at_stake() {
        assert_eq!(
            service_warnings(&MachineName::parse("ams1").unwrap(), &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn service_warnings_name_services_on_the_machine() {
        assert_eq!(
            service_warnings(
                &MachineName::parse("ams1").unwrap(),
                &[
                    QualifiedService::parse("app/api").unwrap(),
                    QualifiedService::parse("app/web").unwrap(),
                ],
            ),
            vec!["WARNING: Machine ams1 is running Services: app/api, app/web".to_owned()]
        );
    }

    #[test]
    fn unreachable_removal_names_no_reset() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Machine aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa did not respond".into(),
            details: Value::Null,
        };
        assert_eq!(
            machine_removal_refusal(error).to_string(),
            "Machine aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa did not respond; use --no-reset to remove it from the Cluster without resetting"
        );
    }

    #[test]
    fn other_data_loss_errors_keep_their_message() {
        let error = RpcError {
            code: RpcErrorCode::NotFound,
            message: "Machine \"gone\" was not found".into(),
            details: Value::Null,
        };
        assert_eq!(
            machine_removal_refusal(error).to_string(),
            "Machine \"gone\" was not found"
        );
    }

    #[test]
    fn services_on_names_services_with_a_service_container_on_that_machine() {
        let live: LiveServices<RpcError> = derive_live_services(PartialResult {
            successes: vec![
                MachineSuccess {
                    machine_id: machine_id('a'),
                    value: vec![
                        observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        observation('2', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                        observation('3', 'a', "hooked", ContainerKind::PreDeployHook, 'b'),
                    ],
                },
                MachineSuccess {
                    machine_id: machine_id('b'),
                    value: vec![observation(
                        '4',
                        'b',
                        "web",
                        ContainerKind::ServiceContainer,
                        'c',
                    )],
                },
            ],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            services_on(&machine_id('a'), &live),
            [QualifiedService::parse("app/api").unwrap()]
        );
    }

    fn observation(
        id: char,
        machine: char,
        name: &str,
        kind: ContainerKind,
        service: char,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(service.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        ContainerObservation {
            container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: name.into(),
            created_at_unix_nanos: 0,
            machine_id: machine_id(machine),
            project_name: ployz_core::ProjectName::parse("app").unwrap(),
            service_id,
            service_name: service_name.clone(),
            kind,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: serde_json::from_value(json!({
                "service_id": service_id,
                "name": service_name,
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
            }))
            .unwrap(),
            address: None,
            labels: Default::default(),
        }
    }

    fn machine_id(value: char) -> MachineId {
        MachineId::parse(value.to_string().repeat(32)).unwrap()
    }
}
