use crate::error::{Error, Result};
use crate::model::{
    DeployChangeKind, DeployPreview, MachineId, ServicePlan, ServiceReleaseRecord,
    ServiceReleaseSlot, SlotId, SlotPlan,
};
use ployz_store_api::{DeployReadStore, MachineStore};
use ployz_types::spec::{DeployManifest, Placement, ServiceSpec, stable_hash_hex};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone)]
pub(crate) struct DesiredSlot {
    pub(crate) slot_id: SlotId,
    pub(crate) machine_id: MachineId,
}

pub async fn preview(
    deploy_read: &dyn DeployReadStore,
    machine_store: &dyn MachineStore,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<DeployPreview> {
    manifest
        .validate()
        .map_err(|error| Error::operation("deploy_preview", error))?;
    let namespace = &manifest.namespace;

    let current_releases = deploy_read.list_service_releases(namespace).await?;
    let machines = machine_store.list_machines().await?;
    let desired_machines = deployable_machines(&machines, local_machine_id);
    let current_release_map: HashMap<String, ServiceReleaseRecord> = current_releases
        .into_iter()
        .map(|record| (record.service.clone(), record))
        .collect();
    let current_slots_by_service = current_slots_by_service(&current_release_map);

    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(manifest)
            .map_err(|error| {
                Error::operation("deploy_preview", format!("serialize manifest: {error}"))
            })?
            .as_slice(),
    );

    let mut participants = BTreeSet::new();
    for machine_id in &desired_machines {
        participants.insert(machine_id.clone());
    }

    let mut services = Vec::new();
    for spec in &manifest.services {
        let revision_hash = spec
            .revision_hash()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let desired_slots = desired_slots(
            spec,
            &desired_machines,
            current_slots_by_service.get(&spec.name).map(Vec::as_slice),
        )?;
        let current_service_slots = current_slots_by_service
            .get(&spec.name)
            .cloned()
            .unwrap_or_default();
        let current_release = current_release_map.get(&spec.name);

        let mut slot_plans = Vec::new();
        for desired_slot in desired_slots {
            participants.insert(desired_slot.machine_id.clone());
            let current_slot = current_service_slots
                .iter()
                .find(|slot| slot.slot_id == desired_slot.slot_id);
            let action = match current_slot {
                Some(slot)
                    if slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash =>
                {
                    DeployChangeKind::Unchanged
                }
                Some(_) => DeployChangeKind::Replace,
                None => DeployChangeKind::Create,
            };
            slot_plans.push(SlotPlan {
                slot_id: desired_slot.slot_id,
                machine_id: desired_slot.machine_id,
                current_instance_id: current_slot.map(|slot| slot.active_instance_id.clone()),
                next_instance_id: None,
                current_revision_hash: current_slot.map(|slot| slot.revision_hash.clone()),
                next_revision_hash: Some(revision_hash.clone()),
                action,
            });
        }

        for slot in &current_service_slots {
            participants.insert(slot.machine_id.clone());
        }

        let action = if slot_plans
            .iter()
            .all(|plan| plan.action == DeployChangeKind::Unchanged)
            && current_release.map(|release| release.release.primary_revision_hash.as_str())
                == Some(revision_hash.as_str())
        {
            DeployChangeKind::Unchanged
        } else if current_release.is_none() {
            DeployChangeKind::Create
        } else {
            DeployChangeKind::Replace
        };

        services.push(ServicePlan {
            service: spec.name.clone(),
            current_revision_hash: current_release
                .map(|release| release.release.primary_revision_hash.clone()),
            next_revision_hash: Some(revision_hash),
            slots: slot_plans,
            action,
        });
    }

    for (service, slots) in current_slots_by_service {
        if manifest.services.iter().any(|spec| spec.name == service) {
            continue;
        }
        for slot in &slots {
            participants.insert(slot.machine_id.clone());
        }
        services.push(ServicePlan {
            service: service.clone(),
            current_revision_hash: current_release_map
                .get(&service)
                .map(|release| release.release.primary_revision_hash.clone()),
            next_revision_hash: None,
            slots: slots
                .into_iter()
                .map(|slot| SlotPlan {
                    slot_id: slot.slot_id,
                    machine_id: slot.machine_id,
                    current_instance_id: Some(slot.active_instance_id),
                    next_instance_id: None,
                    current_revision_hash: Some(slot.revision_hash),
                    next_revision_hash: None,
                    action: DeployChangeKind::Remove,
                })
                .collect(),
            action: DeployChangeKind::Remove,
        });
    }

    Ok(DeployPreview {
        namespace: namespace.clone(),
        manifest_hash,
        participants: participants.into_iter().collect(),
        services,
        warnings: Vec::new(),
    })
}

pub(crate) fn deployable_machines(
    machines: &[crate::model::MachineRecord],
    _local_machine_id: &MachineId,
) -> Vec<MachineId> {
    let mut candidates: Vec<MachineId> =
        machines.iter().map(|machine| machine.id.clone()).collect();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates
}

pub(crate) fn desired_slots(
    spec: &ServiceSpec,
    machines: &[MachineId],
    current_slots: Option<&[ServiceReleaseSlot]>,
) -> Result<Vec<DesiredSlot>> {
    let candidates = machines.to_vec();
    if candidates.is_empty() {
        return Err(Error::operation(
            "desired_slots",
            "slot placement requires at least one candidate machine",
        ));
    }

    let mut desired = Vec::new();
    match spec.placement {
        Placement::Replicated { count } => {
            if count == 0 {
                return Err(Error::operation(
                    "desired_slots",
                    format!("service '{}' requested zero replicas", spec.name),
                ));
            }
            for index in 0..count {
                let slot_id = SlotId(format!("slot-{number:04}", number = usize::from(index) + 1));
                let machine_id = if let Some(machine_id) = current_slots.and_then(|slots| {
                    slots
                        .iter()
                        .find(|slot| slot.slot_id == slot_id)
                        .map(|slot| slot.machine_id.clone())
                }) {
                    machine_id
                } else {
                    let candidate_index = usize::from(index) % candidates.len();
                    let Some(machine_id) = candidates.get(candidate_index) else {
                        return Err(Error::operation(
                            "desired_slots",
                            format!("candidate index {candidate_index} out of bounds"),
                        ));
                    };
                    machine_id.clone()
                };
                desired.push(DesiredSlot {
                    slot_id,
                    machine_id,
                });
            }
        }
        Placement::Global => {
            for machine_id in &candidates {
                desired.push(DesiredSlot {
                    slot_id: SlotId(format!("slot-{}", machine_id.0)),
                    machine_id: machine_id.clone(),
                });
            }
        }
    }
    Ok(desired)
}

fn current_slots_by_service(
    current_releases: &HashMap<String, ServiceReleaseRecord>,
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|(service, release)| (service.clone(), release.release.slots.clone()))
        .collect()
}
