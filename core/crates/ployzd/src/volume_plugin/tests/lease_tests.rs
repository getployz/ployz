//! Lease records, switch verbs and the writer guard through the plugin routes.

use super::*;

/// Serves the plugin over fake ZFS with the given marker files present.
pub(super) fn start(
    test: &TestDir,
    pools: &str,
    markers: &[&str],
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, pools);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    (socket, server)
}

/// Writes a ZFS user property the fake will answer for `dataset`.
pub(super) fn set_property(test: &TestDir, dataset: &str, property: &str, value: &str) {
    let directory = test.0.join("props").join(dataset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(property), format!("{value}\n")).unwrap();
}

fn commands(test: &TestDir) -> String {
    fs::read_to_string(test.0.join("commands")).unwrap()
}

fn declare(name: &str, lease: u64) -> Value {
    json!({
        "switch": {"lease": lease, "pos": {"seq": 3, "round": 0, "sub": 0}},
        "name": name,
        "refquota_bytes": 1_073_741_824_u64,
    })
}

#[tokio::test]
async fn a_first_switch_verb_creates_the_managed_root_to_hold_the_record() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &[]);

    let response = post(&socket, "/Volume.DeclareMirror", declare("data", 1)).await;
    assert_eq!(
        response.pointer("/Ok/decision"),
        Some(&json!("adopt")),
        "{response}"
    );
    let log = commands(&test);
    assert!(
        log.contains("zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz"),
        "{log}"
    );
    assert!(
        log.contains("zfs set ployz:lease.data=1:3.0.0:closed tank/ployz"),
        "{log}"
    );
    server.abort();
}

#[tokio::test]
async fn lease_records_keep_apart_names_that_differ_only_in_case() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root"]);
    let names = [("data", 1), ("Data", 2), ("DATA", 3), ("dAta-B_1.x", 4)];

    for (name, lease) in names {
        let response = post(
            &socket,
            "/Volume.DestroyMirror",
            json!({"switch": {"lease": lease, "pos": {"seq": 3, "round": 0, "sub": 0}}, "name": name}),
        )
        .await;
        assert!(response.get("Ok").is_some(), "{name}: {response}");
    }
    for (name, lease) in names {
        let response = post(&socket, "/Volume.Inspect", json!({"name": name})).await;
        assert_eq!(
            response.pointer("/Ok/lease"),
            Some(
                &json!({"lease": lease, "pos": {"seq": 3, "round": 0, "sub": 0}, "cycle": "closed"})
            ),
            "{name}: {response}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_root_with_its_marker_and_newest_snapshot() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz/data", "ployz:writer", "frozen:42");
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@f-2\t42\t1700000000\ntank/ployz/data@zfs-auto-snap\t7\t1600000000",
    );
    set_property(&test, "tank/ployz", "ployz:lease.data", "2:6.0.0:open");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "copy": {
                "kind": "root",
                "writer": {"phase": "frozen", "guid": "42"},
                "readonly": false,
                "newest": {"name": "f-2", "guid": "42", "created_unix_seconds": 1_700_000_000},
            },
            "lease": {"lease": 2, "pos": {"seq": 6, "round": 0, "sub": 0}, "cycle": "open"},
        }})
    );
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_slot_and_nothing_for_an_unknown_name() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz-mirror/copy", "ployz:mirror", "final:9");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "copy"})).await;
    assert_eq!(
        response.pointer("/Ok/copy").unwrap(),
        &json!({
            "kind": "slot",
            "mirror": {"phase": "final", "guid": "9"},
            "readonly": true,
            "newest": null,
            "resume_token": null,
        })
    );
    let response = post(&socket, "/Volume.Inspect", json!({"name": "missing"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null, "lease": null}}));
    server.abort();
}

#[tokio::test]
async fn inspect_without_a_pool_holds_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null, "lease": null}}));
    server.abort();
}

#[tokio::test]
async fn inspect_refuses_a_marker_it_cannot_read() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz/data", "ployz:writer", "melting");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert!(
        response
            .pointer("/Err/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("melting")),
        "{response}"
    );
    server.abort();
}

#[tokio::test]
async fn remove_refuses_handed_root() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz/data", "ployz:writer", "handed:42");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name": "data"})).await;
    let message = error(&response);
    assert!(message.starts_with("VolumeSwitching:"), "{message}");
    assert!(message.contains("handed:42"), "{message}");
    assert!(!commands(&test).contains("zfs destroy"));
    assert!(test.0.join("volume").exists());

    set_property(&test, "tank/ployz/data", "ployz:writer", "idle");
    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name": "data"})).await,
        json!({"Err": ""})
    );
    assert!(commands(&test).contains("zfs destroy -r tank/ployz/data"));
    server.abort();
}
