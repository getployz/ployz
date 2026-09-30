use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, LiveServices, Machine, MachineId, MachineName, MachineTarget,
    NameMatches, QualifiedService, RpcError, RpcErrorCode, ServiceMode, op,
};

use super::super::runtime;
use super::{ConnectionOptions, target};
use crate::cluster::refuse_last_managed;
use crate::handlers::{
    Error,
    data_loss::{VolumeEffect, VolumeLabels, volume_label},
    leaf_matches, store,
};
use ployz_core::{EnvironmentValues, ObservedDataLoss};
use ployz_store::{EnvironmentRef, NamespacesQuery, VolumesQuery, docker_volume};
use serde_json::json;

use crate::output::{self, say};

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let runtime = runtime()?;
    let (mut client, selected, observed, services, replicated_services) = runtime.block_on(async {
        let mut client = super::connect(matches, options.context()).await?;
        let machines = client.machines().await?;
        let selected = select_machine(&machines, &selector)?;
        let selected_target = MachineTarget::from(&selected.id);
        let current = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await?
            .machine_id;
        if selected.id == current && machines.len() > 1 {
            return Err(Error::conflict(
                "the current entry Server cannot be removed while another Server is visible",
            ));
        }
        // Before anything is listed or confirmed: a removal that can't happen asks nothing.
        refuse_last_managed(&client, &machines, selected.id).await?;
        let observed = if no_reset {
            ployz_core::ObservedDataLoss { data_loss: Vec::new() }
        } else {
            client.data_loss_if_machine_removed(&selected_target).await
                .map_err(machine_removal_refusal)?
        };
        let live = client.live_services_from(&machines, EnvironmentValues::Redacted).await?;
        if !no_reset {
            if let Some(failure) = live.containers.failures.iter().find(|failure| failure.machine_id == selected.id) {
                return Err(Error::unavailable(format!("Cannot observe Services on Server {}: {}. No changes made.", selected.id, failure.error.message)));
            }
            if live.containers.omissions.contains(&selected.id) {
                return Err(Error::unavailable(format!("Cannot observe Services on Server {}: no terminal response. No changes made.", selected.id)));
            }
        }
        let services = services_on(&selected.id, &live);
        let replicated_services = replicated_services_on(&selected.id, &live);
        Ok::<_, Error>((client, selected, observed, services, replicated_services))
    })?;
    for line in service_warnings(&selected.name, &services) {
        eprintln!("{line}");
    }
    // The Store reads block on their own runtime, so they run between the two.
    let labels = volume_labels(root, &observed);
    typed_confirmation(root, &client, &selected, &observed, &services, &labels)?;
    let Some(confirmation) = super::super::data_loss::confirm_removal(
        root,
        &client,
        &observed,
        &format!("Remove Server ({})", selected.id),
        &[selected.name.to_string()],
        if no_reset {
            VolumeEffect::Preserve
        } else {
            VolumeEffect::LoseAccess
        },
        &labels,
    )?
    else {
        return Ok(());
    };
    let selected_target = MachineTarget::from(&selected.id);
    runtime.block_on(async {
        let mut reset_failure = None;

        // TODO: do not reroute away from the current entry before removal.
        // TODO: there is no drain or unschedulable phase before cleanup.
        if no_reset {
            client.remove_machine_membership(&selected_target).await?;
        } else {
            let removed = client
                    .remove_machine(&selected_target, &confirmation)
                    .await
                    .map_err(crate::failure::refusal_from_rpc)?;
            reset_failure = removed.reset_warning;
        }
        say!("Removed Server {} ({}) membership", selected.name, selected.id);
        if let Some(reason) = &reset_failure {
            eprintln!("Server {} cleanup/reset incomplete: {reason}. Reset does not erase volume data.", selected.id);
        } else {
            for loss in &observed.data_loss {
                say!("Volume data was not erased by reset: {loss}");
            }
        }
        if !replicated_services.is_empty() {
            eprintln!(
                "WARNING: Replicated Services may now be under-replicated: {}. Replicas are not re-placed automatically.",
                replicated_services
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        // The removal is committed: print it before local cleanup can fail.
        output::emit(&json!({
            "server": super::server_json(&selected),
            "reset_warning": reset_failure,
            "data_loss": observed.data_loss,
            "under_replicated": replicated_services,
        }))?;
        // Cleanup failure must not leave the removed Machine named in the
        // context (#249); after the printed result it is partial, not a failed removal (#449).
        let mut config = options.load_or_empty_config().map_err(|error| Error::warned("local context cleanup failed after Server removal", error))?;
        if let Some(context_name) = config.context_name(options.context()).map(str::to_owned)
            && let Some(context) = config.contexts.get_mut(&context_name)
        {
            context.drop_machine(&selected.id);
            config.save().map_err(|error| Error::warned("local context cleanup failed after Server removal", error))?;
        }
        if reset_failure.is_some() {
            return Err(Error::partial());
        }
        Ok::<_, Error>(())
    })
}

/// Each observed Docker Volume that keeps a Volume's data, by that Volume's name, so
/// the list and `--accept-volume-loss` speak Volume names. Best effort: without a
/// reachable Store, or for a Docker Volume no Environment owns, the Docker name stays.
fn volume_labels(root: &ArgMatches, observed: &ObservedDataLoss) -> VolumeLabels {
    let mut labels = VolumeLabels::new();
    if observed.data_loss.is_empty() {
        return labels;
    }
    let Ok(Some(store)) = store::reachable(root) else {
        return labels;
    };
    let Ok(owned) = store.try_read(&NamespacesQuery {}) else {
        return labels;
    };
    for owned in owned.namespaces {
        let prefix = format!("{}_", owned.namespace);
        if !observed
            .data_loss
            .iter()
            .any(|loss| loss.name().starts_with(&prefix))
        {
            continue;
        }
        let environment = EnvironmentRef {
            project: Some(owned.project),
            environment: Some(owned.environment),
        };
        let Ok(view) = store.try_read(&VolumesQuery { environment }) else {
            continue;
        };
        for listing in view.volumes {
            if let Ok(docker) = docker_volume(&owned.namespace, listing.volume.id.as_str()) {
                labels.insert(docker.to_string(), listing.volume.name.to_string());
            }
        }
    }
    labels
}

/// `--confirm` must name the Server exactly. Without it, fail with `confirmation_required`,
/// naming what goes and the one command that removes it.
fn typed_confirmation(
    root: &ArgMatches,
    client: &crate::connect::Client,
    selected: &Machine,
    observed: &ObservedDataLoss,
    services: &[QualifiedService],
    labels: &VolumeLabels,
) -> Result<(), Error> {
    match leaf_matches(root).get_one::<String>("confirm") {
        Some(typed) if typed == selected.name.as_str() => Ok(()),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match Server {}. No changes made.",
            typed.escape_debug(),
            selected.name
        ))),
        None => {
            let mut retry = super::super::data_loss::retry_args(root, client.connection_source());
            retry.extend(["--confirm".into(), selected.name.to_string()]);
            let volumes = observed
                .data_loss
                .iter()
                .map(|loss| volume_label(labels, loss));
            for name in volumes.collect::<std::collections::BTreeSet<_>>() {
                retry.extend(["--accept-volume-loss".into(), name.to_owned()]);
            }
            let retry = shell_words::join(retry);
            Err(Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!(
                    "Removing Server {} needs its name typed. No changes made.\nRetry: {retry}",
                    selected.name
                ),
                json!({
                    "server": { "id": selected.id, "name": selected.name },
                    "services": services,
                    "data_loss": observed.data_loss,
                    "next": retry,
                }),
            ))
        }
    }
}

pub(super) fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineTarget::parse(selector)?;
    match selector.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(Error::not_found(format!(
            "Server {} was not found",
            selector.as_str().escape_debug()
        ))),
        NameMatches::One(machine) => Ok(machine.clone()),
        matches @ NameMatches::Ambiguous { .. } => Err(Error::ambiguous(format!(
            "Server name {} is ambiguous: {}",
            selector.as_str().escape_debug(),
            matches
                .iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

fn machine_removal_refusal(error: RpcError) -> Error {
    if error.code == RpcErrorCode::Unavailable {
        Error::unavailable(format!(
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
        "WARNING: Server {machine} is running Services: {}",
        services
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )]
}

#[must_use]
fn replicated_services_on(
    machine_id: &MachineId,
    live: &LiveServices<RpcError>,
) -> Vec<QualifiedService> {
    live.services()
        .into_iter()
        .filter(|service| {
            service.containers.iter().any(|container| {
                let observation = container.as_observation();
                observation.machine_id == *machine_id
                    && matches!(
                        observation.resolved_spec.mode,
                        ServiceMode::Replicated { .. }
                    )
            })
        })
        .map(|service| service.identity)
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
        LiveServices, MachineId, MachineName, MachineSuccess, PartialResult, QualifiedService,
        RpcError, RpcErrorCode, ServiceId, ServiceMode, ServiceName, derive_live_services,
    };
    use serde_json::{Value, json};

    use super::{machine_removal_refusal, replicated_services_on, service_warnings, services_on};

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
            vec!["WARNING: Server ams1 is running Services: app/api, app/web".to_owned()]
        );
    }

    #[test]
    fn unreachable_removal_names_no_reset() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Server aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa did not respond".into(),
            details: Value::Null,
        };
        assert_eq!(
            machine_removal_refusal(error).to_string(),
            "Server aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa did not respond; use --no-reset to remove it from the Cluster without resetting"
        );
    }

    #[test]
    fn other_data_loss_errors_keep_their_message() {
        let error = RpcError {
            code: RpcErrorCode::NotFound,
            message: "Server \"gone\" was not found".into(),
            details: Value::Null,
        };
        assert_eq!(
            machine_removal_refusal(error).to_string(),
            "Server \"gone\" was not found"
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

    #[test]
    fn replicated_services_on_excludes_global_services() {
        let live: LiveServices<RpcError> = derive_live_services(PartialResult {
            successes: vec![MachineSuccess {
                machine_id: machine_id('a'),
                value: vec![
                    observation('1', 'a', "api", ContainerKind::ServiceContainer, 'a'),
                    {
                        let mut global =
                            observation('2', 'a', "caddy", ContainerKind::ServiceContainer, 'b');
                        global
                            .try_update(|parts| parts.resolved_spec.mode = ServiceMode::Global)
                            .unwrap();
                        global
                    },
                ],
            }],
            failures: Vec::new(),
            omissions: Vec::new(),
        });
        assert_eq!(
            replicated_services_on(&machine_id('a'), &live),
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
        ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
            container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: name.into(),
            created_at_unix_nanos: 0,
            machine_id: machine_id(machine),
            namespace: ployz_core::Namespace::parse("app").unwrap(),
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
        })
        .unwrap()
    }

    fn machine_id(value: char) -> MachineId {
        MachineId::parse(value.to_string().repeat(32)).unwrap()
    }
}
