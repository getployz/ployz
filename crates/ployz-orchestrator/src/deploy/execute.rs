use crate::deploy::plan::{PlanFingerprint, ResolvedPlan, resolve_plan};
use crate::deploy::probe::probe_participants;
use crate::deploy::session::{self, DeploySessionFactory};
use crate::deploy::transaction::{CleanupPlan, PreparedDeploy};
use crate::error::{Error, Result};
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployState, InstanceId,
    InstanceStatusRecord, MachineId, ServiceRevisionRecord,
};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use ployz_store_api::{
    DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, InstanceStatusRepository,
    StoreDriver,
};
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

const SESSION_OPEN_CONCURRENCY: usize = 64;
const PHASE_MACHINE_CONCURRENCY: usize = 64;

type SharedSession = Arc<Mutex<Option<Box<dyn session::DeploySession>>>>;

#[derive(Debug, Clone)]
struct StartTask {
    service: String,
    slot_id: crate::model::SlotId,
    machine_id: MachineId,
    instance_id: InstanceId,
    spec_json: String,
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

struct OpenedSession {
    participant: MachineId,
    instance_count: usize,
    session: Box<dyn session::DeploySession>,
}

pub(super) struct SessionSet {
    sessions: BTreeMap<MachineId, SharedSession>,
}

impl SessionSet {
    pub(super) async fn open(
        session_factory: &dyn DeploySessionFactory,
        plan: &ResolvedPlan,
        local_machine_id: &MachineId,
        deploy_id: &DeployId,
    ) -> Result<(Self, Vec<DeployEvent>)> {
        let sorted_participants = plan.participants().iter().cloned().collect::<Vec<_>>();
        let namespace = plan.namespace().clone();
        let opened: Vec<OpenedSession> = stream::iter(sorted_participants.into_iter())
            .map(|participant| {
                let machine = plan.machine_map().get(&participant).cloned();
                let namespace = namespace.clone();
                async move {
                    let Some(machine) = machine else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "participant '{}' is missing from machine inventory",
                                participant
                            ),
                        ));
                    };
                    let (session, instances) = session_factory
                        .open(&machine, &namespace, deploy_id, local_machine_id)
                        .await?;
                    Ok(OpenedSession {
                        participant,
                        instance_count: instances.len(),
                        session,
                    })
                }
            })
            .buffer_unordered(SESSION_OPEN_CONCURRENCY)
            .try_collect()
            .await?;

        let mut opened = opened;
        opened.sort_by(|left, right| left.participant.0.cmp(&right.participant.0));

        let mut sessions = BTreeMap::new();
        let mut events = Vec::new();
        for opened_session in opened {
            events.push(DeployEvent {
                step: "lock".into(),
                message: format!(
                    "acquired lock on '{}' ({} instances)",
                    opened_session.participant, opened_session.instance_count
                ),
            });
            sessions.insert(
                opened_session.participant,
                Arc::new(Mutex::new(Some(opened_session.session))),
            );
        }

        Ok((Self { sessions }, events))
    }

    fn get(&self, machine_id: &MachineId) -> Result<SharedSession> {
        self.sessions.get(machine_id).cloned().ok_or_else(|| {
            Error::operation(
                "deploy_apply",
                format!("no session was available for machine '{}'", machine_id),
            )
        })
    }

    pub(super) async fn close_all(self) {
        for (_machine_id, session) in self.sessions {
            let mut guard = session.lock().await;
            let Some(session) = guard.take() else {
                continue;
            };
            drop(guard);
            let _ = session.close().await;
        }
    }
}

pub(super) async fn apply(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
) -> Result<DeployApplyResult> {
    let initial_plan = resolve_plan(store, local_machine_id, manifest).await?;
    let reachability =
        probe_participants(initial_plan.participants(), initial_plan.machine_map()).await;
    if !reachability.unreachable.is_empty() {
        let unreachable = reachability
            .unreachable
            .iter()
            .map(|machine_id| machine_id.0.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::operation(
            "deploy_apply",
            format!(
                "deploy blocked: {} of {} participants unreachable: {}",
                reachability.unreachable.len(),
                initial_plan.participants().len(),
                unreachable
            ),
        ));
    }

    apply_with_initial_plan(
        store,
        session_factory,
        local_machine_id,
        manifest,
        initial_plan,
    )
    .await
}

pub(super) async fn apply_with_initial_plan(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    initial_plan: ResolvedPlan,
) -> Result<DeployApplyResult> {
    let deploy_id = DeployId(Uuid::new_v4().to_string());
    let started_at = now_unix_secs();
    let initial_fingerprint = initial_plan.fingerprint();

    let (sessions, mut events) =
        SessionSet::open(session_factory, &initial_plan, local_machine_id, &deploy_id).await?;

    let result = async {
        let final_plan = resolve_plan(store, local_machine_id, manifest).await?;
        let final_fingerprint = final_plan.fingerprint();
        ensure_plan_stable(&initial_fingerprint, &final_fingerprint)?;

        let prepared = PreparedDeploy::new(
            deploy_id.clone(),
            started_at,
            local_machine_id.clone(),
            final_plan,
        )?;
        store
            .update_deploy_record(&DeployRecordUpdate {
                deploy: prepared.applying_record().clone(),
            })
            .await?;

        upsert_revisions(store, prepared.revisions()).await?;
        let startup = run_phase_startup(store, &sessions, prepared.plan(), &deploy_id).await?;
        events.extend(startup.events);

        let commit_plan = prepared.into_started(startup.started).into_commit_plan()?;
        store.commit_deploy(commit_plan.commit()).await?;
        let committed = commit_plan.into_committed();
        events.push(committed.commit_event());

        let cleanup_plan = committed.cleanup_plan();
        let cleanup = cleanup_stale_instances(store, &sessions, &cleanup_plan).await?;
        events.extend(cleanup.events);

        let final_state = if cleanup.errors.is_empty() {
            DeployState::Committed
        } else {
            let cleanup_pending_record = committed.cleanup_pending_record(now_unix_secs());
            store
                .update_deploy_record(&DeployRecordUpdate {
                    deploy: cleanup_pending_record,
                })
                .await?;
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

    sessions.close_all().await;
    result
}

async fn upsert_revisions(store: &StoreDriver, revisions: &[ServiceRevisionRecord]) -> Result<()> {
    for revision in revisions {
        store
            .record_service_revision(&DeployRevisionUpsert {
                revision: revision.clone(),
            })
            .await?;
    }
    Ok(())
}

pub(super) async fn run_phase_startup(
    store: &StoreDriver,
    sessions: &SessionSet,
    plan: &ResolvedPlan,
    deploy_id: &DeployId,
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
                });
        }
    }

    let mut result = PhaseStartupResult::default();
    for (_phase, machine_tasks) in phase_queues {
        let machine_results: Vec<MachineStartupResult> = stream::iter(machine_tasks.into_iter())
            .map(|(machine_id, tasks)| async move {
                let session = sessions.get(&machine_id)?;
                run_machine_start_queue(store, session, tasks, deploy_id).await
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
        return Err(Error::operation(
            "deploy_apply",
            "participant set changed after lock acquisition; retry deploy",
        ));
    }
    if final_plan != initial {
        return Err(Error::operation(
            "deploy_apply",
            "resolved execution plan changed after lock acquisition; retry deploy",
        ));
    }
    Ok(())
}

async fn run_machine_start_queue(
    store: &StoreDriver,
    session: SharedSession,
    tasks: Vec<StartTask>,
    _deploy_id: &DeployId,
) -> Result<MachineStartupResult> {
    let machine_id = tasks
        .first()
        .map(|task| task.machine_id.clone())
        .ok_or_else(|| Error::operation("deploy_apply", "empty machine start queue"))?;
    let mut events = Vec::new();
    let mut started = Vec::new();

    for task in tasks {
        let status = {
            let mut guard = session.lock().await;
            let Some(session) = guard.as_mut() else {
                return Err(Error::operation(
                    "deploy_apply",
                    format!(
                        "session for machine '{}' was already closed",
                        task.machine_id
                    ),
                ));
            };
            session
                .start_candidate(session::StartCandidateRequest {
                    service: task.service.clone(),
                    slot_id: task.slot_id.clone(),
                    instance_id: task.instance_id.clone(),
                    spec_json: task.spec_json.clone(),
                })
                .await?
        };
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

async fn cleanup_stale_instances(
    store: &StoreDriver,
    sessions: &SessionSet,
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
            let Some(session) = sessions.sessions.get(&machine_id).cloned() else {
                return Ok(CleanupResult {
                    events: Vec::new(),
                    errors: Vec::new(),
                });
            };
            run_machine_cleanup(session, statuses).await
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
    session: SharedSession,
    statuses: Vec<InstanceStatusRecord>,
) -> Result<CleanupResult> {
    let mut events = Vec::new();
    let mut errors = Vec::new();

    for status in statuses {
        let drain_result = {
            let mut guard = session.lock().await;
            let Some(session) = guard.as_mut() else {
                return Err(Error::operation(
                    "deploy_apply",
                    format!(
                        "session for machine '{}' was already closed",
                        status.machine_id
                    ),
                ));
            };
            session.drain_instance(&status.instance_id).await
        };
        if let Err(error) = drain_result {
            errors.push(error.to_string());
            continue;
        }

        let remove_result = {
            let mut guard = session.lock().await;
            let Some(session) = guard.as_mut() else {
                return Err(Error::operation(
                    "deploy_apply",
                    format!(
                        "session for machine '{}' was already closed",
                        status.machine_id
                    ),
                ));
            };
            session.remove_instance(&status.instance_id).await
        };

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
