use ployz_core::{DockerVolumeId, ServiceId, VolumeSource};
use thiserror::Error;

use crate::deploy::{
    DeployOperation, DeployPlan, DeploySnapshot, ObservedDockerVolume, PlanError, PlanOptions,
    plan_deploy, plan_volume_operations,
};

use super::{ComposeError, ComposeProject};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposeDeployPlan {
    pub volume_operations: Vec<DeployOperation>,
    pub service_plans: Vec<DeployPlan>,
}

impl ComposeDeployPlan {
    #[must_use]
    pub fn operations(&self) -> Vec<&DeployOperation> {
        self.volume_operations
            .iter()
            .chain(self.service_plans.iter().flat_map(|plan| plan.operations()))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ComposePlanError {
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error("plan service '{service}': {source}")]
    Service { service: String, source: PlanError },
}

pub fn plan_compose_deploy(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
    options: PlanOptions,
) -> Result<ComposeDeployPlan, ComposePlanError> {
    let mut resolved = project.clone();
    resolved.resolve_secrets()?;
    let mut effective_snapshot = snapshot.clone();
    let mut volume_operations = Vec::new();
    let mut service_plans = Vec::new();
    for requested in project.dependency_order()? {
        let service = requested.name.to_string();
        let service_id = ServiceId::random();
        // TODO(UT-089): volume scheduling intentionally uses unresolved Requested Service Specs.
        let planned_volumes = plan_volume_operations(requested, &effective_snapshot, options)
            .map_err(|source| ComposePlanError::Service {
                service: service.clone(),
                source,
            })?;
        for operation in &planned_volumes {
            if let DeployOperation::CreateVolume { machine_id, volume } = operation {
                remember_volume(&mut effective_snapshot, machine_id, volume);
                volume_operations.push(operation.clone());
            }
        }
        let resolved_requested = resolved
            .services
            .get(&service)
            .ok_or_else(|| ComposeError::Invalid(format!("undefined service '{service}'")))?;
        let plan = plan_deploy(resolved_requested, &effective_snapshot, service_id, options)
            .map_err(|source| ComposePlanError::Service {
                service: service.clone(),
                source,
            })?;
        // TODO(UT-088): depends_on conditions are not represented as first operations.
        service_plans.push(plan);
    }
    Ok(ComposeDeployPlan {
        volume_operations,
        service_plans,
    })
}

fn remember_volume(
    snapshot: &mut DeploySnapshot,
    machine_id: &ployz_core::MachineId,
    volume: &ployz_core::ServiceVolume,
) {
    let VolumeSource::Named { name, driver, .. } = &volume.source else {
        return;
    };
    snapshot.volumes.push(ObservedDockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id.clone(),
            name: name.clone(),
        },
        driver: driver
            .as_ref()
            .map_or_else(|| "local".into(), |driver| driver.name.clone()),
        options: driver
            .as_ref()
            .map_or_else(Default::default, |driver| driver.options.clone()),
    });
}
