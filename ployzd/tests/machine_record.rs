mod test_dir;

use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, InspectRequest, LocalMachinePhase, Machine, MachineId, MachineName,
    MachineRpc, MachineRuntime, MachineSubnet, MachineUpdate, PublicIpUpdate, ResetRequest,
    RpcErrorCode, RpcResponseBody, SelectedEndpoint, op,
};
use ployzd::{
    machine::{LocalMachineRecord, LocalMachineStore, StoreError},
    rpc::MachineService,
};

use test_dir::TestDir;

#[test]
fn machine_record_is_created_once_and_reopened_with_private_permissions() {
    let dir = TestDir::new("ployzd-state");
    let created = LocalMachineStore::open(&dir.0).unwrap();
    let machine_id = created.record().id;

    assert_eq!(created.record().phase, LocalMachinePhase::Uninitialized);
    assert!(created.record().wireguard_private_key.is_some());
    assert!(created.record().min_store_version.is_empty());
    assert_eq!(
        fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
        0o711
    );
    assert_eq!(
        fs::metadata(dir.0.join("machine.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(created);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().id, machine_id);
    assert_eq!(reopened.record().phase, LocalMachinePhase::Uninitialized);
}

#[test]
fn initialize_and_join_persist_the_only_supported_transitions() {
    let first_dir = TestDir::new("ployzd-initialize");
    let mut first = LocalMachineStore::open(&first_dir.0).unwrap();
    let initialized = first
        .initialize(
            MachineName::parse("first").unwrap(),
            "10.210.0.0/16".parse().unwrap(),
            Some("203.0.113.1".parse().unwrap()),
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            Some(1400),
        )
        .unwrap();
    assert_eq!(first.record().phase, LocalMachinePhase::Participating);
    assert_eq!(first.record().machine.as_ref(), Some(&initialized));
    assert_eq!(initialized.public_ip, Some("203.0.113.1".parse().unwrap()));
    assert_eq!(
        first.record().cluster_network.unwrap().to_string(),
        "10.210.0.0/16"
    );
    assert!(
        first
            .initialize(
                MachineName::parse("again").unwrap(),
                "10.210.0.0/16".parse().unwrap(),
                None,
                vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
                None,
            )
            .is_err()
    );

    let second_dir = TestDir::new("ployzd-join");
    let mut second = LocalMachineStore::open(&second_dir.0).unwrap();
    let public_key = second
        .record()
        .wireguard_private_key
        .as_ref()
        .unwrap()
        .public_key();
    let assigned = Machine {
        id: MachineId::random(),
        name: MachineName::parse("second").unwrap(),
        subnet: MachineSubnet("10.210.1.0/24".parse().unwrap()),
        management_address: ployzd::network::management_address(public_key),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
    };
    second
        .join(
            assigned.clone(),
            vec![initialized.clone()],
            BTreeMap::from([("actor".into(), 4)]),
            None,
        )
        .unwrap();
    assert_eq!(second.record().id, assigned.id);
    assert_eq!(second.record().phase, LocalMachinePhase::Joining);
    assert_eq!(second.record().bootstrap_machines, vec![initialized]);
    assert_eq!(second.record().min_store_version.get("actor"), Some(&4));
}

#[test]
fn reopening_a_participating_machine_refreshes_runtime_metadata() {
    let dir = TestDir::new("ployzd-runtime-refresh");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store
        .initialize(
            MachineName::parse("machine").unwrap(),
            "10.210.0.0/16".parse().unwrap(),
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
        )
        .unwrap();
    drop(store);

    let path = dir.0.join("machine.json");
    let mut stale: LocalMachineRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stale.machine.as_mut().unwrap().runtime = MachineRuntime {
        daemon_version: "stale".into(),
        docker_version: "stale".into(),
        hostname: "stale".into(),
        architecture: "stale".into(),
        os_pretty_name: "stale".into(),
        kernel_version: "stale".into(),
    };
    fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    let expected = ployzd::machine::local_runtime();
    assert_eq!(
        reopened.record().machine.as_ref().unwrap().runtime,
        expected
    );
    let persisted: LocalMachineRecord = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted.machine.unwrap().runtime, expected);
}

#[test]
fn machine_update_is_atomic_and_durable() {
    let dir = TestDir::new("ployzd-update");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let original = store
        .initialize(
            MachineName::parse("before").unwrap(),
            "10.210.0.0/16".parse().unwrap(),
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
        )
        .unwrap();
    let endpoints = vec![AdvertisedEndpoint("198.51.100.2:6000".parse().unwrap())];
    let updated = store
        .update(
            MachineUpdate {
                name: Some(MachineName::parse("after").unwrap()),
                public_ip: PublicIpUpdate::Set("203.0.113.7".parse().unwrap()),
                advertised_endpoints: Some(endpoints.clone()),
            },
            std::slice::from_ref(&original),
        )
        .unwrap();

    assert_eq!(updated.id, original.id);
    assert_eq!(updated.subnet, original.subnet);
    assert_eq!(updated.management_address, original.management_address);
    assert_eq!(updated.public_key, original.public_key);
    assert_eq!(updated.advertised_endpoints, endpoints);
    drop(store);

    let mut reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().machine.as_ref(), Some(&updated));
    reopened.begin_reset().unwrap();
    assert!(matches!(
        reopened.update(
            MachineUpdate {
                name: Some(MachineName::parse("too-late").unwrap()),
                ..Default::default()
            },
            &[updated],
        ),
        Err(StoreError::NotParticipating)
    ));
}

#[test]
fn resetting_state_is_durable_and_completed_on_the_next_open() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let old_machine_id = store.record().id;

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
    store.begin_reset().unwrap();
    assert_eq!(store.record().phase, LocalMachinePhase::Resetting);
    assert!(store.begin_reset().is_err());
    drop(store);

    let persisted: LocalMachineRecord =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.id, old_machine_id);
    assert_eq!(persisted.phase, LocalMachinePhase::Resetting);

    let recreated = LocalMachineStore::open(&dir.0).unwrap();
    assert_ne!(recreated.record().id, old_machine_id);
    assert_eq!(recreated.record().phase, LocalMachinePhase::Uninitialized);
}

#[tokio::test]
async fn repeated_reset_returns_a_typed_conflict() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let service = MachineService::new(Arc::new(Mutex::new(store)), reset);

    let response = service
        .reset(tonic::Request::new(
            op::Reset::into_request(ResetRequest {}).encode().unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();

    assert!(matches!(
        response.body,
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::Conflict
    ));
}

#[tokio::test]
async fn inspect_keeps_the_v1_key_and_endpoint_payload() {
    let dir = TestDir::new("ployzd-state");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let public_key = store
        .record()
        .wireguard_private_key
        .as_ref()
        .unwrap()
        .public_key();
    let endpoint = AdvertisedEndpoint("192.0.2.8:51820".parse().unwrap());
    let (reset, _) = tokio::sync::watch::channel(false);
    let service = MachineService::new(Arc::new(Mutex::new(store)), reset);

    let details = service
        .inspect(tonic::Request::new(
            op::Inspect::into_request(InspectRequest {
                advertised_endpoints: vec![endpoint],
                ..Default::default()
            })
            .encode()
            .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::Inspect>()
        .unwrap();

    assert_eq!(details.public_key, public_key);
    assert_eq!(details.advertised_endpoints, [endpoint]);
}

#[test]
fn nonempty_directory_without_a_machine_record_is_not_claimed() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    let existing = dir.0.join("keep-me");
    fs::write(&existing, b"unrelated").unwrap();

    assert!(matches!(
        LocalMachineStore::open(&dir.0),
        Err(StoreError::UnownedDataDirectory(_))
    ));
    assert_eq!(fs::read(existing).unwrap(), b"unrelated");
}

#[test]
fn reset_stops_if_the_machine_record_changes() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let replacement = LocalMachineRecord {
        id: ployz_core::MachineId::random(),
        phase: LocalMachinePhase::Resetting,
        machine: None,
        wireguard_private_key: None,
        wireguard_mtu: None,
        cluster_network: None,
        bootstrap_machines: Vec::new(),
        selected_endpoints: BTreeMap::new(),
        min_store_version: BTreeMap::new(),
    };
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
}

#[test]
fn machine_store_is_process_exclusive() {
    let dir = TestDir::new("ployzd-state");
    let store = LocalMachineStore::open(&dir.0).unwrap();

    assert!(matches!(
        LocalMachineStore::open(&dir.0),
        Err(StoreError::AlreadyRunning(_))
    ));
    drop(store);
    LocalMachineStore::open(&dir.0).unwrap();
}

#[test]
fn interrupted_initial_write_is_recovered() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(dir.0.join(".machine.json.tmp"), b"partial").unwrap();

    let store = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(store.record().phase, LocalMachinePhase::Uninitialized);
    assert!(!dir.0.join(".machine.json.tmp").exists());
}

#[test]
fn completing_catch_up_persists_participation_and_clears_the_target() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    let record = LocalMachineRecord {
        id: ployz_core::MachineId::random(),
        phase: LocalMachinePhase::Joining,
        machine: None,
        wireguard_private_key: None,
        wireguard_mtu: None,
        cluster_network: None,
        bootstrap_machines: Vec::new(),
        selected_endpoints: BTreeMap::new(),
        min_store_version: BTreeMap::from([("actor".to_owned(), 4)]),
    };
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.complete_catch_up().unwrap();
    drop(store);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().phase, LocalMachinePhase::Participating);
    assert!(reopened.record().min_store_version.is_empty());
}

#[test]
fn selected_endpoint_is_best_effort_local_state() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let peer = MachineId::random();
    let endpoint = SelectedEndpoint(SocketAddr::from(([192, 0, 2, 4], 51820)));
    store.persist_selected_endpoint(peer, endpoint).unwrap();
    drop(store);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(
        reopened.record().selected_endpoints.get(&peer),
        Some(&endpoint)
    );
}
