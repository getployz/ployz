//! First-Pool creation behavior through the Docker plugin boundary.

use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    time::Duration,
};

use super::super::pool::{POOL_BACKING_FILE, PoolStorage};
use super::*;

#[tokio::test]
async fn first_create_builds_the_pool_and_bounded_volume_in_one_request() {
    let test = TestDir::new();
    let backing = test.0.join(POOL_BACKING_FILE);
    assert!(!backing.exists());

    assert_eq!(
        create_first_volume(&test, 8192, "1g").await,
        json!({"Err":""})
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 2147483648 ").count(), 1);
    assert_eq!(log.matches("zpool create ").count(), 1);
    assert!(log.contains("-o ashift=13 -O canmount=off ployz"));
    assert_eq!(
        log.matches("zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes ployz/ployz")
            .count(),
        1
    );
    assert_eq!(
        log.matches("zfs create -o refquota=1073741824 ployz/ployz/data")
            .count(),
        1
    );
    assert!(backing.exists());
}

#[tokio::test]
async fn first_create_converges_on_usable_pool_capacity() {
    let test = TestDir::new();
    fs::write(test.0.join("pool-overhead"), "268435456\n").unwrap();

    assert_eq!(
        create_first_volume(&test, 4096, "1g").await,
        json!({"Err":""})
    );

    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "3221225472\n"
    );
}

#[tokio::test]
async fn additional_create_grows_the_pool_before_committing_its_bound() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"20g"}}),
        )
        .await,
        json!({"Err":""})
    );
    server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    let grow = log
        .find(&format!(
            "fallocate -l 25769803776 {}",
            test.0.join(POOL_BACKING_FILE).display()
        ))
        .expect("the backing file grows to the committed capacity plus headroom");
    let online = log
        .find(&format!(
            "zpool online -e ployz {}",
            test.0.join(POOL_BACKING_FILE).display()
        ))
        .expect("ZFS claims the extended backing file");
    let refquota = log
        .find("zfs create -o refquota=21474836480 ployz/ployz/other")
        .expect("the additional bound is committed");
    assert!(grow < online && online < refquota, "{log}");
}

#[tokio::test]
async fn growth_retries_after_an_undersized_usable_capacity_observation() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("underclaim-growth"), "").unwrap();

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":"2g"}}),
    )
    .await;
    server.abort();

    assert_eq!(response, json!({"Err":""}));
    assert!(test.0.join("other").exists());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool online -e ployz ").count(), 2);
}

#[tokio::test]
async fn growth_converges_across_multiple_usable_capacity_increases() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("slow-growth"), "").unwrap();

    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"3g"}}),
        )
        .await,
        json!({"Err":""})
    );
    server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool online -e ployz ").count(), 3);
    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "5368709120\n"
    );
    assert!(test.0.join("other").exists());
}

#[tokio::test]
async fn growth_stops_when_usable_capacity_does_not_increase() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("no-growth"), "").unwrap();

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":"2g"}}),
    )
    .await;
    server.abort();

    assert!(error(&response).contains("made no usable-capacity progress"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool online -e ployz ").count(), 1);
    assert!(!test.0.join("other").exists());
}

#[tokio::test]
async fn growth_overflow_is_refused_before_mutation() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("pool-overhead"), "1\n").unwrap();
    let commands_before = fs::read_to_string(test.0.join("commands")).unwrap();

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        // The largest request whose existing 1 GiB commitment plus headroom fits in u64.
        json!({"Name":"other","Opts":{"size":"16769767338662214190b"}}),
    )
    .await;
    server.abort();

    assert!(error(&response).contains("backing capacity overflowed u64"));
    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "2147483648\n"
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(
        log.matches("fallocate -l ").count(),
        commands_before.matches("fallocate -l ").count()
    );
    assert_eq!(
        log.matches("zpool online -e").count(),
        commands_before.matches("zpool online -e").count()
    );
    assert!(!test.0.join("other").exists());
}

#[tokio::test]
async fn four_gib_backing_with_three_and_three_quarter_gib_usable_converges() {
    let test = TestDir::new();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(
        UnixListener::bind(&socket).unwrap(),
        fake_first_pool(&test.0, 4096),
    ));
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"2g"}}),
        )
        .await,
        json!({"Err":""})
    );
    fs::write(test.0.join("allocated"), "4294967296\n").unwrap();
    fs::write(test.0.join("claimed"), "4294967296\n").unwrap();
    fs::write(test.0.join("pool-overhead"), "268435456\n").unwrap();

    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"1g"}}),
        )
        .await,
        json!({"Err":""})
    );
    server.abort();

    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "5368709120\n"
    );
    assert!(test.0.join("other").exists());
}

#[tokio::test]
async fn growth_extends_non_aligned_backing_in_whole_gibibytes() {
    let test = TestDir::new();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(
        UnixListener::bind(&socket).unwrap(),
        fake_first_pool(&test.0, 4096),
    ));
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"11g"}}),
        )
        .await,
        json!({"Err":""})
    );
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"1g"}}),
        )
        .await,
        json!({"Err":""})
    );
    server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(
        log.contains(&format!(
            "fallocate -l 15139759718 {}",
            test.0.join(POOL_BACKING_FILE).display()
        )),
        "{log}"
    );
}

#[tokio::test]
async fn sparse_growth_is_refused_before_the_pool_or_dataset_claims_it() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("sparse"), "").unwrap();

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":"2g"}}),
    )
    .await;
    server.abort();

    assert!(error(&response).contains("is sparse"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("zpool online -e"));
    assert!(!test.0.join("other").exists());
}

#[tokio::test]
async fn growth_reserve_refusal_reports_the_shortfall_before_mutation() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("insufficient"), "").unwrap();

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"other","Opts":{"size":"2g"}}),
    )
    .await;
    server.abort();

    let message = error(&response);
    assert!(message.contains("1073741824 bytes short"), "{message}");
    assert!(
        message.contains("host-root reserve is 10737418240 bytes"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "2147483648\n"
    );
    assert_eq!(
        fs::read_to_string(test.0.join("claimed")).unwrap(),
        "2147483648\n"
    );
    assert!(!test.0.join("other").exists());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("fallocate -l 4294967296"));
    assert!(!log.contains("zpool online -e"));
}

#[tokio::test]
async fn retry_claims_an_interrupted_extension_once_and_then_is_idle() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("fail-online-once"), "").unwrap();
    let request = json!({"Name":"other","Opts":{"size":"512m"}});

    let interrupted = post(&socket, "/VolumeDriver.Create", request.clone()).await;
    assert!(error(&interrupted).contains("zpool online -e"));
    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "3221225472\n"
    );
    assert_eq!(
        fs::read_to_string(test.0.join("claimed")).unwrap(),
        "2147483648\n"
    );
    assert!(!test.0.join("other").exists());

    assert_eq!(
        post(&socket, "/VolumeDriver.Create", request.clone()).await,
        json!({"Err":""})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Create", request).await,
        json!({"Err":""})
    );
    assert_eq!(
        fs::read_to_string(test.0.join("claimed")).unwrap(),
        "3221225472\n"
    );
    server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 3221225472 ").count(), 1);
    assert_eq!(log.matches("zpool online -e ployz ").count(), 2);
    assert_eq!(
        log.matches("zfs create -o refquota=536870912 ployz/ployz/other")
            .count(),
        1
    );
}

#[tokio::test]
async fn remove_releases_a_commitment_and_usage_is_not_a_growth_input() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    let other = json!({"Name":"other","Opts":{"size":"2g"}});
    assert_eq!(
        post(&socket, "/VolumeDriver.Create", other.clone()).await,
        json!({"Err":""})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name":"other"}),).await,
        json!({"Err":""})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Create", other).await,
        json!({"Err":""})
    );
    server.abort();

    assert_eq!(
        fs::read_to_string(test.0.join("allocated")).unwrap(),
        "4294967296\n"
    );
    assert_eq!(
        fs::read_to_string(test.0.join("claimed")).unwrap(),
        "4294967296\n"
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("referenced"));
    assert!(!log.contains("available"));
    assert_eq!(log.matches("fallocate -l 4294967296 ").count(), 1);
    assert_eq!(log.matches("zpool online -e ployz ").count(), 1);
    assert_eq!(
        log.matches("zfs create -o refquota=2147483648 ployz/ployz/other")
            .count(),
        2
    );
}

async fn start_pool_with_data(
    test: &TestDir,
) -> (PathBuf, tokio::task::JoinHandle<std::io::Result<()>>) {
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(
        UnixListener::bind(&socket).unwrap(),
        fake_first_pool(&test.0, 4096),
    ));
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await,
        json!({"Err":""})
    );
    (socket, server)
}

#[tokio::test]
async fn concurrent_and_retried_first_creates_converge_on_one_pool() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "interrupted").unwrap();
    fs::write(test.0.join("concurrent"), "").unwrap();
    let first = fake_first_pool(&test.0, 4096);
    let second = VolumeStorage {
        pool: first.pool.clone(),
        zfs: first.zfs.clone(),
        mutation: Arc::new(Mutex::new(())),
    };
    let first_socket = test.0.join("first-plugin.sock");
    let second_socket = test.0.join("second-plugin.sock");
    let first_server = tokio::spawn(serve(UnixListener::bind(&first_socket).unwrap(), first));
    let second_server = tokio::spawn(serve(UnixListener::bind(&second_socket).unwrap(), second));

    let (data, other) = tokio::join!(
        post(
            &first_socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        ),
        post(
            &second_socket,
            "/VolumeDriver.Create",
            json!({"Name":"other","Opts":{"size":"1g"}}),
        ),
    );
    assert_eq!(data, json!({"Err":""}));
    assert_eq!(other, json!({"Err":""}));
    assert_eq!(
        post(
            &first_socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await,
        json!({"Err":""})
    );
    first_server.abort();
    second_server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 2147483648 ").count(), 1);
    assert_eq!(log.matches("fallocate -l 3221225472 ").count(), 1);
    assert_eq!(log.matches("zpool create ").count(), 1);
    assert_eq!(log.matches("zpool online -e ployz ").count(), 1);
    assert_eq!(
        log.matches("zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes ployz/ployz")
            .count(),
        1
    );
    assert_eq!(
        log.matches("zfs create -o refquota=1073741824 ployz/ployz/data")
            .count(),
        1
    );
    assert_eq!(
        log.matches("zfs create -o refquota=1073741824 ployz/ployz/other")
            .count(),
        1
    );
    assert!(test.0.join(POOL_BACKING_FILE).exists());
    assert!(test.0.join("pool").exists());
}

#[tokio::test]
async fn a_second_process_cannot_use_a_pool_before_its_owner_finishes() {
    let test = TestDir::new();
    fs::write(test.0.join("pause-failed-volume"), "").unwrap();
    let first = fake_first_pool(&test.0, 4096);
    let second = VolumeStorage {
        pool: first.pool.clone(),
        zfs: first.zfs.clone(),
        mutation: Arc::new(Mutex::new(())),
    };
    let first_socket = test.0.join("first-plugin.sock");
    let second_socket = test.0.join("second-plugin.sock");
    let first_server = tokio::spawn(serve(UnixListener::bind(&first_socket).unwrap(), first));
    let second_server = tokio::spawn(serve(UnixListener::bind(&second_socket).unwrap(), second));

    let failed = tokio::spawn({
        let socket = first_socket.clone();
        async move {
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"data","Opts":{"size":"1g"}}),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !test.0.join("pool-visible").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let succeeds = tokio::spawn({
        let socket = second_socket.clone();
        async move {
            post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name":"other","Opts":{"size":"1g"}}),
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!test.0.join("other").exists());
    fs::write(test.0.join("continue-failure"), "").unwrap();

    assert!(!error(&failed.await.unwrap()).is_empty());
    assert_eq!(succeeds.await.unwrap(), json!({"Err":""}));
    first_server.abort();
    second_server.abort();

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool create ").count(), 2);
    assert_eq!(log.matches("zpool destroy -f ployz").count(), 1);
    assert!(test.0.join("pool").exists());
    assert!(test.0.join("other").exists());
}

#[tokio::test]
async fn valid_unimported_backing_pool_is_imported_and_reused() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "existing Pool").unwrap();
    fs::write(test.0.join("allocated"), "2147483648").unwrap();
    fs::write(test.0.join("importable"), "").unwrap();

    assert_eq!(
        create_first_volume(&test, 4096, "1g").await,
        json!({"Err":""})
    );

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(log.contains(&format!(
        "zpool import -d {} -f -N ployz",
        test.0.join(POOL_BACKING_FILE).display()
    )));
    assert!(!log.contains("fallocate"));
    assert!(!log.contains("zpool create"));
    assert_eq!(
        fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
        "existing Pool"
    );
}

#[tokio::test]
async fn unlabeled_stale_backing_is_replaced() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "interrupted").unwrap();

    assert_eq!(
        create_first_volume(&test, 4096, "1g").await,
        json!({"Err":""})
    );

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool import -d ").count(), 1);
    assert_eq!(log.matches("fallocate -l 2147483648 ").count(), 1);
    assert_eq!(log.matches("zpool create ").count(), 1);
}

#[tokio::test]
async fn failed_import_preserves_the_backing_file() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "existing Pool").unwrap();
    fs::write(test.0.join("importable"), "").unwrap();
    fs::write(test.0.join("fail-import"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(error(&response).contains("zpool import"));
    assert_eq!(
        fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
        "existing Pool"
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("fallocate"));
    assert!(!log.contains("zpool create"));
    assert!(!log.contains("zpool destroy"));
}

#[tokio::test]
async fn ambiguous_or_destroyed_pool_labels_are_preserved() {
    for marker in ["foreign", "destroyed"] {
        let test = TestDir::new();
        fs::write(test.0.join(POOL_BACKING_FILE), "labeled Pool").unwrap();
        fs::write(test.0.join(marker), "").unwrap();

        let response = create_first_volume(&test, 4096, "1g").await;

        assert!(!error(&response).is_empty());
        assert_eq!(
            fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
            "labeled Pool"
        );
        let log = fs::read_to_string(test.0.join("commands")).unwrap();
        assert!(!log.contains("fallocate"));
        assert!(!log.contains("zpool create"));
        assert!(!log.contains("zpool destroy"));
    }
}

#[tokio::test]
async fn retry_after_pool_creation_completes_the_interrupted_volume() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "existing Pool").unwrap();
    fs::write(test.0.join("allocated"), "2147483648").unwrap();
    fs::write(test.0.join("pool"), "").unwrap();

    assert_eq!(
        create_first_volume(&test, 4096, "1g").await,
        json!({"Err":""})
    );

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("fallocate"));
    assert!(!log.contains("zpool create"));
    assert_eq!(
        log.matches("zfs create -o refquota=1073741824 ployz/ployz/data")
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(test.0.join(POOL_BACKING_FILE)).unwrap(),
        "existing Pool"
    );
}

#[tokio::test]
async fn empty_existing_pool_grows_for_a_larger_first_bound() {
    let test = TestDir::new();
    fs::write(test.0.join(POOL_BACKING_FILE), "existing Pool").unwrap();
    fs::write(test.0.join("allocated"), "2147483648").unwrap();
    fs::write(test.0.join("pool"), "").unwrap();

    assert_eq!(
        create_first_volume(&test, 4096, "20g").await,
        json!({"Err":""})
    );

    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 23622320128 ").count(), 1);
    assert_eq!(log.matches("zpool online -e ployz ").count(), 1);
}

#[tokio::test]
async fn initial_capacity_uses_ten_percent_when_it_exceeds_one_gibibyte() {
    let test = TestDir::new();

    assert_eq!(
        create_first_volume(&test, 512, "11g").await,
        json!({"Err":""})
    );
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("fallocate -l 12992276070 ").count(), 1);
    assert_eq!(log.matches("fallocate -l ").count(), 1);
    assert!(!log.contains("zpool online -e"));
    assert!(log.contains("-o ashift=12 -O canmount=off ployz"));
}

#[tokio::test]
async fn first_create_refuses_before_mutation_when_root_reserve_would_be_broken() {
    let test = TestDir::new();
    fs::write(test.0.join("insufficient"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(error(&response).contains("host-root reserve"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("fallocate"));
    assert!(!log.contains("zpool create"));
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

#[tokio::test]
async fn first_create_rejects_sparse_allocation_without_creating_a_pool() {
    let test = TestDir::new();
    fs::write(test.0.join("sparse"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(error(&response).contains("is sparse"));
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert!(!log.contains("zpool create"));
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
}

#[tokio::test]
async fn failed_pool_creation_removes_the_partial_pool_and_backing_file() {
    let test = TestDir::new();
    fs::write(test.0.join("fail-pool"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(!error(&response).is_empty());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool destroy -f ployz").count(), 1);
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

#[tokio::test]
async fn failed_first_volume_removes_the_new_pool_and_backing_file() {
    let test = TestDir::new();
    fs::write(test.0.join("fail-volume"), "").unwrap();

    let response = create_first_volume(&test, 4096, "1g").await;

    assert!(!error(&response).is_empty());
    let log = fs::read_to_string(test.0.join("commands")).unwrap();
    assert_eq!(log.matches("zpool destroy -f ployz").count(), 1);
    assert!(!test.0.join(POOL_BACKING_FILE).exists());
    assert!(!test.0.join("pool").exists());
}

async fn create_first_volume(test: &TestDir, physical_block_size: u64, size: &str) -> Value {
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(
        listener,
        fake_first_pool(&test.0, physical_block_size),
    ));
    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"data","Opts":{"size":size}}),
    )
    .await;
    server.abort();
    response
}

fn fake_first_pool(directory: &Path, physical_block_size: u64) -> VolumeStorage {
    let script = directory.join("fake-storage");
    let commands = directory.join("commands");
    let pool = directory.join("pool");
    let root = directory.join("root");
    let volume = directory.join("volume");
    let volume_bound = directory.join("volume-bound");
    let allocated = directory.join("allocated");
    let claimed = directory.join("claimed");
    let insufficient = directory.join("insufficient");
    let sparse = directory.join("sparse");
    let fail_pool = directory.join("fail-pool");
    let fail_online_once = directory.join("fail-online-once");
    let underclaim_growth = directory.join("underclaim-growth");
    let slow_growth = directory.join("slow-growth");
    let no_growth = directory.join("no-growth");
    let pool_overhead = directory.join("pool-overhead");
    let fail_volume = directory.join("fail-volume");
    let importable = directory.join("importable");
    let fail_import = directory.join("fail-import");
    let pause_failed_volume = directory.join("pause-failed-volume");
    let pool_visible = directory.join("pool-visible");
    let continue_failure = directory.join("continue-failure");
    let foreign = directory.join("foreign");
    let destroyed = directory.join("destroyed");
    let concurrent = directory.join("concurrent");
    let other = directory.join("other");
    let other_bound = directory.join("other-bound");
    let script_body = format!(
        r#"#!/bin/sh
set -eu
name=${{0##*/}}
printf '%s %s\n' "$name" "$*" >> '{commands}'
case "$name" in
  df)
    if [ -e '{insufficient}' ]; then
      printf 'Size Avail\n42949672960 11811160064\n'
    else
      printf 'Size Avail\n107374182400 96636764160\n'
    fi
    ;;
  fallocate) touch "$3"; printf '%s\n' "$2" > '{allocated}' ;;
  stat)
    bytes=$(cat '{allocated}')
    if [ -e '{sparse}' ]; then
      printf '%s 0 512\n' "$bytes"
    else
      printf '%s %s 512\n' "$bytes" "$(((bytes + 511) / 512))"
    fi
    ;;
  zpool)
    case "$*" in
      'import -d {backing}')
        if [ -e '{foreign}' ]; then printf '   pool: foreign\n'
        elif [ -e '{importable}' ]; then printf '   pool: ployz\n'
        fi
        ;;
      'import -D -d {backing}') [ ! -e '{destroyed}' ] || printf '   pool: ployz\n' ;;
      'import -d {backing} -f -N ployz')
        [ ! -e '{fail_import}' ] || {{ echo 'import failed' >&2; exit 2; }}
        touch '{pool}'
        ;;
      'list -Hp -o name,size,allocated,free,health,readonly')
        if [ -e '{concurrent}' ] && [ ! -e '{pool}' ]; then
          sleep 0.1
        else
          if [ -e '{pool}' ]; then
            capacity='{allocated}'
            [ ! -e '{claimed}' ] || capacity='{claimed}'
            capacity=$(cat "$capacity")
            [ ! -e '{pool_overhead}' ] || capacity=$((capacity - $(cat '{pool_overhead}')))
            printf 'ployz\t%s\t0\t%s\tONLINE\toff\n' "$capacity" "$capacity"
          fi
        fi
        ;;
      'list -H -o name') [ ! -e '{pool}' ] || printf 'ployz\n' ;;
      create*) touch '{pool}'; cp '{allocated}' '{claimed}'; [ ! -e '{fail_pool}' ] || exit 2 ;;
      'online -e ployz {backing}')
        if [ -e '{fail_online_once}' ]; then
          rm '{fail_online_once}'
          echo 'online interrupted' >&2
          exit 2
        fi
        if [ -e '{no_growth}' ]; then
          :
        elif [ -e '{slow_growth}' ]; then
          current=$(cat '{claimed}')
          limit=$(cat '{allocated}')
          next=$((current + 1073741824))
          [ "$next" -le "$limit" ] || next=$limit
          printf '%s\n' "$next" > '{claimed}'
        elif [ -e '{underclaim_growth}' ]; then
          rm '{underclaim_growth}'
          printf '3221225472\n' > '{claimed}'
        else
          cp '{allocated}' '{claimed}'
        fi
        ;;
      'destroy -f ployz') rm -f '{pool}' '{root}' '{volume}' '{volume_bound}' '{other}' '{other_bound}' '{claimed}' ;;
      *) echo "unexpected fake zpool command: $*" >&2; exit 2 ;;
    esac
    ;;
  zfs)
    case "$*" in
      'list -Hp -o name,refquota,used,mountpoint,mounted,readonly -r ployz')
        printf 'ployz\t0\t0\t/ployz\tyes\toff\n'
        [ ! -e '{root}' ] || printf 'ployz/ployz\t0\t0\t/var/lib/ployz-volumes\tno\toff\n'
        [ ! -e '{volume}' ] || printf 'ployz/ployz/data\t%s\t0\t/var/lib/ployz-volumes/data\tno\toff\n' "$(cat '{volume_bound}')"
        [ ! -e '{other}' ] || printf 'ployz/ployz/other\t%s\t0\t/var/lib/ployz-volumes/other\tno\toff\n' "$(cat '{other_bound}')"
        ;;
      'create -o canmount=off -o mountpoint=/var/lib/ployz-volumes ployz/ployz') touch '{root}' ;;
      'create -o refquota='*' ployz/ployz/data')
        if [ -e '{pause_failed_volume}' ]; then
          touch '{pool_visible}'
          while [ ! -e '{continue_failure}' ]; do sleep 0.01; done
          exit 2
        fi
        [ ! -e '{fail_volume}' ] || exit 2
        printf '%s\n' "${{3#refquota=}}" > '{volume_bound}'
        touch '{volume}'
        ;;
      'create -o refquota='*' ployz/ployz/other')
        printf '%s\n' "${{3#refquota=}}" > '{other_bound}'
        touch '{other}'
        ;;
      'destroy ployz/ployz/other') rm '{other}' '{other_bound}' ;;
      *) echo "unexpected fake zfs command: $*" >&2; exit 2 ;;
    esac
    ;;
  *) echo "unexpected fake storage program: $name" >&2; exit 2 ;;
esac
"#,
        commands = commands.display(),
        allocated = allocated.display(),
        claimed = claimed.display(),
        pool = pool.display(),
        root = root.display(),
        volume = volume.display(),
        volume_bound = volume_bound.display(),
        insufficient = insufficient.display(),
        sparse = sparse.display(),
        fail_pool = fail_pool.display(),
        fail_online_once = fail_online_once.display(),
        underclaim_growth = underclaim_growth.display(),
        slow_growth = slow_growth.display(),
        no_growth = no_growth.display(),
        pool_overhead = pool_overhead.display(),
        fail_volume = fail_volume.display(),
        importable = importable.display(),
        fail_import = fail_import.display(),
        pause_failed_volume = pause_failed_volume.display(),
        pool_visible = pool_visible.display(),
        continue_failure = continue_failure.display(),
        foreign = foreign.display(),
        destroyed = destroyed.display(),
        concurrent = concurrent.display(),
        other = other.display(),
        other_bound = other_bound.display(),
        backing = directory.join(POOL_BACKING_FILE).display(),
    );
    fs::write(&script, script_body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let program = |name: &str| {
        let path = directory.join(name);
        std::os::unix::fs::symlink(&script, &path).unwrap();
        path
    };
    let host = fs::metadata(directory).unwrap();
    let sys_dev_block = directory.join("sys-dev-block");
    let device = format!(
        "{}:{}",
        nix::sys::stat::major(host.dev()),
        nix::sys::stat::minor(host.dev())
    );
    let disk = directory.join("sys-devices/vda");
    fs::create_dir_all(disk.join("vda2")).unwrap();
    fs::create_dir(&sys_dev_block).unwrap();
    std::os::unix::fs::symlink(disk.join("vda2"), sys_dev_block.join(device)).unwrap();
    let queue = disk.join("queue");
    fs::create_dir_all(&queue).unwrap();
    fs::write(
        queue.join("physical_block_size"),
        format!("{physical_block_size}\n"),
    )
    .unwrap();
    VolumeStorage {
        pool: PoolStorage::with_environment(
            program("zpool"),
            program("fallocate"),
            program("stat"),
            program("df"),
            directory.join(POOL_BACKING_FILE),
            directory.to_owned(),
            sys_dev_block,
        ),
        zfs: program("zfs"),
        mutation: Arc::new(Mutex::new(())),
    }
}
