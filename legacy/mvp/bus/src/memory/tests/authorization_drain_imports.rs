#[test]
fn revoked_session_cannot_read_facts() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let admin = authority.grant_in(island("prod"), principal("admin"), Grant::allow_all());
    let reader = authority.grant_in(island("prod"), principal("reader"), Grant::allow_all());
    bus.write_fact(&admin, fact_key("/facts/deploy/d1/plan"), hash("b3:prod"))
        .expect("write prod fact");
    assert!(authority.revoke(&reader));

    let error = bus
        .read_fact(&reader, &fact_key("/facts/deploy/d1/plan"))
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedFactRead { .. }));
}

#[test]
fn fact_write_conflict_is_a_stored_candidate() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let writer = authority.grant(principal("admin"), Grant::allow_all());
    bus.write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:first"))
        .expect("write fact");

    let repeated = bus
        .write_fact(&writer, fact_key("/facts/deploy/d1/plan"), hash("b3:first"))
        .expect("repeat fact");
    let conflict = bus
        .write_fact(
            &writer,
            fact_key("/facts/deploy/d1/plan"),
            hash("b3:second"),
        )
        .expect("write conflicting fact candidate");

    assert!(matches!(repeated, FactWriteOutcome::AlreadyPresent(_)));
    assert!(matches!(conflict, FactWriteOutcome::Conflict(_)));
    assert_eq!(
        bus.list_facts(&writer, &fact_pattern("/facts/deploy/>"))
            .expect("list facts")
            .len(),
        2
    );
    assert!(
        bus.read_fact(&writer, &fact_key("/facts/deploy/d1/plan"))
            .expect("read conflicted fact")
            .is_none()
    );
}

#[test]
fn unauthorized_subscribe_fails_before_registration() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let subscriber = authority.grant(principal("subscriber"), Grant::empty());

    let error = bus
        .subscribe(&subscriber, pattern("node.alpha.status"), |_| Ok(()))
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedSubscribe { .. }));
}

#[test]
fn unauthorized_queue_subscribe_fails_before_registration() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let subscriber = authority.grant(
        principal("subscriber"),
        Grant::empty().with_subscribe(pattern("deploy.submit")),
    );

    let error = bus
        .queue_subscribe(&subscriber, pattern("deploy.submit"), "schedulers", |_| {
            Ok(())
        })
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedQueue { .. }));
}

#[test]
fn unauthorized_queue_name_fails_before_registration() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let subscriber = authority.grant(
        principal("subscriber"),
        Grant::empty()
            .with_subscribe(pattern("deploy.submit"))
            .with_queue(pattern("deploy.submit"), "schedulers"),
    );

    let error = bus
        .queue_subscribe(
            &subscriber,
            pattern("deploy.submit"),
            "privileged-schedulers",
            |_| Ok(()),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedQueue { .. }));
}

#[test]
fn unauthorized_response_does_not_reach_requester() {
    let (bus, admin) = bus_with_admin();
    let response_error = Arc::new(Mutex::new(None));
    let response_error_for_handler = Arc::clone(&response_error);
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
        let intruder = principal("intruder");
        let error = ctx.reply_as(&intruder, b"bad".to_vec()).unwrap_err();
        *response_error_for_handler
            .lock()
            .expect("lock response error") = Some(error);
        Ok(())
    })
    .expect("subscribe");

    let request_error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(20),
        )
        .unwrap_err();

    assert!(matches!(request_error, BusError::Timeout { .. }));
    assert!(matches!(
        response_error.lock().expect("lock response error").as_ref(),
        Some(BusError::UnauthorizedResponse { .. })
    ));
}

#[test]
fn reply_permit_is_one_use() {
    let (bus, admin) = bus_with_admin();
    let duplicate_error = Arc::new(Mutex::new(None));
    let duplicate_error_for_handler = Arc::clone(&duplicate_error);
    let (duplicate_recorded_tx, duplicate_recorded_rx) = mpsc::channel();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
        ctx.reply(b"first".to_vec())?;
        let error = ctx.reply(b"second".to_vec()).unwrap_err();
        *duplicate_error_for_handler
            .lock()
            .expect("lock duplicate error") = Some(error);
        let _ = duplicate_recorded_tx.send(());
        Ok(())
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

    assert_eq!(response.payload().as_bytes(), b"first".as_slice());
    duplicate_recorded_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("duplicate response recorded");
    assert!(matches!(
        duplicate_error
            .lock()
            .expect("lock duplicate error")
            .as_ref(),
        Some(BusError::DuplicateResponse { .. })
    ));
}

#[test]
fn reply_permit_expires_with_request_deadline() {
    let (bus, admin) = bus_with_admin();
    let expired_error = Arc::new(Mutex::new(None));
    let expired_error_for_handler = Arc::clone(&expired_error);
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
        std::thread::sleep(Duration::from_millis(30));
        let error = ctx.reply(b"late".to_vec()).unwrap_err();
        *expired_error_for_handler
            .lock()
            .expect("lock expired error") = Some(error);
        Ok(())
    })
    .expect("subscribe");

    let request_error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(5),
        )
        .unwrap_err();
    std::thread::sleep(Duration::from_millis(40));

    assert!(matches!(request_error, BusError::Timeout { .. }));
    assert!(matches!(
        expired_error.lock().expect("lock expired error").as_ref(),
        Some(BusError::UnauthorizedResponse { .. })
    ));
}

#[test]
fn reply_permit_deadline_includes_delivery_queue_wait() {
    let (bus, authority) =
        MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(1));
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    let (blocker_started_tx, blocker_started_rx) = mpsc::channel();
    bus.subscribe(&admin, pattern("node.blocker"), move |_| {
        let _ = blocker_started_tx.send(());
        std::thread::sleep(Duration::from_millis(40));
        Ok(())
    })
    .expect("subscribe blocker");

    let reply_error = Arc::new(Mutex::new(None));
    let reply_error_for_handler = Arc::clone(&reply_error);
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
        let error = ctx.reply(b"late".to_vec()).unwrap_err();
        *reply_error_for_handler.lock().expect("reply error lock") = Some(error);
        Ok(())
    })
    .expect("subscribe inspect");

    let bus_for_publish = bus.clone();
    let admin_for_publish = admin.clone();
    let publish = std::thread::spawn(move || {
        bus_for_publish.publish(&admin_for_publish, subject("node.blocker"), Vec::new())
    });
    blocker_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("blocker started");

    let request_error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(5),
        )
        .unwrap_err();
    publish
        .join()
        .expect("publish thread joins")
        .expect("publish");
    std::thread::sleep(Duration::from_millis(20));

    assert!(matches!(request_error, BusError::Timeout { .. }));
    assert!(matches!(
        reply_error.lock().expect("reply error lock").as_ref(),
        Some(BusError::UnauthorizedResponse { .. })
    ));
}

#[test]
fn revoked_subscriber_is_skipped_at_dispatch() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    let subscriber_id = principal("subscriber");
    let subscriber = authority.grant(subscriber_id.clone(), Grant::allow_all());
    let received = Arc::new(AtomicUsize::new(0));
    let received_for_handler = Arc::clone(&received);
    bus.subscribe(&subscriber, pattern("node.alpha.status"), move |_| {
        received_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("subscribe");
    authority.grant(subscriber_id, Grant::empty());

    bus.publish(&admin, subject("node.alpha.status"), Vec::new())
        .expect("publish");

    assert_eq!(received.load(Ordering::SeqCst), 0);
}

#[test]
fn unauthorized_drain_fails() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let limited = authority.grant(
        principal("limited"),
        Grant::empty().with_publish(pattern(">")),
    );

    let error = bus.drain(&limited, Duration::from_secs(1)).unwrap_err();

    assert!(matches!(error, BusError::UnauthorizedDrain { .. }));
}

#[test]
fn drain_rejects_new_work() {
    let (bus, admin) = bus_with_admin();
    bus.drain(&admin, Duration::from_secs(1)).expect("drain");

    let error = bus
        .publish(&admin, subject("node.alpha.status"), Vec::new())
        .unwrap_err();

    assert_eq!(error, BusError::Draining);
}

#[test]
fn drain_rejects_new_request() {
    let (bus, admin) = bus_with_admin();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), |ctx| {
        ctx.reply(b"ok".to_vec())
    })
    .expect("subscribe");
    bus.drain(&admin, Duration::from_secs(1)).expect("drain");

    let error = bus
        .request(
            &admin,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(10),
        )
        .unwrap_err();

    assert_eq!(error, BusError::Draining);
}

#[test]
fn drain_waits_for_inflight_work() {
    let (bus, admin) = bus_with_admin();
    let completed = Arc::new(AtomicUsize::new(0));
    let completed_for_handler = Arc::clone(&completed);
    let (started_tx, started_rx) = mpsc::channel();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |ctx| {
        let _ = started_tx.send(());
        std::thread::sleep(Duration::from_millis(20));
        completed_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"ok".to_vec())
    })
    .expect("subscribe");

    let bus_for_request = bus.clone();
    let admin_for_request = admin.clone();
    let request = std::thread::spawn(move || {
        bus_for_request.request(
            &admin_for_request,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_secs(1),
        )
    });
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("handler started");

    bus.drain(&admin, Duration::from_secs(1)).expect("drain");

    let response = request
        .join()
        .expect("request thread joins")
        .expect("request");
    assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
    assert_eq!(completed.load(Ordering::SeqCst), 1);
}

#[test]
fn drain_waits_for_queued_delivery_work() {
    let (bus, authority) =
        MemoryBus::new_with_authority_and_config(BusRuntimeConfig::with_delivery_workers(1));
    let admin = authority.grant(principal("admin"), Grant::allow_all());
    let completed = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel();
    for index in 0..3 {
        let completed_for_handler = Arc::clone(&completed);
        let started_tx = started_tx.clone();
        bus.subscribe(&admin, pattern("node.*.capacity"), move |ctx| {
            if index == 0 {
                let _ = started_tx.send(());
            }
            std::thread::sleep(Duration::from_millis(10));
            completed_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"ok".to_vec())
        })
        .expect("subscribe");
    }
    drop(started_tx);

    let bus_for_request = bus.clone();
    let admin_for_request = admin.clone();
    let request = std::thread::spawn(move || {
        bus_for_request.request_many(
            &admin_for_request,
            RequestTarget::Pattern(pattern("node.*.capacity")),
            subject("node.broadcast.capacity"),
            Vec::new(),
            RequestManyPolicy::new(8, Duration::from_secs(1)),
        )
    });
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("first queued handler started");

    bus.drain(&admin, Duration::from_secs(1)).expect("drain");

    let replies = request
        .join()
        .expect("request thread joins")
        .expect("request many");
    assert_eq!(replies.len(), 3);
    assert_eq!(completed.load(Ordering::SeqCst), 3);
}

#[test]
fn drain_times_out_waiting_for_inflight_work() {
    let (bus, admin) = bus_with_admin();
    let (started_tx, started_rx) = mpsc::channel();
    bus.subscribe(&admin, pattern("node.alpha.inspect"), move |_ctx| {
        let _ = started_tx.send(());
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    })
    .expect("subscribe");

    let bus_for_request = bus.clone();
    let admin_for_request = admin.clone();
    let request = std::thread::spawn(move || {
        bus_for_request.request(
            &admin_for_request,
            subject("node.alpha.inspect"),
            Vec::new(),
            Duration::from_millis(100),
        )
    });
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("handler started");

    let error = bus.drain(&admin, Duration::from_millis(1)).unwrap_err();

    assert_eq!(
        error,
        BusError::Timeout {
            subject: String::from("drain"),
        }
    );
    assert!(request.join().expect("request thread joins").is_err());
}

#[test]
fn service_import_routes_request_to_remote_island() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    let prod_scheduler = authority.grant_in(
        prod.clone(),
        principal("scheduler"),
        Grant::empty()
            .with_queue(pattern("deploy.submit"), "schedulers")
            .with_response(),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let observed = Arc::new(Mutex::new(None));
    let observed_for_handler = Arc::clone(&observed);
    bus.queue_subscribe(
        &prod_scheduler,
        pattern("deploy.submit"),
        "schedulers",
        move |ctx| {
            *observed_for_handler.lock().expect("lock observed") = Some((
                ctx.message.island().clone(),
                ctx.message.principal().clone(),
                ctx.message.subject().clone(),
                ctx.message.bridge_origin().cloned(),
            ));
            ctx.reply(b"accepted".to_vec())
        },
    )
    .expect("prod scheduler subscribes");

    let response = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            b"manifest".to_vec(),
            Duration::from_secs(1),
        )
        .expect("imported request");

    assert_eq!(response.payload().as_bytes(), b"accepted".as_slice());
    assert_eq!(response.island(), &prod);
    assert_eq!(response.responder().as_str(), "scheduler");
    let observed = observed.lock().expect("lock observed").clone();
    assert!(matches!(
        observed,
        Some((observed_island, observed_principal, observed_subject, Some(origin)))
            if observed_island == prod
                && observed_principal.as_str() == "bridge"
                && observed_subject.as_str() == "deploy.submit"
                && origin.rule_id().as_str() == "gpu-deploy"
                && origin.source_island() == &laptop
                && origin.source_principal().as_str() == "user"
                && origin.original_subject().as_str() == "gpu.deploy.submit"
    ));
}

#[test]
fn service_import_request_uses_one_remote_responder() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let calls = Arc::new(AtomicUsize::new(0));
    for responder in ["scheduler-a", "scheduler-b"] {
        let session = authority.grant_in(
            prod.clone(),
            principal(responder),
            Grant::empty()
                .with_subscribe(pattern("deploy.submit"))
                .with_response(),
        );
        let calls = Arc::clone(&calls);
        bus.subscribe(&session, pattern("deploy.submit"), move |ctx| {
            calls.fetch_add(1, Ordering::SeqCst);
            ctx.reply(format!("accepted:{responder}").into_bytes())
        })
        .expect("prod scheduler subscribes");
    }

    let response = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            b"manifest".to_vec(),
            Duration::from_secs(1),
        )
        .expect("imported request");

    assert_eq!(response.island(), &prod);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn exact_request_many_can_use_one_imported_service_responder() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let calls = Arc::new(AtomicUsize::new(0));
    let prod_scheduler = authority.grant_in(
        prod.clone(),
        principal("scheduler"),
        Grant::empty()
            .with_queue(pattern("deploy.submit"), "schedulers")
            .with_response(),
    );
    let calls_for_handler = Arc::clone(&calls);
    bus.queue_subscribe(
        &prod_scheduler,
        pattern("deploy.submit"),
        "schedulers",
        move |ctx| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            ctx.reply(b"accepted".to_vec())
        },
    )
    .expect("prod scheduler subscribes");

    let replies = bus
        .request_many(
            &laptop_user,
            RequestTarget::Subject(subject("gpu.deploy.submit")),
            subject("gpu.deploy.submit"),
            b"manifest".to_vec(),
            RequestManyPolicy::new(1, Duration::from_secs(1)),
        )
        .expect("imported request_many");
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].island(), &prod);
    assert_eq!(replies[0].payload().as_bytes(), b"accepted".as_slice());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let unsupported = bus
        .request_many(
            &laptop_user,
            RequestTarget::Subject(subject("gpu.deploy.submit")),
            subject("gpu.deploy.submit"),
            b"manifest".to_vec(),
            RequestManyPolicy::new(2, Duration::from_secs(1)),
        )
        .unwrap_err();
    assert!(matches!(
        unsupported,
        BusError::BridgeRequestManyUnsupported { requested: 2, .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn service_import_preserves_remote_response_scope() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    let prod_scheduler = authority.grant_in(
        prod,
        principal("scheduler"),
        Grant::empty()
            .with_subscribe(pattern("deploy.submit"))
            .with_response(),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
            BridgeEndpoint::new(island("prod"), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let duplicate_error = Arc::new(Mutex::new(None));
    let wrong_principal_error = Arc::new(Mutex::new(None));
    let (handler_done_tx, handler_done_rx) = mpsc::channel();
    let duplicate_error_for_handler = Arc::clone(&duplicate_error);
    let wrong_principal_error_for_handler = Arc::clone(&wrong_principal_error);
    bus.subscribe(&prod_scheduler, pattern("deploy.submit"), move |ctx| {
        let wrong = ctx
            .reply_as(&principal("bridge"), b"wrong".to_vec())
            .unwrap_err();
        *wrong_principal_error_for_handler
            .lock()
            .expect("lock wrong principal error") = Some(wrong);
        ctx.reply(b"ok".to_vec())?;
        let duplicate = ctx.reply(b"second".to_vec()).unwrap_err();
        *duplicate_error_for_handler
            .lock()
            .expect("lock duplicate error") = Some(duplicate);
        let _ = handler_done_tx.send(());
        Ok(())
    })
    .expect("prod scheduler subscribes");

    let response = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .expect("imported request");

    assert_eq!(response.payload().as_bytes(), b"ok".as_slice());
    handler_done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("handler completed");
    assert!(matches!(
        wrong_principal_error
            .lock()
            .expect("lock wrong principal error")
            .as_ref(),
        Some(BusError::UnauthorizedResponse { .. })
    ));
    assert!(matches!(
        duplicate_error
            .lock()
            .expect("lock duplicate error")
            .as_ref(),
        Some(BusError::DuplicateResponse { .. })
    ));
}

#[test]
fn disabled_service_import_fails_before_remote_dispatch() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    let rule = rule_id("gpu-deploy");
    authority
        .add_service_import(ServiceImport::new(
            rule.clone(),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");
    authority
        .set_service_import_state(&rule, BridgeState::Disabled)
        .expect("disable import");
    let remote_calls = Arc::new(AtomicUsize::new(0));
    let remote_calls_for_handler = Arc::clone(&remote_calls);
    let prod_scheduler = authority.grant_in(
        prod,
        principal("scheduler"),
        Grant::empty()
            .with_subscribe(pattern("deploy.submit"))
            .with_response(),
    );
    bus.subscribe(&prod_scheduler, pattern("deploy.submit"), move |ctx| {
        remote_calls_for_handler.fetch_add(1, Ordering::SeqCst);
        ctx.reply(b"should-not-run".to_vec())
    })
    .expect("prod scheduler subscribes");

    let error = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert!(matches!(
        error,
        BusError::BridgeUnavailable {
            failure: crate::BridgeFailure::Disabled,
            ..
        }
    ));
    assert_eq!(remote_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn enabled_service_import_without_remote_responder_reports_no_responders() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(
        laptop.clone(),
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop, subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod, subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let error = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();

    assert!(matches!(error, BusError::NoResponders { .. }));
}

#[test]
fn service_import_requires_local_and_remote_publish_grants() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_user = authority.grant_in(laptop.clone(), principal("user"), Grant::empty());
    let prod_bridge = authority.grant_in(prod.clone(), principal("bridge"), Grant::empty());
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("add service import");

    let local_error = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();
    assert!(matches!(
        local_error,
        BusError::UnauthorizedPublish { island, principal, subject }
            if island == laptop && principal.as_str() == "user"
                && subject.as_str() == "gpu.deploy.submit"
    ));

    let laptop_user = authority.grant_in(
        laptop,
        principal("user"),
        Grant::empty().with_publish(pattern("gpu.deploy.submit")),
    );
    let remote_error = bus
        .request(
            &laptop_user,
            subject("gpu.deploy.submit"),
            Vec::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();
    assert!(matches!(
        remote_error,
        BusError::UnauthorizedPublish { island, principal, subject }
            if island == prod && principal.as_str() == "bridge"
                && subject.as_str() == "deploy.submit"
    ));
}

#[test]
fn service_import_conflicts_with_local_subscriber_in_both_orders() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_admin = authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    bus.subscribe(&laptop_admin, pattern("gpu.deploy.submit"), |_| Ok(()))
        .expect("local subscriber registers");

    let add_error = authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .unwrap_err();
    assert!(matches!(
        add_error,
        BusError::BridgeRuleInvalid {
            violation: BridgeRuleViolation::LocalResponderConflict { .. }
        }
    ));

    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop_admin = authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod, subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("service import registers");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let subscribe_error = bus
        .subscribe(&laptop_admin, pattern("gpu.deploy.submit"), move |_| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(
        subscribe_error,
        BusError::BridgeRuleInvalid {
            violation: BridgeRuleViolation::LocalResponderConflict { .. }
        }
    ));
    bus.publish(
        &laptop_admin,
        subject("gpu.deploy.submit"),
        b"local".to_vec(),
    )
    .expect("publish does not register failed subscriber");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn service_import_conflicts_with_local_queue_subscriber_in_both_orders() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let laptop_admin = authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    bus.queue_subscribe(
        &laptop_admin,
        pattern("gpu.deploy.submit"),
        "schedulers",
        |_| Ok(()),
    )
    .expect("local queue subscriber registers");

    let add_error = authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod.clone(), subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .unwrap_err();
    assert!(matches!(
        add_error,
        BusError::BridgeRuleInvalid {
            violation: BridgeRuleViolation::LocalResponderConflict { .. }
        }
    ));

    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop_admin = authority.grant_in(laptop.clone(), principal("admin"), Grant::allow_all());
    let prod_bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("deploy.submit")),
    );
    authority
        .add_service_import(ServiceImport::new(
            rule_id("gpu-deploy"),
            BridgeEndpoint::new(laptop.clone(), subject("gpu.deploy.submit")),
            BridgeEndpoint::new(prod, subject("deploy.submit")),
            prod_bridge.principal().clone(),
        ))
        .expect("service import registers");

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&calls);
    let queue_error = bus
        .queue_subscribe(
            &laptop_admin,
            pattern("gpu.deploy.submit"),
            "schedulers",
            move |_| {
                calls_for_handler.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap_err();
    assert!(matches!(
        queue_error,
        BusError::BridgeRuleInvalid {
            violation: BridgeRuleViolation::LocalResponderConflict { .. }
        }
    ));
    bus.publish(
        &laptop_admin,
        subject("gpu.deploy.submit"),
        b"local".to_vec(),
    )
    .expect("publish does not register failed queue subscriber");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn stream_import_delivers_with_bridge_origin() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let prod_admin = authority.grant_in(
        prod.clone(),
        principal("prod-admin"),
        Grant::empty().with_publish(pattern("deploy.*.status")),
    );
    let bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")),
    );
    authority.grant_in(
        laptop.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("prod.deploy.*.status")),
    );
    let laptop_reader = authority.grant_in(
        laptop.clone(),
        principal("reader"),
        Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
    );
    authority
        .add_stream_import(StreamImport::new(
            rule_id("prod-status"),
            prod.clone(),
            laptop.clone(),
            bridge.principal().clone(),
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("transform validates"),
        ))
        .expect("add stream import");

    let observed = Arc::new(Mutex::new(None));
    let observed_for_handler = Arc::clone(&observed);
    bus.subscribe(
        &laptop_reader,
        pattern("prod.deploy.*.status"),
        move |ctx| {
            *observed_for_handler.lock().expect("lock observed") = Some((
                ctx.message.island().clone(),
                ctx.message.principal().clone(),
                ctx.message.subject().clone(),
                ctx.message.bridge_origin().cloned(),
            ));
            Ok(())
        },
    )
    .expect("laptop reader subscribes");

    bus.publish(
        &prod_admin,
        subject("deploy.d1.status"),
        b"running".to_vec(),
    )
    .expect("prod publishes status");

    let observed = observed
        .lock()
        .expect("lock observed")
        .clone()
        .expect("observed imported status");
    assert_eq!(observed.0, laptop);
    assert_eq!(observed.1.as_str(), "bridge");
    assert_eq!(observed.2.as_str(), "prod.deploy.d1.status");
    let origin = observed.3.expect("bridge origin exists");
    assert_eq!(origin.rule_id().as_str(), "prod-status");
    assert_eq!(origin.source_island(), &prod);
    assert_eq!(origin.source_principal().as_str(), "prod-admin");
    assert_eq!(origin.original_subject().as_str(), "deploy.d1.status");
}

#[test]
fn stream_import_requires_remote_export_and_local_publish_grants() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let prod_admin = authority.grant_in(
        prod.clone(),
        principal("prod-admin"),
        Grant::empty().with_publish(pattern("deploy.*.status")),
    );
    let bridge = authority.grant_in(prod.clone(), principal("bridge"), Grant::empty());
    let laptop_reader = authority.grant_in(
        laptop.clone(),
        principal("reader"),
        Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
    );
    authority
        .add_stream_import(StreamImport::new(
            rule_id("prod-status"),
            prod.clone(),
            laptop.clone(),
            bridge.principal().clone(),
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("transform validates"),
        ))
        .expect("add stream import");

    let deliveries = Arc::new(AtomicUsize::new(0));
    let deliveries_for_handler = Arc::clone(&deliveries);
    bus.subscribe(&laptop_reader, pattern("prod.deploy.*.status"), move |_| {
        deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("laptop reader subscribes");

    bus.publish(
        &prod_admin,
        subject("deploy.d1.status"),
        b"running".to_vec(),
    )
    .expect("prod publish still succeeds without export grant");
    assert_eq!(deliveries.load(Ordering::SeqCst), 0);

    authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")),
    );
    bus.publish(
        &prod_admin,
        subject("deploy.d1.status"),
        b"running".to_vec(),
    )
    .expect("prod publish still succeeds without local bridge publish");
    assert_eq!(deliveries.load(Ordering::SeqCst), 0);

    authority.grant_in(
        laptop,
        principal("bridge"),
        Grant::empty().with_publish(pattern("prod.deploy.*.status")),
    );
    bus.publish(
        &prod_admin,
        subject("deploy.d1.status"),
        b"running".to_vec(),
    )
    .expect("prod publish succeeds with both bridge grants");
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
}

#[test]
fn disabled_stream_import_skips_remote_delivery_but_keeps_local_publish() {
    let (bus, authority) = MemoryBus::new_with_authority();
    let laptop = island("laptop");
    let prod = island("prod");
    let prod_admin = authority.grant_in(
        prod.clone(),
        principal("prod-admin"),
        Grant::empty()
            .with_publish(pattern("deploy.*.status"))
            .with_subscribe(pattern("deploy.*.status")),
    );
    let bridge = authority.grant_in(
        prod.clone(),
        principal("bridge"),
        Grant::empty().with_bridge_export(pattern("deploy.*.status")),
    );
    authority.grant_in(
        laptop.clone(),
        principal("bridge"),
        Grant::empty().with_publish(pattern("prod.deploy.*.status")),
    );
    let laptop_reader = authority.grant_in(
        laptop.clone(),
        principal("reader"),
        Grant::empty().with_subscribe(pattern("prod.deploy.*.status")),
    );
    let rule = rule_id("prod-status");
    authority
        .add_stream_import(StreamImport::new(
            rule.clone(),
            prod.clone(),
            laptop,
            bridge.principal().clone(),
            SubjectTransform::new(pattern("deploy.*.status"), pattern("prod.deploy.*.status"))
                .expect("transform validates"),
        ))
        .expect("add stream import");
    authority
        .set_stream_import_state(&rule, BridgeState::Disabled)
        .expect("disable stream import");

    let prod_deliveries = Arc::new(AtomicUsize::new(0));
    let laptop_deliveries = Arc::new(AtomicUsize::new(0));
    let prod_deliveries_for_handler = Arc::clone(&prod_deliveries);
    bus.subscribe(&prod_admin, pattern("deploy.*.status"), move |_| {
        prod_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("prod subscribes locally");
    let laptop_deliveries_for_handler = Arc::clone(&laptop_deliveries);
    bus.subscribe(&laptop_reader, pattern("prod.deploy.*.status"), move |_| {
        laptop_deliveries_for_handler.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .expect("laptop subscribes");

    bus.publish(
        &prod_admin,
        subject("deploy.d1.status"),
        b"running".to_vec(),
    )
    .expect("prod publish succeeds");

    assert_eq!(prod_deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(laptop_deliveries.load(Ordering::SeqCst), 0);
}

fn record_max(max_active: &AtomicUsize, active_now: usize) {
    max_active.fetch_max(active_now, Ordering::SeqCst);
}
