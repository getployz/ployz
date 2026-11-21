//! The one Docker start of a Container, which refuses a second running Service
//! Container on a Provisioned Volume root. Every start Ployz makes goes through
//! [`start`], bridge recovery's included. Only Docker's own restarts bypass it.

use bollard::Docker;
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum};
use bollard::query_parameters::ListContainersOptionsBuilder;
use ployz_core::labels::ServiceLabels;
use ployz_core::{ContainerKind, PROVISIONED_VOLUME_DRIVER, QualifiedService, ResolvedServiceSpec};

use super::{Error, Status, classify};

/// Start `me` through Docker, unless it is a Service Container and another runs on a
/// Provisioned Volume root it mounts. Its Volumes come from `spec`, the one this
/// Machine kept, which names each as the user does; without one, as bridge recovery
/// starts, from Docker's own mounts.
///
/// # Errors
/// [`Error::VolumeHeld`] beside a running holder, the error of a read that failed,
/// and Docker's own error for a start it refused.
pub(crate) async fn start(
    client: &Docker,
    me: &ContainerInspectResponse,
    spec: Option<&ResolvedServiceSpec>,
) -> Result<(), Error> {
    let id = me.id.as_deref().unwrap_or_default();
    if serving(me) {
        let volumes: Vec<(String, String)> = match spec {
            Some(spec) => provisioned(spec),
            None => me
                .mounts
                .iter()
                .flatten()
                .filter(|mount| {
                    mount.typ.as_deref() == Some("volume")
                        && mount.driver.as_deref() == Some(PROVISIONED_VOLUME_DRIVER)
                })
                .filter_map(|mount| Some((mount.name.clone()?, mount.name.clone()?)))
                .collect(),
        };
        for (name, called) in &volumes {
            refuse_beside(client, Some(id), name, called).await?;
        }
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the one Docker start of a Container, which checks its Volume's holders"
    )]
    client.start_container(id, None).await?;
    Ok(())
}

/// Refuse a Service Container on Docker Volume `volume`, which the user calls `called`,
/// while a Container other than `me` runs on it: running, restarting or paused, each of
/// which can write again without a start. A stopped holder doesn't count, so a
/// stop-first redeploy and its rollback go on as before. A pre-deploy hook mounts the
/// root beside the serving copy by design, so it doesn't count, and neither does the
/// Ingress Proxy. A running Container Ployz didn't make counts.
///
/// # Errors
/// [`Error::VolumeHeld`] naming the running holder, or the error of a read that failed.
pub(crate) async fn refuse_beside(
    client: &Docker,
    me: Option<&str>,
    volume: &str,
    called: &str,
) -> Result<(), Error> {
    let filters = std::collections::HashMap::from([("volume", vec![volume])]);
    let listed = client
        .list_containers(Some(
            ListContainersOptionsBuilder::default()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await?;
    for id in listed.into_iter().filter_map(|container| container.id) {
        if me == Some(id.as_str()) {
            continue;
        }
        let holder = match client.inspect_container(&id, None).await {
            Ok(holder) => holder,
            Err(error) if classify(&error) == Status::Absent => continue,
            Err(error) => return Err(error.into()),
        };
        // Before reading labels, so a stopped helper Ployz didn't make never matters.
        if !active(&holder) {
            continue;
        }
        let labels = holder
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        let held_by = match labels.map(ServiceLabels::parse) {
            Some(Ok(service)) => {
                if service.kind != ContainerKind::ServiceContainer
                    || service.identity == QualifiedService::system_ingress()
                {
                    continue;
                }
                service.identity.name.to_string()
            }
            _ => holder
                .name
                .as_deref()
                .unwrap_or(&id)
                .trim_start_matches('/')
                .to_owned(),
        };
        return Err(Error::VolumeHeld {
            volume: called.to_owned(),
            holder: held_by,
        });
    }
    Ok(())
}

/// Each Provisioned Volume `spec` mounts: its Docker name, and how the user knows it.
/// A Machine never learns the Volume's own name, so that is where it is mounted.
pub(crate) fn provisioned(spec: &ResolvedServiceSpec) -> Vec<(String, String)> {
    let graph = spec.volume_graph();
    graph
        .mounts()
        .iter()
        .filter_map(|mount| {
            let volume = graph.volume_for(mount);
            let ployz_core::RawVolumeSource::Provisioned { name, .. } = volume.source.kind() else {
                return None;
            };
            Some((name.to_string(), format!("the Volume at {}", mount.target)))
        })
        .collect()
}

/// Whether `me` is a Service Container other than the Ingress Proxy's.
fn serving(me: &ContainerInspectResponse) -> bool {
    me.config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| ServiceLabels::parse(labels).ok())
        .is_some_and(|service| {
            service.kind == ContainerKind::ServiceContainer
                && service.identity != QualifiedService::system_ingress()
        })
}

fn active(holder: &ContainerInspectResponse) -> bool {
    matches!(
        holder.state.as_ref().and_then(|state| state.status),
        Some(
            ContainerStateStatusEnum::RUNNING
                | ContainerStateStatusEnum::RESTARTING
                | ContainerStateStatusEnum::PAUSED
        )
    )
}
