use super::*;

pub(super) fn plan_deploy_service(
    target: &DeployPlanningTarget,
    input: DeployPlanningInput,
    volume_plan: &VolumePlan,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let service = planning_service(target, &input.service_id)?;
    match (service.mode(), input.placement) {
        (
            ServiceMode::Replicated { replicas },
            DeployPlanningPlacementInput::Replicated { eligible_machines },
        ) => plan_replicated_service(
            service,
            input.service_id,
            eligible_machines,
            input.existing_replicas,
            input.cleanup_candidates,
            volume_plan,
            replicas,
        ),
        (ServiceMode::Global, DeployPlanningPlacementInput::Global(placement)) => {
            plan_global_service(
                service,
                input.service_id,
                placement,
                input.existing_replicas,
                input.cleanup_candidates,
            )
        }
        _ => Err(DeployPlanError::PlacementModeMismatch {
            service_id: input.service_id,
        }),
    }
}

fn plan_replicated_service(
    service: &DeployPlanningService,
    service_id: ServiceId,
    eligible_machines: Vec<MachineId>,
    mut existing_replicas: Vec<ExistingServiceReplica>,
    cleanup_candidates: Vec<ObservedCleanupCandidate>,
    volume_plan: &VolumePlan,
    replicas: ReplicaCount,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let volume_placement = volume_plan.placement(&service_id)?;
    let target_replicas = usize::from(replicas.get());
    if let Some(machine_id) = &volume_placement.machine_id {
        existing_replicas.retain(|replica| &replica.machine_id == machine_id);
    }
    normalize_existing_replicas(&mut existing_replicas);
    let mut steps = existing_replicas
        .into_iter()
        .take(target_replicas)
        .enumerate()
        .map(|(index, replica)| DeployPlanStep::UseExistingContainer {
            machine_id: replica.machine_id,
            container_id: replica.container_id,
            slot: replicated_slot((index + 1) as u16),
        })
        .collect::<Vec<_>>();
    let missing_replicas = target_replicas.saturating_sub(steps.len());
    if missing_replicas > 0 && eligible_machines.is_empty() {
        return Err(DeployPlanError::NoEligibleMachines { service_id });
    }

    let existing_replica_count = steps.len();
    let run_machines = volume_placement
        .machine_id
        .as_ref()
        .map(|machine_id| vec![machine_id.clone()])
        .unwrap_or(eligible_machines);
    steps.extend(
        run_machines
            .iter()
            .cycle()
            .take(missing_replicas)
            .enumerate()
            .map(|(index, machine_id)| DeployPlanStep::RunContainer {
                machine_id: machine_id.clone(),
                slot: replicated_slot((existing_replica_count + index + 1) as u16),
            }),
    );

    Ok(finalize_service_plan(
        service,
        service_id,
        DeployServicePlacement::Replicated,
        steps,
        cleanup_candidates,
    ))
}

fn plan_global_service(
    service: &DeployPlanningService,
    service_id: ServiceId,
    placement: GlobalPlanningInput,
    mut existing_replicas: Vec<ExistingServiceReplica>,
    mut cleanup_candidates: Vec<ObservedCleanupCandidate>,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let selected = placement
        .candidates()
        .iter()
        .filter_map(|(machine_id, disposition)| {
            matches!(disposition, GlobalCandidateDisposition::Selected)
                .then_some(machine_id.clone())
        })
        .collect::<Vec<_>>();
    if selected.is_empty()
        && matches!(
            placement.empty_selection_policy(),
            EmptyGlobalSelectionPolicy::RequireSelected
        )
    {
        return Err(DeployPlanError::NoEligibleMachines { service_id });
    }
    let selected_set = selected.iter().cloned().collect::<BTreeSet<_>>();
    let deferred_set = placement
        .candidates()
        .iter()
        .filter_map(|(machine_id, disposition)| {
            matches!(disposition, GlobalCandidateDisposition::Deferred { .. })
                .then_some(machine_id.clone())
        })
        .collect::<BTreeSet<_>>();
    let draining_set = placement
        .candidates()
        .iter()
        .filter_map(|(machine_id, disposition)| {
            matches!(disposition, GlobalCandidateDisposition::Draining)
                .then_some(machine_id.clone())
        })
        .collect::<BTreeSet<_>>();
    normalize_existing_replicas(&mut existing_replicas);
    let steps = selected
        .iter()
        .map(|machine_id| {
            existing_replicas
                .iter()
                .find(|replica| replica.machine_id == *machine_id)
                .map_or_else(
                    || DeployPlanStep::RunContainer {
                        machine_id: machine_id.clone(),
                        slot: ReplicaSlot::Global,
                    },
                    |replica| DeployPlanStep::UseExistingContainer {
                        machine_id: machine_id.clone(),
                        container_id: replica.container_id.clone(),
                        slot: ReplicaSlot::Global,
                    },
                )
        })
        .collect::<Vec<_>>();

    cleanup_candidates.retain(|candidate| {
        !deferred_set.contains(&candidate.target.machine_id)
            && (selected_set.contains(&candidate.target.machine_id)
                || draining_set.contains(&candidate.target.machine_id))
    });
    let service_placement = DeployServicePlacement::Global {
        candidates: placement.candidates().keys().cloned().collect(),
        selected,
        deferred: placement
            .candidates()
            .iter()
            .filter_map(|(machine_id, disposition)| match disposition {
                GlobalCandidateDisposition::Deferred { reason } => {
                    Some(crate::operation::UnusableMachine {
                        machine_id: machine_id.clone(),
                        reason: reason.clone(),
                    })
                }
                GlobalCandidateDisposition::Selected | GlobalCandidateDisposition::Draining => None,
            })
            .collect(),
        draining: draining_set.into_iter().collect(),
    };

    Ok(finalize_service_plan(
        service,
        service_id,
        service_placement,
        steps,
        cleanup_candidates,
    ))
}

fn finalize_service_plan(
    service: &DeployPlanningService,
    service_id: ServiceId,
    placement: DeployServicePlacement,
    steps: Vec<DeployPlanStep>,
    cleanup_candidates: Vec<ObservedCleanupCandidate>,
) -> DeploySingleServicePlan {
    let selected_containers = steps
        .iter()
        .filter_map(|step| match step {
            DeployPlanStep::UseExistingContainer { container_id, .. } => Some(container_id),
            DeployPlanStep::RunContainer { .. } => None,
        })
        .collect::<Vec<_>>();
    let volume_handoff =
        plan_volume_handoff(service, &steps, &cleanup_candidates, &selected_containers);
    let mut cleanup_actions = super::super::retention::plan_cleanup(
        cleanup_candidates,
        &selected_containers,
        service.keep(),
    );
    if let Some((_, participants)) = &volume_handoff {
        cleanup_actions.extend(participants.as_slice().iter().map(|participant| {
            DeployCleanupAction::RemoveContainer {
                target: participant.target.clone(),
            }
        }));
    }
    cleanup_actions.sort_by(|left, right| {
        left.target()
            .machine_id
            .cmp(&right.target().machine_id)
            .then_with(|| left.target().container_id.cmp(&right.target().container_id))
    });
    cleanup_actions.dedup_by(|left, right| {
        left.target().machine_id == right.target().machine_id
            && left.target().container_id == right.target().container_id
    });
    let pre_start = service.pre_start().and_then(|_| {
        steps.iter().find_map(|step| match step {
            DeployPlanStep::RunContainer { machine_id, .. } => Some(PreStartHookStep {
                machine_id: machine_id.clone(),
            }),
            DeployPlanStep::UseExistingContainer { .. } => None,
        })
    });
    DeploySingleServicePlan {
        service_id,
        placement,
        work: match volume_handoff {
            Some((replacement, participants)) => DeployServiceWork::VolumeHandoff {
                replacement,
                remaining_steps: steps.into_iter().skip(1).collect(),
                participants,
            },
            None => DeployServiceWork::Ordinary { steps },
        },
        pre_start,
        cleanup_actions,
    }
}

fn plan_volume_handoff(
    service: &DeployPlanningService,
    steps: &[DeployPlanStep],
    cleanup_candidates: &[ObservedCleanupCandidate],
    selected_containers: &[&ContainerId],
) -> Option<(DeployRunContainerStep, NonEmptyVolumeHandoffParticipants)> {
    let mut volume_names = service
        .runtime()
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<Vec<_>>();
    volume_names.sort();
    volume_names.dedup();
    if volume_names.is_empty() {
        return None;
    }

    let (replacement_index, replacement) =
        steps
            .iter()
            .enumerate()
            .find_map(|(index, step)| match step {
                DeployPlanStep::RunContainer { machine_id, slot } => Some((
                    index,
                    DeployRunContainerStep {
                        machine_id: machine_id.clone(),
                        slot: *slot,
                    },
                )),
                DeployPlanStep::UseExistingContainer { .. } => None,
            })?;
    if replacement_index != 0 {
        return None;
    }
    let machine_id = &replacement.machine_id;
    let mut superseded = cleanup_candidates
        .iter()
        .filter_map(|candidate| {
            if candidate.target.machine_id != *machine_id
                || candidate.target.identity.service_id != *service.service_id()
                || selected_containers.contains(&&candidate.target.container_id)
            {
                return None;
            }
            let shared_volume_names = NonEmptyVolumeNames::try_new(
                volume_names
                    .iter()
                    .filter(|name| candidate.named_volume_names.contains(name.as_str()))
                    .cloned(),
            )
            .ok()?;
            Some(DeployVolumeHandoffParticipant {
                target: candidate.target.clone(),
                prior_state: if candidate.state.is_running() {
                    DeployVolumeHandoffPriorState::Running
                } else {
                    DeployVolumeHandoffPriorState::Stopped
                },
                shared_volume_names,
            })
        })
        .collect::<Vec<_>>();
    superseded.sort_by(|left, right| {
        left.target
            .machine_id
            .cmp(&right.target.machine_id)
            .then_with(|| left.target.container_id.cmp(&right.target.container_id))
    });
    superseded.dedup_by(|left, right| {
        left.target.machine_id == right.target.machine_id
            && left.target.container_id == right.target.container_id
    });
    Some((
        replacement,
        NonEmptyVolumeHandoffParticipants::try_new(superseded).ok()?,
    ))
}

fn normalize_existing_replicas(replicas: &mut Vec<ExistingServiceReplica>) {
    replicas.sort_by(|left, right| {
        left.machine_id
            .cmp(&right.machine_id)
            .then_with(|| left.container_id.cmp(&right.container_id))
    });
    replicas.dedup_by(|left, right| {
        left.machine_id == right.machine_id && left.container_id == right.container_id
    });
}

pub(super) fn replicated_slot(number: u16) -> ReplicaSlot {
    ReplicaSlot::Replicated {
        number: ReplicatedReplicaSlot::try_new(number).expect("planner emits positive slots"),
    }
}
