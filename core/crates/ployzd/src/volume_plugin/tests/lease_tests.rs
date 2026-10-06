//! Lease records, switch verbs and the writer guard through the plugin routes.

use super::*;

const FAR_FUTURE: i64 = 4_102_444_800;

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

fn property(test: &TestDir, dataset: &str, property: &str) -> Option<String> {
    fs::read_to_string(test.0.join("props").join(dataset).join(property))
        .ok()
        .map(|value| value.trim_end().to_owned())
}

fn adopt(lease: u64, not_after: i64) -> Value {
    json!({
        "switch": {
            "lease": lease,
            "pos": {"seq": 2, "round": 0, "sub": 0},
            "not_after_unix_seconds": not_after,
        },
        "name": "data",
    })
}

fn commands(test: &TestDir) -> String {
    fs::read_to_string(test.0.join("commands")).unwrap()
}

#[tokio::test]
async fn adopt_lease_records_a_closed_lease_and_answers_the_copy() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.AdoptLease", adopt(1, FAR_FUTURE)).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "decision": "adopt",
            "lease": {"lease": 1, "pos": {"seq": 2, "round": 0, "sub": 0}, "cycle": "closed"},
            "copy": {"kind": "root", "writer": {"phase": "idle"}, "readonly": false, "newest": null},
        }})
    );
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("1:2.0.0:closed")
    );
    server.abort();
}

#[tokio::test]
async fn adopt_lease_preserves_open() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "3:7.0.0:open");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.AdoptLease", adopt(4, FAR_FUTURE)).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "adopt");
    assert_eq!(response.pointer("/Ok/lease/cycle").unwrap(), "open");
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("4:2.0.0:open")
    );
    server.abort();
}

#[tokio::test]
async fn adopt_lease_refuses_stale_or_expired_requests_and_replays_its_own() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "4:2.0.0:closed");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let stale = post(&socket, "/Volume.AdoptLease", adopt(3, FAR_FUTURE)).await;
    assert_eq!(stale.pointer("/Err/code").unwrap(), "conflict");
    assert_eq!(stale.pointer("/Err/details/reason").unwrap(), "stale_lease");

    let expired = post(&socket, "/Volume.AdoptLease", adopt(5, 0)).await;
    assert_eq!(expired.pointer("/Err/details/reason").unwrap(), "expired");
    assert!(
        expired
            .pointer("/Err/details/skew_seconds")
            .and_then(Value::as_i64)
            .is_some_and(|skew| skew > 0)
    );

    let replay = post(&socket, "/Volume.AdoptLease", adopt(4, FAR_FUTURE)).await;
    assert_eq!(replay.pointer("/Ok/decision").unwrap(), "replay");

    assert!(!commands(&test).contains("zfs set"));
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("4:2.0.0:closed")
    );
    server.abort();
}

#[tokio::test]
async fn adopt_lease_creates_the_managed_root_to_hold_the_record() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &[]);

    let response = post(&socket, "/Volume.AdoptLease", adopt(1, FAR_FUTURE)).await;
    assert_eq!(response.pointer("/Ok/copy").unwrap(), &Value::Null);
    let log = commands(&test);
    assert!(
        log.contains("zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz")
    );
    assert!(log.contains("zfs set ployz:lease.data=1:2.0.0:closed tank/ployz"));
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_root_with_its_marker_and_newest_snapshot() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz/data", "ployz:writer", "frozen:42");
    set_property(&test, "tank/ployz/data", "snapshots", "42\t1700000000");
    set_property(&test, "tank/ployz", "ployz:lease.data", "2:6.0.0:open");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "copy": {
                "kind": "root",
                "writer": {"phase": "frozen", "guid": 42},
                "readonly": false,
                "newest": {"guid": 42, "created_unix_seconds": 1_700_000_000},
            },
            "lease": {"lease": 2, "pos": {"seq": 6, "round": 0, "sub": 0}, "cycle": "open"},
        }})
    );
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_slot_and_nothing_for_an_unknown_name() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz-mirror/copy/fs",
        "ployz:mirror",
        "final:9",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "copy"})).await;
    assert_eq!(
        response.pointer("/Ok/copy").unwrap(),
        &json!({
            "kind": "slot",
            "mirror": {"phase": "final", "guid": 9},
            "readonly": true,
            "newest": null,
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
