use std::{
    collections::BTreeMap,
    net::TcpListener,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ployz_core::{
    CertificateHost, ContainerId, DockerVolume, DockerVolumeId, DockerVolumeName, ForgottenAnswer,
    IngressHost, IssuanceClock, IssuanceFailure, Machine, MachineId, QualifiedService,
};
use serde_json::json;

use super::{
    IngressRouteAck, IngressRouteChange, IngressRouteRow, ReplicatedStore, store::RuntimeOverlay,
};
use crate::corrosion::{ApiClient, Statement};
use crate::machine::{
    LocalMachine, LocalMachineBody, LocalMachinePrior, LocalMachineRecord, LocalMachineStore,
    ParticipationOrigin, RecordOwner,
};
use crate::runtime_watch::RuntimeWatchSnapshot;

#[tokio::test]
async fn catch_up_waits_for_removal_and_rechecks_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-catch-up-reset-{}",
        ployz_core::MachineId::random()
    ));
    let mut local = LocalMachineStore::open(&data_dir).unwrap();
    let public_key = local.record().private_key().public_key();
    let machine: Machine = serde_json::from_value(json!({
        "id": local.record().id(),
        "name": "joining",
        "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": public_key.0,
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();
    local
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: ployz_core::Registered {
                    assigned_machine: machine.clone(),
                    visible_peers: vec![machine],
                    target_versions: BTreeMap::new(),
                    sealing_key: None,
                    devices: Vec::new(),
                },
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let local = RecordOwner::spawn(local).unwrap();

    let first = store.machine_publication().await;
    let clone = store.clone();
    let task_local = local.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication.complete_catch_up(&task_local).await
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    local
        .mutate(LocalMachineStore::begin_reset)
        .await
        .unwrap()
        .unwrap();
    drop(first);
    let completed = tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!completed);
    drop(local);
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn container_publication_waits_for_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, _local) = participating_record();
    let machine_id = machine.id;
    let first = store.machine_publication().await;
    let clone = store.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication
            .apply_container_rows(&machine_id, &[], &[])
            .await
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn volume_publication_waits_for_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, _local) = participating_record();
    let machine_id = machine.id;
    let first = store.machine_publication().await;
    let clone = store.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication
            .apply_volume_rows(&machine_id, &[], &[], &[])
            .await
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    drop(first);
    tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn volume_store_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    let id = DockerVolumeId {
        machine_id: MachineId::random(),
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let volume = DockerVolume {
        id: id.clone(),
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    assert!(store.publish_volume(&volume).await.is_err());
    assert!(store.volume(&id).await.is_err());
    assert!(store.volumes().await.is_err());
}

#[tokio::test]
async fn runtime_watch_snapshot_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    assert!(store.certificate_rows().await.is_err());
    assert!(RuntimeWatchSnapshot::from_store(&store).await.is_err());
    assert!(store.subscribe_runtime_watch_changes().await.is_err());
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-runtime-watch-unreachable-{}",
        ployz_core::MachineId::random()
    ));
    let local =
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap());
    assert!(
        crate::runtime_watch::serve_replicated_runtime_watch(
            store,
            local,
            crate::network::PathBook::default(),
            MachineId::random(),
        )
        .await
        .is_err()
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn record_certificate_failure_is_an_error_when_the_store_is_unreachable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = ReplicatedStore::new(ApiClient::new(address, &"a".repeat(64)).unwrap());
    let hostname = IngressHost::parse("app.example.com").unwrap();
    assert!(
        store
            .record_certificate_failure(
                &hostname,
                None,
                "does not resolve",
                IssuanceClock::new(
                    1,
                    SystemTime::UNIX_EPOCH,
                    IssuanceFailure::Refused(ployz_core::Refusal::DoesNotResolve),
                ),
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn publication_guard_rechecks_the_local_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (mut machine, local) = participating_record();
    machine.runtime.management_identity = Some(local.management_secret().public_key());
    let publication = store.machine_publication().await;
    assert_eq!(
        publication.publishable_machine(&local, RuntimeOverlay::default()),
        Some(machine.clone())
    );
    let mut observed = machine;
    observed.runtime.running_builds = 2;
    observed.runtime.pool_committable_bytes = Some(7 << 30);
    assert_eq!(
        publication.publishable_machine(
            &local,
            RuntimeOverlay {
                running_builds: 2,
                pool_committable_bytes: Some(7 << 30),
                sealing: None,
            }
        ),
        Some(observed)
    );
    assert!(matches!(
        local.body(),
        LocalMachineBody::Participating { machine, .. }
            if machine.runtime.running_builds == 0 && machine.runtime.pool_committable_bytes.is_none()
    ));

    let LocalMachineBody::Participating {
        machine: body_machine,
        origin,
    } = local.body().clone()
    else {
        panic!("fixture is participating");
    };
    let local = LocalMachineRecord::parse(
        LocalMachineBody::Resetting {
            prior: Box::new(LocalMachinePrior::Participating {
                machine: body_machine,
                origin,
            }),
        },
        local.private_key().clone(),
    )
    .unwrap();
    assert_eq!(
        publication.publishable_machine(&local, RuntimeOverlay::default()),
        None
    );
}

#[tokio::test]
async fn publisher_republishes_when_pool_committable_bytes_change() {
    use std::time::Duration;

    let (replicated, server) = crate::corrosion::fake_cluster::store_with_subscriptions().await;
    let data_dir = std::env::temp_dir().join(format!("ployzd-committable-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: ployz_core::MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let owner = RecordOwner::spawn(store).unwrap();
    let id = owner.record().id();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let view = crate::corrosion::MachineView::start(replicated.clone(), shutdown.clone());
    let mut machines = view.watch();
    let (committable, observed) = tokio::sync::watch::channel(None);
    let publisher = tokio::spawn(crate::corrosion::run_machine_publisher(
        Some((replicated.clone(), view)),
        owner,
        crate::corrosion::LiveRuntime::new(tokio::sync::watch::channel(0).1, observed),
        shutdown.clone(),
    ));
    let mut published = async |expected: Option<u64>| {
        tokio::time::timeout(
            Duration::from_secs(5),
            machines.wait_for(|snapshot| {
                snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.observations.iter().any(|machine| {
                        machine.id == id && machine.runtime.pool_committable_bytes == expected
                    })
                })
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("{expected:?} was never published"))
        .unwrap();
    };

    published(None).await;
    committable.send_replace(Some(7 << 30));
    published(Some(7 << 30)).await;

    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    server.abort();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn publisher_publishes_the_founders_sealing_public_key() {
    use std::time::Duration;

    let cluster = crate::corrosion::fake_cluster::FakeCluster::start();
    let replicated = cluster.store();
    let data_dir = std::env::temp_dir().join(format!("ployzd-sealing-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: ployz_core::MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    let held = crate::sealing_key::SealingKey::load(&data_dir)
        .unwrap()
        .expect("the founder holds a Sealing Key")
        .public_key();
    let owner = RecordOwner::spawn(store).unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let view = crate::corrosion::MachineView::start(replicated.clone(), shutdown.clone());
    let publisher = tokio::spawn(crate::corrosion::run_machine_publisher(
        Some((replicated, view)),
        owner,
        crate::corrosion::LiveRuntime::unobserved(),
        shutdown.clone(),
    ));

    let published = tokio::time::timeout(Duration::from_secs(5), cluster.sealing_public_key())
        .await
        .expect("the founder's Sealing public key was never published");
    assert_eq!(published, held.to_string());

    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn a_founder_awaiting_recovery_names_its_key_before_the_network_and_holds_none() {
    use std::time::Duration;

    let cluster = crate::corrosion::fake_cluster::FakeCluster::start();
    cluster.clear_network();
    let replicated = cluster.store();
    let data_dir = std::env::temp_dir().join(format!("ployzd-awaits-{}", MachineId::random()));
    let expected = ployz_core::SealingPublicKey::from_bytes([4; 32]);
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll_for_recovery(
            ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: ployz_core::MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            },
            ployz_core::RecoverySeed {
                expected_public_key: expected,
                backup: None,
            },
        )
        .unwrap();
    assert!(
        crate::sealing_key::SealingKey::load(&data_dir)
            .unwrap()
            .is_none()
    );
    let owner = RecordOwner::spawn(store).unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let view = crate::corrosion::MachineView::start(replicated.clone(), shutdown.clone());
    let publisher = tokio::spawn(crate::corrosion::run_machine_publisher(
        Some((replicated.clone(), view)),
        owner,
        crate::corrosion::LiveRuntime::unobserved(),
        shutdown.clone(),
    ));

    let awaited = tokio::time::timeout(Duration::from_secs(5), cluster.awaited_key_at_network())
        .await
        .expect("the founder never published its Cluster network");

    assert_eq!(awaited, Some(expected.to_string()));
    assert_eq!(
        replicated.sealing_awaits_recovery().await.unwrap(),
        Some(expected)
    );
    assert_eq!(replicated.sealing_public_key().await.unwrap(), None);
    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    let _ = std::fs::remove_dir_all(data_dir);
}

#[test]
fn only_a_founder_recovers_a_sealing_key() {
    let data_dir = std::env::temp_dir().join(format!("ployzd-member-seed-{}", MachineId::random()));
    let mut local = LocalMachineStore::open(&data_dir).unwrap();
    let machine: Machine = serde_json::from_value(json!({
        "id": local.record().id(),
        "name": "joining",
        "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": local.record().private_key().public_key().0,
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();

    let refused = local.enroll_for_recovery(
        ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: ployz_core::Registered {
                    assigned_machine: machine.clone(),
                    visible_peers: vec![machine],
                    target_versions: BTreeMap::new(),
                    sealing_key: None,
                    devices: Vec::new(),
                },
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        },
        ployz_core::RecoverySeed {
            expected_public_key: ployz_core::SealingPublicKey::from_bytes([4; 32]),
            backup: None,
        },
    );

    assert!(matches!(
        refused,
        Err(crate::machine::StoreError::RecoveryWithoutFounding)
    ));
    assert_eq!(local.record().recovery_seed(), None);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn the_first_sealing_key_awaiting_recovery_stays() {
    let (store, task) = identity_store(corrosion_db()).await;
    assert_eq!(store.sealing_awaits_recovery().await.unwrap(), None);
    let first = ployz_core::SealingPublicKey::from_bytes([1; 32]);
    store.publish_sealing_awaits_recovery(first).await.unwrap();
    store
        .publish_sealing_awaits_recovery(ployz_core::SealingPublicKey::from_bytes([2; 32]))
        .await
        .unwrap();

    assert_eq!(store.sealing_awaits_recovery().await.unwrap(), Some(first));
    task.abort();
}

#[tokio::test]
async fn publishing_a_sealing_public_key_never_replaces_the_clusters() {
    let (store, task) = identity_store(corrosion_db()).await;
    let first = ployz_core::SealingPublicKey::from_bytes([1; 32]);
    store.publish_sealing_public_key(first).await.unwrap();
    store
        .publish_sealing_public_key(ployz_core::SealingPublicKey::from_bytes([2; 32]))
        .await
        .unwrap();

    let rows = store
        .api()
        .query(Statement::new(
            "SELECT value FROM cluster WHERE key = 'sealing_public_key'",
            [],
        ))
        .await
        .unwrap()
        .rows(["value"])
        .unwrap();
    assert_eq!(rows, [[json!(first.to_string())]]);
    task.abort();
}

#[tokio::test]
async fn the_sealing_readers_report_the_clusters_key_and_its_recovery_marker() {
    let (store, task) = identity_store(corrosion_db()).await;
    assert_eq!(store.sealing_public_key().await.unwrap(), None);
    assert_eq!(store.sealing_awaits_recovery().await.unwrap(), None);

    let published = ployz_core::SealingPublicKey::from_bytes([3; 32]);
    let awaited = ployz_core::SealingPublicKey::from_bytes([4; 32]);
    store.publish_sealing_public_key(published).await.unwrap();
    store
        .api()
        .execute([Statement::new(
            "INSERT INTO cluster (key, value) VALUES ('sealing_awaits_recovery', ?)",
            [json!(awaited.to_string())],
        )])
        .await
        .unwrap();

    assert_eq!(store.sealing_public_key().await.unwrap(), Some(published));
    assert_eq!(
        store.sealing_awaits_recovery().await.unwrap(),
        Some(awaited)
    );
    task.abort();
}

fn participating_record() -> (Machine, LocalMachineRecord) {
    let machine: Machine = serde_json::from_value(json!({
        "id": "b".repeat(32),
        "name": "machine",
        "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": crate::network::WireGuardPrivateKey::from_bytes([0; 32]).public_key(),
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();
    let local = LocalMachineRecord::parse(
        LocalMachineBody::Participating {
            machine: machine.clone(),
            origin: ParticipationOrigin::Join {
                bootstrap: vec![machine.clone()],
            },
        },
        crate::network::WireGuardPrivateKey::from_bytes([0; 32]),
    )
    .unwrap();
    (machine, local)
}
fn sql_param(value: &serde_json::Value) -> rusqlite::types::Value {
    match value {
        serde_json::Value::String(value) => value.clone().into(),
        serde_json::Value::Number(value) => value.as_i64().unwrap().into(),
        serde_json::Value::Array(bytes) => bytes
            .iter()
            .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
            .collect::<Vec<_>>()
            .into(),
        serde_json::Value::Null => rusqlite::types::Value::Null,
        value @ (serde_json::Value::Bool(_) | serde_json::Value::Object(_)) => {
            panic!("unsupported test parameter: {value}")
        }
    }
}

#[tokio::test]
async fn catch_up_rejects_invalid_targets_without_retrying() {
    use super::wait_for_catch_up;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    for (actor, version) in [
        ("a".repeat(32), -1),
        ("invalid".into(), 0),
        ("ab".into(), 1),
    ] {
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_catch_up(&store, &BTreeMap::from([(actor, version)])),
        )
        .await
        .expect("invalid targets must fail without retrying")
        .expect_err("invalid targets must not complete catch-up");
    }
}

/// #2930: a recreated `store.db` holds only this Machine's row until gossip reaches it,
/// while a removed Machine leaves its key behind.
#[tokio::test]
async fn a_store_has_held_another_machine_once_its_key_arrived() {
    let own = MachineId::random();
    let other = MachineId::random();
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE machines__crsql_pks (__crsql_key INTEGER PRIMARY KEY, id);")
        .unwrap();
    db.execute(
        "INSERT INTO machines__crsql_pks (id) VALUES (?)",
        [own.to_string()],
    )
    .unwrap();
    let (store, server) = identity_store(db).await;
    assert!(!store.held_another_machine(&own).await.unwrap());
    store
        .api()
        .execute([Statement::new(
            "INSERT INTO machines__crsql_pks (id) VALUES (?)",
            [json!(other)],
        )])
        .await
        .unwrap();
    assert!(store.held_another_machine(&own).await.unwrap());
    server.abort();
}

#[tokio::test]
async fn catch_up_waits_for_pending_target_transactions() {
    use super::{Statement, wait_for_catch_up};
    use std::time::Duration;

    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);
         INSERT INTO crsql_db_versions VALUES (X'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 5);
         INSERT INTO __corro_seq_bookkeeping VALUES (X'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 5);",
    )
    .unwrap();
    let (store, server) = identity_store(db).await;
    let target = BTreeMap::from([("a".repeat(32), 5)]);
    assert!(!store.has_reached_version(&target).await.unwrap());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_catch_up(&store, &target)
        )
        .await
        .is_err()
    );

    store
        .api()
        .execute([
            Statement::new("DELETE FROM __corro_seq_bookkeeping", []),
            Statement::new(
                "INSERT INTO __corro_bookkeeping_gaps VALUES (?, 5, 5)",
                [json!(vec![0xaa; 16])],
            ),
        ])
        .await
        .unwrap();
    assert!(!store.has_reached_version(&target).await.unwrap());

    // Later gaps and pending transactions, including other actors, do not block this target.
    store
        .api()
        .execute([
            Statement::new("UPDATE __corro_bookkeeping_gaps SET start = 6, end = 6", []),
            Statement::new(
                "INSERT INTO __corro_seq_bookkeeping VALUES (?, 6)",
                [json!(vec![0xaa; 16])],
            ),
            Statement::new(
                "INSERT INTO __corro_bookkeeping_gaps VALUES (?, 1, 1)",
                [json!(vec![0xbb; 16])],
            ),
            Statement::new(
                "INSERT INTO __corro_seq_bookkeeping VALUES (?, 1)",
                [json!(vec![0xbb; 16])],
            ),
        ])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), wait_for_catch_up(&store, &target))
        .await
        .unwrap()
        .unwrap();
    assert!(
        !store
            .has_reached_version(&BTreeMap::from([("a".repeat(32), 6)]))
            .await
            .unwrap()
    );
    assert!(
        !store
            .has_reached_version(&BTreeMap::from([("c".repeat(32), 1)]))
            .await
            .unwrap()
    );
    assert!(
        store
            .has_reached_version(&BTreeMap::from([("c".repeat(32), 0)]))
            .await
            .unwrap()
    );
    assert!(store.has_reached_version(&BTreeMap::new()).await.unwrap());
    assert!(
        store
            .has_reached_version(&BTreeMap::from([("invalid".into(), 1)]))
            .await
            .is_err()
    );
    assert!(
        store
            .has_reached_version(&BTreeMap::from([("a".repeat(32), -1)]))
            .await
            .is_err()
    );
    server.abort();
}

#[tokio::test]
async fn store_version_stops_below_each_actors_first_hole() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);
         INSERT INTO crsql_db_versions VALUES (X'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 14);
         INSERT INTO __corro_bookkeeping_gaps VALUES (X'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 10, 13);
         INSERT INTO crsql_db_versions VALUES (X'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 5);
         INSERT INTO __corro_seq_bookkeeping VALUES (X'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 4);
         INSERT INTO crsql_db_versions VALUES (X'cccccccccccccccccccccccccccccccc', 7);
         INSERT INTO crsql_db_versions VALUES (X'dddddddddddddddddddddddddddddddd', 3);
         INSERT INTO __corro_bookkeeping_gaps VALUES (X'dddddddddddddddddddddddddddddddd', 1, 2);",
    )
    .unwrap();
    let (store, server) = identity_store(db).await;
    let version = store.version().await.unwrap();
    assert_eq!(
        version,
        BTreeMap::from([
            ("a".repeat(32), 9),
            ("b".repeat(32), 3),
            ("c".repeat(32), 7)
        ])
    );
    // A joiner given this map as its target can finish while every live store keeps the
    // hole, as one does when the actor that wrote the missing versions is gone (#3611).
    assert!(store.has_reached_version(&version).await.unwrap());
    server.abort();
}

// Exercise the store API against real SQL rows, including corrupt imported documents.
pub(crate) async fn identity_store(
    db: rusqlite::Connection,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    use axum::{Router, body::Bytes, extract::State, routing::post};
    async fn query(State(db): State<Arc<Mutex<rusqlite::Connection>>>, body: Bytes) -> Vec<u8> {
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let db = db.lock().unwrap();
        let mut statement = db
            .prepare(request.get("query").unwrap().as_str().unwrap())
            .unwrap();
        let columns = statement
            .column_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        let params = request
            .get("params")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(sql_param);
        let mut rows = statement.query(rusqlite::params_from_iter(params)).unwrap();
        let mut response = serde_json::to_vec(&json!({"columns": columns})).unwrap();
        let mut index = 0;
        while let Some(row) = rows.next().unwrap() {
            index += 1;
            let values = (0..columns.len())
                .map(|i| match row.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                    rusqlite::types::ValueRef::Integer(value) => json!(value),
                    rusqlite::types::ValueRef::Text(value) => {
                        json!(std::str::from_utf8(value).unwrap())
                    }
                    rusqlite::types::ValueRef::Blob(value) => json!(value),
                    value @ rusqlite::types::ValueRef::Real(_) => {
                        panic!("unsupported test row value: {value:?}")
                    }
                })
                .collect::<Vec<_>>();
            response.extend(serde_json::to_vec(&json!({"row": [index, values]})).unwrap());
        }
        response.extend(br#"{"eoq":{"time":0.0}}"#);
        response
    }
    async fn transact(State(db): State<Arc<Mutex<rusqlite::Connection>>>, body: Bytes) -> Vec<u8> {
        let requests: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        let db = db.lock().unwrap();
        let results = requests
            .iter()
            .map(|request| {
                let params = request
                    .get("params")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(sql_param);
                let affected = db
                    .execute(
                        request.get("query").unwrap().as_str().unwrap(),
                        rusqlite::params_from_iter(params),
                    )
                    .unwrap();
                json!({"rows_affected": affected, "time": 0.0})
            })
            .collect::<Vec<_>>();
        serde_json::to_vec(&json!({"results": results, "time": 0.0})).unwrap()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let store = ReplicatedStore::new(
        ApiClient::http1(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/queries", post(query))
                .route("/v1/transactions", post(transact))
                .with_state(Arc::new(Mutex::new(db))),
        )
        .await
        .unwrap();
    });
    (store, task)
}

/// Corrosion re-renders schema.sql before creating its tables, which declares
/// the SQLite keyword column `key` as `KEY`; an unaliased `SELECT key` then
/// reports `KEY`.
pub(crate) fn corrosion_db() -> rusqlite::Connection {
    let schema = include_str!("schema.sql").replacen("    key  ", "    KEY  ", 1);
    assert_ne!(schema, include_str!("schema.sql"));
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(&schema).unwrap();
    db
}

#[tokio::test]
async fn machine_reads_reject_document_identity_different_from_row_key() {
    let db = corrosion_db();
    let key = MachineId::parse("a".repeat(32)).unwrap();
    let document = json!({"id": "b".repeat(32), "name": "peer", "subnet": "10.210.1.0/24", "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": vec![1; 32], "advertised_endpoints": []}).to_string();
    db.execute(
        "INSERT INTO machines (id, info) VALUES (?, ?)",
        rusqlite::params![key.as_str(), document],
    )
    .unwrap();
    let (store, task) = identity_store(db).await;
    assert!(store.machine(key.as_str()).await.is_err());
    assert!(store.machines().await.is_err());
    task.abort();
}

#[tokio::test]
async fn ingress_route_acks_upsert_and_delete_against_the_schema() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    let (store, task) = identity_store(db).await;
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let api = QualifiedService::parse("app/api").unwrap();
    let web = QualifiedService::parse("app/web").unwrap();
    let ack = |containers: &[&str]| IngressRouteAck {
        containers: containers
            .iter()
            .map(|id| ContainerId::parse(id.repeat(64)).unwrap())
            .collect(),
    };
    let rows = || async {
        store
            .api()
            .query(Statement::new(
                "SELECT machine_id, service_id, containers FROM ingress_routes ORDER BY service_id",
                [],
            ))
            .await
            .unwrap()
            .rows(["machine_id", "service_id", "containers"])
            .unwrap()
            .into_iter()
            .map(|row| row.map(|value| value.as_str().unwrap().to_owned()))
            .collect::<Vec<_>>()
    };

    store
        .publish_ingress_routes(
            &machine,
            &BTreeMap::from([
                (api.clone(), IngressRouteChange::Upsert(ack(&["b", "a"]))),
                (web.clone(), IngressRouteChange::Upsert(ack(&[]))),
            ]),
        )
        .await
        .unwrap();
    store
        .publish_ingress_routes(
            &machine,
            &BTreeMap::from([
                (api, IngressRouteChange::Upsert(ack(&["c"]))),
                (web, IngressRouteChange::Delete),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(
        rows().await,
        vec![[
            machine.to_string(),
            "app/api".to_owned(),
            format!("[\"{}\"]", "c".repeat(64)),
        ]]
    );
    assert_eq!(
        store.ingress_routes().await.unwrap(),
        BTreeMap::from([(
            (machine, QualifiedService::parse("app/api").unwrap()),
            IngressRouteRow::Acked(ack(&["c"]))
        )])
    );
    task.abort();
}

#[tokio::test]
async fn remove_machine_deletes_route_ack() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    let (store, task) = identity_store(db).await;
    let gone = MachineId::parse("a".repeat(32)).unwrap();
    let kept = MachineId::parse("b".repeat(32)).unwrap();
    let api = QualifiedService::parse("app/api").unwrap();
    let ack = IngressRouteAck {
        containers: [ContainerId::parse("c".repeat(64)).unwrap()].into(),
    };
    for machine in [&gone, &kept] {
        store
            .publish_ingress_routes(
                machine,
                &BTreeMap::from([(api.clone(), IngressRouteChange::Upsert(ack.clone()))]),
            )
            .await
            .unwrap();
    }

    store.remove_machine(&gone).await.unwrap();

    assert_eq!(
        store.ingress_routes().await.unwrap(),
        BTreeMap::from([((kept, api), IngressRouteRow::Acked(ack))])
    );
    task.abort();
}

#[tokio::test]
async fn forget_leaves_a_tombstone_only_the_forgotten_identity_matches() {
    let (store, task) = identity_store(corrosion_db()).await;
    let (machine, local) = participating_record();
    let identity = local.management_secret().public_key();
    let other = crate::management::ManagementSecret::generate().public_key();
    let publication = store.machine_publication().await;
    let published = publication
        .publishable_machine(&local, RuntimeOverlay::default())
        .unwrap();
    assert_eq!(published.runtime.management_identity, Some(identity));
    publication.publish(&published).await.unwrap();
    drop(publication);
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::Listed
    );

    store.forget_machine(&machine.id).await.unwrap();
    assert_eq!(store.machine(machine.id.as_str()).await.unwrap(), None);
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::Forgotten
    );
    assert_eq!(
        store.forgotten(other).await.unwrap(),
        ForgottenAnswer::NoAnswer
    );

    // An Entry's Register writes the id back without an identity: Listed wins.
    store.publish_local_machine(&machine).await.unwrap();
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::Listed
    );
    store.forget_machine(&machine.id).await.unwrap();

    let mut twin = published.clone();
    twin.id = MachineId::parse("c".repeat(32)).unwrap();
    store.publish_local_machine(&twin).await.unwrap();
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::Listed
    );
    task.abort();
}

#[tokio::test]
async fn a_forgotten_machine_still_running_cannot_publish_itself_back() {
    let (store, task) = identity_store(corrosion_db()).await;
    let (machine, local) = participating_record();
    let identity = local.management_secret().public_key();
    let publication = store.machine_publication().await;
    let published = publication
        .publishable_machine(&local, RuntimeOverlay::default())
        .unwrap();
    publication.publish(&published).await.unwrap();
    let mut updated = published.clone();
    updated.runtime.running_builds += 1;
    publication.publish(&updated).await.unwrap();
    assert_eq!(
        store.machine(machine.id.as_str()).await.unwrap(),
        Some(updated.clone())
    );
    drop(publication);

    store.forget_machine(&machine.id).await.unwrap();
    store
        .machine_publication()
        .await
        .publish(&updated)
        .await
        .unwrap();

    assert_eq!(store.machine(machine.id.as_str()).await.unwrap(), None);
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::Forgotten
    );
    task.abort();
}

#[tokio::test]
async fn forget_of_a_row_without_an_identity_writes_no_tombstone() {
    let (store, task) = identity_store(corrosion_db()).await;
    let (machine, local) = participating_record();
    store.publish_local_machine(&machine).await.unwrap();

    store.forget_machine(&machine.id).await.unwrap();

    assert_eq!(store.machine(machine.id.as_str()).await.unwrap(), None);
    assert_eq!(
        store
            .forgotten(local.management_secret().public_key())
            .await
            .unwrap(),
        ForgottenAnswer::NoAnswer
    );
    task.abort();
}

#[tokio::test]
async fn leave_writes_no_tombstone() {
    let (store, task) = identity_store(corrosion_db()).await;
    let (machine, local) = participating_record();
    let identity = local.management_secret().public_key();
    let publication = store.machine_publication().await;
    let published = publication
        .publishable_machine(&local, RuntimeOverlay::default())
        .unwrap();
    publication.publish(&published).await.unwrap();
    drop(publication);

    store.remove_machine(&machine.id).await.unwrap();

    assert_eq!(store.machine(machine.id.as_str()).await.unwrap(), None);
    assert_eq!(
        store.forgotten(identity).await.unwrap(),
        ForgottenAnswer::NoAnswer
    );
    task.abort();
}

fn identity_container(machine_id: MachineId) -> ployz_core::ContainerObservation {
    serde_json::from_value(json!({
        "container_id": "c".repeat(64),
        "display_name": "service-c",
        "machine_id": machine_id,
        "namespace": "app",
        "kind": "service_container",
        "runtime": { "state": "created" },
        "resolved_spec": {
            "service_id": "d".repeat(32),
            "name": "service",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "busybox", "pull_policy": "missing" }
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn container_reads_reject_row_identity_and_owner_mismatches() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let container = identity_container(machine);
    for (id, owner) in [
        ("b".repeat(64), machine.to_string()),
        (container.container_id.to_string(), "b".repeat(32)),
        (container.container_id.to_string(), String::new()),
    ] {
        let db = corrosion_db();
        db.execute(
            "INSERT INTO containers (id, machine_id, container) VALUES (?, ?, ?)",
            rusqlite::params![id, owner, serde_json::to_string(&container).unwrap()],
        )
        .unwrap();
        let (store, task) = identity_store(db).await;
        let id = ployz_core::ContainerId::parse(id).unwrap();
        assert!(
            store.container(&id).await.is_err(),
            "single row accepted {id}/{owner}"
        );
        assert!(
            store.containers().await.is_err(),
            "list accepted {id}/{owner}"
        );
        if let Ok(owner) = MachineId::parse(&owner) {
            assert!(
                store
                    .machine_publication()
                    .await
                    .local_containers(&owner)
                    .await
                    .is_err(),
                "local inventory accepted {id}/{owner}"
            );
        }
        task.abort();
    }
}

#[tokio::test]
async fn volume_reads_reject_machine_qualified_identity_mismatches() {
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let volume = DockerVolume {
        id: DockerVolumeId {
            machine_id: machine,
            name: DockerVolumeName::parse("data").unwrap(),
        },
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    for (owner, name) in [
        (machine, "other"),
        (MachineId::parse("b".repeat(32)).unwrap(), "data"),
    ] {
        let db = corrosion_db();
        db.execute(
            "INSERT INTO volumes (machine_id, name, volume) VALUES (?, ?, ?)",
            rusqlite::params![
                owner.as_str(),
                name,
                serde_json::to_string(&volume).unwrap()
            ],
        )
        .unwrap();
        let (store, task) = identity_store(db).await;
        let id = DockerVolumeId {
            machine_id: owner,
            name: DockerVolumeName::parse(name).unwrap(),
        };
        assert!(
            store.volume(&id).await.is_err(),
            "single row accepted {owner}/{name}"
        );
        assert!(
            store.volumes().await.is_err(),
            "list accepted {owner}/{name}"
        );
        assert!(
            store
                .machine_publication()
                .await
                .local_volumes(&owner)
                .await
                .is_err(),
            "local inventory accepted {owner}/{name}"
        );
        task.abort();
    }
}

#[tokio::test]
async fn store_preserves_published_identities_and_keyed_incomplete_rows() {
    let machine: Machine = serde_json::from_value(json!({
        "id": "a".repeat(32), "name": "peer", "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": vec![1; 32], "advertised_endpoints": []
    }))
    .unwrap();
    let container = identity_container(machine.id);
    let volume = DockerVolume {
        id: DockerVolumeId {
            machine_id: machine.id,
            name: DockerVolumeName::parse("data").unwrap(),
        },
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    let incomplete_machine = MachineId::parse("b".repeat(32)).unwrap();
    let incomplete_container = ployz_core::ContainerId::parse("e".repeat(64)).unwrap();
    let unowned_container = ployz_core::ContainerId::parse("f".repeat(64)).unwrap();
    let incomplete_volume = DockerVolumeId {
        machine_id: machine.id,
        name: DockerVolumeName::parse("pending").unwrap(),
    };
    let db = corrosion_db();
    db.execute(
        "INSERT INTO machines (id) VALUES (?)",
        [incomplete_machine.as_str()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO containers (id, machine_id) VALUES (?, ?)",
        rusqlite::params![incomplete_container.as_str(), machine.id.as_str()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO containers (id) VALUES (?)",
        [unowned_container.as_str()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO volumes (machine_id, name) VALUES (?, ?)",
        rusqlite::params![machine.id.as_str(), incomplete_volume.name.as_str()],
    )
    .unwrap();
    let (store, task) = identity_store(db).await;
    store.publish_local_machine(&machine).await.unwrap();
    store.publish_container(&container).await.unwrap();
    store.publish_volume(&volume).await.unwrap();
    assert_eq!(
        store.machine(machine.id.as_str()).await.unwrap(),
        Some(machine.clone())
    );
    assert_eq!(
        store.container(&container.container_id).await.unwrap(),
        Some(container.clone())
    );
    assert_eq!(
        store.volume(&volume.id).await.unwrap(),
        Some(volume.clone())
    );
    assert_eq!(
        store.machine(incomplete_machine.as_str()).await.unwrap(),
        None
    );
    assert_eq!(store.container(&incomplete_container).await.unwrap(), None);
    assert_eq!(store.container(&unowned_container).await.unwrap(), None);
    assert_eq!(store.volume(&incomplete_volume).await.unwrap(), None);
    assert_eq!(
        store.machines().await.unwrap(),
        super::ReplicatedObservations {
            observations: vec![machine.clone()],
            incomplete_ids: vec![incomplete_machine]
        }
    );
    assert_eq!(
        store.containers().await.unwrap(),
        super::ReplicatedObservations {
            observations: vec![container.clone()],
            incomplete_ids: vec![incomplete_container, unowned_container]
        }
    );
    assert_eq!(
        store.volumes().await.unwrap(),
        super::ReplicatedObservations {
            observations: vec![volume.clone()],
            incomplete_ids: vec![incomplete_volume.clone()]
        }
    );
    let publication = store.machine_publication().await;
    let local_containers = publication.local_containers(&machine.id).await.unwrap();
    assert_eq!(
        local_containers.ids().copied().collect::<Vec<_>>(),
        vec![container.container_id, incomplete_container]
    );
    assert_eq!(
        local_containers.observations.get(&container.container_id),
        Some(&container)
    );
    let local_volumes = publication.local_volumes(&machine.id).await.unwrap();
    assert_eq!(local_volumes.get(&volume.id.name), Some(Some(&volume)));
    assert_eq!(local_volumes.get(&incomplete_volume.name), Some(None));
    let index = store
        .api()
        .query(super::Statement::new(
            "SELECT service_id, service_name FROM containers WHERE id = ?",
            [json!(container.container_id)],
        ))
        .await
        .unwrap();
    assert_eq!(
        index.rows(["service_id", "service_name"]).unwrap(),
        vec![[json!("d".repeat(32)), json!("service")]]
    );
    task.abort();
}

/// A newer Machine may add optional fields to any replicated body; this reader
/// must still recover every value it knows.
#[tokio::test]
async fn replicated_bodies_ignore_fields_from_newer_machines() {
    fn with_future_field(mut body: serde_json::Value) -> String {
        body.as_object_mut()
            .unwrap()
            .insert("added_by_a_newer_machine".into(), json!({"any": [1]}));
        body.to_string()
    }
    let machine: Machine = serde_json::from_value(json!({
        "id": "a".repeat(32), "name": "peer", "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": vec![1; 32], "advertised_endpoints": []
    }))
    .unwrap();
    let container = identity_container(machine.id);
    let volume = DockerVolume {
        id: DockerVolumeId {
            machine_id: machine.id,
            name: DockerVolumeName::parse("data").unwrap(),
        },
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let mut container_body = serde_json::to_value(&container).unwrap();
    container_body
        .get_mut("resolved_spec")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .insert("added_by_a_newer_machine".into(), json!(true));
    let db = corrosion_db();
    db.execute(
        "INSERT INTO machines (id, info) VALUES (?, ?)",
        rusqlite::params![
            machine.id.as_str(),
            with_future_field(serde_json::to_value(&machine).unwrap())
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO containers (id, machine_id, container) VALUES (?, ?, ?)",
        rusqlite::params![
            container.container_id.as_str(),
            machine.id.as_str(),
            with_future_field(container_body)
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO volumes (machine_id, name, volume) VALUES (?, ?, ?)",
        rusqlite::params![
            machine.id.as_str(),
            volume.id.name.as_str(),
            with_future_field(serde_json::to_value(&volume).unwrap())
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO certificates (hostname, body) VALUES (?, ?)",
        rusqlite::params![
            hostname.as_str(),
            with_future_field(json!({"last_error": "refused"}))
        ],
    )
    .unwrap();
    let (store, task) = identity_store(db).await;
    assert_eq!(
        store.machine(machine.id.as_str()).await.unwrap(),
        Some(machine)
    );
    assert_eq!(
        store.container(&container.container_id).await.unwrap(),
        Some(container)
    );
    assert_eq!(store.volume(&volume.id).await.unwrap(), Some(volume));
    assert_eq!(
        store.certificate_row(&hostname).await.unwrap().last_error(),
        Some("refused")
    );
    task.abort();
}

#[tokio::test]
async fn certificate_rows_round_trip_material_and_refusal_through_the_store() {
    let db = corrosion_db();
    let (store, task) = identity_store(db).await;
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let pair = rcgen::generate_simple_self_signed([hostname.as_str().to_owned()]).unwrap();
    let material =
        super::CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem())
            .unwrap();
    store
        .publish_certificate(&hostname, &material, ployz_core::ClusterRoute::Direct)
        .await
        .unwrap();
    store
        .record_certificate_error(&hostname, "policy refused")
        .await
        .unwrap();
    let row = store.certificate_row(&hostname).await.unwrap();
    assert_eq!(row.material(), Some(&material));
    assert_eq!(row.last_error(), Some("policy refused"));
    assert_eq!(
        store.certificates().await.unwrap(),
        BTreeMap::from([(CertificateHost::from(hostname), material)])
    );
    task.abort();
}

#[tokio::test]
async fn a_failed_renewal_keeps_its_material_and_backs_off() {
    let db = corrosion_db();
    let (store, task) = identity_store(db).await;
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let pair = rcgen::generate_simple_self_signed([hostname.as_str().to_owned()]).unwrap();
    let material =
        super::CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem())
            .unwrap();
    store
        .publish_certificate(&hostname, &material, ployz_core::ClusterRoute::Direct)
        .await
        .unwrap();
    // Another Machine's renewal is presenting its challenge meanwhile.
    let challenge = super::CertificateChallenge::parse(
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    store
        .publish_certificate_challenge(&hostname, &challenge)
        .await
        .unwrap();
    let clock = IssuanceClock::new(1, SystemTime::UNIX_EPOCH, IssuanceFailure::Authority);
    store
        .record_certificate_failure(&hostname, Some(&material), "ACME request failed", clock)
        .await
        .unwrap();
    let row = store.certificate_row(&hostname).await.unwrap();
    assert_eq!(row.material(), Some(&material));
    assert_eq!(row.challenge(), Some(&challenge));
    assert_eq!(row.last_error(), Some("ACME request failed"));
    assert_eq!(row.clock(), Some(clock));

    // Another Machine renewed meanwhile: its new material carries no stale failure.
    let pair = rcgen::generate_simple_self_signed([hostname.as_str().to_owned()]).unwrap();
    let renewed =
        super::CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem())
            .unwrap();
    store
        .publish_certificate(&hostname, &renewed, ployz_core::ClusterRoute::Direct)
        .await
        .unwrap();
    store
        .record_certificate_failure(&hostname, Some(&material), "ACME request failed", clock)
        .await
        .unwrap();
    let row = store.certificate_row(&hostname).await.unwrap();
    assert_eq!(row.material(), Some(&renewed));
    assert_eq!(row.last_error(), None);
    task.abort();
}

#[tokio::test]
async fn published_material_is_left_alone_by_acme_and_clear_hands_it_back() {
    let db = corrosion_db();
    let (store, task) = identity_store(db).await;
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let published_name = CertificateHost::from(hostname.clone());
    let material = |name: &str| {
        let pair = rcgen::generate_simple_self_signed([name.to_owned()]).unwrap();
        super::CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem())
            .unwrap()
    };
    let published = material("app.example.com");
    store
        .publish_certificate_material(&published_name, published.clone())
        .await
        .unwrap();

    store
        .publish_certificate(
            &hostname,
            &material("app.example.com"),
            ployz_core::ClusterRoute::Direct,
        )
        .await
        .unwrap();
    store
        .publish_certificate_challenge(
            &hostname,
            &super::CertificateChallenge::parse(
                "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
                "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            )
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .record_certificate_error(&hostname, "policy refused")
        .await
        .unwrap();
    assert_eq!(
        store.certificate_row(&hostname).await.unwrap(),
        super::CertificateRow::Published(published)
    );

    let wildcard = CertificateHost::parse("*.example.com").unwrap();
    let wildcard_material = material("*.example.com");
    store
        .publish_certificate_material(&wildcard, wildcard_material.clone())
        .await
        .unwrap();
    assert_eq!(
        store.certificates().await.unwrap().get("*.example.com"),
        Some(&wildcard_material)
    );

    store
        .clear_published_certificate(&published_name)
        .await
        .unwrap();
    assert_eq!(store.certificate(&hostname).await.unwrap(), None);
    let issued = material("app.example.com");
    store
        .publish_certificate(&hostname, &issued, ployz_core::ClusterRoute::Direct)
        .await
        .unwrap();
    store
        .clear_published_certificate(&published_name)
        .await
        .unwrap();
    assert_eq!(store.certificate(&hostname).await.unwrap(), Some(issued));
    task.abort();
}

#[tokio::test]
async fn container_rows_expose_rotation_and_gen_against_the_schema() {
    let (store, task) = identity_store(corrosion_db()).await;
    let machine = MachineId::parse("a".repeat(32)).unwrap();
    let observation = |id: &str, extra: serde_json::Value| -> ployz_core::ContainerObservation {
        let mut parts = json!({
            "container_id": id.repeat(64),
            "display_name": "api-1",
            "machine_id": machine,
            "namespace": "app",
            "kind": "service_container",
            "runtime": {"state": "running", "health": "healthy"},
            "resolved_spec": crate::docker::test_support::spec_with_sources(vec![]),
        });
        parts
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(parts).unwrap()
    };
    store
        .machine_publication()
        .await
        .apply_container_rows(
            &machine,
            &[],
            &[
                observation("b", json!({"rotation": "in", "gen": 2})),
                observation("c", json!({})),
            ],
        )
        .await
        .unwrap();

    let rows = store
        .api()
        .query(Statement::new(
            "SELECT rotation, gen FROM containers ORDER BY id",
            [],
        ))
        .await
        .unwrap()
        .rows(["rotation", "gen"])
        .unwrap();
    assert_eq!(
        rows,
        vec![
            [json!("in"), json!(2)],
            [serde_json::Value::Null, serde_json::Value::Null]
        ]
    );
    task.abort();
}
