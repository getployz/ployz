mod test_dir;

use std::{collections::BTreeMap, fs, net::SocketAddr, os::unix::fs::PermissionsExt};

use ployz_core::{
    AdvertisedEndpoint, InspectRequest, JoinRequest, LocalMachinePhase, Machine, MachineId,
    MachineName, MachineRuntime, MachineUpdate, ManagementClientLabel, PublicIpUpdate, Registered,
    SelectedEndpoint, SetManagementClientRequest,
};
use ployzd::machine::{
    LocalMachine, LocalMachineBody, LocalMachineError, LocalMachinePrior, LocalMachineRecord,
    LocalMachineStore, ParticipationOrigin, RecordOwner, StoreError,
};
use ployzd::management::ManagementSecret;
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

#[tokio::test]
async fn initialize_commits_policy_in_the_first_participating_record() {
    let dir = TestDir::new("ployzd-initialize-policy");
    let local =
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(&dir.0).unwrap()).unwrap());
    let request = serde_json::from_value(serde_json::json!({
        "name": "builder",
        "cluster_network": "10.210.0.0/16",
        "advertised_endpoints": ["192.0.2.1:51820"],
        "initial_policy": {
            "labels": {"pool": "build"},
            "accepts_builds": true,
            "accepts_services": false,
            "accepts_ingress": false
        }
    }))
    .unwrap();
    let initialized = local.initialize(request).await.unwrap().machine;
    assert!(
        !initialized.accepts_services,
        "initial durable record must refuse Services"
    );
    assert!(!initialized.accepts_ingress);
    assert!(initialized.accepts_builds);
    assert_eq!(initialized.labels.get("pool").unwrap().as_str(), "build");
    drop(local);
    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().phase(), LocalMachinePhase::Participating);
    assert_eq!(reopened.record().machine(), Some(&initialized));
}

#[test]
fn initialize_and_join_persist_the_only_supported_transitions() {
    let first_dir = TestDir::new("ployzd-initialize");
    let mut first = LocalMachineStore::open(&first_dir.0).unwrap();
    let initialized = first
        .initialize(ployz_core::InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: Some("203.0.113.1".parse().unwrap()),
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: Some(1400),
        })
        .unwrap();
    assert_eq!(first.record().phase(), LocalMachinePhase::Participating);
    assert_eq!(first.record().machine(), Some(&initialized));
    assert_eq!(initialized.public_ip, Some("203.0.113.1".parse().unwrap()));
    assert_eq!(
        first.record().cluster_network().unwrap().to_string(),
        "10.210.0.0/16"
    );
    let LocalMachineBody::Participating {
        origin: ParticipationOrigin::Founder { cluster: _ },
        ..
    } = first.record().body()
    else {
        panic!("initialized Machine must retain its founding Cluster seed");
    };
    assert!(!first.record().has_management_clients());
    assert!(
        first
            .initialize(ployz_core::InitializeRequest {
                initial_policy: Default::default(),
                name: MachineName::parse("again").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
                wireguard_mtu: None,
            })
            .is_err()
    );

    let second_dir = TestDir::new("ployzd-join");
    let mut second = LocalMachineStore::open(&second_dir.0).unwrap();
    let public_key = second.record().private_key().public_key();
    let assigned = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: second.record().id(),
        name: MachineName::parse("second").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
        build_concurrency: None,
    };
    second
        .join(
            assigned.clone(),
            vec![initialized.clone()],
            BTreeMap::from([("actor".into(), 4)]),
            None,
        )
        .unwrap();
    assert_eq!(second.record().id(), assigned.id);
    assert_eq!(second.record().phase(), LocalMachinePhase::Joining);
    assert_eq!(second.record().bootstrap(), [initialized].as_slice());
    assert_eq!(second.record().min_store_version().get("actor"), Some(&4));
    assert!(!second.record().has_management_clients());
}

fn cloud() -> ManagementClientLabel {
    ManagementClientLabel::parse("cloud").unwrap()
}

async fn participating(dir: &TestDir) -> LocalMachine {
    let local =
        LocalMachine::new(RecordOwner::spawn(LocalMachineStore::open(&dir.0).unwrap()).unwrap());
    local
        .initialize(ployz_core::InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("first").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
        })
        .await
        .unwrap();
    local
}

#[tokio::test]
async fn set_management_client_persists_only_public_client_keys() {
    let dir = TestDir::new("ployzd-set-management-client");
    let local = participating(&dir).await;
    assert!(!local.record().has_management_clients());

    let capability = local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap()
        .capability
        .unwrap();
    drop(local);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(
        reopened.record().management_clients().collect::<Vec<_>>(),
        [&cloud()]
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    let access = persisted.pointer("/management_clients/cloud").unwrap();
    let mut fields = access.as_object().unwrap().keys().collect::<Vec<_>>();
    fields.sort();
    assert_eq!(fields, ["pending", "state"], "{access}");
    let text = persisted.to_string();
    assert!(!text.contains(&capability.to_secret_string()));
    assert!(!text.contains(&serde_json::to_string(capability.client_secret()).unwrap()));
}

/// A later daemon may add optional fields anywhere in the record; this reader
/// must reopen it without losing a known value.
#[tokio::test]
async fn record_written_by_a_later_daemon_reopens_with_every_known_value() {
    let dir = TestDir::new("ployzd-record-unknown-fields");
    let local = participating(&dir).await;
    local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap();
    drop(local);
    let known = LocalMachineStore::open(&dir.0).unwrap().record().clone();

    let path = dir.0.join("machine.json");
    let mut persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for pointer in ["/body/machine", ""] {
        persisted
            .pointer_mut(pointer)
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert("added_by_a_later_daemon".into(), serde_json::json!(1));
    }
    fs::write(&path, persisted.to_string()).unwrap();

    assert_eq!(LocalMachineStore::open(&dir.0).unwrap().record(), &known);
}

#[tokio::test]
async fn clear_persists_a_tombstone_of_public_keys_only() {
    let dir = TestDir::new("ployzd-clear-management-client");
    let local = participating(&dir).await;
    let set = |label: &'static str| {
        let local = local.clone();
        async move {
            let capability = local
                .set_management_client(SetManagementClientRequest::Set {
                    label: ManagementClientLabel::parse(label).unwrap(),
                })
                .await
                .unwrap()
                .capability
                .unwrap();
            let public = *iroh::SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes();
            (capability, public)
        }
    };
    let clear = |label: &'static str| {
        let local = local.clone();
        async move {
            local
                .set_management_client(SetManagementClientRequest::Clear {
                    label: ManagementClientLabel::parse(label).unwrap(),
                })
                .await
                .unwrap()
        }
    };
    // One slot in each live state: pending, active and rotating.
    let (pending, pending_key) = set("pending").await;
    let (active, active_key) = set("active").await;
    local.activate_management_client(active_key).await.unwrap();
    let (rotated, rotated_key) = set("rotating").await;
    local.activate_management_client(rotated_key).await.unwrap();
    let (rotating, rotating_key) = set("rotating").await;
    for label in ["pending", "active", "rotating"] {
        clear(label).await;
    }
    assert!(!local.record().has_management_clients());
    let cleared = local.record();
    // Clear is idempotent: a tombstone stays unchanged and an absent label stays absent.
    clear("rotating").await;
    clear("cli").await;
    assert_eq!(local.record(), cleared);
    drop(local);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record(), &*cleared);
    assert_eq!(reopened.record().management_clients().count(), 0);
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(
        persisted.get("management_clients"),
        Some(&serde_json::json!({
            "pending": {
                "state": "cleared",
                "was": { "state": "pending", "pending": pending_key },
            },
            "active": {
                "state": "cleared",
                "was": { "state": "active", "accepted": active_key },
            },
            "rotating": {
                "state": "cleared",
                "was": {
                    "state": "rotating",
                    "accepted": rotated_key,
                    "pending": rotating_key,
                },
            },
        }))
    );
    let text = persisted.to_string();
    for capability in [pending, active, rotated, rotating] {
        assert!(!text.contains(&serde_json::to_string(capability.client_secret()).unwrap()));
    }
}

#[tokio::test]
async fn set_management_client_before_initialize_is_not_participating() {
    let dir = TestDir::new("ployzd-set-management-client-uninitialized");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());
    for request in [
        SetManagementClientRequest::Set { label: cloud() },
        SetManagementClientRequest::Clear { label: cloud() },
    ] {
        let error = local.set_management_client(request).await.unwrap_err();
        assert!(matches!(error, LocalMachineError::NotParticipating));
    }
}

#[test]
fn reopening_a_participating_machine_refreshes_runtime_metadata() {
    let dir = TestDir::new("ployzd-runtime-refresh");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store
        .initialize(ployz_core::InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("machine").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
        })
        .unwrap();
    drop(store);

    let path = dir.0.join("machine.json");
    let mut stale: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    *stale.pointer_mut("/body/machine/runtime").unwrap() = serde_json::to_value(MachineRuntime {
        daemon_version: "stale".into(),
        docker_version: "stale".into(),
        hostname: "stale".into(),
        architecture: "stale".into(),
        os_pretty_name: "stale".into(),
        kernel_version: "stale".into(),
        memory_total_bytes: None,
        running_builds: 0,
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
        .initialize(ployz_core::InitializeRequest {
            initial_policy: Default::default(),
            name: MachineName::parse("before").unwrap(),
            cluster_network: "10.210.0.0/16".parse().unwrap(),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            wireguard_mtu: None,
        })
        .unwrap();
    let endpoints = vec![AdvertisedEndpoint("198.51.100.2:6000".parse().unwrap())];
    let updated = store
        .update(
            MachineUpdate {
                label_changes: std::collections::BTreeMap::from([(
                    "zone".parse().unwrap(),
                    Some("west".parse().unwrap()),
                )]),
                accepts_services: Some(false),
                name: Some(MachineName::parse("after").unwrap()),
                public_ip: PublicIpUpdate::Set("203.0.113.7".parse().unwrap()),
                advertised_endpoints: Some(endpoints.clone()),
                build_concurrency: ployz_core::BuildConcurrencyUpdate::Set("3".parse().unwrap()),
                ..Default::default()
            },
            std::slice::from_ref(&original),
        )
        .unwrap();

    assert_eq!(updated.id, original.id);
    assert_eq!(updated.subnet, original.subnet);
    assert_eq!(updated.management_address(), original.management_address());
    assert_eq!(updated.public_key, original.public_key);
    assert_eq!(updated.advertised_endpoints, endpoints);
    assert_eq!(
        updated
            .labels
            .get("zone")
            .map(ployz_core::MachineLabelValue::as_str),
        Some("west")
    );
    assert!(!updated.accepts_services);
    assert!(updated.accepts_builds && updated.accepts_ingress);
    assert_eq!(updated.build_concurrency, Some("3".parse().unwrap()));
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
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());

    let details = local
        .inspect(InspectRequest {
            advertised_endpoints: vec![endpoint],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(details.public_key, public_key);
    assert_eq!(details.advertised_endpoints, [endpoint]);
    assert!(details.management_clients.is_empty());
    assert_eq!(details.storage, None);
}

#[tokio::test]
async fn inspect_lists_management_clients_holding_keys() {
    let dir = TestDir::new("ployzd-inspect-management-clients");
    let local = participating(&dir).await;
    let details = local.inspect(InspectRequest::default()).await.unwrap();
    assert!(details.management_clients.is_empty());
    local
        .set_management_client(SetManagementClientRequest::Set { label: cloud() })
        .await
        .unwrap();

    let details = local.inspect(InspectRequest::default()).await.unwrap();
    assert_eq!(details.management_clients, [cloud()]);
    let encoded = serde_json::to_value(&details).unwrap();
    assert_eq!(
        encoded.get("management_clients"),
        Some(&serde_json::json!(["cloud"]))
    );
    assert!(!encoded.to_string().contains("cloud_pair"));
    assert!(encoded.get("secret").is_none());

    // A Cleared tombstone holds no key the Machine serves, so Inspect does not list it.
    local
        .set_management_client(SetManagementClientRequest::Clear { label: cloud() })
        .await
        .unwrap();
    let details = local.inspect(InspectRequest::default()).await.unwrap();
    assert!(details.management_clients.is_empty());
}

#[tokio::test]
async fn repeated_reset_returns_a_typed_conflict() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());

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
    let replacement = LocalMachineRecord::parse(
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
    let record = LocalMachineRecord::parse(
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
            "wireguard_private_key": key,
            "management_secret": ManagementSecret::generate(),
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
        LocalMachineRecord::parse(
            LocalMachineBody::Uninitialized {
                id: MachineId::random(),
            },
            key.clone(),
        )
        .unwrap(),
        LocalMachineRecord::parse(
            LocalMachineBody::Joining {
                machine: machine.clone(),
                bootstrap: vec![peer.clone()],
                min_store_version: BTreeMap::from([("actor".into(), 4)]),
            },
            key.clone(),
        )
        .unwrap(),
        {
            let mut record = LocalMachineRecord::parse(
                LocalMachineBody::Participating {
                    machine: machine.clone(),
                    origin: ParticipationOrigin::Founder {
                        cluster: ployzd::machine::FoundingCluster {
                            network: "10.210.0.0/16".parse().unwrap(),
                        },
                    },
                },
                key.clone(),
            )
            .unwrap();
            record.wireguard_mtu = Some(1400);
            record
        },
        LocalMachineRecord::parse(
            LocalMachineBody::Participating {
                machine: machine.clone(),
                origin: ParticipationOrigin::Join {
                    bootstrap: vec![peer],
                },
            },
            key.clone(),
        )
        .unwrap(),
        LocalMachineRecord::parse(
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
    let record = LocalMachineRecord::parse(
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
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id,
        name: MachineName::parse("machine").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key,
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
        build_concurrency: None,
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
        "wireguard_private_key": key,
        "management_secret": ManagementSecret::generate(),
        "management_clients": {},
    });
    assert!(serde_json::from_value::<LocalMachineRecord>(valid.clone()).is_ok());
    for (path, value) in [
        (
            "/body/machine/public_key",
            serde_json::to_value(WireGuardPrivateKey::generate().public_key()).unwrap(),
        ),
        ("/body/machine/advertised_endpoints", serde_json::json!([])),
        ("/body/bootstrap", serde_json::json!([])),
    ] {
        let mut malformed = valid.clone();
        *malformed.pointer_mut(path).unwrap() = value;
        for phase in ["joining", "participating", "resetting"] {
            let mut record = malformed.clone();
            let body = record.get_mut("body").unwrap();
            if phase == "participating" {
                *body.get_mut("phase").unwrap() = serde_json::json!(phase);
                body.as_object_mut()
                    .unwrap()
                    .insert("origin".into(), serde_json::json!("join"));
            } else if phase == "resetting" {
                let prior = body.take();
                *body = serde_json::json!({"phase": phase, "prior": prior});
            }
            let body = serde_json::from_value::<LocalMachineBody>(body.clone()).unwrap();
            assert!(
                LocalMachineRecord::parse(body, key.clone()).is_err(),
                "{phase} constructor accepted invalid {path}"
            );
            assert!(
                serde_json::from_value::<LocalMachineRecord>(record).is_err(),
                "{phase} accepted invalid {path}"
            );
        }
    }
}

#[test]
fn join_rejects_empty_local_endpoints_without_changing_the_durable_record() {
    let dir = TestDir::new("ployzd-empty-join-endpoints");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let original = store.record().clone();
    let mut assigned = sample_machine(original.id(), original.private_key().public_key());
    let peer = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    assigned.advertised_endpoints.clear();
    assert!(matches!(
        store.join(assigned, vec![peer], BTreeMap::new(), None),
        Err(StoreError::MissingEndpoints)
    ));
    assert_eq!(store.record(), &original);
}

#[test]
fn data_directory_errors_render_paths_without_debug_quotes() {
    let path = std::path::PathBuf::from("/var/lib/café data\n\u{1b}[2J");
    for error in [
        StoreError::AlreadyRunning(path.clone()),
        StoreError::UnsafeDataDirectory(path.clone()),
        StoreError::UnownedDataDirectory(path.clone()),
        StoreError::OwnershipLost(path.clone()),
        StoreError::ResetPreparationLost(path),
    ] {
        let message = error.to_string();
        assert!(
            message.ends_with(r"/var/lib/café data\n\u{1b}[2J"),
            "{message}"
        );
        assert!(!message.contains('"'), "{message}");
    }
}

#[test]
fn data_directory_errors_preserve_non_utf8_bytes() {
    use std::os::unix::ffi::OsStringExt;
    for byte in [0xfe, 0xff] {
        let mut bytes = "/var/lib/café-".as_bytes().to_vec();
        bytes.push(byte);
        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes));
        let error = StoreError::UnownedDataDirectory(path).to_string();
        assert!(
            error.ends_with(&format!(r"/var/lib/café-\x{byte:02x}")),
            "{error}"
        );
        assert!(!error.contains('\u{fffd}'), "{error}");
    }
}

#[tokio::test]
async fn join_preserves_identity_rejects_wrong_inputs_and_resumes_after_lost_response() {
    let dir = TestDir::new("ployzd-join-replay");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let id = store.record().id();
    let assigned = sample_machine(id, store.record().private_key().public_key());
    let peer = sample_machine(
        MachineId::random(),
        WireGuardPrivateKey::generate().public_key(),
    );
    let request = JoinRequest {
        registration: Registered {
            assigned_machine: assigned,
            visible_peers: vec![peer],
            target_versions: BTreeMap::from([("actor".into(), 4)]),
        },
        wireguard_mtu: Some(1380),
    };
    let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());
    let mut restart_observation = local.owner().restart_requested();
    for wrong_id in [true, false] {
        let mut invalid = request.clone();
        if wrong_id {
            invalid.registration.assigned_machine.id = MachineId::random();
        } else {
            invalid.registration.assigned_machine.public_key =
                WireGuardPrivateKey::generate().public_key();
        }
        assert!(local.join(invalid).await.is_err());
        assert_eq!(local.record().phase(), LocalMachinePhase::Uninitialized);
        assert_eq!(local.record().id(), id);
    }
    assert!(!local.join(request.clone()).await.unwrap().already_accepted);
    assert!(*restart_observation.borrow_and_update());
    assert!(local.join(request.clone()).await.unwrap().already_accepted);
    assert!(!restart_observation.has_changed().unwrap());
    drop(local);
    let mut reopened = LocalMachineStore::open(&dir.0).unwrap();
    reopened.complete_catch_up().unwrap();
    let local = LocalMachine::new(RecordOwner::spawn(reopened).unwrap());
    assert!(local.join(request.clone()).await.unwrap().already_accepted);
    let mut conflict = request;
    conflict.wireguard_mtu = None;
    assert!(local.join(conflict).await.is_err());
    assert_eq!(local.record().id(), id);
}

#[test]
fn local_record_rejects_incomplete_management_client_slots() {
    let dir = TestDir::new("ployzd-invalid-management-clients");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let valid = serde_json::to_value(store.record()).unwrap();
    let key = serde_json::to_value([1_u8; 32]).unwrap();
    for clients in [
        serde_json::json!({"cloud": {"state": "active"}}),
        serde_json::json!({"cloud": {"state": "pending"}}),
        serde_json::json!({"cloud": {"state": "rotating", "accepted": key}}),
        serde_json::json!({"cloud": {"state": "enrolling", "accepted": key}}),
        serde_json::json!({"cloud": {"state": "cleared"}}),
        serde_json::json!({"cloud": {"state": "cleared", "was": {"state": "cleared"}}}),
        serde_json::json!({"cloud": {"state": "cleared", "was": {"state": "active"}}}),
        serde_json::json!({"cloud": {"state": "cleared", "was": {"state": "active", "accepted": key}, "secret": "s"}}),
        serde_json::json!({"cloud": {"state": "cleared", "was": {"state": "active", "accepted": key, "secret": "s"}}}),
        serde_json::json!({"Cloud": {"state": "active", "accepted": key}}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .insert("management_clients".into(), clients);
        assert!(serde_json::from_value::<LocalMachineRecord>(invalid).is_err());
    }
}

#[test]
fn local_record_refuses_unknown_management_client_slot_fields() {
    let dir = TestDir::new("ployzd-strict-management-clients");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let valid = serde_json::to_value(store.record()).unwrap();
    let key = serde_json::to_value([1_u8; 32]).unwrap();
    let mut extended = valid.clone();
    extended.as_object_mut().unwrap().insert(
        "management_clients".into(),
        serde_json::json!({"cloud": {"state": "active", "accepted": key, "secret": "s"}}),
    );
    // Key material fails closed: an unknown slot field may be a secret.
    assert!(serde_json::from_value::<LocalMachineRecord>(extended).is_err());

    // A record with no slots still writes the field.
    assert_eq!(
        valid.get("management_clients"),
        Some(&serde_json::json!({}))
    );
    serde_json::from_value::<LocalMachineRecord>(valid.clone()).unwrap();
}

/// A record from a build before Management Clients has no `management_clients`.
/// Reading it as "no slots" would silently forget the admitted Cloud key.
#[test]
fn local_record_without_management_clients_fails_to_load() {
    let dir = TestDir::new("ployzd-record-without-management-clients");
    let store = LocalMachineStore::open(&dir.0).unwrap();
    let mut earlier = serde_json::to_value(store.record()).unwrap();
    let record = earlier.as_object_mut().unwrap();
    record.remove("management_clients");
    record.insert(
        "cloud_access".into(),
        serde_json::json!({ "state": "active", "accepted": vec![1_u8; 32] }),
    );
    assert!(serde_json::from_value::<LocalMachineRecord>(earlier).is_err());
}
