//! `machine.json` is a persisted row: its field names, defaults and file name are frozen.

use serde_json::{Value, json};

use super::*;

fn bytes(byte: u8) -> Value {
    Value::from(vec![byte; 32])
}

/// Every field set to a value other than its default, as today's daemon writes it.
fn golden() -> Value {
    json!({
        "body": {"phase": "uninitialized", "id": "a".repeat(32)},
        "wireguard_private_key": bytes(7),
        "management_secret": bytes(9),
        "management_clients": {"cloud": {"state": "active", "accepted": bytes(1)}},
        "wireguard_mtu": 1280,
        "selected_endpoints": {"b".repeat(32): "192.0.2.4:51820"},
        "pinned_addresses": {"public_ip": true, "advertised_endpoints": true},
        "push_target": "https://cloud.example/push",
        "device_slots_vetted": false,
        "device_enforcement": true,
        "born_enforcing": true,
        "trusted_machines": [{
            "id": "c".repeat(32),
            "public_key": bytes(3),
            "subnet": "10.210.3.0/24",
        }],
        "passkeys": [{
            "passkey": {
                "credential_id": "BQECAw",
                "public_key": {
                    "algorithm": "eddsa",
                    "key": "bnoc3Smwt4_ROvTFWY_v9O8qlxZuPKby5Pv8zYBQW_E",
                },
                "account": "ada@example.com",
                "relying_party": {"id": "ployz.cloud", "origin": "https://ployz.cloud/"},
            },
            "approved_by": {"by": "server"},
        }],
        "membership_cluster": "BAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ=",
        "enrollment": {"as": {
            "kind": "founder",
            "name": "edge",
            "cluster_network": "10.210.0.0/16",
            "initial_policy": {
                "labels": {},
                "accepts_builds": true,
                "accepts_services": true,
                "accepts_ingress": true,
            },
            "advertised_endpoints": ["192.0.2.4:51820"],
        }}
    })
}

fn populated() -> LocalMachineRecord {
    serde_json::from_value(golden()).unwrap()
}

/// What a record born today holds apart from the identity, key and secret it is given.
fn born_defaults(body: Value) -> Value {
    json!({
        "body": body,
        "management_clients": {},
        "pinned_addresses": {"public_ip": false, "advertised_endpoints": false},
        "device_slots_vetted": true,
        "device_enforcement": false,
        "trusted_machines": [],
        "membership_cluster": null,
        "enrollment": null,
        "trusted_snapshot": {},
    })
}

fn without_key_material(mut row: Value) -> Value {
    let row = row.as_object_mut().unwrap();
    row.remove("wireguard_private_key");
    row.remove("management_secret");
    Value::Object(std::mem::take(row))
}

#[test]
fn a_record_written_today_reads_and_writes_back_unchanged() {
    let record = populated();

    assert_eq!(serde_json::to_value(&record).unwrap(), golden());
    assert_eq!(
        record.id(),
        MachineId::parse("a".repeat(32)).unwrap(),
        "the id lives inside the tagged body"
    );
}

/// The bytes a client key takes in `machine.json`: a JSON array of its 32 byte values.
fn key_text(byte: u8) -> String {
    format!("[{}]", vec![byte.to_string(); 32].join(","))
}

const EDGE: &str = concat!(
    r#"{"labels":{},"accepts_builds":true,"accepts_services":true,"accepts_ingress":true,"#,
    r#""id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"edge","subnet":"10.210.1.0/24","#,
    r#""public_key":[19,190,79,234,234,242,4,199,253,51,88,252,156,0,114,24,129,209,116,39,129,40,34,126,198,116,243,127,127,233,123,109],"#,
    r#""public_ip":null,"advertised_endpoints":["192.0.2.4:51820"],"#,
    r#""runtime":{"daemon_version":"","docker_version":"","hostname":"","architecture":"","os_pretty_name":"","kernel_version":"","memory_total_bytes":null,"running_builds":0,"pool_committable_bytes":null,"sealing":null,"store_filesystem":null,"store_syncs_commits":null,"capabilities":[],"device_enforcement":null,"management_identity":null,"leaving":false},"#,
    r#""build_concurrency":null}"#,
);

const PEER: &str = concat!(
    r#"{"labels":{},"accepts_builds":true,"accepts_services":true,"accepts_ingress":true,"#,
    r#""id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"peer","subnet":"10.210.2.0/24","#,
    r#""public_key":[5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5,5],"#,
    r#""public_ip":null,"advertised_endpoints":["192.0.2.4:51820"],"#,
    r#""runtime":{"daemon_version":"","docker_version":"","hostname":"","architecture":"","os_pretty_name":"","kernel_version":"","memory_total_bytes":null,"running_builds":0,"pool_committable_bytes":null,"sealing":null,"store_filesystem":null,"store_syncs_commits":null,"capabilities":[],"device_enforcement":null,"management_identity":null,"leaving":false},"#,
    r#""build_concurrency":null}"#,
);

/// A record's bytes as the daemon serializes them: `body`, then the keys and the defaults
/// a record born today holds.
fn record_text(body: &str) -> String {
    format!(
        concat!(
            r#"{{"body":{body},"wireguard_private_key":{private},"management_secret":{secret},"#,
            r#""management_clients":{{}},"pinned_addresses":{{"public_ip":false,"advertised_endpoints":false}},"#,
            r#""device_slots_vetted":true,"device_enforcement":false,"trusted_machines":[],"#,
            r#""membership_cluster":null,"enrollment":null,"trusted_snapshot":{{}}}}"#,
        ),
        body = body,
        private = key_text(7),
        secret = key_text(9),
    )
}

/// One checked-in body per phase, each Participating origin, and Resetting around each.
fn phase_bodies() -> Vec<(String, String)> {
    let uninitialized = r#"{"phase":"uninitialized","id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
    let joining = format!(
        r#"{{"phase":"joining","machine":{EDGE},"bootstrap":[{PEER}],"min_store_version":{{"actor":4}}}}"#
    );
    let joined = format!(
        r#"{{"phase":"participating","machine":{EDGE},"origin":"join","bootstrap":[{PEER}]}}"#
    );
    let founded = format!(
        r#"{{"phase":"participating","machine":{EDGE},"origin":"founder","cluster":{{"network":"10.210.0.0/16"}}}}"#
    );
    let live = [
        ("uninitialized".to_owned(), uninitialized.to_owned()),
        ("joining".to_owned(), joining),
        ("participating after a join".to_owned(), joined),
        ("participating as the founder".to_owned(), founded),
    ];
    let resetting = live.iter().map(|(name, body)| {
        (
            format!("resetting from {name}"),
            format!(r#"{{"phase":"resetting","prior":{body}}}"#),
        )
    });
    live.iter().cloned().chain(resetting).collect()
}

#[test]
fn a_record_in_each_phase_reads_and_writes_back_byte_for_byte() {
    for (name, body) in phase_bodies() {
        let text = record_text(&body);
        let record: LocalMachineRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&record).unwrap(), text, "{name}");
    }
}

#[test]
fn a_record_in_each_phase_reads_the_fields_the_phases_share() {
    let id = MachineId::parse("a".repeat(32)).unwrap();
    let peer = MachineId::parse("b".repeat(32)).unwrap();
    let network: Ipv4Net = "10.210.0.0/16".parse().unwrap();
    let expected = [
        (None, None, vec![], 0),
        (None, Some(id), vec![peer], 1),
        (None, Some(id), vec![peer], 0),
        (Some(network), Some(id), vec![], 0),
    ];
    for ((name, body), (cluster_network, machine, bootstrap, store_versions)) in
        phase_bodies().into_iter().zip(expected.iter().cycle())
    {
        let record: LocalMachineRecord = serde_json::from_str(&record_text(&body)).unwrap();
        assert_eq!(record.id(), id, "{name}");
        assert_eq!(record.cluster_network(), *cluster_network, "{name}");
        assert_eq!(
            record.machine().map(|machine| machine.id),
            *machine,
            "{name}"
        );
        assert_eq!(
            record
                .body()
                .bootstrap()
                .iter()
                .map(|machine| machine.id)
                .collect::<Vec<_>>(),
            *bootstrap,
            "{name}"
        );
        assert_eq!(record.min_store_version().len(), *store_versions, "{name}");
    }
}

#[test]
fn every_management_client_slot_state_is_written_as_arrays_of_key_bytes() {
    let mut row = golden();
    row.as_object_mut().unwrap().insert(
        "management_clients".into(),
        json!({
        "cleared": {"state": "cleared", "was": {"state": "rotating", "accepted": bytes(4), "pending": bytes(5)}},
        "cloud": {"state": "active", "accepted": bytes(1)},
        "cli": {"state": "rotating", "accepted": bytes(2), "pending": bytes(3)},
        "waiting": {"state": "pending", "pending": bytes(6)},
    }),
    );
    let record: LocalMachineRecord = serde_json::from_value(row).unwrap();

    let written = serde_json::to_string(&record).unwrap();

    let clients = format!(
        concat!(
            r#""management_clients":{{"#,
            r#""cleared":{{"state":"cleared","was":{{"state":"rotating","accepted":{a4},"pending":{p5}}}}},"#,
            r#""cli":{{"state":"rotating","accepted":{a2},"pending":{p3}}},"#,
            r#""cloud":{{"state":"active","accepted":{a1}}},"#,
            r#""waiting":{{"state":"pending","pending":{p6}}}}}"#,
        ),
        a1 = key_text(1),
        a2 = key_text(2),
        p3 = key_text(3),
        a4 = key_text(4),
        p5 = key_text(5),
        p6 = key_text(6),
    );
    assert!(written.contains(&clients), "{written}");
}

#[test]
fn a_record_born_through_parse_holds_the_defaults_a_file_stores() {
    let id = MachineId::parse("a".repeat(32)).unwrap();
    let record = LocalMachineRecord::parse(
        LocalMachineBody::Uninitialized { id },
        WireGuardPrivateKey::from_bytes([7; 32]),
    )
    .unwrap();

    assert_eq!(
        without_key_material(serde_json::to_value(&record).unwrap()),
        born_defaults(json!({"phase": "uninitialized", "id": "a".repeat(32)})),
    );
}

#[test]
fn a_store_opened_on_an_empty_directory_writes_the_same_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let store = LocalMachineStore::open(&data_dir).unwrap();
    let id = store.record().id();
    drop(store);

    let file: Value =
        serde_json::from_slice(&fs::read(data_dir.join("machine.json")).unwrap()).unwrap();
    assert_eq!(
        without_key_material(file),
        born_defaults(json!({"phase": "uninitialized", "id": id.as_str()})),
    );
}

#[test]
fn resetting_keeps_every_field_but_the_body() {
    let before = populated();
    let LocalMachineRecord {
        body: before_body,
        wireguard_private_key,
        management_secret,
        management_clients,
        wireguard_mtu,
        selected_endpoints,
        pinned_addresses,
        push_target,
        device_slots_vetted,
        filters_peers,
        born_enforcing,
        trusted_machines,
        passkeys,
        membership_cluster,
        enrollment,
        trusted_snapshot,
        activation,
        activated_at,
    } = before.clone();

    let LocalMachineRecord {
        body,
        wireguard_private_key: kept_private_key,
        management_secret: kept_secret,
        management_clients: kept_clients,
        wireguard_mtu: kept_mtu,
        selected_endpoints: kept_endpoints,
        pinned_addresses: kept_pins,
        push_target: kept_push_target,
        device_slots_vetted: kept_vetted,
        filters_peers: kept_filter,
        born_enforcing: kept_born,
        trusted_machines: kept_trusted,
        passkeys: kept_passkeys,
        membership_cluster: kept_cluster,
        enrollment: kept_enrollment,
        trusted_snapshot: kept_snapshot,
        activation: kept_activation,
        activated_at: kept_activated_at,
    } = before.into_resetting();

    assert_eq!(
        body,
        LocalMachineBody::Resetting {
            prior: Box::new(before_body.into_prior()),
        }
    );
    assert!(kept_private_key == wireguard_private_key);
    assert!(kept_secret == management_secret);
    assert_eq!(kept_clients, management_clients);
    assert_eq!(kept_mtu, wireguard_mtu);
    assert_eq!(kept_endpoints, selected_endpoints);
    assert_eq!(kept_pins, pinned_addresses);
    assert_eq!(kept_push_target, push_target);
    assert_eq!(kept_vetted, device_slots_vetted);
    assert_eq!(kept_filter, filters_peers);
    assert_eq!(kept_born, born_enforcing);
    assert_eq!(kept_trusted, trusted_machines);
    assert_eq!(kept_passkeys, passkeys);
    assert_eq!(kept_cluster, membership_cluster);
    assert_eq!(kept_enrollment, enrollment);
    assert_eq!(kept_snapshot, trusted_snapshot);
    assert_eq!(kept_activation, activation);
    assert_eq!(kept_activated_at, activated_at);
}

#[test]
fn the_record_lives_in_machine_json_under_the_data_directory() {
    assert_eq!(store::STATE_FILE_NAME, "machine.json");
    assert_eq!(
        store::path(Path::new("/var/lib/ployz")),
        Path::new("/var/lib/ployz/machine.json")
    );
}

#[test]
fn the_identity_reader_learns_the_id_target_and_secret_from_a_written_record() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(store::path(dir.path()), golden().to_string()).unwrap();

    let identity = store::identity(dir.path()).unwrap();

    assert_eq!(identity.id, MachineId::parse("a".repeat(32)).unwrap());
    assert_eq!(
        identity.push_target.as_ref().map(Url::as_str),
        Some("https://cloud.example/push")
    );
    assert!(identity.management_secret == *populated().management_secret());
}

#[test]
fn the_identity_reader_sees_a_record_with_no_push_target_and_none_for_a_missing_or_broken_one() {
    let dir = tempfile::tempdir().unwrap();
    assert!(store::identity(dir.path()).is_none());

    fs::write(store::path(dir.path()), b"{").unwrap();
    assert!(store::identity(dir.path()).is_none());

    let mut row = golden();
    row.as_object_mut().unwrap().remove("push_target");
    fs::write(store::path(dir.path()), row.to_string()).unwrap();
    let identity = store::identity(dir.path()).unwrap();
    assert!(identity.push_target.is_none());
}
