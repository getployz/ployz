//! Departure over fake ZFS: every root becomes a slot, every record moves on, and the
//! roles Storage.Inspect and Mount derive from the markers.

use super::lease_tests::{set_property, start};
use super::mirror_tests::{commands, property, snapshot_names};
use super::*;

#[tokio::test]
async fn departure_demotes_each_root_to_a_slot_and_bumps_every_record() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "3:5.2.1:open");
    set_property(&test, "tank/ployz", "ployz:lease.gone", "1:2.0.0:closed");
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@dep-100\t900\t1690000000\ntank/ployz/data@w-3-1\t901\t1690000001",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mounted"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": ["data"]}));

    let log = commands(&test);
    let order = [
        "zfs unmount tank/ployz/data\n",
        "zfs destroy tank/ployz/data@dep-100\n",
        "zfs snapshot tank/ployz/data@dep-",
        "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror\n",
        "zfs create -o canmount=off -o readonly=on -o refquota=1073741824 tank/ployz-mirror/data\n",
        "zfs set ployz:mirror=idle tank/ployz-mirror/data\n",
        "zfs rename tank/ployz/data tank/ployz-mirror/data/fs\n",
        "zfs set readonly=on tank/ployz-mirror/data/fs\n",
        "zfs inherit ployz:writer tank/ployz-mirror/data/fs\n",
    ];
    let mut cursor = 0;
    for step in order {
        let at = log[cursor..]
            .find(step)
            .unwrap_or_else(|| panic!("{step:?} missing after offset {cursor} in {log}"));
        cursor += at + step.len();
    }
    assert!(!log.contains("zfs destroy tank/ployz/data@w-3-1"), "{log}");
    let kept = snapshot_names(&test, "tank/ployz-mirror/data/fs");
    assert!(kept.iter().any(|name| name.starts_with("dep-")), "{kept:?}");
    assert!(kept.contains(&"w-3-1".to_owned()), "{kept:?}");
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("4:2.0.0:closed")
    );
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.gone").as_deref(),
        Some("2:2.0.0:closed")
    );

    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(capacity.pointer("/Ok/volumes").unwrap(), &json!({}));
    assert_eq!(
        capacity.pointer("/Ok/copies/data/role").unwrap(),
        &json!("slot")
    );
    let listed = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(listed.pointer("/Volumes").unwrap(), &json!([]));
    server.abort();
}

#[tokio::test]
async fn departure_without_a_pool_demotes_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": []}));
    server.abort();
}

#[tokio::test]
async fn departure_idles_a_slot_marker_and_keeps_its_data() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz-mirror/copy", "ployz:mirror", "final");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": []}));
    assert_eq!(
        property(&test, "tank/ployz-mirror/copy", "ployz:mirror").as_deref(),
        Some("idle")
    );
    let log = commands(&test);
    assert!(!log.contains("zfs rename"), "{log}");
    assert!(!log.contains("zfs destroy"), "{log}");
    server.abort();
}

/// The read-only slot `data` departs into, with no writer marker and the departure
/// snapshot kept, as Storage.Inspect reports it.
async fn assert_departed_into_a_slot(test: &TestDir, socket: &Path) {
    assert!(!test.0.join("volume").exists(), "the root is still there");
    assert!(test.0.join("mirror-fs").exists(), "the slot holds no fs");
    assert_eq!(
        property(test, "tank/ployz-mirror/data/fs", "readonly").as_deref(),
        Some("on")
    );
    assert_eq!(
        property(test, "tank/ployz-mirror/data/fs", "ployz:writer"),
        None
    );
    let kept = snapshot_names(test, "tank/ployz-mirror/data/fs");
    assert!(kept.iter().any(|name| name.starts_with("dep-")), "{kept:?}");
    let capacity = post(socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/copies/data/role").unwrap(),
        &json!("slot"),
        "{capacity}"
    );
}

fn error_message(response: &Value) -> &str {
    response
        .pointer("/Err/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn departure_moves_a_root_into_the_empty_slot_beside_it() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mirror"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": ["data"]}));
    assert_departed_into_a_slot(&test, &socket).await;
    let log = commands(&test);
    assert!(
        !log.contains("zfs create -o canmount=off -o readonly=on"),
        "{log}"
    );
    server.abort();
}

#[tokio::test]
async fn departure_refuses_a_root_beside_a_slot_that_holds_a_copy() {
    let test = TestDir::new();
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "mirror", "mirror-fs"],
    );

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        error_message(&response).contains("both a writer and a mirror"),
        "{response}"
    );
    let log = commands(&test);
    assert!(!log.contains("zfs rename"), "{log}");
    assert!(!log.contains("zfs snapshot"), "{log}");
    assert!(test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn departure_interrupted_before_the_rename_finishes_on_retry() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "rename-fails"]);

    let interrupted = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        error_message(&interrupted).contains("rename interrupted"),
        "{interrupted}"
    );
    assert!(
        test.0.join("mirror").exists(),
        "the slot parent was not made"
    );
    fs::remove_file(test.0.join("rename-fails")).unwrap();

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(retried, json!({"Ok": ["data"]}));
    assert_departed_into_a_slot(&test, &socket).await;
    server.abort();
}

#[tokio::test]
async fn departure_interrupted_after_the_rename_seals_the_slot_on_retry() {
    for toggle in ["props/fail-properties", "inherit-fails"] {
        let test = TestDir::new();
        set_property(&test, "tank/ployz/data", "ployz:writer", "stopping");
        let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
        fs::write(test.0.join(toggle), "").unwrap();

        let interrupted = post(&socket, "/Storage.Demote", json!(null)).await;
        assert!(
            error_message(&interrupted).contains("property unavailable"),
            "{toggle}: {interrupted}"
        );
        assert!(test.0.join("mirror-fs").exists(), "{toggle}: no rename");
        fs::remove_file(test.0.join(toggle)).unwrap();

        let retried = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_departed_into_a_slot(&test, &socket).await;
        assert_eq!(retried, json!({"Ok": ["data"]}), "{toggle}");
        server.abort();
    }
}

#[tokio::test]
async fn storage_inspection_derives_each_copy_role() {
    struct Case {
        markers: &'static [&'static str],
        writer: Option<&'static str>,
        record: Option<&'static str>,
        role: &'static str,
        listed: bool,
    }
    let cases = [
        Case {
            markers: &["root", "volume"],
            writer: None,
            record: None,
            role: "writer",
            listed: true,
        },
        Case {
            markers: &["root", "volume"],
            writer: None,
            record: Some("2:4.0.0:closed"),
            role: "writer",
            listed: true,
        },
        Case {
            markers: &["root", "volume"],
            writer: Some("stopping"),
            record: None,
            role: "switching",
            listed: true,
        },
        Case {
            markers: &["root", "volume"],
            writer: None,
            record: Some("2:4.0.0:open"),
            role: "switching",
            listed: true,
        },
        Case {
            markers: &["root", "volume", "readonly-volume"],
            writer: None,
            record: None,
            role: "switching",
            listed: true,
        },
        Case {
            markers: &["root", "mirror", "mirror-fs"],
            writer: None,
            record: None,
            role: "slot",
            listed: false,
        },
    ];
    for case in cases {
        let test = TestDir::new();
        if let Some(writer) = case.writer {
            set_property(&test, "tank/ployz/data", "ployz:writer", writer);
        }
        if let Some(record) = case.record {
            set_property(&test, "tank/ployz", "ployz:lease.data", record);
        }
        let (socket, server) = start(&test, USABLE_POOL, case.markers);
        let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(
            capacity.pointer("/Ok/copies/data/role").unwrap(),
            &json!(case.role),
            "{:?} {:?} {:?}: {capacity}",
            case.markers,
            case.writer,
            case.record
        );
        assert_eq!(
            capacity.pointer("/Ok/volumes/data").is_some(),
            case.listed,
            "{:?}: {capacity}",
            case.markers
        );
        server.abort();
    }
}

#[tokio::test]
async fn mount_admits_only_an_idle_closed_root() {
    for (writer, record, admitted) in [
        (None, None, true),
        (None, Some("2:4.0.0:closed"), true),
        (Some("stopping"), None, false),
        (Some("frozen:42"), None, false),
        (None, Some("2:4.0.0:open"), false),
    ] {
        let test = TestDir::new();
        if let Some(writer) = writer {
            set_property(&test, "tank/ployz/data", "ployz:writer", writer);
        }
        if let Some(record) = record {
            set_property(&test, "tank/ployz", "ployz:lease.data", record);
        }
        let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
        let response = post(
            &socket,
            "/VolumeDriver.Mount",
            json!({"Name": "data", "ID": "container"}),
        )
        .await;
        let error = response
            .get("Err")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if admitted {
            assert_eq!(error, "", "{writer:?} {record:?}: {response}");
            assert_eq!(
                response.get("Mountpoint").and_then(Value::as_str),
                Some("/var/lib/ployz-volumes/data")
            );
        } else {
            assert!(
                error.starts_with("VolumeSwitching:"),
                "{writer:?} {record:?}: {response}"
            );
        }
        server.abort();
    }
}
