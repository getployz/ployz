use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{BusActorHandle, BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_deploy::{
    CapacityReply, CleanupStatus, DeployCoordinator, DeployId, DeployManifest, DeployOutcome,
    DeployTimeouts, DnsCommitId, DrainStatus, GatewayCommitId, InstanceCapacityRequirement,
    InstanceCommandReply, InstanceCommandRequest, InstanceId, InstancePlan, InstanceStartOutcome,
    PhaseId, PhasePlan, PhasePolicy, PhaseReversibility, ProjectionCatchUp, RevisionId,
    RouteCommitId, ServingCommitId, ServingCommitPlan,
};
use mvp_identity::NodeId;
use mvp_projection::{
    BackendEndpoint, BusFactSource, DnsRecordFact, ProjectionIgnoreReason, RouteId, ServiceName,
    load_dns_snapshot, load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{pattern, subject};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct DeployCommitDrainReport {
    scenario: &'static str,
    scheduler_queue_deliveries: usize,
    visible_nodes_at_decision: usize,
    phase_1_ms: u128,
    phase_2_ms: u128,
    local_route_commit_ms: u128,
    route_commit_to_projection_ms: u128,
    drain_requests: usize,
    stop_requests: usize,
    old_backend_alive_during_projection: bool,
    cleanup_pending_count: usize,
    superseded_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for deploy contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("deploy-commit-drain-contract");
    reset_dir(&root)?;

    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("operator"),
        Grant::allow_all(),
    );
    let scheduler_a = authority.grant_in(
        prod.clone(),
        PrincipalId::new("scheduler-a"),
        Grant::allow_all(),
    );
    let scheduler_b = authority.grant_in(
        prod.clone(),
        PrincipalId::new("scheduler-b"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(prod.clone(), PrincipalId::new("node"), Grant::allow_all());
    let projection = authority.grant_in(prod, PrincipalId::new("projection"), Grant::allow_all());

    let scheduler_queue_deliveries =
        prove_scheduler_queue(&bus, &operator, &scheduler_a, &scheduler_b).await?;
    let participant_state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &participant_state, "node-db", true).await?;
    register_capacity(&bus, &node, &participant_state, "node-web", false).await?;
    register_capacity(&bus, &node, &participant_state, "node-queue", false).await?;
    register_instance_participants(&bus, &node, &participant_state).await?;
    register_cleanup_participants(&bus, &node, &participant_state, true).await?;

    let coordinator = DeployCoordinator::new(bus.clone(), operator.clone(), test_timeouts());
    let manifest = deploy_manifest(
        "deploy-main",
        "route-commit-main",
        "gateway-main",
        "dns-main",
        1,
    );

    let phase_1_started = Instant::now();
    let pending = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .map_err(|error| format!("execute deploy until serving commit: {error}"))?;
    let phase_1_ms = phase_1_started.elapsed().as_millis();
    let local_route_commit_ms = phase_1_ms;

    assert_eq_named(
        "old drain before projection",
        participant_state.drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "old stop before projection",
        participant_state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;
    let old_backend_alive_during_projection =
        participant_state.drain_requests.load(Ordering::SeqCst) == 0
            && participant_state.stop_requests.load(Ordering::SeqCst) == 0;
    let projection_started = Instant::now();
    let projection_actor = projection_actor(
        Arc::new(BusFactSource::new(raw_bus.clone())),
        projection.clone(),
        &root,
    )?;
    let projected = projection_actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project deploy facts: {error}"))?;
    let route_commit_to_projection_ms = projection_started.elapsed().as_millis();
    participant_state
        .record(DeployEvent::ProjectionCaughtUp)
        .map_err(|error| format!("record projection event: {error}"))?;
    let gateway = projected
        .state
        .gateway
        .as_ref()
        .ok_or_else(|| "gateway projection missing after serving commit".to_string())?;
    let [route] = gateway.routes.as_slice() else {
        return Err("gateway projection should contain exactly one route".to_string());
    };
    assert_eq_named("active backend count", route.backends.len(), 1)?;
    assert_eq_named(
        "old backend drain metadata count",
        route.old_backends_to_drain.len(),
        1,
    )?;
    assert_eq_named(
        "loaded gateway snapshot routes",
        load_gateway_snapshot(root.join("gateway.snapshot"), &IslandId::new("prod"))
            .map_err(|error| format!("load gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    let dns = projected
        .state
        .dns
        .as_ref()
        .ok_or_else(|| "dns projection missing after serving commit".to_string())?;
    assert_eq_named(
        "projected dns commit",
        dns.dns_commit_id.as_str(),
        "dns-main",
    )?;
    let [dns_record] = dns.records.as_slice() else {
        return Err("dns projection should contain exactly one record".to_string());
    };
    assert_eq_named("projected dns value", dns_record.value.as_str(), "fd00::2")?;
    assert!(projected.dns_snapshot.is_some());
    let loaded_dns = load_dns_snapshot(root.join("dns.snapshot"), &IslandId::new("prod"))
        .map_err(|error| format!("load dns snapshot: {error}"))?;
    assert_eq_named(
        "loaded dns snapshot commit",
        loaded_dns.dns_commit_id,
        "dns-main".to_string(),
    )?;
    assert_eq_named("loaded dns snapshot records", loaded_dns.records.len(), 1)?;
    let mut mismatched_projection_commit = manifest.serving_commit.clone();
    mismatched_projection_commit.active_backends.clear();
    if !matches!(
        ProjectionCatchUp::from_report(&mismatched_projection_commit, &projected),
        Err(mvp_deploy::RoutingError::ProjectionCatchUpMismatch { .. })
    ) {
        return Err("projection proof accepted mismatched active backend content".to_string());
    }

    let pending = pending
        .after_projection(
            ProjectionCatchUp::from_report(&manifest.serving_commit, &projected)
                .map_err(|error| format!("build projection catch-up proof: {error}"))?,
        )
        .map_err(|error| format!("accept projection catch-up proof: {error}"))?;
    let phase_2_started = Instant::now();
    let result = coordinator
        .finish_cleanup(pending)
        .await
        .map_err(|error| format!("finish deploy cleanup: {error}"))?;
    let phase_2_ms = phase_2_started.elapsed().as_millis();
    assert_eq_named(
        "deploy outcome",
        result.outcome.clone(),
        DeployOutcome::DeployDone,
    )?;
    assert_eq_named(
        "drain status",
        result.drain_status.clone(),
        DrainStatus::Completed,
    )?;
    assert_eq_named("visible nodes", result.visible_nodes.len(), 3)?;
    assert_visible_nodes(
        &result,
        &[
            NodeId::new("node-db"),
            NodeId::new("node-queue"),
            NodeId::new("node-web"),
        ],
    )?;
    assert_eq_named(
        "drain requests",
        participant_state.drain_requests.load(Ordering::SeqCst),
        1,
    )?;
    assert_eq_named(
        "stop requests",
        participant_state.stop_requests.load(Ordering::SeqCst),
        1,
    )?;
    participant_state.assert_main_order()?;

    let cleanup_pending_count = cleanup_pending_variant().await?;
    irreversible_failure_variant().await?;
    serving_conflict_after_irreversible_variant().await?;
    forged_capacity_variant().await?;
    insufficient_capacity_variant().await?;
    let superseded_count = supersession_variant(&bus, &raw_bus, &projection, &root).await?;

    let report = DeployCommitDrainReport {
        scenario: "deploy-commit-drain-contract",
        scheduler_queue_deliveries,
        visible_nodes_at_decision: result.visible_nodes.len(),
        phase_1_ms,
        phase_2_ms,
        local_route_commit_ms,
        route_commit_to_projection_ms,
        drain_requests: participant_state.drain_requests.load(Ordering::SeqCst),
        stop_requests: participant_state.stop_requests.load(Ordering::SeqCst),
        old_backend_alive_during_projection,
        cleanup_pending_count,
        superseded_count,
        elapsed_ms: started.elapsed().as_millis(),
    };
    if !report.old_backend_alive_during_projection {
        return Err("old backend was stopped before projection catch-up".to_string());
    }
    let json = write_json(
        &root.join("deploy-commit-drain-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS deploy-commit-drain-contract");
    Ok(())
}

async fn prove_scheduler_queue(
    bus: &BusActorHandle,
    operator: &BusSession,
    scheduler_a: &BusSession,
    scheduler_b: &BusSession,
) -> Result<usize, String> {
    let deliveries = Arc::new(AtomicUsize::new(0));
    register_scheduler(bus, scheduler_a, Arc::clone(&deliveries)).await?;
    register_scheduler(bus, scheduler_b, Arc::clone(&deliveries)).await?;
    let response = bus
        .request(
            operator,
            subject("deploy.submit")?,
            b"manifest".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .map_err(|error| format!("request deploy.submit: {error}"))?;
    assert_eq_named(
        "deploy submit reply",
        response.payload().as_bytes(),
        b"accepted".as_slice(),
    )?;
    assert_eq_named(
        "scheduler queue deliveries",
        deliveries.load(Ordering::SeqCst),
        1,
    )?;
    Ok(deliveries.load(Ordering::SeqCst))
}

fn assert_visible_nodes(
    result: &mvp_deploy::DeployCommandResult,
    expected: &[NodeId],
) -> Result<(), String> {
    let actual = result.visible_nodes.iter().cloned().collect::<Vec<_>>();
    assert_eq_named("visible node ids", actual, expected.to_vec())
}

async fn register_scheduler(
    bus: &BusActorHandle,
    session: &BusSession,
    deliveries: Arc<AtomicUsize>,
) -> Result<(), String> {
    bus.queue_subscribe(
        session,
        pattern("deploy.submit")?,
        "schedulers",
        move |ctx| {
            deliveries.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"accepted".to_vec())
        },
    )
    .await
    .map_err(|error| format!("register scheduler: {error}"))?;
    Ok(())
}

async fn register_capacity(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    node_id: &'static str,
    can_run_database: bool,
) -> Result<(), String> {
    let state_for_capacity = Arc::clone(state);
    bus.subscribe(
        session,
        pattern(&format!("node.{node_id}.capacity"))?,
        move |ctx| {
            state_for_capacity.record(DeployEvent::Capacity(node_id))?;
            let reply = CapacityReply {
                node_id: NodeId::new(node_id),
                memory_free_bytes: 1024 * 1024 * 1024,
                can_run_database,
            };
            ctx.reply(
                serde_json::to_vec(&reply).map_err(|_| mvp_bus::BusError::HandlerFailed {
                    subject: ctx.message.subject().to_string(),
                    failure: mvp_bus::HandlerFailure::Application,
                })?,
            )
        },
    )
    .await
    .map_err(|error| format!("register capacity {node_id}: {error}"))?;
    Ok(())
}

async fn register_forged_capacity(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    let state_for_capacity = Arc::clone(state);
    bus.subscribe(session, pattern("node.node-db.capacity")?, move |ctx| {
        state_for_capacity.record(DeployEvent::Capacity("node-db"))?;
        let reply = CapacityReply {
            node_id: NodeId::new("node-web"),
            memory_free_bytes: 1024 * 1024 * 1024,
            can_run_database: true,
        };
        ctx.reply(
            serde_json::to_vec(&reply).map_err(|_| mvp_bus::BusError::HandlerFailed {
                subject: ctx.message.subject().to_string(),
                failure: mvp_bus::HandlerFailure::Application,
            })?,
        )
    })
    .await
    .map_err(|error| format!("register forged capacity: {error}"))?;
    Ok(())
}

#[derive(Default)]
struct ParticipantState {
    prepare_requests: AtomicUsize,
    start_requests: AtomicUsize,
    drain_requests: AtomicUsize,
    stop_requests: AtomicUsize,
    events: Mutex<Vec<DeployEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployEvent {
    Capacity(&'static str),
    Prepare(&'static str),
    Start(&'static str),
    ProjectionCaughtUp,
    Drain(&'static str),
    Stop(&'static str),
}

impl ParticipantState {
    fn record(&self, event: DeployEvent) -> Result<(), mvp_bus::BusError> {
        self.events
            .lock()
            .map_err(|_| mvp_bus::BusError::HandlerFailed {
                subject: "deploy event log".to_string(),
                failure: mvp_bus::HandlerFailure::Application,
            })?
            .push(event);
        Ok(())
    }

    fn assert_main_order(&self) -> Result<(), String> {
        let events = self
            .events
            .lock()
            .map_err(|_| "deploy event log mutex poisoned".to_string())?
            .clone();
        let first_prepare = event_index(&events, |event| matches!(event, DeployEvent::Prepare(_)))?;
        let first_start = event_index(&events, |event| matches!(event, DeployEvent::Start(_)))?;
        let projection = event_index(&events, |event| *event == DeployEvent::ProjectionCaughtUp)?;
        let drain = event_index(&events, |event| matches!(event, DeployEvent::Drain(_)))?;
        let stop = event_index(&events, |event| matches!(event, DeployEvent::Stop(_)))?;
        for node_id in ["node-db", "node-web", "node-queue"] {
            let capacity = event_index(&events, |event| *event == DeployEvent::Capacity(node_id))?;
            if capacity > first_prepare || capacity > first_start {
                return Err(format!("capacity for {node_id} happened after mutation"));
            }
        }
        if projection > drain || projection > stop {
            return Err("cleanup started before projection catch-up".to_string());
        }
        if drain > stop {
            return Err("stop happened before drain".to_string());
        }
        Ok(())
    }
}

fn event_index(
    events: &[DeployEvent],
    matches: impl Fn(&DeployEvent) -> bool,
) -> Result<usize, String> {
    events
        .iter()
        .position(matches)
        .ok_or_else(|| format!("missing deploy event in {events:?}"))
}

async fn register_instance_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
) -> Result<(), String> {
    register_instance_participants_with_not_ready(bus, session, state, None).await
}

async fn register_instance_participants_with_not_ready(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    not_ready_node: Option<&'static str>,
) -> Result<(), String> {
    for node_id in ["node-db", "node-web", "node-queue"] {
        let state_for_prepare = Arc::clone(state);
        bus.subscribe(
            session,
            pattern(&format!("node.{node_id}.rpc.prepare_instance"))?,
            move |ctx| {
                state_for_prepare.record(DeployEvent::Prepare(node_id))?;
                state_for_prepare
                    .prepare_requests
                    .fetch_add(1, Ordering::SeqCst);
                ctx.reply(b"prepared".to_vec())
            },
        )
        .await
        .map_err(|error| format!("register prepare {node_id}: {error}"))?;
        let state_for_start = Arc::clone(state);
        bus.subscribe(
            session,
            pattern(&format!("node.{node_id}.rpc.start_instance"))?,
            move |ctx| {
                state_for_start.record(DeployEvent::Start(node_id))?;
                state_for_start
                    .start_requests
                    .fetch_add(1, Ordering::SeqCst);
                let request: InstanceCommandRequest =
                    serde_json::from_slice(ctx.message.payload().as_bytes()).map_err(|_| {
                        mvp_bus::BusError::HandlerFailed {
                            subject: ctx.message.subject().to_string(),
                            failure: mvp_bus::HandlerFailure::Application,
                        }
                    })?;
                let reply = InstanceCommandReply {
                    instance_id: request.instance_id,
                    outcome: if Some(node_id) == not_ready_node {
                        InstanceStartOutcome::NotReady {
                            reason: mvp_deploy::InstanceNotReadyReason::ReadinessCheckFailed,
                        }
                    } else {
                        InstanceStartOutcome::Ready
                    },
                    backend: None,
                };
                ctx.reply(serde_json::to_vec(&reply).map_err(|_| {
                    mvp_bus::BusError::HandlerFailed {
                        subject: ctx.message.subject().to_string(),
                        failure: mvp_bus::HandlerFailure::Application,
                    }
                })?)
            },
        )
        .await
        .map_err(|error| format!("register start {node_id}: {error}"))?;
    }
    Ok(())
}

async fn register_cleanup_participants(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &Arc<ParticipantState>,
    register_stop: bool,
) -> Result<(), String> {
    let state_for_drain = Arc::clone(state);
    bus.subscribe(
        session,
        pattern("node.node-old.rpc.drain_instance")?,
        move |ctx| {
            state_for_drain.record(DeployEvent::Drain("node-old"))?;
            state_for_drain
                .drain_requests
                .fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"draining".to_vec())
        },
    )
    .await
    .map_err(|error| format!("register old drain: {error}"))?;
    if register_stop {
        let state_for_stop = Arc::clone(state);
        bus.subscribe(
            session,
            pattern("node.node-old.rpc.stop_instance")?,
            move |ctx| {
                state_for_stop.record(DeployEvent::Stop("node-old"))?;
                state_for_stop.stop_requests.fetch_add(1, Ordering::SeqCst);
                ctx.reply(b"stopped".to_vec())
            },
        )
        .await
        .map_err(|error| format!("register old stop: {error}"))?;
    }
    Ok(())
}

fn deploy_manifest(
    deploy_id: &str,
    route_commit_id: &str,
    gateway_commit_id: &str,
    dns_commit_id: &str,
    epoch: u64,
) -> DeployManifest {
    DeployManifest::new(
        DeployId::new(deploy_id),
        vec![
            PhasePlan::new(
                PhaseId::new(1),
                vec![InstancePlan::new(
                    InstanceId::new("db-1"),
                    NodeId::new("node-db"),
                    ServiceName::new("db"),
                    RevisionId::new("rev-db"),
                    InstanceCapacityRequirement::Database,
                )],
                PhasePolicy::irreversible(),
            ),
            PhasePlan::new(
                PhaseId::new(2),
                vec![
                    InstancePlan::new(
                        InstanceId::new("web-1"),
                        NodeId::new("node-web"),
                        ServiceName::new("web"),
                        RevisionId::new("rev-web"),
                        InstanceCapacityRequirement::General,
                    ),
                    InstancePlan::new(
                        InstanceId::new("queue-1"),
                        NodeId::new("node-queue"),
                        ServiceName::new("queue"),
                        RevisionId::new("rev-queue"),
                        InstanceCapacityRequirement::General,
                    ),
                ],
                PhasePolicy::serving(PhaseReversibility::Reversible),
            ),
        ],
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new(route_commit_id),
            route_commit_id: RouteCommitId::new(route_commit_id),
            gateway_commit_id: GatewayCommitId::new(gateway_commit_id),
            dns_commit_id: DnsCommitId::new(dns_commit_id),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-web"),
                address: "fd00::2:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "fd00::1:8080".to_string(),
            }],
            dns_records: vec![DnsRecordFact {
                name: "web.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::2".to_string(),
                ttl_seconds: 30,
            }],
            epoch,
        },
    )
}

async fn cleanup_pending_variant() -> Result<usize, String> {
    let root = scenario_dir("deploy-commit-drain-cleanup-pending");
    reset_dir(&root)?;
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("cleanup-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(prod, PrincipalId::new("cleanup-node"), Grant::allow_all());
    let projection = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("cleanup-projection"),
        Grant::allow_all(),
    );
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-db", true).await?;
    register_capacity(&bus, &node, &state, "node-web", false).await?;
    register_capacity(&bus, &node, &state, "node-queue", false).await?;
    register_instance_participants(&bus, &node, &state).await?;
    register_cleanup_participants(&bus, &node, &state, false).await?;
    let manifest = deploy_manifest(
        "deploy-cleanup",
        "route-commit-cleanup",
        "gateway-cleanup",
        "dns-cleanup",
        2,
    );
    let coordinator = DeployCoordinator::new(bus.clone(), operator, test_timeouts());
    let pending = coordinator
        .execute_until_serving_commit(manifest.clone())
        .await
        .map_err(|error| format!("cleanup pending deploy commit: {error}"))?;
    let projection_actor = projection_actor(
        Arc::new(BusFactSource::new(raw_bus)),
        projection.clone(),
        &root,
    )?;
    let projected = projection_actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project cleanup-pending commit: {error}"))?;
    assert_eq_named(
        "cleanup pending projected gateway routes",
        projected
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        1,
    )?;
    let pending = pending
        .after_projection(
            ProjectionCatchUp::from_report(&manifest.serving_commit, &projected)
                .map_err(|error| format!("build cleanup projection proof: {error}"))?,
        )
        .map_err(|error| format!("accept cleanup projection proof: {error}"))?;
    let result = coordinator
        .finish_cleanup(pending)
        .await
        .map_err(|error| format!("cleanup pending deploy: {error}"))?;
    assert_eq_named(
        "cleanup pending outcome",
        result.outcome,
        DeployOutcome::CleanupPending,
    )?;
    assert_eq_named(
        "cleanup pending serving commit",
        result.serving_commit_id.as_ref().map(ToString::to_string),
        Some("route-commit-cleanup".to_string()),
    )?;
    assert_eq_named(
        "cleanup pending status",
        result.cleanup_status,
        CleanupStatus::Pending {
            reason: mvp_deploy::CleanupPendingReason::StopUnavailable {
                node_id: NodeId::new("node-old"),
                cause: mvp_deploy::CleanupFailureKind::NoResponders,
            },
        },
    )?;
    assert_eq_named(
        "cleanup pending drain requests",
        state.drain_requests.load(Ordering::SeqCst),
        1,
    )?;
    assert_eq_named(
        "cleanup pending stop requests",
        state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;
    Ok(1)
}

async fn irreversible_failure_variant() -> Result<(), String> {
    let root = scenario_dir("deploy-commit-drain-irreversible-block");
    reset_dir(&root)?;
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("blocked-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        prod.clone(),
        PrincipalId::new("blocked-node"),
        Grant::allow_all(),
    );
    let projection = authority.grant_in(
        prod,
        PrincipalId::new("blocked-projection"),
        Grant::allow_all(),
    );
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-db", true).await?;
    register_capacity(&bus, &node, &state, "node-web", false).await?;
    register_capacity(&bus, &node, &state, "node-queue", false).await?;
    register_instance_participants_with_not_ready(&bus, &node, &state, Some("node-web")).await?;
    register_cleanup_participants(&bus, &node, &state, true).await?;

    let coordinator = DeployCoordinator::new(bus, operator, test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(deploy_manifest(
            "deploy-blocked",
            "route-commit-blocked",
            "gateway-blocked",
            "dns-blocked",
            3,
        ))
        .await
        .expect_err("phase 2 not-ready after irreversible phase should block");
    if !matches!(
        error,
        mvp_deploy::DeployError::BlockedAfterIrreversiblePhase
    ) {
        return Err(format!("unexpected irreversible failure error: {error}"));
    }
    assert_eq_named(
        "blocked drain requests",
        state.drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "blocked stop requests",
        state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;

    let projection_actor =
        projection_actor(Arc::new(BusFactSource::new(raw_bus)), projection, &root)?;
    let projected = projection_actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project irreversible block: {error}"))?;
    if projected.state.gateway.is_some() || projected.state.dns.is_some() {
        return Err("blocked deploy wrote serving projection facts".to_string());
    }
    Ok(())
}

async fn serving_conflict_after_irreversible_variant() -> Result<(), String> {
    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("conflict-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(prod, PrincipalId::new("conflict-node"), Grant::allow_all());
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-db", true).await?;
    register_capacity(&bus, &node, &state, "node-web", false).await?;
    register_capacity(&bus, &node, &state, "node-queue", false).await?;
    register_instance_participants(&bus, &node, &state).await?;

    let manifest = deploy_manifest(
        "deploy-conflict",
        "route-commit-conflict",
        "gateway-conflict",
        "dns-conflict",
        4,
    );
    let mut conflicting = manifest.clone();
    conflicting.serving_commit.hostnames = vec!["other.example.test".to_string()];
    mvp_routing::write_serving_commit(&bus, &operator, &conflicting.serving_commit)
        .await
        .map_err(|error| format!("preseed conflicting serving commit: {error}"))?;

    let coordinator = DeployCoordinator::new(bus, operator, test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(manifest)
        .await
        .expect_err("serving conflict after irreversible phase should block");
    if !matches!(
        error,
        mvp_deploy::DeployError::BlockedAfterIrreversiblePhase
    ) {
        return Err(format!(
            "unexpected serving conflict after irreversible error: {error}"
        ));
    }
    assert_eq_named(
        "conflict drain requests",
        state.drain_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "conflict stop requests",
        state.stop_requests.load(Ordering::SeqCst),
        0,
    )?;

    let source = Arc::new(BusFactSource::new(raw_bus));
    let actor = projection_actor(source, node, &scenario_dir("deploy-commit-conflict"))?;
    let report = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project conflict variant: {error}"))?;
    assert_eq_named(
        "conflict superseded count",
        report
            .state
            .statuses
            .iter()
            .filter(|status| status.reason == ProjectionIgnoreReason::Superseded)
            .map(|status| status.count)
            .sum::<usize>(),
        0,
    )?;
    Ok(())
}

async fn forged_capacity_variant() -> Result<(), String> {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("capacity-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(prod, PrincipalId::new("capacity-node"), Grant::allow_all());
    let state = Arc::new(ParticipantState::default());
    register_forged_capacity(&bus, &node, &state).await?;
    register_instance_participants(&bus, &node, &state).await?;

    let coordinator = DeployCoordinator::new(bus, operator, test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(deploy_manifest(
            "deploy-forged-capacity",
            "route-commit-forged",
            "gateway-forged",
            "dns-forged",
            5,
        ))
        .await
        .expect_err("forged capacity payload should fail before mutation");
    if !matches!(
        error,
        mvp_deploy::DeployError::CapacityReplyNodeMismatch { .. }
    ) {
        return Err(format!("unexpected forged capacity error: {error}"));
    }
    assert_eq_named(
        "forged capacity prepare requests",
        state.prepare_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "forged capacity start requests",
        state.start_requests.load(Ordering::SeqCst),
        0,
    )?;
    Ok(())
}

async fn insufficient_capacity_variant() -> Result<(), String> {
    let (bus, authority, _raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let operator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("capacity-reject-operator"),
        Grant::allow_all(),
    );
    let node = authority.grant_in(
        prod,
        PrincipalId::new("capacity-reject-node"),
        Grant::allow_all(),
    );
    let state = Arc::new(ParticipantState::default());
    register_capacity(&bus, &node, &state, "node-db", false).await?;
    register_capacity(&bus, &node, &state, "node-web", false).await?;
    register_capacity(&bus, &node, &state, "node-queue", false).await?;
    register_instance_participants(&bus, &node, &state).await?;

    let coordinator = DeployCoordinator::new(bus, operator, test_timeouts());
    let error = coordinator
        .execute_until_serving_commit(deploy_manifest(
            "deploy-insufficient-capacity",
            "route-commit-insufficient-capacity",
            "gateway-insufficient-capacity",
            "dns-insufficient-capacity",
            6,
        ))
        .await
        .expect_err("database capacity failure should fail before mutation");
    if !matches!(
        error,
        mvp_deploy::DeployError::InsufficientCapacity {
            reason: mvp_deploy::CapacityRejectionReason::DatabaseUnsupported,
            ..
        }
    ) {
        return Err(format!("unexpected insufficient capacity error: {error}"));
    }
    assert_eq_named(
        "insufficient capacity prepare requests",
        state.prepare_requests.load(Ordering::SeqCst),
        0,
    )?;
    assert_eq_named(
        "insufficient capacity start requests",
        state.start_requests.load(Ordering::SeqCst),
        0,
    )?;
    Ok(())
}

async fn supersession_variant(
    bus: &BusActorHandle,
    raw_bus: &InMemoryBus,
    projection: &BusSession,
    root: &std::path::Path,
) -> Result<usize, String> {
    let manifest = deploy_manifest(
        "deploy-race",
        "route-commit-race",
        "gateway-race",
        "dns-race",
        1,
    );
    mvp_routing::write_serving_commit(bus, projection, &manifest.serving_commit)
        .await
        .map_err(|error| format!("write superseded serving commit: {error}"))?;
    let source = Arc::new(BusFactSource::new(raw_bus.clone()));
    let actor = projection_actor(source, projection.clone(), &root.join("supersession"))?;
    let report = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project superseded race: {error}"))?;
    Ok(report
        .state
        .statuses
        .iter()
        .filter(|status| status.reason == ProjectionIgnoreReason::Superseded)
        .map(|status| status.count)
        .sum())
}

fn test_timeouts() -> DeployTimeouts {
    DeployTimeouts {
        capacity: Duration::from_secs(1),
        participant: Duration::from_secs(1),
    }
}
