use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mvp_bus::{BusActorHandle, BusError, Grant, PrincipalId, RequestManyPolicy, RequestTarget};
use serde::Serialize;

use crate::bus_syntax::{pattern, subject};
use crate::metrics::write_json;

#[derive(Debug, Serialize)]
struct ActorContractReport {
    scenario: &'static str,
    publish_deliveries: usize,
    queue_deliveries: usize,
    request_many_replies: usize,
    no_responders: usize,
    drain_rejections: usize,
    runtime_workers: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for actor contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let (bus, authority) = BusActorHandle::new_with_authority();
    let admin = authority.grant(PrincipalId::new("admin"), Grant::allow_all());
    let scheduler_a = authority.grant(PrincipalId::new("scheduler-a"), Grant::allow_all());
    let scheduler_b = authority.grant(PrincipalId::new("scheduler-b"), Grant::allow_all());

    let publish_deliveries = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let publish_deliveries = Arc::clone(&publish_deliveries);
        bus.subscribe(&admin, pattern("gateway.changed")?, move |_| {
            publish_deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .map_err(|error| format!("actor subscribe failed: {error}"))?;
    }

    for node in ["alpha", "beta"] {
        bus.subscribe(
            &admin,
            pattern(&format!("node.{node}.capacity"))?,
            move |ctx| ctx.reply(format!("capacity:{node}").into_bytes()),
        )
        .await
        .map_err(|error| format!("actor capacity subscribe failed: {error}"))?;
    }

    let queue_deliveries = Arc::new(AtomicUsize::new(0));
    register_scheduler(&bus, &scheduler_a, Arc::clone(&queue_deliveries)).await?;
    register_scheduler(&bus, &scheduler_b, Arc::clone(&queue_deliveries)).await?;

    bus.publish(&admin, subject("gateway.changed")?, b"snapshot".to_vec())
        .await
        .map_err(|error| format!("actor publish failed: {error}"))?;
    assert_eq_named(
        "actor publish deliveries",
        publish_deliveries.load(Ordering::SeqCst),
        2,
    )?;

    let deploy = bus
        .request(
            &admin,
            subject("deploy.submit")?,
            b"deploy-actor".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .map_err(|error| format!("actor queue request failed: {error}"))?;
    assert_eq_named(
        "actor queue reply",
        deploy.payload.as_bytes(),
        b"accepted".as_slice(),
    )?;
    assert_eq_named(
        "actor queue deliveries",
        queue_deliveries.load(Ordering::SeqCst),
        1,
    )?;

    let replies = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")?),
            subject("node.broadcast.capacity")?,
            b"inspect".to_vec(),
            RequestManyPolicy::new(4, Duration::from_secs(1)),
        )
        .await
        .map_err(|error| format!("actor request_many failed: {error}"))?;
    assert_eq_named("actor request_many replies", replies.len(), 2)?;

    let no_responders = bus
        .request(
            &admin,
            subject("node.missing.inspect")?,
            Vec::new(),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
    assert_error("actor no responders", &no_responders, |error| {
        matches!(error, BusError::NoResponders { .. })
    })?;

    let snapshot = bus
        .runtime_snapshot()
        .await
        .map_err(|error| format!("actor runtime snapshot failed: {error}"))?;
    bus.drain(&admin, Duration::from_secs(1))
        .await
        .map_err(|error| format!("actor drain failed: {error}"))?;
    let drain_rejection = bus
        .publish(&admin, subject("gateway.changed")?, b"after-drain".to_vec())
        .await
        .unwrap_err();
    assert_error("actor drain rejection", &drain_rejection, |error| {
        matches!(error, BusError::Draining)
    })?;

    let report = ActorContractReport {
        scenario: "actor-contract",
        publish_deliveries: publish_deliveries.load(Ordering::SeqCst),
        queue_deliveries: queue_deliveries.load(Ordering::SeqCst),
        request_many_replies: replies.len(),
        no_responders: 1,
        drain_rejections: 1,
        runtime_workers: snapshot.delivery_workers,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = Path::new("target")
        .join("mvp-e2e")
        .join("actor-contract-metrics.json");
    write_json(&path, &report)?;
    println!("PASS actor-contract");
    Ok(())
}

async fn register_scheduler(
    bus: &BusActorHandle,
    principal: &mvp_bus::BusSession,
    deliveries: Arc<AtomicUsize>,
) -> Result<(), String> {
    bus.queue_subscribe(
        principal,
        pattern("deploy.submit")?,
        "schedulers",
        move |ctx| {
            deliveries.fetch_add(1, Ordering::SeqCst);
            ctx.reply(if ctx.message.payload.is_empty() {
                b"empty".to_vec()
            } else {
                b"accepted".to_vec()
            })
        },
    )
    .await
    .map(|_| ())
    .map_err(|error| format!("actor scheduler queue subscribe failed: {error}"))
}

fn assert_eq_named<T>(name: &str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        return Ok(());
    }
    Err(format!("{name}: expected {expected:?}, got {actual:?}"))
}

fn assert_error(
    name: &str,
    error: &BusError,
    predicate: impl FnOnce(&BusError) -> bool,
) -> Result<(), String> {
    if predicate(error) {
        return Ok(());
    }
    Err(format!("{name}: unexpected error {error}"))
}
