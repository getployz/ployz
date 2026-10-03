//! Placement convergence: move one replicated Service's Containers off Machines that no
//! longer admit it, one at a time and start-first. No hooks, no Deployment record, no
//! Config Store writes, and each moved Container keeps the image ID it ran.

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, InspectContainerRequest, MachineName,
    MachineTarget, PullPolicy, QualifiedService, RawVolumeSource, ResolvedServiceSpec, ServiceMode,
    ServicePlacementEligibility, op,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

use super::DeploySnapshot;

/// One Container moved between Servers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Move {
    pub(crate) from: MachineName,
    pub(crate) to: MachineName,
}

/// What Placement convergence did for one Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum Convergence {
    /// Nothing moved: why the Containers stay where they are.
    Stays { reason: String },
    /// Moves made in order. A failure stops the rest; the Container it was moving keeps
    /// serving where it was.
    Moved {
        moved: Vec<Move>,
        failed: Option<String>,
    },
}

/// Converge `service`: replace each active Container on a Machine its own spec now rules
/// out with one on an eligible Machine, then remove the old one.
///
/// # Errors
///
/// Returns when the Cluster can't be observed. A failed move is a `Moved { failed }`.
pub(crate) async fn converge(
    client: &mut Client,
    service: &QualifiedService,
    cancellation: &CancellationToken,
) -> Result<Convergence, ConnectError> {
    let snapshot = observe(client).await?;
    let stranded = stranded(&snapshot, service);
    if let Some(reason) = stranded.iter().find_map(|container| {
        stays(
            &container.resolved_spec,
            &machine_name(&snapshot, container),
        )
    }) {
        return Ok(Convergence::Stays { reason });
    }
    let ids = stranded
        .iter()
        .map(|container| container.container_id)
        .collect::<Vec<_>>();
    let mut moved = Vec::new();
    let mut snapshot = Some(snapshot);
    for id in ids {
        let snapshot = match snapshot.take() {
            Some(snapshot) => snapshot,
            None => observe(client).await?,
        };
        let Some(container) = snapshot
            .containers
            .iter()
            .find(|container| container.container_id == id)
        else {
            continue;
        };
        match move_one(client, &snapshot, container, cancellation).await {
            Ok(step) => moved.push(step),
            Err(error) => {
                return Ok(Convergence::Moved {
                    moved,
                    failed: Some(error),
                });
            }
        }
    }
    Ok(Convergence::Moved {
        moved,
        failed: None,
    })
}

/// Why this Service's Containers can't move off `machine`, if they can't. The one place
/// that decides; it refuses anything a move could lose.
fn stays(spec: &ResolvedServiceSpec, machine: &MachineName) -> Option<String> {
    if spec.mode == ServiceMode::Global {
        return Some("Global Services run on every Server that accepts them".into());
    }
    spec.volume_graph()
        .mounted_volumes()
        .find_map(|volume| match volume.source.kind() {
            RawVolumeSource::Tmpfs { .. } => None,
            RawVolumeSource::Bind { .. } => Some(format!("Bind Mount on {machine}")),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. } => {
                Some(format!("Volume {} is on {machine}", volume.reference))
            }
        })
}

async fn observe(client: &mut Client) -> Result<DeploySnapshot, ConnectError> {
    let machines = client.machines().await?;
    client.deploy_snapshot(machines).await
}

/// The Service's active Containers on Machines its own spec definitely rules out.
fn stranded<'a>(
    snapshot: &'a DeploySnapshot,
    service: &QualifiedService,
) -> Vec<&'a ContainerObservation> {
    snapshot
        .containers
        .iter()
        .filter(|container| {
            container.kind == ContainerKind::ServiceContainer
                && container.namespace == service.namespace
                && container.resolved_spec.name == service.name
                && super::is_active_runtime(&container.runtime)
                && snapshot
                    .machines
                    .iter()
                    .find(|machine| machine.machine.id == container.machine_id)
                    .is_some_and(|machine| {
                        matches!(
                            container
                                .resolved_spec
                                .to_requested()
                                .placement_eligibility_in_namespace(
                                    &service.namespace,
                                    &machine.machine,
                                    machine.storage.as_ref(),
                                ),
                            ServicePlacementEligibility::Ineligible(_)
                        )
                    })
        })
        .collect()
}

fn machine_name(snapshot: &DeploySnapshot, container: &ContainerObservation) -> MachineName {
    snapshot
        .machines
        .iter()
        .find(|machine| machine.machine.id == container.machine_id)
        .expect("stranded Containers sit on observed Machines")
        .machine
        .name
        .clone()
}

/// Place, copy the exact image, start and serve, then remove the old Container.
async fn move_one(
    client: &mut Client,
    snapshot: &DeploySnapshot,
    container: &ContainerObservation,
    cancellation: &CancellationToken,
) -> Result<Move, String> {
    let machine = |id| {
        snapshot
            .machines
            .iter()
            .find(|machine| machine.machine.id == id)
            .map(|machine| &machine.machine)
            .expect("placement picks observed Machines")
    };
    let source = machine(container.machine_id);
    let spec = &container.resolved_spec;
    let dest = super::planning::place_one(&spec.to_requested(), &container.namespace, snapshot)
        .map_err(|error| format!("no Server can take it: {error}"))?;
    let dest = machine(dest);
    let image_id = image_id(client, source, &container.container_id).await?;
    crate::image::copy_running_image(client, source, dest, &spec.container.image, &image_id)
        .await
        .map_err(|error| format!("copying its image to {}: {error}", dest.name))?;
    // The image is on `dest` by now; a registry pull could fetch a different one.
    let mut spec = spec.clone();
    spec.container.pull_policy = PullPolicy::Never;
    super::exec::move_container(
        client,
        &container.namespace,
        &spec,
        &dest.id,
        (&source.id, &container.container_id),
        cancellation,
    )
    .await
    .map_err(|error| format!("moving it from {} to {}: {error}", source.name, dest.name))?;
    Ok(Move {
        from: source.name.clone(),
        to: dest.name.clone(),
    })
}

async fn image_id(
    client: &Client,
    source: &ployz_core::Machine,
    container: &ContainerId,
) -> Result<String, String> {
    client
        .invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container,
            },
            &MachineTarget::from(&source.id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| format!("inspecting it on {}: {}", source.name, error.message))?
        .image_id
        .ok_or_else(|| {
            format!(
                "Server {} does not report image IDs; upgrade it with `ployz server upgrade {}`, then rerun",
                source.name, source.name
            )
        })
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineName, ResolvedServiceSpec};
    use serde_json::json;

    use super::stays;

    fn spec(mode: serde_json::Value, source: serde_json::Value) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": mode,
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [{ "reference": "data", "source": source }],
            "mounts": [{ "volume": "data", "target": "/data" }],
        }))
        .unwrap()
    }

    #[test]
    fn only_what_a_move_cannot_lose_stays() {
        let web = MachineName::parse("web-2").unwrap();
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let tmpfs = json!({ "kind": "tmpfs", "size_bytes": 4096 });
        let stays_with =
            |mode: &serde_json::Value, source| stays(&spec(mode.clone(), source), &web);
        assert_eq!(stays_with(&replicated, tmpfs.clone()), None);
        assert_eq!(
            stays_with(
                &replicated,
                json!({ "kind": "bind", "machine_path": "/srv" })
            )
            .as_deref(),
            Some("Bind Mount on web-2")
        );
        assert_eq!(
            stays_with(&replicated, json!({ "kind": "external", "name": "app_db" })).as_deref(),
            Some("Volume data is on web-2")
        );
        assert!(stays_with(&json!({ "mode": "global" }), tmpfs).is_some());
    }
}
