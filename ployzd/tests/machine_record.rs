mod test_dir;

use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

use ployz_core::{
    AdvertisedEndpoint, CloudPairing, InspectRequest, JoinRequest, LocalMachinePhase, Machine,
    MachineId, MachineName, MachineRuntime, MachineUpdate, PairingCredential, PublicIpUpdate,
    Registered, SelectedEndpoint,
};
use ployzd::machine::{
    LocalMachine, LocalMachineBody, LocalMachineError, LocalMachinePrior, LocalMachineRecord,
    LocalMachineStore, ParticipationOrigin, StoreError,
};
use ployzd::network::WireGuardPrivateKey;

use test_dir::TestDir;

#[test]
fn machine_record_is_created_once_and_reopened_with_private_permissions() {
    let dir = TestDir::new("ployzd-state");
    let created = LocalMachineStore::open(&dir.0).unwrap();
    let machine_id = created.record().id();

    assert_eq!(created.record().phase(), LocalMachinePhase::Uninitialized);
    assert!(created.record().min_store_version().is_empty());
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
    assert_eq!(reopened.record().id(), machine_id);
    assert_eq!(reopened.record().phase(), LocalMachinePhase::Uninitialized);
}

#[test]
fn initialize_and_join_persist_the_only_supported_transitions() {
    let first_dir = TestDir::new("ployzd-initialize");
    let mut first = LocalMachineStore::open(&first_dir.0).unwrap();
    let initialized = first
        .initialize(
            MachineName::parse("first").unwrap(),
            ployzd::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            Some("203.0.113.1".parse().unwrap()),
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            Some(1400),
            None,
        )
        .unwrap();
    assert_eq!(first.record().phase(), LocalMachinePhase::Participating);
    assert_eq!(first.record().machine(), Some(&initialized));
    assert_eq!(initialized.public_ip, Some("203.0.113.1".parse().unwrap()));
    assert_eq!(
        first.record().cluster_network().unwrap().to_string(),
        "10.210.0.0/16"
    );
    let LocalMachineBody::Participating {
        origin: ParticipationOrigin::Founder { cluster: founding },
        ..
    } = first.record().body()
    else {
        panic!("initialized Machine must retain its founding Cluster seed");
    };
    assert_eq!(
        founding.ingress_proxy_backend,
        ployz_core::IngressProxyBackend::Caddy
    );
    assert_eq!(first.record().cloud_pairing, None);
    assert!(
        first
            .initialize(
                MachineName::parse("again").unwrap(),
                ployzd::machine::FoundingCluster {
                    network: "10.210.0.0/16".parse().unwrap(),
                    ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
                },
                None,
                vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
                None,
                None,
            )
            .is_err()
    );

    let second_dir = TestDir::new("ployzd-join");
    let mut second = LocalMachineStore::open(&second_dir.0).unwrap();
    let public_key = second.record().private_key().public_key();
    let assigned = Machine {
        id: MachineId::random(),
        name: MachineName::parse("second").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
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
            None,
        )
        .unwrap();
    assert_eq!(second.record().id(), assigned.id);
    assert_eq!(second.record().phase(), LocalMachinePhase::Joining);
    assert_eq!(second.record().bootstrap(), [initialized].as_slice());
    assert_eq!(second.record().min_store_version().get("actor"), Some(&4));
    assert_eq!(second.record().cloud_pairing, None);
}

fn sample_cloud_pairing() -> CloudPairing {
    CloudPairing::parse(
        "https://relay.example.invalid",
        PairingCredential::parse("pairing-secret").unwrap(),
    )
    .unwrap()
}

#[test]
fn initialize_with_cloud_pairing_stores_relay_url_and_pairing_credential() {
    let dir = TestDir::new("ployzd-initialize-cloud-pairing");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    let pairing = sample_cloud_pairing();

    local
        .initialize(ployz_core::InitializeRequest {
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
            cloud_pairing: Some(pairing.clone()),
        })
        .unwrap();

    assert_eq!(
        local.record().unwrap().cloud_pairing.as_ref(),
        Some(&pairing)
    );
    drop(local);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().cloud_pairing.as_ref(), Some(&pairing));
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    let pairing_json = persisted.get("cloud_pairing").expect("cloud_pairing field");
    assert_eq!(
        pairing_json,
        &serde_json::json!({
            "relayUrl": "https://relay.example.invalid/",
            "secret": "pairing-secret",
        })
    );
    assert!(persisted.get("dial").is_none());
    assert!(pairing_json.get("dial").is_none());
}

#[test]
fn set_cloud_pairing_after_initialize_persists() {
    let dir = TestDir::new("ployzd-set-cloud-pairing");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    let pairing = sample_cloud_pairing();

    local
        .initialize(ployz_core::InitializeRequest {
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
            cloud_pairing: None,
        })
        .unwrap();
    assert_eq!(local.record().unwrap().cloud_pairing, None);

    local.set_cloud_pairing(Some(pairing.clone())).unwrap();
    assert_eq!(
        local.record().unwrap().cloud_pairing.as_ref(),
        Some(&pairing)
    );
    drop(local);
    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().cloud_pairing.as_ref(), Some(&pairing));
}

#[test]
fn set_cloud_pairing_none_clears_persisted_pairing() {
    let dir = TestDir::new("ployzd-clear-cloud-pairing");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    local
        .initialize(ployz_core::InitializeRequest {
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
            cloud_pairing: Some(sample_cloud_pairing()),
        })
        .unwrap();
    local.set_cloud_pairing(None).unwrap();
    assert_eq!(local.record().unwrap().cloud_pairing, None);
    drop(local);
    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().cloud_pairing, None);
}

#[test]
fn set_cloud_pairing_before_initialize_is_not_participating() {
    let dir = TestDir::new("ployzd-set-cloud-pairing-uninitialized");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    let error = local
        .set_cloud_pairing(Some(sample_cloud_pairing()))
        .unwrap_err();
    assert!(matches!(error, LocalMachineError::NotParticipating));
}

#[test]
fn join_with_cloud_pairing_stores_the_same_two_fields() {
    let first_dir = TestDir::new("ployzd-join-cloud-pairing-first");
    let mut first = LocalMachineStore::open(&first_dir.0).unwrap();
    let initialized = first
        .initialize(
            MachineName::parse("first").unwrap(),
            ployzd::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();

    let second_dir = TestDir::new("ployzd-join-cloud-pairing-second");
    let store = LocalMachineStore::open(&second_dir.0).unwrap();
    let public_key = store.record().private_key().public_key();
    let assigned = Machine {
        id: MachineId::random(),
        name: MachineName::parse("second").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
    };
    let pairing = sample_cloud_pairing();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    local
        .join(JoinRequest {
            registration: Registered {
                assigned_machine: assigned.clone(),
                visible_peers: vec![initialized],
                target_versions: BTreeMap::from([("actor".into(), 4)]),
            },
            wireguard_mtu: None,
            cloud_pairing: Some(pairing.clone()),
        })
        .unwrap();

    assert_eq!(
        local.record().unwrap().cloud_pairing.as_ref(),
        Some(&pairing)
    );
    drop(local);

    let reopened = LocalMachineStore::open(&second_dir.0).unwrap();
    assert_eq!(reopened.record().id(), assigned.id);
    assert_eq!(reopened.record().cloud_pairing.as_ref(), Some(&pairing));
}

#[test]
fn reopening_a_participating_machine_refreshes_runtime_metadata() {
    let dir = TestDir::new("ployzd-runtime-refresh");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store
        .initialize(
            MachineName::parse("machine").unwrap(),
            ployzd::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
            None,
        )
        .unwrap();
    drop(store);

    let path = dir.0.join("machine.json");
    let mut stale: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stale["body"]["machine"]["runtime"] = serde_json::to_value(MachineRuntime {
        daemon_version: "stale".into(),
        docker_version: "stale".into(),
        hostname: "stale".into(),
        architecture: "stale".into(),
        os_pretty_name: "stale".into(),
        kernel_version: "stale".into(),
    })
    .unwrap();
    fs::write(&path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    let expected = ployzd::machine::local_runtime();
    assert_eq!(reopened.record().machine().unwrap().runtime, expected);
    let persisted: LocalMachineRecord = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(persisted.machine().unwrap().runtime, expected);
}

#[test]
fn machine_update_is_atomic_and_durable() {
    let dir = TestDir::new("ployzd-update");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let original = store
        .initialize(
            MachineName::parse("before").unwrap(),
            ployzd::machine::FoundingCluster {
                network: "10.210.0.0/16".parse().unwrap(),
                ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            },
            None,
            vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            None,
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
    assert_eq!(updated.management_address(), original.management_address());
    assert_eq!(updated.public_key, original.public_key);
    assert_eq!(updated.advertised_endpoints, endpoints);
    drop(store);

    let mut reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().machine(), Some(&updated));
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
    let old_machine_id = store.record().id();

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
    store.begin_reset().unwrap();
    assert_eq!(store.record().phase(), LocalMachinePhase::Resetting);
    assert!(store.begin_reset().is_err());
    drop(store);

    let persisted: LocalMachineRecord =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.id(), old_machine_id);
    assert_eq!(persisted.phase(), LocalMachinePhase::Resetting);

    let recreated = LocalMachineStore::open(&dir.0).unwrap();
    assert_ne!(recreated.record().id(), old_machine_id);
    assert_eq!(recreated.record().phase(), LocalMachinePhase::Uninitialized);
}

#[tokio::test]
async fn inspect_keeps_the_v1_key_and_endpoint_payload() {
    let dir = TestDir::new("ployzd-state");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let public_key = store.record().private_key().public_key();
    let endpoint = AdvertisedEndpoint("192.0.2.8:51820".parse().unwrap());
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);

    let details = local
        .inspect(InspectRequest {
            advertised_endpoints: vec![endpoint],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(details.public_key, public_key);
    assert_eq!(details.advertised_endpoints, [endpoint]);
    assert!(!details.cloud_paired);
    assert_eq!(details.storage, None);
}

#[tokio::test]
async fn inspect_reports_stored_cloud_pairing_without_the_secret() {
    let dir = TestDir::new("ployzd-inspect-cloud-pairing");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);
    local
        .initialize(ployz_core::InitializeRequest {
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
            cloud_pairing: Some(sample_cloud_pairing()),
        })
        .unwrap();

    let details = local.inspect(InspectRequest::default()).await.unwrap();
    assert!(details.cloud_paired);
    let encoded = serde_json::to_value(&details).unwrap();
    assert_eq!(encoded.get("cloud_paired"), Some(&serde_json::json!(true)));
    assert!(encoded.get("cloud_pairing").is_none());
    assert!(encoded.get("secret").is_none());
}

#[tokio::test]
async fn repeated_reset_returns_a_typed_conflict() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let local = LocalMachine::new(Arc::new(Mutex::new(store)), reset);

    let error = local.reset().await.unwrap_err();

    assert!(matches!(
        error,
        LocalMachineError::Store(StoreError::AlreadyResetting)
    ));
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
    let replacement = LocalMachineRecord::new(
        LocalMachineBody::Resetting {
            prior: Box::new(LocalMachinePrior::Uninitialized {
                id: ployz_core::MachineId::random(),
            }),
        },
        WireGuardPrivateKey::generate(),
    )
    .unwrap();
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
    assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);
    assert!(!dir.0.join(".machine.json.tmp").exists());
}

#[test]
fn completing_catch_up_persists_participation_and_clears_the_target() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    let key = WireGuardPrivateKey::generate();
    let assigned = sample_machine(MachineId::random(), key.public_key());
    let record = LocalMachineRecord::new(
        LocalMachineBody::Joining {
            machine: assigned,
            bootstrap: vec![sample_machine(
                MachineId::random(),
                WireGuardPrivateKey::generate().public_key(),
            )],
            min_store_version: BTreeMap::from([("actor".to_owned(), 4)]),
        },
        key,
    )
    .unwrap();
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.complete_catch_up().unwrap();
    drop(store);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().phase(), LocalMachinePhase::Participating);
    assert!(reopened.record().min_store_version().is_empty());
    assert!(reopened.record().cluster_network().is_none());
}

#[test]
fn complete_catch_up_requires_joining() {
    let dir = TestDir::new("ployzd-catch-up-not-joining");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    assert!(matches!(
        store.complete_catch_up(),
        Err(StoreError::NotJoining)
    ));
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

#[test]
fn opening_joining_without_a_machine_or_key_fails() {
    let dir = TestDir::new("ployzd-joining-without-machine");
    fs::create_dir_all(&dir.0).unwrap();
    let key = serde_json::to_value(WireGuardPrivateKey::generate()).unwrap();
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&serde_json::json!({
            "body": {
                "phase": "joining",
                "bootstrap": [],
                "min_store_version": { "actor": 1 }
            },
            "wireguard_private_key": key
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(LocalMachineStore::open(&dir.0).is_err());

    let dir = TestDir::new("ployzd-joining-without-key");
    fs::create_dir_all(&dir.0).unwrap();
    let assigned = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&serde_json::json!({
            "body": {
                "phase": "joining",
                "machine": assigned,
                "bootstrap": [],
                "min_store_version": { "actor": 1 }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(LocalMachineStore::open(&dir.0).is_err());
}

#[test]
fn uninitialized_cannot_persist_a_machine() {
    let dir = TestDir::new("ployzd-uninitialized-no-machine");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    assert!(store.record().machine().is_none());
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    let body = persisted.get("body").expect("persisted body");
    assert_eq!(
        body.get("phase").and_then(serde_json::Value::as_str),
        Some("uninitialized")
    );
    assert!(body.get("machine").is_none());
}

#[test]
fn legal_bodies_round_trip() {
    let key = WireGuardPrivateKey::generate();
    let machine = sample_machine(MachineId::random(), key.public_key());
    let peer = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    let records = [
        LocalMachineRecord::new(
            LocalMachineBody::Uninitialized {
                id: MachineId::random(),
            },
            key.clone(),
        )
        .unwrap(),
        LocalMachineRecord::new(
            LocalMachineBody::Joining {
                machine: machine.clone(),
                bootstrap: vec![peer.clone()],
                min_store_version: BTreeMap::from([("actor".into(), 4)]),
            },
            key.clone(),
        )
        .unwrap(),
        {
            let mut record = LocalMachineRecord::new(
                LocalMachineBody::Participating {
                    machine: machine.clone(),
                    origin: ParticipationOrigin::Founder {
                        cluster: ployzd::machine::FoundingCluster {
                            network: "10.210.0.0/16".parse().unwrap(),
                            ingress_proxy_backend: ployz_core::IngressProxyBackend::Caddy,
                        },
                    },
                },
                key.clone(),
            )
            .unwrap();
            record.wireguard_mtu = Some(1400);
            record
        },
        LocalMachineRecord::new(
            LocalMachineBody::Participating {
                machine: machine.clone(),
                origin: ParticipationOrigin::Join {
                    bootstrap: vec![peer],
                },
            },
            key.clone(),
        )
        .unwrap(),
        LocalMachineRecord::new(
            LocalMachineBody::Resetting {
                prior: Box::new(LocalMachinePrior::Participating {
                    machine,
                    origin: ParticipationOrigin::Join {
                        bootstrap: vec![sample_machine(
                            MachineId::random(),
                            WireGuardPrivateKey::generate().public_key(),
                        )],
                    },
                }),
            },
            key,
        )
        .unwrap(),
    ];
    let founder = serde_json::to_value(&records[2]).unwrap();
    let founder = founder.get("body").unwrap().as_object().unwrap();
    assert_eq!(
        founder.get("origin").and_then(serde_json::Value::as_str),
        Some("founder")
    );
    assert!(founder.get("bootstrap").is_none());
    let join = serde_json::to_value(&records[3]).unwrap();
    let join = join.get("body").unwrap().as_object().unwrap();
    assert_eq!(
        join.get("origin").and_then(serde_json::Value::as_str),
        Some("join")
    );
    assert!(join.get("cluster").is_none());
    for record in records {
        let loaded: LocalMachineRecord =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(loaded, record);
    }
}

#[test]
fn pre_616_participating_authority_shape_is_not_migrated() {
    let key = WireGuardPrivateKey::generate();
    let machine = sample_machine(MachineId::random(), key.public_key());
    let record = LocalMachineRecord::new(
        LocalMachineBody::Participating {
            machine,
            origin: ParticipationOrigin::Join {
                bootstrap: vec![sample_machine(
                    MachineId::random(),
                    WireGuardPrivateKey::generate().public_key(),
                )],
            },
        },
        key,
    )
    .unwrap();
    let mut legacy = serde_json::to_value(record).unwrap();
    let body = legacy.get_mut("body").unwrap().as_object_mut().unwrap();
    body.remove("origin");
    body.insert("founding_cluster".into(), serde_json::Value::Null);

    assert!(serde_json::from_value::<LocalMachineRecord>(legacy).is_err());
}

fn sample_machine(id: MachineId, public_key: ployz_core::WireGuardPublicKey) -> Machine {
    Machine {
        id,
        name: MachineName::parse("machine").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}

#[test]
fn local_record_decoding_rejects_incoherent_identity_and_empty_join_payloads() {
    let key = WireGuardPrivateKey::generate();
    let machine = sample_machine(MachineId::random(), key.public_key());
    let peer = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    let valid = serde_json::json!({
        "body": { "phase": "joining", "machine": machine, "bootstrap": [peer] },
        "wireguard_private_key": key
    });
    assert!(serde_json::from_value::<LocalMachineRecord>(valid.clone()).is_ok());
    for field in ["public_key", "advertised_endpoints", "bootstrap"] {
        let mut malformed = valid.clone();
        match field {
            "public_key" => {
                malformed["body"]["machine"][field] =
                    serde_json::to_value(WireGuardPrivateKey::generate().public_key()).unwrap()
            }
            "advertised_endpoints" => malformed["body"]["machine"][field] = serde_json::json!([]),
            _ => malformed["body"][field] = serde_json::json!([]),
        }
        for phase in ["joining", "participating", "resetting"] {
            let mut record = malformed.clone();
            if phase == "participating" {
                record["body"]["phase"] = serde_json::json!(phase);
                record["body"]["origin"] = serde_json::json!("join");
            } else if phase == "resetting" {
                record["body"] = serde_json::json!({"phase": phase, "prior": record["body"]});
            }
            let body = serde_json::from_value::<LocalMachineBody>(record["body"].clone()).unwrap();
            assert!(
                LocalMachineRecord::new(body, key.clone()).is_err(),
                "{phase} constructor accepted invalid {field}"
            );
            assert!(
                serde_json::from_value::<LocalMachineRecord>(record).is_err(),
                "{phase} accepted invalid {field}"
            );
        }
    }
}

#[test]
fn join_rejects_empty_local_endpoints_without_changing_the_durable_record() {
    let dir = TestDir::new("ployzd-empty-join-endpoints");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let original = store.record().clone();
    let mut assigned = sample_machine(MachineId::random(), original.private_key().public_key());
    let peer = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    assigned.advertised_endpoints.clear();
    assert!(matches!(
        store.join(assigned, vec![peer], BTreeMap::new(), None, None),
        Err(StoreError::MissingEndpoints)
    ));
    assert_eq!(store.record(), &original);
}
