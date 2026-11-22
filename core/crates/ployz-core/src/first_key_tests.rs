use super::*;

fn device(seed: u8) -> LineKey {
    LineKey::Device {
        key: crate::authority::Signer::from_seed(&[seed; 32])
            .public()
            .into(),
    }
}

#[test]
fn an_install_line_never_replaces_a_spent_or_other_waiting_key() {
    let root = tempfile::tempdir().unwrap();
    let file = FirstKeyFile::beside(&root.path().join("data"));
    let held = file.lock().unwrap();

    assert_eq!(held.arm(&device(1)).unwrap(), Armed::Created);
    assert_eq!(held.arm(&device(1)).unwrap(), Armed::AlreadyWaiting);
    assert_eq!(
        held.arm(&device(2)).unwrap(),
        Armed::Held(FirstKey::Waiting { key: device(1) })
    );

    let at = Timestamp::from_millis(1_000).unwrap();
    held.spend(at).unwrap();
    let spent = FirstKey::Spent {
        code: Some(device(1).code()),
        at,
    };
    assert_eq!(held.read().unwrap(), Some(spent.clone()));
    assert_eq!(held.arm(&device(1)).unwrap(), Armed::Held(spent));
}

#[test]
fn a_line_key_reads_back_as_printed_and_refuses_anything_else() {
    let key = device(3);
    assert_eq!(LineKey::parse(&key.to_string()).unwrap(), key);
    for line in [
        "",
        "pk1_",
        "pk2_e30",
        "pk1_e30",
        &format!("pk1_{}", "A".repeat(5000)),
    ] {
        assert!(LineKey::parse(line).is_err(), "{line:.12}");
    }
}
