use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{
    BridgeEndpoint, BridgeFailure, BridgeRuleId, BridgeState, BusActorHandle, BusError,
    FactContentHash, Grant, IslandId, PrincipalId, ServiceImport, StreamImport, SubjectTransform,
};
use serde::Serialize;

use crate::assertions::{assert_eq_named, assert_error};
use crate::bus_syntax::{fact_key, fact_pattern, pattern, subject};
use crate::metrics::{scenario_dir, write_json};

#[derive(Debug, Default)]
struct Metrics {
    bridged_service_requests: usize,
    bridged_service_responses: usize,
    bridged_stream_deliveries: usize,
    denied_bridge_attempts: usize,
    bridge_outage_failures: usize,
    direct_prod_fact_write_denials: usize,
    cross_island_leakage_count: usize,
}

#[derive(Debug, Serialize)]
struct BridgeContractReport {
    scenario: &'static str,
    bridged_service_requests: usize,
    bridged_service_responses: usize,
    bridged_stream_deliveries: usize,
    denied_bridge_attempts: usize,
    bridge_outage_failures: usize,
    direct_prod_fact_write_denials: usize,
    cross_island_leakage_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for bridge contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let (bus, authority) = BusActorHandle::new_with_authority();
    let laptop = IslandId::new("laptop");
    let prod = IslandId::new("prod");
    let bridge_rule = BridgeRuleId::new("gpu-deploy");
    let status_rule = BridgeRuleId::new("prod-status");

    let laptop_user = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("user"),
        Grant::empty()
            .with_publish(pattern("gpu.deploy.submit")?)
            .with_fact_write(fact_pattern("/facts/deploy/>")?)
            .with_fact_read(fact_pattern("/facts/deploy/>")?),
    );
    let laptop_reader = authority.grant_in(
        laptop.clone(),
        PrincipalId::new("reader"),
        Grant::empty().with_subscribe(pattern("prod.deploy.*.status")?),
    );
    let prod_admin =
        authority.grant_in(prod.clone(), PrincipalId::new("admin"), Grant::allow_all());
    let prod_bridge = authority.grant_in(
        prod.clone(),
        PrincipalId::new("bridge"),
        Grant::empty()
            .with_publish(pattern("deploy.submit")?)
            .with_bridge_export(pattern("deploy.*.status")?),
    );
    authority.grant_in(
        laptop.clone(),
        PrincipalId::new("bridge"),
        Grant::empty().with_publish(pattern("prod.deploy.*.status")?),
    );
    let prod_scheduler = authority.grant_in(
        prod.clone(),
        PrincipalId::new("scheduler"),
        Grant::empty()
            .with_queue(pattern("deploy.submit")?, "schedulers")
            .with_response(),
    );
    let prod_intruder = authority.grant_in(
        prod.clone(),
        PrincipalId::new("laptop-user"),
        Grant::empty(),
    );

    authority
        .add_service_import(ServiceImport::new(
            bridge_rule.clone(),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")?),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")?),
            prod_bridge.principal().clone(),
        ))
        .map_err(|error| format!("add service import: {error}"))?;
    authority
        .add_stream_import(StreamImport::new(
            status_rule.clone(),
            prod.clone(),
            laptop.clone(),
            prod_bridge.principal().clone(),
            SubjectTransform::new(
                pattern("deploy.*.status")?,
                pattern("prod.deploy.*.status")?,
            )
            .map_err(|error| format!("create status transform: {error}"))?,
        ))
        .map_err(|error| format!("add stream import: {error}"))?;

    let mut metrics = Metrics::default();
    let prod_scheduler_calls = Arc::new(AtomicUsize::new(0));
    let prod_scheduler_calls_for_handler = Arc::clone(&prod_scheduler_calls);
    bus.queue_subscribe(
        &prod_scheduler,
        pattern("deploy.submit")?,
        "schedulers",
        move |ctx| {
            let Some(origin) = ctx.message.bridge_origin() else {
                return Err(BusError::HandlerFailed {
                    subject: String::from("deploy.submit"),
                    failure: mvp_bus::HandlerFailure::Application,
                });
            };
            if ctx.message.island().as_str() != "prod"
                || ctx.message.principal().as_str() != "bridge"
                || ctx.message.subject().as_str() != "deploy.submit"
                || origin.rule_id().as_str() != "gpu-deploy"
                || origin.source_island().as_str() != "laptop"
                || origin.source_principal().as_str() != "user"
                || origin.original_subject().as_str() != "gpu.deploy.submit"
            {
                return Err(BusError::HandlerFailed {
                    subject: String::from("deploy.submit"),
                    failure: mvp_bus::HandlerFailure::Application,
                });
            }
            prod_scheduler_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"prod-accepted".to_vec())
        },
    )
    .await
    .map_err(|error| format!("prod scheduler subscribe: {error}"))?;

    let deploy_response = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit")?,
            b"manifest".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .map_err(|error| format!("bridged deploy request failed: {error}"))?;
    assert_eq_named(
        "bridged deploy response",
        deploy_response.payload().as_bytes(),
        b"prod-accepted".as_slice(),
    )?;
    assert_eq_named(
        "prod scheduler calls",
        prod_scheduler_calls.load(Ordering::SeqCst),
        1,
    )?;
    assert_eq_named("response island", deploy_response.island(), &prod)?;
    assert_eq_named(
        "response principal",
        deploy_response.responder().as_str(),
        "scheduler",
    )?;
    metrics.bridged_service_requests += 1;
    metrics.bridged_service_responses += 1;

    let status_deliveries = Arc::new(AtomicUsize::new(0));
    let cross_island_leaks = Arc::new(AtomicUsize::new(0));
    let observed_origin = Arc::new(Mutex::new(None));
    let status_deliveries_for_handler = Arc::clone(&status_deliveries);
    let cross_island_leaks_for_handler = Arc::clone(&cross_island_leaks);
    let observed_origin_for_handler = Arc::clone(&observed_origin);
    bus.subscribe(
        &laptop_reader,
        pattern("prod.deploy.*.status")?,
        move |ctx| {
            status_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
            if ctx.message.island().as_str() != "laptop"
                || ctx.message.principal().as_str() != "bridge"
                || ctx.message.subject().as_str() != "prod.deploy.d1.status"
            {
                cross_island_leaks_for_handler.fetch_add(1, Ordering::SeqCst);
            }
            *observed_origin_for_handler
                .lock()
                .map_err(|_| BusError::HandlerFailed {
                    subject: String::from("prod.deploy.d1.status"),
                    failure: mvp_bus::HandlerFailure::LockPoisoned,
                })? = ctx.message.bridge_origin().cloned();
            Ok(())
        },
    )
    .await
    .map_err(|error| format!("laptop status subscribe: {error}"))?;

    bus.publish(
        &prod_admin,
        subject("deploy.d1.status")?,
        b"running".to_vec(),
    )
    .await
    .map_err(|error| format!("prod status publish failed: {error}"))?;
    assert_eq_named(
        "status deliveries",
        status_deliveries.load(Ordering::SeqCst),
        1,
    )?;
    let origin = observed_origin
        .lock()
        .map_err(|_| String::from("origin lock poisoned"))?
        .clone()
        .ok_or_else(|| String::from("imported status did not carry bridge origin"))?;
    assert_eq_named(
        "status origin rule",
        origin.rule_id().as_str(),
        "prod-status",
    )?;
    assert_eq_named("status origin island", origin.source_island(), &prod)?;
    assert_eq_named(
        "status origin principal",
        origin.source_principal().as_str(),
        "admin",
    )?;
    assert_eq_named(
        "status origin subject",
        origin.original_subject().as_str(),
        "deploy.d1.status",
    )?;
    metrics.bridged_stream_deliveries += 1;

    bus.publish(
        &prod_admin,
        subject("deploy.d1.internal")?,
        b"private".to_vec(),
    )
    .await
    .map_err(|error| format!("prod non-exported publish failed: {error}"))?;
    assert_eq_named(
        "non-exported deliveries",
        status_deliveries.load(Ordering::SeqCst),
        1,
    )?;
    let fact_write_denial = bus
        .write_fact(
            &prod_intruder,
            fact_key("/facts/deploy/d1/plan")?,
            hash("b3:intruder"),
        )
        .await
        .unwrap_err();
    assert_error(
        "direct prod fact write denial",
        &fact_write_denial,
        |error| {
            matches!(
                error,
                BusError::UnauthorizedFactWrite { island, principal, key }
                    if island.as_str() == "prod"
                        && principal.as_str() == "laptop-user"
                        && key.as_str() == "/facts/deploy/d1/plan"
            )
        },
    )?;
    metrics.direct_prod_fact_write_denials += 1;

    authority
        .set_service_import_state(&bridge_rule, BridgeState::Disabled)
        .map_err(|error| format!("disable service import: {error}"))?;
    let disabled_error = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit")?,
            b"manifest".to_vec(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
    assert_error("disabled bridge failure", &disabled_error, |error| {
        matches!(
            error,
            BusError::BridgeUnavailable {
                failure: BridgeFailure::Disabled,
                ..
            }
        )
    })?;
    assert_eq_named(
        "prod scheduler calls after disabled bridge",
        prod_scheduler_calls.load(Ordering::SeqCst),
        1,
    )?;
    metrics.bridge_outage_failures += 1;
    metrics.denied_bridge_attempts += 1;

    metrics.cross_island_leakage_count = cross_island_leaks.load(Ordering::SeqCst);
    assert_eq_named(
        "cross island leakage count",
        metrics.cross_island_leakage_count,
        0,
    )?;

    write_metrics(&metrics, started)?;
    println!("PASS bridge-contract");
    Ok(())
}

fn hash(value: &str) -> FactContentHash {
    FactContentHash::new(value)
}

fn write_metrics(metrics: &Metrics, started: Instant) -> Result<(), String> {
    let report = BridgeContractReport {
        scenario: "bridge-contract",
        bridged_service_requests: metrics.bridged_service_requests,
        bridged_service_responses: metrics.bridged_service_responses,
        bridged_stream_deliveries: metrics.bridged_stream_deliveries,
        denied_bridge_attempts: metrics.denied_bridge_attempts,
        bridge_outage_failures: metrics.bridge_outage_failures,
        direct_prod_fact_write_denials: metrics.direct_prod_fact_write_denials,
        cross_island_leakage_count: metrics.cross_island_leakage_count,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let path = scenario_dir("bridge-contract").join("bridge-contract-metrics.json");
    write_json(&path, &report).map(|_| ())
}
