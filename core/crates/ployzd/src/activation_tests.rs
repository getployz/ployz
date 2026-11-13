use std::{cell::RefCell, collections::BTreeMap, io, path::Path};

use ployz_core::{
    DeviceKey, EnrollAs, EnrollRequest, MachineId, MachineName, MachineSubnet, ManagementScope,
    OperationId, SealingPublicKey, WireGuardPublicKey,
    authority::{
        Member, Signed, Signer, Timestamp, TrustedPasskey,
        join::{JoinAllocation, JoinApproval},
    },
};

use super::{DURABLE, PluginUnit, Rule, commit_pending, rule_of};
use crate::machine::{Evidence, LocalMachineStore, local_machine::PendingActivation};

#[derive(Default)]
struct Plugin(RefCell<Vec<&'static str>>);

impl PluginUnit for Plugin {
    fn stop(&self) -> io::Result<()> {
        self.0.borrow_mut().push("stop");
        Ok(())
    }

    fn start(&self) -> io::Result<()> {
        self.0.borrow_mut().push("start");
        Ok(())
    }
}

fn signer(seed: u8) -> Signer {
    Signer::from_seed(&[seed; 32])
}

fn device(seed: u8) -> DeviceKey {
    DeviceKey::from(signer(seed).public())
}

fn passkey() -> TrustedPasskey {
    use ployz_core::authority::{
        Passkey, PasskeyAccount, PasskeyApprover, PasskeyPublicKey,
        trusted_key::{Base64Url, PasskeyAlgorithm, RelyingParty},
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[5; 32]).verifying_key();
    TrustedPasskey {
        passkey: Passkey {
            credential_id: Base64Url::new(vec![5, 1, 2, 3]).try_into().unwrap(),
            public_key: PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes()).unwrap(),
            account: PasskeyAccount::parse("ada@example.com").unwrap(),
            relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
        },
        approved_by: PasskeyApprover::Server,
    }
}

/// A founded Server holding Device 1, installed over its local socket, and Device 2 and a
/// passkey from before its first 0.4 start.
fn founded() -> (tempfile::TempDir, LocalMachineStore) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = LocalMachineStore::open(dir.path().join("data")).unwrap();
    store
        .enroll(EnrollRequest {
            role: EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("edge").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: Some("198.51.100.7".parse().unwrap()),
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "198.51.100.7:51820".parse().unwrap(),
                )],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    store
        .approve_device(device(1), Some(Evidence::LocalSocket))
        .unwrap();
    store.approve_device(device(2), None).unwrap();
    store.approve_passkey(passkey(), None).unwrap();
    (dir, store)
}

fn pending(store: &LocalMachineStore) -> PendingActivation {
    PendingActivation {
        operation: OperationId::random(),
        members: vec![Member::from(store.record().machine().unwrap())],
        signer: device(1).code(),
        signer_evidence: Evidence::LocalSocket,
        kept_keys: Vec::new(),
    }
}

fn reopened(dir: &tempfile::TempDir, store: LocalMachineStore) -> LocalMachineStore {
    drop(store);
    LocalMachineStore::open(dir.path().join("data")).unwrap()
}

#[test]
fn activation_is_a_process_boundary() {
    let (dir, store) = founded();
    let admitted = pending(&store);
    admitted.write(store.data_dir()).unwrap();
    assert_eq!(store.record().activation(), None);
    let mut store = reopened(&dir, store);
    let plugin = Plugin::default();

    commit_pending(&mut store, &plugin).unwrap();

    let record = store.record();
    assert_eq!(record.activation(), Some(admitted.operation));
    assert!(record.filters_peers());
    assert_eq!(*plugin.0.borrow(), ["stop", "start"]);
    assert!(PendingActivation::read(store.data_dir()).unwrap().is_none());
    assert_eq!(
        record.management_scope(&device(1).into()),
        Some(ManagementScope::Full)
    );
    assert_eq!(record.management_scope(&device(2).into()), None);
    assert!(record.trusted_keys().passkeys.is_empty());
    let snapshot = record.trusted_snapshot().unwrap();
    assert!(snapshot.holds_member(admitted.members.first().unwrap()));

    let mut store = reopened(&dir, store);
    let plugin = Plugin::default();
    commit_pending(&mut store, &plugin).unwrap();

    assert_eq!(store.record().activation(), Some(admitted.operation));
    assert!(plugin.0.borrow().is_empty());
    assert_eq!(store.record().management_scope(&device(2).into()), None);
}

#[test]
fn a_key_the_person_kept_by_fingerprint_survives_the_commit() {
    let (dir, store) = founded();
    let admitted = PendingActivation {
        kept_keys: vec![device(2).code(), passkey().passkey.code()],
        ..pending(&store)
    };
    admitted.write(store.data_dir()).unwrap();
    let mut store = reopened(&dir, store);

    commit_pending(&mut store, &Plugin::default()).unwrap();

    let record = store.record();
    assert_eq!(
        record.management_scope(&device(2).into()),
        Some(ManagementScope::Full)
    );
    assert_eq!(record.trusted_keys().passkeys.len(), 1);
    assert_eq!(
        record
            .trusted_snapshot()
            .unwrap()
            .keys()
            .get(&device(2).code()),
        Some(&Evidence::Activation {
            operation: admitted.operation
        })
    );
}

#[test]
fn a_commit_cut_after_its_write_starts_the_plugin_on_the_next_boot() {
    let (dir, store) = founded();
    let admitted = pending(&store);
    admitted.write(store.data_dir()).unwrap();
    let mut store = reopened(&dir, store);
    commit_pending(&mut store, &Plugin::default()).unwrap();
    admitted.write(store.data_dir()).unwrap();
    let mut store = reopened(&dir, store);
    let plugin = Plugin::default();

    commit_pending(&mut store, &plugin).unwrap();

    assert_eq!(*plugin.0.borrow(), ["start"]);
    assert!(PendingActivation::read(store.data_dir()).unwrap().is_none());
}

#[test]
fn enforcement_can_not_be_turned_off_after_activation_and_reload() {
    let (dir, store) = founded();
    pending(&store).write(store.data_dir()).unwrap();
    let mut store = reopened(&dir, store);
    commit_pending(&mut store, &Plugin::default()).unwrap();
    let path = dir.path().join("data").join("machine.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    *json.get_mut("device_enforcement").unwrap() = serde_json::Value::Bool(false);
    drop(store);
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

    let store = LocalMachineStore::open(dir.path().join("data")).unwrap();

    assert!(store.record().filters_peers());
}

fn join_approval(id: MachineId, signer: &Signer) -> (JoinApproval, Signed<JoinApproval>) {
    let issued_at = Timestamp::now();
    let body = JoinApproval {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        machine: JoinAllocation {
            id,
            name: MachineName::parse("joiner").unwrap(),
            subnet: "10.210.6.0/24".parse::<MachineSubnet>().unwrap(),
            public_key: WireGuardPublicKey([6; 32]),
            public_ip: None,
            advertised_endpoints: vec![],
            policy: ployz_core::InitialMachinePolicy {
                labels: BTreeMap::new(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
            },
        },
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: WireGuardPublicKey([6; 32]),
        devices: vec![DeviceKey::from(signer.public())],
        passkeys: Vec::new(),
        admitting_machines: vec![],
        bootstrap: vec![],
        issued_at,
        expires_at: issued_at.plus(600_000),
        signer: DeviceKey::from(signer.public()),
    };
    let signed = Signed::sign(&body, signer).unwrap();
    (body, signed)
}

#[test]
fn boot_reverifies_durable_state() {
    let (dir, store) = founded();
    let data_dir = store.data_dir().to_path_buf();
    let kept = MachineId::parse("6".repeat(32)).unwrap();
    let legacy = MachineId::parse("7".repeat(32)).unwrap();
    let unapproved = MachineId::parse("8".repeat(32)).unwrap();
    let (kept_body, kept_signed) = join_approval(kept, &signer(1));
    let (legacy_body, legacy_signed) = join_approval(legacy, &signer(2));
    let (unapproved_body, _) = join_approval(unapproved, &signer(2));
    std::fs::write(
        data_dir.join("join-approvals.json"),
        serde_json::to_vec(&serde_json::json!({
            "proposals": {
                kept.as_str(): {"body": kept_body, "approval": kept_signed},
                legacy.as_str(): {"body": legacy_body, "approval": legacy_signed},
                unapproved.as_str(): {"body": unapproved_body, "approval": null},
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(data_dir.join("recipient-generated.key"), [9; 64]).unwrap();
    std::fs::create_dir_all(data_dir.join("tailscale.forgotten")).unwrap();
    std::fs::write(
        data_dir.join("tailscale.forgotten/tailscaled.state"),
        b"node",
    )
    .unwrap();
    pending(&store).write(&data_dir).unwrap();
    let mut store = reopened(&dir, store);

    commit_pending(&mut store, &Plugin::default()).unwrap();

    let joins: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data_dir.join("join-approvals.json")).unwrap())
            .unwrap();
    let proposals = joins.get("proposals").unwrap().as_object().unwrap();
    assert!(proposals.contains_key(kept.as_str()));
    assert!(!proposals.contains_key(legacy.as_str()));
    assert!(proposals.contains_key(unapproved.as_str()));
    assert!(!data_dir.join("recipient-generated.key").exists());
    assert!(!data_dir.join("tailscale.forgotten").exists());
}

#[test]
fn every_entry_of_the_data_directory_has_a_boot_rule() {
    let (dir, store) = founded();
    pending(&store).write(store.data_dir()).unwrap();
    let mut store = reopened(&dir, store);
    commit_pending(&mut store, &Plugin::default()).unwrap();

    let mut unruled = Vec::new();
    walk(store.data_dir(), store.data_dir(), &mut unruled);

    assert!(unruled.is_empty(), "no boot rule for {unruled:?}");
    for (name, rule) in DURABLE {
        if let Rule::Kept(why) = rule {
            assert!(!why.is_empty(), "{name} keeps no reason");
        }
    }
}

fn walk(root: &Path, dir: &Path, unruled: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let relative = path.strip_prefix(root).unwrap();
        if rule_of(relative).is_none() {
            if path.is_dir() {
                walk(root, &path, unruled);
            } else {
                unruled.push(relative.display().to_string());
            }
        }
    }
}

/// Names a source file in this crate gives a file that lives outside the data directory,
/// and where it lives.
const ELSEWHERE: &[(&str, &str)] = &[
    ("dns.json", "the run directory"),
    (".install.lock", "the run directory"),
    (".api-token", "corrosion/"),
    ("account.json", "acme/"),
    ("/etc/docker/daemon.json", "Docker's configuration"),
];

/// Every file name the daemon's own code declares has a row here or a place outside the
/// data directory, so a new durable file can't skip its boot rule. The observe store
/// lives under Docker's root directory.
#[test]
fn every_file_the_daemon_names_has_a_boot_rule() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut unruled = Vec::new();
    let mut seen = Vec::new();
    names_in(&src, &mut |name| {
        seen.push(name.to_owned());
        let ruled = rule_of(Path::new(name)).is_some()
            || ELSEWHERE.iter().any(|(elsewhere, _)| *elsewhere == name);
        if !ruled {
            unruled.push(name.to_owned());
        }
    });

    assert!(seen.iter().any(|name| name == "restart-approvals.json"));
    assert!(seen.iter().any(|name| name == "recipient.key"));
    assert!(unruled.is_empty(), "no boot rule for {unruled:?}");
}

fn names_in(dir: &Path, found: &mut impl FnMut(&str)) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "observe" && !name.contains("test") {
                names_in(&path, found);
            }
            continue;
        }
        if !name.ends_with(".rs") || name.ends_with("tests.rs") || name.contains("test_") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let text = text.split("#[cfg(test)]\nmod").next().unwrap();
        for line in text.lines() {
            let Some((_, value)) = line.trim().split_once(": &str = \"") else {
                continue;
            };
            if !line.trim_start().starts_with("const ") && !line.contains(" const ") {
                continue;
            }
            let value = value.trim_end_matches("\";");
            let file = [".json", ".key", ".db", ".pending", ".lock", ".tmp", ".toml"]
                .iter()
                .any(|extension| value.ends_with(extension))
                || (value.starts_with('.') && value.len() > 1 && !value.contains('/'));
            if file {
                found(value);
            }
        }
    }
}
