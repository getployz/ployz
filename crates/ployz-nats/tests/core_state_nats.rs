use async_nats::jetstream;
use ployz_core::ids::{RevisionId, ServiceId};
use ployz_core::state::{
    ActiveServiceCommit, ActiveServiceCommitRequest, ActiveServiceState, ActiveServiceStateKey,
    ExpectedActiveService,
};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::kv::KV_CORE_BUCKET;

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
            service_id,
            active_revision: revision
        })
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
        | CoreStateStoreError::Get { .. }
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

#[test]
fn active_service_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveServiceStateKey::from_service_id(&service_id("svc_api")).as_str(),
        "services.svc_api"
    );
}

struct TestNats {
    _server: nats_server::Server,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let server = nats_server::run_server("tests/configs/jetstream.conf");
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_CORE_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("create KV_CORE bucket");

    TestNats {
        _server: server,
        jetstream,
    }
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn commit_request(
    service_id: &ServiceId,
    expected_current: ExpectedActiveService,
    target_revision: &RevisionId,
) -> ActiveServiceCommitRequest {
    ActiveServiceCommitRequest {
        service_id: service_id.clone(),
        expected_current,
        target_revision: target_revision.clone(),
    }
}
