use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusSession, FactAuthorizer, FactKey, FactKeyPattern, FactPayload, Grant, IslandId, PrincipalId,
    harness::InMemoryBus,
};
use mvp_commands::{
    CommandContext, CommandError, CommandFact, CommandName, CommandPhaseStore, CommandResult,
    IntentId, StoredCommandPhase, command_intent_fact_key, command_phase_fact_key,
};
use mvp_environment::{
    BranchEnvironmentCommand, BranchEnvironmentRequest, EnvironmentBranchId, EnvironmentCommandId,
    EnvironmentCommandResult, EnvironmentEpoch, EnvironmentFactPayload, EnvironmentFactWriter,
    EnvironmentHeadFact, EnvironmentHeadId, EnvironmentId, EnvironmentPromoteDecisionFact,
    EnvironmentRollbackDecisionFact, EnvironmentRouteRef, EnvironmentServingPendingReason,
    EnvironmentVolumeForkEvidence, EnvironmentVolumeForkParticipant, EnvironmentVolumeForkReply,
    EnvironmentVolumeForkRequest, EnvironmentVolumeRef, PromoteEnvironmentCommand,
    PromoteEnvironmentRequest, RollbackEnvironmentCommand, RollbackEnvironmentRequest,
    environment_branch_fact_key, environment_head_fact_key, environment_promote_decision_fact_key,
    environment_rollback_decision_fact_key, read_environment_heads,
};
use mvp_environment_p2panda::PandaEnvironmentFactWriter;
use mvp_identity::{NodeId, VisibleNodes};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey,
    SharedPandaFactStore,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsProjection, DnsRecordFact, FactCandidate, FactSource,
    GatewayProjection, GatewayRouteProjection, ProjectionReport, ProjectionState, RouteId,
    SnapshotWriteReport,
};
use mvp_routing::{
    DnsCommitId, GatewayCommitId, ProjectionCatchUp, RouteCommitId, ServingCommitId,
    ServingCommitPlan, ServingFactWriter,
};
use mvp_routing_p2panda::PandaServingFactWriter;
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    RoleRequest, assert_serving_role_answer,
    cleanup_orphaned_children as cleanup_process_role_children, request_await_rebuild,
    request_begin_rebuild, request_reload, request_role, serving_role_gateway_revision,
    spawn_process_role, wait_for_serving_role,
};

#[derive(Debug, Serialize)]
struct EnvironmentBranchPromoteRollbackReport {
    scenario: &'static str,
    visible_nodes_at_branch: usize,
    branch_volume_ref: String,
    promoted_volume_ref: String,
    rollback_volume_ref: String,
    baseline_gateway_revision: String,
    promoted_gateway_revision: String,
    rollback_gateway_revision: String,
    serving_alive_without_command_adapter: bool,
    projection_rebuilds: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for environment contract: {error}"))?;
    runtime.block_on(run_async())
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    cleanup_process_role_children(&scenario_dir(
        "environment-branch-promote-rollback-contract",
    ))
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("environment-branch-promote-rollback-contract");
    reset_dir(&root)?;

    let island = IslandId::new("prod");
    let author = Arc::new(PandaFactAuthor::new(PrincipalId::new(
        "environment-operator",
    )));
    let store_path = root.join("p2panda-facts.sqlite");
    let (raw_bus, session) = p2panda_writer_bus(&island, author.principal());
    let fact_authorizer: Arc<dyn FactAuthorizer> = Arc::new(raw_bus);
    let store = SharedPandaFactStore::new(
        PandaFactStore::open_sqlite(
            Arc::clone(&fact_authorizer),
            PandaSqliteOpenConfig::new(&store_path, vec![island.clone()]).with_trusted_author_key(
                PandaTrustedAuthorKey::new(
                    island.clone(),
                    author.principal().clone(),
                    author.author_key(),
                ),
            ),
        )
        .await
        .map_err(|error| format!("open environment p2panda store: {error}"))?,
    );
    let environment_writer =
        PandaEnvironmentFactWriter::new(store.clone(), session.clone(), Arc::clone(&author));
    let serving_writer =
        PandaServingFactWriter::new(store.clone(), session.clone(), Arc::clone(&author));

    let prod = environment("prod")?;
    let branch_env = environment("pr-123")?;
    let branch_id = branch_id("pr-123")?;
    let baseline_plan = serving_plan("serving-prod-1", "fd00::1:8080", "fd00::1", 1);
    let baseline_head = EnvironmentHeadFact {
        environment: prod.clone(),
        epoch: epoch(1)?,
        head_id: head_id("head-prod-1")?,
        source_command_id: command_id("seed-prod")?,
        serving_commit_id: baseline_plan.serving_commit_id.clone(),
        previous_head: None,
        volume_refs: vec![volume("db-prod")?],
        source_branch_id: None,
    };
    environment_writer
        .write_head(baseline_head.clone())
        .await
        .map_err(|error| format!("seed production head: {error}"))?;
    serving_writer
        .write_serving_commit(&baseline_plan)
        .await
        .map_err(|error| format!("seed serving commit: {error}"))?;

    let serving_socket = root.join("serving.sock");
    let p2panda_path = store_path.display().to_string();
    let args = [
        "--fact-source".to_string(),
        "p2panda-sqlite".to_string(),
        "--p2panda-path".to_string(),
        p2panda_path,
        "--p2panda-author".to_string(),
        author.principal().as_str().to_string(),
        "--p2panda-author-key".to_string(),
        author.author_key().as_hex(),
    ];
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut serving = spawn_process_role(
        "environment-branch-serving-projection",
        "serving-projection",
        &root,
        &serving_socket,
        &arg_refs,
    )?;
    wait_for_serving_role(&serving_socket).await?;
    rebuild_projection(&serving_socket).await?;
    let baseline_status = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "environment baseline",
    )
    .await?;
    let baseline_gateway_revision = serving_role_gateway_revision(&baseline_status)?;

    let branch_head = EnvironmentHeadFact {
        environment: branch_env.clone(),
        epoch: epoch(1)?,
        head_id: head_id("head-pr-123-1")?,
        source_command_id: command_id("branch-1")?,
        serving_commit_id: ServingCommitId::new("serving-branch-1"),
        previous_head: None,
        volume_refs: vec![volume("db-pr-123")?],
        source_branch_id: Some(branch_id.clone()),
    };
    let branch_result =
        BranchEnvironmentCommand::new(&store, &session, &environment_writer, &ForkParticipant)
            .execute(BranchEnvironmentRequest {
                command_id: command_id("branch-1")?,
                source_environment: prod.clone(),
                expected_source_epoch: epoch(1)?,
                branch_id: branch_id.clone(),
                branch_environment: branch_env.clone(),
                branch_head: branch_head.clone(),
                route_refs: vec![
                    EnvironmentRouteRef::parse("route-pr-123")
                        .map_err(|error| error.to_string())?,
                ],
                visible_nodes: visible_nodes(),
            })
            .await
            .map_err(|error| format!("branch environment: {error}"))?;
    let branch_fact = read_branch_fact(&store, &island, &session, &prod, &branch_id)?;
    assert_eq_named(
        "branch source head",
        branch_fact.source_head_id.clone(),
        baseline_head.head_id.clone(),
    )?;
    let branch_head_read =
        read_environment_heads(&store, &session, &branch_result.branch.branch_environment)
            .map_err(|error| format!("read branch head: {error}"))?
            .require_current(&branch_result.branch.branch_environment)
            .map_err(|error| format!("require branch head: {error}"))?
            .fact;
    assert_eq_named(
        "branch head volume refs",
        branch_head_read.volume_refs.clone(),
        vec![volume("db-pr-123")?],
    )?;
    rebuild_projection(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "branch does not alter production",
    )
    .await?;

    let promote_plan = serving_plan("serving-promote-1", "fd00::2:8080", "fd00::2", 2);
    let promote_command =
        PromoteEnvironmentCommand::new(&store, &session, &environment_writer, &serving_writer);
    let promote_cx = CommandContext::new(Arc::new(PandaCommandPhaseStore::new(
        store.clone(),
        session.clone(),
        Arc::clone(&author),
    )));
    let promote_request = PromoteEnvironmentRequest {
        command_id: command_id("promote-1")?,
        environment: prod.clone(),
        expected_environment_epoch: epoch(1)?,
        branch_environment: branch_env,
        expected_branch_epoch: epoch(1)?,
        serving_commit: promote_plan.clone(),
        visible_nodes: visible_nodes(),
    };
    let still_pending = promote_command
        .execute_phased(&promote_cx, promote_request.clone(), None)
        .await
        .map_err(|error| format!("phased promote without catchup: {error}"))?;
    assert!(matches!(
        still_pending,
        EnvironmentCommandResult::Pending {
            reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing
        }
    ));
    let promote_decision =
        read_promote_decision(&store, &island, &session, &prod, command_id("promote-1")?)?;
    assert_eq_named(
        "promote decision target volumes",
        promote_decision.target_volume_refs.clone(),
        vec![volume("db-pr-123")?],
    )?;
    let promote_resume_cx = reopened_command_phase_context(
        &fact_authorizer,
        &store_path,
        &island,
        &session,
        Arc::clone(&author),
    )
    .await?;
    rebuild_projection(&serving_socket).await?;
    let promoted_status = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "environment promoted",
    )
    .await?;
    let mut poisoned_promote_request = promote_request;
    poisoned_promote_request.serving_commit =
        serving_plan("serving-promote-poison", "fd00::99:8080", "fd00::99", 99);
    promote_command
        .execute_phased(
            &promote_resume_cx,
            poisoned_promote_request,
            Some(catch_up(&island, &promote_plan)),
        )
        .await
        .map_err(|error| format!("resume phased promote: {error}"))?;
    let promoted_gateway_revision = serving_role_gateway_revision(&promoted_status)?;
    let promoted_head = read_environment_heads(&store, &session, &prod)
        .map_err(|error| format!("read promoted head: {error}"))?
        .require_current(&prod)
        .map_err(|error| format!("require promoted head: {error}"))?
        .fact;
    assert_eq_named(
        "promoted volume ref",
        promoted_head.volume_refs.clone(),
        vec![volume("db-pr-123")?],
    )?;

    let serving_alive_without_command_adapter = serving.is_running()?;

    let rollback_plan = serving_plan("serving-rollback-1", "fd00::1:8080", "fd00::1", 3);
    let rollback_command =
        RollbackEnvironmentCommand::new(&store, &session, &environment_writer, &serving_writer);
    let rollback_cx = CommandContext::new(Arc::new(PandaCommandPhaseStore::new(
        store.clone(),
        session.clone(),
        Arc::clone(&author),
    )));
    let rollback_request = RollbackEnvironmentRequest {
        command_id: command_id("rollback-1")?,
        environment: prod.clone(),
        expected_environment_epoch: epoch(2)?,
        serving_commit: rollback_plan.clone(),
        visible_nodes: visible_nodes(),
    };
    let pending_rollback = rollback_command
        .execute_phased(&rollback_cx, rollback_request.clone(), None)
        .await
        .map_err(|error| format!("phased rollback without catchup: {error}"))?;
    assert!(matches!(
        pending_rollback,
        EnvironmentCommandResult::Pending {
            reason: EnvironmentServingPendingReason::ProjectionCatchUpMissing
        }
    ));
    let rollback_decision =
        read_rollback_decision(&store, &island, &session, &prod, command_id("rollback-1")?)?;
    assert_eq_named(
        "rollback decision target volumes",
        rollback_decision.target_volume_refs.clone(),
        baseline_head.volume_refs.clone(),
    )?;
    let rollback_resume_cx = reopened_command_phase_context(
        &fact_authorizer,
        &store_path,
        &island,
        &session,
        Arc::clone(&author),
    )
    .await?;
    rebuild_projection(&serving_socket).await?;
    let rollback_status = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "environment rollback",
    )
    .await?;
    let mut poisoned_rollback_request = rollback_request;
    poisoned_rollback_request.serving_commit =
        serving_plan("serving-rollback-poison", "fd00::99:8080", "fd00::99", 99);
    rollback_command
        .execute_phased(
            &rollback_resume_cx,
            poisoned_rollback_request,
            Some(catch_up(&island, &rollback_plan)),
        )
        .await
        .map_err(|error| format!("resume phased rollback: {error}"))?;
    let rollback_gateway_revision = serving_role_gateway_revision(&rollback_status)?;

    let rollback_head = read_environment_heads(&store, &session, &prod)
        .map_err(|error| format!("read rollback head: {error}"))?
        .require_current(&prod)
        .map_err(|error| format!("require rollback head: {error}"))?
        .fact;
    assert_eq_named(
        "rollback volume refs",
        rollback_head.volume_refs.clone(),
        baseline_head.volume_refs.clone(),
    )?;

    std::fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete projection sqlite before rebuild: {error}"))?;
    let token = request_begin_rebuild(&serving_socket).await?;
    request_await_rebuild(&serving_socket, token).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "environment rollback after rebuild",
    )
    .await?;
    assert_eq_named(
        "rollback head key",
        environment_head_fact_key(&prod, epoch(3)?).map_err(|error| error.to_string())?,
        environment_head_fact_key(&rollback_head.environment, rollback_head.epoch)
            .map_err(|error| error.to_string())?,
    )?;
    let reopened = SharedPandaFactStore::new(
        PandaFactStore::open_sqlite(
            Arc::clone(&fact_authorizer),
            PandaSqliteOpenConfig::new(&store_path, vec![island.clone()]).with_trusted_author_key(
                PandaTrustedAuthorKey::new(
                    island.clone(),
                    author.principal().clone(),
                    author.author_key(),
                ),
            ),
        )
        .await
        .map_err(|error| format!("reopen environment p2panda store: {error}"))?,
    );
    let replayed_head = read_environment_heads(&reopened, &session, &prod)
        .map_err(|error| format!("read replayed rollback head: {error}"))?
        .require_current(&prod)
        .map_err(|error| format!("require replayed rollback head: {error}"))?
        .fact;
    assert_eq_named("replayed rollback epoch", replayed_head.epoch, epoch(3)?)?;
    assert_eq_named(
        "replayed rollback volumes",
        replayed_head.volume_refs,
        baseline_head.volume_refs.clone(),
    )?;

    request_role(&serving_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown environment serving process: {error:?}"))?;
    serving.wait_for_exit().await?;

    let report = EnvironmentBranchPromoteRollbackReport {
        scenario: "environment-branch-promote-rollback-contract",
        visible_nodes_at_branch: branch_result.visible_nodes.len(),
        branch_volume_ref: single_volume_ref("branch", &branch_result.head.volume_refs)?,
        promoted_volume_ref: single_volume_ref("promoted", &promoted_head.volume_refs)?,
        rollback_volume_ref: single_volume_ref("rollback", &rollback_head.volume_refs)?,
        baseline_gateway_revision,
        promoted_gateway_revision,
        rollback_gateway_revision,
        serving_alive_without_command_adapter,
        projection_rebuilds: 3,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("environment-branch-promote-rollback-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS environment-branch-promote-rollback-contract");
    Ok(())
}

struct ForkParticipant;

impl EnvironmentVolumeForkParticipant for ForkParticipant {
    fn fork_volume<'a>(
        &'a self,
        request: EnvironmentVolumeForkRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = mvp_environment::EnvironmentResult<EnvironmentVolumeForkReply>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(EnvironmentVolumeForkReply {
                evidence: EnvironmentVolumeForkEvidence {
                    command_id: request.command_id,
                    source_environment: request.source_environment,
                    branch_environment: request.branch_environment,
                    source_volume: request.source_volume,
                    forked_volume: EnvironmentVolumeRef::parse("db-pr-123")?,
                },
            })
        })
    }
}

fn read_branch_fact(
    source: &impl FactSource,
    island: &IslandId,
    session: &BusSession,
    environment: &EnvironmentId,
    branch_id: &EnvironmentBranchId,
) -> Result<mvp_environment::EnvironmentBranchFact, String> {
    let key =
        environment_branch_fact_key(environment, branch_id).map_err(|error| error.to_string())?;
    match read_environment_fact_payload(source, island, session, &key)? {
        EnvironmentFactPayload::Branch(fact) => Ok(fact),
        other => Err(format!("expected branch fact at {key}, got {other:?}")),
    }
}

fn read_promote_decision(
    source: &impl FactSource,
    island: &IslandId,
    session: &BusSession,
    environment: &EnvironmentId,
    command_id: EnvironmentCommandId,
) -> Result<EnvironmentPromoteDecisionFact, String> {
    let key = environment_promote_decision_fact_key(environment, &command_id)
        .map_err(|error| error.to_string())?;
    match read_environment_fact_payload(source, island, session, &key)? {
        EnvironmentFactPayload::PromoteDecision(fact) => Ok(fact),
        other => Err(format!(
            "expected promote decision fact at {key}, got {other:?}"
        )),
    }
}

fn read_rollback_decision(
    source: &impl FactSource,
    island: &IslandId,
    session: &BusSession,
    environment: &EnvironmentId,
    command_id: EnvironmentCommandId,
) -> Result<EnvironmentRollbackDecisionFact, String> {
    let key = environment_rollback_decision_fact_key(environment, &command_id)
        .map_err(|error| error.to_string())?;
    match read_environment_fact_payload(source, island, session, &key)? {
        EnvironmentFactPayload::RollbackDecision(fact) => Ok(fact),
        other => Err(format!(
            "expected rollback decision fact at {key}, got {other:?}"
        )),
    }
}

fn read_environment_fact_payload(
    source: &impl FactSource,
    island: &IslandId,
    session: &BusSession,
    key: &FactKey,
) -> Result<EnvironmentFactPayload, String> {
    let pattern = FactKeyPattern::parse(key.as_str()).map_err(|error| error.to_string())?;
    let candidates = source
        .list_candidates(island, &pattern, session)
        .map_err(|error| error.to_string())?;
    let payloads = source
        .read_payloads(island, &candidates, session)
        .map_err(|error| error.to_string())?;
    let payload = candidates
        .iter()
        .find_map(|candidate| {
            (candidate.key() == key)
                .then(|| payloads.get(candidate.content_hash()))
                .flatten()
        })
        .ok_or_else(|| format!("missing environment fact payload for {key}"))?;
    decode_environment_payload(key, payload)
}

fn decode_environment_payload(
    key: &FactKey,
    payload: &FactPayload,
) -> Result<EnvironmentFactPayload, String> {
    serde_json::from_slice(payload.as_bytes())
        .map_err(|error| format!("decode environment fact {key}: {error}"))
}

fn p2panda_writer_bus(island: &IslandId, principal: &PrincipalId) -> (InMemoryBus, BusSession) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let session = authority.grant_in(
        island.clone(),
        principal.clone(),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/environment/>").expect("environment pattern"))
            .with_fact_write(fact_pattern("/facts/serving/>").expect("serving pattern"))
            .with_fact_write(fact_pattern("/facts/command/>").expect("command pattern"))
            .with_fact_read(fact_pattern("/facts/>").expect("operator read pattern")),
    );
    authority.grant_in(
        island.clone(),
        PrincipalId::new("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/>").expect("read pattern")),
    );
    (bus, session)
}

struct PandaCommandPhaseStore {
    store: SharedPandaFactStore,
    session: BusSession,
    author: Arc<PandaFactAuthor>,
}

impl PandaCommandPhaseStore {
    fn new(store: SharedPandaFactStore, session: BusSession, author: Arc<PandaFactAuthor>) -> Self {
        Self {
            store,
            session,
            author,
        }
    }
}

impl CommandPhaseStore for PandaCommandPhaseStore {
    fn read_phase_history(
        &self,
        command: &CommandName,
        intent: &IntentId,
    ) -> CommandResult<Vec<StoredCommandPhase>> {
        let pattern = FactKeyPattern::parse(format!(
            "/facts/command/{}/{}/phase/>",
            command.as_str(),
            intent.as_str()
        ))?;
        let candidates = self
            .store
            .list_candidates(self.session.island(), &pattern, &self.session)
            .map_err(|error| CommandError::Store {
                message: error.to_string(),
            })?;
        command_phase_history(&self.store, &self.session, command, intent, candidates)
    }

    fn write_intent(&self, fact: CommandFact) -> CommandResult<FactKey> {
        let key = command_intent_fact_key(&fact.command, &fact.intent)?;
        let payload = serde_json::to_vec(&fact)
            .map(FactPayload::from)
            .map_err(|error| CommandError::Store {
                message: error.to_string(),
            })?;
        write_command_fact_payload(&self.store, &self.session, &self.author, key, payload)
    }

    fn append_phase(
        &self,
        command: &CommandName,
        intent: &IntentId,
        expected_previous: Option<&StoredCommandPhase>,
        index: u64,
        payload: FactPayload,
    ) -> CommandResult<FactKey> {
        let latest = self.read_latest_phase(command, intent)?;
        let key = command_phase_fact_key(command, intent, index)?;
        match latest.as_ref() {
            Some(stored) if expected_previous != Some(stored) => {
                return Err(CommandError::PhaseAdvanced {
                    key: stored.key.clone(),
                });
            }
            None if expected_previous.is_some() => {
                return Err(CommandError::PhaseAdvanced {
                    key: expected_previous
                        .expect("expected previous checked above")
                        .key
                        .clone(),
                });
            }
            Some(_) | None => {}
        }
        append_command_phase_payload(&self.store, &self.session, &self.author, key, payload)
    }
}

async fn reopened_command_phase_context(
    fact_authorizer: &Arc<dyn FactAuthorizer>,
    store_path: &std::path::Path,
    island: &IslandId,
    session: &BusSession,
    author: Arc<PandaFactAuthor>,
) -> Result<CommandContext, String> {
    let reopened = SharedPandaFactStore::new(
        PandaFactStore::open_sqlite(
            Arc::clone(fact_authorizer),
            PandaSqliteOpenConfig::new(store_path, vec![island.clone()]).with_trusted_author_key(
                PandaTrustedAuthorKey::new(
                    island.clone(),
                    author.principal().clone(),
                    author.author_key(),
                ),
            ),
        )
        .await
        .map_err(|error| format!("reopen command phase p2panda store: {error}"))?,
    );
    Ok(CommandContext::new(Arc::new(PandaCommandPhaseStore::new(
        reopened,
        session.clone(),
        author,
    ))))
}

fn write_command_fact_payload(
    store: &SharedPandaFactStore,
    session: &BusSession,
    author: &Arc<PandaFactAuthor>,
    key: FactKey,
    payload: FactPayload,
) -> CommandResult<FactKey> {
    let outcome = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(store.write_fact_payload(
            session,
            author,
            key.clone(),
            payload,
        ))
    })
    .map_err(|error| CommandError::Store {
        message: error.to_string(),
    })?;
    match outcome {
        mvp_p2panda_facts::PandaFactWriteOutcome::Inserted(_)
        | mvp_p2panda_facts::PandaFactWriteOutcome::AlreadyPresent(_) => Ok(key),
        mvp_p2panda_facts::PandaFactWriteOutcome::Conflict(_) => {
            Err(CommandError::PhaseConflict { key })
        }
    }
}

fn append_command_phase_payload(
    store: &SharedPandaFactStore,
    session: &BusSession,
    author: &Arc<PandaFactAuthor>,
    key: FactKey,
    payload: FactPayload,
) -> CommandResult<FactKey> {
    let outcome = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(store.write_fact_payload(
            session,
            author,
            key.clone(),
            payload,
        ))
    })
    .map_err(|error| CommandError::Store {
        message: error.to_string(),
    })?;
    match outcome {
        mvp_p2panda_facts::PandaFactWriteOutcome::Inserted(_)
        | mvp_p2panda_facts::PandaFactWriteOutcome::AlreadyPresent(_) => Ok(key),
        mvp_p2panda_facts::PandaFactWriteOutcome::Conflict(_) => {
            Err(CommandError::PhaseConflict { key })
        }
    }
}

fn command_phase_history(
    store: &SharedPandaFactStore,
    session: &BusSession,
    command: &CommandName,
    intent: &IntentId,
    candidates: Vec<FactCandidate>,
) -> CommandResult<Vec<StoredCommandPhase>> {
    let mut candidates_by_index = BTreeMap::<u64, Vec<FactCandidate>>::new();
    for candidate in candidates {
        if let Some(index) = command_phase_index(candidate.key(), command, intent) {
            candidates_by_index
                .entry(index)
                .or_default()
                .push(candidate);
        }
    }
    let mut selected = Vec::with_capacity(candidates_by_index.len());
    for (expected_index, (index, candidates)) in (1..).zip(candidates_by_index) {
        if index != expected_index {
            return Err(CommandError::PhaseHistoryGap {
                command: command.clone(),
                intent: intent.clone(),
                expected_index,
                actual_index: index,
            });
        }
        let key = command_phase_fact_key(command, intent, index)?;
        let [candidate] = candidates.as_slice() else {
            return Err(CommandError::PhaseConflict { key });
        };
        if candidate.status() == CandidateStatus::Conflict {
            return Err(CommandError::PhaseConflict { key });
        }
        selected.push(candidate.clone());
    }
    let payloads = store
        .read_payloads(session.island(), &selected, session)
        .map_err(|error| CommandError::Store {
            message: error.to_string(),
        })?;
    selected
        .into_iter()
        .map(|candidate| {
            let index = command_phase_index(candidate.key(), command, intent).ok_or_else(|| {
                CommandError::Store {
                    message: format!(
                        "candidate key does not match command phase pattern: {}",
                        candidate.key()
                    ),
                }
            })?;
            let payload = payloads
                .get(candidate.content_hash())
                .cloned()
                .ok_or_else(|| CommandError::Store {
                    message: format!("command phase payload missing for {}", candidate.key()),
                })?;
            Ok(StoredCommandPhase {
                key: candidate.key().clone(),
                index,
                payload,
            })
        })
        .collect()
}

fn command_phase_index(key: &FactKey, command: &CommandName, intent: &IntentId) -> Option<u64> {
    let segments = key.segments().collect::<Vec<_>>();
    let [
        "facts",
        "command",
        command_segment,
        intent_segment,
        "phase",
        index,
    ] = segments.as_slice()
    else {
        return None;
    };
    if *command_segment != command.as_str() || *intent_segment != intent.as_str() {
        return None;
    }
    index.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn p2panda_command_phase_conflict_blocks_history_read() {
        let root = scenario_dir("environment-command-phase-conflict-test");
        reset_dir(&root).expect("reset test dir");
        let island = IslandId::new("prod");
        let author = Arc::new(PandaFactAuthor::new(PrincipalId::new(
            "environment-operator",
        )));
        let (raw_bus, session) = p2panda_writer_bus(&island, author.principal());
        let fact_authorizer: Arc<dyn FactAuthorizer> = Arc::new(raw_bus);
        let store = SharedPandaFactStore::new(
            PandaFactStore::open_sqlite(
                Arc::clone(&fact_authorizer),
                PandaSqliteOpenConfig::new(root.join("facts.sqlite"), vec![island.clone()])
                    .with_trusted_author_key(PandaTrustedAuthorKey::new(
                        island,
                        author.principal().clone(),
                        author.author_key(),
                    )),
            )
            .await
            .expect("open p2panda store"),
        );
        let command = CommandName::parse("environment.promote").expect("command name");
        let intent = IntentId::parse("promote-conflict").expect("intent id");
        let key = command_phase_fact_key(&command, &intent, 1).expect("phase key");
        store
            .write_fact_payload(
                &session,
                &author,
                key.clone(),
                FactPayload::from(br#""DecisionWritten""#.to_vec()),
            )
            .await
            .expect("write first phase candidate");
        store
            .write_fact_payload(
                &session,
                &author,
                key.clone(),
                FactPayload::from(br#""ServingCommitWritten""#.to_vec()),
            )
            .await
            .expect("write conflicting phase candidate");
        let phase_store = PandaCommandPhaseStore::new(store, session, author);

        let error = phase_store
            .read_phase_history(&command, &intent)
            .expect_err("conflict blocks phase resume");

        assert!(
            matches!(error, CommandError::PhaseConflict { key: error_key } if error_key == key)
        );
    }
}

async fn rebuild_projection(socket: &std::path::Path) -> Result<(), String> {
    let token = request_begin_rebuild(socket).await?;
    request_await_rebuild(socket, token).await?;
    Ok(())
}

fn environment(value: &str) -> Result<EnvironmentId, String> {
    EnvironmentId::parse(value).map_err(|error| error.to_string())
}

fn branch_id(value: &str) -> Result<EnvironmentBranchId, String> {
    EnvironmentBranchId::parse(value).map_err(|error| error.to_string())
}

fn command_id(value: &str) -> Result<EnvironmentCommandId, String> {
    EnvironmentCommandId::parse(value).map_err(|error| error.to_string())
}

fn head_id(value: &str) -> Result<EnvironmentHeadId, String> {
    EnvironmentHeadId::parse(value).map_err(|error| error.to_string())
}

fn epoch(value: u64) -> Result<EnvironmentEpoch, String> {
    EnvironmentEpoch::new(value).map_err(|error| error.to_string())
}

fn volume(value: &str) -> Result<EnvironmentVolumeRef, String> {
    EnvironmentVolumeRef::parse(value).map_err(|error| error.to_string())
}

fn single_volume_ref(label: &str, refs: &[EnvironmentVolumeRef]) -> Result<String, String> {
    let [volume_ref] = refs else {
        return Err(format!(
            "{label} expected exactly one volume ref, got {}",
            refs.len()
        ));
    };
    Ok(volume_ref.as_str().to_string())
}

fn visible_nodes() -> VisibleNodes {
    VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")])
}

fn serving_plan(id: &str, backend: &str, dns: &str, epoch: u64) -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(id),
        route_commit_id: RouteCommitId::new(format!("{id}-route")),
        gateway_commit_id: GatewayCommitId::new(format!("{id}-gateway")),
        dns_commit_id: DnsCommitId::new(format!("{id}-dns")),
        route_id: RouteId::new("web-http"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-web"),
            address: backend.to_string(),
        }],
        old_backends_to_drain: Vec::new(),
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: dns.to_string(),
            ttl_seconds: 30,
        }],
        epoch,
    }
}

fn catch_up(island: &IslandId, plan: &ServingCommitPlan) -> ProjectionCatchUp {
    let mut state = ProjectionState::for_island(island.clone());
    state.gateway = Some(GatewayProjection {
        gateway_commit_id: plan.gateway_commit_id.to_string(),
        route_commit_id: plan.route_commit_id.to_string(),
        routes: vec![GatewayRouteProjection {
            route_id: plan.route_id.clone(),
            hostnames: plan.hostnames.clone(),
            backends: plan.active_backends.clone(),
            old_backends_to_drain: plan.old_backends_to_drain.clone(),
        }],
    });
    state.dns = Some(DnsProjection {
        dns_commit_id: plan.dns_commit_id.to_string(),
        records: plan.dns_records.iter().cloned().map(Into::into).collect(),
    });
    ProjectionCatchUp::from_report(
        plan,
        &ProjectionReport {
            state,
            sqlite_path: PathBuf::from("projection.sqlite"),
            gateway_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("gateway.snapshot"),
                bytes_written: 1,
                revision: format!(
                    "gateway:{}:{}:acme:none",
                    plan.gateway_commit_id, plan.route_commit_id
                ),
            }),
            dns_snapshot: Some(SnapshotWriteReport {
                path: PathBuf::from("dns.snapshot"),
                bytes_written: 1,
                revision: format!("dns:{}", plan.dns_commit_id),
            }),
            duration: Duration::from_millis(1),
        },
    )
    .expect("projection catch-up")
}
