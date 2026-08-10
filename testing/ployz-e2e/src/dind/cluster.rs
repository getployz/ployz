//! Per-run Docker network, machine provisioning, and label-scoped cleanup.

use super::evidence::{capture_machine_evidence, evidence_dir};
use super::machine::{DindMachine, MachineSpec, provision_machine};
use super::{DindError, MANAGED_LABEL, MANAGED_LABEL_VALUE, RUN_LABEL, docker_api_error};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::models::NetworkCreateRequest;
use bollard::query_parameters::{
    ListContainersOptionsBuilder, ListNetworksOptionsBuilder, ListVolumesOptionsBuilder,
    RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
};
use futures_util::future::try_join_all;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Unique identifier of one harness run and the cleanup label on its resources.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DindRunId(String);

impl DindRunId {
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
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

/// Requested role-neutral machine set on one isolated Docker network.
#[derive(Debug, Clone)]
pub struct DindClusterSpec {
    /// Host directory mounted read-only in every machine.
    pub artifact_dir: PathBuf,
    pub machines: Vec<MachineSpec>,
}

/// One provisioned run: a labeled bridge network plus ready machines.
///
/// Cleanup is explicit so a failed test can capture evidence before removing
/// its resources.
#[derive(Debug)]
pub struct DindCluster {
    docker: Docker,
    run_id: DindRunId,
    network_name: String,
    machines: Vec<DindMachine>,
}

impl DindCluster {
    /// Creates a per-run network and starts each requested machine in order.
    pub async fn provision(docker: &Docker, spec: DindClusterSpec) -> Result<Self, DindError> {
        let DindClusterSpec {
            artifact_dir,
            machines: machine_specs,
        } = spec;
        let run_id = DindRunId::generate();
        let network_name = format!("ployz-dind-{run_id}");
        let provisioned = provision_network_and_machines(
            docker,
            &run_id,
            &network_name,
            &artifact_dir,
            &machine_specs,
        )
        .await;
        match provisioned {
            Ok(machines) => Ok(Self {
                docker: docker.clone(),
                run_id,
                network_name,
                machines,
            }),
            Err(error) => {
                if !super::keep_requested() {
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
    pub fn machines(&self) -> &[DindMachine] {
        &self.machines
    }

    /// Captures generic host and inner-Docker evidence for every machine.
    pub async fn capture_evidence(&self) -> Result<PathBuf, DindError> {
        for machine in &self.machines {
            capture_machine_evidence(&self.docker, &self.run_id, machine).await?;
        }
        Ok(evidence_dir(&self.run_id))
    }

    /// Removes every container, anonymous volume, and network from this run.
    pub async fn teardown(self) -> Result<(), DindError> {
        let Self {
            docker,
            run_id,
            network_name: _,
            machines: _,
        } = self;
        sweep_run_resources(&docker, &run_id).await
    }
}

async fn provision_network_and_machines(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
    artifact_dir: &std::path::Path,
    machine_specs: &[MachineSpec],
) -> Result<Vec<DindMachine>, DindError> {
    create_network(docker, run_id, network_name).await?;
    try_join_all(machine_specs.iter().enumerate().map(|(index, spec)| {
        let name = format!("ployz-dind-{run_id}-machine-{}", index + 1);
        provision_machine(docker, run_id, network_name, artifact_dir, spec, name)
    }))
    .await
}

async fn create_network(
    docker: &Docker,
    run_id: &DindRunId,
    network_name: &str,
) -> Result<(), DindError> {
    let request = NetworkCreateRequest {
        name: network_name.to_owned(),
        driver: Some("bridge".to_owned()),
        labels: Some(HashMap::from([
            (MANAGED_LABEL.to_owned(), MANAGED_LABEL_VALUE.to_owned()),
            (RUN_LABEL.to_owned(), run_id.as_str().to_owned()),
        ])),
        ..Default::default()
    };
    docker
        .create_network(request)
        .await
        .map_err(docker_api_error("create run network"))?;
    Ok(())
}

/// Removes every container, network, and volume carrying the managed label.
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

#[cfg(test)]
mod tests {
    use super::DindRunId;

    #[test]
    fn generated_run_ids_are_uuid_simple_strings() {
        let run_id = DindRunId::generate();

        assert_eq!(run_id.as_str().len(), 32);
        assert!(run_id.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
