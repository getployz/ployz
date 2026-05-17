use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mvp_bus::{
    BridgeEndpoint, BridgeRuleId, BusRuntimeConfig, BusRuntimeSnapshot, Grant, IslandId, Payload,
    PrincipalId, RequestManyPolicy, RequestTarget, ServiceImport, StreamImport, Subject,
    SubjectTransform, harness::InMemoryBus,
};
use mvp_projection::{
    BackendEndpoint, BusFactSource, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeId,
    NodeJoinedFact, ProjectionActorHandle, ProjectionFactPayload, RouteCommitFact, RouteId,
    ServiceName, ServiceRegistrationFact, SqliteProjectionStore,
};
use serde::Serialize;

use crate::bus_syntax::{fact_pattern, pattern, subject, write_projection_fact};
use crate::metrics::{
    LatencyRecorder, LatencySummary, MemorySnapshot, memory_snapshot, reset_dir, scenario_dir,
    write_json,
};

const NODE_COUNTS: [usize; 3] = [200, 1_000, 10_000];
const PUBLISH_ITERATIONS: usize = 100;
const REQUEST_MANY_ITERATIONS: usize = 100;
const DELIVERY_WORKERS: usize = 64;
const SATURATION_DELIVERY_WORKERS: usize = 4;
const SATURATION_QUEUE_CAPACITY: usize = 8;
const SATURATION_SUBSCRIBERS: usize = 96;
const SATURATION_PUBLISHERS: usize = 4;
const SATURATION_HANDLER_SLEEP: Duration = Duration::from_millis(15);
const MULTI_ISLAND_SUBSCRIBERS: usize = 1_000;
const QUEUE_GROUP_SUBSCRIBERS: usize = 10_000;
const QUEUE_GROUP_REQUESTS: usize = 100;
const BRIDGE_STREAM_ITERATIONS: usize = 20;
const BRIDGE_SERVICE_RESPONDERS: usize = 100;
const BRIDGE_SERVICE_REQUESTS: usize = 100;
const BRIDGE_RULE_VOLUME_IMPORTS: usize = 10_000;

#[derive(Debug, Serialize)]
struct ScaleReport {
    scenario: &'static str,
    node_counts: Vec<ScaleRunReport>,
    multi_island: MultiIslandReport,
    bridge: BridgeScaleReport,
    projection: ProjectionScaleReport,
    queue_group: QueueGroupReport,
    saturation: SaturationReport,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct ScaleRunReport {
    logical_nodes: usize,
    subscribers: usize,
    responders: usize,
    publish_iterations: usize,
    request_many_iterations: usize,
    publish_payload_bytes: usize,
    request_payload_bytes: usize,
    delivered_payload_bytes: usize,
    expected_publish_deliveries: usize,
    observed_publish_deliveries: usize,
    expected_replies: usize,
    observed_replies: usize,
    publish_latency: LatencySummary,
    request_many_latency: LatencySummary,
    runtime: RuntimeReport,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct RuntimeReport {
    delivery_workers: usize,
    delivery_queue_capacity: usize,
    max_worker_concurrency: usize,
    max_queued_deliveries: usize,
    enqueue_full_count: usize,
    enqueue_blocked_ns: u64,
}

#[derive(Debug, Serialize)]
struct SaturationReport {
    publishers: usize,
    subscribers: usize,
    expected_deliveries: usize,
    observed_deliveries: usize,
    handler_sleep_ms: u128,
    minimum_expected_elapsed_ms: u128,
    observed_elapsed_ms: u128,
    bounded_backpressure_observed: bool,
    runtime: RuntimeReport,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
}

#[derive(Debug, Serialize)]
struct MultiIslandReport {
    subscribers: usize,
    island_a_subscribers: usize,
    island_b_subscribers: usize,
    island_a_deliveries: usize,
    island_b_deliveries: usize,
    island_a_replies: usize,
    island_b_request_calls: usize,
    cross_island_delivery_count: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct QueueGroupReport {
    subscribers: usize,
    requests: usize,
    expected_deliveries: usize,
    observed_deliveries: usize,
    unique_responders: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct BridgeScaleReport {
    stream_runs: Vec<BridgeStreamRunReport>,
    service: BridgeServiceReport,
    rule_volume: BridgeRuleVolumeReport,
}

#[derive(Debug, Serialize)]
struct BridgeStreamRunReport {
    logical_subscribers: usize,
    publish_iterations: usize,
    expected_deliveries: usize,
    observed_deliveries: usize,
    cross_island_leakage_count: usize,
    publish_latency: LatencySummary,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct BridgeServiceReport {
    responders: usize,
    requests: usize,
    expected_deliveries: usize,
    observed_deliveries: usize,
    unique_responders: usize,
    request_latency: LatencySummary,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct BridgeRuleVolumeReport {
    service_imports: usize,
    stream_imports: usize,
    service_delivery_count: usize,
    stream_delivery_count: usize,
    stream_leakage_count: usize,
    matching_stream_imports: usize,
    matching_stream_delivery_count: usize,
    matching_stream_leakage_count: usize,
    service_request_us: u128,
    stream_publish_us: u128,
    matching_stream_publish_us: u128,
    elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
struct ProjectionScaleReport {
    node_counts: Vec<ProjectionScaleRunReport>,
}

#[derive(Debug, Serialize)]
struct ProjectionScaleRunReport {
    logical_nodes: usize,
    fact_writes: usize,
    projected_nodes: usize,
    projected_services: usize,
    gateway_routes: usize,
    gateway_backends: usize,
    dns_records: usize,
    ignored_fact_count: usize,
    actor_duration_ms: u128,
    elapsed_ms: u128,
    deadline_seconds: u64,
    deadline_satisfied: bool,
    sqlite_bytes: u64,
    gateway_snapshot_bytes: u64,
    dns_snapshot_bytes: u64,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let mut node_counts = Vec::with_capacity(NODE_COUNTS.len());
    for logical_nodes in NODE_COUNTS {
        node_counts.push(run_node_count(logical_nodes)?);
    }

    let report = ScaleReport {
        scenario: "scale",
        node_counts,
        multi_island: run_multi_island_case()?,
        bridge: run_bridge_case()?,
        projection: run_projection_case()?,
        queue_group: run_queue_group_case()?,
        saturation: run_saturation_case()?,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = scenario_dir("scale").join("scale-metrics.json");
    let json = write_json(&path, &report)?;
    println!("{json}");
    eprintln!("PASS scale");
    Ok(())
}

fn run_node_count(logical_nodes: usize) -> Result<ScaleRunReport, String> {
    let started = Instant::now();
    let memory_before = memory_snapshot();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let admin = authority.grant(PrincipalId::new("admin"), Grant::allow_all());
    let gateway_wakeups = Arc::new(AtomicUsize::new(0));
    register_logical_nodes(&bus, &admin, logical_nodes, Arc::clone(&gateway_wakeups))?;

    let publish_payload = Payload::from_static(b"snapshot-ready");
    let request_payload = Payload::from_static(b"inspect-capacity");
    let mut publish_latency = LatencyRecorder::new()?;
    let mut request_many_latency = LatencyRecorder::new()?;

    for _ in 0..PUBLISH_ITERATIONS {
        let started = Instant::now();
        bus.publish(&admin, subject("gateway.changed")?, publish_payload.clone())
            .map_err(|error| {
                format!("publish gateway.changed to {logical_nodes} nodes: {error}")
            })?;
        publish_latency.record(started.elapsed())?;
    }

    let mut observed_replies = 0usize;
    let mut observed_reply_payload_bytes = 0usize;
    for _ in 0..REQUEST_MANY_ITERATIONS {
        let started = Instant::now();
        let replies = bus
            .request_many(
                &admin,
                RequestTarget::Pattern(pattern("node.*.capacity")?),
                subject("node.broadcast.capacity")?,
                request_payload.clone(),
                RequestManyPolicy::new(logical_nodes, Duration::from_secs(30)),
            )
            .map_err(|error| {
                format!("request_many capacity from {logical_nodes} logical nodes: {error}")
            })?;
        if replies.len() != logical_nodes {
            return Err(format!(
                "request_many capacity expected {logical_nodes} replies, got {}",
                replies.len()
            ));
        }
        let unique_replies = replies
            .iter()
            .map(|response| response.payload().as_bytes().to_vec())
            .collect::<BTreeSet<_>>();
        if unique_replies.len() != logical_nodes {
            return Err(format!(
                "request_many capacity expected {logical_nodes} unique replies, got {}",
                unique_replies.len()
            ));
        }
        observed_reply_payload_bytes += replies
            .iter()
            .map(|response| response.payload().len())
            .sum::<usize>();
        observed_replies += replies.len();
        request_many_latency.record(started.elapsed())?;
    }

    let expected_publish_deliveries = logical_nodes * PUBLISH_ITERATIONS;
    let observed_publish_deliveries = gateway_wakeups.load(Ordering::SeqCst);
    if observed_publish_deliveries != expected_publish_deliveries {
        return Err(format!(
            "publish fanout expected {expected_publish_deliveries} deliveries, got {observed_publish_deliveries}"
        ));
    }

    let runtime = runtime_report(bus.runtime_snapshot());
    let expected_replies = logical_nodes * REQUEST_MANY_ITERATIONS;
    let delivered_payload_bytes =
        expected_publish_deliveries * publish_payload.len() + observed_reply_payload_bytes;
    let report = ScaleRunReport {
        logical_nodes,
        subscribers: logical_nodes,
        responders: logical_nodes,
        publish_iterations: PUBLISH_ITERATIONS,
        request_many_iterations: REQUEST_MANY_ITERATIONS,
        publish_payload_bytes: publish_payload.len(),
        request_payload_bytes: request_payload.len(),
        delivered_payload_bytes,
        expected_publish_deliveries,
        observed_publish_deliveries,
        expected_replies,
        observed_replies,
        publish_latency: publish_latency.summary(),
        request_many_latency: request_many_latency.summary(),
        runtime,
        memory_before,
        memory_after: memory_snapshot(),
        elapsed_ms: started.elapsed().as_millis(),
    };
    if report.runtime.max_worker_concurrency > DELIVERY_WORKERS {
        return Err(format!(
            "runtime concurrency exceeded worker bound: {} > {DELIVERY_WORKERS}",
            report.runtime.max_worker_concurrency
        ));
    }
    Ok(report)
}

fn run_projection_case() -> Result<ProjectionScaleReport, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for projection scale: {error}"))?;
    let mut node_counts = Vec::with_capacity(NODE_COUNTS.len());
    for logical_nodes in NODE_COUNTS {
        node_counts.push(runtime.block_on(run_projection_node_count(logical_nodes))?);
    }
    Ok(ProjectionScaleReport { node_counts })
}

async fn run_projection_node_count(
    logical_nodes: usize,
) -> Result<ProjectionScaleRunReport, String> {
    const DEADLINE: Duration = Duration::from_secs(120);

    let started = Instant::now();
    let memory_before = memory_snapshot();
    let root = scenario_dir("scale-projection").join(logical_nodes.to_string());
    reset_dir(&root)?;

    let (bus, authority) = InMemoryBus::new_with_authority();
    let prod = IslandId::new("prod");
    let projection = authority.grant_in(
        prod.clone(),
        PrincipalId::new("projection"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/routes/>")?)
            .with_fact_write(fact_pattern("/facts/gateway/>")?)
            .with_fact_write(fact_pattern("/facts/dns/>")?)
            .with_fact_read(fact_pattern("/facts/>")?),
    );

    for node_index in 0..logical_nodes {
        let node_id = NodeId::new(format!("n{node_index}"));
        let node = authority.grant_in(
            prod.clone(),
            PrincipalId::new(format!("node-{node_index}")),
            Grant::empty()
                .with_fact_write(fact_pattern(&format!("/facts/node/n{node_index}/>"))?)
                .with_fact_write(fact_pattern(&format!(
                    "/facts/service/worker/n{node_index}/>"
                ))?),
        );
        write_projection_fact(
            &bus,
            &node,
            &format!("/facts/node/n{node_index}/joined/1"),
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: node_id.clone(),
                epoch: 1,
                overlay_ip: format!("fd00::{node_index:x}"),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            }),
        )?;
        write_projection_fact(
            &bus,
            &node,
            &format!("/facts/service/worker/n{node_index}/registered/1"),
            ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
                service: ServiceName::new("worker"),
                node_id,
                version: "1.0.0".to_string(),
                endpoint_subject: format!("node.n{node_index}.worker"),
                epoch: 1,
            }),
        )?;
    }

    let backends = (0..logical_nodes)
        .map(|node_index| BackendEndpoint {
            node_id: NodeId::new(format!("n{node_index}")),
            address: format!("10.0.{}.{}:8080", node_index / 256, node_index % 256),
        })
        .collect::<Vec<_>>();
    write_projection_fact(
        &bus,
        &projection,
        "/facts/routes/route-scale",
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-scale".to_string(),
            route_id: RouteId::new("worker-http"),
            hostnames: vec!["workers.example.com".to_string()],
            backends,
            old_backends_to_drain: Vec::new(),
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/gateway/gateway-scale",
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-scale".to_string(),
            route_commit_id: "route-scale".to_string(),
            epoch: 1,
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/dns/dns-scale",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-scale".to_string(),
            epoch: 1,
            records: vec![DnsRecordFact {
                name: "workers.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
        }),
    )?;

    let actor = ProjectionActorHandle::spawn(
        Arc::new(BusFactSource::new(bus)),
        prod,
        projection,
        fact_pattern("/facts/>")?,
        SqliteProjectionStore::new(root.join("projections.sqlite")),
        root.join("gateway.snapshot"),
        root.join("dns.snapshot"),
    );
    let report = actor.project_once(DEADLINE).await.map_err(|error| {
        format!("projection scale for {logical_nodes} logical nodes failed: {error}")
    })?;
    if report.state.nodes.len() != logical_nodes {
        return Err(format!(
            "projection scale expected {logical_nodes} nodes, got {}",
            report.state.nodes.len()
        ));
    }
    if report.state.services.len() != logical_nodes {
        return Err(format!(
            "projection scale expected {logical_nodes} services, got {}",
            report.state.services.len()
        ));
    }
    let gateway = report
        .state
        .gateway
        .as_ref()
        .ok_or_else(|| "projection scale missing gateway state".to_string())?;
    if gateway.routes.len() != 1 {
        return Err(format!(
            "projection scale expected one gateway route, got {}",
            gateway.routes.len()
        ));
    }
    let gateway_backends = gateway
        .routes
        .first()
        .map_or(0, |route| route.backends.len());
    if gateway_backends != logical_nodes {
        return Err(format!(
            "projection scale expected {logical_nodes} gateway backends, got {gateway_backends}"
        ));
    }
    let dns_records = report.state.dns.as_ref().map_or(0, |dns| dns.records.len());
    if dns_records != 1 {
        return Err(format!(
            "projection scale expected one DNS record, got {dns_records}"
        ));
    }
    if report.duration > DEADLINE {
        return Err(format!(
            "projection scale exceeded deadline for {logical_nodes} logical nodes: {:?} > {:?}",
            report.duration, DEADLINE
        ));
    }

    let row_counts = SqliteProjectionStore::new(root.join("projections.sqlite"))
        .row_counts()
        .map_err(|error| format!("load projection scale sqlite: {error}"))?;
    if row_counts.nodes != logical_nodes
        || row_counts.services != logical_nodes
        || row_counts.gateway_routes != 1
        || row_counts.dns_records != 1
    {
        return Err(format!(
            "projection scale sqlite rows did not match state for {logical_nodes} logical nodes"
        ));
    }

    Ok(ProjectionScaleRunReport {
        logical_nodes,
        fact_writes: logical_nodes * 2 + 3,
        projected_nodes: report.state.nodes.len(),
        projected_services: report.state.services.len(),
        gateway_routes: gateway.routes.len(),
        gateway_backends,
        dns_records,
        ignored_fact_count: report
            .state
            .statuses
            .iter()
            .map(|status| status.count)
            .sum(),
        actor_duration_ms: report.duration.as_millis(),
        elapsed_ms: started.elapsed().as_millis(),
        deadline_seconds: DEADLINE.as_secs(),
        deadline_satisfied: report.duration <= DEADLINE,
        sqlite_bytes: file_len(root.join("projections.sqlite"))?,
        gateway_snapshot_bytes: file_len(root.join("gateway.snapshot"))?,
        dns_snapshot_bytes: file_len(root.join("dns.snapshot"))?,
        memory_before,
        memory_after: memory_snapshot(),
    })
}

fn register_logical_nodes(
    bus: &InMemoryBus,
    admin: &mvp_bus::BusSession,
    logical_nodes: usize,
    gateway_wakeups: Arc<AtomicUsize>,
) -> Result<(), String> {
    for node_index in 0..logical_nodes {
        let gateway_wakeups = Arc::clone(&gateway_wakeups);
        bus.subscribe(admin, pattern("gateway.changed")?, move |_| {
            gateway_wakeups.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| format!("subscribe gateway.changed for node {node_index}: {error}"))?;

        bus.subscribe(
            admin,
            pattern(&format!("node.n{node_index}.capacity"))?,
            move |ctx| ctx.reply(format!("capacity:n{node_index}").into_bytes()),
        )
        .map_err(|error| format!("subscribe capacity for node {node_index}: {error}"))?;
    }
    Ok(())
}

fn runtime_report(snapshot: BusRuntimeSnapshot) -> RuntimeReport {
    RuntimeReport {
        delivery_workers: snapshot.delivery_workers,
        delivery_queue_capacity: snapshot.delivery_queue_capacity,
        max_worker_concurrency: snapshot.max_active_deliveries,
        max_queued_deliveries: snapshot.max_queued_deliveries,
        enqueue_full_count: snapshot.enqueue_full_count,
        enqueue_blocked_ns: snapshot.enqueue_blocked_ns,
    }
}

fn file_len(path: impl AsRef<Path>) -> Result<u64, String> {
    let path = path.as_ref();
    Ok(std::fs::metadata(path)
        .map_err(|error| format!("metadata '{}': {error}", path.display()))?
        .len())
}

fn run_multi_island_case() -> Result<MultiIslandReport, String> {
    let started = Instant::now();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let island_a = authority.grant_in(
        IslandId::new("island-a"),
        PrincipalId::new("admin"),
        Grant::allow_all(),
    );
    let island_b = authority.grant_in(
        IslandId::new("island-b"),
        PrincipalId::new("admin"),
        Grant::allow_all(),
    );
    let island_a_subscribers = MULTI_ISLAND_SUBSCRIBERS / 2;
    let island_b_subscribers = MULTI_ISLAND_SUBSCRIBERS - island_a_subscribers;
    let island_a_deliveries = Arc::new(AtomicUsize::new(0));
    let island_b_deliveries = Arc::new(AtomicUsize::new(0));
    let island_b_request_calls = Arc::new(AtomicUsize::new(0));
    let cross_island_delivery_count = Arc::new(AtomicUsize::new(0));

    for subscriber_index in 0..island_a_subscribers {
        let island_a_deliveries = Arc::clone(&island_a_deliveries);
        let cross_island_delivery_count = Arc::clone(&cross_island_delivery_count);
        bus.subscribe(&island_a, pattern("gateway.changed")?, move |ctx| {
            if ctx.message.island().as_str() != "island-a" {
                cross_island_delivery_count.fetch_add(1, Ordering::SeqCst);
            }
            island_a_deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| {
            format!("subscribe island-a handler {subscriber_index} failed: {error}")
        })?;
        bus.subscribe(
            &island_a,
            pattern(&format!("node.a{subscriber_index}.capacity"))?,
            move |ctx| ctx.reply(format!("capacity:a{subscriber_index}").into_bytes()),
        )
        .map_err(|error| {
            format!("subscribe island-a capacity handler {subscriber_index} failed: {error}")
        })?;
    }
    for subscriber_index in 0..island_b_subscribers {
        let island_b_deliveries = Arc::clone(&island_b_deliveries);
        let island_b_request_calls = Arc::clone(&island_b_request_calls);
        let cross_island_delivery_count = Arc::clone(&cross_island_delivery_count);
        bus.subscribe(&island_b, pattern("gateway.changed")?, move |ctx| {
            if ctx.message.island().as_str() != "island-b" {
                cross_island_delivery_count.fetch_add(1, Ordering::SeqCst);
            }
            island_b_deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| {
            format!("subscribe island-b handler {subscriber_index} failed: {error}")
        })?;
        bus.subscribe(
            &island_b,
            pattern(&format!("node.b{subscriber_index}.capacity"))?,
            move |ctx| {
                island_b_request_calls.fetch_add(1, Ordering::SeqCst);
                ctx.reply(format!("capacity:b{subscriber_index}").into_bytes())
            },
        )
        .map_err(|error| {
            format!("subscribe island-b capacity handler {subscriber_index} failed: {error}")
        })?;
    }

    bus.publish(
        &island_a,
        subject("gateway.changed")?,
        Payload::from_static(b"snapshot-ready"),
    )
    .map_err(|error| format!("multi-island publish failed: {error}"))?;

    let island_a_observed = island_a_deliveries.load(Ordering::SeqCst);
    let island_b_observed = island_b_deliveries.load(Ordering::SeqCst);
    let cross_island_observed = cross_island_delivery_count.load(Ordering::SeqCst);
    if island_a_observed != island_a_subscribers {
        return Err(format!(
            "multi-island expected {island_a_subscribers} island-a deliveries, got {island_a_observed}"
        ));
    }
    if island_b_observed != 0 {
        return Err(format!(
            "multi-island expected zero island-b deliveries, got {island_b_observed}"
        ));
    }
    if cross_island_observed != 0 {
        return Err(format!(
            "multi-island expected zero cross-island deliveries, got {cross_island_observed}"
        ));
    }

    let replies = bus
        .request_many(
            &island_a,
            RequestTarget::Pattern(pattern("node.*.capacity")?),
            subject("node.broadcast.capacity")?,
            Payload::from_static(b"inspect-capacity"),
            RequestManyPolicy::new(island_a_subscribers, Duration::from_secs(5)),
        )
        .map_err(|error| format!("multi-island request_many failed: {error}"))?;
    let unique_replies = replies
        .iter()
        .map(|response| response.payload().as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    let island_b_request_calls = island_b_request_calls.load(Ordering::SeqCst);
    if unique_replies.len() != island_a_subscribers {
        return Err(format!(
            "multi-island expected {island_a_subscribers} unique island-a replies, got {}",
            unique_replies.len()
        ));
    }
    if island_b_request_calls != 0 {
        return Err(format!(
            "multi-island expected zero island-b request calls, got {island_b_request_calls}"
        ));
    }

    Ok(MultiIslandReport {
        subscribers: MULTI_ISLAND_SUBSCRIBERS,
        island_a_subscribers,
        island_b_subscribers,
        island_a_deliveries: island_a_observed,
        island_b_deliveries: island_b_observed,
        island_a_replies: replies.len(),
        island_b_request_calls,
        cross_island_delivery_count: cross_island_observed,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn run_bridge_case() -> Result<BridgeScaleReport, String> {
    let mut stream_runs = Vec::with_capacity(NODE_COUNTS.len());
    for logical_subscribers in NODE_COUNTS {
        stream_runs.push(run_bridge_stream_case(logical_subscribers)?);
    }
    Ok(BridgeScaleReport {
        stream_runs,
        service: run_bridge_service_case()?,
        rule_volume: run_bridge_rule_volume_case()?,
    })
}

fn run_bridge_stream_case(logical_subscribers: usize) -> Result<BridgeStreamRunReport, String> {
    let started = Instant::now();
    let memory_before = memory_snapshot();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let laptop = IslandId::new("laptop");
    let prod = IslandId::new("prod");
    let prod_admin = authority.grant_in(
        prod.clone(),
        PrincipalId::new("admin"),
        Grant::empty().with_publish(pattern("deploy.*.status")?),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        PrincipalId::new("bridge"),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")?),
    );
    authority.grant_in(
        laptop.clone(),
        PrincipalId::new("bridge"),
        Grant::empty().with_publish(pattern("prod.deploy.*.status")?),
    );
    let laptop_reader = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("reader"),
        Grant::empty().with_subscribe(pattern("prod.deploy.*.status")?),
    );
    authority
        .add_stream_import(StreamImport::new(
            BridgeRuleId::new("prod-status"),
            prod.clone(),
            laptop.clone(),
            prod_bridge.principal().clone(),
            SubjectTransform::new(
                pattern("deploy.*.status")?,
                pattern("prod.deploy.*.status")?,
            )
            .map_err(|error| format!("create bridge stream transform: {error}"))?,
        ))
        .map_err(|error| format!("add bridge stream import: {error}"))?;

    let deliveries = Arc::new(AtomicUsize::new(0));
    let cross_island_leakage = Arc::new(AtomicUsize::new(0));
    for subscriber_index in 0..logical_subscribers {
        let deliveries = Arc::clone(&deliveries);
        let cross_island_leakage = Arc::clone(&cross_island_leakage);
        bus.subscribe(
            &laptop_reader,
            pattern("prod.deploy.*.status")?,
            move |ctx| {
                if ctx.message.island().as_str() != "laptop"
                    || ctx.message.principal().as_str() != "bridge"
                    || ctx.message.bridge_origin().is_none()
                {
                    cross_island_leakage.fetch_add(1, Ordering::SeqCst);
                }
                deliveries.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .map_err(|error| {
            format!("subscribe bridge stream handler {subscriber_index} failed: {error}")
        })?;
    }

    let mut publish_latency = LatencyRecorder::new()?;
    for _ in 0..BRIDGE_STREAM_ITERATIONS {
        let started = Instant::now();
        bus.publish(
            &prod_admin,
            subject("deploy.d1.status")?,
            Payload::from_static(b"running"),
        )
        .map_err(|error| {
            format!("bridge stream publish to {logical_subscribers} subscribers failed: {error}")
        })?;
        publish_latency.record(started.elapsed())?;
    }

    let expected_deliveries = logical_subscribers * BRIDGE_STREAM_ITERATIONS;
    let observed_deliveries = deliveries.load(Ordering::SeqCst);
    let cross_island_leakage_count = cross_island_leakage.load(Ordering::SeqCst);
    if observed_deliveries != expected_deliveries {
        return Err(format!(
            "bridge stream expected {expected_deliveries} deliveries, got {observed_deliveries}"
        ));
    }
    if cross_island_leakage_count != 0 {
        return Err(format!(
            "bridge stream expected zero cross-island leakage, got {cross_island_leakage_count}"
        ));
    }

    Ok(BridgeStreamRunReport {
        logical_subscribers,
        publish_iterations: BRIDGE_STREAM_ITERATIONS,
        expected_deliveries,
        observed_deliveries,
        cross_island_leakage_count,
        publish_latency: publish_latency.summary(),
        memory_before,
        memory_after: memory_snapshot(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn run_bridge_service_case() -> Result<BridgeServiceReport, String> {
    let started = Instant::now();
    let memory_before = memory_snapshot();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let laptop = IslandId::new("laptop");
    let prod = IslandId::new("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")?),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        PrincipalId::new("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")?),
    );
    authority
        .add_service_import(ServiceImport::new(
            BridgeRuleId::new("gpu-deploy"),
            BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")?),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")?),
            prod_bridge.principal().clone(),
        ))
        .map_err(|error| format!("add bridge service import: {error}"))?;

    let deliveries = Arc::new(AtomicUsize::new(0));
    for responder_index in 0..BRIDGE_SERVICE_RESPONDERS {
        let prod_scheduler = authority.grant_in(
            prod.clone(),
            PrincipalId::new(format!("scheduler-{responder_index}")),
            Grant::empty()
                .with_queue(pattern("deploy.submit")?, "schedulers")
                .with_response(),
        );
        let deliveries = Arc::clone(&deliveries);
        bus.queue_subscribe(
            &prod_scheduler,
            pattern("deploy.submit")?,
            "schedulers",
            move |ctx| {
                deliveries.fetch_add(1, Ordering::SeqCst);
                ctx.reply(format!("scheduler:{responder_index}").into_bytes())
            },
        )
        .map_err(|error| {
            format!("subscribe bridge service responder {responder_index} failed: {error}")
        })?;
    }

    let mut request_latency = LatencyRecorder::new()?;
    let mut unique_responders = BTreeSet::new();
    for request_index in 0..BRIDGE_SERVICE_REQUESTS {
        let started = Instant::now();
        let response = bus
            .request(
                &laptop_user,
                subject("gpu.deploy.submit")?,
                Payload::from_static(b"manifest"),
                Duration::from_secs(5),
            )
            .map_err(|error| format!("bridge service request {request_index} failed: {error}"))?;
        request_latency.record(started.elapsed())?;
        if response.island().as_str() != "prod" {
            return Err(format!(
                "bridge service response {request_index} came from {}",
                response.island()
            ));
        }
        unique_responders.insert(response.responder().as_str().to_string());
    }

    let observed_deliveries = deliveries.load(Ordering::SeqCst);
    if observed_deliveries != BRIDGE_SERVICE_REQUESTS {
        return Err(format!(
            "bridge service expected {BRIDGE_SERVICE_REQUESTS} deliveries, got {observed_deliveries}"
        ));
    }
    if unique_responders.len() != BRIDGE_SERVICE_REQUESTS {
        return Err(format!(
            "bridge service expected {BRIDGE_SERVICE_REQUESTS} unique queue responders, got {}",
            unique_responders.len()
        ));
    }
    Ok(BridgeServiceReport {
        responders: BRIDGE_SERVICE_RESPONDERS,
        requests: BRIDGE_SERVICE_REQUESTS,
        expected_deliveries: BRIDGE_SERVICE_REQUESTS,
        observed_deliveries,
        unique_responders: unique_responders.len(),
        request_latency: request_latency.summary(),
        memory_before,
        memory_after: memory_snapshot(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn run_bridge_rule_volume_case() -> Result<BridgeRuleVolumeReport, String> {
    let started = Instant::now();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let laptop = IslandId::new("laptop");
    let prod = IslandId::new("prod");
    let bridge_principal = PrincipalId::new("bridge");
    let target_index = BRIDGE_RULE_VOLUME_IMPORTS - 1;
    let target_local_subject = subject(&format!("gpu{target_index}.deploy.submit"))?;

    let laptop_user = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("user"),
        Grant::empty().with_publish(pattern(&target_local_subject.to_string())?),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        bridge_principal.clone(),
        Grant::empty().with_publish(pattern("deploy.submit")?),
    );
    for import_index in 0..BRIDGE_RULE_VOLUME_IMPORTS {
        authority
            .add_service_import(ServiceImport::new(
                BridgeRuleId::new(format!("service-{import_index}")),
                BridgeEndpoint::new(
                    laptop.clone(),
                    subject(&format!("gpu{import_index}.deploy.submit"))?,
                ),
                BridgeEndpoint::new(prod.clone(), subject("deploy.submit")?),
                prod_bridge.principal().clone(),
            ))
            .map_err(|error| format!("add bridge service volume import {import_index}: {error}"))?;
    }

    let service_deliveries = Arc::new(AtomicUsize::new(0));
    let scheduler = authority.grant_in(
        prod.clone(),
        PrincipalId::new("scheduler"),
        Grant::empty()
            .with_queue(pattern("deploy.submit")?, "schedulers")
            .with_response(),
    );
    let service_deliveries_for_handler = Arc::clone(&service_deliveries);
    bus.queue_subscribe(
        &scheduler,
        pattern("deploy.submit")?,
        "schedulers",
        move |ctx| {
            service_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(Payload::from_static(b"scheduled"))
        },
    )
    .map_err(|error| format!("subscribe bridge volume scheduler: {error}"))?;

    let service_started = Instant::now();
    let response = bus
        .request(
            &laptop_user,
            target_local_subject,
            Payload::from_static(b"manifest"),
            Duration::from_secs(5),
        )
        .map_err(|error| format!("bridge volume service request failed: {error}"))?;
    let service_request_us = service_started.elapsed().as_micros();
    if response.island() != &prod {
        return Err(format!(
            "bridge volume service response came from {}",
            response.island()
        ));
    }
    let service_delivery_count = service_deliveries.load(Ordering::SeqCst);
    if service_delivery_count != 1 {
        return Err(format!(
            "bridge volume service expected one delivery, got {service_delivery_count}"
        ));
    }

    let target_stream_island = IslandId::new(format!("prod-stream-{target_index}"));
    let target_destination = format!("prod{target_index}.deploy.*.status");
    let target_delivery_subject = format!("prod{target_index}.deploy.d1.status");
    let stream_admin = authority.grant_in(
        target_stream_island.clone(),
        PrincipalId::new("admin"),
        Grant::empty().with_publish(pattern("deploy.*.status")?),
    );
    authority.grant_in(
        target_stream_island.clone(),
        bridge_principal.clone(),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")?),
    );
    authority.grant_in(
        laptop.clone(),
        bridge_principal.clone(),
        Grant::empty().with_publish(pattern(&target_destination)?),
    );
    let stream_reader = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("reader"),
        Grant::empty().with_subscribe(pattern(&target_destination)?),
    );
    for import_index in 0..BRIDGE_RULE_VOLUME_IMPORTS {
        authority
            .add_stream_import(StreamImport::new(
                BridgeRuleId::new(format!("stream-{import_index}")),
                IslandId::new(format!("prod-stream-{import_index}")),
                laptop.clone(),
                bridge_principal.clone(),
                SubjectTransform::new(
                    pattern("deploy.*.status")?,
                    pattern(&format!("prod{import_index}.deploy.*.status"))?,
                )
                .map_err(|error| {
                    format!("create bridge volume stream transform {import_index}: {error}")
                })?,
            ))
            .map_err(|error| format!("add bridge stream volume import {import_index}: {error}"))?;
    }

    let stream_deliveries = Arc::new(AtomicUsize::new(0));
    let stream_leakage = Arc::new(AtomicUsize::new(0));
    let stream_deliveries_for_handler = Arc::clone(&stream_deliveries);
    let stream_leakage_for_handler = Arc::clone(&stream_leakage);
    bus.subscribe(
        &stream_reader,
        pattern(&target_delivery_subject)?,
        move |ctx| {
            if ctx.message.island().as_str() != "laptop"
                || ctx.message.principal().as_str() != "bridge"
                || ctx.message.bridge_origin().is_none()
            {
                stream_leakage_for_handler.fetch_add(1, Ordering::SeqCst);
            }
            stream_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .map_err(|error| format!("subscribe bridge volume stream reader: {error}"))?;

    let stream_started = Instant::now();
    bus.publish(
        &stream_admin,
        subject("deploy.d1.status")?,
        Payload::from_static(b"running"),
    )
    .map_err(|error| format!("bridge volume stream publish failed: {error}"))?;
    let stream_publish_us = stream_started.elapsed().as_micros();

    let stream_delivery_count = stream_deliveries.load(Ordering::SeqCst);
    let stream_leakage_count = stream_leakage.load(Ordering::SeqCst);
    if stream_delivery_count != 1 {
        return Err(format!(
            "bridge volume stream expected one delivery, got {stream_delivery_count}"
        ));
    }
    if stream_leakage_count != 0 {
        return Err(format!(
            "bridge volume stream expected zero leakage, got {stream_leakage_count}"
        ));
    }

    let matching_source = IslandId::new("prod-stream-shared");
    let matching_stream_admin = authority.grant_in(
        matching_source.clone(),
        PrincipalId::new("matching-admin"),
        Grant::empty().with_publish(pattern("deploy.*.status")?),
    );
    authority.grant_in(
        matching_source.clone(),
        bridge_principal.clone(),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")?),
    );
    let matching_stream_deliveries = Arc::new(AtomicUsize::new(0));
    let matching_stream_leakage = Arc::new(AtomicUsize::new(0));
    for import_index in 0..BRIDGE_RULE_VOLUME_IMPORTS {
        let local_island = IslandId::new(format!("laptop-match-{import_index}"));
        authority.grant_in(
            local_island.clone(),
            bridge_principal.clone(),
            Grant::empty().with_publish(pattern("prod.deploy.*.status")?),
        );
        let reader = authority.grant_in(
            local_island.clone(),
            PrincipalId::new("reader"),
            Grant::empty().with_subscribe(pattern("prod.deploy.*.status")?),
        );
        authority
            .add_stream_import(StreamImport::new(
                BridgeRuleId::new(format!("stream-match-{import_index}")),
                matching_source.clone(),
                local_island,
                bridge_principal.clone(),
                SubjectTransform::new(
                    pattern("deploy.*.status")?,
                    pattern("prod.deploy.*.status")?,
                )
                .map_err(|error| {
                    format!("create matching bridge volume transform {import_index}: {error}")
                })?,
            ))
            .map_err(|error| {
                format!("add matching bridge stream import {import_index}: {error}")
            })?;
        let matching_stream_deliveries_for_handler = Arc::clone(&matching_stream_deliveries);
        let matching_stream_leakage_for_handler = Arc::clone(&matching_stream_leakage);
        bus.subscribe(&reader, pattern("prod.deploy.*.status")?, move |ctx| {
            if ctx.message.principal().as_str() != "bridge"
                || ctx.message.subject().as_str() != "prod.deploy.d1.status"
                || ctx
                    .message
                    .bridge_origin()
                    .is_none_or(|origin| origin.source_island().as_str() != "prod-stream-shared")
            {
                matching_stream_leakage_for_handler.fetch_add(1, Ordering::SeqCst);
            }
            matching_stream_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| {
            format!("subscribe matching bridge volume reader {import_index}: {error}")
        })?;
    }

    let matching_stream_started = Instant::now();
    bus.publish(
        &matching_stream_admin,
        subject("deploy.d1.status")?,
        Payload::from_static(b"running"),
    )
    .map_err(|error| format!("matching bridge volume stream publish failed: {error}"))?;
    let matching_stream_publish_us = matching_stream_started.elapsed().as_micros();
    let matching_stream_delivery_count = matching_stream_deliveries.load(Ordering::SeqCst);
    let matching_stream_leakage_count = matching_stream_leakage.load(Ordering::SeqCst);
    if matching_stream_delivery_count != BRIDGE_RULE_VOLUME_IMPORTS {
        return Err(format!(
            "matching bridge volume stream expected {BRIDGE_RULE_VOLUME_IMPORTS} deliveries, got {matching_stream_delivery_count}"
        ));
    }
    if matching_stream_leakage_count != 0 {
        return Err(format!(
            "matching bridge volume stream expected zero leakage, got {matching_stream_leakage_count}"
        ));
    }

    Ok(BridgeRuleVolumeReport {
        service_imports: BRIDGE_RULE_VOLUME_IMPORTS,
        stream_imports: BRIDGE_RULE_VOLUME_IMPORTS * 2,
        service_delivery_count,
        stream_delivery_count,
        stream_leakage_count,
        matching_stream_imports: BRIDGE_RULE_VOLUME_IMPORTS,
        matching_stream_delivery_count,
        matching_stream_leakage_count,
        service_request_us,
        stream_publish_us,
        matching_stream_publish_us,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn run_saturation_case() -> Result<SaturationReport, String> {
    let memory_before = memory_snapshot();
    let started = Instant::now();
    let config = BusRuntimeConfig::with_delivery_workers(SATURATION_DELIVERY_WORKERS)
        .with_delivery_queue_capacity(SATURATION_QUEUE_CAPACITY);
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(config);
    let admin = authority.grant(PrincipalId::new("admin"), Grant::allow_all());
    let deliveries = Arc::new(AtomicUsize::new(0));
    for subscriber_index in 0..SATURATION_SUBSCRIBERS {
        let deliveries = Arc::clone(&deliveries);
        bus.subscribe(&admin, pattern("gateway.changed")?, move |_| {
            thread::sleep(SATURATION_HANDLER_SLEEP);
            deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| {
            format!("subscribe saturation handler {subscriber_index} failed: {error}")
        })?;
    }

    let mut publishers = Vec::with_capacity(SATURATION_PUBLISHERS);
    for _ in 0..SATURATION_PUBLISHERS {
        let bus = bus.clone();
        let admin = admin.clone();
        publishers.push(thread::spawn(move || {
            bus.publish(
                &admin,
                Subject::parse("gateway.changed").expect("subject parses"),
                Payload::from_static(b"snapshot-ready"),
            )
        }));
    }
    for publisher in publishers {
        publisher
            .join()
            .map_err(|_| String::from("saturation publisher thread panicked"))?
            .map_err(|error| format!("saturation publish failed: {error}"))?;
    }

    let observed_elapsed_ms = started.elapsed().as_millis();
    let expected_deliveries = SATURATION_SUBSCRIBERS * SATURATION_PUBLISHERS;
    let observed_deliveries = deliveries.load(Ordering::SeqCst);
    if observed_deliveries != expected_deliveries {
        return Err(format!(
            "saturation expected {expected_deliveries} deliveries, got {observed_deliveries}"
        ));
    }

    let runtime = runtime_report(bus.runtime_snapshot());
    if runtime.max_worker_concurrency > SATURATION_DELIVERY_WORKERS {
        return Err(format!(
            "saturation concurrency exceeded worker bound: {} > {SATURATION_DELIVERY_WORKERS}",
            runtime.max_worker_concurrency
        ));
    }
    if runtime.max_queued_deliveries < SATURATION_QUEUE_CAPACITY || runtime.enqueue_full_count == 0
    {
        return Err(format!(
            "saturation did not observe delivery queue pressure: max_queued={}, full_count={}",
            runtime.max_queued_deliveries, runtime.enqueue_full_count
        ));
    }

    let minimum_expected_elapsed_ms = saturation_minimum_expected_elapsed_ms(expected_deliveries);
    let bounded_backpressure_observed = observed_elapsed_ms >= minimum_expected_elapsed_ms;
    if !bounded_backpressure_observed {
        return Err(format!(
            "saturation completed too quickly: expected at least {minimum_expected_elapsed_ms}ms, got {observed_elapsed_ms}ms"
        ));
    }

    Ok(SaturationReport {
        publishers: SATURATION_PUBLISHERS,
        subscribers: SATURATION_SUBSCRIBERS,
        expected_deliveries,
        observed_deliveries,
        handler_sleep_ms: SATURATION_HANDLER_SLEEP.as_millis(),
        minimum_expected_elapsed_ms,
        observed_elapsed_ms,
        bounded_backpressure_observed,
        runtime,
        memory_before,
        memory_after: memory_snapshot(),
    })
}

fn run_queue_group_case() -> Result<QueueGroupReport, String> {
    let started = Instant::now();
    let (bus, authority) = InMemoryBus::new_with_authority_and_config(
        BusRuntimeConfig::with_delivery_workers(DELIVERY_WORKERS),
    );
    let admin = authority.grant(PrincipalId::new("admin"), Grant::allow_all());
    let deliveries = Arc::new(AtomicUsize::new(0));
    for subscriber_index in 0..QUEUE_GROUP_SUBSCRIBERS {
        let deliveries = Arc::clone(&deliveries);
        bus.queue_subscribe(
            &admin,
            pattern("image.pull")?,
            "image-workers",
            move |ctx| {
                deliveries.fetch_add(1, Ordering::SeqCst);
                ctx.reply(format!("worker:{subscriber_index}").into_bytes())
            },
        )
        .map_err(|error| {
            format!("subscribe queue group handler {subscriber_index} failed: {error}")
        })?;
    }

    let mut unique_responders = BTreeSet::new();
    for request_index in 0..QUEUE_GROUP_REQUESTS {
        let response = bus
            .request(
                &admin,
                subject("image.pull")?,
                Payload::from_static(b"pull-image"),
                Duration::from_secs(5),
            )
            .map_err(|error| format!("queue group request {request_index} failed: {error}"))?;
        unique_responders.insert(response.payload().as_bytes().to_vec());
    }

    let observed_deliveries = deliveries.load(Ordering::SeqCst);
    if observed_deliveries != QUEUE_GROUP_REQUESTS {
        return Err(format!(
            "queue group expected {QUEUE_GROUP_REQUESTS} deliveries, got {observed_deliveries}"
        ));
    }
    if unique_responders.len() != QUEUE_GROUP_REQUESTS {
        return Err(format!(
            "queue group expected {QUEUE_GROUP_REQUESTS} unique responders, got {}",
            unique_responders.len()
        ));
    }

    Ok(QueueGroupReport {
        subscribers: QUEUE_GROUP_SUBSCRIBERS,
        requests: QUEUE_GROUP_REQUESTS,
        expected_deliveries: QUEUE_GROUP_REQUESTS,
        observed_deliveries,
        unique_responders: unique_responders.len(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn saturation_minimum_expected_elapsed_ms(expected_deliveries: usize) -> u128 {
    let delivery_batches = expected_deliveries.div_ceil(SATURATION_DELIVERY_WORKERS);
    let ideal_elapsed_ms = delivery_batches as u128 * SATURATION_HANDLER_SLEEP.as_millis();
    ideal_elapsed_ms / 2
}
