//! Volume.Inspect through the plugin route, and the helpers other plugin tests start from.

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

#[tokio::test]
async fn inspect_reports_a_root_with_its_newest_snapshot() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@f-2\t42\t1700000000\ntank/ployz/data@zfs-auto-snap\t7\t1600000000",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "copy": {
                "kind": "root",
                "readonly": false,
                "newest": {"name": "f-2", "guid": "42", "created_unix_seconds": 1_700_000_000},
                "epoch": 0,
                "seed": false,
            },
        }})
    );
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_slot_and_nothing_for_an_unknown_name() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "copy"})).await;
    assert_eq!(
        response.pointer("/Ok/copy").unwrap(),
        &json!({
            "kind": "slot",
            "slot": "1",
            "readonly": true,
            "newest": null,
            "resume_token": null,
            "pull": null,
        })
    );
    let response = post(&socket, "/Volume.Inspect", json!({"name": "missing"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null}}));
    server.abort();
}

#[tokio::test]
async fn inspect_without_a_pool_holds_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null}}));
    server.abort();
}
