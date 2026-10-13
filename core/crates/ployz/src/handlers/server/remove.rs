use clap::ArgMatches;
use ployz_core::{
    DescribeContractRequest, Machine, MachineId, MachineTarget, RpcError, RpcErrorCode, op,
};

use super::super::runtime;
use super::{ConnectionOptions, target};
use crate::approval::{self, Verb};
use crate::cloud_account::{self, Credential, Release};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::cluster::{
    CloudHold, DeadConfirmed, HeldCopies, LeftNotForgotten, copies_left_behind, refuse_last_managed,
};
use crate::connect::{Remover, TARGET_RPC_TIMEOUT};
use crate::context::ConnectionSource;
use crate::drain::{replicated_services_on, services_on};
use crate::handlers::{
    Error,
    data_loss::{Typed, VolumeEffect, VolumeLabels, volume_label},
    leaf_matches, store,
};
use ployz_core::{DataLoss, DockerVolumeName, EnvironmentValues};
use ployz_store::{EnvironmentRef, NamespacesQuery, VolumesQuery, docker_volume};
use serde_json::json;

use crate::ui::Hint;

pub(in crate::handlers) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let options = ConnectionOptions::from_matches(root)?;
    let matches = leaf_matches(root);
    let selector = target(matches, "server")?.to_owned();
    let no_reset = matches.get_flag("no-reset");
    let runtime = runtime()?;
    // Before anything is listed or confirmed: a removal that can't happen asks nothing.
    let (mut client, machines, selected, current, hold, held) = runtime.block_on(async {
        let mut client = super::connect(matches, options.context()).await?;
        let machines = client.machines().await?;
        let observation =
            crate::cluster::visible_machine(&MachineTarget::parse(&selector)?, &machines)?;
        let selected = observation.machine.clone();
        let current = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await?
            .machine_id;
        if selected.id == current && machines.len() > 1 {
            return Err(Error::conflict(
                "the current entry Server cannot be removed while another Server is visible",
            ));
        }
        let hold = refuse_last_managed(&client, &machines, selected.id).await?;
        let held = if no_reset {
            Some(client.held_copies(observation).await?)
        } else {
            None
        };
        Ok::<_, Error>((client, machines, selected, current, hold, held))
    })?;
    if let Some(HeldCopies::Answered(held)) = &held
        && !held.is_empty()
    {
        let docker = held
            .iter()
            .map(DockerVolumeName::as_str)
            .collect::<Vec<_>>();
        let labels = volume_labels(root, &docker);
        return Err(copies_left_behind(
            &selected.name,
            docker
                .iter()
                .map(|name| labels.get(*name).map_or(*name, String::as_str)),
        )
        .into());
    }
    let (cloud, observed, services, replicated_services) = runtime.block_on(async {
        let selected_target = MachineTarget::from(&selected.id);
        let entry = machines
            .iter()
            .find(|observed| observed.machine.id == current)
            .map_or_else(
                || current.to_string(),
                |observed| observed.machine.name.to_string(),
            );
        let cloud = if hold == CloudHold::Last || cloud_manages(&client, current, &entry).await? {
            Some(cloud_removal(matches, &selected, hold, no_reset).await?)
        } else {
            None
        };
        let observed = if no_reset {
            ployz_core::ObservedDataLoss {
                data_loss: Vec::new(),
            }
        } else {
            client
                .data_loss_if_machine_removed(&selected_target)
                .await
                .map_err(machine_removal_refusal)?
        };
        let live = client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await?;
        if !no_reset {
            if let Some(failure) = live
                .containers
                .failures
                .iter()
                .find(|failure| failure.machine_id == selected.id)
            {
                return Err(Error::caused(
                    RpcErrorCode::Unavailable,
                    format!(
                        "Cannot observe Services on Server {}. No changes made.",
                        selected.name
                    ),
                    failure.error.clone(),
                ));
            }
            if live.containers.omissions.contains(&selected.id) {
                return Err(Error::unavailable(format!(
                    "Cannot observe Services on Server {}: no terminal response. No changes made.",
                    selected.name
                )));
            }
        }
        let services = services_on(&selected.id, &live);
        let replicated_services = replicated_services_on(&selected.id, &live);
        Ok::<_, Error>((cloud, observed, services, replicated_services))
    })?;
    if !services.is_empty() {
        crate::ui::warn(format!(
            "Server {} is running Services: {}.",
            selected.name,
            super::super::joined(&services)
        ));
        crate::ui::hint(&Hint::Next(format!("ployz server drain {}", selected.name)));
    }
    if hold == CloudHold::Last {
        crate::ui::warn(format!(
            "Server {} is the last Server: whatever runs on it stops, and nothing runs until you add a Server.",
            selected.name
        ));
    }
    // The Store reads block on their own runtime, so they run between the two.
    let labels = volume_labels(
        root,
        &observed
            .data_loss
            .iter()
            .map(DataLoss::name)
            .collect::<Vec<_>>(),
    );
    let dead = matches!(held, Some(HeldCopies::Dead));
    let unanswered = format!(
        "Server {} did not answer in {}s. Removing it without a reset leaves any Volume copies on it behind",
        selected.name,
        TARGET_RPC_TIMEOUT.as_secs()
    );
    if dead && crate::ui::can_prompt() {
        crate::ui::warn(format!("{unanswered}."));
    }
    let (confirmation, accepting) = super::super::data_loss::confirm_removal(
        root,
        &client,
        match &cloud {
            Some(_) => &ConnectionSource::Cloud,
            None => client.connection_source(),
        },
        &observed,
        if dead {
            Typed::dead(selected.name.as_str())
        } else {
            Typed::name(selected.name.as_str())
        },
        if no_reset {
            VolumeEffect::Preserve
        } else {
            VolumeEffect::LoseAccess
        },
        &labels,
        |retry| {
            Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                if dead {
                    format!("{unanswered} and needs dead typed. No changes made.")
                } else {
                    format!(
                        "Removing Server {} needs its name typed. No changes made.",
                        selected.name
                    )
                },
                json!({
                    "server": { "id": selected.id, "name": selected.name },
                    "services": services,
                    "data_loss": observed.data_loss,
                    "answered": !dead,
                }),
            )
            .hint(Hint::Retry(retry))
        },
    )?;
    let selected_target = MachineTarget::from(&selected.id);
    let removal = match &cloud {
        Some(credential) => {
            let reset = (!no_reset).then_some(&confirmation);
            Some(approval::approved(
                &runtime,
                credential,
                Verb::Remove,
                matches.get_one::<String>("approval").cloned(),
                |id| {
                    let mut command = accepting.clone();
                    command.extend(["--approval".into(), id.to_owned()]);
                    shell_words::join(command)
                },
                async |approval| {
                    cloud_account::start_removal(credential, &selected.id, reset, approval).await
                },
            )?)
        }
        None => None,
    };
    runtime.block_on(async {
        let mut reset_failure = None;
        let mut cloud_released = None;

        // TODO: do not reroute away from the current entry before removal.
        if let (Some(credential), Some(id)) = (&cloud, &removal) {
            let removed = cloud_account::follow_removal(credential, &selected.name, id).await?;
            reset_failure = removed.reset_warning;
            if let Release::Kept { reason } = &removed.release {
                crate::ui::warn(format!("Cloud keeps its hold on the Cluster: {reason}"));
            }
            cloud_released = Some(removed.release == Release::Released);
        } else if no_reset {
            client
                .remove_membership(
                    &selected_target,
                    if dead { DeadConfirmed::Yes } else { DeadConfirmed::No },
                )
                .await
                .map_err(|error| {
                    let late_dead = error.code == RpcErrorCode::ConfirmationRequired;
                    let error = Error::from(error);
                    if !late_dead {
                        return error;
                    }
                    let mut retry = accepting.clone();
                    if let Some(at) = retry.iter().position(|word| word == "--confirm") {
                        retry.truncate(at);
                    }
                    retry.extend(["--confirm".into(), "dead".into()]);
                    error.hint(Hint::Retry(shell_words::join(retry)))
                })?;
        } else {
            let removed = client
                    .remove_machine(&selected_target, &confirmation, Remover::Operator)
                    .await
                    .map_err(|error| reset_removal_refusal(error, selected.name.as_str()))?;
            reset_failure = removed.reset_warning;
        }
        crate::ui::stream(format_args!("Removed Server {}.", selected.name));
        if cloud_released == Some(true) {
            crate::ui::stream("Cloud let go of the Cluster: this Organization has no Server now. Its Environments keep their config; nothing runs until you add a Server.");
            crate::ui::hint(&Hint::Next("ployz server add".into()));
        }
        if let Some(reason) = &reset_failure {
            crate::ui::warn(format!("Server {} was not fully cleaned up or reset: {reason}. Reset does not erase volume data.", selected.name));
        } else {
            for loss in &observed.data_loss {
                crate::ui::stream(format_args!(
                    "Reset left the data of Volume {} on {}.",
                    volume_label(&labels, loss),
                    selected.name
                ));
            }
        }
        if !replicated_services.is_empty() {
            crate::ui::warn(format!(
                "Replicated Services may now be under-replicated: {}. Their replicas were not moved.",
                super::super::joined(&replicated_services)
            ));
        }

        // The removal is committed: print it before local cleanup can fail.
        crate::ui::emit(&json!({
            "server": super::server_json(&selected),
            "reset_warning": reset_failure,
            "data_loss": observed.data_loss,
            "under_replicated": replicated_services,
            "cloud_released": cloud_released,
            "next": (cloud_released == Some(true)).then_some("ployz server add"),
        }))?;
        let mut config = options.load_or_empty_config().map_err(|error| Error::from(error).context("Server removed; local context cleanup failed."))?;
        if let Some(context_name) = config.context_name(options.context()).map(str::to_owned)
            && let Some(context) = config.contexts.get_mut(&context_name)
        {
            context.drop_machine(&selected.id);
            config.save().map_err(|error| Error::from(error).context("Server removed; local context cleanup failed."))?;
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
        Err(LoginError::SignedOut) => Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "Cloud manages Server {}, so Cloud removes it and drops its hold on it. \
                 Sign in to Cloud first, or remove it from the dashboard. No changes made.",
                selected.name
            ),
        )
        .hint(Hint::Next("ployz login".into()))),
        Err(error) => Err(error.into()),
    }
}

/// Whether Cloud manages this Cluster: the entry Server, which always answers, holds
/// Cloud's key. Cloud holds every Server it enrolled, so it removes any of them itself
/// and drops its row. Holders that can't be read refuse: they may be Cloud's.
async fn cloud_manages(
    client: &crate::connect::Client,
    entry: MachineId,
    name: &str,
) -> Result<bool, Error> {
    let details = client
        .invoke::<op::Inspect>(
            ployz_core::InspectRequest::default(),
            &MachineTarget::from(&entry),
            Some(crate::connect::TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| {
            Error::caused(
                RpcErrorCode::Unavailable,
                format!("Cannot read who manages Server {name}. No changes made."),
                error,
            )
        })?;
    Ok(details
        .management_clients
        .iter()
        .any(|label| label.as_str() == "cloud"))
}

/// Each of the `docker` Volumes that keeps a Volume's data, by that Volume's name, so
/// the list and `--accept-volume-loss` speak Volume names. Best effort: without a
/// reachable Store, or for a Docker Volume no Environment owns, the Docker name stays.
fn volume_labels(root: &ArgMatches, docker: &[&str]) -> VolumeLabels {
    let mut owners = Vec::new();
    if docker.is_empty() {
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
        if !docker.iter().any(|name| name.starts_with(&prefix)) {
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
            if let Ok(volume) = docker_volume(&owned.namespace, listing.volume.id.as_str())
                && docker.contains(&volume.as_str())
            {
                owners.push(VolumeOwner {
                    docker: volume.to_string(),
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

pub(super) fn select_machine(
    machines: &[ployz_core::MachineObservation],
    selector: &str,
) -> Result<Machine, Error> {
    let selector = MachineTarget::parse(selector)?;
    Ok(crate::cluster::visible_machine(&selector, machines)?
        .machine
        .clone())
}

fn machine_removal_refusal(error: RpcError) -> Error {
    if error.code == RpcErrorCode::Unavailable {
        Error::from(error).context(
            "The Server could not be reset; use --no-reset to remove it from the Cluster without resetting.",
        )
    } else {
        error.into()
    }
}

/// A reset removal's refusal. A Server that reset before the Cluster could forget it needs
/// only the no-reset removal to finish.
fn reset_removal_refusal(error: RpcError, name: &str) -> Error {
    if LeftNotForgotten::from_rpc_error(&error).is_some() {
        Error::from(error)
            .context(format!("Server {name} reset but is still listed."))
            .hint(Hint::Next(format!("ployz server rm {name} --no-reset")))
    } else {
        crate::failure::refusal_from_rpc(error)
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerRuntimeObservation, HealthObservation,
        LiveServices, MachineId, MachineSuccess, PartialResult, QualifiedService, RpcError,
        RpcErrorCode, ServiceId, ServiceMode, ServiceName, derive_live_services,
    };
    use serde_json::{Value, json};

    use super::{
        VolumeOwner, machine_removal_refusal, qualified_labels, replicated_services_on,
        reset_removal_refusal, services_on,
    };
    use crate::ui::Hint;

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
    fn unreachable_removal_names_no_reset() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Server fra-1 did not respond".into(),
            details: Value::Null,
            cause: Vec::new(),
        };
        let refusal = machine_removal_refusal(error);
        assert_eq!(
            refusal.to_string(),
            "The Server could not be reset; use --no-reset to remove it from the Cluster without resetting."
        );
        assert_eq!(refusal.causes(), ["Server fra-1 did not respond"]);
        assert_eq!(refusal.report().code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn a_reset_server_still_listed_names_the_no_reset_removal() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "the entry could not reach the Cluster store".into(),
            details: json!({ "left": true, "reset_warning": null }),
            cause: Vec::new(),
        };
        let refusal = reset_removal_refusal(error, "fra-1");
        assert_eq!(
            refusal.to_string(),
            "Server fra-1 reset but is still listed."
        );
        assert_eq!(
            refusal.hints(),
            [Hint::Next("ployz server rm fra-1 --no-reset".into())]
        );
        assert_eq!(refusal.report().code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn other_data_loss_errors_keep_their_message() {
        let error = RpcError {
            code: RpcErrorCode::NotFound,
            message: "Server \"gone\" was not found".into(),
            details: Value::Null,
            cause: Vec::new(),
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
