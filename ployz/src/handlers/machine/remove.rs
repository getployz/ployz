use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, DockerVolume, DockerVolumeName, LiveServices, Machine, MachineId,
    MachineName, MachineTarget, NameMatches, PartialResult, RemoveLocalMachineRequest,
    RemoveMachineRequest, RpcError, ServiceName, op,
};

use super::super::{connect_client, runtime};
use super::{ConnectionOptions, helpers, machine_list, target};
use crate::handlers::{Error, leaf_matches};

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "machine")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let yes = matches.get_flag("yes");
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
        let volumes = if no_reset {
            Vec::new()
        } else {
            docker_volumes_on(
                &selected.id,
                &client
                    .list_volumes(std::slice::from_ref(selected_observation))
                    .await,
            )
        };
        let services = services_on(
            &selected.id,
            &client
                .live_services_from(std::slice::from_ref(selected_observation))
                .await?,
        );
        for line in removal_warnings(&selected.name, &volumes, &services) {
            eprintln!("{line}");
        }
        helpers::confirm(
            yes,
            &format!("Remove Machine {} ({})?", selected.name, selected.id),
        )?;

        // TODO(UT-055): do not reroute away from the current entry before removal.
        // TODO(UT-056): there is no drain or unschedulable phase before cleanup.
        let mut shared_rows_removed_by_entry = false;
        if !no_reset {
            match client
                .call::<op::RemoveLocalMachine>(
                    RemoveLocalMachineRequest {
                        restart_on_cleanup_failure: selected.id != current,
                    },
                    Some(&selected_target),
                )
                .await
            {
                Ok(removed) => {
                    if let Some(warning) = &removed.reset_warning {
                        eprintln!("WARNING: target cleanup/reset failed: {warning}");
                    } else if selected.id == current {
                        shared_rows_removed_by_entry = true;
                    }
                }
                Err(error) if error.is_unreachable() => {
                    eprintln!("WARNING: target is unreachable; removing shared rows: {error}");
                }
                Err(error) => return Err(error.into()),
            }
        }
        if !shared_rows_removed_by_entry {
            client
                .call::<op::RemoveMachine>(
                    RemoveMachineRequest {
                        machine_id: selected.id,
                    },
                    None,
                )
                .await?;
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

#[must_use]
fn docker_volumes_on(
    machine_id: &MachineId,
    listed: &PartialResult<Vec<DockerVolume>, RpcError>,
) -> Vec<DockerVolumeName> {
    listed
        .successes
        .iter()
        .filter(|success| success.machine_id == *machine_id)
        .flat_map(|success| success.value.iter())
        .map(|volume| volume.id.name.clone())
        .collect()
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
fn removal_warnings(
    machine: &MachineName,
    volumes: &[DockerVolumeName],
    services: &[ServiceName],
) -> Vec<String> {
    let mut lines = Vec::new();
    if !volumes.is_empty() {
        lines.push(format!(
            "WARNING: Docker Volumes on Machine {machine} will be destroyed: {}",
            volumes
                .iter()
                .map(DockerVolumeName::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !services.is_empty() {
        lines.push(format!(
            "WARNING: Machine {machine} is running Services: {}",
            services
                .iter()
                .map(ServiceName::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, DockerVolume,
        DockerVolumeId, DockerVolumeName, HealthObservation, LiveServices, MachineFailure,
        MachineId, MachineName, MachineSuccess, PartialResult, RpcError, RpcErrorCode, ServiceId,
        ServiceName, derive_live_services,
    };
    use serde_json::{Value, json};

    use super::{docker_volumes_on, removal_warnings, services_on};
    use crate::connect::ConnectError;

    #[test]
    fn removal_warnings_are_silent_when_nothing_is_at_stake() {
        assert_eq!(
            removal_warnings(&MachineName::parse("ams1").unwrap(), &[], &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn removal_warnings_name_docker_volumes_that_will_be_destroyed() {
        assert_eq!(
            removal_warnings(
                &MachineName::parse("ams1").unwrap(),
                &[
                    DockerVolumeName::parse("ams-critical").unwrap(),
                    DockerVolumeName::parse("data").unwrap(),
                ],
                &[],
            ),
            vec![
                "WARNING: Docker Volumes on Machine ams1 will be destroyed: ams-critical, data"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn removal_warnings_name_services_on_the_machine() {
        assert_eq!(
            removal_warnings(
                &MachineName::parse("ams1").unwrap(),
                &[],
                &[
                    ServiceName::parse("api").unwrap(),
                    ServiceName::parse("web").unwrap(),
                ],
            ),
            vec!["WARNING: Machine ams1 is running Services: api, web".to_owned()]
        );
    }

    #[test]
    fn docker_volumes_on_names_volumes_observed_on_that_machine() {
        let listed = PartialResult {
            successes: vec![
                MachineSuccess {
                    machine_id: machine_id('a'),
                    value: vec![volume('a', "data"), volume('a', "ams-critical")],
                },
                MachineSuccess {
                    machine_id: machine_id('b'),
                    value: vec![volume('b', "data")],
                },
            ],
            failures: vec![MachineFailure {
                machine_id: machine_id('c'),
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "offline".into(),
                    details: Value::Null,
                },
            }],
            omissions: vec![machine_id('d')],
        };
        assert_eq!(
            docker_volumes_on(&machine_id('a'), &listed),
            [
                DockerVolumeName::parse("data").unwrap(),
                DockerVolumeName::parse("ams-critical").unwrap(),
            ]
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

    fn volume(machine: char, name: &str) -> DockerVolume {
        DockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id(machine),
                name: DockerVolumeName::parse(name).unwrap(),
            },
            driver: "local".into(),
            options: Default::default(),
            labels: Default::default(),
        }
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

    #[test]
    fn reached_target_cleanup_rejections_are_not_unreachable_fallbacks() {
        assert!(
            ConnectError::Rpc(crate::connect::TransportError::from(
                tonic::Status::unavailable("route failed")
            ))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Rpc(crate::connect::TransportError::from(
                tonic::Status::unimplemented("older daemon")
            ))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Remote(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Docker is unavailable".into(),
                details: Value::Null,
            })
            .is_unreachable()
        );
    }

    #[test]
    fn deadline_exceeded_is_retryable_not_unreachable() {
        let error = ConnectError::Rpc(crate::connect::TransportError::from(
            tonic::Status::deadline_exceeded("timed out"),
        ));
        assert!(error.is_retryable());
        assert!(!error.is_unreachable());
    }
}
