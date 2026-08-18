use std::io::{self, IsTerminal, Write};

use clap::ArgMatches;
use ployz_core::{
    DataLoss, DescribeContractRequest, LiveServices, Machine, MachineId, MachineName,
    MachineTarget, NameMatches, ObservedDataLoss, RemoveMachineRequest, RpcError, ServiceName,
    UnconfirmedDataLoss, op,
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
                .await?;
            Some(collect_data_loss_confirmation(&observed, &named)?)
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
                    .map_err(refusal_from_rpc)?;
                if let Some(warning) = removed.reset_warning {
                    eprintln!("WARNING: target cleanup/reset failed: {warning}");
                }
            }
        }

        // Drop the local connection before DNS refresh. A refresh failure
        // must not leave the removed Machine named in the context (#249).
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
            return Err(Error::warned(
                "hosted DNS refresh failed after removing the Machine",
                error,
            ));
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

fn collect_data_loss_confirmation(
    observed: &ObservedDataLoss,
    named: &[String],
) -> Result<Vec<DataLoss>, Error> {
    if !observed.data_loss.is_empty() {
        eprintln!("Data Loss:");
        for loss in &observed.data_loss {
            eprintln!("  {}", loss.name());
        }
    }
    let prompted;
    let names = if named.is_empty() && !observed.data_loss.is_empty() {
        prompted = read_data_loss_names(observed)?;
        prompted.as_slice()
    } else {
        named
    };
    let confirmation = observed.named(names.iter().map(String::as_str))?;
    let missing = observed.uncovered_by(&confirmation);
    if missing.is_empty() {
        Ok(confirmation)
    } else {
        Err(Error::usage(pass_data_loss_names_message(&missing)))
    }
}

fn read_data_loss_names(observed: &ObservedDataLoss) -> Result<Vec<String>, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(pass_data_loss_names_message(
            &observed.data_loss,
        )));
    }
    print!("Name the Data Loss to continue: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.split_whitespace().map(str::to_owned).collect())
}

fn pass_data_loss_names_message(missing: &[DataLoss]) -> String {
    format!(
        "Data Loss is not covered by the confirmation; pass the names as arguments: {}",
        missing
            .iter()
            .map(DataLoss::name)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn refusal_from_rpc(error: RpcError) -> Error {
    match UnconfirmedDataLoss::from_rpc_error(&error) {
        Some(unconfirmed) => Error::usage(pass_data_loss_names_message(&unconfirmed.missing)),
        None => error.into(),
    }
}

#[must_use]
fn services_on(machine_id: &MachineId, live: &LiveServices<RpcError>) -> Vec<ServiceName> {
    live.services()
        .into_iter()
        .filter(|service| {
            service
                .containers
                .iter()
                .any(|container| container.as_observation().machine_id == *machine_id)
        })
        .filter_map(|service| service.service_name().cloned())
        .collect()
}

#[must_use]
fn service_warnings(machine: &MachineName, services: &[ServiceName]) -> Vec<String> {
    if services.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "WARNING: Machine {machine} is running Services: {}",
        services
            .iter()
            .map(ServiceName::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )]
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, DataLoss, DockerVolumeId,
        DockerVolumeName, HealthObservation, LiveServices, MachineId, MachineName, MachineSuccess,
        ObservedDataLoss, PartialResult, RpcError, ServiceId, ServiceName, UnconfirmedDataLoss,
        derive_live_services,
    };
    use serde_json::json;

    use super::{
        collect_data_loss_confirmation, pass_data_loss_names_message, refusal_from_rpc,
        service_warnings, services_on,
    };

    #[test]
    fn refusing_without_a_confirmation_states_which_names_to_pass() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('a', "logs")],
        };
        let error = collect_data_loss_confirmation(&observed, &["gone".into()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Data Loss is not covered by the confirmation; pass the names as arguments: data logs"
        );
    }

    #[test]
    fn typed_names_that_match_more_than_one_listed_entry_are_refused() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('b', "data")],
        };
        let error = collect_data_loss_confirmation(&observed, &["data".into()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Data Loss name \"data\" matches more than one listed entry"
        );
    }

    #[test]
    fn a_machine_with_no_data_loss_needs_no_names() {
        let observed = ObservedDataLoss {
            data_loss: Vec::new(),
        };
        assert_eq!(
            collect_data_loss_confirmation(&observed, &[]).unwrap(),
            Vec::<DataLoss>::new()
        );
    }

    #[test]
    fn execute_time_unconfirmed_data_loss_states_which_names_to_pass() {
        let missing = vec![loss('a', "logs")];
        let error = UnconfirmedDataLoss {
            missing: missing.clone(),
        }
        .into_rpc_error();
        assert_eq!(
            refusal_from_rpc(error).to_string(),
            pass_data_loss_names_message(&missing)
        );
    }

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
                    ServiceName::parse("api").unwrap(),
                    ServiceName::parse("web").unwrap(),
                ],
            ),
            vec!["WARNING: Machine ams1 is running Services: api, web".to_owned()]
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
            [ServiceName::parse("api").unwrap()]
        );
    }

    fn loss(machine: char, name: &str) -> DataLoss {
        DataLoss::DockerVolume(DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        })
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
