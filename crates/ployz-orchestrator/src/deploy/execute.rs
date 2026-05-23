use crate::certificates::{
    AcmeAccountCoordinator, AcmeIssuerFactory, Http01ChallengeReadiness, IssuanceCoordinator,
    LocalHttp01ChallengeReadiness, NoopAcmeAccountCoordinator, NoopAcmeIssuerFactory,
    NoopIssuanceCoordinator, spawn_certificate_finalization_with_coordination,
    start_pending_orders,
};
use crate::deploy::lifecycle::{
    CleanupPlan, PreparedDeploy, build_committed_releases_for_services,
};
use crate::deploy::managed_domains;
use crate::deploy::participant::{self, DeployParticipantClient};
use crate::deploy::plan::{
    ResolvedPlan, VolumeChange, preflight_image_availability, resolve_plan, volume_record_change,
};
use crate::deploy::probe::{NoopParticipantProbe, ParticipantProbe, probe_participants};
use crate::error::{DeployError, Error, Result, StoreError};
use crate::model::{
    DeployApplyResult, DeployBaselineDiff, DeployChangeKind, DeployEvent, DeployId,
    DeployPhaseCommitPolicy, DeployPhaseCommitRecord, DeployPhaseFailure, DeployPhaseId,
    DeployPhasePlan, DeployPhaseRecord, DeployPhaseRecordState, DeployPhaseState, DeployPhaseWork,
    DeployPreview, DeployPreviewBaseline, DeployRecord, DeployState, InstanceId, InstancePhase,
    InstanceStatusRecord, MachineId, MachineMembership, PreparedDeployRecord, PreparedDeployState,
    VolumeBranchLineageRecord, VolumeMovementRecord, VolumeRecord,
};
use futures_util::stream::{self, FuturesUnordered, StreamExt, TryStreamExt};
use ployz_store_api::{
    DeployCommit, DeployStore, InstanceStatusStore, MachineMembershipStore, StoreDriver,
};
use ployz_types::spec::{VolumeCloneConsistency, VolumeCloneDataPolicy};
use ployz_types::time::now_unix_secs;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

const PARTICIPANT_INSPECT_CONCURRENCY: usize = 64;
const PHASE_MACHINE_CONCURRENCY: usize = 64;

pub(super) fn new_deploy_id() -> DeployId {
    DeployId::new(Uuid::new_v4().to_string())
}

enum PreparedApplyReplay {
    Complete(DeployApplyResult),
    DurableCommit(DeployCommit),
}

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
    started_or_attempted_services: BTreeSet<String>,
}

#[derive(Debug)]
struct MachineStartupFailure {
    error: Error,
    partial: MachineStartupResult,
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
    started_or_attempted_services: BTreeSet<String>,
}

#[derive(Debug)]
struct PhaseStartupFailure {
    error: Error,
    partial: PhaseStartupResult,
}

#[derive(Debug)]
struct CleanupResult {
    events: Vec<DeployEvent>,
    errors: Vec<String>,
}

struct InspectedParticipant {
    participant: MachineId,
    instances: Vec<InstanceStatusRecord>,
}

pub(super) struct ParticipantSet {
    machines: BTreeMap<MachineId, MachineMembership>,
    instances: Vec<InstanceStatusRecord>,
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
        let namespace = plan.namespace().clone();
        Self::inspect_participants(
            participant_client,
            namespace,
            plan.machine_map().clone(),
            plan.participants().clone(),
            local_machine_id,
            deploy_id,
        )
        .await
    }

    async fn inspect_participants(
        participant_client: &dyn DeployParticipantClient,
        namespace: ployz_types::spec::Namespace,
        machine_map: HashMap<MachineId, MachineMembership>,
        participant_ids: BTreeSet<MachineId>,
        local_machine_id: &MachineId,
        deploy_id: &DeployId,
    ) -> Result<(Self, Vec<DeployEvent>)> {
        let sorted_participants = participant_ids.iter().cloned().collect::<Vec<_>>();
        let inspected: Vec<InspectedParticipant> = stream::iter(sorted_participants.into_iter())
            .map(|participant| {
                let machine = machine_map.get(&participant).cloned();
                let namespace = namespace.clone();
                let deploy_id = deploy_id.clone();
                async move {
                    let Some(machine) = machine else {
                        return Err(Error::Deploy(DeployError::ParticipantMissing {
                            machine_id: participant.as_str().to_string(),
                        }));
                    };
                    let instances = participant_client
                        .inspect_namespace(&machine, &namespace, &deploy_id, local_machine_id)
                        .await?;
                    Ok(InspectedParticipant {
                        participant,
                        instances,
                    })
                }
            })
            .buffer_unordered(PARTICIPANT_INSPECT_CONCURRENCY)
            .try_collect()
            .await?;

        let mut inspected = inspected;
        inspected.sort_by(|left, right| left.participant.as_str().cmp(right.participant.as_str()));

        let machines = participant_ids
            .iter()
            .map(|machine_id| {
                let machine = machine_map.get(machine_id).cloned().ok_or_else(|| {
                    Error::Deploy(DeployError::ParticipantMissing {
                        machine_id: machine_id.as_str().to_string(),
                    })
                })?;
                Ok((machine_id.clone(), machine))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut events = Vec::new();
        let mut instances = Vec::new();
        for inspected in inspected {
            let instance_count = inspected.instances.len();
            events.push(DeployEvent {
                step: "inspect".into(),
                message: format!(
                    "inspected '{}' ({} instances)",
                    inspected.participant, instance_count
                ),
            });
            instances.extend(inspected.instances);
        }
        instances.sort_by(|left, right| left.instance_id.as_str().cmp(right.instance_id.as_str()));

        Ok((
            Self {
                machines,
                instances,
                namespace,
                deploy_id: deploy_id.clone(),
            },
            events,
        ))
    }

    fn get(&self, machine_id: &MachineId) -> Result<&MachineMembership> {
        self.machines.get(machine_id).ok_or_else(|| {
            Error::Deploy(DeployError::ParticipantMissing {
                machine_id: machine_id.as_str().to_string(),
            })
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DeployApplyPreconditions<'a> {
    pub expected_baseline: Option<&'a DeployPreviewBaseline>,
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
    apply_with_deploy_id_and_preconditions(
        store,
        participant_client,
        local_machine_id,
        manifest,
        new_deploy_id(),
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
        DeployApplyPreconditions::default(),
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

#[cfg(test)]
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
    apply_with_deploy_id_initial_plan_and_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        new_deploy_id(),
        initial_plan,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        None,
        None,
    )
    .await
}

pub(super) async fn apply_with_deploy_id_and_preconditions(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    deploy_id: DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
    preconditions: DeployApplyPreconditions<'_>,
) -> Result<DeployApplyResult> {
    apply_with_deploy_id_and_preconditions_for_prepared(
        store,
        participant_client,
        local_machine_id,
        manifest,
        deploy_id,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
        preconditions,
        None,
    )
    .await
}

async fn apply_with_deploy_id_and_preconditions_for_prepared(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    deploy_id: DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
    preconditions: DeployApplyPreconditions<'_>,
    prepared_record: Option<&PreparedDeployRecord>,
) -> Result<DeployApplyResult> {
    let initial_plan = resolve_plan(store, local_machine_id, manifest).await?;
    ensure_deploy_baseline(preconditions.expected_baseline, &initial_plan)?;
    preflight_image_availability(store, &initial_plan).await?;
    let expected_baseline = preconditions.expected_baseline.cloned();
    ensure_volume_move_execution_supported(participant_client, &initial_plan)?;
    ensure_volume_clone_execution_supported(participant_client, &initial_plan)?;
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
            .map(|machine_id| machine_id.as_str().to_string())
            .collect::<Vec<_>>();
        return Err(Error::Deploy(DeployError::ParticipantsUnreachable {
            unreachable_count: machine_ids.len(),
            participant_count: initial_plan.participants().len(),
            machine_ids,
        }));
    }

    apply_with_deploy_id_initial_plan_and_certificate_coordination(
        store,
        participant_client,
        local_machine_id,
        manifest,
        deploy_id,
        initial_plan,
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        expected_baseline,
        prepared_record,
    )
    .await
}

pub(super) async fn apply_prepared_with_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    prepared_deploy_id: DeployId,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    prober: &dyn ParticipantProbe,
) -> Result<DeployApplyResult> {
    let prepared = load_applicable_prepared_deploy(store, &prepared_deploy_id).await?;
    let manifest = super::validated_prepared_manifest(&prepared)?;
    if let Some(replay) = existing_prepared_apply_replay(store, &prepared).await? {
        return match replay {
            PreparedApplyReplay::Complete(result) => Ok(result),
            PreparedApplyReplay::DurableCommit(commit) => {
                resume_prepared_apply_after_durable_commit(
                    store,
                    participant_client,
                    local_machine_id,
                    &manifest,
                    &prepared,
                    commit,
                    certificate_coordinator,
                    account_coordinator,
                    challenge_readiness,
                    issuer_factory,
                )
                .await
            }
        };
    }
    if prepared.state != PreparedDeployState::Prepared {
        return Err(Error::Deploy(DeployError::PreparedDeployNotApplicable {
            prepared_deploy_id: prepared.prepared_deploy_id.as_str().to_string(),
            state: prepared.state,
        }));
    }
    ensure_prepared_not_expired(store, &prepared).await?;
    let result = apply_with_deploy_id_and_preconditions_for_prepared(
        store,
        participant_client,
        local_machine_id,
        &manifest,
        prepared.prepared_deploy_id.clone(),
        certificate_coordinator,
        account_coordinator,
        challenge_readiness,
        issuer_factory,
        prober,
        DeployApplyPreconditions {
            expected_baseline: Some(&prepared.baseline),
        },
        Some(&prepared),
    )
    .await;
    if matches!(
        result,
        Err(Error::Deploy(DeployError::DeployBaselineChanged { .. }))
    ) {
        let _ = store
            .supersede_prepared_deploy(&prepared.prepared_deploy_id, now_unix_secs())
            .await;
    }
    result
}

async fn load_applicable_prepared_deploy(
    store: &StoreDriver,
    prepared_deploy_id: &DeployId,
) -> Result<PreparedDeployRecord> {
    let Some(prepared) = store.get_prepared_deploy(prepared_deploy_id).await? else {
        return Err(Error::Deploy(DeployError::PreparedDeployMissing {
            prepared_deploy_id: prepared_deploy_id.as_str().to_string(),
        }));
    };
    Ok(prepared)
}

async fn ensure_prepared_not_expired(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
) -> Result<()> {
    if prepared.expires_at > now_unix_secs() {
        return Ok(());
    }
    store
        .expire_prepared_deploy(&prepared.prepared_deploy_id, now_unix_secs())
        .await?;
    Err(Error::Deploy(DeployError::PreparedDeployExpired {
        prepared_deploy_id: prepared.prepared_deploy_id.as_str().to_string(),
        expires_at: prepared.expires_at,
    }))
}

async fn existing_prepared_apply_replay(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
) -> Result<Option<PreparedApplyReplay>> {
    let status_record = store.get_deploy(&prepared.prepared_deploy_id).await?;
    if let Some(record) = status_record.as_ref()
        && record.state() == DeployState::Committed
        && let Some(mut result) = prepared_apply_result_from_terminal_record(
            store,
            prepared,
            record.clone(),
            "prepared deploy already has a terminal committed deploy record",
        )
        .await?
    {
        if let Some(commit) = store
            .get_deploy_commit(&prepared.namespace, &prepared.prepared_deploy_id)
            .await?
        {
            repair_phase_records_from_durable_commit(store, &commit, &mut result.events).await;
        }
        return Ok(Some(PreparedApplyReplay::Complete(result)));
    }
    if let Some(commit) = store
        .get_deploy_commit(&prepared.namespace, &prepared.prepared_deploy_id)
        .await?
    {
        return Ok(Some(PreparedApplyReplay::DurableCommit(commit)));
    }
    if let Some(record) = status_record {
        return prepared_apply_result_from_terminal_record(
            store,
            prepared,
            record,
            "prepared deploy already has a terminal committed deploy record",
        )
        .await
        .map(|result| result.map(PreparedApplyReplay::Complete));
    }
    Ok(None)
}

fn post_commit_pending_deploy_record(record: &DeployRecord) -> DeployRecord {
    let mut pending = record.clone();
    pending
        .mark_cleanup_pending(now_unix_secs())
        .expect("committed deploy can become cleanup pending");
    pending
}

async fn prepared_apply_result_from_terminal_record(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
    record: DeployRecord,
    message: &str,
) -> Result<Option<DeployApplyResult>> {
    match record.state() {
        DeployState::Committed | DeployState::CleanupPending => {
            let mut events = vec![DeployEvent {
                step: "prepared_deploy".into(),
                message: message.into(),
            }];
            if prepared.state == PreparedDeployState::Prepared {
                if let Err(error) = store
                    .mark_prepared_deploy_applied(&prepared.prepared_deploy_id, now_unix_secs())
                    .await
                {
                    events.push(DeployEvent {
                        step: "prepared_deploy".into(),
                        message: format!(
                            "prepared deploy replayed committed deploy but could not mark state applied: {error}"
                        ),
                    });
                }
            }
            let preview: DeployPreview =
                serde_json::from_str(&record.summary_json()).map_err(|error| {
                    Error::operation(
                        "prepared_deploy_replay",
                        format!("decode committed deploy preview: {error}"),
                    )
                })?;
            let state = record.state();
            Ok(Some(DeployApplyResult {
                deploy_id: record.deploy_id,
                preview,
                state,
                events,
            }))
        }
        DeployState::CheckpointCommitted | DeployState::FailedAfterCheckpoint => {
            store
                .supersede_prepared_deploy(&prepared.prepared_deploy_id, now_unix_secs())
                .await?;
            Err(Error::Deploy(DeployError::PreparedDeployNotApplicable {
                prepared_deploy_id: prepared.prepared_deploy_id.as_str().to_string(),
                state: PreparedDeployState::Superseded,
            }))
        }
        DeployState::Planning | DeployState::Applying | DeployState::Failed => Ok(None),
    }
}

async fn resume_prepared_apply_after_durable_commit(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    prepared: &PreparedDeployRecord,
    commit: DeployCommit,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
) -> Result<DeployApplyResult> {
    match commit.deploy.state() {
        DeployState::Committed | DeployState::CleanupPending => {}
        DeployState::CheckpointCommitted | DeployState::FailedAfterCheckpoint => {
            return prepared_apply_result_from_terminal_record(
                store,
                prepared,
                commit.deploy,
                "prepared deploy already has a terminal checkpoint deploy record",
            )
            .await?
            .ok_or_else(|| {
                Error::operation(
                    "prepared_deploy_replay",
                    "checkpoint deploy record did not produce replay result",
                )
            });
        }
        DeployState::Planning | DeployState::Applying | DeployState::Failed => {
            let preview = serde_json::from_str(commit.deploy.summary_json()).map_err(|error| {
                Error::operation(
                    "prepared_deploy_replay",
                    format!("decode committed deploy preview: {error}"),
                )
            })?;
            let state = commit.deploy.state();
            return Ok(DeployApplyResult {
                deploy_id: commit.deploy.deploy_id,
                preview,
                state,
                events: vec![DeployEvent {
                    step: "prepared_deploy".into(),
                    message: "prepared deploy commit log is not terminal".into(),
                }],
            });
        }
    }

    let mut events = vec![DeployEvent {
        step: "prepared_deploy".into(),
        message: "prepared deploy recovered replayable terminal status from durable commit log"
            .into(),
    }];
    let mut final_preview: DeployPreview = serde_json::from_str(&commit.deploy.summary_json())
        .map_err(|error| {
            Error::operation(
                "prepared_deploy_replay",
                format!("decode committed deploy preview: {error}"),
            )
        })?;
    let mut final_deploy_record = commit.deploy.clone();
    store
        .write_deploy_status(&post_commit_pending_deploy_record(&final_deploy_record))
        .await?;
    repair_phase_records_from_durable_commit(store, &commit, &mut events).await;
    mark_prepared_applied_after_terminal_status(store, Some(prepared), &mut events).await;

    let plan = match resolve_plan(store, local_machine_id, manifest).await {
        Ok(plan) => plan,
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit finalization could not resolve plan: {error}"),
            )
            .await;
        }
    };
    let mut cleanup_participants = plan.participants().clone();
    cleanup_participants.extend(prepared.preview.participants.iter().cloned());
    let machine_map = match store.list_machines().await {
        Ok(machines) => machines
            .into_iter()
            .map(|machine| (machine.id.clone(), machine))
            .collect::<HashMap<_, _>>(),
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit cleanup could not list machines: {error}"),
            )
            .await;
        }
    };
    let (participants, inspect_events) = match ParticipantSet::inspect_participants(
        participant_client,
        plan.namespace().clone(),
        machine_map,
        cleanup_participants.clone(),
        local_machine_id,
        &prepared.prepared_deploy_id,
    )
    .await
    {
        Ok(inspected) => inspected,
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit cleanup could not inspect participants: {error}"),
            )
            .await;
        }
    };
    events.push(DeployEvent {
        step: "prepared_deploy".into(),
        message: "prepared deploy resumed post-commit finalization from durable commit log".into(),
    });
    events.extend(inspect_events);

    let managed_hostnames = match managed_domains::ensure_certificate_intents(
        store,
        &plan,
        issuer_factory.issuer_url(),
    )
    .await
    {
        Ok(hostnames) => hostnames,
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit certificate intent finalization failed: {error}"),
            )
            .await;
        }
    };
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
    let mut managed_warnings = match managed_domains::warnings_for_plan(store, &plan).await {
        Ok(warnings) => warnings,
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit warning refresh failed: {error}"),
            )
            .await;
        }
    };
    managed_warnings.extend(acme_warnings);
    final_preview.warnings = managed_warnings;

    spawn_certificate_finalization_with_coordination(
        store.clone(),
        issuer_factory.clone(),
        challenge_readiness,
        account_coordinator,
        certificate_coordinator,
    );

    let active_instance_ids = match store.list_deploy_releases(&commit.namespace).await {
        Ok(releases) => releases
            .iter()
            .flat_map(|release| release.release.slots.iter())
            .map(|slot| slot.active_instance_id.as_str().to_string())
            .collect(),
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit cleanup could not list committed releases: {error}"),
            )
            .await;
        }
    };
    let cleanup_plan = CleanupPlan::new(
        commit.namespace.clone(),
        cleanup_participants,
        active_instance_ids,
    );
    let cleanup = match cleanup_stale_instances(
        store,
        participant_client,
        &participants,
        &cleanup_plan,
    )
    .await
    {
        Ok(cleanup) => cleanup,
        Err(error) => {
            return recovered_commit_result_with_post_commit_warning(
                store,
                prepared,
                final_deploy_record,
                final_preview,
                events,
                format!("post-commit cleanup failed: {error}"),
            )
            .await;
        }
    };
    events.extend(cleanup.events);

    let final_summary_json = serde_json::to_string(&final_preview).map_err(|error| {
        Error::operation(
            "prepared_deploy_replay",
            format!("serialize preview: {error}"),
        )
    })?;
    final_deploy_record.set_summary_json(final_summary_json);
    let final_state = if cleanup.errors.is_empty() {
        final_deploy_record.state()
    } else {
        final_deploy_record
            .mark_cleanup_pending(now_unix_secs())
            .expect("committed deploy can become cleanup pending");
        for error in cleanup.errors {
            events.push(DeployEvent {
                step: "cleanup_pending".into(),
                message: error,
            });
        }
        DeployState::CleanupPending
    };
    store.write_deploy_status(&final_deploy_record).await?;

    Ok(DeployApplyResult {
        deploy_id: prepared.prepared_deploy_id.clone(),
        preview: final_preview,
        state: final_state,
        events,
    })
}

async fn recovered_commit_result_with_post_commit_warning(
    store: &StoreDriver,
    prepared: &PreparedDeployRecord,
    mut record: DeployRecord,
    mut preview: DeployPreview,
    mut events: Vec<DeployEvent>,
    warning: String,
) -> Result<DeployApplyResult> {
    preview.warnings.push(warning.clone());
    events.push(DeployEvent {
        step: "cleanup_pending".into(),
        message: warning,
    });
    let summary_json = serde_json::to_string(&preview).map_err(|error| {
        Error::operation(
            "prepared_deploy_replay",
            format!("serialize warning preview: {error}"),
        )
    })?;
    record.set_summary_json(summary_json);
    record
        .mark_cleanup_pending(now_unix_secs())
        .expect("committed deploy can become cleanup pending");
    if let Err(update_error) = store.write_deploy_status(&record).await {
        events.push(DeployEvent {
            step: "cleanup_pending".into(),
            message: format!("could not record cleanup-pending recovery status: {update_error}"),
        });
    }
    Ok(DeployApplyResult {
        deploy_id: prepared.prepared_deploy_id.clone(),
        preview,
        state: DeployState::CleanupPending,
        events,
    })
}

async fn repair_phase_records_from_durable_commit(
    store: &StoreDriver,
    commit: &DeployCommit,
    events: &mut Vec<DeployEvent>,
) {
    if commit.phase_commits.is_empty() {
        return;
    }

    let phase_commits = commit
        .phase_commits
        .iter()
        .map(|phase_commit| (phase_commit.phase_id.clone(), phase_commit))
        .collect::<BTreeMap<_, _>>();
    let phases = match store
        .list_deploy_phases(&commit.namespace, &commit.deploy.deploy_id)
        .await
    {
        Ok(phases) => phases,
        Err(error) => {
            let message =
                format!("could not list deploy phases for durable commit recovery: {error}");
            warn!(
                ?error,
                deploy_id = %commit.deploy.deploy_id,
                "failed to list deploy phases for durable commit recovery"
            );
            events.push(DeployEvent {
                step: "phase".into(),
                message,
            });
            return;
        }
    };

    let mut repaired_count = 0usize;
    for mut phase in phases {
        let Some(phase_commit) = phase_commits.get(&phase.phase_id) else {
            continue;
        };
        if let Err(error) = phase.mark_succeeded(
            Some(phase_commit.commit_deploy_id.clone()),
            phase_commit.committed_at,
        ) {
            warn!(
                deploy_id = %phase.deploy_id,
                phase_id = %phase.phase_id,
                error = %error,
                "phase commit fact did not match deploy phase commit policy"
            );
            continue;
        }
        match store.upsert_deploy_phase(&phase).await {
            Ok(()) => repaired_count += 1,
            Err(error) => {
                warn!(
                    ?error,
                    deploy_id = %commit.deploy.deploy_id,
                    phase_id = %phase.phase_id,
                    "failed to repair deploy phase from durable commit"
                );
                events.push(DeployEvent {
                    step: "phase".into(),
                    message: format!(
                        "could not repair deploy phase '{}' from durable commit: {error}",
                        phase.phase_id
                    ),
                });
            }
        }
    }

    if repaired_count > 0 {
        events.push(DeployEvent {
            step: "phase".into(),
            message: format!(
                "repaired {repaired_count} deploy phase records from durable commit log"
            ),
        });
    }
}

async fn mark_prepared_applied_after_terminal_status(
    store: &StoreDriver,
    prepared: Option<&PreparedDeployRecord>,
    events: &mut Vec<DeployEvent>,
) {
    let Some(prepared) = prepared else {
        return;
    };
    if let Err(error) = store
        .mark_prepared_deploy_applied(&prepared.prepared_deploy_id, now_unix_secs())
        .await
    {
        events.push(DeployEvent {
            step: "prepared_deploy".into(),
            message: format!(
                "deploy committed but prepared deploy state could not be marked applied: {error}"
            ),
        });
    }
}

async fn apply_with_deploy_id_initial_plan_and_certificate_coordination(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    local_machine_id: &MachineId,
    manifest: &ployz_types::spec::DeployManifest,
    deploy_id: DeployId,
    initial_plan: ResolvedPlan,
    certificate_coordinator: Arc<dyn IssuanceCoordinator>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    challenge_readiness: Arc<dyn Http01ChallengeReadiness>,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    expected_baseline: Option<DeployPreviewBaseline>,
    prepared_record: Option<&PreparedDeployRecord>,
) -> Result<DeployApplyResult> {
    let started_at = now_unix_secs();
    ensure_volume_move_execution_supported(participant_client, &initial_plan)?;
    ensure_volume_clone_execution_supported(participant_client, &initial_plan)?;

    let (participants, mut events) = ParticipantSet::inspect(
        participant_client,
        &initial_plan,
        local_machine_id,
        &deploy_id,
    )
    .await?;

    let mut last_written_deploy_record = None;
    let mut durable_final_commit_record = None;
    let mut pending_phase_successes = Vec::new();
    let mut executed_volume_clones: BTreeMap<String, ExecutedVolumeClone> = BTreeMap::new();
    let mut checkpointed_volumes = BTreeSet::new();
    let mut started_clone_volumes = BTreeSet::new();
    let result = async {
        let final_plan = resolve_plan(store, local_machine_id, manifest).await?;
        ensure_plan_stable(&initial_plan, &final_plan, expected_baseline.as_ref())?;
        managed_domains::validate_hostname_ownership(store, &final_plan).await?;
        let image_availability = preflight_image_availability(store, &final_plan).await?;

        let prepared = PreparedDeploy::new(
            deploy_id.clone(),
            started_at,
            local_machine_id.clone(),
            final_plan,
            image_availability,
        )?;
        store
            .write_deploy_status(prepared.applying_record())
            .await?;
        last_written_deploy_record = Some(prepared.applying_record().clone());

        let phases = prepared.preview().phases.clone();
        let mut unstarted_phase_records = Vec::new();
        for phase in &phases {
            let pending_phase =
                deploy_phase_record(&prepared, phase, DeployPhaseState::Pending, started_at);
            if let Err(error) = store.upsert_deploy_phase(&pending_phase).await {
                mark_phases_failed_best_effort(
                    store,
                    std::mem::take(&mut unstarted_phase_records),
                    &error,
                    prepared.deploy_id(),
                )
                .await;
                return Err(error);
            }
            unstarted_phase_records.push(pending_phase);
        }
        let mut started = HashMap::new();
        let mut executed_volume_moves = BTreeMap::new();
        let mut checkpointed_services = BTreeSet::new();
        let mut stopped_uncommitted_instance_ids = BTreeSet::new();

        for phase in &phases {
            let phase_execution_result = execute_phase(
                store,
                participant_client,
                &participants,
                &prepared,
                phase,
                started_at,
                &mut stopped_uncommitted_instance_ids,
            )
            .await;
            let phase_execution = match phase_execution_result {
                Ok(phase_execution) => {
                    remove_phase_record(&mut unstarted_phase_records, &phase.phase_id);
                    phase_execution
                }
                Err(error) => {
                    if error.phase_reached_running {
                        remove_phase_record(&mut unstarted_phase_records, &phase.phase_id);
                    }
                    for clone in executed_volume_clones.values() {
                        if volume_has_started_or_attempted_attached_service(
                            prepared.plan(),
                            &clone.volume_name,
                            &error.started_or_attempted_services,
                        ) {
                            started_clone_volumes.insert(clone.volume_name.clone());
                        }
                    }
                    mark_pending_and_unstarted_phases_failed_best_effort(
                        store,
                        &mut pending_phase_successes,
                        &mut unstarted_phase_records,
                        &error.error,
                        prepared.deploy_id(),
                    )
                    .await;
                    return Err(error.error);
                }
            };
            events.extend(phase_execution.events);
            started.extend(phase_execution.started);
            executed_volume_moves.extend(phase_execution.volume_movements);
            executed_volume_clones.extend(phase_execution.volume_branches);
            for clone in executed_volume_clones.values() {
                if volume_has_started_attached_service(
                    prepared.plan(),
                    &clone.volume_name,
                    &started,
                ) {
                    started_clone_volumes.insert(clone.volume_name.clone());
                }
            }

            match phase.commit_policy {
                DeployPhaseCommitPolicy::Checkpoint => {
                    let phase_services = phase_services(phase);
                    let phase_volumes = phase_volumes(phase);
                    let removed_services =
                        removed_services_for_phase(prepared.plan(), &phase_services);
                    let phase_commit_id =
                        phase_commit_deploy_id(prepared.deploy_id(), &phase.phase_id);
                    let committed_at = phase_commit_timestamp(&prepared, phase);
                    let commit_result = build_phase_commit(
                        &prepared,
                        &started,
                        &executed_volume_moves,
                        &executed_volume_clones,
                        &phase_services,
                        &phase_services,
                        &phase_volumes,
                        removed_services,
                        Vec::new(),
                        vec![phase_commit_record(
                            &prepared,
                            &phase_execution.running_phase,
                            &phase_commit_id,
                            committed_at,
                        )],
                        DeployState::CheckpointCommitted,
                        phase_commit_id.clone(),
                        committed_at,
                    );
                    let commit = match commit_result {
                        Ok(commit) => commit,
                        Err(error) => {
                            mark_phase_and_pending_phases_failed_best_effort(
                                store,
                                phase_execution.running_phase,
                                &mut pending_phase_successes,
                                &mut unstarted_phase_records,
                                &error,
                                prepared.deploy_id(),
                            )
                            .await;
                            return Err(error);
                        }
                    };
                    if let Err(error) = store.commit_deploy(&commit).await {
                        if is_post_commit_routing_publish_failure(&error) {
                            checkpointed_services.extend(phase_services);
                            checkpointed_volumes.extend(phase_volumes);
                            record_phase_succeeded_after_commit(
                                store,
                                prepared.deploy_id(),
                                Some(phase_execution.running_phase),
                                Some(phase_commit_id),
                            )
                            .await;
                            let status = checkpoint_deploy_record(&prepared)?;
                            last_written_deploy_record = Some(status.clone());
                            let _ = store.write_deploy_status(&status).await;
                            return Err(error);
                        }
                        mark_phase_and_pending_phases_failed_best_effort(
                            store,
                            phase_execution.running_phase,
                            &mut pending_phase_successes,
                            &mut unstarted_phase_records,
                            &error,
                            prepared.deploy_id(),
                        )
                        .await;
                        return Err(error);
                    }
                    checkpointed_services.extend(phase_services);
                    checkpointed_volumes.extend(phase_volumes);
                    record_phase_succeeded_after_commit(
                        store,
                        prepared.deploy_id(),
                        Some(phase_execution.running_phase),
                        Some(phase_commit_id),
                    )
                    .await;
                    let status = checkpoint_deploy_record(&prepared)?;
                    last_written_deploy_record = Some(status.clone());
                    let status_result = store.write_deploy_status(&status).await;
                    if let Err(error) = status_result {
                        mark_pending_and_unstarted_phases_failed_best_effort(
                            store,
                            &mut pending_phase_successes,
                            &mut unstarted_phase_records,
                            &error,
                            prepared.deploy_id(),
                        )
                        .await;
                        return Err(error);
                    }
                    events.push(DeployEvent {
                        step: "phase".into(),
                        message: format!("checkpointed phase {} ({})", phase.phase_id, phase.name),
                    });
                }
                DeployPhaseCommitPolicy::EndOfDeploy => {
                    pending_phase_successes.push(phase_execution.running_phase);
                }
                DeployPhaseCommitPolicy::NoStoreCommit => {
                    record_phase_succeeded_after_commit(
                        store,
                        prepared.deploy_id(),
                        Some(phase_execution.running_phase),
                        None,
                    )
                    .await;
                    events.push(DeployEvent {
                        step: "phase".into(),
                        message: format!(
                            "completed no-store phase {} ({})",
                            phase.phase_id, phase.name
                        ),
                    });
                }
            }
        }

        let remaining_services =
            remaining_changed_services(prepared.plan(), &checkpointed_services);
        let remaining_branch_lineage_services =
            remaining_branch_lineage_services(prepared.plan(), &checkpointed_services);
        let remaining_volumes = remaining_changed_volumes(prepared.plan(), &checkpointed_volumes);
        let removed_services =
            removed_services_for_final_commit(prepared.plan(), &checkpointed_services);
        let final_commit_result = async {
            let removed_volumes_list = removed_volumes(store, prepared.plan()).await?;
            let final_commit_timestamp = now_unix_secs();
            build_phase_commit(
                &prepared,
                &started,
                &executed_volume_moves,
                &executed_volume_clones,
                &remaining_services,
                &remaining_branch_lineage_services,
                &remaining_volumes,
                removed_services,
                removed_volumes_list,
                pending_phase_successes
                    .iter()
                    .map(|phase| {
                        phase_commit_record(
                            &prepared,
                            phase,
                            prepared.deploy_id(),
                            final_commit_timestamp,
                        )
                    })
                    .collect(),
                DeployState::Committed,
                prepared.deploy_id().clone(),
                final_commit_timestamp,
            )
        }
        .await;
        let final_commit = match final_commit_result {
            Ok(commit) => commit,
            Err(error) => {
                mark_pending_and_unstarted_phases_failed_best_effort(
                    store,
                    &mut pending_phase_successes,
                    &mut unstarted_phase_records,
                    &error,
                    prepared.deploy_id(),
                )
                .await;
                return Err(error);
            }
        };
        if let Err(error) = store.commit_deploy(&final_commit).await {
            if is_post_commit_routing_publish_failure(&error) {
                durable_final_commit_record = Some(final_commit.deploy.clone());
                let pending_record = post_commit_pending_deploy_record(&final_commit.deploy);
                record_phases_succeeded_after_commit(
                    store,
                    &deploy_id,
                    std::mem::take(&mut pending_phase_successes),
                    Some(final_commit.deploy.deploy_id.clone()),
                )
                .await;
                if store.write_deploy_status(&pending_record).await.is_ok() {
                    last_written_deploy_record = Some(pending_record);
                    mark_prepared_applied_after_terminal_status(
                        store,
                        prepared_record,
                        &mut events,
                    )
                    .await;
                }
                return Err(error);
            }
            mark_pending_and_unstarted_phases_failed_best_effort(
                store,
                &mut pending_phase_successes,
                &mut unstarted_phase_records,
                &error,
                prepared.deploy_id(),
            )
            .await;
            return Err(error);
        }
        durable_final_commit_record = Some(final_commit.deploy.clone());
        let pending_record = post_commit_pending_deploy_record(&final_commit.deploy);
        let committed_status_result = store.write_deploy_status(&pending_record).await;
        record_phases_succeeded_after_commit(
            store,
            &deploy_id,
            std::mem::take(&mut pending_phase_successes),
            Some(final_commit.deploy.deploy_id.clone()),
        )
        .await;
        if committed_status_result.is_ok() {
            last_written_deploy_record = Some(pending_record);
            mark_prepared_applied_after_terminal_status(store, prepared_record, &mut events).await;
        }
        committed_status_result?;
        events.push(DeployEvent {
            step: "commit".into(),
            message: format!(
                "committed deploy {} for '{}'",
                prepared.deploy_id(),
                prepared.plan().namespace()
            ),
        });

        let managed_hostnames = managed_domains::ensure_certificate_intents(
            store,
            prepared.plan(),
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
            managed_domains::warnings_for_plan(store, prepared.plan()).await?;
        managed_warnings.extend(acme_warnings);
        let mut final_deploy_record = final_commit.deploy.clone();
        let mut final_preview = prepared.preview().clone();
        final_preview.warnings = managed_warnings;
        final_deploy_record.set_summary_json(serde_json::to_string(&final_preview).map_err(
            |error| Error::operation("deploy_apply", format!("serialize preview: {error}")),
        )?);
        let post_warning_pending_record = post_commit_pending_deploy_record(&final_deploy_record);
        store
            .write_deploy_status(&post_warning_pending_record)
            .await?;
        last_written_deploy_record = Some(post_warning_pending_record);
        spawn_certificate_finalization_with_coordination(
            store.clone(),
            issuer_factory.clone(),
            challenge_readiness.clone(),
            account_coordinator.clone(),
            certificate_coordinator.clone(),
        );

        let all_releases = build_committed_releases_for_services(
            prepared.plan(),
            &started,
            prepared.deploy_id(),
            now_unix_secs(),
            None,
        )?;
        let cleanup_plan = CleanupPlan::new(
            prepared.plan().namespace().clone(),
            prepared.plan().participants().clone(),
            all_releases
                .iter()
                .flat_map(|release| release.release.slots.iter())
                .map(|slot| slot.active_instance_id.as_str().to_string())
                .collect(),
        );
        let cleanup =
            cleanup_stale_instances(store, participant_client, &participants, &cleanup_plan)
                .await?;
        events.extend(cleanup.events);

        let final_state = if cleanup.errors.is_empty() {
            store.write_deploy_status(&final_deploy_record).await?;
            last_written_deploy_record = Some(final_deploy_record.clone());
            DeployState::Committed
        } else {
            let mut cleanup_pending_record = final_deploy_record.clone();
            cleanup_pending_record
                .mark_cleanup_pending(now_unix_secs())
                .expect("committed deploy can become cleanup pending");
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
            deploy_id: prepared.deploy_id().clone(),
            preview: final_preview,
            state: final_state,
            events,
        })
    }
    .await;

    if let Err(error) = &result {
        let status_error = if durable_final_commit_record.is_none() {
            let cleanup_errors = cleanup_uncommitted_volume_clones_after_failure(
                participant_client,
                &participants,
                &deploy_id,
                &executed_volume_clones,
                &checkpointed_volumes,
                &started_clone_volumes,
            )
            .await;
            error_with_clone_cleanup_failures(error.clone(), cleanup_errors)
        } else {
            error.clone()
        };
        if let Some(committed_record) = durable_final_commit_record {
            if !matches!(
                last_written_deploy_record.as_ref().map(DeployRecord::state),
                Some(DeployState::Committed | DeployState::CleanupPending)
            ) {
                let pending_record = post_commit_pending_deploy_record(&committed_record);
                match store.write_deploy_status(&pending_record).await {
                    Ok(()) => {
                        if let Some(prepared) = prepared_record
                            && let Err(update_error) = store
                                .mark_prepared_deploy_applied(
                                    &prepared.prepared_deploy_id,
                                    now_unix_secs(),
                                )
                                .await
                        {
                            warn!(
                                ?update_error,
                                deploy_id = %deploy_id,
                                "failed to mark prepared deploy applied after committed status repair"
                            );
                        }
                    }
                    Err(update_error) => {
                        warn!(
                            ?update_error,
                            deploy_id = %deploy_id,
                            "failed to record committed deploy state after durable commit"
                        );
                    }
                }
            }
        } else if let Some(last_record) = last_written_deploy_record
            && matches!(
                last_record.state(),
                DeployState::Applying | DeployState::CheckpointCommitted
            )
        {
            let failed_record = if last_record.state() == DeployState::CheckpointCommitted {
                failed_after_checkpoint_deploy_record(last_record, &status_error)
            } else {
                failed_deploy_record(last_record, &status_error)
            };
            if let Err(update_error) = store.write_deploy_status(&failed_record).await {
                warn!(
                    ?update_error,
                    deploy_id = %deploy_id,
                    "failed to record terminal failed deploy state after apply error"
                );
            }
        }
    }

    result
}

async fn cleanup_uncommitted_volume_clones_after_failure(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    deploy_id: &DeployId,
    executed_volume_clones: &BTreeMap<String, ExecutedVolumeClone>,
    checkpointed_volumes: &BTreeSet<String>,
    started_clone_volumes: &BTreeSet<String>,
) -> Vec<String> {
    let mut cleanup_errors = Vec::new();
    for clone in executed_volume_clones.values() {
        if checkpointed_volumes.contains(&clone.volume_name)
            || started_clone_volumes.contains(&clone.volume_name)
        {
            continue;
        }
        if let Err(error) = participant_client
            .cleanup_volume_clone(
                &clone.target_machine,
                &participants.namespace,
                deploy_id,
                participant::CleanupVolumeCloneRequest {
                    volume: clone.volume_name.clone(),
                    source_namespace: clone.source_namespace.clone(),
                    source_volume: clone.source_volume.clone(),
                    snapshot: clone.snapshot_name.clone(),
                },
            )
            .await
        {
            warn!(
                ?error,
                deploy_id = %deploy_id,
                volume = %clone.volume_name,
                "failed to clean up uncommitted volume clone after deploy failure"
            );
            cleanup_errors.push(format!("{}: {error}", clone.volume_name));
        }
    }
    cleanup_errors
}

fn volume_has_started_attached_service(
    plan: &ResolvedPlan,
    volume_name: &str,
    started: &HashMap<(String, String), InstanceStatusRecord>,
) -> bool {
    plan.volumes()
        .iter()
        .find(|volume| volume.declaration.name == volume_name)
        .is_some_and(|volume| {
            volume.attached_services.iter().any(|service| {
                started
                    .keys()
                    .any(|(started_service, _)| started_service == service)
            })
        })
}

fn cleanup_eligible_unstarted_volume_clones(
    plan: &ResolvedPlan,
    branches: &BTreeMap<String, ExecutedVolumeClone>,
    started_or_attempted_services: &BTreeSet<String>,
) -> BTreeMap<String, ExecutedVolumeClone> {
    branches
        .iter()
        .filter(|(volume_name, _branch)| {
            !volume_has_started_or_attempted_attached_service(
                plan,
                volume_name,
                started_or_attempted_services,
            )
        })
        .map(|(volume_name, branch)| (volume_name.clone(), branch.clone()))
        .collect()
}

fn volume_has_started_or_attempted_attached_service(
    plan: &ResolvedPlan,
    volume_name: &str,
    started_or_attempted_services: &BTreeSet<String>,
) -> bool {
    plan.volumes()
        .iter()
        .find(|volume| volume.declaration.name == volume_name)
        .is_some_and(|volume| {
            volume
                .attached_services
                .iter()
                .any(|service| started_or_attempted_services.contains(service))
        })
}

fn is_post_commit_routing_publish_failure(error: &Error) -> bool {
    matches!(
        error,
        Error::Store(StoreError::DeployCommitRoutingPublishFailed { .. })
    )
}

struct PhaseExecution {
    started: HashMap<(String, String), InstanceStatusRecord>,
    volume_movements: BTreeMap<String, ExecutedVolumeMove>,
    volume_branches: BTreeMap<String, ExecutedVolumeClone>,
    events: Vec<DeployEvent>,
    running_phase: DeployPhaseRecord,
}

struct PhaseExecutionError {
    error: Error,
    phase_reached_running: bool,
    started_or_attempted_services: BTreeSet<String>,
}

struct PhaseWorkError {
    error: Error,
    started_or_attempted_services: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ExecutedVolumeMove {
    volume_name: String,
    from_machine: MachineId,
    to_machine: MachineId,
    phase_id: DeployPhaseId,
    snapshot_name: String,
    snapshot_guid: u64,
    bytes_transferred: u64,
}

#[derive(Debug, Clone)]
struct ExecutedVolumeClone {
    volume_name: String,
    source_namespace: ployz_types::spec::Namespace,
    source_volume: String,
    source_machine: MachineId,
    target_machine: MachineId,
    data_policy: VolumeCloneDataPolicy,
    consistency: VolumeCloneConsistency,
    phase_id: DeployPhaseId,
    snapshot_name: String,
    snapshot_guid: u64,
    target_dataset: String,
}

async fn execute_phase(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    prepared: &PreparedDeploy,
    phase: &DeployPhasePlan,
    started_at: u64,
    stopped_uncommitted_instance_ids: &mut BTreeSet<String>,
) -> std::result::Result<PhaseExecution, PhaseExecutionError> {
    let running = deploy_phase_record(&prepared, &phase, DeployPhaseState::Running, started_at);
    if let Err(error) = store.upsert_deploy_phase(&running).await {
        return Err(PhaseExecutionError {
            error,
            phase_reached_running: false,
            started_or_attempted_services: BTreeSet::new(),
        });
    }

    let mut events = Vec::new();
    events.push(DeployEvent {
        step: "phase".into(),
        message: format!("started phase {} ({})", phase.phase_id, phase.name),
    });

    let included_phase_volumes = phase_volumes(phase);
    let volume_clones = match execute_volume_clones(
        participant_client,
        participants,
        prepared.plan(),
        &phase.phase_id,
        Some(&included_phase_volumes),
        stopped_uncommitted_instance_ids,
    )
    .await
    {
        Ok(volume_clones) => {
            events.extend(volume_clones.events.clone());
            volume_clones
        }
        Err(error) => {
            if let Err(update_error) =
                mark_phase_failed(store, running, &error, now_unix_secs()).await
            {
                warn!(
                    ?update_error,
                    deploy_id = %prepared.deploy_id(),
                    phase_id = %phase.phase_id,
                    "failed to record terminal failed deploy phase after phase error"
                );
            }
            return Err(PhaseExecutionError {
                error,
                phase_reached_running: true,
                started_or_attempted_services: BTreeSet::new(),
            });
        }
    };

    let execution = async {
        let volume_moves = execute_volume_moves(
            participant_client,
            participants,
            prepared.plan(),
            &phase.phase_id,
            Some(&included_phase_volumes),
        )
        .await
        .map_err(|error| PhaseWorkError {
            error,
            started_or_attempted_services: BTreeSet::new(),
        })?;
        events.extend(volume_moves.events);

        let phase_services = phase_services(phase);
        let startup = match run_phase_startup_for_services(
            store,
            participant_client,
            participants,
            prepared.plan(),
            Some(&phase_services),
        )
        .await
        {
            Ok(startup) => startup,
            Err(failure) => {
                events.extend(failure.partial.events);
                return Err(PhaseWorkError {
                    error: failure.error,
                    started_or_attempted_services: failure.partial.started_or_attempted_services,
                });
            }
        };
        events.extend(startup.events);

        Ok::<_, PhaseWorkError>((startup.started, volume_moves.movements, events))
    }
    .await;

    match execution {
        Ok((started, volume_movements, events)) => Ok(PhaseExecution {
            started,
            volume_movements,
            volume_branches: volume_clones.branches,
            events,
            running_phase: running,
        }),
        Err(error) => {
            let cleanup_branches = cleanup_eligible_unstarted_volume_clones(
                prepared.plan(),
                &volume_clones.branches,
                &error.started_or_attempted_services,
            );
            let cleanup_errors = cleanup_uncommitted_volume_clones(
                participant_client,
                participants,
                prepared.plan().namespace(),
                prepared.deploy_id(),
                &cleanup_branches,
            )
            .await;
            let started_or_attempted_services = error.started_or_attempted_services;
            let error = error_with_clone_cleanup_failures(error.error, cleanup_errors);
            if let Err(update_error) =
                mark_phase_failed(store, running, &error, now_unix_secs()).await
            {
                warn!(
                    ?update_error,
                    deploy_id = %prepared.deploy_id(),
                    phase_id = %phase.phase_id,
                    "failed to record terminal failed deploy phase after phase error"
                );
            }
            Err(PhaseExecutionError {
                error,
                phase_reached_running: true,
                started_or_attempted_services,
            })
        }
    }
}

async fn mark_phase_succeeded(
    store: &StoreDriver,
    running_phase: Option<DeployPhaseRecord>,
    commit_deploy_id: Option<DeployId>,
) -> Result<()> {
    let Some(mut phase) = running_phase else {
        return Ok(());
    };
    phase
        .mark_succeeded(commit_deploy_id, now_unix_secs())
        .map_err(|error| Error::operation("deploy_phase", error))?;
    store.upsert_deploy_phase(&phase).await
}

async fn record_phase_succeeded_after_commit(
    store: &StoreDriver,
    deploy_id: &DeployId,
    running_phase: Option<DeployPhaseRecord>,
    commit_deploy_id: Option<DeployId>,
) {
    if let Err(update_error) = mark_phase_succeeded(store, running_phase, commit_deploy_id).await {
        warn!(
            ?update_error,
            deploy_id = %deploy_id,
            "failed to record terminal succeeded deploy phase after commit"
        );
    }
}

async fn record_phases_succeeded_after_commit(
    store: &StoreDriver,
    deploy_id: &DeployId,
    phases: Vec<DeployPhaseRecord>,
    commit_deploy_id: Option<DeployId>,
) {
    for phase in phases {
        record_phase_succeeded_after_commit(
            store,
            deploy_id,
            Some(phase),
            commit_deploy_id.clone(),
        )
        .await;
    }
}

async fn mark_phase_failed(
    store: &StoreDriver,
    mut phase: DeployPhaseRecord,
    error: &Error,
    completed_at: u64,
) -> Result<()> {
    phase.mark_failed(completed_at, deploy_phase_failure(error));
    store.upsert_deploy_phase(&phase).await
}

async fn mark_phase_failed_best_effort(
    store: &StoreDriver,
    phase: DeployPhaseRecord,
    error: &Error,
    deploy_id: &DeployId,
) {
    if let Err(update_error) = mark_phase_failed(store, phase, error, now_unix_secs()).await {
        warn!(
            ?update_error,
            deploy_id = %deploy_id,
            "failed to record terminal failed deploy phase after phase error"
        );
    }
}

async fn mark_phase_and_pending_phases_failed_best_effort(
    store: &StoreDriver,
    phase: DeployPhaseRecord,
    pending_phases: &mut Vec<DeployPhaseRecord>,
    unstarted_phases: &mut Vec<DeployPhaseRecord>,
    error: &Error,
    deploy_id: &DeployId,
) {
    mark_phase_failed_best_effort(store, phase, error, deploy_id).await;
    mark_pending_and_unstarted_phases_failed_best_effort(
        store,
        pending_phases,
        unstarted_phases,
        error,
        deploy_id,
    )
    .await;
}

async fn mark_pending_and_unstarted_phases_failed_best_effort(
    store: &StoreDriver,
    pending_phases: &mut Vec<DeployPhaseRecord>,
    unstarted_phases: &mut Vec<DeployPhaseRecord>,
    error: &Error,
    deploy_id: &DeployId,
) {
    mark_phases_failed_best_effort(store, std::mem::take(pending_phases), error, deploy_id).await;
    mark_phases_failed_best_effort(store, std::mem::take(unstarted_phases), error, deploy_id).await;
}

async fn mark_phases_failed_best_effort(
    store: &StoreDriver,
    phases: Vec<DeployPhaseRecord>,
    error: &Error,
    deploy_id: &DeployId,
) {
    for phase in phases {
        mark_phase_failed_best_effort(store, phase, error, deploy_id).await;
    }
}

fn remove_phase_record(
    phases: &mut Vec<DeployPhaseRecord>,
    phase_id: &DeployPhaseId,
) -> Option<DeployPhaseRecord> {
    let index = phases
        .iter()
        .position(|phase| phase.phase_id == *phase_id)?;
    Some(phases.remove(index))
}

fn deploy_phase_failure(error: &Error) -> DeployPhaseFailure {
    DeployPhaseFailure {
        code: deploy_phase_failure_code(error).into(),
        message: error.to_string(),
    }
}

fn deploy_phase_failure_code(error: &Error) -> &'static str {
    match error {
        Error::Deploy(_) => "DEPLOY_ERROR",
        Error::Image(_) => "IMAGE_ERROR",
        Error::Build(_) => "BUILD_ERROR",
        Error::Runtime(_) => "RUNTIME_ERROR",
        Error::Storage(_) => "STORAGE_ERROR",
        Error::Store(_) => "STORE_ERROR",
        Error::Coordination(_) => "COORDINATION_ERROR",
        Error::Certificate(_) => "CERTIFICATE_ERROR",
        Error::InviteAlreadyExists { .. } => "INVITE_ALREADY_EXISTS",
        Error::InviteNotFound { .. } => "INVITE_NOT_FOUND",
        Error::InviteRevoked { .. } => "INVITE_REVOKED",
        Error::InviteExpired { .. } => "INVITE_EXPIRED",
        Error::InviteConsumed { .. } => "INVITE_CONSUMED",
        Error::SubscriptionLagged { .. } => "SUBSCRIPTION_LAGGED",
        Error::RoutingEventAckReceiverClosed { .. } => "ROUTING_EVENT_ACK_RECEIVER_CLOSED",
        Error::Operation { operation, .. } => operation,
    }
}

fn deploy_phase_record(
    prepared: &PreparedDeploy,
    phase: &DeployPhasePlan,
    state: DeployPhaseState,
    started_at: u64,
) -> DeployPhaseRecord {
    let state = match state {
        DeployPhaseState::Pending => DeployPhaseRecordState::pending(phase.commit_policy),
        DeployPhaseState::Running => DeployPhaseRecordState::running(phase.commit_policy),
        DeployPhaseState::Succeeded { .. } | DeployPhaseState::Failed { .. } => {
            unreachable!("deploy phase records start pending or running")
        }
    };
    DeployPhaseRecord {
        namespace: prepared.plan().namespace().clone(),
        deploy_id: prepared.deploy_id().clone(),
        phase_id: phase.phase_id.clone(),
        name: phase.name.clone(),
        order: phase.order,
        state,
        rollback_policy: phase.rollback_policy,
        advance_policy: phase.advance_policy,
        after: phase.after.clone(),
        participants: phase.participants.clone(),
        work: phase.work.clone(),
        started_at,
    }
}

fn checkpoint_deploy_record(prepared: &PreparedDeploy) -> Result<DeployRecord> {
    let mut record = prepared.applying_record().clone();
    let summary_json = serde_json::to_string(prepared.preview())
        .map_err(|error| Error::operation("deploy_apply", format!("serialize preview: {error}")))?;
    record.mark_checkpoint_committed(summary_json);
    Ok(record)
}

fn committed_deploy_record(
    prepared: &PreparedDeploy,
    state: DeployState,
    deploy_record_id: DeployId,
    committed_at: u64,
) -> Result<DeployRecord> {
    let mut record = prepared.applying_record().clone();
    record.deploy_id = deploy_record_id;
    let summary_json = serde_json::to_string(prepared.preview())
        .map_err(|error| Error::operation("deploy_apply", format!("serialize preview: {error}")))?;
    match state {
        DeployState::Committed => record.mark_committed(committed_at, committed_at, summary_json),
        DeployState::CleanupPending => {
            record.mark_committed(committed_at, committed_at, summary_json);
            record
                .mark_cleanup_pending(committed_at)
                .expect("committed deploy can become cleanup pending");
        }
        DeployState::CheckpointCommitted => record.mark_checkpoint_committed(summary_json),
        DeployState::Planning
        | DeployState::Applying
        | DeployState::FailedAfterCheckpoint
        | DeployState::Failed => {
            return Err(Error::operation(
                "deploy_apply",
                format!("invalid committed deploy terminal state {state}"),
            ));
        }
    }
    Ok(record)
}

fn failed_deploy_record(mut record: DeployRecord, error: &Error) -> DeployRecord {
    let finished_at = now_unix_secs();
    let mut summary_json = record.summary_json().to_string();
    if let Ok(mut preview) = serde_json::from_str::<DeployPreview>(&record.summary_json()) {
        preview.warnings.push(format!("deploy failed: {error}"));
        if let Ok(next_summary_json) = serde_json::to_string(&preview) {
            summary_json = next_summary_json;
        }
    }
    record.mark_failed(finished_at, summary_json);
    record
}

fn failed_after_checkpoint_deploy_record(mut record: DeployRecord, error: &Error) -> DeployRecord {
    let finished_at = now_unix_secs();
    let mut summary_json = record.summary_json().to_string();
    if let Ok(mut preview) = serde_json::from_str::<DeployPreview>(&record.summary_json()) {
        preview
            .warnings
            .push(format!("deploy failed after checkpoint: {error}"));
        if let Ok(next_summary_json) = serde_json::to_string(&preview) {
            summary_json = next_summary_json;
        }
    }
    record.mark_failed_after_checkpoint(finished_at, summary_json);
    record
}

fn phase_commit_timestamp(prepared: &PreparedDeploy, phase: &DeployPhasePlan) -> u64 {
    prepared.applying_record().started_at + u64::from(phase.order) + 1
}

fn ensure_volume_move_execution_supported(
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

fn ensure_volume_clone_execution_supported(
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

#[cfg(test)]
pub(super) async fn run_phase_startup(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
) -> Result<PhaseStartupResult> {
    run_phase_startup_for_services(store, participant_client, participants, plan, None)
        .await
        .map_err(|failure| failure.error)
}

async fn run_phase_startup_for_services(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    plan: &ResolvedPlan,
    included_services: Option<&BTreeSet<String>>,
) -> std::result::Result<PhaseStartupResult, PhaseStartupFailure> {
    let mut phase_queues: BTreeMap<u32, BTreeMap<MachineId, Vec<StartTask>>> = BTreeMap::new();
    for service in plan.services() {
        if let Some(included_services) = included_services
            && !included_services.contains(&service.service)
        {
            continue;
        }
        let Some(phase) = service.phase() else {
            continue;
        };
        let Some(spec_json) = service.spec_json() else {
            continue;
        };
        for slot in service.slots.iter().filter(|slot| {
            matches!(
                slot.action(),
                DeployChangeKind::Create | DeployChangeKind::Replace
            )
        }) {
            phase_queues
                .entry(phase)
                .or_default()
                .entry(slot.machine_id().clone())
                .or_default()
                .push(StartTask {
                    service: service.service.clone(),
                    slot_id: slot.slot_id().clone(),
                    machine_id: slot.machine_id().clone(),
                    instance_id: InstanceId::new(Uuid::new_v4().to_string()),
                    spec_json: spec_json.to_string(),
                    volumes_json: plan.volumes_json().to_string(),
                });
        }
    }

    let mut result = PhaseStartupResult::default();
    for (_phase, machine_tasks) in phase_queues {
        let mut pending_tasks = machine_tasks.into_iter();
        let mut in_flight = FuturesUnordered::new();
        let start_machine_queue = |machine_id: MachineId, tasks: Vec<StartTask>| async move {
            let machine = match participants.get(&machine_id) {
                Ok(machine) => machine,
                Err(error) => {
                    return Err(MachineStartupFailure {
                        error,
                        partial: MachineStartupResult {
                            machine_id,
                            events: Vec::new(),
                            started: Vec::new(),
                            started_or_attempted_services: BTreeSet::new(),
                        },
                    });
                }
            };
            run_machine_start_queue(store, participant_client, participants, machine, tasks).await
        };

        for _ in 0..PHASE_MACHINE_CONCURRENCY {
            let Some((machine_id, tasks)) = pending_tasks.next() else {
                break;
            };
            in_flight.push(start_machine_queue(machine_id, tasks));
        }

        let mut saw_failure = false;
        let mut machine_results = Vec::new();
        while let Some(machine_result) = in_flight.next().await {
            if !saw_failure {
                match &machine_result {
                    Ok(_) => {
                        if let Some((machine_id, tasks)) = pending_tasks.next() {
                            in_flight.push(start_machine_queue(machine_id, tasks));
                        }
                    }
                    Err(_failure) => {
                        saw_failure = true;
                    }
                }
            }
            machine_results.push(machine_result);
        }

        machine_results.sort_by(|left, right| {
            let left_id = match left {
                Ok(result) => &result.machine_id,
                Err(failure) => &failure.partial.machine_id,
            };
            let right_id = match right {
                Ok(result) => &result.machine_id,
                Err(failure) => &failure.partial.machine_id,
            };
            left_id.as_str().cmp(&right_id.as_str())
        });
        let mut first_error = None;
        for machine_result in machine_results {
            let machine_result = match machine_result {
                Ok(machine_result) => machine_result,
                Err(failure) => {
                    if first_error.is_none() {
                        first_error = Some(failure.error);
                    }
                    failure.partial
                }
            };
            result.events.extend(machine_result.events);
            result
                .started_or_attempted_services
                .extend(machine_result.started_or_attempted_services);
            for started in machine_result.started {
                result.started.insert(
                    (started.service, started.slot_id.as_str().to_string()),
                    started.status,
                );
            }
        }
        if let Some(error) = first_error {
            return Err(PhaseStartupFailure {
                error,
                partial: result,
            });
        }
    }

    Ok(result)
}

pub(super) fn ensure_plan_stable(
    initial: &ResolvedPlan,
    final_plan: &ResolvedPlan,
    expected_baseline: Option<&DeployPreviewBaseline>,
) -> Result<()> {
    let initial_fingerprint = initial.fingerprint();
    let final_fingerprint = final_plan.fingerprint();
    if final_fingerprint.participants != initial_fingerprint.participants {
        return Err(Error::Deploy(DeployError::ParticipantSetChanged));
    }
    if let Some(expected) = expected_baseline {
        let actual = final_plan.baseline();
        if &actual != expected {
            return Err(deploy_baseline_changed_error(expected.clone(), actual));
        }
    }
    if final_fingerprint != initial_fingerprint {
        return Err(Error::Deploy(DeployError::ExecutionPlanChanged));
    }
    Ok(())
}

fn ensure_deploy_baseline(
    expected_baseline: Option<&DeployPreviewBaseline>,
    plan: &ResolvedPlan,
) -> Result<()> {
    let Some(expected) = expected_baseline else {
        return Ok(());
    };
    if expected.is_empty() {
        return Err(Error::Deploy(DeployError::DeployOptionInvalid {
            field: "expected_baseline".into(),
            message: "must be omitted or set to a non-empty preview baseline".into(),
        }));
    }
    if !expected.is_canonical() {
        return Err(Error::Deploy(DeployError::DeployOptionInvalid {
            field: "expected_baseline".into(),
            message: "fingerprint must match preview baseline components".into(),
        }));
    }
    let actual = plan.baseline();
    if &actual == expected {
        return Ok(());
    }
    Err(deploy_baseline_changed_error(expected.clone(), actual))
}

fn deploy_baseline_changed_error(
    expected: DeployPreviewBaseline,
    actual: DeployPreviewBaseline,
) -> Error {
    Error::Deploy(DeployError::DeployBaselineChanged {
        diff: DeployBaselineDiff::new(expected, actual),
    })
}

async fn execute_volume_moves(
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
                &participants.deploy_id,
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

async fn execute_volume_clones(
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
            &participants.deploy_id,
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
                &participants.deploy_id,
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
                    &participants.deploy_id,
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

struct VolumeMoveExecution {
    events: Vec<DeployEvent>,
    movements: BTreeMap<String, ExecutedVolumeMove>,
}

struct VolumeCloneExecution {
    events: Vec<DeployEvent>,
    branches: BTreeMap<String, ExecutedVolumeClone>,
}

async fn cleanup_uncommitted_volume_clones(
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    namespace: &ployz_types::spec::Namespace,
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

fn error_with_clone_cleanup_failures(error: Error, cleanup_errors: Vec<String>) -> Error {
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
    for status in &participants.instances {
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
                &participants.deploy_id,
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
                &participants.deploy_id,
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
    for status in &participants.instances {
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
                &participants.deploy_id,
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
                &participants.deploy_id,
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

async fn run_machine_start_queue(
    store: &StoreDriver,
    participant_client: &dyn DeployParticipantClient,
    participants: &ParticipantSet,
    machine: &MachineMembership,
    tasks: Vec<StartTask>,
) -> std::result::Result<MachineStartupResult, MachineStartupFailure> {
    let machine_id = match tasks.first() {
        Some(task) => task.machine_id.clone(),
        None => {
            return Err(MachineStartupFailure {
                error: Error::Deploy(DeployError::EmptyMachineStartQueue),
                partial: MachineStartupResult {
                    machine_id: machine.id.clone(),
                    events: Vec::new(),
                    started: Vec::new(),
                    started_or_attempted_services: BTreeSet::new(),
                },
            });
        }
    };
    let mut events = Vec::new();
    let mut started = Vec::new();
    let mut started_or_attempted_services = BTreeSet::new();

    for task in tasks {
        started_or_attempted_services.insert(task.service.clone());
        let status = match participant_client
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
            .await
        {
            Ok(status) => status,
            Err(error) => {
                return Err(MachineStartupFailure {
                    error,
                    partial: MachineStartupResult {
                        machine_id,
                        events,
                        started,
                        started_or_attempted_services,
                    },
                });
            }
        };
        let started_slot = StartedSlot {
            service: task.service.clone(),
            slot_id: task.slot_id.clone(),
            status: status.clone(),
        };
        if let Err(error) = store.record_instance_status(&status).await {
            started.push(started_slot);
            return Err(MachineStartupFailure {
                error,
                partial: MachineStartupResult {
                    machine_id,
                    events,
                    started,
                    started_or_attempted_services,
                },
            });
        }
        events.push(DeployEvent {
            step: "start_candidate".into(),
            message: format!(
                "starting {} slot {} as instance {} on {}",
                task.service, task.slot_id, status.instance_id, task.machine_id
            ),
        });
        started.push(started_slot);
    }

    Ok(MachineStartupResult {
        machine_id,
        events,
        started,
        started_or_attempted_services,
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

fn phase_services(phase: &DeployPhasePlan) -> BTreeSet<String> {
    phase
        .work
        .iter()
        .filter_map(|work| match work {
            DeployPhaseWork::Service { service, .. } => Some(service.clone()),
            DeployPhaseWork::Volume { .. }
            | DeployPhaseWork::VolumeMove { .. }
            | DeployPhaseWork::VolumeClone { .. } => None,
        })
        .collect()
}

fn phase_volumes(phase: &DeployPhasePlan) -> BTreeSet<String> {
    phase
        .work
        .iter()
        .filter_map(|work| match work {
            DeployPhaseWork::Volume { volume, .. }
            | DeployPhaseWork::VolumeMove { volume, .. }
            | DeployPhaseWork::VolumeClone { volume, .. } => Some(volume.clone()),
            DeployPhaseWork::Service { .. } => None,
        })
        .collect()
}

fn phase_commit_deploy_id(
    deploy_id: &DeployId,
    phase_id: &crate::model::DeployPhaseId,
) -> DeployId {
    DeployId::new(format!(
        "{}:phase:{}",
        deploy_id.as_str(),
        phase_id.as_str()
    ))
}

fn changed_services(
    plan: &ResolvedPlan,
) -> impl Iterator<Item = &crate::deploy::plan::PlannedService> {
    plan.services()
        .iter()
        .filter(|service| service.action() != DeployChangeKind::Unchanged)
}

fn remaining_changed_services(
    plan: &ResolvedPlan,
    checkpointed_services: &BTreeSet<String>,
) -> BTreeSet<String> {
    changed_services(plan)
        .filter(|service| !checkpointed_services.contains(&service.service))
        .map(|service| service.service.clone())
        .collect()
}

fn remaining_branch_lineage_services(
    plan: &ResolvedPlan,
    checkpointed_services: &BTreeSet<String>,
) -> BTreeSet<String> {
    plan.services()
        .iter()
        .filter(|service| service.branch_source().is_some())
        .filter(|service| !checkpointed_services.contains(&service.service))
        .map(|service| service.service.clone())
        .collect()
}

fn remaining_changed_volumes(
    plan: &ResolvedPlan,
    checkpointed_volumes: &BTreeSet<String>,
) -> BTreeSet<String> {
    plan.volumes()
        .iter()
        .filter(|volume| !matches!(volume_record_change(volume), VolumeChange::Skip))
        .filter(|volume| !checkpointed_volumes.contains(&volume.declaration.name))
        .map(|volume| volume.declaration.name.clone())
        .collect()
}

fn removed_services_for_phase(
    plan: &ResolvedPlan,
    phase_services: &BTreeSet<String>,
) -> Vec<String> {
    changed_services(plan)
        .filter(|service| service.action() == DeployChangeKind::Remove)
        .filter(|service| phase_services.contains(&service.service))
        .map(|service| service.service.clone())
        .collect()
}

fn removed_services_for_final_commit(
    plan: &ResolvedPlan,
    checkpointed_services: &BTreeSet<String>,
) -> Vec<String> {
    changed_services(plan)
        .filter(|service| service.action() == DeployChangeKind::Remove)
        .filter(|service| !checkpointed_services.contains(&service.service))
        .map(|service| service.service.clone())
        .collect()
}

fn build_phase_commit(
    prepared: &PreparedDeploy,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    executed_volume_moves: &BTreeMap<String, ExecutedVolumeMove>,
    executed_volume_clones: &BTreeMap<String, ExecutedVolumeClone>,
    included_services: &BTreeSet<String>,
    included_branch_lineage_services: &BTreeSet<String>,
    included_volumes: &BTreeSet<String>,
    removed_services: Vec<String>,
    removed_volumes: Vec<String>,
    phase_commits: Vec<DeployPhaseCommitRecord>,
    deploy_state: DeployState,
    deploy_record_id: DeployId,
    committed_at: u64,
) -> Result<DeployCommit> {
    let revisions = prepared
        .revisions()
        .iter()
        .filter(|revision| included_services.contains(&revision.service))
        .cloned()
        .collect::<Vec<_>>();
    let releases = build_committed_releases_for_services(
        prepared.plan(),
        started,
        prepared.deploy_id(),
        committed_at,
        Some(included_services),
    )?;
    let branch_lineage = prepared
        .plan()
        .service_branch_lineage_records(prepared.deploy_id(), committed_at)
        .into_iter()
        .filter(|record| included_branch_lineage_services.contains(&record.service))
        .collect::<Vec<_>>();
    let volume_movements = build_volume_movement_records(
        prepared,
        executed_volume_moves,
        included_volumes,
        &deploy_record_id,
        committed_at,
    );
    let volume_branches = build_volume_branch_records(
        prepared,
        executed_volume_clones,
        included_volumes,
        &deploy_record_id,
        committed_at,
    );
    let volumes = build_committed_volumes_for_names(
        prepared.plan(),
        started,
        prepared.deploy_id(),
        committed_at,
        Some(included_volumes),
    )?;
    let deploy = committed_deploy_record(prepared, deploy_state, deploy_record_id, committed_at)?;

    Ok(DeployCommit {
        namespace: prepared.plan().namespace().clone(),
        revisions,
        removed_services,
        removed_volumes,
        branch_lineage,
        volume_movements,
        volume_branches,
        phase_commits,
        releases,
        volumes,
        deploy,
    })
}

fn phase_commit_record(
    prepared: &PreparedDeploy,
    phase: &DeployPhaseRecord,
    commit_deploy_id: &DeployId,
    committed_at: u64,
) -> DeployPhaseCommitRecord {
    DeployPhaseCommitRecord {
        namespace: prepared.plan().namespace().clone(),
        deploy_id: prepared.deploy_id().clone(),
        phase_id: phase.phase_id.clone(),
        commit_deploy_id: commit_deploy_id.clone(),
        committed_at,
    }
}

fn build_volume_movement_records(
    prepared: &PreparedDeploy,
    executed_volume_moves: &BTreeMap<String, ExecutedVolumeMove>,
    included_volumes: &BTreeSet<String>,
    commit_deploy_id: &DeployId,
    committed_at: u64,
) -> Vec<VolumeMovementRecord> {
    executed_volume_moves
        .values()
        .filter(|movement| included_volumes.contains(&movement.volume_name))
        .map(|movement| VolumeMovementRecord {
            namespace: prepared.plan().namespace().clone(),
            volume_name: movement.volume_name.clone(),
            from_machine: movement.from_machine.clone(),
            to_machine: movement.to_machine.clone(),
            final_machine: movement.to_machine.clone(),
            deploy_id: prepared.deploy_id().clone(),
            commit_deploy_id: commit_deploy_id.clone(),
            phase_id: Some(movement.phase_id.clone()),
            snapshot_name: movement.snapshot_name.clone(),
            snapshot_guid: movement.snapshot_guid,
            bytes_transferred: movement.bytes_transferred,
            created_at: committed_at,
        })
        .collect()
}

fn build_volume_branch_records(
    prepared: &PreparedDeploy,
    executed_volume_clones: &BTreeMap<String, ExecutedVolumeClone>,
    included_volumes: &BTreeSet<String>,
    commit_deploy_id: &DeployId,
    committed_at: u64,
) -> Vec<VolumeBranchLineageRecord> {
    executed_volume_clones
        .values()
        .filter(|branch| included_volumes.contains(&branch.volume_name))
        .map(|branch| VolumeBranchLineageRecord {
            namespace: prepared.plan().namespace().clone(),
            volume_name: branch.volume_name.clone(),
            source_namespace: branch.source_namespace.clone(),
            source_volume_name: branch.source_volume.clone(),
            source_machine: branch.source_machine.clone(),
            target_machine: branch.target_machine.clone(),
            data_policy: branch.data_policy,
            consistency: branch.consistency,
            deploy_id: prepared.deploy_id().clone(),
            commit_deploy_id: commit_deploy_id.clone(),
            phase_id: Some(branch.phase_id.clone()),
            snapshot_name: branch.snapshot_name.clone(),
            snapshot_guid: branch.snapshot_guid,
            target_dataset: branch.target_dataset.clone(),
            created_at: committed_at,
        })
        .collect()
}

fn build_committed_volumes_for_names(
    plan: &ResolvedPlan,
    started: &HashMap<(String, String), InstanceStatusRecord>,
    deploy_id: &DeployId,
    now: u64,
    included_volumes: Option<&BTreeSet<String>>,
) -> Result<Vec<VolumeRecord>> {
    let mut volumes = Vec::new();
    for planned in plan
        .volumes()
        .iter()
        .filter(|planned| !matches!(volume_record_change(planned), VolumeChange::Skip))
    {
        if let Some(included_volumes) = included_volumes
            && !included_volumes.contains(&planned.declaration.name)
        {
            continue;
        }
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
            .current()
            .map(|record| record.created_at)
            .unwrap_or(now);
        let created_by_deploy_id = planned
            .current()
            .map(|record| record.created_by_deploy_id.clone())
            .unwrap_or_else(|| deploy_id.clone());
        volumes.push(VolumeRecord {
            namespace: plan.namespace().clone(),
            volume_name: planned.declaration.name.clone(),
            scope: planned.declaration.scope,
            machine_id: planned.machine_id.clone(),
            quota: planned.declaration.quota.to_string(),
            mode: planned.declaration.mode.to_string(),
            owner: planned.declaration.owner.to_string(),
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
        .map(|machine_id| machine_id.as_str().to_string())
        .collect::<BTreeSet<_>>();

    let mut stale_by_machine: BTreeMap<MachineId, Vec<InstanceStatusRecord>> = BTreeMap::new();
    for status in store.list_instance_status(plan.namespace()).await? {
        if plan
            .active_instance_ids()
            .contains(status.instance_id.as_str())
        {
            continue;
        }
        if !participant_ids.contains(status.machine_id.as_str()) {
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
