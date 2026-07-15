//! Cluster lifecycle: per-run identity, the labeled bridge network, machine
//! provisioning order, sweeps, and explicit teardown.

use super::evidence::{capture_machine_evidence, evidence_dir};
use super::machine::{DindMachine, DindMachineRole, MachineSpec, provision_machine};
use super::{DindError, MANAGED_LABEL, MANAGED_LABEL_VALUE, RUN_LABEL, docker_api_error};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::{Ipam, IpamConfig, NetworkCreateRequest};
use bollard::query_parameters::{
    ListContainersOptionsBuilder, ListNetworksOptionsBuilder, ListVolumesOptionsBuilder,
    RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
};
use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

/// Unique identifier of one harness run; part of every resource name and the
/// value of the [`RUN_LABEL`] on every resource the run creates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DindRunId(String);

impl DindRunId {
    #[must_use]
    pub fn generate() -> Self {
        Self(nuid::next().to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DindRunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Requested cluster shape: exactly one core machine plus any number of
/// edges, all mounting the same host artifact directory.
#[derive(Debug, Clone)]
pub struct DindClusterSpec {
    /// Host directory mounted read-only at
    /// [`super::ARTIFACTS_MOUNT_PATH`] in every machine.
    pub artifact_dir: PathBuf,
    pub machines: Vec<MachineSpec>,
}

/// One provisioned cluster: a labeled bridge network plus ready machines.
///
/// Cleanup is the explicit [`DindCluster::teardown`] call (plus
/// `scripts/dind-clean.sh` for crashed runs) — there is deliberately no
/// `Drop` impl, because panics must leave evidence behind.
#[derive(Debug)]
pub struct DindCluster {
    docker: Docker,
    run_id: DindRunId,
    network_name: String,
    core: DindMachine,
    edges: Vec<DindMachine>,
}

impl DindCluster {
    /// Creates the per-run network and boots every machine to readiness (core
    /// first). Runs use unique resources and clean up by run label, so one
    /// cluster never sweeps another cluster's resources.
    ///
    /// On failure the run's resources are swept again — unless
    /// [`super::keep_requested`] — so retries start clean.
    pub async fn provision(docker: &Docker, spec: DindClusterSpec) -> Result<Self, DindError> {
        let DindClusterSpec {
            artifact_dir,
            machines,
        } = spec;
        let (core_spec, edge_specs) = split_roles(machines)?;
        let run_id = DindRunId::generate();
        let network_name = format!("ployz-dind-{run_id}");
        let provisioned = provision_network_and_machines(
            docker,
            &run_id,
            &network_name,
            &artifact_dir,
            &core_spec,
            &edge_specs,
        )
        .await;
        match provisioned {
            Ok((core, edges)) => Ok(Self {
                docker: docker.clone(),
                run_id,
                network_name,
                core,
                edges,
            }),
            Err(error) => {
                if !super::keep_requested() {
                    // Best-effort failure-path sweep; the original error is
                    // the one worth reporting.
                    let _ = sweep_run_resources(docker, &run_id).await;
                }
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn run_id(&self) -> &DindRunId {
        &self.run_id
    }

    #[must_use]
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    #[must_use]
    pub fn core(&self) -> &DindMachine {
        &self.core
    }

    #[must_use]
    pub fn edges(&self) -> &[DindMachine] {
        &self.edges
    }

    /// Dumps journal, failed units, inner `docker ps -a`, and the NATS
    /// authorization file of every machine into
    /// `target/dind-evidence/<run_id>/`. Call this before failing a test.
    pub async fn capture_evidence(&self) -> Result<PathBuf, DindError> {
        capture_machine_evidence(&self.docker, &self.run_id, &self.core).await?;
        for edge in &self.edges {
            capture_machine_evidence(&self.docker, &self.run_id, edge).await?;
        }
        Ok(evidence_dir(&self.run_id))
    }

    /// Removes the run's containers (with their anonymous volumes) and the
    /// run network.
    pub async fn teardown(self) -> Result<(), DindError> {
        let Self {
            docker,
            run_id,
            network_name: _,
            core: _,
            edges: _,
        } = self;
        sweep_run_resources(&docker, &run_id).await
    }
}

async fn provision_network_and_machines(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &std::path::Path,
    core_spec: &MachineSpec,
    edge_specs: &[MachineSpec],
) -> Result<(DindMachine, Vec<DindMachine>), DindError> {
    create_cluster_network(docker, run_id, network_name).await?;
    let core_name = format!("ployz-dind-{run_id}-core");
    let core = provision_machine(
        docker,
        run_id,
        network_name,
        artifact_dir,
        core_spec,
        core_name,
    )
    .await?;
    let mut edges = Vec::with_capacity(edge_specs.len());
    for (index, edge_spec) in edge_specs.iter().enumerate() {
        let edge_name = format!("ployz-dind-{run_id}-edge-{}", index + 1);
        let edge = provision_machine(
            docker,
            run_id,
            network_name,
            artifact_dir,
            edge_spec,
            edge_name,
        )
        .await?;
        edges.push(edge);
    }
    Ok((core, edges))
}

fn split_roles(machines: Vec<MachineSpec>) -> Result<(MachineSpec, Vec<MachineSpec>), DindError> {
    let mut core = None;
    let mut edges = Vec::new();
    for spec in machines {
        match spec.role {
            DindMachineRole::Core => {
                if core.is_some() {
                    return Err(DindError::ClusterShape {
                        detail: String::from("more than one core machine requested"),
                    });
                }
                core = Some(spec);
            }
            DindMachineRole::Edge => edges.push(spec),
        }
    }
    let Some(core) = core else {
        return Err(DindError::ClusterShape {
            detail: String::from("no core machine requested"),
        });
    };
    Ok((core, edges))
}

async fn create_cluster_network(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
) -> Result<(), DindError> {
    let request = NetworkCreateRequest {
        name: network_name.to_owned(),
        driver: Some("bridge".to_owned()),
        // The managed-DNS path publishes only globally routable gateway
        // testimony. An isolated routable-looking subnet lets the harness
        // exercise that classification while Docker keeps all traffic local.
        ipam: Some(Ipam {
            config: Some(vec![IpamConfig {
                subnet: Some(cluster_subnet(run_id)),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        labels: Some(HashMap::from([
            (MANAGED_LABEL.to_owned(), MANAGED_LABEL_VALUE.to_owned()),
            (RUN_LABEL.to_owned(), run_id.as_str().to_owned()),
        ])),
        ..Default::default()
    };
    docker
        .create_network(request)
        .await
        .map_err(docker_api_error("create cluster network"))?;
    Ok(())
}

fn cluster_subnet(run_id: &DindRunId) -> String {
    let mut hasher = DefaultHasher::new();
    run_id.hash(&mut hasher);
    let value = hasher.finish();
    let second = (value >> 12) & 0xff;
    let third = (value >> 4) & 0xff;
    let fourth = (value & 0x0f) << 4;
    format!("11.{second}.{third}.{fourth}/28")
}

/// Removes every container, network, and volume carrying the
/// [`MANAGED_LABEL`] — stale leftovers from crashed runs included.
pub async fn sweep_managed_resources(docker: &Docker) -> Result<(), DindError> {
    sweep_by_label(docker, format!("{MANAGED_LABEL}={MANAGED_LABEL_VALUE}")).await
}

pub(super) async fn sweep_run_resources(
    docker: &Docker,
    run_id: &DindRunId,
) -> Result<(), DindError> {
    sweep_by_label(docker, format!("{RUN_LABEL}={}", run_id.as_str())).await
}

async fn sweep_by_label(docker: &Docker, label_filter: String) -> Result<(), DindError> {
    let filters = HashMap::from([("label".to_owned(), vec![label_filter])]);

    let containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .map_err(docker_api_error("list labeled containers"))?;
    for container in containers {
        let Some(id) = container.id else {
            continue;
        };
        remove_container(docker, &id).await?;
    }

    let networks = docker
        .list_networks(Some(
            ListNetworksOptionsBuilder::new().filters(&filters).build(),
        ))
        .await
        .map_err(docker_api_error("list labeled networks"))?;
    for network in networks {
        let Some(id) = network.id else {
            continue;
        };
        let remove = docker.remove_network(&id).await;
        ignore_not_found(remove).map_err(docker_api_error("remove labeled network"))?;
    }

    let volumes = docker
        .list_volumes(Some(
            ListVolumesOptionsBuilder::new().filters(&filters).build(),
        ))
        .await
        .map_err(docker_api_error("list labeled volumes"))?;
    for volume in volumes.volumes.unwrap_or_default() {
        let remove = docker
            .remove_volume(
                &volume.name,
                Some(RemoveVolumeOptionsBuilder::new().force(true).build()),
            )
            .await;
        ignore_not_found(remove).map_err(docker_api_error("remove labeled volume"))?;
    }

    Ok(())
}

async fn remove_container(docker: &Docker, id: &str) -> Result<(), DindError> {
    const ATTEMPTS: u32 = 5;

    for attempt in 0..ATTEMPTS {
        let remove = docker
            .remove_container(
                id,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await;
        match ignore_not_found(remove) {
            Ok(()) => return Ok(()),
            Err(_) if attempt + 1 < ATTEMPTS => {
                let delay_ms = 250_u64 << attempt;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(error) => return Err(docker_api_error("remove labeled container")(error)),
        }
    }
    unreachable!("bounded container removal loop returns on every final attempt")
}

/// Sweeps race against auto-removal and against each other; a resource that
/// is already gone is a success for cleanup purposes.
fn ignore_not_found(result: Result<(), BollardError>) -> Result<(), BollardError> {
    match result {
        Ok(()) => Ok(()),
        Err(BollardError::DockerResponseServerError {
            status_code: 404,
            message: _,
        }) => Ok(()),
        Err(other) => Err(other),
    }
}
