use std::collections::BTreeMap;

use ployz_core::{
    DockerVolumeId, DockerVolumeName, RequestedServiceSpec, ServiceId, ServiceMode, ServiceVolume,
    VolumeSource,
};
use thiserror::Error;

use crate::deploy::{
    DeployOperation, DeployPlan, DeploySnapshot, ObservedDockerVolume, PlanError, PlanOptions,
    plan_deploy, volume_eligible_machine_ids,
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
    #[error(
        "Docker Volume {name} cannot be shared by global service '{global}' and replicated service '{replicated}'"
    )]
    MixedVolumeModes {
        name: DockerVolumeName,
        global: String,
        replicated: String,
    },
}

pub fn plan_compose_deploy(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
    options: PlanOptions,
) -> Result<ComposeDeployPlan, ComposePlanError> {
    let volume_uses = named_volume_uses(project);
    reject_mixed_volume_modes(&volume_uses)?;
    let mut resolved = project.clone();
    resolved.resolve_secrets()?;
    let mut effective_snapshot = snapshot.clone();
    let mut volume_operations =
        prepare_shared_replicated_volumes(&volume_uses, &mut effective_snapshot, options)?;
    let mut service_plans = Vec::new();
    for requested in project.dependency_order()? {
        let service = requested.name.to_string();
        let service_id = ServiceId::random();
        let resolved_requested = resolved
            .services
            .get(&service)
            .ok_or_else(|| ComposeError::Invalid(format!("undefined service '{service}'")))?;
        let plan = plan_deploy(resolved_requested, &effective_snapshot, service_id, options)
            .map_err(|source| ComposePlanError::Service {
                service: service.clone(),
                source,
            })?;
        let DeployPlan {
            service_id,
            is_new_service,
            operation,
        } = plan;
        let DeployOperation::Sequence { operations } = operation else {
            unreachable!("plan_deploy always returns a sequence")
        };
        let mut service_operations = Vec::new();
        for operation in operations {
            if let DeployOperation::CreateVolume { machine_id, volume } = &operation {
                remember_volume(&mut effective_snapshot, machine_id, volume);
                volume_operations.push(operation);
            } else {
                service_operations.push(operation);
            }
        }
        // TODO(UT-088): depends_on conditions are not represented as first operations.
        service_plans.push(DeployPlan {
            service_id,
            is_new_service,
            operation: DeployOperation::Sequence {
                operations: service_operations,
            },
        });
    }
    Ok(ComposeDeployPlan {
        volume_operations,
        service_plans,
    })
}

#[derive(Clone, Copy)]
struct NamedVolumeUse<'a> {
    service_name: &'a str,
    service: &'a RequestedServiceSpec,
    volume: &'a ServiceVolume,
    global: bool,
}

fn named_volume_uses(
    project: &ComposeProject,
) -> BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>> {
    let mut uses = BTreeMap::<DockerVolumeName, Vec<NamedVolumeUse<'_>>>::new();
    for (service_name, service) in &project.services {
        for mount in &service.mounts {
            let Some(volume) = service
                .volumes
                .iter()
                .find(|volume| volume.reference == mount.volume)
            else {
                continue;
            };
            let VolumeSource::Named { name, .. } = &volume.source else {
                continue;
            };
            let uses = uses.entry(name.clone()).or_default();
            if !uses
                .iter()
                .any(|volume_use| volume_use.service_name == service_name)
            {
                uses.push(NamedVolumeUse {
                    service_name,
                    service,
                    volume,
                    global: matches!(service.mode, ServiceMode::Global),
                });
            }
        }
    }
    uses
}

fn prepare_shared_replicated_volumes(
    volume_uses: &BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>>,
    snapshot: &mut DeploySnapshot,
    options: PlanOptions,
) -> Result<Vec<DeployOperation>, ComposePlanError> {
    let mut operations = Vec::new();
    let mut remaining = volume_uses
        .iter()
        .filter(|(_, uses)| uses.len() > 1 && uses.iter().all(|volume_use| !volume_use.global))
        .collect::<Vec<_>>();
    while !remaining.is_empty() {
        let mut component = vec![remaining.remove(0)];
        while let Some(index) = remaining.iter().position(|(_, candidate_uses)| {
            candidate_uses.iter().any(|candidate| {
                component.iter().any(|(_, component_uses)| {
                    component_uses
                        .iter()
                        .any(|volume_use| volume_use.service_name == candidate.service_name)
                })
            })
        }) {
            component.push(remaining.remove(index));
        }
        let services = component
            .iter()
            .flat_map(|(_, uses)| uses.iter())
            .map(|volume_use| (volume_use.service_name, volume_use.service))
            .collect::<BTreeMap<_, _>>();
        let mut services = services.into_iter();
        let (first_service_name, first_service) = services
            .next()
            .expect("shared Volume component has at least two services");
        let mut eligible =
            volume_eligible_machine_ids(first_service, snapshot, options).map_err(|source| {
                ComposePlanError::Service {
                    service: first_service_name.into(),
                    source,
                }
            })?;
        for (service_name, service) in services {
            let other_eligible =
                volume_eligible_machine_ids(service, snapshot, options).map_err(|source| {
                    ComposePlanError::Service {
                        service: service_name.into(),
                        source,
                    }
                })?;
            eligible.retain(|machine_id| other_eligible.contains(machine_id));
        }
        if eligible.is_empty() {
            return Err(ComposePlanError::Service {
                service: first_service_name.into(),
                source: PlanError::NoEligibleMachines,
            });
        }
        let machine_id = eligible.remove(0);
        for (name, uses) in component {
            snapshot
                .volumes
                .retain(|volume| volume.id.name != *name || volume.id.machine_id == machine_id);
            if !snapshot
                .volumes
                .iter()
                .any(|volume| volume.id.machine_id == machine_id && volume.id.name == *name)
            {
                let first_use = uses.first().expect("shared Volume has at least two uses");
                let operation = DeployOperation::CreateVolume {
                    machine_id: machine_id.clone(),
                    volume: first_use.volume.clone(),
                };
                remember_volume(snapshot, &machine_id, first_use.volume);
                operations.push(operation);
            }
        }
    }
    Ok(operations)
}

fn reject_mixed_volume_modes(
    volume_uses: &BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>>,
) -> Result<(), ComposePlanError> {
    for (name, uses) in volume_uses {
        if let (Some(global), Some(replicated)) = (
            uses.iter().find(|volume_use| volume_use.global),
            uses.iter().find(|volume_use| !volume_use.global),
        ) {
            return Err(ComposePlanError::MixedVolumeModes {
                name: name.clone(),
                global: global.service_name.into(),
                replicated: replicated.service_name.into(),
            });
        }
    }
    Ok(())
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
