//! Storage admission and recovery through the plugin routes.

use super::super::pool::POOL_BACKING_FILE;
use super::first_pool_tests::fake_first_pool;
use super::*;

#[tokio::test]
async fn device_pool_recovery_preserves_volumes_through_every_admission_route() {
    for route in [
        "/Storage.Inspect",
        "/Storage.Prepare",
        "/VolumeDriver.Create",
    ] {
        let test = TestDir::new();
        let storage = fake_first_pool(&test.0, 4096);
        let script = test.0.join("fake-storage");
        let body = fs::read_to_string(&script)
            .unwrap()
            .replace("ployz/ployz", "ployz-device/ployz")
            .replace("-r ployz'", "-r ployz-device'")
            .replace(
                "  zpool)\n",
                &format!(
                    r#"  zpool)
    case "$*" in
      import) printf '   pool: ployz-device\n'; exit 0 ;;
      'import -N ployz-device') touch '{}/device-imported'; exit 0 ;;
      'list -Hp -o name,size,allocated,free,health,readonly')
        if [ -e '{}/device-imported' ]; then
          printf 'ployz-device\t8589934592\t0\t8589934592\tONLINE\toff\n'
        fi
        exit 0 ;;
    esac
"#,
                    test.0.display(),
                    test.0.display()
                ),
            );
        fs::write(script, body).unwrap();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("volume"), "existing device data").unwrap();
        fs::write(test.0.join("volume-bound"), "1073741824").unwrap();
        let socket = test.0.join("plugin.sock");
        let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
        let request = if route == "/VolumeDriver.Create" {
            json!({"Name":"data","Opts":{"size":"1g"}})
        } else {
            json!({"data":1073741824u64})
        };
        let response = post(&socket, route, request).await;
        if route == "/VolumeDriver.Create" {
            assert_eq!(response, json!({"Err":""}));
        } else {
            assert!(response.get("Ok").is_some(), "{route}: {response}");
        }
        let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(capacity.pointer("/Ok/volumes/data").unwrap(), 1073741824u64);
        assert_eq!(
            fs::read_to_string(test.0.join("volume")).unwrap(),
            "existing device data"
        );
        assert!(!test.0.join(POOL_BACKING_FILE).exists());
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("zpool create"), "{log}");
        assert!(!log.contains("zfs create"), "{log}");
        server.abort();
    }
}

#[tokio::test]
async fn storage_admission_recovers_unimported_pool_without_recreating_data() {
    for (route, request) in [
        ("/Storage.Inspect", json!({"data":1073741824u64})),
        ("/Storage.Prepare", json!({"data":1073741824u64})),
        (
            "/Volume.Size.Set",
            json!({"name":"data","maximum_bytes":1073741824u64}),
        ),
    ] {
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
        let response = post(&socket, route, request).await;
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
async fn batch_preparation_rejects_an_existing_read_only_volume_before_dataset_creation() {
    let test = TestDir::new();
    for marker in ["root", "volume", "readonly-volume"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(
        UnixListener::bind(&socket).unwrap(),
        VolumeStorage::with_programs(zpool, zfs),
    ));

    let response = post(
        &socket,
        "/Storage.Prepare",
        json!({"data":1073741824u64,"other":1073741824u64}),
    )
    .await;

    assert!(
        response
            .pointer("/Err/cause/0")
            .and_then(Value::as_str)
            .is_some_and(|cause| cause.contains("read-only")),
        "{response}"
    );
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs create")
    );
    server.abort();
}

#[tokio::test]
async fn batch_preparation_reserves_capacity_before_docker_creates_volumes() {
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
    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await,
        json!({"Err":"Provisioned Volume data does not exist"})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.List", json!({})).await,
        json!({"Volumes":[],"Err":""})
    );
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(capacity.pointer("/Ok/volumes").unwrap(), &json!({}));

    for (name, size) in [("data", "1073741824b"), ("other", "2147483648b")] {
        assert_eq!(
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":name,"Opts":{"size":size}}),
            )
            .await,
            json!({"Err":""})
        );
    }
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

fn snapshotted(test: &TestDir, dataset: &str) {
    super::inspect_tests::set_property(test, dataset, "used", "3000");
    super::inspect_tests::set_property(test, dataset, "usedbydataset", "1000");
    super::inspect_tests::set_property(test, dataset, "referenced", "2000");
}

#[tokio::test]
async fn a_root_reports_the_bytes_it_references_not_what_its_snapshots_hold() {
    let test = TestDir::new();
    snapshotted(&test, "tank/ployz/data");
    let (socket, server) = super::inspect_tests::start(&test, USABLE_POOL, &["root", "volume"]);

    let get = post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await;
    assert_eq!(
        get.pointer("/Volume/Status/used_bytes").unwrap(),
        2000,
        "{get}"
    );
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/copies/data/used_bytes").unwrap(),
        2000,
        "{capacity}"
    );
    server.abort();
}

#[tokio::test]
async fn a_slot_reports_the_bytes_its_copy_references() {
    let test = TestDir::new();
    let (socket, server) = super::inspect_tests::start(&test, USABLE_POOL, &["root", "mirror"]);
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/copies/data/used_bytes").unwrap(),
        0,
        "an empty slot holds no copy yet: {capacity}"
    );

    fs::write(test.0.join("mirror-fs"), "").unwrap();
    snapshotted(&test, "tank/ployz-mirror/data/fs");
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/copies/data/role").unwrap(),
        "slot",
        "{capacity}"
    );
    assert_eq!(
        capacity.pointer("/Ok/copies/data/used_bytes").unwrap(),
        2000,
        "{capacity}"
    );
    server.abort();
}

#[tokio::test]
async fn a_cut_root_answers_get_with_the_bytes_its_slot_references() {
    let test = TestDir::new();
    fs::write(test.0.join("mirror-fs"), "").unwrap();
    snapshotted(&test, "tank/ployz-mirror/data/fs");
    super::inspect_tests::set_property(&test, "tank/ployz-mirror/data/fs", "ployz:cut", "on");
    let (socket, server) = super::inspect_tests::start(&test, USABLE_POOL, &["root", "mirror"]);
    let get = post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await;
    assert_eq!(
        get.pointer("/Volume/Status/used_bytes").unwrap(),
        2000,
        "{get}"
    );
    server.abort();
}

const GIB: u64 = 1024 * 1024 * 1024;
const ROOT_BOUND: u64 = 2 * GIB;
const SLOT_BOUND: u64 = GIB;

fn committed_pool(
    test: &TestDir,
    fixed: bool,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    committed_pool_with_slot(test, fixed, "copy")
}

fn committed_pool_with_slot(
    test: &TestDir,
    fixed: bool,
    slot: &str,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let storage = fake_first_pool(&test.0, 4096);
    let script = test.0.join("fake-storage");
    let mut body = fs::read_to_string(&script)
        .unwrap()
        .replace("ployz-mirror/data", &format!("ployz-mirror/{slot}"));
    if fixed {
        body = body
            .replace("ployz/ployz", "ployz-device/ployz")
            .replace("-r ployz'", "-r ployz-device'")
            .replace(
                "  zpool)\n",
                &format!(
                    r#"  zpool)
    case "$*" in
      import) printf '   pool: ployz-device\n'; exit 0 ;;
      'list -Hp -o name,size,allocated,free,health,readonly')
        printf 'ployz-device\t{}\t{}\t0\tONLINE\toff\n'
        exit 0 ;;
    esac
"#,
                    8 * GIB,
                    GIB
                ),
            );
    } else {
        fs::write(test.0.join(POOL_BACKING_FILE), "").unwrap();
        fs::write(test.0.join("pool"), "").unwrap();
        fs::write(test.0.join("allocated"), (8 * GIB).to_string()).unwrap();
    }
    fs::write(script, body).unwrap();
    for marker in ["root", "volume", "mirror-root"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    fs::write(test.0.join("volume-bound"), ROOT_BOUND.to_string()).unwrap();
    fs::write(test.0.join("slot"), SLOT_BOUND.to_string()).unwrap();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    (socket, server)
}

async fn on_committed_pool(test: &TestDir, fixed: bool, path: &str, body: Value) -> Value {
    let (socket, server) = committed_pool(test, fixed);
    let response = post(&socket, path, body).await;
    server.abort();
    response
}

async fn create_other(test: &TestDir, fixed: bool, bytes: u64) -> Value {
    on_committed_pool(
        test,
        fixed,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":format!("{bytes}b")}}),
    )
    .await
}

async fn grow_data_by(test: &TestDir, fixed: bool, bytes: u64) -> Value {
    on_committed_pool(
        test,
        fixed,
        "/Volume.Size.Set",
        json!({"name": "data", "maximum_bytes": ROOT_BOUND + bytes}),
    )
    .await
}

async fn committable(fixed: bool) -> u64 {
    let test = TestDir::new();
    let (socket, server) = committed_pool(&test, fixed);
    let inspected = post(&socket, "/Storage.Inspect", json!(null)).await;
    server.abort();
    let capacity: ployz_core::StorageCapacity =
        serde_json::from_value(inspected.get("Ok").cloned().unwrap()).unwrap();
    assert_eq!(capacity.copies.len(), 2, "a root and a slot: {inspected}");
    let committable = capacity.committable_bytes().unwrap();
    assert!(committable > GIB, "fixed={fixed}: {committable}");
    committable
}

#[tokio::test]
async fn committable_bytes_is_exactly_what_volume_creation_admits() {
    for (fixed, refusal) in [
        (true, "cannot grow automatically"),
        (false, "Not enough disk space"),
    ] {
        let committable = committable(fixed).await;

        let over = TestDir::new();
        let refused = create_other(&over, fixed, committable + 1).await;
        let message = refused.get("Err").and_then(Value::as_str).unwrap();
        assert!(message.contains(refusal), "fixed={fixed}: {refused}");
        assert!(
            !fs::read_to_string(over.0.join("commands"))
                .unwrap()
                .contains("ployz/other"),
            "fixed={fixed}: a refused Volume creates no dataset"
        );

        let edge = TestDir::new();
        assert_eq!(
            create_other(&edge, fixed, committable).await,
            json!({"Err":""}),
            "fixed={fixed}"
        );
    }
}

#[tokio::test]
async fn committable_bytes_is_exactly_what_a_grow_admits() {
    for fixed in [true, false] {
        let committable = committable(fixed).await;

        let over = TestDir::new();
        let refused = grow_data_by(&over, fixed, committable + 1).await;
        assert_eq!(
            refused
                .pointer("/Err/details/reason")
                .and_then(Value::as_str),
            Some("no_capacity"),
            "fixed={fixed}: {refused}"
        );
        assert!(
            !fs::read_to_string(over.0.join("commands"))
                .unwrap()
                .contains("zfs set refquota="),
            "fixed={fixed}: a refused grow sets no bound"
        );

        let edge = TestDir::new();
        let grown = grow_data_by(&edge, fixed, committable).await;
        assert_eq!(
            grown,
            json!({"Ok": {"bound_bytes": ROOT_BOUND + committable}}),
            "fixed={fixed}"
        );
    }
}

#[tokio::test]
async fn a_slot_a_root_of_its_name_hides_still_counts_against_committable_bytes() {
    let test = TestDir::new();
    let (socket, server) = committed_pool_with_slot(&test, true, "data");
    let inspected = post(&socket, "/Storage.Inspect", json!(null)).await;
    server.abort();
    let capacity: ployz_core::StorageCapacity =
        serde_json::from_value(inspected.get("Ok").cloned().unwrap()).unwrap();
    assert_eq!(capacity.copies.len(), 1, "{inspected}");
    assert_eq!(capacity.slot_bound_bytes, SLOT_BOUND, "{inspected}");
    let committable = capacity.committable_bytes().unwrap();

    let over = TestDir::new();
    let (socket, server) = committed_pool_with_slot(&over, true, "data");
    let refused = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":format!("{}b", committable + 1)}}),
    )
    .await;
    server.abort();
    assert!(
        refused
            .get("Err")
            .and_then(Value::as_str)
            .is_some_and(|error| !error.is_empty()),
        "{refused}"
    );

    let edge = TestDir::new();
    let (socket, server) = committed_pool_with_slot(&edge, true, "data");
    let created = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":format!("{committable}b")}}),
    )
    .await;
    server.abort();
    assert_eq!(created, json!({"Err":""}));
}
