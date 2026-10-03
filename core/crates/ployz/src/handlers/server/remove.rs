use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, LiveServices, Machine, MachineId, MachineName, MachineTarget,
    NameMatches, QualifiedService, RpcError, RpcErrorCode, ServiceMode, op,
};

use super::super::runtime;
use super::{ConnectionOptions, target};
use crate::cloud_account::{self, Credential, Release};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::cluster::{CloudHold, refuse_last_managed};
use crate::connect::Remover;
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
    let (mut client, selected, hold, cloud, observed, services, replicated_services) = runtime.block_on(async {
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
        let hold = refuse_last_managed(&client, &machines, selected.id).await?;
        let cloud = if hold == CloudHold::Last || cloud_manages(&client, current).await? {
            Some(cloud_removal(matches, &selected, hold, no_reset).await?)
        } else {
            None
        };
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
        Ok::<_, Error>((client, selected, hold, cloud, observed, services, replicated_services))
    })?;
    for line in service_warnings(&selected.name, &services) {
        eprintln!("{line}");
    }
    if hold == CloudHold::Last {
        output::warn(format!(
            "Server {} is the last Server: whatever runs on it stops, and nothing runs until you add a Server.",
            selected.name
        ));
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
        let mut cloud_released = None;

        // TODO: do not reroute away from the current entry before removal.
        if let Some(credential) = &cloud {
            let reset = (!no_reset).then_some(&confirmation);
            let removed = cloud_account::remove_server(credential, &selected.id, reset).await?;
            reset_failure = removed.reset_warning;
            if let Release::Kept { reason } = &removed.release {
                output::warn(format!("Cloud keeps its hold on the Cluster: {reason}"));
            }
            cloud_released = Some(removed.release == Release::Released);
        } else if no_reset {
            client.remove_machine_membership(&selected_target).await?;
        } else {
            let removed = client
                    .remove_machine(&selected_target, &confirmation, Remover::Operator)
                    .await
                    .map_err(crate::failure::refusal_from_rpc)?;
            reset_failure = removed.reset_warning;
        }
        say!("Removed Server {} ({}) membership", selected.name, selected.id);
        if cloud_released == Some(true) {
            say!("Cloud let go of the Cluster: this Organization has no Server now. Its Environments keep their config; nothing runs until you add a Server.\nnext: ployz server add");
        }
        if let Some(reason) = &reset_failure {
            eprintln!("Server {} cleanup/reset incomplete: {reason}. Reset does not erase volume data.", selected.id);
        } else {
            for loss in &observed.data_loss {
                say!("Volume data was not erased by reset: {loss}");
            }
        }
        if !replicated_services.is_empty() {
            eprintln!(
                "WARNING: Replicated Services may now be under-replicated: {}. Drain a Server before removing it to move its replicas first: ployz server drain <server>",
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
            "cloud_released": cloud_released,
            "next": (cloud_released == Some(true)).then_some("ployz server add"),
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

/// Cloud manages this Server, so Cloud removes it through its durable removal and drops
/// its own hold: its row for it, and the Cluster with its last Server. That needs a
/// Cloud credential, and for the last Server the reset; without either nothing happens.
async fn cloud_removal(
    matches: &ArgMatches,
    selected: &Machine,
    hold: CloudHold,
    no_reset: bool,
) -> Result<Credential, Error> {
    if hold == CloudHold::Last && no_reset {
        return Err(Error::conflict(format!(
            "Server {} is the last Server and Cloud manages it; Cloud lets go of it only by resetting it. Drop --no-reset. No changes made.",
            selected.name
        )));
    }
    let store = CredentialStore::beside(&crate::handlers::config_path(matches)?);
    match cloud_account::from_env(&store).await {
        Ok(credential) => Ok(credential),
        Err(LoginError::SignedOut) => Err(Error::detailed(
            RpcErrorCode::Conflict,
            format!(
                "Cloud manages Server {}, so Cloud removes it and drops its hold on it. \
                 Sign in to Cloud first, or remove it from the dashboard. No changes made.",
                selected.name
            ),
            json!({ "next": "ployz login" }),
        )),
        Err(error) => Err(error.into()),
    }
}

/// Whether Cloud manages this Cluster: the entry Server, which always answers, holds
/// Cloud's key. Cloud holds every Server it enrolled, so it removes any of them itself
/// and drops its row. Holders that can't be read refuse: they may be Cloud's.
async fn cloud_manages(client: &crate::connect::Client, entry: MachineId) -> Result<bool, Error> {
    let details = client
        .invoke::<op::Inspect>(
            ployz_core::InspectRequest::default(),
            &MachineTarget::from(&entry),
            Some(crate::connect::TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| {
            Error::unavailable(format!(
                "Cannot read who manages Server {entry}: {}. No changes made.",
                error.message
            ))
        })?;
    Ok(details
        .management_clients
        .iter()
        .any(|label| label.as_str() == "cloud"))
}

/// Each observed Docker Volume that keeps a Volume's data, by that Volume's name, so
/// the list and `--accept-volume-loss` speak Volume names. Best effort: without a
/// reachable Store, or for a Docker Volume no Environment owns, the Docker name stays.
fn volume_labels(root: &ArgMatches, observed: &ObservedDataLoss) -> VolumeLabels {
    let mut owners = Vec::new();
    if observed.data_loss.is_empty() {
        return VolumeLabels::new();
    }
    let Ok(Some(store)) = store::reachable(root) else {
        return VolumeLabels::new();
    };
    let Ok(owned) = store.try_read(&NamespacesQuery {}) else {
        return VolumeLabels::new();
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
            project: Some(owned.project.clone()),
            environment: Some(owned.environment.clone()),
        };
        let Ok(view) = store.try_read(&VolumesQuery { environment }) else {
            continue;
        };
        for listing in view.volumes {
            if let Ok(docker) = docker_volume(&owned.namespace, listing.volume.id.as_str())
                && observed
                    .data_loss
                    .iter()
                    .any(|loss| loss.name() == docker.as_str())
            {
                owners.push(VolumeOwner {
                    docker: docker.to_string(),
                    project: owned.project.to_string(),
                    environment: owned.environment.to_string(),
                    volume: listing.volume.name.to_string(),
                });
            }
        }
    }
    qualified_labels(&owners)
}

/// A Volume some Environment keeps on a Docker Volume.
struct VolumeOwner {
    docker: String,
    project: String,
    environment: String,
    volume: String,
}

/// Each Docker Volume by the shortest name that means one Volume: `VOLUME`, else
/// `ENVIRONMENT/VOLUME`, else `PROJECT/ENVIRONMENT/VOLUME`. So one accepted name never
/// covers a Volume of another Environment.
fn qualified_labels(owners: &[VolumeOwner]) -> VolumeLabels {
    let unique = |name: &dyn Fn(&VolumeOwner) -> String, owner: &VolumeOwner| {
        owners
            .iter()
            .filter(|other| name(other) == name(owner))
            .all(|other| {
                (&other.project, &other.environment) == (&owner.project, &owner.environment)
            })
    };
    let bare = |owner: &VolumeOwner| owner.volume.clone();
    let in_environment = |owner: &VolumeOwner| format!("{}/{}", owner.environment, owner.volume);
    owners
        .iter()
        .map(|owner| {
            let label = if unique(&bare, owner) {
                bare(owner)
            } else if unique(&in_environment, owner) {
                in_environment(owner)
            } else {
                format!("{}/{}/{}", owner.project, owner.environment, owner.volume)
            };
            (owner.docker.clone(), label)
        })
        .collect()
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
pub(super) fn services_on(
    machine_id: &MachineId,
    live: &LiveServices<RpcError>,
) -> Vec<QualifiedService> {
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
        "WARNING: Server {machine} is running Services: {}. Move them off first: ployz server drain {machine}",
        services
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )]
}

#[must_use]
pub(super) fn replicated_services_on(
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

    use super::{
        VolumeOwner, machine_removal_refusal, qualified_labels, replicated_services_on,
        service_warnings, services_on,
    };

    #[test]
    fn a_volume_name_two_environments_share_is_qualified_by_environment() {
        let owner = |docker: &str, project: &str, environment: &str, volume: &str| VolumeOwner {
            docker: docker.into(),
            project: project.into(),
            environment: environment.into(),
            volume: volume.into(),
        };
        let labels = qualified_labels(&[
            owner("a_vol-1", "shop", "production", "data"),
            owner("b_vol-2", "shop", "staging", "data"),
            owner("c_vol-3", "blog", "staging", "data"),
            owner("a_vol-4", "shop", "production", "logs"),
        ]);
        assert_eq!(
            labels.get("a_vol-1").map(String::as_str),
            Some("production/data")
        );
        assert_eq!(
            labels.get("b_vol-2").map(String::as_str),
            Some("shop/staging/data")
        );
        assert_eq!(
            labels.get("c_vol-3").map(String::as_str),
            Some("blog/staging/data")
        );
        assert_eq!(labels.get("a_vol-4").map(String::as_str), Some("logs"));
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
                    QualifiedService::parse("app/api").unwrap(),
                    QualifiedService::parse("app/web").unwrap(),
                ],
            ),
            vec!["WARNING: Server ams1 is running Services: app/api, app/web. Move them off first: ployz server drain ams1".to_owned()]
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
