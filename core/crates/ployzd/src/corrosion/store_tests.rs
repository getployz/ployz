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

use super::ReplicatedStore;
use crate::corrosion::ApiClient;
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
        "id": local.record().id(),
        "name": "joining",
        "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
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
        crate::runtime_watch::serve_replicated_runtime_watch(store, local, MachineId::random())
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
    assert_eq!(publication.publishable_machine(&local), None);
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
        value @ (serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Object(_)) => panic!("unsupported test parameter: {value}"),
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
                    value @ (rusqlite::types::ValueRef::Real(_)
                    | rusqlite::types::ValueRef::Blob(_)) => {
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

#[tokio::test]
async fn machine_reads_reject_document_identity_different_from_row_key() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
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
    let valid = crate::hosted_dns::Reservation::parse(
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
