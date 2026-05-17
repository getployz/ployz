use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusRuntimeConfig, BusRuntimeSnapshot, Grant, IslandId, Payload, PrincipalId, RequestManyPolicy,
    RequestTarget, Subject, harness::InMemoryBus,
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
const MULTI_ISLAND_SUBSCRIBERS: usize = 1_000;
const QUEUE_GROUP_SUBSCRIBERS: usize = 10_000;
const QUEUE_GROUP_REQUESTS: usize = 100;

#[derive(Debug, Serialize)]
struct ScaleReport {
    scenario: &'static str,
    node_counts: Vec<ScaleRunReport>,
    multi_island: MultiIslandReport,
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
        queue_group: run_queue_group_case()?,
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
