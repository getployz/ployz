use crate::error::{Error, Result};
use crate::machine_policy::{can_keep_existing_slot, is_new_placement_candidate};
use crate::model::{
    DeployChangeKind, DeployPreview, MachineId, MachineMembership, ServicePlan, ServiceReleaseRecord,
    ServiceReleaseSlot, SlotId, SlotPlan,
};
use ployz_store_api::{DeployStore, MachineStore, StoreDriver};
use ployz_types::spec::{DeployManifest, Namespace, Placement, ServiceSpec, stable_hash_hex};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedSlot {
    pub(super) slot_id: SlotId,
    pub(super) machine_id: MachineId,
    pub(super) current: Option<ServiceReleaseSlot>,
    pub(super) action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedService {
    pub(super) service: String,
    pub(super) phase: Option<u32>,
    pub(super) spec: Option<ServiceSpec>,
    pub(super) spec_json: Option<String>,
    pub(super) current_revision_hash: Option<String>,
    pub(super) next_revision_hash: Option<String>,
    pub(super) slots: Vec<PlannedSlot>,
    pub(super) action: DeployChangeKind,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedPlan {
    namespace: Namespace,
    manifest_hash: String,
    participants: BTreeSet<MachineId>,
    services: Vec<PlannedService>,
    machine_map: HashMap<MachineId, MachineMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanFingerprint {
    pub(super) namespace: Namespace,
    pub(super) manifest_hash: String,
    pub(super) participants: Vec<MachineId>,
    pub(super) services: Vec<PlannedService>,
}

#[derive(Debug, Clone)]
pub(super) struct DesiredSlot {
    pub(super) slot_id: SlotId,
    pub(super) machine_id: MachineId,
}

impl ResolvedPlan {
    pub(super) fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub(super) fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }

    pub(super) fn participants(&self) -> &BTreeSet<MachineId> {
        &self.participants
    }

    pub(super) fn services(&self) -> &[PlannedService] {
        &self.services
    }

    #[cfg(test)]
    pub(super) fn services_mut(&mut self) -> &mut [PlannedService] {
        &mut self.services
    }

    pub(super) fn machine_map(&self) -> &HashMap<MachineId, MachineMembership> {
        &self.machine_map
    }

    pub(super) fn fingerprint(&self) -> PlanFingerprint {
        PlanFingerprint {
            namespace: self.namespace.clone(),
            manifest_hash: self.manifest_hash.clone(),
            participants: self.participants.iter().cloned().collect(),
            services: self.services.clone(),
        }
    }

    pub(super) fn to_preview(&self, warnings: Vec<String>) -> DeployPreview {
        DeployPreview {
            namespace: self.namespace.clone(),
            manifest_hash: self.manifest_hash.clone(),
            participants: self.participants.iter().cloned().collect(),
            services: self
                .services
                .iter()
                .map(|service| ServicePlan {
                    service: service.service.clone(),
                    current_revision_hash: service.current_revision_hash.clone(),
                    next_revision_hash: service.next_revision_hash.clone(),
                    slots: service
                        .slots
                        .iter()
                        .map(|slot| SlotPlan {
                            slot_id: slot.slot_id.clone(),
                            machine_id: slot.machine_id.clone(),
                            current_instance_id: slot
                                .current
                                .as_ref()
                                .map(|current| current.active_instance_id.clone()),
                            next_instance_id: None,
                            current_revision_hash: slot
                                .current
                                .as_ref()
                                .map(|current| current.revision_hash.clone()),
                            next_revision_hash: match slot.action {
                                DeployChangeKind::Remove => None,
                                DeployChangeKind::Create
                                | DeployChangeKind::Replace
                                | DeployChangeKind::Unchanged => service.next_revision_hash.clone(),
                            },
                            action: slot.action,
                        })
                        .collect(),
                    action: service.action,
                })
                .collect(),
            warnings,
        }
    }
}

impl PlannedService {
    pub(super) fn phase(&self) -> Option<u32> {
        self.phase
    }

    pub(super) fn next_revision_hash(&self) -> Option<&str> {
        self.next_revision_hash.as_deref()
    }

    pub(super) fn spec_json(&self) -> Option<&str> {
        self.spec_json.as_deref()
    }
}

pub(super) async fn resolve_plan(
    store: &StoreDriver,
    local_machine_id: &MachineId,
    manifest: &DeployManifest,
) -> Result<ResolvedPlan> {
    manifest
        .validate()
        .map_err(|error| Error::operation("deploy_preview", error))?;

    let current_releases = store.list_service_releases(&manifest.namespace).await?;
    let machines = store.list_machines().await?;
    let machine_map: HashMap<MachineId, MachineMembership> = machines
        .iter()
        .map(|machine| (machine.id.clone(), machine.clone()))
        .collect();
    let desired_machines = deployable_machines(&machines, local_machine_id);
    let current_slots_by_service = current_slots_by_service(&current_releases);
    let current_release_map: HashMap<String, ServiceReleaseRecord> = current_releases
        .iter()
        .map(|release| (release.service.clone(), release.clone()))
        .collect();
    let manifest_hash = stable_hash_hex(
        serde_json::to_vec(manifest)
            .map_err(|error| {
                Error::operation("deploy_preview", format!("serialize manifest: {error}"))
            })?
            .as_slice(),
    );

    let mut participants = BTreeSet::new();
    let mut services = Vec::new();

    for spec in &manifest.services {
        let revision_hash = spec
            .revision_hash()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let spec_json = spec
            .canonical_revision_json()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let desired_slots = desired_slots(
            spec,
            &desired_machines,
            current_slots_by_service.get(&spec.name).map(Vec::as_slice),
            &machine_map,
        )?;
        let current_release = current_release_map.get(&spec.name);
        let current_service_slots = current_slots_by_service
            .get(&spec.name)
            .cloned()
            .unwrap_or_default();

        let mut current_slots_by_id: HashMap<String, ServiceReleaseSlot> = current_service_slots
            .iter()
            .cloned()
            .map(|slot| (slot.slot_id.0.clone(), slot))
            .collect();
        let mut slots = Vec::new();

        for desired_slot in &desired_slots {
            participants.insert(desired_slot.machine_id.clone());
            let current = current_slots_by_id.remove(&desired_slot.slot_id.0);
            if let Some(current_slot) = &current {
                participants.insert(current_slot.machine_id.clone());
            }
            let action = match &current {
                Some(slot)
                    if slot.machine_id == desired_slot.machine_id
                        && slot.revision_hash == revision_hash =>
                {
                    DeployChangeKind::Unchanged
                }
                Some(_) => DeployChangeKind::Replace,
                None => DeployChangeKind::Create,
            };
            slots.push(PlannedSlot {
                slot_id: desired_slot.slot_id.clone(),
                machine_id: desired_slot.machine_id.clone(),
                current,
                action,
            });
        }

        let mut extra_current_slots = current_slots_by_id.into_values().collect::<Vec<_>>();
        extra_current_slots.sort_by(|left, right| left.slot_id.0.cmp(&right.slot_id.0));
        for current_slot in extra_current_slots {
            participants.insert(current_slot.machine_id.clone());
            slots.push(PlannedSlot {
                slot_id: current_slot.slot_id.clone(),
                machine_id: current_slot.machine_id.clone(),
                current: Some(current_slot),
                action: DeployChangeKind::Remove,
            });
        }

        slots.sort_by(|left, right| left.slot_id.0.cmp(&right.slot_id.0));

        let action = if current_release.is_none() {
            DeployChangeKind::Create
        } else if slots
            .iter()
            .all(|slot| slot.action == DeployChangeKind::Unchanged)
            && current_release.map(|release| release.release.primary_revision_hash.as_str())
                == Some(revision_hash.as_str())
        {
            DeployChangeKind::Unchanged
        } else {
            DeployChangeKind::Replace
        };

        services.push(PlannedService {
            service: spec.name.clone(),
            phase: Some(0),
            spec: Some(spec.clone()),
            spec_json: Some(spec_json),
            current_revision_hash: current_release
                .map(|release| release.release.primary_revision_hash.clone()),
            next_revision_hash: Some(revision_hash),
            slots,
            action,
        });
    }

    let manifest_service_names = manifest
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut removed_releases = current_releases
        .iter()
        .filter(|release| !manifest_service_names.contains(release.service.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    removed_releases.sort_by(|left, right| left.service.cmp(&right.service));

    for release in removed_releases {
        let mut slots = release
            .release
            .slots
            .iter()
            .cloned()
            .map(|slot| {
                participants.insert(slot.machine_id.clone());
                PlannedSlot {
                    slot_id: slot.slot_id.clone(),
                    machine_id: slot.machine_id.clone(),
                    current: Some(slot),
                    action: DeployChangeKind::Remove,
                }
            })
            .collect::<Vec<_>>();
        slots.sort_by(|left, right| left.slot_id.0.cmp(&right.slot_id.0));
        services.push(PlannedService {
            service: release.service.clone(),
            phase: None,
            spec: None,
            spec_json: None,
            current_revision_hash: Some(release.release.primary_revision_hash.clone()),
            next_revision_hash: None,
            slots,
            action: DeployChangeKind::Remove,
        });
    }

    Ok(ResolvedPlan {
        namespace: manifest.namespace.clone(),
        manifest_hash,
        participants,
        services,
        machine_map,
    })
}

pub(super) fn deployable_machines(
    machines: &[MachineMembership],
    local_machine_id: &MachineId,
) -> Vec<MachineId> {
    let mut enabled: Vec<MachineId> = machines
        .iter()
        .filter(|machine| is_new_placement_candidate(&machine.placement_candidate()))
        .map(|machine| machine.id.clone())
        .collect();
    enabled.sort_by(|left, right| left.0.cmp(&right.0));
    if enabled.is_empty() {
        return vec![local_machine_id.clone()];
    }
    enabled
}

#[allow(clippy::indexing_slicing)]
pub(super) fn desired_slots(
    spec: &ServiceSpec,
    machines: &[MachineId],
    current_slots: Option<&[ServiceReleaseSlot]>,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Result<Vec<DesiredSlot>> {
    let candidates = if machines.is_empty() {
        vec![MachineId("local".into())]
    } else {
        machines.to_vec()
    };

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
                let machine_id = current_slots
                    .and_then(|slots| {
                        slots
                            .iter()
                            .find(|slot| slot.slot_id == slot_id)
                            .map(|slot| slot.machine_id.clone())
                    })
                    .filter(|machine_id| {
                        machine_map
                            .get(machine_id)
                            .is_some_and(|record| can_keep_existing_slot(&record.placement_candidate()))
                    })
                    .unwrap_or_else(|| candidates[usize::from(index) % candidates.len()].clone());
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
    current_releases: &[ServiceReleaseRecord],
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|release| (release.service.clone(), release.release.slots.clone()))
        .collect()
}
