use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusRuntimeConfig, BusRuntimeSnapshot, Grant, MemoryBus, Payload, PrincipalId,
    RequestManyPolicy, RequestTarget, Subject,
};
use serde::Serialize;

use crate::bus_syntax::{pattern, subject};
use crate::metrics::{
    LatencyRecorder, LatencySummary, MemorySnapshot, memory_snapshot, write_json,
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

#[derive(Debug, Serialize)]
struct ScaleReport {
    scenario: &'static str,
    node_counts: Vec<ScaleRunReport>,
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

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let mut node_counts = Vec::with_capacity(NODE_COUNTS.len());
    for logical_nodes in NODE_COUNTS {
        node_counts.push(run_node_count(logical_nodes)?);
    }

    let report = ScaleReport {
        scenario: "scale",
        node_counts,
        saturation: run_saturation_case()?,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = Path::new("target")
        .join("mvp-e2e")
        .join("scale-metrics.json");
    let json = write_json(&path, &report)?;
    println!("{json}");
    eprintln!("PASS scale");
    Ok(())
}

fn run_node_count(logical_nodes: usize) -> Result<ScaleRunReport, String> {
    let started = Instant::now();
    let memory_before = memory_snapshot();
    let (bus, authority) = MemoryBus::new_with_authority_and_config(
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
    let delivered_payload_bytes = expected_publish_deliveries * publish_payload.len()
        + expected_replies * request_payload.len();
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

fn register_logical_nodes(
    bus: &MemoryBus,
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
            move |ctx| ctx.reply(Payload::from_static(b"capacity-ready")),
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

fn run_saturation_case() -> Result<SaturationReport, String> {
    let memory_before = memory_snapshot();
    let started = Instant::now();
    let config = BusRuntimeConfig::with_delivery_workers(SATURATION_DELIVERY_WORKERS)
        .with_delivery_queue_capacity(SATURATION_QUEUE_CAPACITY);
    let (bus, authority) = MemoryBus::new_with_authority_and_config(config);
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

fn saturation_minimum_expected_elapsed_ms(expected_deliveries: usize) -> u128 {
    let delivery_batches = expected_deliveries.div_ceil(SATURATION_DELIVERY_WORKERS);
    let ideal_elapsed_ms = delivery_batches as u128 * SATURATION_HANDLER_SLEEP.as_millis();
    ideal_elapsed_ms / 2
}
