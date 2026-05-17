use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusError, BusSession, Grant, PrincipalId, RequestManyPolicy, RequestTarget, Subject,
    harness::InMemoryBus,
};
use serde::Serialize;

use crate::bus_syntax::{pattern, subject};
use crate::metrics::write_json;

#[derive(Debug, Default)]
struct Metrics {
    requests: usize,
    responses: usize,
    no_responders: usize,
    timeouts: usize,
    queue_deliveries: usize,
    unauthorized_failures: usize,
}

#[derive(Debug, Serialize)]
struct MetricsReport {
    scenario: &'static str,
    requests: usize,
    responses: usize,
    no_responders: usize,
    timeouts: usize,
    queue_deliveries: usize,
    unauthorized_failures: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let (bus, authority) = InMemoryBus::new_with_authority();
    let admin = PrincipalId::new("admin");
    let scheduler_a = PrincipalId::new("scheduler-a");
    let scheduler_b = PrincipalId::new("scheduler-b");
    let intruder = PrincipalId::new("intruder");

    let admin = authority.grant(admin, Grant::allow_all());
    let scheduler_a = authority.grant(scheduler_a, Grant::allow_all());
    let scheduler_b = authority.grant(scheduler_b, Grant::allow_all());
    let intruder_session = authority.grant(intruder.clone(), Grant::empty());

    let mut metrics = Metrics::default();

    register_capacity_responder(&bus, "alpha", &admin)?;
    register_capacity_responder(&bus, "beta", &admin)?;

    let status_deliveries = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let deliveries = Arc::clone(&status_deliveries);
        bus.subscribe(&admin, pattern("node.*.status")?, move |_| {
            deliveries.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .map_err(|error| format!("status subscribe failed: {error}"))?;
    }
    bus.publish(&admin, subject("node.alpha.status")?, b"healthy".to_vec())
        .map_err(|error| format!("status publish failed: {error}"))?;
    assert_eq_named(
        "publish fanout deliveries",
        status_deliveries.load(Ordering::SeqCst),
        2,
    )?;

    let capacity = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")?),
            subject("node.broadcast.capacity")?,
            b"inspect".to_vec(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .map_err(|error| format!("request_many capacity failed: {error}"))?;
    metrics.requests += 1;
    metrics.responses += capacity.len();
    assert_eq_named("capacity reply count", capacity.len(), 2)?;
    let mut capacity_payloads = capacity
        .iter()
        .map(|response| String::from_utf8_lossy(response.payload()).to_string())
        .collect::<Vec<_>>();
    capacity_payloads.sort();
    assert_eq_named(
        "capacity payloads",
        capacity_payloads,
        vec![
            String::from("capacity:alpha"),
            String::from("capacity:beta"),
        ],
    )?;

    let queue_deliveries = Arc::new(AtomicUsize::new(0));
    register_scheduler(&bus, &scheduler_a, Arc::clone(&queue_deliveries))?;
    register_scheduler(&bus, &scheduler_b, Arc::clone(&queue_deliveries))?;

    for deploy in ["deploy-1", "deploy-2"] {
        let response = bus
            .request(
                &admin,
                subject("deploy.submit")?,
                deploy.as_bytes().to_vec(),
                Duration::from_secs(1),
            )
            .map_err(|error| format!("deploy submit request failed: {error}"))?;
        if response.payload().as_bytes() != b"accepted".as_slice() {
            return Err(format!(
                "deploy submit returned unexpected payload: {:?}",
                String::from_utf8_lossy(response.payload())
            ));
        }
        metrics.requests += 1;
        metrics.responses += 1;
    }
    assert_eq_named(
        "queue delivery count",
        queue_deliveries.load(Ordering::SeqCst),
        2,
    )?;
    metrics.queue_deliveries += queue_deliveries.load(Ordering::SeqCst);

    let no_responder_error = expect_error(
        "no responders request",
        bus.request(
            &admin,
            subject("node.missing.inspect")?,
            Vec::new(),
            Duration::from_millis(20),
        ),
    )?;
    assert_error("no responders", &no_responder_error, |error| {
        matches!(error, BusError::NoResponders { .. })
    })?;
    metrics.requests += 1;
    metrics.no_responders += 1;

    bus.subscribe(&admin, pattern("node.slow.inspect")?, |_ctx| {
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })
    .map_err(|error| format!("subscribe slow responder failed: {error}"))?;
    let timeout_error = expect_error(
        "timeout request",
        bus.request(
            &admin,
            subject("node.slow.inspect")?,
            Vec::new(),
            Duration::from_millis(5),
        ),
    )?;
    assert_error("timeout", &timeout_error, |error| {
        matches!(error, BusError::Timeout { .. })
    })?;
    metrics.requests += 1;
    metrics.timeouts += 1;

    let unauthorized_publish = expect_error(
        "unauthorized publish",
        bus.publish(
            &intruder_session,
            subject("node.alpha.status")?,
            b"bad".to_vec(),
        ),
    )?;
    assert_error("unauthorized publish", &unauthorized_publish, |error| {
        matches!(error, BusError::UnauthorizedPublish { .. })
    })?;
    metrics.unauthorized_failures += 1;

    let response_error = Arc::new(Mutex::new(None));
    let response_error_for_handler = Arc::clone(&response_error);
    let intruder_for_handler = intruder.clone();
    bus.subscribe(&admin, pattern("node.alpha.secure")?, move |ctx| {
        let error = ctx
            .reply_as(&intruder_for_handler, b"not allowed".to_vec())
            .unwrap_err();
        *response_error_for_handler
            .lock()
            .map_err(|_| BusError::HandlerFailed {
                subject: String::from("node.alpha.secure"),
                reason: String::from("response error lock poisoned"),
            })? = Some(error);
        Ok(())
    })
    .map_err(|error| format!("subscribe secure responder failed: {error}"))?;
    let secure_error = expect_error(
        "secure request",
        bus.request(
            &admin,
            subject("node.alpha.secure")?,
            Vec::new(),
            Duration::from_millis(20),
        ),
    )?;
    assert_error("secure request timeout", &secure_error, |error| {
        matches!(error, BusError::Timeout { .. })
    })?;
    let stored_response_error = response_error
        .lock()
        .map_err(|_| String::from("response error lock poisoned"))?
        .clone()
        .ok_or_else(|| String::from("handler did not record unauthorized response"))?;
    assert_error("unauthorized response", &stored_response_error, |error| {
        matches!(error, BusError::UnauthorizedResponse { .. })
    })?;
    metrics.requests += 1;
    metrics.timeouts += 1;
    metrics.unauthorized_failures += 1;

    let (drain_started_tx, drain_started_rx) = mpsc::channel();
    bus.subscribe(&admin, pattern("node.drain.inspect")?, move |ctx| {
        let _ = drain_started_tx.send(());
        thread::sleep(Duration::from_millis(20));
        ctx.reply(b"drained".to_vec())
    })
    .map_err(|error| format!("subscribe drain responder failed: {error}"))?;
    let bus_for_drain_request = bus.clone();
    let admin_for_drain_request = admin.clone();
    let drain_request = thread::spawn(move || {
        bus_for_drain_request.request(
            &admin_for_drain_request,
            Subject::parse("node.drain.inspect").expect("valid subject"),
            Vec::new(),
            Duration::from_secs(1),
        )
    });
    drain_started_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|error| format!("drain responder did not start: {error}"))?;
    bus.drain(&admin, Duration::from_secs(1))
        .map_err(|error| format!("drain failed: {error}"))?;
    let drain_response = drain_request
        .join()
        .map_err(|_| String::from("drain request thread panicked"))?
        .map_err(|error| format!("in-flight drain request failed: {error}"))?;
    assert_eq_named(
        "in-flight drain response",
        drain_response.payload().as_bytes(),
        b"drained".as_slice(),
    )?;
    metrics.requests += 1;
    metrics.responses += 1;

    let drain_error = expect_error(
        "drain rejects publish",
        bus.publish(
            &admin,
            subject("node.alpha.status")?,
            b"after drain".to_vec(),
        ),
    )?;
    assert_error("drain rejects publish", &drain_error, |error| {
        matches!(error, BusError::Draining)
    })?;
    let drain_request_error = expect_error(
        "drain rejects request",
        bus.request(
            &admin,
            subject("deploy.submit")?,
            b"deploy-after-drain".to_vec(),
            Duration::from_millis(20),
        ),
    )?;
    assert_error("drain rejects request", &drain_request_error, |error| {
        matches!(error, BusError::Draining)
    })?;

    write_metrics(&metrics, started)?;
    println!("PASS bus-contract");
    Ok(())
}

fn register_capacity_responder(
    bus: &InMemoryBus,
    node: &str,
    admin: &BusSession,
) -> Result<(), String> {
    let node = node.to_string();
    bus.subscribe(
        admin,
        pattern(&format!("node.{node}.capacity"))?,
        move |ctx| ctx.reply(format!("capacity:{node}").into_bytes()),
    )
    .map(|_| ())
    .map_err(|error| format!("capacity subscribe failed: {error}"))
}

fn register_scheduler(
    bus: &InMemoryBus,
    principal: &BusSession,
    deliveries: Arc<AtomicUsize>,
) -> Result<(), String> {
    bus.queue_subscribe(
        principal,
        pattern("deploy.submit")?,
        "schedulers",
        move |ctx| {
            deliveries.fetch_add(1, Ordering::SeqCst);
            ctx.reply(if ctx.message.payload().is_empty() {
                b"empty".to_vec()
            } else {
                b"accepted".to_vec()
            })
        },
    )
    .map(|_| ())
    .map_err(|error| format!("scheduler queue subscribe failed: {error}"))
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

fn expect_error<T>(name: &str, result: Result<T, BusError>) -> Result<BusError, String> {
    match result {
        Ok(_) => Err(format!("{name}: expected error, got success")),
        Err(error) => Ok(error),
    }
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

fn write_metrics(metrics: &Metrics, started: Instant) -> Result<(), String> {
    let report = MetricsReport {
        scenario: "bus-contract",
        requests: metrics.requests,
        responses: metrics.responses,
        no_responders: metrics.no_responders,
        timeouts: metrics.timeouts,
        queue_deliveries: metrics.queue_deliveries,
        unauthorized_failures: metrics.unauthorized_failures,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = Path::new("target")
        .join("mvp-e2e")
        .join("bus-contract-metrics.json");
    write_json(&path, &report).map(|_| ())
}
