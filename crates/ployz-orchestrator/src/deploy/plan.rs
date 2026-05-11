use crate::error::{DeployError, Error, Result};
use crate::machine_policy::{can_keep_existing_slot, is_new_placement_candidate};
use crate::model::{
    DeployChangeKind, DeployId, DeployImageAvailabilityPlan, DeployImageAvailabilityStatus,
    DeployPhaseAdvancePolicy, DeployPhaseCommitPolicy, DeployPhaseId, DeployPhasePlan,
    DeployPhaseRollbackPolicy, DeployPhaseWork, DeployPreview, DeployPreviewBaseline,
    DeployPreviewBaselineComponents, ImageDigest, ImagePresence, MachineId, MachineLifecycle,
    MachineMembership, ServiceBranchLineageRecord, ServiceBranchSourcePlan, ServicePlan,
    ServiceReleaseRecord, ServiceReleaseSlot, ServiceSourceMode, ServiceSourcePlan, SlotId,
    SlotPlan, VolumeClonePlan, VolumeClonePreflightAction, VolumeClonePreflightPlan,
    VolumeClonePreflightScope, VolumeMovePlan, VolumeRecord, service_source_fingerprint,
};
use ployz_store_api::{DeployStore, ImageAvailabilityStore, MachineMembershipStore, StoreDriver};
use ployz_types::spec::{
    DeployManifest, DeployPhaseIntent, MountSource, Namespace, Placement, PullPolicy,
    ServiceIntent, ServiceSpec, VolumeCloneConsistency, VolumeCloneDataPolicy, VolumeDeclaration,
    VolumeIntent, VolumeScope, parse_quota_bytes, stable_hash_hex,
};
use serde::Serialize;
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
    phases: Vec<DeployPhasePlan>,
    services: Vec<PlannedService>,
    machine_map: HashMap<MachineId, MachineMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedVolume {
    pub(super) declaration: VolumeDeclaration,
    pub(super) machine_id: MachineId,
    pub(super) attached_services: Vec<String>,
    pub(super) current_writer_slots: Vec<PlannedVolumeWriter>,
    pub(super) current: Option<VolumeRecord>,
    pub(super) movement: Option<PlannedVolumeMove>,
    pub(super) clone_source: Option<PlannedVolumeClone>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedVolumeWriter {
    pub(super) service: String,
    pub(super) slot: ServiceReleaseSlot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedVolumeMove {
    pub(super) from_machine: MachineId,
    pub(super) to_machine: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedVolumeClone {
    pub(super) source_namespace: Namespace,
    pub(super) source_volume: String,
    pub(super) source_machine: MachineId,
    pub(super) data_policy: VolumeCloneDataPolicy,
    pub(super) consistency: VolumeCloneConsistency,
    pub(super) source_record: VolumeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanFingerprint {
    pub(super) namespace: Namespace,
    pub(super) manifest_hash: String,
    pub(super) participants: Vec<MachineId>,
    pub(super) volumes: Vec<PlannedVolume>,
    pub(super) services: Vec<PlannedService>,
    pub(super) phases: Vec<DeployPhasePlan>,
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
            volumes: self.volumes.clone(),
            services: self.services.clone(),
            phases: self.phases.clone(),
        }
    }

    pub(super) fn service_sources(&self) -> Vec<ServiceSourcePlan> {
        self.services
            .iter()
            .filter(|service| service.spec.is_some())
            .map(|service| {
                let mode = match &service.branch_source {
                    Some(branch_source) => ServiceSourceMode::Branch {
                        source_namespace: branch_source.source_namespace.clone(),
                        source_service: branch_source.source_service.clone(),
                        source_revision_hash: branch_source.source_revision_hash.clone(),
                    },
                    None => ServiceSourceMode::Fresh,
                };
                ServiceSourcePlan {
                    service: service.service.clone(),
                    mode,
                }
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn service_source_fingerprint(&self) -> String {
        service_source_fingerprint(&self.service_sources())
    }

    pub(super) fn baseline(&self) -> DeployPreviewBaseline {
        let parts = self.preview_parts();
        self.baseline_from_parts(&parts)
    }

    fn baseline_from_parts(&self, parts: &ResolvedPreviewParts) -> DeployPreviewBaseline {
        DeployPreviewBaseline::new(DeployPreviewBaselineComponents {
            manifest: self.manifest_hash.clone(),
            participants: stable_hash_json(&self.participants.iter().cloned().collect::<Vec<_>>()),
            phases: stable_hash_json(&self.phases),
            services: stable_hash_json(&parts.services),
            service_sources: service_source_fingerprint(&parts.service_sources),
            volumes: stable_hash_json(&self.volume_baseline_inputs()),
            volume_moves: stable_hash_json(&parts.volume_moves),
            volume_clones: stable_hash_json(&parts.volume_clones),
        })
    }

    fn preview_parts(&self) -> ResolvedPreviewParts {
        ResolvedPreviewParts {
            service_sources: self.service_sources(),
            services: self.service_plans(),
            volume_moves: self.volume_moves(),
            volume_clones: self.volume_clones(),
        }
    }

    fn volume_baseline_inputs(&self) -> Vec<VolumeBaselineInput> {
        self.volumes
            .iter()
            .map(|volume| {
                let PlannedVolume {
                    declaration,
                    machine_id,
                    attached_services,
                    current_writer_slots,
                    current,
                    movement,
                    clone_source,
                } = volume;
                VolumeBaselineInput {
                    declaration: declaration.clone(),
                    machine_id: machine_id.clone(),
                    attached_services: attached_services.clone(),
                    current_writer_slots: current_writer_slots
                        .iter()
                        .map(|writer| {
                            let PlannedVolumeWriter { service, slot } = writer;
                            VolumeWriterBaselineInput {
                                service: service.clone(),
                                slot: slot.clone(),
                            }
                        })
                        .collect(),
                    current: current.clone(),
                    movement: movement.as_ref().map(|movement| {
                        let PlannedVolumeMove {
                            from_machine,
                            to_machine,
                        } = movement;
                        VolumeMoveBaselineInput {
                            from_machine: from_machine.clone(),
                            to_machine: to_machine.clone(),
                        }
                    }),
                    clone_source: clone_source.as_ref().map(|clone_source| {
                        let PlannedVolumeClone {
                            source_namespace,
                            source_volume,
                            source_machine,
                            data_policy,
                            consistency,
                            source_record,
                        } = clone_source;
                        VolumeCloneBaselineInput {
                            source_namespace: source_namespace.clone(),
                            source_volume: source_volume.clone(),
                            source_machine: source_machine.clone(),
                            data_policy: *data_policy,
                            consistency: *consistency,
                            source_record: source_record.clone(),
                        }
                    }),
                }
            })
            .collect()
    }

    fn service_plans(&self) -> Vec<ServicePlan> {
        self.services
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
            .collect()
    }

    fn volume_moves(&self) -> Vec<VolumeMovePlan> {
        self.volumes
            .iter()
            .filter_map(|volume| {
                let movement = volume.movement.as_ref()?;
                Some(VolumeMovePlan {
                    volume: volume.declaration.name.clone(),
                    from_machine: movement.from_machine.clone(),
                    to_machine: movement.to_machine.clone(),
                    attached_services: volume.attached_services.clone(),
                })
            })
            .collect()
    }

    fn volume_clones(&self) -> Vec<VolumeClonePlan> {
        self.volumes
            .iter()
            .filter_map(|volume| {
                let clone_source = volume.clone_source.as_ref()?;
                Some(VolumeClonePlan {
                    volume: volume.declaration.name.clone(),
                    source_namespace: clone_source.source_namespace.clone(),
                    source_volume: clone_source.source_volume.clone(),
                    source_machine: clone_source.source_machine.clone(),
                    target_machine: volume.machine_id.clone(),
                    data_policy: clone_source.data_policy,
                    consistency: clone_source.consistency,
                    attached_services: volume.attached_services.clone(),
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn to_preview(&self, warnings: Vec<String>) -> DeployPreview {
        self.to_preview_with_image_availability(warnings, Vec::new())
    }

    pub(super) fn to_preview_with_image_availability(
        &self,
        warnings: Vec<String>,
        image_availability: Vec<DeployImageAvailabilityPlan>,
    ) -> DeployPreview {
        let parts = self.preview_parts();
        let baseline = self.baseline_from_parts(&parts);
        let service_source_fingerprint = baseline.components.service_sources.clone();
        let service_branch_sources = service_branch_sources_from_sources(&parts.service_sources);
        DeployPreview {
            namespace: self.namespace.clone(),
            manifest_hash: self.manifest_hash.clone(),
            baseline: Some(baseline),
            participants: self.participants.iter().cloned().collect(),
            phases: self.phases.clone(),
            services: parts.services,
            service_sources: parts.service_sources,
            service_source_fingerprint,
            service_branch_sources,
            volume_moves: parts.volume_moves,
            volume_clones: parts.volume_clones,
            volume_clone_preflights: self
                .phases
                .iter()
                .filter_map(|phase| {
                    let volumes = phase
                        .work
                        .iter()
                        .filter_map(|work| match work {
                            DeployPhaseWork::VolumeClone { volume, .. } => Some(volume.clone()),
                            DeployPhaseWork::Service { .. }
                            | DeployPhaseWork::Volume { .. }
                            | DeployPhaseWork::VolumeMove { .. } => None,
                        })
                        .collect::<Vec<_>>();
                    if volumes.is_empty() {
                        return None;
                    }
                    Some(VolumeClonePreflightPlan {
                        phase_id: phase.phase_id.clone(),
                        volumes,
                        action: VolumeClonePreflightAction::DrainAndRemoveBeforeCloneReplacement,
                        scope: VolumeClonePreflightScope::UncommittedNamespaceInstances,
                    })
                })
                .collect(),
            image_availability,
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

fn service_branch_sources_from_sources(
    sources: &[ServiceSourcePlan],
) -> Vec<ServiceBranchSourcePlan> {
    sources
        .iter()
        .filter_map(|source| match &source.mode {
            ServiceSourceMode::Fresh => None,
            ServiceSourceMode::Branch {
                source_namespace,
                source_service,
                source_revision_hash,
            } => Some(ServiceBranchSourcePlan {
                service: source.service.clone(),
                source_namespace: source_namespace.clone(),
                source_service: source_service.clone(),
                source_revision_hash: source_revision_hash.clone(),
            }),
        })
        .collect()
}

pub(super) async fn preflight_image_availability(
    store: &StoreDriver,
    plan: &ResolvedPlan,
) -> Result<Vec<DeployImageAvailabilityPlan>> {
    let mut checks = Vec::new();
    for service in &plan.services {
        let Some(spec) = &service.spec else {
            continue;
        };
        if spec.template.pull_policy != PullPolicy::Never {
            continue;
        }
        if !service.slots.iter().any(|slot| {
            matches!(
                slot.action,
                DeployChangeKind::Create | DeployChangeKind::Replace
            )
        }) {
            continue;
        }
        let image = spec.template.image.clone();
        let digest = parse_required_image_digest(&service.service, &image)?;
        for slot in &service.slots {
            if !matches!(
                slot.action,
                DeployChangeKind::Create | DeployChangeKind::Replace
            ) {
                continue;
            }
            let record = store
                .get_image_availability(&slot.machine_id, &digest)
                .await?;
            match record.as_ref().map(|record| &record.presence) {
                Some(ImagePresence::Present { .. }) => {
                    checks.push(DeployImageAvailabilityPlan {
                        service: service.service.clone(),
                        slot_id: slot.slot_id.clone(),
                        machine_id: slot.machine_id.clone(),
                        image: image.clone(),
                        digest: digest.clone(),
                        status: DeployImageAvailabilityStatus::Present,
                    });
                }
                Some(presence) => {
                    return Err(Error::Deploy(
                        DeployError::DeployImageAvailabilityNotPresent {
                            service: service.service.clone(),
                            slot_id: slot.slot_id.0.clone(),
                            machine_id: slot.machine_id.0.clone(),
                            image,
                            digest: digest.as_str().into(),
                            state: image_presence_state(presence).into(),
                        },
                    ));
                }
                None => {
                    return Err(Error::Deploy(DeployError::DeployImageAvailabilityMissing {
                        service: service.service.clone(),
                        slot_id: slot.slot_id.0.clone(),
                        machine_id: slot.machine_id.0.clone(),
                        image,
                        digest: digest.as_str().into(),
                    }));
                }
            }
        }
    }
    Ok(checks)
}

fn parse_required_image_digest(service: &str, image: &str) -> Result<ImageDigest> {
    ImageDigest::try_new(image).map_err(|_| {
        Error::Deploy(DeployError::DeployImageDigestRequired {
            service: service.into(),
            image: image.into(),
        })
    })
}

fn image_presence_state(presence: &ImagePresence) -> &'static str {
    match presence {
        ImagePresence::Present { .. } => "present",
        ImagePresence::Absent { .. } => "absent",
        ImagePresence::Transferring { .. } => "transferring",
        ImagePresence::Failed { .. } => "failed",
    }
}

struct ResolvedPreviewParts {
    service_sources: Vec<ServiceSourcePlan>,
    services: Vec<ServicePlan>,
    volume_moves: Vec<VolumeMovePlan>,
    volume_clones: Vec<VolumeClonePlan>,
}

#[derive(Serialize)]
struct VolumeBaselineInput {
    declaration: VolumeDeclaration,
    machine_id: MachineId,
    attached_services: Vec<String>,
    current_writer_slots: Vec<VolumeWriterBaselineInput>,
    current: Option<VolumeRecord>,
    movement: Option<VolumeMoveBaselineInput>,
    clone_source: Option<VolumeCloneBaselineInput>,
}

#[derive(Serialize)]
struct VolumeWriterBaselineInput {
    service: String,
    slot: ServiceReleaseSlot,
}

#[derive(Serialize)]
struct VolumeMoveBaselineInput {
    from_machine: MachineId,
    to_machine: MachineId,
}

#[derive(Serialize)]
struct VolumeCloneBaselineInput {
    source_namespace: Namespace,
    source_volume: String,
    source_machine: MachineId,
    data_policy: VolumeCloneDataPolicy,
    consistency: VolumeCloneConsistency,
    source_record: VolumeRecord,
}

fn stable_hash_json<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_vec(value).expect("deploy baseline input should serialize");
    stable_hash_hex(&json)
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
    let current_volume_branches = store.list_volume_branches(&manifest.namespace).await?;
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
    let volume_move_intents = volume_move_intents(manifest);
    let volume_clone_intents = volume_clone_intents(manifest);
    let phase_intents = ordered_phase_intents(manifest)?;
    let mut service_phase_owners = phase_service_owners(&phase_intents);
    let mut volume_phase_owners = phase_volume_owners(&phase_intents);

    for declaration in &manifest.volumes {
        let attached_services = volume_attachments
            .get(&declaration.name)
            .cloned()
            .unwrap_or_default();
        let current_attached_services = volume_map
            .get(&declaration.name)
            .map(|record| record.attached_services.clone())
            .unwrap_or_default();
        let writer_services = attached_services
            .iter()
            .chain(current_attached_services.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut current_writer_slots = Vec::new();
        for service in writer_services {
            if let Some(slots) = current_slots_by_service.get(&service) {
                for slot in slots {
                    current_writer_slots.push(PlannedVolumeWriter {
                        service: service.clone(),
                        slot: slot.clone(),
                    });
                }
            }
        }
        current_writer_slots.sort_by(|left, right| {
            left.service
                .cmp(&right.service)
                .then_with(|| left.slot.slot_id.0.cmp(&right.slot.slot_id.0))
        });
        let volume_move_intent = volume_move_intents.get(&declaration.name);
        let volume_clone_intent = volume_clone_intents.get(&declaration.name);
        let (machine_id, movement, clone_source) = match volume_map.get(&declaration.name) {
            Some(record) => {
                validate_existing_volume(declaration, record)?;
                if let Some(intent) = volume_clone_intent
                    && !current_volume_branches.iter().any(|branch| {
                        branch.volume_name == declaration.name
                            && branch.source_namespace == intent.source_namespace
                            && branch.source_volume_name == intent.source_volume
                            && branch.data_policy == intent.data_policy
                            && branch.consistency == intent.consistency
                    })
                {
                    return Err(Error::Deploy(DeployError::VolumeCloneTargetExists {
                        volume: declaration.name.clone(),
                    }));
                }
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
                match volume_move_intent {
                    Some(intent) => {
                        let (machine_id, movement) =
                            resolve_volume_move(declaration, record, intent, &machine_map)?;
                        (machine_id, movement, None)
                    }
                    None if should_move_volume_from_draining_source(
                        declaration,
                        record,
                        &machine_map,
                    ) =>
                    {
                        let preferred_target = preferred_existing_volume_pin(
                            &declaration.name,
                            record,
                            &attached_services,
                            &service_volume_refs,
                            &volume_machine_map,
                            &volume_move_intents,
                            &volume_map,
                            &machine_map,
                        );
                        let (machine_id, movement) = resolve_inferred_volume_move(
                            record,
                            &desired_machines,
                            &machine_map,
                            preferred_target.as_ref(),
                        )?;
                        (machine_id, movement, None)
                    }
                    None => (record.machine_id.clone(), None, None),
                }
            }
            None => {
                if volume_move_intent.is_some() {
                    return Err(Error::Deploy(DeployError::VolumeMoveMissingSource {
                        volume: declaration.name.clone(),
                    }));
                }
                if let Some(intent) = volume_clone_intent {
                    let clone_source = resolve_volume_clone(
                        store,
                        &manifest.namespace,
                        declaration,
                        intent,
                        &machine_map,
                    )
                    .await?;
                    (
                        clone_source.source_machine.clone(),
                        None,
                        Some(clone_source),
                    )
                } else {
                    (
                        new_volume_machine(
                            &attached_services,
                            &current_slots_by_service,
                            &machine_map,
                            &desired_machines,
                            local_machine_id,
                        )?,
                        None,
                        None,
                    )
                }
            }
        };
        if let Some(movement) = &movement {
            participants.insert(movement.from_machine.clone());
            participants.insert(movement.to_machine.clone());
        }
        if let Some(clone_source) = &clone_source {
            participants.insert(clone_source.source_machine.clone());
        }
        volume_machine_map.insert(declaration.name.clone(), machine_id.clone());
        planned_volumes.push(PlannedVolume {
            declaration: declaration.clone(),
            machine_id,
            attached_services,
            current_writer_slots,
            current: volume_map.get(&declaration.name).cloned(),
            movement,
            clone_source,
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
            phase: Some(phase_index_for_service(
                &spec.name,
                &service_phase_owners,
                &phase_intents,
            )),
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

    assign_volume_attached_services_to_volume_phases(
        &planned_volumes,
        &services,
        &mut volume_phase_owners,
        &mut service_phase_owners,
    )?;
    for service in &mut services {
        if service.spec.is_some() {
            service.phase = Some(phase_index_for_service(
                &service.service,
                &service_phase_owners,
                &phase_intents,
            ));
        }
    }
    let phases = build_phase_plans(
        &phase_intents,
        &planned_volumes,
        &services,
        &participants,
        &service_phase_owners,
        &volume_phase_owners,
    );

    Ok(ResolvedPlan {
        namespace: manifest.namespace.clone(),
        manifest_hash,
        volumes_json,
        volumes: planned_volumes,
        participants,
        phases,
        services,
        machine_map,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchIntent {
    source_namespace: Namespace,
    source_service: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeMoveIntent {
    from_machine: MachineId,
    to_machine: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeCloneIntent {
    source_namespace: Namespace,
    source_volume: String,
    data_policy: VolumeCloneDataPolicy,
    consistency: VolumeCloneConsistency,
}

const SYNTHETIC_DEPLOY_PHASE_ID: &str = "deploy";

fn ordered_phase_intents(manifest: &DeployManifest) -> Result<Vec<DeployPhaseIntent>> {
    let Some(deploy_intent) = &manifest.intent else {
        return Ok(Vec::new());
    };
    if deploy_intent
        .phases
        .iter()
        .any(|phase| phase.phase_id == SYNTHETIC_DEPLOY_PHASE_ID)
    {
        return Err(Error::Deploy(DeployError::ManifestInvalid {
            message: format!(
                "deploy phase id '{SYNTHETIC_DEPLOY_PHASE_ID}' is reserved for the synthetic final deploy phase"
            ),
        }));
    }
    let mut remaining = deploy_intent.phases.clone();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let ordered_ids = ordered
            .iter()
            .map(|phase: &DeployPhaseIntent| phase.phase_id.as_str())
            .collect::<BTreeSet<_>>();
        let Some(index) = remaining.iter().position(|phase| {
            phase
                .after
                .iter()
                .all(|after| ordered_ids.contains(after.as_str()))
        }) else {
            return Err(Error::Deploy(DeployError::ManifestInvalid {
                message: "deploy phase dependencies contain a cycle".into(),
            }));
        };
        ordered.push(remaining.remove(index));
    }
    Ok(ordered)
}

fn phase_service_owners(phases: &[DeployPhaseIntent]) -> BTreeMap<String, DeployPhaseId> {
    phases
        .iter()
        .flat_map(|phase| {
            phase
                .services
                .iter()
                .map(|service| (service.clone(), DeployPhaseId(phase.phase_id.clone())))
        })
        .collect()
}

fn phase_volume_owners(phases: &[DeployPhaseIntent]) -> BTreeMap<String, DeployPhaseId> {
    phases
        .iter()
        .flat_map(|phase| {
            phase
                .volumes
                .iter()
                .map(|volume| (volume.clone(), DeployPhaseId(phase.phase_id.clone())))
        })
        .collect()
}

fn phase_index_for_service(
    service: &str,
    service_phase_owners: &BTreeMap<String, DeployPhaseId>,
    phase_intents: &[DeployPhaseIntent],
) -> u32 {
    service_phase_owners
        .get(service)
        .and_then(|phase_id| {
            phase_intents
                .iter()
                .position(|phase| phase.phase_id == phase_id.0)
        })
        .unwrap_or(phase_intents.len()) as u32
}

fn assign_volume_attached_services_to_volume_phases(
    volumes: &[PlannedVolume],
    services: &[PlannedService],
    volume_phase_owners: &mut BTreeMap<String, DeployPhaseId>,
    service_phase_owners: &mut BTreeMap<String, DeployPhaseId>,
) -> Result<()> {
    let manifest_services = services
        .iter()
        .filter(|service| service.spec.is_some())
        .map(|service| service.service.as_str())
        .collect::<BTreeSet<_>>();
    for volume in volumes
        .iter()
        .filter(|volume| !matches!(volume_record_change(volume), VolumeChange::Skip))
    {
        let volume_phase_id = volume_phase_owners.get(&volume.declaration.name).cloned();
        for service in &volume.attached_services {
            if !manifest_services.contains(service.as_str()) {
                continue;
            }
            match (volume_phase_id.as_ref(), service_phase_owners.get(service)) {
                (Some(volume_phase_id), Some(service_phase_id))
                    if service_phase_id != volume_phase_id =>
                {
                    return Err(Error::Deploy(DeployError::ManifestInvalid {
                        message: format!(
                            "volume '{}' and attached service '{}' must be assigned to the same deploy phase",
                            volume.declaration.name, service
                        ),
                    }));
                }
                (Some(volume_phase_id), None) => {
                    service_phase_owners.insert(service.clone(), volume_phase_id.clone());
                }
                (None, Some(service_phase_id)) => {
                    volume_phase_owners
                        .insert(volume.declaration.name.clone(), service_phase_id.clone());
                }
                (Some(_), Some(_)) | (None, None) => {}
            }
        }
    }
    Ok(())
}

fn build_phase_plans(
    phase_intents: &[DeployPhaseIntent],
    volumes: &[PlannedVolume],
    services: &[PlannedService],
    participants: &BTreeSet<MachineId>,
    service_phase_owners: &BTreeMap<String, DeployPhaseId>,
    volume_phase_owners: &BTreeMap<String, DeployPhaseId>,
) -> Vec<DeployPhasePlan> {
    let default_phase_id = DeployPhaseId(SYNTHETIC_DEPLOY_PHASE_ID.into());
    let mut phase_work: BTreeMap<DeployPhaseId, Vec<DeployPhaseWork>> = BTreeMap::new();
    let mut phase_participants: BTreeMap<DeployPhaseId, BTreeSet<MachineId>> = BTreeMap::new();

    for volume in volumes {
        let change = volume_record_change(volume);
        if matches!(change, VolumeChange::Skip) {
            continue;
        }
        let phase_id = volume_phase_owners
            .get(&volume.declaration.name)
            .cloned()
            .unwrap_or_else(|| default_phase_id.clone());
        match change {
            VolumeChange::Move => {
                if let Some(movement) = &volume.movement {
                    phase_participants
                        .entry(phase_id.clone())
                        .or_default()
                        .insert(movement.from_machine.clone());
                    phase_participants
                        .entry(phase_id.clone())
                        .or_default()
                        .insert(movement.to_machine.clone());
                    phase_work
                        .entry(phase_id)
                        .or_default()
                        .push(DeployPhaseWork::VolumeMove {
                            volume: volume.declaration.name.clone(),
                            from_machine: movement.from_machine.clone(),
                            to_machine: movement.to_machine.clone(),
                            attached_services: volume.attached_services.clone(),
                        });
                }
            }
            VolumeChange::Create if volume.clone_source.is_some() => {
                let Some(clone_source) = &volume.clone_source else {
                    continue;
                };
                phase_participants
                    .entry(phase_id.clone())
                    .or_default()
                    .insert(clone_source.source_machine.clone());
                phase_work
                    .entry(phase_id)
                    .or_default()
                    .push(DeployPhaseWork::VolumeClone {
                        volume: volume.declaration.name.clone(),
                        source_namespace: clone_source.source_namespace.clone(),
                        source_volume: clone_source.source_volume.clone(),
                        source_machine: clone_source.source_machine.clone(),
                        target_machine: volume.machine_id.clone(),
                        data_policy: clone_source.data_policy,
                        consistency: clone_source.consistency,
                        attached_services: volume.attached_services.clone(),
                    });
            }
            VolumeChange::Create | VolumeChange::Update => {
                phase_participants
                    .entry(phase_id.clone())
                    .or_default()
                    .insert(volume.machine_id.clone());
                phase_work
                    .entry(phase_id)
                    .or_default()
                    .push(DeployPhaseWork::Volume {
                        volume: volume.declaration.name.clone(),
                        action: match change {
                            VolumeChange::Create => DeployChangeKind::Create,
                            VolumeChange::Update => DeployChangeKind::Replace,
                            VolumeChange::Move | VolumeChange::Skip => DeployChangeKind::Unchanged,
                        },
                    });
            }
            VolumeChange::Skip => {}
        }
    }

    for service in services {
        if service.action == DeployChangeKind::Unchanged {
            continue;
        }
        let phase_id = service_phase_owners
            .get(&service.service)
            .cloned()
            .unwrap_or_else(|| default_phase_id.clone());
        let participants_for_service = service
            .slots
            .iter()
            .flat_map(|slot| {
                [
                    Some(slot.machine_id.clone()),
                    slot.current
                        .as_ref()
                        .map(|current| current.machine_id.clone()),
                ]
            })
            .flatten()
            .collect::<Vec<_>>();
        phase_participants
            .entry(phase_id.clone())
            .or_default()
            .extend(participants_for_service);
        phase_work
            .entry(phase_id)
            .or_default()
            .push(DeployPhaseWork::Service {
                service: service.service.clone(),
                action: service.action,
            });
    }

    let mut phases = Vec::new();
    for (index, intent) in phase_intents.iter().enumerate() {
        let phase_id = DeployPhaseId(intent.phase_id.clone());
        phases.push(DeployPhasePlan {
            phase_id: phase_id.clone(),
            name: intent
                .name
                .clone()
                .unwrap_or_else(|| intent.phase_id.clone()),
            order: index as u32,
            after: intent.after.iter().cloned().map(DeployPhaseId).collect(),
            participants: phase_participants
                .remove(&phase_id)
                .unwrap_or_default()
                .into_iter()
                .collect(),
            work: phase_work.remove(&phase_id).unwrap_or_default(),
            commit_policy: intent.commit_policy,
            rollback_policy: intent.rollback_policy,
            advance_policy: intent.advance_policy,
        });
    }

    if phase_intents.is_empty()
        || phase_work.contains_key(&default_phase_id)
        || phase_participants.contains_key(&default_phase_id)
    {
        phases.push(DeployPhasePlan {
            phase_id: default_phase_id.clone(),
            name: "Deploy".into(),
            order: phases.len() as u32,
            after: Vec::new(),
            participants: phase_participants
                .remove(&default_phase_id)
                .unwrap_or_else(|| participants.clone())
                .into_iter()
                .collect(),
            work: phase_work.remove(&default_phase_id).unwrap_or_default(),
            commit_policy: DeployPhaseCommitPolicy::EndOfDeploy,
            rollback_policy: DeployPhaseRollbackPolicy::Reversible,
            advance_policy: DeployPhaseAdvancePolicy::Immediate,
        });
    }

    phases
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

fn volume_move_intents(manifest: &DeployManifest) -> HashMap<String, VolumeMoveIntent> {
    let mut intents = HashMap::new();
    let Some(deploy_intent) = &manifest.intent else {
        return intents;
    };

    for hint in &deploy_intent.volumes {
        match &hint.intent {
            VolumeIntent::Move {
                from_machine,
                to_machine,
            } => {
                intents.insert(
                    hint.volume.clone(),
                    VolumeMoveIntent {
                        from_machine: MachineId(from_machine.clone()),
                        to_machine: MachineId(to_machine.clone()),
                    },
                );
            }
            VolumeIntent::Clone {
                source_namespace: _,
                source_volume: _,
                data_policy: _,
                consistency: _,
            } => {}
        }
    }

    intents
}

fn volume_clone_intents(manifest: &DeployManifest) -> HashMap<String, VolumeCloneIntent> {
    let mut intents = HashMap::new();
    let Some(deploy_intent) = &manifest.intent else {
        return intents;
    };

    for hint in &deploy_intent.volumes {
        match &hint.intent {
            VolumeIntent::Clone {
                source_namespace,
                source_volume,
                data_policy,
                consistency,
            } => {
                intents.insert(
                    hint.volume.clone(),
                    VolumeCloneIntent {
                        source_namespace: source_namespace.clone(),
                        source_volume: source_volume.clone(),
                        data_policy: *data_policy,
                        consistency: *consistency,
                    },
                );
            }
            VolumeIntent::Move {
                from_machine: _,
                to_machine: _,
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

async fn resolve_volume_clone(
    store: &StoreDriver,
    target_namespace: &Namespace,
    declaration: &VolumeDeclaration,
    intent: &VolumeCloneIntent,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Result<PlannedVolumeClone> {
    if &intent.source_namespace == target_namespace && intent.source_volume == declaration.name {
        return Err(Error::Deploy(DeployError::VolumeCloneSourceIsTarget {
            volume: declaration.name.clone(),
        }));
    }

    let Some(source_record) = store
        .get_volume(&intent.source_namespace, &intent.source_volume)
        .await?
    else {
        return Err(Error::Deploy(DeployError::VolumeCloneSourceMissing {
            volume: declaration.name.clone(),
            source_namespace: intent.source_namespace.0.clone(),
            source_volume: intent.source_volume.clone(),
        }));
    };

    if declaration.scope != VolumeScope::Single || source_record.scope != VolumeScope::Single {
        return Err(Error::Deploy(DeployError::VolumeCloneRequiresSingleScope {
            volume: declaration.name.clone(),
        }));
    }

    let Some(source_machine) = machine_map.get(&source_record.machine_id) else {
        return Err(Error::Deploy(
            DeployError::VolumeCloneSourceMachineMissing {
                volume: declaration.name.clone(),
                machine_id: source_record.machine_id.0.clone(),
            },
        ));
    };
    if !is_new_placement_candidate(&source_machine.placement_candidate()) {
        return Err(Error::Deploy(
            DeployError::VolumeCloneSourceMachineIneligible {
                volume: declaration.name.clone(),
                machine_id: source_record.machine_id.0.clone(),
            },
        ));
    }
    if !source_machine.storage {
        return Err(Error::Deploy(
            DeployError::VolumeCloneSourceMachineNotStorageCapable {
                volume: declaration.name.clone(),
                machine_id: source_record.machine_id.0.clone(),
            },
        ));
    }

    Ok(PlannedVolumeClone {
        source_namespace: intent.source_namespace.clone(),
        source_volume: intent.source_volume.clone(),
        source_machine: source_record.machine_id.clone(),
        data_policy: intent.data_policy,
        consistency: intent.consistency,
        source_record,
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
                            can_retain_existing_slot_for_deploy(
                                record,
                                slot,
                                next_revision_hash,
                                service_has_volume_changes,
                            )
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

fn can_retain_existing_slot_for_deploy(
    machine: &MachineMembership,
    slot: &ServiceReleaseSlot,
    next_revision_hash: &str,
    service_has_volume_changes: bool,
) -> bool {
    let candidate = machine.placement_candidate();
    is_new_placement_candidate(&candidate)
        || (can_keep_existing_slot(&candidate)
            && !should_relocate_existing_work(machine)
            && slot.revision_hash == next_revision_hash
            && !service_has_volume_changes)
}

fn should_relocate_existing_work(machine: &MachineMembership) -> bool {
    machine.lifecycle == MachineLifecycle::Draining
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
    Move,
    Skip,
}

pub(super) fn volume_record_change(volume: &PlannedVolume) -> VolumeChange {
    let Some(current) = &volume.current else {
        return VolumeChange::Create;
    };
    if volume.movement.is_some() && volume.machine_id != current.machine_id {
        return VolumeChange::Move;
    }
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

fn resolve_volume_move(
    declaration: &VolumeDeclaration,
    record: &VolumeRecord,
    intent: &VolumeMoveIntent,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Result<(MachineId, Option<PlannedVolumeMove>)> {
    if record.machine_id != intent.from_machine {
        return Err(Error::Deploy(DeployError::VolumeMoveSourceMismatch {
            volume: declaration.name.clone(),
            expected_machine: intent.from_machine.0.clone(),
            actual_machine: record.machine_id.0.clone(),
        }));
    }
    if declaration.scope != VolumeScope::Single {
        return Err(Error::Deploy(DeployError::VolumeMoveRequiresSingleScope {
            volume: declaration.name.clone(),
        }));
    }
    if intent.from_machine == intent.to_machine {
        return Ok((record.machine_id.clone(), None));
    }

    let target = volume_move_target(&declaration.name, &intent.to_machine, machine_map)?;
    Ok((
        target.clone(),
        Some(PlannedVolumeMove {
            from_machine: record.machine_id.clone(),
            to_machine: target,
        }),
    ))
}

fn should_move_volume_from_draining_source(
    declaration: &VolumeDeclaration,
    record: &VolumeRecord,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> bool {
    declaration.scope == VolumeScope::Single
        && machine_map
            .get(&record.machine_id)
            .is_some_and(should_relocate_existing_work)
}

fn resolve_inferred_volume_move(
    record: &VolumeRecord,
    desired_machines: &[MachineId],
    machine_map: &HashMap<MachineId, MachineMembership>,
    preferred_target: Option<&MachineId>,
) -> Result<(MachineId, Option<PlannedVolumeMove>)> {
    if let Some(target) = preferred_target
        && desired_machines.contains(target)
        && target != &record.machine_id
        && machine_map
            .get(target)
            .is_some_and(|machine| machine.storage)
    {
        return Ok((
            target.clone(),
            Some(PlannedVolumeMove {
                from_machine: record.machine_id.clone(),
                to_machine: target.clone(),
            }),
        ));
    }

    let Some(target) = desired_machines.iter().find(|machine_id| {
        **machine_id != record.machine_id
            && machine_map
                .get(*machine_id)
                .is_some_and(|machine| machine.storage)
    }) else {
        return Err(Error::Deploy(DeployError::NoEligiblePlacementTargets));
    };

    Ok((
        target.clone(),
        Some(PlannedVolumeMove {
            from_machine: record.machine_id.clone(),
            to_machine: target.clone(),
        }),
    ))
}

fn preferred_existing_volume_pin(
    volume_name: &str,
    record: &VolumeRecord,
    attached_services: &[String],
    service_volume_refs: &HashMap<String, Vec<String>>,
    planned_volume_machines: &HashMap<String, MachineId>,
    volume_move_intents: &HashMap<String, VolumeMoveIntent>,
    current_volumes: &HashMap<String, VolumeRecord>,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Option<MachineId> {
    let mut preferred = None;
    for service in attached_services {
        let Some(service_volumes) = service_volume_refs.get(service) else {
            continue;
        };
        for service_volume in service_volumes {
            if service_volume == volume_name {
                continue;
            }
            let Some(machine_id) = planned_volume_machines
                .get(service_volume)
                .cloned()
                .or_else(|| {
                    pending_volume_move_target(
                        service_volume,
                        volume_move_intents,
                        current_volumes,
                        machine_map,
                    )
                })
                .or_else(|| {
                    current_volumes
                        .get(service_volume)
                        .map(|record| record.machine_id.clone())
                })
            else {
                continue;
            };
            if machine_id == record.machine_id {
                continue;
            }
            let Some(machine) = machine_map.get(&machine_id) else {
                continue;
            };
            if !machine.storage || !is_new_placement_candidate(&machine.placement_candidate()) {
                continue;
            }
            if preferred
                .as_ref()
                .is_some_and(|existing: &MachineId| existing != &machine_id)
            {
                return None;
            }
            preferred = Some(machine_id);
        }
    }
    preferred
}

fn pending_volume_move_target(
    volume_name: &str,
    volume_move_intents: &HashMap<String, VolumeMoveIntent>,
    current_volumes: &HashMap<String, VolumeRecord>,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Option<MachineId> {
    let intent = volume_move_intents.get(volume_name)?;
    let record = current_volumes.get(volume_name)?;
    if record.machine_id != intent.from_machine || intent.from_machine == intent.to_machine {
        return None;
    }
    let machine = machine_map.get(&intent.to_machine)?;
    if !machine.storage || !is_new_placement_candidate(&machine.placement_candidate()) {
        return None;
    }
    Some(intent.to_machine.clone())
}

fn volume_move_target(
    volume: &str,
    machine_id: &MachineId,
    machine_map: &HashMap<MachineId, MachineMembership>,
) -> Result<MachineId> {
    let Some(machine) = machine_map.get(machine_id) else {
        return Err(Error::Deploy(DeployError::VolumeMoveTargetMissing {
            volume: volume.to_string(),
            machine_id: machine_id.0.clone(),
        }));
    };
    if !is_new_placement_candidate(&machine.placement_candidate()) {
        return Err(Error::Deploy(DeployError::VolumeMoveTargetIneligible {
            volume: volume.to_string(),
            machine_id: machine_id.0.clone(),
        }));
    }
    if !machine.storage {
        return Err(Error::Deploy(
            DeployError::VolumeMoveTargetNotStorageCapable {
                volume: volume.to_string(),
                machine_id: machine_id.0.clone(),
            },
        ));
    }
    Ok(machine_id.clone())
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
