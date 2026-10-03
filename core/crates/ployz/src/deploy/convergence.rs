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
    if let Some(reason) = refusal(&snapshot, service, &stranded) {
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

/// Why nothing of this Service may move now, if so. It holds whatever the snapshot can't
/// vouch for, and refuses before anything is removed when no Server can take a Container.
fn refusal(
    snapshot: &DeploySnapshot,
    service: &QualifiedService,
    stranded: &[&ContainerObservation],
) -> Option<String> {
    let name = |id: &ployz_core::MachineId| {
        snapshot
            .machines
            .iter()
            .find(|machine| machine.machine.id == *id)
            .map_or_else(
                || id.to_string(),
                |machine| machine.machine.name.to_string(),
            )
    };
    if let Some(id) = snapshot
        .container_failures
        .iter()
        .map(|failure| &failure.machine_id)
        .chain(&snapshot.container_omissions)
        .next()
    {
        return Some(format!("cannot observe {}", name(id)));
    }
    let active = snapshot
        .containers
        .iter()
        .filter(|container| is_service_container(container, service))
        .collect::<Vec<_>>();
    let spec = &active.first()?.resolved_spec;
    if active
        .iter()
        .any(|container| container.resolved_spec.serving_shape() != spec.serving_shape())
    {
        return Some("mid-rollout: deploy it first".into());
    }
    let requested = spec.to_requested();
    if let Some(machine) = snapshot.machines.iter().find(|machine| {
        matches!(
            requested.placement_eligibility_in_namespace(
                &service.namespace,
                &machine.machine,
                machine.storage.as_ref(),
            ),
            ServicePlacementEligibility::Unknown(_)
        )
    }) {
        return Some(format!(
            "eligibility on {} is unknown",
            machine.machine.name
        ));
    }
    let first = stranded.first()?;
    if let Some(reason) = stays(spec, &machine_name(snapshot, first)) {
        return Some(reason);
    }
    super::planning::place_one(&requested, &service.namespace, snapshot)
        .err()
        .map(|error| format!("no eligible Server: {error}"))
}

fn is_service_container(container: &ContainerObservation, service: &QualifiedService) -> bool {
    container.kind == ContainerKind::ServiceContainer
        && container.namespace == service.namespace
        && container.resolved_spec.name == service.name
        && super::is_active_runtime(&container.runtime)
}

/// Why this Service's Containers can't move off `machine`, if they can't. It refuses
/// anything a move could lose.
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
            is_service_container(container, service)
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
        .map_err(|error| format!("no eligible Server: {error}"))?;
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
    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
        ContainerRuntimeObservation, HealthObservation, Machine, MachineId, MachineName,
        MachineObservation, MembershipObservation, Namespace, QualifiedService,
        ResolvedServiceSpec, WireGuardPublicKey,
    };
    use serde_json::json;

    use super::{DeploySnapshot, refusal, stays};

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

    #[test]
    fn what_the_snapshot_cannot_vouch_for_is_held() {
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let stateless = spec(replicated.clone(), json!({ "kind": "tmpfs" }));
        let service = QualifiedService::parse("app/api").unwrap();
        let refused = |snapshot: &DeploySnapshot| {
            let stranded = super::stranded(snapshot, &service);
            refusal(snapshot, &service, &stranded)
        };
        let base = || DeploySnapshot {
            machines: vec![machine('a', false), machine('b', true)],
            containers: vec![container('1', 'a', stateless.clone())],
            ..DeploySnapshot::default()
        };
        assert_eq!(refused(&base()), None);

        let mut newer = stateless.clone();
        newer.container.image = "alpine:3.24".into();
        let mut mixed = base();
        mixed.containers.push(container('2', 'b', newer));
        assert_eq!(
            refused(&mixed).as_deref(),
            Some("mid-rollout: deploy it first")
        );

        let provisioned = spec(
            replicated,
            json!({
                "kind": "provisioned",
                "name": "app_data",
                "maximum_bytes": 1_073_741_824,
                "scope": { "namespace": "app", "logical_name": "data" }
            }),
        );
        let mut unknown = base();
        unknown.containers = vec![container('1', 'a', provisioned)];
        assert_eq!(
            refused(&unknown).as_deref(),
            Some("eligibility on machine-b is unknown")
        );

        let mut unobserved = base();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        assert_eq!(
            refused(&unobserved).as_deref(),
            Some("cannot observe machine-b")
        );

        let mut nowhere = base();
        nowhere.machines = vec![machine('a', false), machine('b', false)];
        assert!(
            refused(&nowhere).is_some_and(|reason| reason.starts_with("no eligible Server")),
            "{:?}",
            refused(&nowhere)
        );
    }

    fn machine(hex: char, accepts_services: bool) -> MachineObservation {
        MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services,
                accepts_ingress: false,
                id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
                name: MachineName::parse(format!("machine-{hex}")).unwrap(),
                subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
                public_key: WireGuardPublicKey([hex as u8; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    }

    fn container(id: char, machine: char, spec: ResolvedServiceSpec) -> ContainerObservation {
        ContainerObservation::try_from(ContainerObservationParts {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: "api".into(),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: spec,
            address: None,
            labels: Default::default(),
        })
        .unwrap()
    }
}
