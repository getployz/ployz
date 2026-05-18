use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusActorHandle, BusError, FactContentHash, FactWriteOutcome, Grant, HandlerFailure, IslandId,
    PrincipalId,
};
use serde::Serialize;

use crate::assertions::{assert_eq_named, assert_error};
use crate::bus_syntax::{fact_key, fact_pattern, pattern, subject};
use crate::metrics::{scenario_dir, write_json};

#[derive(Debug, Default)]
struct Metrics {
    authorized_publishes: usize,
    isolated_publishes: usize,
    denied_publish_attempts: usize,
    denied_fact_writes: usize,
    denied_fact_reads: usize,
    revocation_failures: usize,
    fact_writes: usize,
    no_responders: usize,
    response_scope_failures: usize,
    queue_deliveries: usize,
    remote_queue_deliveries: usize,
    cross_island_delivery_count: usize,
}

#[derive(Debug, Serialize)]
struct AuthorityContractReport {
    scenario: &'static str,
    authorized_publishes: usize,
    isolated_publishes: usize,
    denied_publish_attempts: usize,
    denied_fact_writes: usize,
    denied_fact_reads: usize,
    revocation_failures: usize,
    fact_writes: usize,
    no_responders: usize,
    response_scope_failures: usize,
    queue_deliveries: usize,
    remote_queue_deliveries: usize,
    cross_island_delivery_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for authority contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let (bus, authority) = BusActorHandle::new_with_authority();
    let default_admin = authority.grant(PrincipalId::new("default-admin"), Grant::allow_all());
    let laptop = IslandId::new("laptop");
    let prod = IslandId::new("prod");
    let laptop_admin = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("admin"),
        Grant::allow_all(),
    );
    let prod_admin =
        authority.grant_in(prod.clone(), PrincipalId::new("admin"), Grant::allow_all());
    let intruder = authority.grant_in(prod.clone(), PrincipalId::new("intruder"), Grant::empty());
    let prod_deployer = authority.grant_in(
        prod.clone(),
        PrincipalId::new("deployer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/deploy/>")?),
    );

    let mut metrics = Metrics::default();

    let default_deliveries = Arc::new(AtomicUsize::new(0));
    let default_deliveries_for_handler = Arc::clone(&default_deliveries);
    bus.subscribe(&default_admin, pattern("gateway.changed")?, move |_| {
        default_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .map_err(|error| format!("default subscribe failed: {error}"))?;
    bus.publish(
        &default_admin,
        subject("gateway.changed")?,
        b"default-snapshot".to_vec(),
    )
    .await
    .map_err(|error| format!("default publish failed: {error}"))?;
    assert_eq_named(
        "default island publish deliveries",
        default_deliveries.load(Ordering::SeqCst),
        1,
    )?;
    metrics.authorized_publishes += 1;

    let laptop_deploys = Arc::new(AtomicUsize::new(0));
    let prod_deploys = Arc::new(AtomicUsize::new(0));
    let cross_island_deliveries = Arc::new(AtomicUsize::new(0));
    let laptop_deploys_for_handler = Arc::clone(&laptop_deploys);
    let laptop_cross_island_deliveries = Arc::clone(&cross_island_deliveries);
    bus.subscribe(&laptop_admin, pattern("deploy.submit")?, move |ctx| {
        if ctx.message.island().as_str() != "laptop" {
            laptop_cross_island_deliveries.fetch_add(1, Ordering::SeqCst);
        }
        laptop_deploys_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .map_err(|error| format!("laptop deploy subscribe failed: {error}"))?;
    let prod_deploys_for_handler = Arc::clone(&prod_deploys);
    let prod_cross_island_deliveries = Arc::clone(&cross_island_deliveries);
    bus.subscribe(&prod_admin, pattern("deploy.submit")?, move |ctx| {
        if ctx.message.island().as_str() != "prod" {
            prod_cross_island_deliveries.fetch_add(1, Ordering::SeqCst);
        }
        prod_deploys_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await
    .map_err(|error| format!("prod deploy subscribe failed: {error}"))?;
    bus.publish(
        &laptop_admin,
        subject("deploy.submit")?,
        b"laptop-deploy".to_vec(),
    )
    .await
    .map_err(|error| format!("laptop deploy publish failed: {error}"))?;
    assert_eq_named(
        "laptop deploy deliveries",
        laptop_deploys.load(Ordering::SeqCst),
        1,
    )?;
    assert_eq_named(
        "prod cross deliveries",
        prod_deploys.load(Ordering::SeqCst),
        0,
    )?;
    metrics.isolated_publishes += 1;

    let unauthorized_publish = bus
        .publish(&intruder, subject("deploy.submit")?, b"bad".to_vec())
        .await
        .unwrap_err();
    assert_error("unauthorized publish", &unauthorized_publish, |error| {
        matches!(
            error,
            BusError::UnauthorizedPublish {
                island,
                principal,
                subject,
            } if island.as_str() == "prod"
                && principal.as_str() == "intruder"
                && subject.as_str() == "deploy.submit"
        )
    })?;
    assert_eq_named(
        "prod deploys after denial",
        prod_deploys.load(Ordering::SeqCst),
        0,
    )?;
    metrics.denied_publish_attempts += 1;

    let prod_request_calls = Arc::new(AtomicUsize::new(0));
    let prod_request_calls_for_handler = Arc::clone(&prod_request_calls);
    bus.subscribe(&prod_admin, pattern("node.gpu.capacity")?, move |ctx| {
        prod_request_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"gpu-ready".to_vec())
    })
    .await
    .map_err(|error| format!("prod capacity subscribe failed: {error}"))?;
    let no_responders = bus
        .request(
            &laptop_admin,
            subject("node.gpu.capacity")?,
            Vec::new(),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
    assert_error("cross-island no responders", &no_responders, |error| {
        matches!(error, BusError::NoResponders { .. })
    })?;
    assert_eq_named(
        "prod request calls from laptop",
        prod_request_calls.load(Ordering::SeqCst),
        0,
    )?;
    metrics.no_responders += 1;

    let duplicate_response_error = Arc::new(Mutex::new(None));
    let duplicate_response_error_for_handler = Arc::clone(&duplicate_response_error);
    let (duplicate_recorded_tx, duplicate_recorded_rx) = mpsc::channel();
    bus.subscribe(&laptop_admin, pattern("node.local.inspect")?, move |ctx| {
        ctx.reply(b"ok".to_vec())?;
        let error = ctx.reply(b"second".to_vec()).unwrap_err();
        *duplicate_response_error_for_handler
            .lock()
            .map_err(|_| BusError::HandlerFailed {
                subject: String::from("node.local.inspect"),
                failure: HandlerFailure::LockPoisoned,
            })? = Some(error);
        let _ = duplicate_recorded_tx.send(());
        Ok(())
    })
    .await
    .map_err(|error| format!("laptop inspect subscribe failed: {error}"))?;
    let inspect = bus
        .request(
            &laptop_admin,
            subject("node.local.inspect")?,
            Vec::new(),
            Duration::from_secs(1),
        )
        .await
        .map_err(|error| format!("laptop inspect request failed: {error}"))?;
    assert_eq_named(
        "inspect reply payload",
        inspect.payload().as_bytes(),
        b"ok".as_slice(),
    )?;
    duplicate_recorded_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|error| format!("duplicate response error was not recorded: {error}"))?;
    let duplicate_error = duplicate_response_error
        .lock()
        .map_err(|_| String::from("duplicate response lock poisoned"))?
        .clone()
        .ok_or_else(|| String::from("duplicate response error was not captured"))?;
    assert_error("duplicate response", &duplicate_error, |error| {
        matches!(error, BusError::DuplicateResponse { .. })
    })?;
    metrics.response_scope_failures += 1;

    let laptop_queue_deliveries = Arc::new(AtomicUsize::new(0));
    let prod_queue_deliveries = Arc::new(AtomicUsize::new(0));
    let laptop_queue_deliveries_for_handler = Arc::clone(&laptop_queue_deliveries);
    bus.queue_subscribe(
        &laptop_admin,
        pattern("image.pull")?,
        "image-workers",
        move |ctx| {
            laptop_queue_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"laptop-worker".to_vec())
        },
    )
    .await
    .map_err(|error| format!("laptop queue subscribe failed: {error}"))?;
    let prod_queue_deliveries_for_handler = Arc::clone(&prod_queue_deliveries);
    bus.queue_subscribe(
        &prod_admin,
        pattern("image.pull")?,
        "image-workers",
        move |ctx| {
            prod_queue_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"prod-worker".to_vec())
        },
    )
    .await
    .map_err(|error| format!("prod queue subscribe failed: {error}"))?;
    let queue_reply = bus
        .request(
            &laptop_admin,
            subject("image.pull")?,
            b"image".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .map_err(|error| format!("laptop queue request failed: {error}"))?;
    assert_eq_named(
        "laptop queue reply",
        queue_reply.payload().as_bytes(),
        b"laptop-worker".as_slice(),
    )?;
    assert_eq_named(
        "laptop queue deliveries",
        laptop_queue_deliveries.load(Ordering::SeqCst),
        1,
    )?;
    assert_eq_named(
        "prod queue deliveries",
        prod_queue_deliveries.load(Ordering::SeqCst),
        0,
    )?;
    metrics.queue_deliveries += 1;
    metrics.remote_queue_deliveries = prod_queue_deliveries.load(Ordering::SeqCst);

    let write_outcome = bus
        .write_fact(
            &prod_deployer,
            fact_key("/facts/deploy/d1/plan")?,
            hash("b3:prod-plan"),
        )
        .await
        .map_err(|error| format!("prod fact write failed: {error}"))?;
    assert!(matches!(write_outcome, FactWriteOutcome::Inserted(_)));
    metrics.fact_writes += 1;

    let unauthorized_fact = bus
        .write_fact(
            &intruder,
            fact_key("/facts/deploy/d1/plan")?,
            hash("b3:bad-plan"),
        )
        .await
        .unwrap_err();
    assert_error("unauthorized fact write", &unauthorized_fact, |error| {
        matches!(
            error,
            BusError::UnauthorizedFactWrite {
                island,
                principal,
                key,
            } if island.as_str() == "prod"
                && principal.as_str() == "intruder"
                && key.as_str() == "/facts/deploy/d1/plan"
        )
    })?;
    metrics.denied_fact_writes += 1;

    let unauthorized_fact_read = bus
        .read_fact(&intruder, fact_key("/facts/deploy/d1/plan")?)
        .await
        .unwrap_err();
    assert_error("unauthorized fact read", &unauthorized_fact_read, |error| {
        matches!(
            error,
            BusError::UnauthorizedFactRead {
                island,
                principal,
                key,
            } if island.as_str() == "prod"
                && principal.as_str() == "intruder"
                && key.as_str() == "/facts/deploy/d1/plan"
        )
    })?;
    metrics.denied_fact_reads += 1;

    bus.write_fact(
        &laptop_admin,
        fact_key("/facts/deploy/d1/plan")?,
        hash("b3:laptop-plan"),
    )
    .await
    .map_err(|error| format!("laptop fact write failed: {error}"))?;
    metrics.fact_writes += 1;
    let prod_fact = bus
        .read_fact(&prod_admin, fact_key("/facts/deploy/d1/plan")?)
        .await
        .map_err(|error| format!("prod fact read failed: {error}"))?
        .ok_or_else(|| String::from("prod fact missing"))?;
    let laptop_fact = bus
        .read_fact(&laptop_admin, fact_key("/facts/deploy/d1/plan")?)
        .await
        .map_err(|error| format!("laptop fact read failed: {error}"))?
        .ok_or_else(|| String::from("laptop fact missing"))?;
    assert_eq_named(
        "prod fact hash after laptop write",
        prod_fact.content_hash().as_str(),
        "b3:prod-plan",
    )?;
    assert_eq_named(
        "laptop fact hash",
        laptop_fact.content_hash().as_str(),
        "b3:laptop-plan",
    )?;

    let revoked = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("revoked-publisher"),
        Grant::allow_all(),
    );
    assert!(authority.revoke(&revoked));
    let revoked_error = bus
        .publish(&revoked, subject("gateway.changed")?, b"revoked".to_vec())
        .await
        .unwrap_err();
    assert_error("revoked publish", &revoked_error, |error| {
        matches!(
            error,
            BusError::UnauthorizedPublish {
                island,
                principal,
                subject,
            } if island.as_str() == "laptop"
                && principal.as_str() == "revoked-publisher"
                && subject.as_str() == "gateway.changed"
        )
    })?;
    metrics.revocation_failures += 1;

    metrics.cross_island_delivery_count = cross_island_deliveries.load(Ordering::SeqCst);
    assert_eq_named(
        "cross island delivery count",
        metrics.cross_island_delivery_count,
        0,
    )?;

    write_metrics(&metrics, started)?;
    println!("PASS authority-contract");
    Ok(())
}

fn hash(value: &str) -> FactContentHash {
    FactContentHash::new(value)
}

fn write_metrics(metrics: &Metrics, started: Instant) -> Result<(), String> {
    let report = AuthorityContractReport {
        scenario: "authority-contract",
        authorized_publishes: metrics.authorized_publishes,
        isolated_publishes: metrics.isolated_publishes,
        denied_publish_attempts: metrics.denied_publish_attempts,
        denied_fact_writes: metrics.denied_fact_writes,
        denied_fact_reads: metrics.denied_fact_reads,
        revocation_failures: metrics.revocation_failures,
        fact_writes: metrics.fact_writes,
        no_responders: metrics.no_responders,
        response_scope_failures: metrics.response_scope_failures,
        queue_deliveries: metrics.queue_deliveries,
        remote_queue_deliveries: metrics.remote_queue_deliveries,
        cross_island_delivery_count: metrics.cross_island_delivery_count,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = scenario_dir("authority-contract").join("authority-contract-metrics.json");
    write_json(&path, &report).map(|_| ())
}
