use crate::error::{DeployError, Error, Result};
use crate::machine_policy::{can_keep_existing_slot, is_new_placement_candidate};
use crate::model::{
    DeployChangeKind, DeployId, DeployPreview, MachineId, MachineMembership,
    ServiceBranchLineageRecord, ServiceBranchSourcePlan, ServicePlan, ServiceReleaseRecord,
    ServiceReleaseSlot, SlotId, SlotPlan, VolumeRecord,
};
use ployz_store_api::{DeployStore, MachineMembershipStore, StoreDriver};
use ployz_types::spec::{
    DeployManifest, MountSource, Namespace, Placement, ServiceIntent, ServiceSpec,
    VolumeDeclaration, parse_quota_bytes, stable_hash_hex,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    pub(super) branch_source: Option<PlannedBranchSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedBranchSource {
    pub(super) source_namespace: Namespace,
    pub(super) source_service: String,
    pub(super) source_revision_hash: String,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedPlan {
    namespace: Namespace,
    manifest_hash: String,
    volumes_json: String,
    volumes: Vec<PlannedVolume>,
    participants: BTreeSet<MachineId>,
    services: Vec<PlannedService>,
    machine_map: HashMap<MachineId, MachineMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedVolume {
    pub(super) declaration: VolumeDeclaration,
    pub(super) machine_id: MachineId,
    pub(super) attached_services: Vec<String>,
    pub(super) current: Option<VolumeRecord>,
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

    pub(super) fn volumes_json(&self) -> &str {
        &self.volumes_json
    }

    pub(super) fn volumes(&self) -> &[PlannedVolume] {
        &self.volumes
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
            service_branch_sources: self
                .services
                .iter()
                .filter_map(|service| {
                    let branch_source = service.branch_source.as_ref()?;
                    Some(ServiceBranchSourcePlan {
                        service: service.service.clone(),
                        source_namespace: branch_source.source_namespace.clone(),
                        source_service: branch_source.source_service.clone(),
                        source_revision_hash: branch_source.source_revision_hash.clone(),
                    })
                })
                .collect(),
            warnings,
        }
    }

    pub(super) fn service_branch_lineage_records(
        &self,
        deploy_id: &DeployId,
        created_at: u64,
    ) -> Vec<ServiceBranchLineageRecord> {
        self.services
            .iter()
            .filter_map(|service| {
                let branch_source = service.branch_source.as_ref()?;
                let revision_hash = service.next_revision_hash.as_ref()?;
                Some(ServiceBranchLineageRecord {
                    namespace: self.namespace.clone(),
                    service: service.service.clone(),
                    revision_hash: revision_hash.clone(),
                    source_namespace: branch_source.source_namespace.clone(),
                    source_service: branch_source.source_service.clone(),
                    source_revision_hash: branch_source.source_revision_hash.clone(),
                    deploy_id: deploy_id.clone(),
                    created_at,
                })
            })
            .collect()
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
        .map_err(|message| Error::Deploy(DeployError::ManifestInvalid { message }))?;

    let current_releases = store.list_deploy_releases(&manifest.namespace).await?;
    let current_volumes = store.list_volumes(&manifest.namespace).await?;
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
    let volumes_json = serde_json::to_string(&manifest.volumes).map_err(|error| {
        Error::operation("deploy_preview", format!("serialize volumes: {error}"))
    })?;

    let service_volume_refs = service_volume_refs(manifest);
    let volume_attachments = volume_attachments(manifest);
    let volume_map = current_volumes
        .into_iter()
        .map(|volume| (volume.volume_name.clone(), volume))
        .collect::<HashMap<_, _>>();

    let mut participants = BTreeSet::new();
    let mut services = Vec::new();
    let mut planned_volumes = Vec::new();
    let mut volume_machine_map = HashMap::new();
    let branch_intents = branch_intents(manifest);

    for declaration in &manifest.volumes {
        let attached_services = volume_attachments
            .get(&declaration.name)
            .cloned()
            .unwrap_or_default();
        let machine_id = match volume_map.get(&declaration.name) {
            Some(record) => {
                validate_existing_volume(declaration, record)?;
                if !machine_can_keep_existing_work(
                    &record.machine_id,
                    &machine_map,
                    local_machine_id,
                ) {
                    return Err(Error::Deploy(
                        DeployError::VolumeBoundToUnavailableMachine {
                            volume: declaration.name.clone(),
                            machine_id: record.machine_id.0.clone(),
                        },
                    ));
                }
                record.machine_id.clone()
            }
            None => new_volume_machine(
                &attached_services,
                &current_slots_by_service,
                &machine_map,
                &desired_machines,
                local_machine_id,
            )?,
        };
        volume_machine_map.insert(declaration.name.clone(), machine_id.clone());
        planned_volumes.push(PlannedVolume {
            declaration: declaration.clone(),
            machine_id,
            attached_services,
            current: volume_map.get(&declaration.name).cloned(),
        });
    }
    let services_with_volume_changes = planned_volumes
        .iter()
        .filter(|volume| !matches!(volume_record_change(volume), VolumeChange::Skip))
        .flat_map(|volume| volume.attached_services.iter().cloned())
        .collect::<BTreeSet<_>>();

    for spec in &manifest.services {
        let branch_source = if let Some(intent) = branch_intents.get(&spec.name) {
            Some(resolve_branch_source(store, &manifest.namespace, &spec.name, intent).await?)
        } else {
            None
        };
        let revision_hash = spec
            .revision_hash()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let spec_json = spec
            .canonical_revision_json()
            .map_err(|error| Error::operation("deploy_preview", error))?;
        let volume_pin = service_volume_pin(&spec.name, &service_volume_refs, &volume_machine_map)?;
        let desired_slots = desired_slots(
            spec,
            &desired_machines,
            current_slots_by_service.get(&spec.name).map(Vec::as_slice),
            &machine_map,
            volume_pin.as_ref(),
            &revision_hash,
            services_with_volume_changes.contains(&spec.name),
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
                        && slot.revision_hash == revision_hash
                        && !services_with_volume_changes.contains(&spec.name) =>
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
            branch_source,
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
            branch_source: None,
        });
    }

    Ok(ResolvedPlan {
        namespace: manifest.namespace.clone(),
        manifest_hash,
        volumes_json,
        volumes: planned_volumes,
        participants,
        services,
        machine_map,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchIntent {
    source_namespace: Namespace,
    source_service: String,
}

fn branch_intents(manifest: &DeployManifest) -> HashMap<String, BranchIntent> {
    let mut intents = HashMap::new();
    let Some(deploy_intent) = &manifest.intent else {
        return intents;
    };

    for hint in &deploy_intent.services {
        match &hint.intent {
            ServiceIntent::Branch {
                source_namespace,
                source_service,
            } => {
                intents.insert(
                    hint.service.clone(),
                    BranchIntent {
                        source_namespace: source_namespace.clone(),
                        source_service: source_service.clone(),
                    },
                );
            }
            ServiceIntent::Move { to_machine: _ }
            | ServiceIntent::Portal {
                source_namespace: _,
                source_service: _,
            } => {}
        }
    }

    intents
}

async fn resolve_branch_source(
    store: &StoreDriver,
    target_namespace: &Namespace,
    target_service: &str,
    intent: &BranchIntent,
) -> Result<PlannedBranchSource> {
    if &intent.source_namespace == target_namespace && intent.source_service == target_service {
        return Err(Error::Deploy(DeployError::BranchSourceIsTarget {
            namespace: target_namespace.0.clone(),
            service: target_service.to_string(),
        }));
    }

    let source_releases = store.list_deploy_releases(&intent.source_namespace).await?;
    let Some(source_release) = source_releases
        .iter()
        .find(|release| release.service == intent.source_service)
    else {
        return Err(Error::Deploy(DeployError::BranchSourceMissingRelease {
            namespace: intent.source_namespace.0.clone(),
            service: intent.source_service.clone(),
        }));
    };
    let source_revision_hash = source_release.release.primary_revision_hash.clone();

    let source_revisions = store
        .list_deploy_revisions(&intent.source_namespace)
        .await?;
    let Some(source_revision) = source_revisions.iter().find(|revision| {
        revision.service == intent.source_service && revision.revision_hash == source_revision_hash
    }) else {
        return Err(Error::Deploy(DeployError::BranchSourceMissingRevision {
            namespace: intent.source_namespace.0.clone(),
            service: intent.source_service.clone(),
            revision_hash: source_revision_hash,
        }));
    };

    let _source_spec: ServiceSpec =
        serde_json::from_str(&source_revision.spec_json).map_err(|error| {
            Error::Deploy(DeployError::BranchSourceSpecDecode {
                namespace: intent.source_namespace.0.clone(),
                service: intent.source_service.clone(),
                message: error.to_string(),
            })
        })?;

    Ok(PlannedBranchSource {
        source_namespace: intent.source_namespace.clone(),
        source_service: intent.source_service.clone(),
        source_revision_hash: source_revision.revision_hash.clone(),
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
    if !enabled.is_empty() {
        return enabled;
    }
    if machines.is_empty() {
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
    pinned_machine: Option<&MachineId>,
    next_revision_hash: &str,
    service_has_volume_changes: bool,
) -> Result<Vec<DesiredSlot>> {
    let candidates = machines.to_vec();

    let mut desired = Vec::new();
    match spec.placement {
        Placement::Replicated { count } => {
            if count == 0 {
                return Err(Error::Deploy(DeployError::ZeroReplicas {
                    service: spec.name.clone(),
                }));
            }
            for index in 0..count {
                let slot_id = SlotId(format!("slot-{number:04}", number = usize::from(index) + 1));
                let retained_machine = current_slots
                    .and_then(|slots| slots.iter().find(|slot| slot.slot_id == slot_id))
                    .filter(|slot| {
                        if let Some(pinned_machine) = pinned_machine
                            && slot.machine_id != *pinned_machine
                        {
                            return false;
                        }
                        machine_map.get(&slot.machine_id).is_some_and(|record| {
                            let candidate = record.placement_candidate();
                            is_new_placement_candidate(&candidate)
                                || (can_keep_existing_slot(&candidate)
                                    && slot.revision_hash == next_revision_hash
                                    && !service_has_volume_changes)
                        })
                    })
                    .map(|slot| slot.machine_id.clone());
                let machine_id = match retained_machine {
                    Some(machine_id) => machine_id,
                    None => new_slot_machine(pinned_machine, &candidates, usize::from(index))?,
                };
                desired.push(DesiredSlot {
                    slot_id,
                    machine_id,
                });
            }
        }
        Placement::Global => {
            if candidates.is_empty() {
                return Err(Error::Deploy(DeployError::NoEligiblePlacementTargets));
            }
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

fn new_slot_machine(
    pinned_machine: Option<&MachineId>,
    candidates: &[MachineId],
    index: usize,
) -> Result<MachineId> {
    if let Some(pinned_machine) = pinned_machine {
        if candidates.contains(pinned_machine) {
            return Ok(pinned_machine.clone());
        }
        return Err(Error::Deploy(DeployError::NoEligiblePlacementTargets));
    }
    if candidates.is_empty() {
        return Err(Error::Deploy(DeployError::NoEligiblePlacementTargets));
    }
    Ok(candidates[index % candidates.len()].clone())
}

fn service_volume_refs(manifest: &DeployManifest) -> HashMap<String, Vec<String>> {
    manifest
        .services
        .iter()
        .map(|service| {
            let mut refs = service
                .template
                .mounts
                .iter()
                .filter_map(|mount| match &mount.source {
                    MountSource::Volume(name) => Some(name.clone()),
                    MountSource::Bind(_) | MountSource::Tmpfs => None,
                })
                .collect::<Vec<_>>();
            refs.sort();
            refs.dedup();
            (service.name.clone(), refs)
        })
        .collect()
}

fn volume_attachments(manifest: &DeployManifest) -> HashMap<String, Vec<String>> {
    let mut attachments: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for service in &manifest.services {
        for mount in &service.template.mounts {
            let MountSource::Volume(name) = &mount.source else {
                continue;
            };
            attachments
                .entry(name.clone())
                .or_default()
                .push(service.name.clone());
        }
    }
    attachments
        .into_iter()
        .map(|(name, mut services)| {
            services.sort();
            services.dedup();
            (name, services)
        })
        .collect()
}

fn service_volume_pin(
    service: &str,
    service_volume_refs: &HashMap<String, Vec<String>>,
    volume_machine_map: &HashMap<String, MachineId>,
) -> Result<Option<MachineId>> {
    let Some(volume_names) = service_volume_refs.get(service) else {
        return Ok(None);
    };
    let mut pinned = None;
    for name in volume_names {
        let Some(machine_id) = volume_machine_map.get(name) else {
            continue;
        };
        if let Some(existing) = &pinned
            && existing != machine_id
        {
            return Err(Error::Deploy(
                DeployError::ServiceVolumesOnDifferentMachines {
                    service: service.to_string(),
                },
            ));
        }
        pinned = Some(machine_id.clone());
    }
    Ok(pinned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VolumeChange {
    Create,
    Update,
    Skip,
}

pub(super) fn volume_record_change(volume: &PlannedVolume) -> VolumeChange {
    let Some(current) = &volume.current else {
        return VolumeChange::Create;
    };
    let drifted = volume.declaration.scope != current.scope
        || volume.machine_id != current.machine_id
        || volume.declaration.quota != current.quota
        || volume.declaration.mode != current.mode
        || volume.declaration.owner != current.owner
        || volume.attached_services != current.attached_services;
    if drifted {
        VolumeChange::Update
    } else {
        VolumeChange::Skip
    }
}

fn validate_existing_volume(declaration: &VolumeDeclaration, record: &VolumeRecord) -> Result<()> {
    if declaration.scope != record.scope {
        return Err(Error::Deploy(DeployError::VolumeScopeChange {
            volume: declaration.name.clone(),
        }));
    }
    if declaration.mode != record.mode {
        return Err(Error::Deploy(DeployError::VolumeModeChange {
            volume: declaration.name.clone(),
        }));
    }
    if declaration.owner != record.owner {
        return Err(Error::Deploy(DeployError::VolumeOwnerChange {
            volume: declaration.name.clone(),
        }));
    }
    let requested = parse_quota_bytes(&declaration.quota).map_err(|message| {
        Error::Deploy(DeployError::VolumeQuotaInvalid {
            volume: declaration.name.clone(),
            quota_kind: "requested",
            message,
        })
    })?;
    let current = parse_quota_bytes(&record.quota).map_err(|message| {
        Error::Deploy(DeployError::VolumeQuotaInvalid {
            volume: declaration.name.clone(),
            quota_kind: "current",
            message,
        })
    })?;
    if requested < current {
        return Err(Error::Deploy(DeployError::VolumeQuotaShrink {
            volume: declaration.name.clone(),
        }));
    }
    Ok(())
}

fn new_volume_machine(
    attached_services: &[String],
    current_slots_by_service: &HashMap<String, Vec<ServiceReleaseSlot>>,
    machine_map: &HashMap<MachineId, MachineMembership>,
    desired_machines: &[MachineId],
    local_machine_id: &MachineId,
) -> Result<MachineId> {
    for service in attached_services {
        if let Some(slots) = current_slots_by_service.get(service) {
            for slot in slots {
                if machine_is_deployable(&slot.machine_id, machine_map, local_machine_id) {
                    return Ok(slot.machine_id.clone());
                }
            }
        }
    }
    desired_machines
        .first()
        .cloned()
        .ok_or(Error::Deploy(DeployError::NoEligiblePlacementTargets))
}

fn machine_can_keep_existing_work(
    machine_id: &MachineId,
    machine_map: &HashMap<MachineId, MachineMembership>,
    local_machine_id: &MachineId,
) -> bool {
    if machine_id == local_machine_id && !machine_map.contains_key(machine_id) {
        return true;
    }
    machine_map
        .get(machine_id)
        .is_some_and(|record| can_keep_existing_slot(&record.placement_candidate()))
}

fn machine_is_deployable(
    machine_id: &MachineId,
    machine_map: &HashMap<MachineId, MachineMembership>,
    local_machine_id: &MachineId,
) -> bool {
    if machine_id == local_machine_id && !machine_map.contains_key(machine_id) {
        return true;
    }
    machine_map
        .get(machine_id)
        .is_some_and(|record| is_new_placement_candidate(&record.placement_candidate()))
}

fn current_slots_by_service(
    current_releases: &[ServiceReleaseRecord],
) -> HashMap<String, Vec<ServiceReleaseSlot>> {
    current_releases
        .iter()
        .map(|release| (release.service.clone(), release.release.slots.clone()))
        .collect()
}
