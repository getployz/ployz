use crate::certificates::{
    AcmeAccountCoordinator, AcmeIssuerFactory, Http01ChallengeReadiness, IssuanceCoordinator,
    LocalHttp01ChallengeReadiness, NoopAcmeAccountCoordinator, NoopAcmeIssuerFactory,
    NoopIssuanceCoordinator, spawn_certificate_finalization_with_coordination,
    start_pending_orders,
};
use crate::deploy::lifecycle::{CleanupPlan, PreparedDeploy};
use crate::deploy::managed_domains;
use crate::deploy::participant::{self, DeployParticipantClient};
use crate::deploy::plan::{
    PlanFingerprint, ResolvedPlan, VolumeChange, resolve_plan, volume_record_change,
};
use crate::deploy::probe::{NoopParticipantProbe, ParticipantProbe, probe_participants};
use crate::error::{DeployError, Error, Result};
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployPreview, DeployRecord,
    DeployState, InstanceId, InstanceStatusRecord, MachineId, MachineMembership, VolumeRecord,
};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use ployz_store_api::{DeployStore, InstanceStatusStore, StoreDriver};
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const PARTICIPANT_INSPECT_CONCURRENCY: usize = 64;
const PHASE_MACHINE_CONCURRENCY: usize = 64;

#[derive(Debug, Clone)]
struct StartTask {
    service: String,
    slot_id: crate::model::SlotId,
    machine_id: MachineId,
    instance_id: InstanceId,
    spec_json: String,
    volumes_json: String,
}

#[derive(Debug)]
struct MachineStartupResult {
    machine_id: MachineId,
    events: Vec<DeployEvent>,
    started: Vec<StartedSlot>,
}

#[derive(Debug, Clone)]
struct StartedSlot {
    service: String,
    slot_id: crate::model::SlotId,
    status: InstanceStatusRecord,
}

#[derive(Debug, Default)]
pub(super) struct PhaseStartupResult {
    events: Vec<DeployEvent>,
    pub(super) started: HashMap<(String, String), InstanceStatusRecord>,
}

#[derive(Debug)]
struct CleanupResult {
    events: Vec<DeployEvent>,
    errors: Vec<String>,
}

struct InspectedParticipant {
    participant: MachineId,
    instance_count: usize,
}

pub(super) struct ParticipantSet {
    machines: BTreeMap<MachineId, MachineMembership>,
    namespace: ployz_types::spec::Namespace,
    deploy_id: DeployId,
}

impl ParticipantSet {
    pub(super) async fn inspect(
        participant_client: &dyn DeployParticipantClient,
        plan: &ResolvedPlan,
        local_machine_id: &MachineId,
        deploy_id: &DeployId,
    ) -> Result<(Self, Vec<DeployEvent>)> {
        let sorted_participants = plan.participants().iter().cloned().collect::<Vec<_>>();
        let namespace = plan.namespace().clone();
        let inspected: Vec<InspectedParticipant> = stream::iter(sorted_participants.into_iter())
            .map(|participant| {
                let machine = plan.machine_map().get(&participant).cloned();
                let namespace = namespace.clone();
                let deploy_id = deploy_id.clone();
                async move {
                    let Some(machine) = machine else {
                        return Err(Error::Deploy(DeployError::ParticipantMissing {
                            machine_id: participant.0,
                        }));
                    };
                    let instances = participant_client
                        .inspect_namespace(&machine, &namespace, &deploy_id, local_machine_id)
                        .await?;
                    Ok(InspectedParticipant {
                        participant,
                        instance_count: instances.len(),
                    })
                }
            })
            .buffer_unordered(PARTICIPANT_INSPECT_CONCURRENCY)
            .try_collect()
            .await?;

        let mut inspected = inspected;
        inspected.sort_by(|left, right| left.participant.0.cmp(&right.participant.0));

        let machines = plan
            .participants()
            .iter()
            .map(|machine_id| {
                let machine = plan.machine_map().get(machine_id).cloned().ok_or_else(|| {
                    Error::Deploy(DeployError::ParticipantMissing {
                        machine_id: machine_id.0.clone(),
                    })
                })?;
                Ok((machine_id.clone(), machine))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut events = Vec::new();
        for inspected in inspected {
            events.push(DeployEvent {
                step: "inspect".into(),
                message: format!(
                    "inspected '{}' ({} instances)",
                    inspected.participant, inspected.instance_count
                ),
            });
        }

        Ok((
            Self {
                machines,
                namespace,
                deploy_id: deploy_id.clone(),
            },
            events,
        ))
    }

    fn get(&self, machine_id: &MachineId) -> Result<&MachineMembership> {
        self.machines.get(machine_id).ok_or_else(|| {
            Error::Deploy(DeployError::ParticipantMissing {
                machine_id: machine_id.0.clone(),
            })
        })
    }
}

pub(super) async fn apply(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
) -> Result<DeployApplyResult> {
    apply_with_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
        &NoopParticipantProbe,
    )
    .await
}

pub(super) async fn apply_with_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
) -> Result<DeployApplyResult> {
    let initial_plan = resolve_plan(store, local_machine_id, manifest).await?;
    let reachability = probe_participants(
        prober,
        initial_plan.participants(),
        initial_plan.machine_map(),
    )
    .await;
    if !reachability.unreachable.is_empty() {
        let machine_ids = reachability
            .unreachable
            .iter()
            .map(|machine_id| machine_id.0.clone())
            .collect::<Vec<_>>();
        return Err(Error::Deploy(DeployError::ParticipantsUnreachable {
            unreachable_count: machine_ids.len(),
            participant_count: initial_plan.participants().len(),
            machine_ids,
        }));
    }

    apply_with_initial_plan_and_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        initial_plan,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
    )
    .await
}

#[cfg(test)]
pub(super) async fn apply_with_initial_plan(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    initial_plan: ResolvedPlan,
) -> Result<DeployApplyResult> {
    apply_with_initial_plan_and_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        initial_plan,
        Arc::new(NoopIssuanceCoordinator),
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
        Arc::new(NoopAcmeIssuerFactory::default()),
    )
    .await
}

pub(super) async fn apply_with_initial_plan_and_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    initial_plan: ResolvedPlan,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
) -> Result<DeployApplyResult> {
    let deploy_id = DeployId(Uuid::new_v4().to_string());
    let started_at = now_unix_secs();
    let initial_fingerprint = initial_plan.fingerprint();

    let (participants, mut events) = ParticipantSet::inspect(
        participant_client,
        &initial_plan,
        local_machine_id,
        &deploy_id,
    )
    .await?;

    let mut last_written_deploy_record = None;
    let result = async {
        let final_plan = resolve_plan(store, local_machine_id, manifest).await?;
        let final_fingerprint = final_plan.fingerprint();
        ensure_plan_stable(&initial_fingerprint, &final_fingerprint)?;
        managed_domains::validate_hostname_ownership(store, &final_plan).await?;

        let prepared = PreparedDeploy::new(
            deploy_id.clone(),
            started_at,
            local_machine_id.clone(),
            final_plan,
        )?;
        store
            .write_deploy_status(prepared.applying_record())
            .await?;
        last_written_deploy_record = Some(prepared.applying_record().clone());

        let startup =
            run_phase_startup(store, participant_client, &participants, prepared.plan()).await?;
        events.extend(startup.events);

        let started = prepared.into_started(startup.started);
        let committed_volumes = build_committed_volumes(
            started.plan(),
            started.started(),
            started.deploy_id(),
            started_at,
        )?;
        let removed_volumes_list = removed_volumes(store, started.plan()).await?;

        let commit_plan = started.into_commit_plan(removed_volumes_list, committed_volumes)?;
        store.commit_deploy(commit_plan.commit()).await?;
        let mut committed = commit_plan.into_committed();
        events.push(committed.commit_event());

        let managed_hostnames = managed_domains::ensure_certificate_intents(
            store,
            committed.plan(),
            issuer_factory.issuer_url(),
        )
        .await?;
        let issuer = issuer_factory.create(
            Arc::new(LocalHttp01ChallengeReadiness),
            account_coordinator.clone(),
        );
        let acme_warnings = start_pending_orders(
            store,
            issuer.as_ref(),
            certificate_coordinator.as_ref(),
            &managed_hostnames,
        )
        .await;
        let mut managed_warnings =
            managed_domains::warnings_for_plan(store, committed.plan()).await?;
        managed_warnings.extend(acme_warnings);
        committed.set_warnings(managed_warnings)?;
        store.write_deploy_status(committed.deploy_record()).await?;
        last_written_deploy_record = Some(committed.deploy_record().clone());
        spawn_certificate_finalization_with_coordination(
            store.clone(),
            issuer_factory.clone(),
            challenge_readiness.clone(),
            account_coordinator.clone(),
            certificate_coordinator.clone(),
        );

        let cleanup_plan = committed.cleanup_plan();
        let cleanup =
            cleanup_stale_instances(store, participant_client, &participants, &cleanup_plan)
                .await?;
        events.extend(cleanup.events);

        let final_state = if cleanup.errors.is_empty() {
            DeployState::Committed
        } else {
            let cleanup_pending_record = committed.cleanup_pending_record(now_unix_secs())?;
            store.write_deploy_status(&cleanup_pending_record).await?;
            for error in cleanup.errors {
                events.push(DeployEvent {
                    step: "cleanup_pending".into(),
                    message: error,
                });
            }
            DeployState::CleanupPending
        };

        Ok(DeployApplyResult {
            deploy_id: committed.deploy_id().clone(),
            preview: committed.preview().clone(),
            state: final_state,
            events,
        })
    }
    .await;

    if let Err(error) = &result
        && let Some(last_record) = last_written_deploy_record
        && last_record.state == DeployState::Applying
    {
        let failed_record = failed_deploy_record(last_record, error);
        if let Err(update_error) = store.write_deploy_status(&failed_record).await {
            warn!(
                ?update_error,
                deploy_id = %deploy_id,
                "failed to record terminal failed deploy state after apply error"
            );
        }
    }

    result
}

fn failed_deploy_record(mut record: DeployRecord, error: &Error) -> DeployRecord {
    record.state = DeployState::Failed;
    record.finished_at = Some(now_unix_secs());
    if let Ok(mut preview) = serde_json::from_str::<DeployPreview>(&record.summary_json) {
        preview.warnings.push(format!("deploy failed: {error}"));
        if let Ok(summary_json) = serde_json::to_string(&preview) {
            record.summary_json = summary_json;
        }
    }
    record
}

pub(super) async fn run_phase_startup(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
) -> Result<PhaseStartupResult> {
    let mut phase_queues: BTreeMap<u32, BTreeMap<MachineId, Vec<StartTask>>> = BTreeMap::new();
    for service in plan.services() {
        let Some(phase) = service.phase() else {
            continue;
        };
        let Some(spec_json) = service.spec_json() else {
            continue;
        };
        for slot in service.slots.iter().filter(|slot| {
            matches!(
                slot.action,
                DeployChangeKind::Create | DeployChangeKind::Replace
            )
        }) {
            phase_queues
                .entry(phase)
                .or_default()
                .entry(slot.machine_id.clone())
                .or_default()
                .push(StartTask {
                    service: service.service.clone(),
                    slot_id: slot.slot_id.clone(),
                    machine_id: slot.machine_id.clone(),
                    instance_id: InstanceId(Uuid::new_v4().to_string()),
                    spec_json: spec_json.to_string(),
                    volumes_json: plan.volumes_json().to_string(),
                });
        }
    }

    let mut result = PhaseStartupResult::default();
    for (_phase, machine_tasks) in phase_queues {
        let machine_results: Vec<MachineStartupResult> = stream::iter(machine_tasks.into_iter())
            .map(|(machine_id, tasks)| async move {
                let machine = participants.get(&machine_id)?;
                run_machine_start_queue(store, participant_client, participants, machine, tasks)
                    .await
            })
            .buffer_unordered(PHASE_MACHINE_CONCURRENCY)
            .try_collect()
            .await?;

        let mut machine_results = machine_results;
        machine_results.sort_by(|left, right| left.machine_id.0.cmp(&right.machine_id.0));
        for machine_result in machine_results {
            result.events.extend(machine_result.events);
            for started in machine_result.started {
                result
                    .started
                    .insert((started.service, started.slot_id.0.clone()), started.status);
            }
        }
    }

    Ok(result)
}

pub(super) fn ensure_plan_stable(
    initial: &PlanFingerprint,
    final_plan: &PlanFingerprint,
) -> Result<()> {
    if final_plan.participants != initial.participants {
        return Err(Error::Deploy(DeployError::ParticipantSetChanged));
    }
    if final_plan != initial {
        return Err(Error::Deploy(DeployError::ExecutionPlanChanged));
    }
    Ok(())
}

async fn run_machine_start_queue(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    machine: &MachineMembership,
    tasks: Vec<StartTask>,
) -> Result<MachineStartupResult> {
    let machine_id = tasks
        .first()
        .map(|task| task.machine_id.clone())
        .ok_or(Error::Deploy(DeployError::EmptyMachineStartQueue))?;
    let mut events = Vec::new();
    let mut started = Vec::new();

    for task in tasks {
        let status = participant_client
            .start_candidate(
                &machine.id,
                &participants.namespace,
                &participants.deploy_id,
                participant::StartCandidateRequest {
                    service: task.service.clone(),
                    slot_id: task.slot_id.clone(),
                    instance_id: task.instance_id.clone(),
                    spec_json: task.spec_json.clone(),
                    volumes_json: task.volumes_json.clone(),
                },
            )
            .await?;
        store.record_instance_status(&status).await?;
        events.push(DeployEvent {
            step: "start_candidate".into(),
            message: format!(
                "starting {} slot {} as instance {} on {}",
                task.service, task.slot_id, status.instance_id, task.machine_id
            ),
        });
        started.push(StartedSlot {
            service: task.service,
            slot_id: task.slot_id,
            status,
        });
    }

    Ok(MachineStartupResult {
        machine_id,
        events,
        started,
    })
}

async fn removed_volumes(store: &StoreDriver, plan: &ResolvedPlan) -> Result<Vec<String>> {
    let declared = plan
        .volumes()
        .iter()
        .map(|volume| volume.declaration.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut removed = store
        .list_volumes(plan.namespace())
        .await?
        .into_iter()
        .filter(|record| !declared.contains(record.volume_name.as_str()))
        .map(|record| record.volume_name)
        .collect::<Vec<_>>();
    removed.sort();
    Ok(removed)
}

fn build_committed_volumes(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
    now: u64,
) -> Result<Vec<VolumeRecord>> {
    let mut volumes = Vec::new();
    for planned in plan
        .volumes()
        .iter()
        .filter(|planned| !matches!(volume_record_change(planned), VolumeChange::Skip))
    {
        if !planned.attached_services.is_empty()
            && !planned.attached_services.iter().any(|service| {
                started
                    .keys()
                    .any(|(started_service, _)| started_service == service)
            })
        {
            return Err(Error::Deploy(DeployError::VolumeChangedWithoutRestart {
                volume: planned.declaration.name.clone(),
            }));
        }

        let created_at = planned
            .current
            .as_ref()
            .map(|record| record.created_at)
            .unwrap_or(now);
        let created_by_deploy_id = planned
            .current
            .as_ref()
            .map(|record| record.created_by_deploy_id.clone())
            .unwrap_or_else(|| deploy_id.clone());
        volumes.push(VolumeRecord {
            namespace: plan.namespace().clone(),
            volume_name: planned.declaration.name.clone(),
            scope: planned.declaration.scope,
            machine_id: planned.machine_id.clone(),
            quota: planned.declaration.quota.clone(),
            mode: planned.declaration.mode.clone(),
            owner: planned.declaration.owner.clone(),
            attached_services: planned.attached_services.clone(),
            created_at,
            created_by_deploy_id,
            last_modified_at: now,
            last_modified_by_deploy_id: deploy_id.clone(),
        });
    }
    Ok(volumes)
}

async fn cleanup_stale_instances(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &CleanupPlan,
) -> Result<CleanupResult> {
    let participant_ids = plan
        .participants()
        .iter()
        .map(|machine_id| machine_id.0.clone())
        .collect::<BTreeSet<_>>();

    let mut stale_by_machine: BTreeMap<MachineId, Vec<InstanceStatusRecord>> = BTreeMap::new();
    for status in store.list_instance_status(plan.namespace()).await? {
        if plan.active_instance_ids().contains(&status.instance_id.0) {
            continue;
        }
        if !participant_ids.contains(&status.machine_id.0) {
            continue;
        }
        stale_by_machine
            .entry(status.machine_id.clone())
            .or_default()
            .push(status);
    }

    let cleanup_results: Vec<CleanupResult> = stream::iter(stale_by_machine.into_iter())
        .map(|(machine_id, statuses)| async move {
            let Some(machine) = participants.machines.get(&machine_id) else {
                return Ok(CleanupResult {
                    events: Vec::new(),
                    errors: Vec::new(),
                });
            };
            run_machine_cleanup(participant_client, participants, machine, statuses).await
        })
        .buffer_unordered(PHASE_MACHINE_CONCURRENCY)
        .try_collect()
        .await?;

    let mut result = CleanupResult {
        events: Vec::new(),
        errors: Vec::new(),
    };
    for cleanup in cleanup_results {
        result.events.extend(cleanup.events);
        result.errors.extend(cleanup.errors);
    }
    result
        .events
        .sort_by(|left, right| left.message.cmp(&right.message));
    result.errors.sort();
    Ok(result)
}

async fn run_machine_cleanup(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    machine: &MachineMembership,
    statuses: Vec<InstanceStatusRecord>,
) -> Result<CleanupResult> {
    let mut events = Vec::new();
    let mut errors = Vec::new();

    for status in statuses {
        let drain_result = participant_client
            .drain_instance(
                &machine.id,
                &participants.namespace,
                &participants.deploy_id,
                &status.instance_id,
            )
            .await;
        if let Err(error) = drain_result {
            errors.push(error.to_string());
            continue;
        }

        let remove_result = participant_client
            .remove_instance(
                &machine.id,
                &participants.namespace,
                &participants.deploy_id,
                &status.instance_id,
            )
            .await;

        match remove_result {
            Ok(()) => events.push(DeployEvent {
                step: "cleanup".into(),
                message: format!(
                    "removed old instance {} from {}",
                    status.instance_id, status.machine_id
                ),
            }),
            Err(error) => errors.push(error.to_string()),
        }
    }

    Ok(CleanupResult { events, errors })
}
