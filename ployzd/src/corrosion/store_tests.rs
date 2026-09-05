use std::{
    collections::BTreeMap,
    net::TcpListener,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ployz_core::{
    DockerVolume, DockerVolumeId, DockerVolumeName, IngressHost, IngressProxyBackend,
    IssuanceClock, IssuanceFailure, Machine, MachineId,
};
use serde_json::json;

use super::{ReplicatedStore, fake_cluster, run_machine_publisher};
use crate::corrosion::ApiClient;
use crate::corrosion::publisher::founder_allocator_id;
use crate::corrosion::store::{
    AGE_ALLOCATOR, ALLOCATOR_ROW, CLAIM_FOUNDER_ALLOCATOR, STEAL_ALLOCATOR,
};
use crate::machine::{
    LocalMachine, LocalMachineBody, LocalMachinePrior, LocalMachineRecord, LocalMachineStore,
    ParticipationOrigin,
};
use crate::runtime_watch::RuntimeWatchSnapshot;
use tokio_util::sync::CancellationToken;

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
        "id": "b".repeat(32),
        "name": "joining",
        "subnet": "10.210.1.0/24",
        "public_key": public_key.0,
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();
    local
        .join(machine.clone(), vec![machine], BTreeMap::new(), None, None)
        .unwrap();
    let local = Arc::new(Mutex::new(local));

    let first = store.machine_publication().await;
    let clone = store.clone();
    let task_local = Arc::clone(&local);
    let (started, waiting) = tokio::sync::oneshot::channel();
    let second = tokio::spawn(async move {
        started.send(()).unwrap();
        let publication = clone.machine_publication().await;
        publication.complete_catch_up(&mut task_local.lock().unwrap())
    });
    waiting.await.unwrap();
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    local.lock().unwrap().begin_reset().unwrap();
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
    assert!(
        store
            .publish_founder_allocator(&id.machine_id)
            .await
            .is_err()
    );
    assert!(store.allocator().await.is_err());
    assert!(store.ingress_proxy_backend().await.is_err());
    assert!(
        store
            .publish_founding_ingress_proxy_backend(IngressProxyBackend::Caddy)
            .await
            .is_err()
    );
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
    let local = LocalMachine::new(
        Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap())),
        tokio::sync::watch::channel(false).0,
    );
    assert!(
        crate::runtime_watch::serve_replicated_runtime_watch(
            store,
            local,
            MachineId::random(),
            crate::global_reconcile::global_reconcile_observation_channel().1,
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
                "does not resolve",
                IssuanceClock::new(1, SystemTime::UNIX_EPOCH, IssuanceFailure::DoesNotResolve,),
            )
            .await
            .is_err()
    );
}

#[test]
fn founder_allocator_sql_is_quiet_and_does_not_overwrite() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    let id = "a".repeat(32);
    db.execute(CLAIM_FOUNDER_ALLOCATOR, rusqlite::params![id])
        .unwrap();
    let (value, quiet): (String, i64) = db
        .query_row(ALLOCATOR_ROW, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    assert_eq!(value, id);
    assert_eq!(quiet, 1);

    db.execute(CLAIM_FOUNDER_ALLOCATOR, rusqlite::params!["b".repeat(32)])
        .unwrap();
    let value: String = db
        .query_row(
            "SELECT value FROM cluster WHERE key = 'allocator'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, id);
}

#[test]
fn allocator_written_at_now_is_not_quiet() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    let id = "a".repeat(32);
    db.execute(STEAL_ALLOCATOR, rusqlite::params![id]).unwrap();
    let (value, quiet): (String, i64) = db
        .query_row(ALLOCATOR_ROW, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    assert_eq!(value, id);
    assert_eq!(quiet, 0);
}

#[test]
fn second_steal_overwrites_and_age_makes_quiet() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    db.execute(STEAL_ALLOCATOR, rusqlite::params!["a".repeat(32)])
        .unwrap();
    db.execute(STEAL_ALLOCATOR, rusqlite::params!["b".repeat(32)])
        .unwrap();
    let (value, quiet): (String, i64) = db
        .query_row(ALLOCATOR_ROW, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    assert_eq!(value, "b".repeat(32));
    assert_eq!(quiet, 0);

    db.execute(AGE_ALLOCATOR, []).unwrap();
    let (value, quiet): (String, i64) = db
        .query_row(ALLOCATOR_ROW, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    assert_eq!(value, "b".repeat(32));
    assert_eq!(quiet, 1);
}

#[test]
fn only_a_participating_founder_claims_allocator() {
    let (machine, joined) = participating_record();
    assert_eq!(founder_allocator_id(&joined), None);

    let founder = LocalMachineRecord::new(
        LocalMachineBody::Participating {
            machine: machine.clone(),
            origin: ParticipationOrigin::Founder {
                cluster: crate::machine::FoundingCluster {
                    network: "10.210.0.0/16".parse().unwrap(),
                    ingress_proxy_backend: IngressProxyBackend::Caddy,
                },
            },
        },
        joined.private_key().clone(),
    )
    .unwrap();
    assert_eq!(founder_allocator_id(&founder), Some(machine.id));

    let resetting = LocalMachineRecord::new(
        LocalMachineBody::Resetting {
            prior: Box::new(LocalMachinePrior::Participating {
                machine: machine.clone(),
                origin: ParticipationOrigin::Founder {
                    cluster: crate::machine::FoundingCluster {
                        network: "10.210.0.0/16".parse().unwrap(),
                        ingress_proxy_backend: IngressProxyBackend::Caddy,
                    },
                },
            }),
        },
        joined.private_key().clone(),
    )
    .unwrap();
    assert_eq!(founder_allocator_id(&resetting), None);

    let uninitialized = LocalMachineRecord::new(
        LocalMachineBody::Uninitialized { id: machine.id },
        joined.private_key().clone(),
    )
    .unwrap();
    assert_eq!(founder_allocator_id(&uninitialized), None);

    let joining = LocalMachineRecord::new(
        LocalMachineBody::Joining {
            machine: machine.clone(),
            bootstrap: vec![machine.clone()],
            min_store_version: BTreeMap::new(),
        },
        joined.private_key().clone(),
    )
    .unwrap();
    assert_eq!(founder_allocator_id(&joining), None);
}

#[tokio::test]
async fn allocator_row_names_the_machine() {
    let id = MachineId::parse("a".repeat(32)).unwrap();
    let (store, server) = fake_cluster::store().await;
    store.publish_founder_allocator(&id).await.unwrap();
    let row = store.allocator().await.unwrap().expect("allocator row");
    assert_eq!(row.machine_id, id);
    assert!(row.quiet);
    server.abort();
}

#[tokio::test]
async fn missing_allocator_row_is_none() {
    let (store, server) = fake_cluster::store().await;
    assert_eq!(store.allocator().await.unwrap(), None);
    server.abort();
}

#[tokio::test]
async fn invalid_allocator_value_is_an_error() {
    let (store, server) = fake_cluster::store_with_allocator_value("not-a-machine-id").await;
    assert!(store.allocator().await.is_err());
    server.abort();
}

#[tokio::test]
async fn ingress_proxy_backend_is_typed_immutable_cluster_authority() {
    let (store, server) = fake_cluster::store().await;
    let missing = store.ingress_proxy_backend().await.unwrap_err();
    assert!(missing.to_string().contains("missing"), "{missing}");

    store
        .publish_founding_ingress_proxy_backend(IngressProxyBackend::Caddy)
        .await
        .unwrap();
    assert_eq!(
        store.ingress_proxy_backend().await.unwrap(),
        IngressProxyBackend::Caddy
    );
    store
        .require_ingress_proxy_backend(IngressProxyBackend::Caddy)
        .await
        .unwrap();
    let mismatch = store
        .require_ingress_proxy_backend(IngressProxyBackend::Zentinel)
        .await
        .unwrap_err();
    assert!(mismatch.to_string().contains("not zentinel"), "{mismatch}");

    let changed = store
        .publish_founding_ingress_proxy_backend(IngressProxyBackend::Zentinel)
        .await
        .unwrap_err();
    assert!(changed.to_string().contains("caddy"), "{changed}");
    assert_eq!(
        store.ingress_proxy_backend().await.unwrap(),
        IngressProxyBackend::Caddy
    );
    server.abort();
}

#[tokio::test]
async fn ingress_proxy_backend_refuses_unrecognized_cluster_value() {
    let (store, server) = fake_cluster::store_with_ingress_proxy_backend_value("traefik").await;
    let error = store.ingress_proxy_backend().await.unwrap_err();
    assert!(error.to_string().contains("traefik"), "{error}");
    server.abort();
}

#[tokio::test]
async fn joining_machine_inherits_each_recognized_ingress_proxy_backend() {
    for backend in [
        IngressProxyBackend::Caddy,
        IngressProxyBackend::Zentinel,
        IngressProxyBackend::Envoy,
    ] {
        let (store, server) =
            fake_cluster::store_with_ingress_proxy_backend_value(backend.as_str()).await;
        let (data_dir, local) = joining_record();
        let shutdown = CancellationToken::new();
        let (participating, mut participating_rx) = tokio::sync::watch::channel(false);
        let publisher = tokio::spawn(run_machine_publisher(
            Some(store),
            Arc::clone(&local),
            participating,
            shutdown.clone(),
        ));

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            participating_rx.wait_for(|participating| *participating),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            local.lock().unwrap().record().phase(),
            ployz_core::LocalMachinePhase::Participating
        );

        shutdown.cancel();
        publisher.await.unwrap().unwrap();
        server.abort();
        drop(local);
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}

#[tokio::test]
async fn joining_machine_refuses_missing_or_unrecognized_ingress_proxy_backend() {
    for value in [None, Some("traefik")] {
        let (store, server) = match value {
            Some(value) => fake_cluster::store_with_ingress_proxy_backend_value(value).await,
            None => fake_cluster::store().await,
        };
        let (data_dir, local) = joining_record();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_machine_publisher(
                Some(store),
                Arc::clone(&local),
                tokio::sync::watch::channel(false).0,
                CancellationToken::new(),
            ),
        )
        .await
        .expect("invalid backend must refuse instead of waiting")
        .unwrap_err();

        assert!(
            value.map_or_else(
                || result.to_string().contains("missing"),
                |value| result.to_string().contains(value)
            ),
            "{result}"
        );
        assert_eq!(
            local.lock().unwrap().record().phase(),
            ployz_core::LocalMachinePhase::Joining
        );
        server.abort();
        drop(local);
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}

#[tokio::test]
async fn publication_guard_rechecks_the_local_phase() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let store = ReplicatedStore::new(
        ApiClient::new(listener.local_addr().unwrap(), &"a".repeat(64)).unwrap(),
    );
    let (machine, local) = participating_record();
    let publication = store.machine_publication().await;
    assert_eq!(publication.publishable_machine(&local), Some(machine));

    let LocalMachineBody::Participating {
        machine: body_machine,
        origin,
    } = local.body().clone()
    else {
        panic!("fixture is participating");
    };
    let local = LocalMachineRecord::new(
        LocalMachineBody::Resetting {
            prior: Box::new(LocalMachinePrior::Participating {
                machine: body_machine,
                origin,
            }),
        },
        local.private_key().clone(),
    )
    .unwrap();
    assert_eq!(publication.publishable_machine(&local), None);
}

fn participating_record() -> (Machine, LocalMachineRecord) {
    let machine: Machine = serde_json::from_value(json!({
        "id": "b".repeat(32),
        "name": "machine",
        "subnet": "10.210.1.0/24",
        "public_key": crate::network::WireGuardPrivateKey::from_bytes([0; 32]).public_key(),
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();
    let local = LocalMachineRecord::new(
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

fn joining_record() -> (std::path::PathBuf, Arc<Mutex<LocalMachineStore>>) {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-backend-join-{}", MachineId::random()));
    let mut local = LocalMachineStore::open(&data_dir).unwrap();
    let public_key = local.record().private_key().public_key();
    let machine: Machine = serde_json::from_value(json!({
        "id": "c".repeat(32),
        "name": "joining",
        "subnet": "10.210.2.0/24",
        "public_key": public_key.0,
        "advertised_endpoints": ["192.0.2.1:51820"],
    }))
    .unwrap();
    local
        .join(machine.clone(), vec![machine], BTreeMap::new(), None, None)
        .unwrap();
    (data_dir, Arc::new(Mutex::new(local)))
}
