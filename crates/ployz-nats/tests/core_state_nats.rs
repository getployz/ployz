use async_nats::jetstream;
use ployz_core::ids::{RevisionId, ServiceId};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveMachineState, ActiveMachineStateKey, ActiveRouteCommit, ActiveRouteCommitRequest,
    ActiveRouteState, ActiveRouteStateKey, ActiveServiceCommit, ActiveServiceCommitRequest,
    ActiveServiceState, ActiveServiceStateKey, ExpectedActiveRoute, ExpectedActiveRouteRevision,
    ExpectedActiveService,
};
use ployz_nats::core_state::{ActiveRouteStoreError, AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_test_support::ids::{
    machine_id, machine_name, namespace_id, operation_id, revision_id, route_hostname, route_port,
    service_id,
};

#[tokio::test]
async fn active_service_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let revision = revision_id("rev_1");

    let commit = store
        .commit_active_service(&commit_request(
            &service_id,
            ExpectedActiveService::Absent,
            &revision,
        ))
        .await
        .expect("active state stores");
    assert!(matches!(commit, ActiveServiceCommit::Stored { .. }));

    assert_eq!(
        store
            .active_service(&service_id)
            .await
            .expect("active state loads"),
        Some(ActiveServiceState {
            namespace_id: namespace_id("default"),
            service_id,
            active_revision: revision
        })
    );
}

#[tokio::test]
async fn active_machine_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let machine = active_machine_state("machine_7", "edge_7", "op_machine");

    store
        .replace_active_machine(&machine)
        .await
        .expect("active machine stores");

    assert_eq!(
        store
            .active_machine(&machine_id("machine_7"))
            .await
            .expect("active machine loads"),
        Some(machine)
    );
}

#[tokio::test]
async fn active_machines_list_sorted_by_machine_id() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let machine_8 = active_machine_state("machine_8", "edge_8", "op_8");
    let machine_7 = active_machine_state("machine_7", "edge_7", "op_7");

    store
        .replace_active_machine(&machine_8)
        .await
        .expect("machine 8 stores");
    store
        .replace_active_machine(&machine_7)
        .await
        .expect("machine 7 stores");

    assert_eq!(
        store.active_machines().await.expect("machines list"),
        vec![machine_7, machine_8]
    );
}

#[tokio::test]
async fn active_services_list_sorted_by_service_id() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let api = ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        active_revision: revision_id("rev_2"),
    };
    let worker = ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_worker"),
        active_revision: revision_id("rev_1"),
    };

    store
        .commit_active_service(&commit_request(
            &worker.service_id,
            ExpectedActiveService::Absent,
            &worker.active_revision,
        ))
        .await
        .expect("worker service stores");
    store
        .commit_active_service(&commit_request(
            &api.service_id,
            ExpectedActiveService::Absent,
            &api.active_revision,
        ))
        .await
        .expect("api service stores");

    assert_eq!(
        store.active_services().await.expect("services list"),
        vec![api, worker]
    );
}

#[tokio::test]
async fn active_service_commit_rejects_stale_previous_revision() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let rev_1 = revision_id("rev_1");
    let rev_2 = revision_id("rev_2");
    let rev_3 = revision_id("rev_3");

    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Absent,
                &rev_1,
            ))
            .await
            .expect("first commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(rev_1.clone()),
                &rev_2,
            ))
            .await
            .expect("second commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert_eq!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(rev_1.clone()),
                &rev_3,
            ))
            .await
            .expect("stale commit is classified"),
        ActiveServiceCommit::ActiveServiceChanged {
            expected_current: ExpectedActiveService::Revision(rev_1),
            current_revision: Some(rev_2.clone()),
            attempted_revision: rev_3
        }
    );

    assert_eq!(
        store
            .active_service(&service_id)
            .await
            .expect("active state loads"),
        Some(ActiveServiceState {
            namespace_id: namespace_id("default"),
            service_id,
            active_revision: rev_2
        })
    );
}

#[tokio::test]
async fn active_service_absent_precondition_is_idempotent_for_existing_current_revision() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let revision = revision_id("rev_1");

    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Absent,
                &revision,
            ))
            .await
            .expect("first commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert_eq!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Absent,
                &revision,
            ))
            .await
            .expect("existing current revision is idempotent"),
        ActiveServiceCommit::AlreadyCommitted {
            current_revision: revision
        }
    );
}

#[tokio::test]
async fn active_service_revision_precondition_allows_noop_current_revision() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let revision = revision_id("rev_1");

    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Absent,
                &revision,
            ))
            .await
            .expect("first commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert_eq!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(revision.clone()),
                &revision,
            ))
            .await
            .expect("valid noop commit is classified"),
        ActiveServiceCommit::AlreadyCommitted {
            current_revision: revision
        }
    );
}

#[tokio::test]
async fn active_service_same_target_with_wrong_previous_revision_is_idempotent() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let rev_1 = revision_id("rev_1");
    let rev_2 = revision_id("rev_2");

    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Absent,
                &rev_1,
            ))
            .await
            .expect("first commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert!(matches!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(rev_1),
                &rev_2
            ))
            .await
            .expect("second commit stores"),
        ActiveServiceCommit::Stored { .. }
    ));
    assert_eq!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(revision_id("rev_wrong")),
                &rev_2,
            ))
            .await
            .expect("same target revision is classified"),
        ActiveServiceCommit::AlreadyCommitted {
            current_revision: rev_2
        }
    );
}

#[tokio::test]
async fn active_service_commit_reports_missing_expected_revision() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let service_id = service_id("svc_api");
    let expected = revision_id("rev_1");
    let revision = revision_id("rev_2");

    assert_eq!(
        store
            .commit_active_service(&commit_request(
                &service_id,
                ExpectedActiveService::Revision(expected.clone()),
                &revision,
            ))
            .await
            .expect("missing expected revision is classified"),
        ActiveServiceCommit::ActiveServiceChanged {
            expected_current: ExpectedActiveService::Revision(expected),
            current_revision: None,
            attempted_revision: revision
        }
    );
}

#[tokio::test]
async fn active_service_state_rejects_payload_for_wrong_service_key() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target_service_id = service_id("svc_api");
    let other_service_id = service_id("svc_other");
    let key = ActiveServiceStateKey::from_service_id(&target_service_id);
    let bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open test KV_CORE bucket");

    let wrong_payload = serde_json::to_vec(&ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: other_service_id.clone(),
        active_revision: revision_id("rev_1"),
    })
    .expect("encode wrong active state");
    bucket
        .put(key.as_str(), wrong_payload.into())
        .await
        .expect("write corrupt active state");

    let error = store
        .active_service(&target_service_id)
        .await
        .expect_err("wrong service payload is rejected");
    match error {
        CoreStateStoreError::CorruptActiveServiceState {
            key: actual_key,
            expected_service_id,
            actual_service_id,
        } => {
            assert_eq!(actual_key, key.as_str());
            assert_eq!(expected_service_id, target_service_id);
            assert_eq!(actual_service_id, other_service_id);
        }
        other @ (CoreStateStoreError::OpenBucket { .. }
        | CoreStateStoreError::Encode(_)
        | CoreStateStoreError::Decode(_)
        | CoreStateStoreError::CasConflict { .. }
        | CoreStateStoreError::Delete { .. }
        | CoreStateStoreError::Get { .. }
        | CoreStateStoreError::ListKeys { .. }
        | CoreStateStoreError::CorruptKey { .. }
        | CoreStateStoreError::Timeout { .. }) => {
            panic!("unexpected error: {other:?}");
        }
    }
}

#[tokio::test]
async fn missing_active_service_state_returns_none() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");

    assert_eq!(
        store
            .active_service(&service_id("svc_missing"))
            .await
            .expect("missing active state lookup succeeds"),
        None
    );
}

#[tokio::test]
async fn active_route_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target = route_target("API.example.com", 443);

    let commit = store
        .commit_active_route(&route_commit_request(
            &target,
            ExpectedActiveRoute::Absent,
            "svc_api",
            "rev_1",
        ))
        .await
        .expect("route state stores");
    assert!(matches!(commit, ActiveRouteCommit::Stored { .. }));

    assert_eq!(
        store
            .active_route(&target)
            .await
            .expect("active route loads"),
        Some(ActiveRouteState {
            namespace_id: namespace_id("default"),
            target,
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
    );
}

#[tokio::test]
async fn active_routes_lists_only_route_state_sorted_by_target() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let api = route_target("api.example.com", 443);
    let www = route_target("www.example.com", 443);

    store
        .commit_active_service(&commit_request(
            &service_id("svc_api"),
            ExpectedActiveService::Absent,
            &revision_id("rev_1"),
        ))
        .await
        .expect("service state stores");
    store
        .commit_active_route(&route_commit_request(
            &www,
            ExpectedActiveRoute::Absent,
            "svc_web",
            "rev_1",
        ))
        .await
        .expect("www route stores");
    store
        .commit_active_route(&route_commit_request(
            &api,
            ExpectedActiveRoute::Absent,
            "svc_api",
            "rev_2",
        ))
        .await
        .expect("api route stores");

    assert_eq!(
        store.active_routes().await.expect("routes list"),
        vec![
            active_route_state(&api, "svc_api", "rev_2"),
            active_route_state(&www, "svc_web", "rev_1"),
        ]
    );
}

#[tokio::test]
async fn active_route_commit_rejects_stale_previous_revision() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target = route_target("api.example.com", 443);

    assert!(matches!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::Absent,
                "svc_api",
                "rev_1",
            ))
            .await
            .expect("first route commit stores"),
        ActiveRouteCommit::Stored { .. }
    ));
    assert!(matches!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_1"),
                    endpoint_port: route_port(8080),
                }),
                "svc_api",
                "rev_2",
            ))
            .await
            .expect("second route commit stores"),
        ActiveRouteCommit::Stored { .. }
    ));

    assert_eq!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_1"),
                    endpoint_port: route_port(8080),
                }),
                "svc_api",
                "rev_3",
            ))
            .await
            .expect("stale route commit is classified"),
        ActiveRouteCommit::ActiveRouteChanged {
            expected_current: ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_1"),
                endpoint_port: route_port(8080),
            }),
            current: Some(active_route_state(&target, "svc_api", "rev_2")),
            attempted: active_route_state(&target, "svc_api", "rev_3"),
        }
    );
}

#[tokio::test]
async fn active_route_commit_rejects_stale_previous_endpoint_port() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target = route_target("api.example.com", 443);

    assert!(matches!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::Absent,
                "svc_api",
                "rev_1",
            ))
            .await
            .expect("first route commit stores"),
        ActiveRouteCommit::Stored { .. }
    ));

    assert_eq!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_1"),
                    endpoint_port: route_port(3000),
                }),
                "svc_api",
                "rev_2",
            ))
            .await
            .expect("stale route endpoint is classified"),
        ActiveRouteCommit::ActiveRouteChanged {
            expected_current: ExpectedActiveRoute::ServiceRevision(ExpectedActiveRouteRevision {
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_1"),
                endpoint_port: route_port(3000),
            }),
            current: Some(active_route_state(&target, "svc_api", "rev_1")),
            attempted: active_route_state(&target, "svc_api", "rev_2"),
        }
    );
}

#[tokio::test]
async fn active_route_same_target_revision_is_idempotent() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target = route_target("api.example.com", 443);

    assert!(matches!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::Absent,
                "svc_api",
                "rev_1",
            ))
            .await
            .expect("first route commit stores"),
        ActiveRouteCommit::Stored { .. }
    ));

    assert_eq!(
        store
            .commit_active_route(&route_commit_request(
                &target,
                ExpectedActiveRoute::Absent,
                "svc_api",
                "rev_1",
            ))
            .await
            .expect("same route commit is idempotent"),
        ActiveRouteCommit::AlreadyCommitted {
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        }
    );
}

#[tokio::test]
async fn missing_active_route_state_returns_none() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");

    assert_eq!(
        store
            .active_route(&route_target("missing.example.com", 443))
            .await
            .expect("missing route lookup succeeds"),
        None
    );
}

#[tokio::test]
async fn active_route_state_rejects_payload_for_wrong_route_key() {
    let nats = test_nats().await;
    let store = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let target = route_target("api.example.com", 443);
    let other_target = route_target("www.example.com", 443);
    let key = ActiveRouteStateKey::from_target(&target);
    let bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open test KV_CORE bucket");

    let wrong_payload = serde_json::to_vec(&ActiveRouteState {
        namespace_id: namespace_id("default"),
        target: other_target.clone(),
        endpoint_port: route_port(8080),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
    })
    .expect("encode wrong route state");
    bucket
        .put(key.as_str(), wrong_payload.into())
        .await
        .expect("write corrupt route state");

    let error = store
        .active_route(&target)
        .await
        .expect_err("wrong route payload is rejected");
    match error {
        ActiveRouteStoreError::CorruptActiveRouteState {
            key: actual_key,
            expected_target,
            actual_target,
        } => {
            assert_eq!(actual_key, key.as_str());
            assert_eq!(expected_target, target);
            assert_eq!(actual_target, other_target);
        }
        other @ (ActiveRouteStoreError::Encode(_)
        | ActiveRouteStoreError::Decode(_)
        | ActiveRouteStoreError::CasConflict { .. }
        | ActiveRouteStoreError::ListKeys { .. }
        | ActiveRouteStoreError::Watch { .. }
        | ActiveRouteStoreError::Get { .. }
        | ActiveRouteStoreError::CorruptKey { .. }
        | ActiveRouteStoreError::Timeout { .. }) => {
            panic!("unexpected error: {other:?}");
        }
    }
}

#[test]
fn active_service_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveServiceStateKey::from_service_id(&service_id("svc_api")).as_str(),
        "services.svc_api"
    );
}

#[test]
fn active_route_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveRouteStateKey::from_target(&route_target("API.example.com", 443)).as_str(),
        "routes.6170692e6578616d706c652e636f6d.443"
    );
}

#[test]
fn active_machine_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveMachineStateKey::from_machine_id(&machine_id("machine_7")).as_str(),
        "machines.machine_7"
    );
}

struct TestNats {
    _server: ployz_test_support::nats::TestNats,
    jetstream: jetstream::Context,
}

/// A secured server with the store connected as the Controller — the
/// principal that commits active state in production.
async fn test_nats() -> TestNats {
    let server = ployz_test_support::nats::TestNats::start().await;
    server.bootstrap_resources().await;
    let jetstream = server.jetstream.clone();

    TestNats {
        _server: server,
        jetstream,
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn active_route_state(target: &RouteTarget, service: &str, revision: &str) -> ActiveRouteState {
    ActiveRouteState {
        namespace_id: namespace_id("default"),
        target: target.clone(),
        endpoint_port: route_port(8080),
        service_id: service_id(service),
        revision_id: revision_id(revision),
    }
}

fn active_machine_state(machine: &str, name: &str, operation: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(machine),
        name: machine_name(name),
        activated_by: operation_id(operation),
        substrate_versions: None,
    }
}

fn commit_request(
    service_id: &ServiceId,
    expected_current: ExpectedActiveService,
    target_revision: &RevisionId,
) -> ActiveServiceCommitRequest {
    ActiveServiceCommitRequest {
        namespace_id: namespace_id("default"),
        service_id: service_id.clone(),
        expected_current,
        target_revision: target_revision.clone(),
    }
}

fn route_commit_request(
    target: &RouteTarget,
    expected_current: ExpectedActiveRoute,
    service: &str,
    revision: &str,
) -> ActiveRouteCommitRequest {
    ActiveRouteCommitRequest {
        namespace_id: namespace_id("default"),
        target: target.clone(),
        endpoint_port: route_port(8080),
        expected_current,
        service_id: service_id(service),
        revision_id: revision_id(revision),
    }
}
