use crate::deploy::participant::{self, DeployParticipantClient};
use crate::deploy::participant_set::ParticipantSet;
use crate::deploy::plan::{ResolvedPlan, VolumeChange, volume_record_change};
use crate::error::{DeployError, Error, Result};
use crate::model::{DeployEvent, DeployId, DeployPhaseId, InstancePhase, MachineId};
use ployz_spec::{VolumeCloneConsistency, VolumeCloneDataPolicy};
use std::collections::{BTreeMap, BTreeSet};
use tracing::warn;

#[derive(Debug, Clone)]
pub(super) struct ExecutedVolumeMove {
    pub(super) volume_name: String,
    pub(super) from_machine: MachineId,
    pub(super) to_machine: MachineId,
    pub(super) phase_id: DeployPhaseId,
    pub(super) snapshot_name: String,
    pub(super) snapshot_guid: u64,
    pub(super) bytes_transferred: u64,
}

#[derive(Debug, Clone)]
pub(super) struct ExecutedVolumeClone {
    pub(super) volume_name: String,
    pub(super) source_namespace: ployz_spec::Namespace,
    pub(super) source_volume: String,
    pub(super) source_machine: MachineId,
    pub(super) target_machine: MachineId,
    pub(super) data_policy: VolumeCloneDataPolicy,
    pub(super) consistency: VolumeCloneConsistency,
    pub(super) phase_id: DeployPhaseId,
    pub(super) snapshot_name: String,
    pub(super) snapshot_guid: u64,
    pub(super) target_dataset: String,
}

pub(super) fn ensure_volume_move_execution_supported(
    participant_client: &dyn DeployParticipantClient,
    plan: &ResolvedPlan,
) -> Result<()> {
    if participant_client.supports_volume_moves() {
        return Ok(());
    }
    if let Some(volume) = plan
        .volumes()
        .iter()
        .find(|volume| matches!(volume_record_change(volume), VolumeChange::Move))
    {
        return Err(Error::Deploy(DeployError::VolumeMoveExecutionUnsupported {
            volume: volume.declaration.name.clone(),
        }));
    }
    Ok(())
}

pub(super) fn ensure_volume_clone_execution_supported(
    participant_client: &dyn DeployParticipantClient,
    plan: &ResolvedPlan,
) -> Result<()> {
    if participant_client.supports_volume_clones() {
        return Ok(());
    }
    if let Some(volume) = plan.volumes().iter().find(|volume| {
        volume.clone_source().is_some()
            && matches!(volume_record_change(volume), VolumeChange::Create)
    }) {
        return Err(Error::Deploy(
            DeployError::VolumeCloneExecutionUnsupported {
                volume: volume.declaration.name.clone(),
            },
        ));
    }
    Ok(())
}

pub(super) async fn execute_volume_moves(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
    phase_id: &DeployPhaseId,
    included_volumes: Option<&BTreeSet<String>>,
) -> Result<VolumeMoveExecution> {
    let mut events = Vec::new();
    let mut movements = BTreeMap::new();
    for volume in plan
        .volumes()
        .iter()
        .filter(|volume| matches!(volume_record_change(volume), VolumeChange::Move))
    {
        if let Some(included_volumes) = included_volumes
            && !included_volumes.contains(&volume.declaration.name)
        {
            continue;
        }
        let Some(movement) = volume.movement() else {
            continue;
        };
        participants.get(&movement.from_machine)?;
        participants.get(&movement.to_machine)?;

        let mut stopped_writer_events =
            stop_volume_writers(participant_client, participants, plan, volume, "moving").await?;
        events.append(&mut stopped_writer_events);

        let snapshot = volume_move_snapshot_name(plan.manifest_hash(), &volume.declaration.name);
        events.push(DeployEvent {
            step: "move_volume".into(),
            message: format!(
                "moving volume {} from {} to {} using snapshot {}",
                volume.declaration.name, movement.from_machine, movement.to_machine, snapshot
            ),
        });

        let result = participant_client
            .move_volume(
                &movement.from_machine,
                plan.namespace(),
                participants.deploy_id(),
                participant::MoveVolumeRequest {
                    volume: volume.declaration.name.clone(),
                    from_machine: movement.from_machine.clone(),
                    to_machine: movement.to_machine.clone(),
                    snapshot: snapshot.clone(),
                },
            )
            .await?;
        events.push(DeployEvent {
            step: "move_volume".into(),
            message: format!(
                "moved volume {} to {} with snapshot {} guid {} ({} bytes)",
                volume.declaration.name,
                movement.to_machine,
                result.snapshot,
                result.snapshot_guid,
                result.bytes_transferred
            ),
        });
        movements.insert(
            volume.declaration.name.clone(),
            ExecutedVolumeMove {
                volume_name: volume.declaration.name.clone(),
                from_machine: movement.from_machine.clone(),
                to_machine: movement.to_machine.clone(),
                phase_id: phase_id.clone(),
                snapshot_name: result.snapshot,
                snapshot_guid: result.snapshot_guid,
                bytes_transferred: result.bytes_transferred,
            },
        );
    }
    Ok(VolumeMoveExecution { events, movements })
}

pub(super) async fn execute_volume_clones(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
    phase_id: &DeployPhaseId,
    included_volumes: Option<&BTreeSet<String>>,
    stopped_uncommitted_instance_ids: &mut BTreeSet<String>,
) -> Result<VolumeCloneExecution> {
    let mut events = Vec::new();
    let mut branches = BTreeMap::new();
    let clone_volumes = plan
        .volumes()
        .iter()
        .filter(|volume| {
            volume.clone_source().is_some()
                && matches!(volume_record_change(volume), VolumeChange::Create)
        })
        .filter(|volume| {
            included_volumes
                .map(|included| included.contains(&volume.declaration.name))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let clone_volume_names = clone_volumes
        .iter()
        .map(|volume| volume.declaration.name.clone())
        .collect::<Vec<_>>();
    if !clone_volume_names.is_empty() {
        events.push(DeployEvent {
            step: "preflight_clone_replacement".into(),
            message: format!(
                "preflighting clone replacement for volumes {} by draining uncommitted namespace instances",
                clone_volume_names.join(", ")
            ),
        });
        let mut stopped_instance_events =
            stop_uncommitted_namespace_instances_before_volume_clones(
                participant_client,
                participants,
                plan,
                &clone_volume_names,
                stopped_uncommitted_instance_ids,
            )
            .await?;
        events.append(&mut stopped_instance_events);
    }

    for volume in clone_volumes {
        let Some(clone_source) = volume.clone_source() else {
            continue;
        };
        participants.get(&clone_source.source_machine)?;
        participants.get(&volume.machine_id)?;

        let snapshot = volume_clone_snapshot_name(
            participants.deploy_id(),
            plan.manifest_hash(),
            &volume.declaration.name,
        );
        events.push(DeployEvent {
            step: "clone_volume".into(),
            message: format!(
                "cloning volume {} from {}/{} on {} using snapshot {} ({:?}, {:?})",
                volume.declaration.name,
                clone_source.source_namespace,
                clone_source.source_volume,
                clone_source.source_machine,
                snapshot,
                clone_source.data_policy,
                clone_source.consistency
            ),
        });

        let result = match participant_client
            .clone_volume(
                &clone_source.source_machine,
                plan.namespace(),
                participants.deploy_id(),
                participant::CloneVolumeRequest {
                    volume: volume.declaration.name.clone(),
                    source_namespace: clone_source.source_namespace.clone(),
                    source_volume: clone_source.source_volume.clone(),
                    snapshot: snapshot.clone(),
                    quota: volume.declaration.quota.to_string(),
                    mode: volume.declaration.mode.to_string(),
                    owner: volume.declaration.owner.to_string(),
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let mut cleanup_branches = branches.clone();
                cleanup_branches.insert(
                    volume.declaration.name.clone(),
                    ExecutedVolumeClone {
                        volume_name: volume.declaration.name.clone(),
                        source_namespace: clone_source.source_namespace.clone(),
                        source_volume: clone_source.source_volume.clone(),
                        source_machine: clone_source.source_machine.clone(),
                        target_machine: volume.machine_id.clone(),
                        data_policy: clone_source.data_policy,
                        consistency: clone_source.consistency,
                        phase_id: phase_id.clone(),
                        snapshot_name: snapshot.clone(),
                        snapshot_guid: 0,
                        target_dataset: String::new(),
                    },
                );
                let cleanup_errors = cleanup_uncommitted_volume_clones(
                    participant_client,
                    participants,
                    plan.namespace(),
                    participants.deploy_id(),
                    &cleanup_branches,
                )
                .await;
                return Err(error_with_clone_cleanup_failures(error, cleanup_errors));
            }
        };
        events.push(DeployEvent {
            step: "clone_volume".into(),
            message: format!(
                "cloned volume {} from {}/{} with snapshot {} guid {}",
                volume.declaration.name,
                clone_source.source_namespace,
                clone_source.source_volume,
                result.snapshot,
                result.snapshot_guid
            ),
        });
        branches.insert(
            volume.declaration.name.clone(),
            ExecutedVolumeClone {
                volume_name: volume.declaration.name.clone(),
                source_namespace: clone_source.source_namespace.clone(),
                source_volume: clone_source.source_volume.clone(),
                source_machine: clone_source.source_machine.clone(),
                target_machine: volume.machine_id.clone(),
                data_policy: clone_source.data_policy,
                consistency: clone_source.consistency,
                phase_id: phase_id.clone(),
                snapshot_name: result.snapshot,
                snapshot_guid: result.snapshot_guid,
                target_dataset: result.target_dataset,
            },
        );
    }
    Ok(VolumeCloneExecution { events, branches })
}

pub(super) struct VolumeMoveExecution {
    pub(super) events: Vec<DeployEvent>,
    pub(super) movements: BTreeMap<String, ExecutedVolumeMove>,
}

pub(super) struct VolumeCloneExecution {
    pub(super) events: Vec<DeployEvent>,
    pub(super) branches: BTreeMap<String, ExecutedVolumeClone>,
}

pub(super) async fn cleanup_uncommitted_volume_clones(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    namespace: &ployz_spec::Namespace,
    deploy_id: &DeployId,
    branches: &BTreeMap<String, ExecutedVolumeClone>,
) -> Vec<String> {
    let mut cleanup_errors = Vec::new();
    for branch in branches.values() {
        if let Err(error) = participant_client
            .cleanup_volume_clone(
                &branch.target_machine,
                namespace,
                deploy_id,
                participant::CleanupVolumeCloneRequest {
                    volume: branch.volume_name.clone(),
                    source_namespace: branch.source_namespace.clone(),
                    source_volume: branch.source_volume.clone(),
                    snapshot: branch.snapshot_name.clone(),
                },
            )
            .await
        {
            warn!(
                ?error,
                deploy_id = %deploy_id,
                volume = %branch.volume_name,
                machine_id = %branch.target_machine,
                "failed to clean up uncommitted cloned volume after phase failure"
            );
            cleanup_errors.push(format!("{}: {error}", branch.volume_name));
        } else if participants.get(&branch.target_machine).is_err() {
            warn!(
                deploy_id = %deploy_id,
                volume = %branch.volume_name,
                machine_id = %branch.target_machine,
                "cleaned up cloned volume on machine outside participant set"
            );
        }
    }
    cleanup_errors
}

pub(super) fn error_with_clone_cleanup_failures(
    error: Error,
    cleanup_errors: Vec<String>,
) -> Error {
    if cleanup_errors.is_empty() {
        return error;
    }
    Error::operation(
        "deploy_apply",
        format!(
            "{error}; uncommitted volume clone cleanup failed: {}",
            cleanup_errors.join("; ")
        ),
    )
}

async fn stop_volume_writers(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
    moving_volume: &crate::deploy::plan::PlannedVolume,
    operation: &str,
) -> Result<Vec<DeployEvent>> {
    let mut current_instances = BTreeMap::new();
    let writer_services = moving_volume
        .attached_services
        .iter()
        .chain(
            moving_volume
                .current()
                .into_iter()
                .flat_map(|record| record.attached_services.iter()),
        )
        .cloned()
        .collect::<BTreeSet<_>>();
    for status in participants.instances() {
        if status.namespace == *plan.namespace()
            && writer_services.contains(&status.service)
            && !matches!(status.phase, InstancePhase::Failed | InstancePhase::Removed)
        {
            current_instances.insert(
                status.instance_id.as_str().to_string(),
                (
                    status.instance_id.clone(),
                    status.machine_id.clone(),
                    status.service.clone(),
                ),
            );
        }
    }
    for writer in &moving_volume.current_writer_slots {
        current_instances.insert(
            writer.slot.active_instance_id.as_str().to_string(),
            (
                writer.slot.active_instance_id.clone(),
                writer.slot.machine_id.clone(),
                writer.service.clone(),
            ),
        );
    }

    let mut events = Vec::new();
    for (_instance_id_key, (instance_id, machine_id, service)) in current_instances {
        participants.get(&machine_id)?;
        participant_client
            .drain_instance(
                &machine_id,
                plan.namespace(),
                participants.deploy_id(),
                &instance_id,
            )
            .await?;
        events.push(DeployEvent {
            step: "stop_volume_writer".into(),
            message: format!(
                "drained writer instance {} for service {} before {} volume {}",
                instance_id, service, operation, moving_volume.declaration.name
            ),
        });
        participant_client
            .remove_instance(
                &machine_id,
                plan.namespace(),
                participants.deploy_id(),
                &instance_id,
            )
            .await?;
        events.push(DeployEvent {
            step: "stop_volume_writer".into(),
            message: format!(
                "removed writer instance {} for service {} before {} volume {}",
                instance_id, service, operation, moving_volume.declaration.name
            ),
        });
    }
    Ok(events)
}

async fn stop_uncommitted_namespace_instances_before_volume_clones(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
    cloned_volume_names: &[String],
    stopped_uncommitted_instance_ids: &mut BTreeSet<String>,
) -> Result<Vec<DeployEvent>> {
    let cloned_volumes = cloned_volume_names.join(", ");
    let committed_instance_ids = plan
        .services()
        .iter()
        .flat_map(|service| service.slots.iter())
        .filter_map(|slot| slot.current())
        .map(|slot| slot.active_instance_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut current_instances = BTreeMap::new();
    for status in participants.instances() {
        if status.namespace == *plan.namespace()
            && !committed_instance_ids.contains(status.instance_id.as_str())
            && !stopped_uncommitted_instance_ids.contains(status.instance_id.as_str())
            && !matches!(status.phase, InstancePhase::Failed | InstancePhase::Removed)
        {
            current_instances.insert(
                status.instance_id.as_str().to_string(),
                (
                    status.instance_id.clone(),
                    status.machine_id.clone(),
                    status.service.clone(),
                ),
            );
        }
    }
    let mut events = Vec::new();
    for (_instance_id_key, (instance_id, machine_id, service)) in current_instances {
        participants.get(&machine_id)?;
        participant_client
            .drain_instance(
                &machine_id,
                plan.namespace(),
                participants.deploy_id(),
                &instance_id,
            )
            .await?;
        stopped_uncommitted_instance_ids.insert(instance_id.as_str().to_string());
        events.push(DeployEvent {
            step: "stop_uncommitted_instance".into(),
            message: format!(
                "drained uncommitted instance {} for service {} before cloning volumes {}",
                instance_id, service, cloned_volumes
            ),
        });
        participant_client
            .remove_instance(
                &machine_id,
                plan.namespace(),
                participants.deploy_id(),
                &instance_id,
            )
            .await?;
        stopped_uncommitted_instance_ids.insert(instance_id.as_str().to_string());
        events.push(DeployEvent {
            step: "stop_uncommitted_instance".into(),
            message: format!(
                "removed uncommitted instance {} for service {} before cloning volumes {}",
                instance_id, service, cloned_volumes
            ),
        });
    }
    Ok(events)
}

fn volume_move_snapshot_name(manifest_hash: &str, volume: &str) -> String {
    format!("ployz-move-{manifest_hash}-{volume}")
}

fn volume_clone_snapshot_name(deploy_id: &DeployId, manifest_hash: &str, volume: &str) -> String {
    format!(
        "ployz-clone-{}-{manifest_hash}-{volume}",
        deploy_id.as_str()
    )
}
