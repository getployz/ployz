use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::MemoryBus;
use crate::{
    BridgeEndpoint, BridgeRuleId, BridgeRuleViolation, BridgeState, BusError, BusRuntimeConfig,
    BusSession, FactAccess, FactAuthorizer, FactContentHash, FactKey, FactKeyPattern, FactPayload,
    FactWriteOutcome, Grant, HandlerFailure, IslandId, Payload, PrincipalId, RequestManyPolicy,
    RequestTarget, ServiceImport, StreamImport, Subject, SubjectPattern, SubjectTransform,
};

fn island(name: &str) -> IslandId {
    IslandId::new(name)
}

fn principal(name: &str) -> PrincipalId {
    PrincipalId::new(name)
}

fn subject(value: &str) -> Subject {
    Subject::parse(value).expect("subject parses")
}

fn pattern(value: &str) -> SubjectPattern {
    SubjectPattern::parse(value).expect("pattern parses")
}

fn fact_key(value: &str) -> FactKey {
    FactKey::parse(value).expect("fact key parses")
}

fn fact_pattern(value: &str) -> FactKeyPattern {
    FactKeyPattern::parse(value).expect("fact key pattern parses")
}

fn hash(value: &str) -> FactContentHash {
    FactContentHash::new(value)
}

fn fact_payload(value: &str) -> FactPayload {
    value.to_string().into()
}

fn rule_id(value: &str) -> BridgeRuleId {
    BridgeRuleId::new(value)
}

fn bus_with_admin() -> (MemoryBus, BusSession) {
    let (bus, authority) = MemoryBus::new_with_authority();
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    (bus, admin)
}

#[test]
fn publish_fans_out_to_matching_subscribers() {
    let (bus, admin) = bus_with_admin();
    let received = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let received = Arc::clone(&received);
        bus.subscribe(&admin, pattern("node.*.status"), move |_| {
            received.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe");
    }

    bus.publish(&admin, subject("node.alpha.status"), b"up".to_vec())
        .expect("publish");

    assert_eq!(received.load(Ordering::SeqCst), 2);
}

#[test]
fn fanout_reuses_payload_storage_across_subscribers() {
    let (bus, admin) = bus_with_admin();
    let payload_ptrs = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..2 {
        let payload_ptrs = Arc::clone(&payload_ptrs);
        bus.subscribe(&admin, pattern("node.*.status"), move |ctx| {
            payload_ptrs
                .lock()
                .expect("payload ptr lock")
                .push(ctx.message.payload().as_bytes().as_ptr() as usize);
            Ok(())
        })
        .expect("subscribe");
    }

    bus.publish(
        &admin,
        subject("node.alpha.status"),
        Payload::from(vec![1, 2, 3, 4]),
    )
    .expect("publish");

    let payload_ptrs = payload_ptrs.lock().expect("payload ptr lock");
    assert_eq!(payload_ptrs.len(), 2);
    assert_eq!(payload_ptrs[0], payload_ptrs[1]);
}

#[test]
fn delivery_runtime_bounds_handler_concurrency() {
    let (bus, authority) =
        MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(2));
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    let active = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));

    for _ in 0..8 {
        let active = Arc::clone(&active);
        let completed = Arc::clone(&completed);
        let max_active = Arc::clone(&max_active);
        bus.subscribe(&admin, pattern("node.*.status"), move |_| {
            let active_now = active.fetch_add(1, Ordering::SeqCst) + 1;
            record_max(&max_active, active_now);
            std::thread::sleep(Duration::from_millis(10));
            active.fetch_sub(1, Ordering::SeqCst);
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("subscribe");
    }

    bus.publish(&admin, subject("node.alpha.status"), b"up".to_vec())
        .expect("publish");

    let snapshot = bus.runtime_snapshot();
    assert_eq!(completed.load(Ordering::SeqCst), 8);
    assert_eq!(snapshot.delivery_workers, 2);
    assert!(max_active.load(Ordering::SeqCst) <= 2);
    assert!(snapshot.max_active_deliveries <= 2);
}

#[test]
fn publish_until_times_out_when_delivery_queue_stays_full_and_records_pressure() {
    let config = BusRuntimeConfig::with_delivery_workers(1).with_delivery_queue_capacity(1);
    let (bus, authority) = MemoryBus::new_with_authority_and_config(config);
    let admin = authority.grant(principal("admin"), Grant::allow_all());

    for _ in 0..3 {
        bus.subscribe(&admin, pattern("gateway.changed"), move |_| {
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        })
        .expect("subscribe slow gateway handler");
    }

    let error = bus
        .publish_until(
            &admin,
            subject("gateway.changed"),
            Vec::new(),
            super::deadline_after(Duration::from_millis(5)),
        )
        .unwrap_err();
    let snapshot = bus.runtime_snapshot();

    assert!(matches!(error, BusError::Timeout { .. }));
    assert!(snapshot.max_queued_deliveries >= 1);
    assert!(snapshot.enqueue_full_count > 0);
    assert!(snapshot.enqueue_blocked_ns > 0);
}

#[test]
fn queue_publish_delivers_to_one_group_member() {
    let (bus, admin) = bus_with_admin();
    let received = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let received = Arc::clone(&received);
        bus.queue_subscribe(&admin, pattern("deploy.submit"), "schedulers", move |_| {
            received.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("queue subscribe");
    }

    bus.publish(&admin, subject("deploy.submit"), b"deploy".to_vec())
        .expect("publish");

    assert_eq!(received.load(Ordering::SeqCst), 1);
}

#[test]
fn queue_publish_delivers_once_per_queue_name_for_overlapping_patterns() {
    let (bus, admin) = bus_with_admin();
    let received = Arc::new(AtomicUsize::new(0));
    for pattern in ["deploy.*", "deploy.submit"] {
        let received = Arc::clone(&received);
        bus.queue_subscribe(&admin, self::pattern(pattern), "schedulers", move |_| {
            received.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("queue subscribe");
    }

    bus.publish(&admin, subject("deploy.submit"), b"deploy".to_vec())
        .expect("publish");

    assert_eq!(received.load(Ordering::SeqCst), 1);
}

#[test]
fn request_receives_reply() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), |ctx| {
        ctx.reply(b"ok".to_vec())
    })
    .expect("subscribe");

    let response = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .expect("request");

    assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
}

#[test]
fn request_reports_no_responders() {
    let (bus, admin) = bus_with_admin();

    let error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(10),
        )
        .unwrap_err();

    assert_eq!(
        error,
        BusError::NoResponders {
            target: Box::new(RequestTarget::Subject(subject("node.alpha.inspect"))),
        }
    );
}

#[test]
fn request_reports_timeout() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), |_ctx| {
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })
    .expect("subscribe");

    let error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(5),
        )
        .unwrap_err();

    assert_eq!(
        error,
        BusError::Timeout {
            subject: String::from("node.alpha.inspect")
        }
    );
}

#[test]
fn request_reports_handler_failure() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), |_ctx| {
        Err(BusError::HandlerFailed {
            subject: String::from("node.alpha.inspect"),
            failure: HandlerFailure::Application,
        })
    })
    .expect("subscribe");

    let error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert_eq!(
        error,
        BusError::HandlerFailed {
            subject: String::from("node.alpha.inspect"),
            failure: HandlerFailure::Application,
        }
    );
}

#[test]
fn request_many_can_target_pattern() {
    let (bus, admin) = bus_with_admin();
    for node in ["alpha", "beta"] {
        bus.subscribe(
            &admin,
            pattern(&format!("node.{node}.capacity")),
            move |ctx| ctx.reply(ctx.message.subject().as_str().as_bytes().to_vec()),
        )
        .expect("subscribe");
    }

    let mut replies = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.>")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .expect("request many");
    replies.sort_by(|left, right| left.payload().cmp(right.payload()));

    assert_eq!(replies.len(), 2);
    assert_eq!(
        replies[0].payload().as_bytes(),
        b"node.broadcast.capacity".as_slice()
    );
    assert_eq!(
        replies[1].payload().as_bytes(),
        b"node.broadcast.capacity".as_slice()
    );
}

#[test]
fn request_many_can_target_broad_subscriber_pattern() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.>"), |ctx| {
        ctx.reply(ctx.message.subject().as_str().as_bytes().to_vec())
    })
    .expect("subscribe");

    let replies = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .expect("request many");

    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].payload().as_bytes(),
        b"node.broadcast.capacity".as_slice()
    );
}

#[test]
fn request_many_excludes_queue_groups() {
    let (bus, admin) = bus_with_admin();
    bus.queue_subscribe(
        &admin,
        pattern("node.alpha.capacity"),
        "capacity-workers",
        |ctx| ctx.reply(b"queue".to_vec()),
    )
    .expect("queue subscribe");
    bus.subscribe(&admin, pattern("node.beta.capacity"), |ctx| {
        ctx.reply(b"normal".to_vec())
    })
    .expect("subscribe");

    let replies = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .expect("request many");

    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].payload().as_bytes(), b"normal".as_slice());
}

#[test]
fn request_many_authorizes_target_pattern() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let limited = authority.grant(
        principal("limited"),
        Grant::empty().with_publish(pattern("node.broadcast.capacity")),
    );

    let error = bus
        .request_many(
            &limited,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
}

#[test]
fn request_many_authorizes_operation_subject() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let limited = authority.grant(
        principal("limited"),
        Grant::empty().with_publish(pattern("node.*.capacity")),
    );

    let error = bus
        .request_many(
            &limited,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("deploy.submit"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
}

#[test]
fn request_many_reports_incomplete_responses() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.alpha.capacity"), |ctx| {
        ctx.reply(b"alpha".to_vec())
    })
    .expect("subscribe alpha");
    bus.subscribe(&admin, pattern("node.beta.capacity"), |_ctx| {
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })
    .expect("subscribe beta");

    let error = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_millis(5)),
        )
        .unwrap_err();

    assert_eq!(
        error,
        BusError::IncompleteResponses {
            target: Box::new(RequestTarget::Pattern(pattern("node.*.capacity"))),
            expected: 2,
            received: 1,
        }
    );
}

#[test]
fn request_many_zero_max_returns_without_dispatch() {
    let (bus, admin) = bus_with_admin();
    let called = Arc::new(AtomicUsize::new(0));
    let called_for_handler = Arc::clone(&called);
    bus.subscribe(&admin, pattern("node.alpha.capacity"), move |ctx| {
        called_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"capacity".to_vec())
    })
    .expect("subscribe");

    let replies = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(0, Duration::from_secs(1)),
        )
        .expect("request many");

    assert!(replies.is_empty());
    assert_eq!(called.load(Ordering::SeqCst), 0);
}

#[test]
fn request_many_zero_max_still_requires_authority() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let requester = authority.grant(principal("requester"), Grant::empty());

    let error = bus
        .request_many(
            &requester,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(0, Duration::from_secs(1)),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
}

#[test]
fn request_many_zero_max_still_respects_draining() {
    let (bus, admin) = bus_with_admin();
    bus.drain(&admin, Duration::from_secs(1)).expect("drain");

    let error = bus
        .request_many(
            &admin,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(0, Duration::from_secs(1)),
        )
        .unwrap_err();

    assert_eq!(error, BusError::Draining);
}

#[test]
fn unauthorized_publish_fails_before_handler_runs() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let subscriber = authority.grant(principal("subscriber"), Grant::allow_all());
    let publisher = authority.grant(principal("publisher"), Grant::empty());
    let received = Arc::new(AtomicUsize::new(0));
    let received_for_handler = Arc::clone(&received);
    bus.subscribe(&subscriber, pattern("node.alpha.status"), move |_| {
        received_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("subscribe");

    let error = bus
        .publish(&publisher, subject("node.alpha.status"), b"up".to_vec())
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[test]
fn unauthorized_request_fails_before_handler_runs() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let responder = authority.grant(principal("responder"), Grant::allow_all());
    let requester = authority.grant(principal("requester"), Grant::empty());
    let received = Arc::new(AtomicUsize::new(0));
    let received_for_handler = Arc::clone(&received);
    bus.subscribe(&responder, pattern("node.alpha.inspect"), move |ctx| {
        received_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"ok".to_vec())
    })
    .expect("subscribe");

    let error = bus
        .request(
            &requester,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(20),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedRequestTarget { .. }));
    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[test]
fn same_subject_publish_stays_inside_session_island() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
    let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    let laptop_deliveries = Arc::new(AtomicUsize::new(0));
    let prod_deliveries = Arc::new(AtomicUsize::new(0));
    let laptop_deliveries_for_handler = Arc::clone(&laptop_deliveries);
    let prod_deliveries_for_handler = Arc::clone(&prod_deliveries);
    bus.subscribe(&laptop, pattern("deploy.submit"), move |ctx| {
        assert_eq!(ctx.message.island().as_str(), "laptop");
        laptop_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("subscribe laptop");
    bus.subscribe(&prod, pattern("deploy.submit"), move |ctx| {
        assert_eq!(ctx.message.island().as_str(), "prod");
        prod_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("subscribe prod");

    bus.publish(&laptop, subject("deploy.submit"), b"from-laptop".to_vec())
        .expect("publish laptop");

    assert_eq!(laptop_deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(prod_deliveries.load(Ordering::SeqCst), 0);
}

#[test]
fn request_does_not_use_cross_island_responders() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
    let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    let prod_handler_calls = Arc::new(AtomicUsize::new(0));
    let prod_handler_calls_for_handler = Arc::clone(&prod_handler_calls);
    bus.subscribe(&prod, pattern("deploy.submit"), move |ctx| {
        prod_handler_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"prod".to_vec())
    })
    .expect("subscribe prod responder");

    let error = bus
        .request(
            &laptop,
            subject("deploy.submit"),
            Vec::new(),
            Duration::from_millis(20),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::NoResponders { .. }));
    assert_eq!(prod_handler_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn queue_groups_are_island_scoped_for_publish_and_request() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let island_a = island("island-a");
    let admin_a = authority.grant_in(island_a.clone(), principal("admin"), Grant::allow_all());
    let admin_b = authority.grant_in(island("island-b"), principal("admin"), Grant::allow_all());
    let island_a_calls = Arc::new(AtomicUsize::new(0));
    let island_b_calls = Arc::new(AtomicUsize::new(0));

    let island_a_calls_for_handler = Arc::clone(&island_a_calls);
    bus.queue_subscribe(&admin_a, pattern("image.pull"), "workers", move |ctx| {
        island_a_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        let _ = ctx.reply(b"island-a".to_vec());
        Ok(())
    })
    .expect("island-a queue subscriber");
    let island_b_calls_for_handler = Arc::clone(&island_b_calls);
    bus.queue_subscribe(&admin_b, pattern("image.pull"), "workers", move |ctx| {
        island_b_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        let _ = ctx.reply(b"island-b".to_vec());
        Ok(())
    })
    .expect("island-b queue subscriber");

    bus.publish(&admin_a, subject("image.pull"), b"pull".to_vec())
        .expect("publish in island-a");
    assert_eq!(island_a_calls.load(Ordering::SeqCst), 1);
    assert_eq!(island_b_calls.load(Ordering::SeqCst), 0);

    let response = bus
        .request(
            &admin_a,
            subject("image.pull"),
            b"pull".to_vec(),
            Duration::from_secs(1),
        )
        .expect("request in island-a");
    assert_eq!(response.island(), &island_a);
    assert_eq!(response.payload().as_bytes(), b"island-a".as_slice());
    assert_eq!(island_a_calls.load(Ordering::SeqCst), 2);
    assert_eq!(island_b_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn revoking_grant_blocks_future_publish_before_dispatch() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    let publisher = authority.grant(principal("publisher"), Grant::allow_all());
    let received = Arc::new(AtomicUsize::new(0));
    let received_for_handler = Arc::clone(&received);
    bus.subscribe(&admin, pattern("gateway.changed"), move |_| {
        received_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("subscribe");
    assert!(authority.revoke(&publisher));

    let error = bus
        .publish(&publisher, subject("gateway.changed"), Vec::new())
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedPublish { .. }));
    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[test]
fn authorization_error_carries_island_and_principal_context() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = authority.grant_in(island("laptop"), principal("writer"), Grant::empty());

    let error = bus
        .publish(&laptop, subject("gateway.changed"), Vec::new())
        .unwrap_err();

    assert!(matches!(
        error,
        BusError::UnauthorizedPublish {
            island,
            principal,
            ..
        } if island.as_str() == "laptop" && principal.as_str() == "writer"
    ));
}

#[test]
fn allowed_fact_write_inserts_fact_in_session_island() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("deployer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/deploy/>"))
            .with_fact_read(fact_pattern("/facts/deploy/>")),
    );

    let outcome = bus
        .write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:plan"))
        .expect("write fact");
    let fact = bus
        .read_fact(&writer, &fact_key("/facts/deploy/d1/plan"))
        .expect("read fact")
        .expect("fact exists");

    assert!(matches!(outcome, FactWriteOutcome::Inserted(_)));
    assert_eq!(fact.island().as_str(), "prod");
    assert_eq!(fact.author().as_str(), "deployer");
    assert_eq!(fact.content_hash().as_str(), "b3:plan");
}

#[test]
fn allowed_payload_fact_write_stores_body_and_derived_hash() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let writer = authority.grant_in(
        island("prod"),
        principal("deployer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/routes/>"))
            .with_fact_read(fact_pattern("/facts/routes/>")),
    );
    let payload = fact_payload("{\"route\":\"web\"}");
    let expected_hash = FactContentHash::for_payload(&payload);

    let outcome = bus
        .write_fact_payload(&writer, fact_key("/facts/routes/r1"), payload.clone())
        .expect("write payload fact");
    let fact = bus
        .read_fact(&writer, &fact_key("/facts/routes/r1"))
        .expect("read fact")
        .expect("fact exists");
    let body = bus
        .read_fact_payload(&writer, &fact_key("/facts/routes/r1"))
        .expect("read fact payload")
        .expect("payload exists");

    assert!(matches!(outcome, FactWriteOutcome::Inserted(_)));
    assert_eq!(fact.content_hash(), &expected_hash);
    assert_eq!(body, payload);
}

#[test]
fn hash_only_fact_cannot_read_payload_from_another_identity() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let prod = authority.grant_in(
        island("prod"),
        principal("prod-writer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/routes/>"))
            .with_fact_read(fact_pattern("/facts/routes/>")),
    );
    let laptop = authority.grant_in(
        island("laptop"),
        principal("laptop-writer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/routes/>"))
            .with_fact_read(fact_pattern("/facts/routes/>")),
    );
    let payload = fact_payload("{\"route\":\"secret\"}");
    let hash = FactContentHash::for_payload(&payload);

    bus.write_fact_payload(&prod, fact_key("/facts/routes/prod"), payload)
        .expect("write prod payload fact");
    bus.write_fact(&laptop, fact_key("/facts/routes/local"), hash)
        .expect("write laptop hash-only fact");

    assert!(
        bus.read_fact_payload(&laptop, &fact_key("/facts/routes/local"))
            .expect("read laptop payload")
            .is_none()
    );
}

#[test]
fn denied_fact_write_fails_before_mutation() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let reader = authority.grant_in(island("prod"), principal("reader"), Grant::allow_all());
    let writer = authority.grant_in(
        island("prod"),
        principal("deployer"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/deploy/>"))
            .with_fact_write_deny(fact_pattern("/facts/deploy/*/secret")),
    );

    let error = bus
        .write_fact(
            &writer,
            fact_key("/facts/deploy/d1/secret"),
            hash("b3:secret"),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedFactWrite { .. }));
    assert!(
        bus.read_fact(&reader, &fact_key("/facts/deploy/d1/secret"))
            .expect("read secret")
            .is_none()
    );
}

#[test]
fn fact_authorizer_matches_bus_fact_read_and_write_grants() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let session = authority.grant_in(
        island("prod"),
        principal("projection"),
        Grant::empty()
            .with_fact_read(fact_pattern("/facts/routes/>"))
            .with_fact_read_deny(fact_pattern("/facts/routes/secret"))
            .with_fact_write(fact_pattern("/facts/deploy/>"))
            .with_fact_write_deny(fact_pattern("/facts/deploy/*/secret")),
    );

    assert!(bus.can_session_access_fact(
        &session,
        &fact_key("/facts/routes/public"),
        FactAccess::Read
    ));
    assert!(!bus.can_session_access_fact(
        &session,
        &fact_key("/facts/routes/secret"),
        FactAccess::Read
    ));
    assert!(bus.can_session_access_fact(
        &session,
        &fact_key("/facts/deploy/d1/plan"),
        FactAccess::Write
    ));
    assert!(!bus.can_session_access_fact(
        &session,
        &fact_key("/facts/deploy/d1/secret"),
        FactAccess::Write
    ));
    assert!(bus.can_principal_access_fact(
        session.island(),
        session.principal(),
        &fact_key("/facts/deploy/d1/plan"),
        FactAccess::Write
    ));
}

#[test]
fn fact_reads_are_island_scoped_through_bus() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
    let prod = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    bus.write_fact(&prod, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
        .expect("write prod fact");
    bus.write_fact(
        &laptop,
        fact_key("/facts/deploy/d1/plan"),
        hash("b3:laptop"),
    )
    .expect("write laptop fact");

    let prod_fact = bus
        .read_fact(&prod, &fact_key("/facts/deploy/d1/plan"))
        .expect("read prod")
        .expect("prod fact exists");
    let laptop_fact = bus
        .read_fact(&laptop, &fact_key("/facts/deploy/d1/plan"))
        .expect("read laptop")
        .expect("laptop fact exists");

    assert_eq!(prod_fact.content_hash().as_str(), "b3:prod");
    assert_eq!(laptop_fact.content_hash().as_str(), "b3:laptop");
}

#[test]
fn fact_listing_is_deterministic_authorized_and_island_scoped() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let prod_admin = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    let laptop_admin = authority.grant_in(island("laptop"), principal("admin"), Grant::allow_all());
    let prod_reader = authority.grant_in(
        island("prod"),
        principal("reader"),
        Grant::empty().with_fact_read(fact_pattern("/facts/service/>")),
    );
    let intruder = authority.grant_in(island("prod"), principal("intruder"), Grant::empty());
    bus.write_fact(
        &prod_admin,
        fact_key("/facts/service/web/node-2"),
        hash("b3:node-2"),
    )
    .expect("write prod fact");
    bus.write_fact(
        &prod_admin,
        fact_key("/facts/deploy/d1/plan"),
        hash("b3:deploy"),
    )
    .expect("write prod deploy fact");
    bus.write_fact(
        &laptop_admin,
        fact_key("/facts/service/web/node-1"),
        hash("b3:laptop"),
    )
    .expect("write laptop fact");
    bus.write_fact(
        &prod_admin,
        fact_key("/facts/service/web/node-1"),
        hash("b3:node-1"),
    )
    .expect("write prod fact");

    let listed = bus
        .list_facts(&prod_reader, &fact_pattern("/facts/>"))
        .expect("list prod facts");
    let keys = listed
        .iter()
        .map(|fact| fact.key().as_str())
        .collect::<Vec<_>>();
    let denied = bus
        .list_facts(&intruder, &fact_pattern("/facts/>"))
        .expect("list denied facts");

    assert_eq!(
        keys,
        vec!["/facts/service/web/node-1", "/facts/service/web/node-2"]
    );
    assert!(denied.is_empty());
}

#[test]
fn fact_read_requires_active_read_grant() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let admin = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    let intruder = authority.grant_in(island("prod"), principal("intruder"), Grant::empty());
    bus.write_fact(&admin, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
        .expect("write prod fact");

    let error = bus
        .read_fact(&intruder, &fact_key("/facts/deploy/d1/plan"))
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedFactRead { .. }));
}

