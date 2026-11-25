//! A preview's Close through the plugin route: what it destroys, what it moves to the
//! Trash, and when it refuses because it can't be sure what the Pool holds.

use ployz_core::{Namespace, PreviewNamespace};

use crate::volume_copy::PREVIEW_CLOSE_ROUTE;

use super::inspect_tests::set_property;
use super::*;

const NAMESPACE: &str = "pv-1111111111111111-22222222-0000aaaa";
const VOLUME: &str = "pv-1111111111111111-22222222-0000aaaa_data";
const ROOT: &str = "tank/ployz/pv-1111111111111111-22222222-0000aaaa_data";
const SLOT: &str = "tank/ployz-mirror/pv-1111111111111111-22222222-0000aaaa_data";

fn own_origin() -> String {
    PreviewNamespace::of(&Namespace::parse(NAMESPACE).unwrap())
        .unwrap()
        .own_origin(VOLUME)
        .unwrap()
        .to_string()
}

/// Serves the plugin over a fake Pool holding `markers`, and closes the preview once.
async fn close(test: &TestDir, pools: &str, markers: &[&str]) -> Value {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs_seeding(&test.0, pools, VOLUME);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    let route = format!("/{PREVIEW_CLOSE_ROUTE}");
    let response = post(&socket, &route, json!({"namespace": NAMESPACE})).await;
    server.abort();
    response
}

#[tokio::test]
async fn a_close_destroys_a_root_the_preview_made_and_trashes_one_it_did_not() {
    let made = test_dir();
    set_property(&made, ROOT, "ployz:origin", &own_origin());
    let response = close(&made, USABLE_POOL, &["root", "seed-volume"]).await;
    assert_eq!(
        response,
        json!({"Ok": {"destroyed": [VOLUME], "kept": [], "trashed": []}})
    );
    assert!(!made.0.join("seed-volume").exists());
    assert!(trashed(&made).is_empty());

    let foreign = test_dir();
    set_property(&foreign, ROOT, "ployz:origin", &"1".repeat(64));
    let response = close(&foreign, USABLE_POOL, &["root", "seed-volume"]).await;
    assert_eq!(
        response.pointer("/Ok/trashed/0/name"),
        Some(&json!(VOLUME)),
        "{response}"
    );
    assert_eq!(
        property(&foreign, &trashed_one(&foreign, "root"), "ployz:origin"),
        Some("1".repeat(64))
    );
    assert!(!commands(&foreign).contains("zfs destroy"));
}

#[tokio::test]
async fn a_close_never_destroys_a_dataset_below_a_preview_copy() {
    let root = test_dir();
    set_property(&root, ROOT, "ployz:origin", &own_origin());
    let response = close(&root, USABLE_POOL, &["root", "seed-volume", "seed-child"]).await;
    assert!(
        error_text(&response).contains(&format!("{ROOT}/child")),
        "{response}"
    );
    assert!(root.0.join("seed-child").exists());
    assert!(root.0.join("seed-volume").exists());

    let slot = test_dir();
    set_property(&slot, SLOT, "ployz:pull-origin", &own_origin());
    let response = close(
        &slot,
        USABLE_POOL,
        &["seed-slot", "seed-fs", "seed-slot-child"],
    )
    .await;
    assert!(
        error_text(&response).contains(&format!("{SLOT}/other")),
        "{response}"
    );
    assert!(slot.0.join("seed-slot-child").exists());
    assert!(slot.0.join("seed-fs").exists());

    for test in [&root, &slot] {
        assert!(
            !commands(test).contains("zfs destroy"),
            "{}",
            commands(test)
        );
        assert!(trashed(test).is_empty());
    }
}

#[tokio::test]
async fn a_close_refuses_a_listing_zfs_stopped_early() {
    let test = test_dir();
    set_property(&test, ROOT, "ployz:origin", &own_origin());
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/datasets-cut"), "").unwrap();
    let response = close(&test, USABLE_POOL, &["root", "seed-volume"]).await;
    assert!(
        error_text(&response).contains("stopped early"),
        "{response}"
    );
    assert!(test.0.join("seed-volume").exists());
}

#[tokio::test]
async fn a_close_refuses_while_a_held_pool_is_not_imported() {
    let test = test_dir();
    fs::create_dir(test.0.join("ployz-volumes")).unwrap();
    let response = close(&test, "", &[]).await;
    assert!(error_text(&response).contains("not imported"), "{response}");
}

fn error_text(response: &Value) -> String {
    response
        .get("Err")
        .map(ToString::to_string)
        .unwrap_or_default()
}
