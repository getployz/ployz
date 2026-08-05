use super::*;

pub(super) fn plan_deploy_service(
    target: &DeployPlanningTarget,
    input: DeployPlanningInput,
    volume_plan: &VolumePlan,
    placement_load: &mut MachinePlacementLoad,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let service = planning_service(target, &input.service_id)?;
    let DeployPlanningInput {
        service_id,
        placement,
        existing_replicas,
        cleanup_candidates,
        volume_pins: _,
    } = input;
    let planning = ServicePlanning {
        service,
        service_id,
        existing_replicas,
        cleanup_candidates,
        placement_load,
    };
    match (service.mode(), placement) {
        (
            ServiceMode::Replicated { replicas },
            DeployPlanningPlacementInput::Replicated { eligible_machines },
        ) => plan_replicated_service(planning, eligible_machines, volume_plan, replicas),
        (ServiceMode::Global, DeployPlanningPlacementInput::Global(placement)) => {
            plan_global_service(planning, placement)
        }
        _ => Err(DeployPlanError::PlacementModeMismatch {
            service_id: planning.service_id,
        }),
    }
}

/// The service-scoped inputs both placement modes consume: what the service is,
/// what is already observed of it, and the running placement count a new
/// container advances.
struct ServicePlanning<'a> {
    service: &'a DeployPlanningService,
    service_id: ServiceId,
    existing_replicas: Vec<ExistingServiceReplica>,
    cleanup_candidates: Vec<ObservedCleanupCandidate>,
    placement_load: &'a mut MachinePlacementLoad,
}

fn plan_replicated_service(
    planning: ServicePlanning<'_>,
    eligible_machines: Vec<MachineId>,
    volume_plan: &VolumePlan,
    replicas: ReplicaCount,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let ServicePlanning {
        service,
        service_id,
        mut existing_replicas,
        cleanup_candidates,
        placement_load,
    } = planning;
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
    let superseded = superseded_machines(&cleanup_candidates, &reused_containers(&steps));
    for machine_id in &superseded {
        placement_load.retire(machine_id);
    }
    let run_machines = match &volume_placement.machine_id {
        Some(machine_id) => {
            let pinned = vec![machine_id.clone(); missing_replicas];
            for machine_id in &pinned {
                placement_load.record(machine_id);
            }
            pinned
        }
        None => balanced_placements(
            &eligible_machines,
            &superseded,
            missing_replicas,
            placement_load,
        ),
    };
    steps.extend(
        run_machines
            .into_iter()
            .enumerate()
            .map(|(index, machine_id)| DeployPlanStep::RunContainer {
                machine_id,
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
    planning: ServicePlanning<'_>,
    placement: GlobalPlanningInput,
) -> Result<DeploySingleServicePlan, DeployPlanError> {
    let ServicePlanning {
        service,
        service_id,
        mut existing_replicas,
        mut cleanup_candidates,
        placement_load,
    } = planning;
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
    for machine_id in &superseded_machines(&cleanup_candidates, &reused_containers(&steps)) {
        placement_load.retire(machine_id);
    }
    for step in &steps {
        match step {
            DeployPlanStep::RunContainer { machine_id, .. } => placement_load.record(machine_id),
            DeployPlanStep::UseExistingContainer { .. } => {}
        }
    }

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
                    .filter(|name| candidate.named_volume_names.contains(*name))
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

/// Container ids the plan keeps. A reused container is not superseded: its
/// replica already exists, so it is neither retired from the projected count
/// nor a predecessor a further replica may follow onto the same machine.
fn reused_containers(steps: &[DeployPlanStep]) -> BTreeSet<ContainerId> {
    steps
        .iter()
        .filter_map(|step| match step {
            DeployPlanStep::UseExistingContainer { container_id, .. } => Some(container_id.clone()),
            DeployPlanStep::RunContainer { .. } => None,
        })
        .collect()
}

/// Machines whose running containers this plan supersedes. They are counted in
/// the observed load today and gone once the deploy completes, so the
/// projection must lose them before it gains their replacements — otherwise a
/// replacement reads as growth and pushes later services off a machine that is
/// not actually getting busier.
fn superseded_machines(
    cleanup_candidates: &[ObservedCleanupCandidate],
    reused_containers: &BTreeSet<ContainerId>,
) -> Vec<MachineId> {
    cleanup_candidates
        .iter()
        .filter(|candidate| {
            candidate.state.is_running()
                && !reused_containers.contains(&candidate.target.container_id)
        })
        .map(|candidate| candidate.target.machine_id.clone())
        .collect()
}

/// Machines for replicas the plan must create. A replica follows a container it
/// supersedes, so an ordinary redeploy leaves a service on the machine it
/// already runs on and only Rebalance relocates a running service. A replica
/// with no predecessor lands on the eligible machine carrying the fewest placed
/// containers, so a namespace spreads instead of every service landing on
/// whichever machine sorts first.
fn balanced_placements(
    eligible_machines: &[MachineId],
    superseded: &[MachineId],
    missing_replicas: usize,
    placement_load: &mut MachinePlacementLoad,
) -> Vec<MachineId> {
    let mut predecessors = superseded
        .iter()
        .filter(|machine_id| eligible_machines.contains(machine_id))
        .cloned()
        .collect::<Vec<_>>();
    predecessors.sort();
    let mut predecessors = predecessors.into_iter();
    let mut placements = Vec::with_capacity(missing_replicas);
    for _ in 0..missing_replicas {
        let Some(machine_id) = predecessors
            .next()
            .or_else(|| placement_load.least_loaded(eligible_machines))
        else {
            break;
        };
        placement_load.record(&machine_id);
        placements.push(machine_id);
    }
    placements
}

pub(super) fn replicated_slot(number: u16) -> ReplicaSlot {
    ReplicaSlot::Replicated {
        number: ReplicatedReplicaSlot::try_new(number).expect("planner emits positive slots"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{NamespaceRevisionEntryId, OperationId, StepId};
    use crate::machine::runtime::{
        ContainerHealth, ContainerRuntimeState, MachineContainerObservationSnapshot,
        ManagedContainerIdentity, ManagedContainerKind, ManagedContainerObservation,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn machine(name: &str) -> MachineId {
        MachineId::try_new(name).expect("machine id")
    }

    fn container(machine_name: &str, service: &str) -> ContainerId {
        ContainerId::try_new(format!("ctr_{machine_name}_{service}")).expect("container id")
    }

    fn identity(service: &str) -> ManagedContainerIdentity {
        ManagedContainerIdentity {
            namespace_id: NamespaceId::try_new("default").expect("namespace id"),
            service_id: ServiceId::try_new(service).expect("service id"),
            namespace_revision_entry_id: NamespaceRevisionEntryId::try_new("entry_1")
                .expect("entry id"),
            operation_id: OperationId::try_new("op_1").expect("operation id"),
            step_id: StepId::try_new("step_1").expect("step id"),
            kind: ManagedContainerKind::Service,
        }
    }

    fn running() -> ContainerRuntimeState {
        ContainerRuntimeState::Running {
            ip: None,
            health: ContainerHealth::default(),
            started_at_unix_ms: None,
        }
    }

    fn candidate(machine_name: &str, service: &str) -> ObservedCleanupCandidate {
        ObservedCleanupCandidate {
            target: DeployCleanupContainer {
                machine_id: machine(machine_name),
                container_id: container(machine_name, service),
                identity: identity(service),
            },
            state: running(),
            named_volume_names: BTreeSet::new(),
            created_at_unix_seconds: None,
            observed_image_identity: None,
        }
    }

    fn observation(machine_name: &str, service: &str) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine(machine_name),
            container_id: container(machine_name, service),
            identity: identity(service),
            state: running(),
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
            named_volume_names: BTreeSet::new(),
        }
    }

    fn three_machines() -> Vec<MachineId> {
        vec![
            machine("machine_a"),
            machine("machine_b"),
            machine("machine_c"),
        ]
    }

    #[test]
    fn single_replica_services_spread_instead_of_stacking_on_the_first_machine() {
        let eligible = three_machines();
        let mut load = MachinePlacementLoad::new(BTreeMap::new());

        let placed = (0..6)
            .map(|_| {
                let placements = balanced_placements(&eligible, &[], 1, &mut load);
                let [machine_id] = placements.as_slice() else {
                    panic!("one replica yields one placement");
                };
                machine_id.clone()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            placed,
            vec![
                machine("machine_a"),
                machine("machine_b"),
                machine("machine_c"),
                machine("machine_a"),
                machine("machine_b"),
                machine("machine_c"),
            ]
        );
    }

    #[test]
    fn a_replica_follows_the_container_it_supersedes() {
        let eligible = three_machines();
        // machine_a is the busiest, so balance alone would place elsewhere.
        let mut load = MachinePlacementLoad::new(BTreeMap::from([(machine("machine_a"), 9)]));

        let placements = balanced_placements(&eligible, &[machine("machine_a")], 1, &mut load);

        assert_eq!(placements, vec![machine("machine_a")]);
    }

    #[test]
    fn replicas_beyond_their_predecessors_fall_back_to_the_least_loaded_machine() {
        let eligible = three_machines();
        let mut load = MachinePlacementLoad::new(BTreeMap::from([
            (machine("machine_a"), 4),
            (machine("machine_b"), 1),
        ]));

        let placements = balanced_placements(&eligible, &[machine("machine_a")], 3, &mut load);

        assert_eq!(
            placements,
            vec![
                machine("machine_a"),
                machine("machine_c"),
                machine("machine_b")
            ]
        );
    }

    #[test]
    fn a_predecessor_on_an_ineligible_machine_does_not_hold_a_replica_there() {
        let eligible = vec![machine("machine_b")];
        let mut load = MachinePlacementLoad::new(BTreeMap::new());

        let placements =
            balanced_placements(&eligible, &[machine("machine_draining")], 1, &mut load);

        assert_eq!(placements, vec![machine("machine_b")]);
    }

    #[test]
    fn a_container_the_plan_reuses_is_not_superseded() {
        let reused = BTreeSet::from([container("machine_b", "api")]);

        assert_eq!(
            superseded_machines(&[candidate("machine_b", "api")], &reused),
            Vec::<MachineId>::new()
        );
        assert_eq!(
            superseded_machines(&[candidate("machine_b", "api")], &BTreeSet::new()),
            vec![machine("machine_b")]
        );
    }

    #[test]
    fn replacing_a_container_leaves_the_projected_count_unchanged() {
        let eligible = vec![machine("machine_a"), machine("machine_b")];
        // machine_a runs three services; every one of them is being replaced.
        let mut load = MachinePlacementLoad::new(BTreeMap::from([(machine("machine_a"), 3)]));

        for _ in 0..3 {
            load.retire(&machine("machine_a"));
            let placements = balanced_placements(&eligible, &[machine("machine_a")], 1, &mut load);
            assert_eq!(placements, vec![machine("machine_a")]);
        }

        // Six new services then split evenly rather than all fleeing to machine_b.
        let placed = (0..6)
            .map(|_| {
                let placements = balanced_placements(&eligible, &[], 1, &mut load);
                let [machine_id] = placements.as_slice() else {
                    panic!("one replica yields one placement");
                };
                machine_id.clone()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            placed
                .iter()
                .filter(|id| **id == machine("machine_a"))
                .count(),
            2
        );
        assert_eq!(
            placed
                .iter()
                .filter(|id| **id == machine("machine_b"))
                .count(),
            4
        );
    }

    #[test]
    fn placement_load_counts_running_service_containers_across_namespaces() {
        let snapshots = vec![
            MachineContainerObservationSnapshot::try_new(
                machine("machine_a"),
                [
                    observation("machine_a", "api"),
                    observation("machine_a", "web"),
                ],
            )
            .expect("snapshot"),
            MachineContainerObservationSnapshot::try_new(
                machine("machine_b"),
                [observation("machine_b", "api")],
            )
            .expect("snapshot"),
        ];

        let load = observed_placement_load(&snapshots);
        let eligible = vec![machine("machine_a"), machine("machine_b")];

        assert_eq!(load.least_loaded(&eligible), Some(machine("machine_b")));
    }
}
