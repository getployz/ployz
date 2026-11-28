//! The one Docker start of a Container, which refuses a second running Service
//! Container on a Provisioned Volume root, unless the Container starting may share that
//! Volume. Every start Ployz makes goes through [`start_inspected`], bridge recovery's
//! included. Only Docker's own restarts bypass it.

use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;
use ployz_core::labels::{self, LabelMap, ServiceLabels};
use ployz_core::{ContainerKind, PROVISIONED_VOLUME_DRIVER, QualifiedService, ResolvedServiceSpec};

use super::Error;
use super::inspect::{self, Inspected, inspect};

/// [`start_inspected`] from bollard's typed inspect, which bridge recovery in
/// `network::linux::stopped` still reads.
///
/// # Errors
/// As [`start_inspected`], and [`Error::Json`] for an inspect that doesn't re-decode.
pub(crate) async fn start(
    client: &Docker,
    me: &bollard::models::ContainerInspectResponse,
    spec: Option<&ResolvedServiceSpec>,
) -> Result<(), Error> {
    start_inspected(client, &Inspected::from_typed(me)?, spec).await
}

/// Start `me` through Docker, unless it is a Service Container and another runs on a
/// Provisioned Volume root it mounts and doesn't share. Its Volumes come from `spec`, the
/// one this Machine kept; without one, as bridge recovery starts, from Docker's own mounts
/// less those its [`labels::SHARED_WRITES`] names.
///
/// # Errors
/// [`Error::VolumeHeld`] beside a running holder, the error of a read that failed,
/// and Docker's own error for a start it refused.
pub(crate) async fn start_inspected(
    client: &Docker,
    me: &Inspected,
    spec: Option<&ResolvedServiceSpec>,
) -> Result<(), Error> {
    let id = me.id.as_deref().unwrap_or_default();
    if serving(me) {
        let volumes: Vec<String> = match spec {
            Some(spec) => exclusive(spec),
            None => {
                let shared = me
                    .labels()
                    .and_then(|map| map.label(labels::SHARED_WRITES))
                    .unwrap_or_default();
                me.mounts
                    .iter()
                    .flatten()
                    .filter(|mount| {
                        mount.typ.as_deref() == Some("volume")
                            && mount.driver.as_deref() == Some(PROVISIONED_VOLUME_DRIVER)
                    })
                    .filter_map(|mount| mount.name.clone())
                    .filter(|name| !shared.split(',').any(|shared| shared == name))
                    .collect()
            }
        };
        for name in &volumes {
            refuse_beside(client, Some(id), name).await?;
        }
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the one Docker start of a Container, which checks its Volume's holders"
    )]
    client.start_container(id, None).await?;
    Ok(())
}

/// Refuse a Service Container on Docker Volume `volume` while a Container other than `me`
/// runs on it: running, restarting or paused, each of
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
) -> Result<(), Error> {
    for (id, holder) in active_beside(client, me, volume).await? {
        let held_by = match holder.labels().map(ServiceLabels::parse) {
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
        // Where the holder mounts the Volume, which may differ from where `me` would.
        let at = holder
            .mounts
            .iter()
            .flatten()
            .find(|mount| mount.name.as_deref() == Some(volume))
            .and_then(|mount| mount.destination.as_deref());
        return Err(Error::VolumeHeld {
            volume: at.map_or_else(|| volume.to_owned(), |at| format!("the Volume at {at}")),
            holder: held_by,
        });
    }
    Ok(())
}

/// The Docker name of a Container other than `me`, when given, that runs on Docker Volume `volume`:
/// running, restarting or paused, a hook or the Ingress Proxy included.
///
/// # Errors
/// The error of a read that failed.
pub(crate) async fn running_beside(
    client: &Docker,
    me: Option<&str>,
    volume: &str,
) -> Result<Option<String>, Error> {
    Ok(active_beside(client, me, volume)
        .await?
        .into_iter()
        .next()
        .map(|(id, holder)| {
            holder
                .name
                .as_deref()
                .unwrap_or(&id)
                .trim_start_matches('/')
                .to_owned()
        }))
}

/// Whether a Service Container of `service`, other than `me`, runs on Docker Volume
/// `volume`. Only Docker's `running` counts: a paused or restarting replacement serves
/// nothing. With no `service`, a Service Container of any Service but the Ingress
/// Proxy. A hook, or a Container Ployz didn't make, is no Service.
///
/// # Errors
/// The error of a read that failed.
pub(crate) async fn service_beside(
    client: &Docker,
    me: Option<&str>,
    volume: &str,
    service: Option<&QualifiedService>,
) -> Result<bool, Error> {
    Ok(active_beside(client, me, volume)
        .await?
        .iter()
        .filter(|(_, holder)| holder.is_running())
        .filter_map(|(_, holder)| holder.labels().map(ServiceLabels::parse)?.ok())
        .any(|labels| {
            labels.kind == ContainerKind::ServiceContainer
                && labels.identity != QualifiedService::system_ingress()
                && service.is_none_or(|service| *service == labels.identity)
        }))
}

/// Each Container other than `me` on Docker Volume `volume` that is running,
/// restarting or paused, each of which can write again without a start, with its ID.
/// A holder gone between the list and its inspect is not one.
async fn active_beside(
    client: &Docker,
    me: Option<&str>,
    volume: &str,
) -> Result<Vec<(String, Inspected)>, Error> {
    let filters = std::collections::HashMap::from([("volume", vec![volume])]);
    let listed = client
        .list_containers(Some(
            ListContainersOptionsBuilder::default()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await?;
    let mut active_holders = Vec::new();
    for id in listed.into_iter().filter_map(|container| container.id) {
        if Some(id.as_str()) == me {
            continue;
        }
        let Some(holder) = inspect(client, &id).await.map_err(inspect::failed)? else {
            continue;
        };
        if active(&holder) {
            active_holders.push((id, holder));
        }
    }
    Ok(active_holders)
}

/// The Docker name of each Provisioned Volume `spec` mounts and doesn't share: a
/// Container of it starts only while no other Service Container runs on one.
pub(crate) fn exclusive(spec: &ResolvedServiceSpec) -> Vec<String> {
    spec.volume_graph()
        .mounted_provisioned_volumes()
        .filter(|volume| !spec.container.shared_writes.contains(&volume.reference))
        .filter_map(|volume| volume.source.docker_volume_name())
        .map(ToString::to_string)
        .collect()
}

/// The Docker name of each Volume `spec` mounts and may write beside another running
/// Service Container, sorted, as [`labels::SHARED_WRITES`] carries them.
pub(crate) fn shared(spec: &ResolvedServiceSpec) -> Vec<String> {
    let mut shared: Vec<String> = spec
        .volume_graph()
        .volumes()
        .iter()
        .filter(|volume| spec.container.shared_writes.contains(&volume.reference))
        .filter_map(|volume| volume.source.docker_volume_name())
        .map(ToString::to_string)
        .collect();
    shared.sort();
    shared.dedup();
    shared
}

/// Whether `me` is a Service Container other than the Ingress Proxy's.
fn serving(me: &Inspected) -> bool {
    me.labels()
        .and_then(|labels| ServiceLabels::parse(labels).ok())
        .is_some_and(|service| {
            service.kind == ContainerKind::ServiceContainer
                && service.identity != QualifiedService::system_ingress()
        })
}

fn active(holder: &Inspected) -> bool {
    matches!(holder.status(), Some("running" | "restarting" | "paused"))
}
