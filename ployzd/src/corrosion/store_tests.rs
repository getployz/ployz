use std::{
    collections::BTreeMap,
    net::TcpListener,
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ployz_core::{
    DockerVolume, DockerVolumeId, DockerVolumeName, IngressHost, IssuanceClock, IssuanceFailure,
    Machine, MachineId,
};
use serde_json::json;

use super::{ReplicatedStore, fake_cluster};
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
// Exercise the store API against real SQL rows, including corrupt imported documents.
async fn identity_store(
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
            .map(|value| value.as_str().unwrap());
        let mut rows = statement.query(rusqlite::params_from_iter(params)).unwrap();
        let mut response = serde_json::to_vec(&json!({"columns": columns})).unwrap();
        let mut index = 0;
        while let Some(row) = rows.next().unwrap() {
            index += 1;
            let values = (0..columns.len())
                .map(|i| row.get::<_, Option<String>>(i).unwrap())
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
                    .map(|value| value.as_str().unwrap());
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

#[tokio::test]
async fn machine_reads_reject_document_identity_different_from_row_key() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    let key = MachineId::parse("a".repeat(32)).unwrap();
    let document = json!({"id": "b".repeat(32), "name": "peer", "subnet": "10.210.1.0/24", "public_key": vec![1; 32], "advertised_endpoints": []}).to_string();
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

fn identity_container(machine_id: MachineId) -> ployz_core::ContainerObservation {
    serde_json::from_value(json!({
        "container_id": "c".repeat(64),
        "display_name": "service-c",
        "machine_id": machine_id,
        "project_name": "app",
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
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
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
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
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
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
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

#[tokio::test]
async fn invalid_hosted_reservation_is_unavailable_and_explicit_release_recovers() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    db.execute(
        "INSERT INTO cluster (key, value) VALUES ('hosted_dns', ?)",
        [json!({"endpoint": "https://dns.example", "name": "", "token": ""}).to_string()],
    )
    .unwrap();
    let (store, server) = identity_store(db).await;
    let client = crate::hosted_dns::HostedDns::new();
    assert!(store.domain_reservation().await.is_err());
    assert!(client.domain(&store).await.is_err());
    let error = client.release_domain(&store).await.unwrap_err();
    assert!(error.to_string().contains("cleared locally"), "{error}");
    assert!(store.domain_reservation().await.unwrap().is_none());
    let valid = crate::hosted_dns::Reservation::new(
        "http://127.0.0.1:1".into(),
        "cluster.example".into(),
        "opaque-token".into(),
    )
    .unwrap();
    store.publish_domain_reservation(&valid).await.unwrap();
    assert_eq!(client.domain(&store).await.unwrap(), "cluster.example");
    assert_eq!(
        client.release_domain(&store).await.unwrap(),
        "cluster.example"
    );
    assert!(store.domain_reservation().await.unwrap().is_none());
    server.abort();
}
