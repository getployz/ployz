//! Storage admission and recovery through the plugin routes.

use super::super::pool::POOL_BACKING_FILE;
use super::first_pool_tests::fake_first_pool;
use super::*;

#[tokio::test]
async fn storage_admission_recovers_unimported_pool_without_recreating_data() {
    for route in ["/Storage.Inspect", "/Storage.Prepare"] {
        let test = TestDir::new();
        fs::write(test.0.join(POOL_BACKING_FILE), "existing Pool").unwrap();
        fs::write(test.0.join("allocated"), "2147483648").unwrap();
        fs::write(test.0.join("importable"), "").unwrap();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("volume"), "existing data").unwrap();
        fs::write(test.0.join("volume-bound"), "1073741824").unwrap();
        let socket = test.0.join("plugin.sock");
        let server = tokio::spawn(serve(
            UnixListener::bind(&socket).unwrap(),
            fake_first_pool(&test.0, 4096),
        ));
        let response = post(&socket, route, json!({"data":1073741824u64})).await;
        assert!(response.get("Ok").is_some(), "{route}: {response}");
        let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(capacity.pointer("/Ok/volumes/data").unwrap(), 1073741824u64);
        assert_eq!(
            fs::read_to_string(test.0.join("volume")).unwrap(),
            "existing data"
        );
        assert_eq!(
            fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
            "existing Pool"
        );
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("fallocate"));
        assert!(!log.contains("zpool create"));
        assert!(!log.contains("zfs create"));
        server.abort();
    }
}

#[tokio::test]
async fn storage_inspection_preserves_unrecoverable_backing() {
    for marker in ["unlabeled", "foreign", "destroyed", "fail-import"] {
        let test = TestDir::new();
        fs::write(test.0.join(POOL_BACKING_FILE), "preserve me").unwrap();
        fs::write(test.0.join(marker), "").unwrap();
        if marker == "fail-import" {
            fs::write(test.0.join("importable"), "").unwrap();
        }
        let socket = test.0.join("plugin.sock");
        let server = tokio::spawn(serve(
            UnixListener::bind(&socket).unwrap(),
            fake_first_pool(&test.0, 4096),
        ));
        let response = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(
            response.pointer("/Err/details/code").unwrap(),
            "storage_capacity_unknown"
        );
        assert_eq!(
            fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
            "preserve me"
        );
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("fallocate"));
        assert!(!log.contains("zpool create"));
        assert!(!log.contains("zpool destroy"));
        server.abort();
    }
}

#[tokio::test]
async fn batch_preparation_checks_total_before_allocating_and_reuses_committed_volumes() {
    let test = TestDir::new();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(
        UnixListener::bind(&socket).unwrap(),
        fake_first_pool(&test.0, 4096),
    ));
    // Each 30 GiB Volume fits individually; the combined estimate (66 GiB plus
    // one GiB for initial ZFS size loss) exceeds the 65 GiB beyond the reserve.
    let response = post(
        &socket,
        "/Storage.Prepare",
        json!({"data": 32212254720u64, "other": 32212254720u64}),
    )
    .await;
    assert_eq!(
        response.pointer("/Err/details/code").unwrap(),
        "insufficient_storage",
        "{response}"
    );
    assert_eq!(
        response.pointer("/Err/details/shortfall_bytes").unwrap(),
        2147483648u64
    );
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("volume").exists());

    let requested = json!({"data": 1073741824u64, "other": 2147483648u64});
    assert_eq!(
        post(&socket, "/Storage.Prepare", requested.clone()).await,
        json!({"Ok":["data","other"]})
    );
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/volumes").unwrap(),
        &requested,
        "{capacity}"
    );
    // Even with low host headroom, already-backed Volumes require no new allocation.
    fs::write(test.0.join("insufficient"), "").unwrap();
    assert_eq!(
        post(&socket, "/Storage.Prepare", requested).await,
        json!({"Ok":["data","other"]})
    );
    server.abort();
}
