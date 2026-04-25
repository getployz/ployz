use crate::certificates::{
    AcmeAccountCoordinator, CertificateManagerConfig, Http01ChallengeReadiness, InstantAcmeIssuer,
    IssuanceCoordinator, LocalHttp01ChallengeReadiness, NoopAcmeAccountCoordinator,
    NoopIssuanceCoordinator, spawn_certificate_finalization_with_coordination,
    start_pending_orders,
};
use crate::deploy::managed_domains;
use crate::deploy::plan::{PlanFingerprint, ResolvedPlan, resolve_plan};
use crate::deploy::probe::probe_participants;
use crate::deploy::session::{self, DeploySessionFactory};
use crate::error::{Error, Result};
use crate::model::{
    DeployApplyResult, DeployChangeKind, DeployEvent, DeployId, DeployRecord, DeployState,
    InstanceId, InstanceStatusRecord, MachineId, ServiceRelease, ServiceReleaseRecord,
    ServiceRevisionRecord, ServiceRoutingPolicy,
};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use ployz_store_api::{DeployStore, StoreDriver};
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
    apply_with_certificate_coordination(
        store,
        session_factory,
        local_machine_id,
        manifest,
        &NoopIssuanceCoordinator,
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
    )
    .await
}

pub(super) async fn apply_with_certificate_coordination(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    certificate_coordinator: &dyn IssuanceCoordinator,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
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

    apply_with_initial_plan_and_certificate_coordination(
        store,
        session_factory,
        local_machine_id,
        manifest,
        initial_plan,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
    )
    .await
}

#[cfg(test)]
pub(super) async fn apply_with_initial_plan(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    initial_plan: ResolvedPlan,
) -> Result<DeployApplyResult> {
    apply_with_initial_plan_and_certificate_coordination(
        store,
        session_factory,
        local_machine_id,
        manifest,
        initial_plan,
        &NoopIssuanceCoordinator,
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(LocalHttp01ChallengeReadiness),
    )
    .await
}

pub(super) async fn apply_with_initial_plan_and_certificate_coordination(
    store: &StoreDriver,
    session_factory: &dyn DeploySessionFactory,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    initial_plan: ResolvedPlan,
    certificate_coordinator: &dyn IssuanceCoordinator,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
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
        managed_domains::validate_hostname_ownership(store, &final_plan).await?;

        let mut final_preview = final_plan.to_preview(Vec::new());
        let mut deploy_record = DeployRecord {
            deploy_id: deploy_id.clone(),
            namespace: final_plan.namespace().clone(),
            coordinator_machine_id: local_machine_id.clone(),
            manifest_hash: final_plan.manifest_hash().to_string(),
            state: DeployState::Applying,
            started_at,
            committed_at: None,
            finished_at: None,
            summary_json: serde_json::to_string(&final_preview).map_err(|error| {
                Error::operation("deploy_apply", format!("serialize preview: {error}"))
            })?,
        };
        store.upsert_deploy(&deploy_record).await?;

        upsert_revisions(store, &final_plan, local_machine_id, started_at).await?;
        let startup = run_phase_startup(store, &sessions, &final_plan, &deploy_id).await?;
        events.extend(startup.events);

        let committed_releases =
            build_committed_releases(&final_plan, &startup.started, &deploy_id)?;
        let removed_services = final_plan
            .services()
            .iter()
            .filter(|service| service.action == DeployChangeKind::Remove)
            .map(|service| service.service.clone())
            .collect::<Vec<_>>();

        deploy_record.state = DeployState::Committed;
        deploy_record.committed_at = Some(now_unix_secs());
        deploy_record.finished_at = deploy_record.committed_at;
        deploy_record.summary_json = serde_json::to_string(&final_preview).map_err(|error| {
            Error::operation("deploy_apply", format!("serialize preview: {error}"))
        })?;

        store
            .commit_deploy(
                final_plan.namespace(),
                &removed_services,
                &committed_releases,
                &deploy_record,
            )
            .await?;
        events.push(DeployEvent {
            step: "commit".into(),
            message: format!(
                "committed deploy {} for '{}'",
                deploy_id,
                final_plan.namespace()
            ),
        });

        let acme_config = CertificateManagerConfig::from_env();
        let managed_hostnames = managed_domains::ensure_certificate_intents(
            store,
            &final_plan,
            &acme_config.issuer_url,
        )
        .await?;
        let acme_warnings = start_pending_orders(
            store,
            &InstantAcmeIssuer::with_readiness_and_account_coordinator(
                acme_config.clone(),
                Arc::new(LocalHttp01ChallengeReadiness),
                account_coordinator.clone(),
            ),
            certificate_coordinator,
            &managed_hostnames,
        )
        .await;
        let mut managed_warnings = managed_domains::warnings_for_plan(store, &final_plan).await?;
        managed_warnings.extend(acme_warnings);
        if final_preview.warnings != managed_warnings {
            final_preview.warnings = managed_warnings;
            deploy_record.summary_json =
                serde_json::to_string(&final_preview).map_err(|error| {
                    Error::operation("deploy_apply", format!("serialize preview: {error}"))
                })?;
            store.upsert_deploy(&deploy_record).await?;
        }
        spawn_certificate_finalization_with_coordination(
            store.clone(),
            acme_config,
            challenge_readiness.clone(),
            account_coordinator.clone(),
        );

        let cleanup = cleanup_stale_instances(
            store,
            &sessions,
            final_plan.namespace(),
            final_plan.participants(),
            committed_releases.as_slice(),
        )
        .await?;
        events.extend(cleanup.events);

        let final_state = if cleanup.errors.is_empty() {
            DeployState::Committed
        } else {
            deploy_record.state = DeployState::CleanupPending;
            deploy_record.finished_at = Some(now_unix_secs());
            store.upsert_deploy(&deploy_record).await?;
            for error in cleanup.errors {
                events.push(DeployEvent {
                    step: "cleanup_pending".into(),
                    message: error,
                });
            }
            DeployState::CleanupPending
        };

        Ok(DeployApplyResult {
            deploy_id: deploy_id.clone(),
            preview: final_preview,
            state: final_state,
            events,
        })
    }
    .await;

    sessions.close_all().await;
    result
}

async fn upsert_revisions(
    store: &StoreDriver,
    plan: &ResolvedPlan,
    local_machine_id: &MachineId,
    created_at: u64,
) -> Result<()> {
    for service in plan.services() {
        let Some(spec_json) = service.spec_json() else {
            continue;
        };
        let Some(revision_hash) = service.next_revision_hash() else {
            continue;
        };
        store
            .upsert_service_revision(&ServiceRevisionRecord {
                namespace: plan.namespace().clone(),
                service: service.service.clone(),
                revision_hash: revision_hash.to_string(),
                spec_json: spec_json.to_string(),
                created_by: local_machine_id.clone(),
                created_at,
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
        store.upsert_instance_status(&status).await?;
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

fn build_committed_releases(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
) -> Result<Vec<ServiceReleaseRecord>> {
    let mut releases = Vec::new();
    for service in plan.services() {
        let Some(revision_hash) = service.next_revision_hash() else {
            continue;
        };

        let mut next_slots = Vec::new();
        for slot in &service.slots {
            let active_instance_id = match slot.action {
                DeployChangeKind::Unchanged => {
                    let Some(current) = &slot.current else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "missing current slot for unchanged service '{}' slot '{}'",
                                service.service, slot.slot_id
                            ),
                        ));
                    };
                    current.active_instance_id.clone()
                }
                DeployChangeKind::Create | DeployChangeKind::Replace => {
                    let key = (service.service.clone(), slot.slot_id.0.clone());
                    let Some(status) = started.get(&key) else {
                        return Err(Error::operation(
                            "deploy_apply",
                            format!(
                                "missing started instance for service '{}' slot '{}'",
                                service.service, slot.slot_id
                            ),
                        ));
                    };
                    status.instance_id.clone()
                }
                DeployChangeKind::Remove => continue,
            };
            next_slots.push(crate::model::ServiceReleaseSlot {
                slot_id: slot.slot_id.clone(),
                machine_id: slot.machine_id.clone(),
                active_instance_id,
                revision_hash: revision_hash.to_string(),
            });
        }

        releases.push(ServiceReleaseRecord {
            namespace: plan.namespace().clone(),
            service: service.service.clone(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.to_string(),
                referenced_revision_hashes: vec![revision_hash.to_string()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.to_string(),
                },
                slots: next_slots,
                updated_by_deploy_id: deploy_id.clone(),
                updated_at: now_unix_secs(),
            },
        });
    }
    Ok(releases)
}

async fn cleanup_stale_instances(
    store: &StoreDriver,
    sessions: &SessionSet,
    namespace: &ployz_types::spec::Namespace,
    participants: &BTreeSet<MachineId>,
    committed_releases: &[ServiceReleaseRecord],
) -> Result<CleanupResult> {
    let active_instance_ids = committed_releases
        .iter()
        .flat_map(|release| release.release.slots.iter())
        .map(|slot| slot.active_instance_id.0.clone())
        .collect::<BTreeSet<_>>();
    let participant_ids = participants
        .iter()
        .map(|machine_id| machine_id.0.clone())
        .collect::<BTreeSet<_>>();

    let mut stale_by_machine: BTreeMap<MachineId, Vec<InstanceStatusRecord>> = BTreeMap::new();
    for status in store.list_instance_status(namespace).await? {
        if active_instance_ids.contains(&status.instance_id.0) {
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
