use ployz_core::{LocalMachinePhase, MachineId, RegisterRequest, RpcErrorCode, WireGuardPublicKey};

use super::{EnrollPlan, LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner};
use crate::corrosion::{
    AdminClient, MachineView, ReplicatedStore, fake_cluster, run_machine_publisher,
};
use crate::sealing_key::SealingKey;

mod activation;
mod backfill;
mod doors;
mod harness;
mod join_approval;
mod policy;
mod recovery;
use harness::*;

#[tokio::test]
async fn register_rejects_an_uninitialized_machine() {
    let data_dir = std::env::temp_dir().join(format!(
        "ployzd-register-errors-{}",
        ployz_core::MachineId::random()
    ));
    let local =
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap());
    let empty = local
        .register(RegisterRequest {
            join_approval: None,
            join_confirmation: None,
            machine_id: MachineId::random(),
            assigned_subnet: None,
            advertised_endpoints: Vec::new(),
            ..request("peer", WireGuardPublicKey([1; 32]))
        })
        .await
        .unwrap_err();
    assert!(matches!(empty, LocalMachineError::NotParticipating));
    let uninitialized = local
        .register(request("peer", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert!(matches!(uninitialized, LocalMachineError::NotParticipating));
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A founder answers RPCs as soon as it restarts Participating, before its
/// publisher writes the Cluster network: callers must read that as not ready.
#[tokio::test]
async fn unpublished_cluster_network_reads_as_unavailable() {
    let (replicated, server) = fake_cluster::store_without_network().await;
    let (data_dir, store, founder) = open_store("ployzd-unpublished-network");
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated, None);

    let registered = rpc_register(&service, request("joiner", WireGuardPublicKey([1; 32])))
        .await
        .unwrap_err();
    assert_eq!(registered.code, RpcErrorCode::Unavailable, "{registered:?}");
    let listed = rpc_list_machines(&service).await.unwrap_err();
    assert_eq!(listed.code, RpcErrorCode::Unavailable, "{listed:?}");

    server.abort();
    drop(service);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_rpc_exact_replay_returns_the_original_joinable_assignment() {
    let (data_dir, store, founder) = open_store("ployzd-register-replay");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);

    let target_dir = std::env::temp_dir().join(format!(
        "ployzd-register-replay-target-{}",
        MachineId::random()
    ));
    let target_store = RecordOwner::spawn(LocalMachineStore::open(&target_dir).unwrap()).unwrap();
    let public_key = target_store.record().wireguard_private_key.public_key();
    let mut identity = request("joiner", public_key);
    identity.machine_id = target_store.record().id();

    let first = rpc_register(&service, identity.clone()).await.unwrap();
    let replay = rpc_register(&service, identity).await.unwrap();

    assert_eq!(replay.assigned_machine.id, first.assigned_machine.id);
    assert_eq!(
        replay.assigned_machine.subnet,
        first.assigned_machine.subnet
    );
    assert_eq!(replicated.machines().await.unwrap().observations.len(), 2);

    let target = LocalMachine::new(target_store);
    target
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: replay,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(target.record().phase(), LocalMachinePhase::Joining);

    server.abort();
    drop(service);
    drop(target);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(target_dir);
}

#[tokio::test]
async fn register_does_not_reconstruct_membership_while_joining() {
    let (entry, replicated, _founder, data_dir, server) = participating().await;
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-register-joining-{}", MachineId::random()));
    let joiner_store = RecordOwner::spawn(LocalMachineStore::open(&joiner_dir).unwrap()).unwrap();
    let public_key = joiner_store.record().wireguard_private_key.public_key();
    let mut identity = request("peer", public_key);
    identity.machine_id = joiner_store.record().id();
    let registered = entry.register(identity).await.unwrap();
    let joiner = LocalMachine::new(joiner_store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    joiner
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: registered,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(joiner.record().phase(), LocalMachinePhase::Joining);

    let error = joiner
        .register(request("peer", public_key))
        .await
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotParticipating));

    server.abort();
    drop(entry);
    drop(joiner);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

#[tokio::test]
async fn register_carries_the_entrys_devices_to_the_joiner() {
    let (entry, replicated, _founder, data_dir, server) = participating().await;
    let device = ployz_core::DeviceKey::from_bytes([5; 32]);
    entry
        .set_device(ployz_core::SetDeviceRequest::Approve { key: device })
        .await
        .unwrap();
    let joiner_dir =
        std::env::temp_dir().join(format!("ployzd-register-devices-{}", MachineId::random()));
    let joiner_store = RecordOwner::spawn(LocalMachineStore::open(&joiner_dir).unwrap()).unwrap();
    let mut identity = request(
        "peer",
        joiner_store.record().wireguard_private_key.public_key(),
    );
    identity.machine_id = joiner_store.record().id();
    let registered = entry.register(identity).await.unwrap();
    assert_eq!(registered.devices, [device]);
    let joiner = LocalMachine::new(joiner_store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    joiner
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Member {
                registration: registered,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .await
        .unwrap();
    assert_eq!(
        joiner.record().management_scope(&device.into()),
        Some(ployz_core::ManagementScope::Full)
    );

    server.abort();
    drop(entry);
    drop(joiner);
    let _ = std::fs::remove_dir_all(data_dir);
    let _ = std::fs::remove_dir_all(joiner_dir);
}

#[tokio::test]
async fn client_assignment_publishes_through_entry_and_a_changed_register_overwrites_it() {
    let (local, replicated, founder, data_dir, server) = participating().await;
    let id = MachineId::random();
    let mut identity = request("edge", WireGuardPublicKey([21; 32]));
    identity.machine_id = id;
    identity.assigned_subnet = Some("10.210.1.0/24".parse().unwrap());
    let first = local.register(identity.clone()).await.unwrap();
    assert_eq!(first.assigned_machine.id, id);
    assert_eq!(first.visible_peers, vec![founder]);
    assert_eq!(
        replicated.machine(id.as_str()).await.unwrap(),
        Some(first.assigned_machine.clone())
    );
    let replayed = local.register(identity.clone()).await.unwrap();
    assert_eq!(
        ployz_core::Registered {
            sealing_key: first.sealing_key.clone(),
            ..replayed
        },
        first
    );
    let mut rekeyed = identity;
    rekeyed.public_key = WireGuardPublicKey([22; 32]);
    let overwritten = local.register(rekeyed).await.unwrap().assigned_machine;
    assert_eq!(overwritten.public_key, WireGuardPublicKey([22; 32]));
    assert_eq!(
        replicated.machine(id.as_str()).await.unwrap(),
        Some(overwritten)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A plain add of a forgotten Server that still holds its old id reaches Register on
/// an Entry before the CLI refuses it. The Register must not list that id again (#3613).
#[tokio::test]
async fn register_of_a_forgotten_id_does_not_list_it_again() {
    use crate::corrosion::store_tests::{corrosion_db, identity_store};

    let db = corrosion_db();
    // Register answers with the store's version, which cr-sqlite and Corrosion keep here.
    db.execute_batch(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);",
    )
    .unwrap();
    let (replicated, server) = identity_store(db).await;
    replicated
        .publish_cluster_network("10.210.0.0/16".parse().unwrap())
        .await
        .unwrap();
    let (data_dir, store, founder) = open_store("ployzd-register-forgotten");
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));

    let forgotten = request("gone", WireGuardPublicKey([31; 32]));
    let mut published = local
        .register(forgotten.clone())
        .await
        .unwrap()
        .assigned_machine;
    published.runtime.management_identity =
        Some(crate::management::ManagementSecret::generate().public_key());
    replicated.publish_local_machine(&published).await.unwrap();
    replicated.forget_machine(&published.id).await.unwrap();

    local.register(forgotten).await.unwrap();
    assert_eq!(
        replicated.machine(published.id.as_str()).await.unwrap(),
        None
    );

    let mut fresh = request("back", WireGuardPublicKey([32; 32]));
    fresh.assigned_subnet = Some("10.210.2.0/24".parse().unwrap());
    let listed = local.register(fresh).await.unwrap().assigned_machine;
    assert_eq!(
        replicated.machine(listed.id.as_str()).await.unwrap(),
        Some(listed)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A failed add rerun on the same host Registers the same id again, even after the
/// Registered row was forgotten, and finishes that Server (#3342).
#[tokio::test]
async fn register_after_forgetting_a_registered_row_lists_it_again() {
    use crate::corrosion::store_tests::{corrosion_db, identity_store};

    let db = corrosion_db();
    // Register answers with the store's version, which cr-sqlite and Corrosion keep here.
    db.execute_batch(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);",
    )
    .unwrap();
    let (replicated, server) = identity_store(db).await;
    replicated
        .publish_cluster_network("10.210.0.0/16".parse().unwrap())
        .await
        .unwrap();
    let (data_dir, store, founder) = open_store("ployzd-register-rerun-after-forget");
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));

    let rerun = request("rerun", WireGuardPublicKey([33; 32]));
    let registered = local
        .register(rerun.clone())
        .await
        .unwrap()
        .assigned_machine;
    replicated.forget_machine(&registered.id).await.unwrap();

    local.register(rerun).await.unwrap();
    assert_eq!(
        replicated.machine(registered.id.as_str()).await.unwrap(),
        Some(registered.clone())
    );
    let mut joined = registered;
    joined.runtime.management_identity =
        Some(crate::management::ManagementSecret::generate().public_key());
    replicated.publish_local_machine(&joined).await.unwrap();
    assert_eq!(
        replicated.machine(joined.id.as_str()).await.unwrap(),
        Some(joined)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// An Entry over the SQLite schema Corrosion runs, so Register's SQL guards apply.
async fn sqlite_entry(
    prefix: &str,
) -> (
    LocalMachine,
    ReplicatedStore,
    std::path::PathBuf,
    tokio::task::JoinHandle<()>,
) {
    use crate::corrosion::store_tests::{corrosion_db, identity_store};

    let db = corrosion_db();
    // Register answers with the store's version, which cr-sqlite and Corrosion keep here.
    db.execute_batch(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);",
    )
    .unwrap();
    let (replicated, server) = identity_store(db).await;
    replicated
        .publish_cluster_network("10.210.0.0/16".parse().unwrap())
        .await
        .unwrap();
    let (data_dir, store, founder) = open_store(prefix);
    replicated.publish_local_machine(&founder).await.unwrap();
    let local = LocalMachine::new(store).with_cluster(Some((
        replicated.clone(),
        AdminClient::new("/no/such/ployz-admin.sock"),
    )));
    (local, replicated, data_dir, server)
}

/// A Register that reaches an Entry after its Server joined, from Cloud on a stale
/// snapshot or a rerun of `ployz server add`, leaves the row that Server published.
#[tokio::test]
async fn register_keeps_a_row_its_machine_already_published() {
    let (local, replicated, data_dir, server) = sqlite_entry("ployzd-register-joined").await;

    let identity = request("joiner", WireGuardPublicKey([41; 32]));
    local.register(identity.clone()).await.unwrap();
    let mut rekeyed = identity;
    rekeyed.public_key = WireGuardPublicKey([42; 32]);
    let assigned = local
        .register(rekeyed.clone())
        .await
        .unwrap()
        .assigned_machine;
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(assigned.clone())
    );

    let mut joined = assigned;
    joined.runtime.daemon_version = "0.3.0".into();
    joined.runtime.management_identity =
        Some(crate::management::ManagementSecret::generate().public_key());
    replicated.publish_local_machine(&joined).await.unwrap();
    local.register(rekeyed).await.unwrap();
    assert_eq!(
        replicated.machine(joined.id.as_str()).await.unwrap(),
        Some(joined.clone())
    );

    joined.runtime.running_builds += 1;
    replicated.publish_local_machine(&joined).await.unwrap();
    assert_eq!(
        replicated.machine(joined.id.as_str()).await.unwrap(),
        Some(joined)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A CLI Register carries the Server's daemon version from its token before that
/// Server joins. Another Register still replaces the row, and answers what it stored.
#[tokio::test]
async fn register_replaces_a_pending_row_that_carries_a_daemon_version() {
    let (local, replicated, data_dir, server) = sqlite_entry("ployzd-register-pending").await;

    let mut identity = request("joiner", WireGuardPublicKey([41; 32]));
    identity.runtime.daemon_version = "0.3.16".into();
    local.register(identity.clone()).await.unwrap();
    let rekeyed = RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        public_key: WireGuardPublicKey([42; 32]),
        ..identity
    };
    let assigned = local.register(rekeyed).await.unwrap().assigned_machine;
    assert_eq!(assigned.public_key, WireGuardPublicKey([42; 32]));
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(assigned)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A daemon without the `self_published` mark wrote rows that don't show who wrote
/// them: its own publish, and a Register an older Entry stored with the identity the
/// request named. A Register replaces such a row even under the same key, so the row
/// names no identity, and forgetting it leaves a tombstone the next Register takes back.
#[tokio::test]
async fn register_replaces_a_row_without_the_mark_and_takes_back_its_forget() {
    use crate::corrosion::store_tests::store_as_an_older_daemon;

    let (local, replicated, data_dir, server) = sqlite_entry("ployzd-register-unmarked").await;

    let identity = request("joiner", WireGuardPublicKey([41; 32]));
    let mut older = local
        .register(identity.clone())
        .await
        .unwrap()
        .assigned_machine;
    older.runtime.daemon_version = "0.3.16".into();
    older.runtime.management_identity =
        Some(crate::management::ManagementSecret::generate().public_key());
    store_as_an_older_daemon(&replicated, &older).await;

    let assigned = local
        .register(identity.clone())
        .await
        .unwrap()
        .assigned_machine;
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(assigned.clone())
    );
    replicated.forget_machine(&assigned.id).await.unwrap();
    local.register(identity).await.unwrap();
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(assigned)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

/// A Register that names another key than the joined row replaces it, and the joined
/// Machine's own publish writes it back.
#[tokio::test]
async fn a_register_with_another_key_over_a_published_row_is_replaced_and_its_publisher_restores_it()
 {
    let (local, replicated, data_dir, server) = sqlite_entry("ployzd-register-other-key").await;

    let identity = request("joiner", WireGuardPublicKey([41; 32]));
    let mut joined = local
        .register(identity.clone())
        .await
        .unwrap()
        .assigned_machine;
    joined.runtime.daemon_version = "0.3.16".into();
    joined.runtime.management_identity =
        Some(crate::management::ManagementSecret::generate().public_key());
    replicated.publish_local_machine(&joined).await.unwrap();

    let rekeyed = RegisterRequest {
        join_approval: None,
        join_confirmation: None,
        public_key: WireGuardPublicKey([43; 32]),
        ..identity
    };
    let assigned = local.register(rekeyed).await.unwrap().assigned_machine;
    assert_eq!(
        replicated.machine(assigned.id.as_str()).await.unwrap(),
        Some(assigned)
    );

    replicated.publish_local_machine(&joined).await.unwrap();
    assert_eq!(
        replicated.machine(joined.id.as_str()).await.unwrap(),
        Some(joined)
    );
    server.abort();
    drop(local);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn register_requires_a_valid_assignment_and_preserves_the_row_on_refusal() {
    let (data_dir, store, founder) = open_store("ployzd-register-publication");
    let (replicated, server) = fake_cluster::store().await;
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated.clone(), None);
    let mut identity = request("peer", WireGuardPublicKey([1; 32]));
    identity.assigned_subnet = None;
    assert_eq!(
        rpc_register(&service, identity.clone())
            .await
            .unwrap_err()
            .code,
        RpcErrorCode::InvalidArgument
    );
    assert_eq!(
        replicated.machines().await.unwrap().observations,
        vec![founder]
    );
    identity.assigned_subnet = Some("10.210.1.0/24".parse().unwrap());
    let first = rpc_register(&service, identity.clone()).await.unwrap();
    for change in 0..2 {
        let mut invalid = identity.clone();
        match change {
            0 => invalid.advertised_endpoints.clear(),
            _ => invalid.assigned_subnet = Some("10.211.1.0/24".parse().unwrap()),
        }
        assert!(rpc_register(&service, invalid).await.is_err());
        assert_eq!(
            replicated
                .machine(identity.machine_id.as_str())
                .await
                .unwrap(),
            Some(first.assigned_machine.clone())
        );
    }
    server.abort();
    drop(service);
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn replayed_enroll_republishes_runtime_over_a_register_row() {
    let (replicated, server) = fake_cluster::store_with_subscriptions().await;
    let cluster = || {
        Some((
            replicated.clone(),
            AdminClient::new("/no/such/ployz-admin.sock"),
        ))
    };
    let (entry_dir, entry_store, founder) = open_store("ployzd-register-republish");
    replicated.publish_local_machine(&founder).await.unwrap();
    let entry = LocalMachine::new(entry_store.clone()).with_cluster(cluster());

    let target_dir = std::env::temp_dir().join(format!(
        "ployzd-register-republish-target-{}",
        MachineId::random()
    ));
    let target_store = RecordOwner::spawn(LocalMachineStore::open(&target_dir).unwrap()).unwrap();
    let mut identity = request(
        "joiner",
        target_store.record().wireguard_private_key.public_key(),
    );
    identity.machine_id = target_store.record().id();
    let target = LocalMachine::new(target_store.clone()).with_cluster(cluster());
    let enroll = |registration| ployz_core::EnrollRequest {
        role: ployz_core::EnrollAs::Member { registration },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    target
        .enroll(enroll(entry.register(identity.clone()).await.unwrap()))
        .await
        .unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let view = MachineView::start(replicated.clone(), shutdown.clone());
    let publisher = tokio::spawn(run_machine_publisher(
        Some((replicated.clone(), view.clone())),
        target_store,
        crate::corrosion::LiveRuntime::unobserved(),
        shutdown.clone(),
    ));
    // The founder publishes the Cluster network; a Machines view change for
    // another Machine's row must not make it write again.
    let entry_publisher = tokio::spawn(run_machine_publisher(
        Some((replicated.clone(), view.clone())),
        entry_store,
        crate::corrosion::LiveRuntime::unobserved(),
        shutdown.clone(),
    ));
    let network: ipnet::Ipv4Net = "10.210.0.0/16".parse().unwrap();
    let marker: ipnet::Ipv4Net = "10.0.0.0/8".parse().unwrap();
    replicated.publish_cluster_network(marker).await.unwrap();
    eventually(|| async { replicated.cluster_network().await.unwrap() == Some(network) })
        .await
        .expect("the founder publishes the Cluster network");
    let served = ployz_core::served_capabilities();
    assert!(!served.is_empty());
    let capabilities = || async {
        replicated
            .machine(identity.machine_id.as_str())
            .await
            .unwrap()
            .map(|machine| machine.runtime.capabilities)
    };
    let republished = || eventually(|| async { capabilities().await.as_ref() == Some(&served) });
    republished()
        .await
        .expect("the joined Machine publishes itself");
    let published = replicated
        .machine(identity.machine_id.as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !published.runtime.daemon_version.is_empty(),
        "a self-published row names its daemon version; an empty one reads as Unarrived"
    );
    // Settle the way a live cluster has: the Machines view holds the served row.
    let mut settled = view.watch();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        settled.wait_for(|snapshot| {
            snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.observations.iter().any(|machine| {
                    machine.id == identity.machine_id && machine.runtime.capabilities == served
                })
            })
        }),
    )
    .await
    .expect("the Machines view sees the served row")
    .unwrap();
    assert_eq!(target.record().phase(), LocalMachinePhase::Participating);
    replicated.publish_cluster_network(marker).await.unwrap();

    // `ployz server add` registers through the entry with an empty runtime,
    // then resends Enroll to a Machine that already participates.
    let registration = entry.register(identity.clone()).await.unwrap();
    assert!(
        registration
            .assigned_machine
            .runtime
            .capabilities
            .is_empty()
    );
    let replayed = target.enroll(enroll(registration)).await.unwrap();
    assert!(matches!(replayed, ployz_core::Enrolled::Enrolled { .. }));
    assert!(
        republished().await.is_ok(),
        "the row kept the Register runtime: {:?}",
        capabilities().await
    );
    // The founder saw the Register and restore as Machines view changes too.
    // real-wait: gives the founder's publisher time to react over the fake Cluster's
    // HTTP API; a rewrite that must not happen has no event to await.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        replicated.cluster_network().await.unwrap(),
        Some(marker),
        "a Machines view change rewrote the Cluster network"
    );

    shutdown.cancel();
    publisher.await.unwrap().unwrap();
    entry_publisher.await.unwrap().unwrap();
    server.abort();
    drop(entry);
    drop(target);
    let _ = std::fs::remove_dir_all(entry_dir);
    let _ = std::fs::remove_dir_all(target_dir);
}

async fn eventually<F, Fut>(mut done: F) -> Result<(), tokio::time::error::Elapsed>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !done().await {
            // real-wait: polls the fake Cluster store over its HTTP API, which signals no
            // row write.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
}

struct Joiner {
    dir: std::path::PathBuf,
    machine: LocalMachine,
    identity: RegisterRequest,
}

async fn sealing_entry(prefix: &str) -> (LocalMachine, std::path::PathBuf, Joiner) {
    let (replicated, _server) = fake_cluster::store().await;
    let cluster = || {
        Some((
            replicated.clone(),
            AdminClient::new("/no/such/ployz-admin.sock"),
        ))
    };
    let (entry_dir, entry_store, founder) = open_store(prefix);
    replicated.publish_local_machine(&founder).await.unwrap();
    let entry = LocalMachine::new(entry_store).with_cluster(cluster());
    let dir = std::env::temp_dir().join(format!("{prefix}-joiner-{}", MachineId::random()));
    let store = RecordOwner::spawn(LocalMachineStore::open(&dir).unwrap()).unwrap();
    let mut identity = request("joiner", store.record().wireguard_private_key.public_key());
    identity.machine_id = store.record().id();
    let machine = LocalMachine::new(store).with_cluster(cluster());
    (
        entry,
        entry_dir,
        Joiner {
            dir,
            machine,
            identity,
        },
    )
}

fn member(registration: ployz_core::Registered) -> ployz_core::EnrollRequest {
    ployz_core::EnrollRequest {
        role: ployz_core::EnrollAs::Member { registration },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

async fn sealing(machine: &LocalMachine) -> Option<ployz_core::Sealing> {
    machine
        .inspect(ployz_core::InspectRequest {
            advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                "192.0.2.9:51820".parse().unwrap(),
            )],
            token: None,
            include_rtts: false,
            include_storage: false,
            telemetry: ployz_core::InspectTelemetry::None,
        })
        .await
        .unwrap()
        .sealing
}

#[tokio::test]
async fn enroll_hands_the_joiner_the_founders_sealing_key() {
    let (entry, entry_dir, joiner) = sealing_entry("ployzd-enroll-sealing").await;
    let founders = SealingKey::load(&entry_dir)
        .unwrap()
        .expect("the founder holds a key");

    let registration = entry.register(joiner.identity).await.unwrap();
    joiner.machine.enroll(member(registration)).await.unwrap();

    assert_eq!(
        SealingKey::load(&joiner.dir).unwrap(),
        Some(founders.clone())
    );
    assert_eq!(
        sealing(&joiner.machine).await,
        Some(ployz_core::Sealing::Held {
            public_key: founders.public_key()
        })
    );
    assert_eq!(sealing(&entry).await, sealing(&joiner.machine).await);
}

#[tokio::test]
async fn enroll_from_an_issuer_without_a_sealing_key_leaves_the_joiner_needing_access() {
    let (entry, _entry_dir, joiner) = sealing_entry("ployzd-enroll-old-issuer").await;

    let mut registration = entry.register(joiner.identity).await.unwrap();
    registration.sealing_key = None;
    joiner.machine.enroll(member(registration)).await.unwrap();

    assert_eq!(SealingKey::load(&joiner.dir).unwrap(), None);
    assert_eq!(
        sealing(&joiner.machine).await,
        Some(ployz_core::Sealing::NeedsAccess)
    );
}

#[tokio::test]
async fn enroll_refuses_a_sealing_key_sealed_to_another_machine() {
    let (entry, _entry_dir, joiner) = sealing_entry("ployzd-enroll-wrong-recipient").await;

    let mut registration = entry.register(joiner.identity).await.unwrap();
    registration.sealing_key = Some(
        SealingKey::generate()
            .seal_to(&crate::network::WireGuardPrivateKey::generate().public_key())
            .unwrap(),
    );
    assert!(joiner.machine.enroll(member(registration)).await.is_err());

    assert_eq!(SealingKey::load(&joiner.dir).unwrap(), None);
    assert_eq!(
        joiner.machine.record().phase(),
        LocalMachinePhase::Uninitialized
    );
}

#[tokio::test]
async fn a_corrupt_sealing_key_reads_as_needs_access_and_still_admits_the_joiner() {
    let (entry, entry_dir, joiner) = sealing_entry("ployzd-register-corrupt-key").await;
    std::fs::write(entry_dir.join("sealing.key"), b"not a key").unwrap();

    let registration = entry.register(joiner.identity).await.unwrap();
    assert_eq!(registration.sealing_key, None);
    joiner.machine.enroll(member(registration)).await.unwrap();

    assert_eq!(
        sealing(&entry).await,
        Some(ployz_core::Sealing::NeedsAccess)
    );
    assert_eq!(
        sealing(&joiner.machine).await,
        Some(ployz_core::Sealing::NeedsAccess)
    );
}

#[tokio::test]
async fn register_refuses_a_low_order_wireguard_key_instead_of_sealing_to_it() {
    let (entry, _entry_dir, mut joiner) = sealing_entry("ployzd-register-low-order").await;
    joiner.identity.public_key = WireGuardPublicKey([0; 32]);

    let error = entry.register(joiner.identity).await.unwrap_err();

    assert!(matches!(error, LocalMachineError::Seal(_)));
}

#[test]
fn enroll_refuses_a_second_founding_and_keeps_the_sealing_key() {
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-enroll-refound-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    let found = |name: &str| ployz_core::EnrollRequest {
        role: ployz_core::EnrollAs::Founder {
            initial_policy: Default::default(),
            name: ployz_core::MachineName::parse(name).unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                "192.0.2.1:51820".parse().unwrap(),
            )],
            grant: None,
        },
        wireguard_mtu: None,
        pinned: Default::default(),
    };
    assert_eq!(store.enroll(found("edge")).unwrap(), EnrollPlan::Fresh);
    let founded = SealingKey::load(&data_dir)
        .unwrap()
        .expect("the founder holds a key");

    for name in ["edge", "other"] {
        let plan = store.enroll(found(name)).unwrap();
        assert_ne!(plan, EnrollPlan::Fresh, "{name}");
        assert_eq!(SealingKey::load(&data_dir).unwrap(), Some(founded.clone()));
    }
}
