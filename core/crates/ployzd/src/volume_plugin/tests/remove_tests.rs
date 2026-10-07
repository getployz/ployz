//! Docker Volume removal and lookup behavior through plugin routes.

use super::lease_tests::set_property;
use super::*;

#[tokio::test]
async fn docker_can_remove_a_provisioned_volume() {
    let test = TestDir::new();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await,
        json!({"Err":""})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await,
        json!({"Err":""})
    );
    assert!(
        fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy -r tank/ployz/data")
    );
    assert!(!test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn removing_an_unknown_volume_is_idempotent_and_keeps_siblings() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("sibling"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    for _ in 0..2 {
        assert_eq!(
            post(&socket, "/VolumeDriver.Remove", json!({"Name":"missing"})).await,
            json!({"Err":""})
        );
    }
    assert!(
        !error(
            &post(
                &socket,
                "/VolumeDriver.Remove",
                json!({"Name":"../sibling"})
            )
            .await
        )
        .is_empty()
    );
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy")
    );
    server.abort();
}

#[tokio::test]
async fn removing_an_unknown_volume_without_a_pool_is_idempotent() {
    let test = TestDir::new();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name":"missing"})).await,
        json!({"Err":""})
    );
    server.abort();
}

#[tokio::test]
async fn removing_an_unknown_volume_ignores_an_unusable_managed_root() {
    for root_state in ["readonly-root", "incompatible-root"] {
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join(root_state), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert_eq!(
            post(&socket, "/VolumeDriver.Remove", json!({"Name":"missing"})).await,
            json!({"Err":""})
        );
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs destroy")
        );
        server.abort();
    }
}

#[tokio::test]
async fn docker_receives_dataset_destruction_failures() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    fs::write(test.0.join("destroy-fails"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("dataset is busy"));
    assert!(test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn remove_refuses_a_promoted_root_docker_has_not_registered() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    set_property(&test, "tank/ployz/data", "ployz:promote", "1");
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("not registered"), "{response}");
    assert!(test.0.join("volume").exists());
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy")
    );
    server.abort();
}

#[tokio::test]
async fn remove_never_destroys_an_unbounded_dataset() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    fs::write(test.0.join("unbounded-volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(!error(&response).is_empty());
    assert!(test.0.join("volume").exists());
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy")
    );
    server.abort();
}

#[tokio::test]
async fn remove_never_destroys_a_child_below_an_unmanaged_root() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("incompatible-root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("tank/ployz"));
    assert!(test.0.join("volume").exists());
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy")
    );
    server.abort();
}

#[tokio::test]
async fn get_returns_volume_identity_mountpoint_bound_and_usage() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await,
        json!({
            "Volume":{
                "Name":"data",
                "Mountpoint":"/var/lib/ployz-volumes/data",
                "Status":{"bound_bytes":1073741824,"used_bytes":966367642}
            },
            "Err":""
        })
    );
    server.abort();
}

#[tokio::test]
async fn get_answers_only_names_it_is_sure_are_not_provisioned_volumes() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"missing"})).await,
        json!({"Err":"Provisioned Volume missing does not exist"})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"../data"})).await,
        json!({"Err":"invalid Docker Volume name \"../data\""})
    );
    server.abort();
}

#[tokio::test]
async fn get_hangs_up_while_zfs_cannot_list_datasets() {
    let test = TestDir::new();
    for marker in ["root", "volume", "list-fails"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_hangs_up(&socket, "/VolumeDriver.Get", br#"{"Name":"data"}"#).await;

    fs::remove_file(test.0.join("list-fails")).unwrap();
    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"data"}))
            .await
            .pointer("/Volume/Name"),
        Some(&json!("data"))
    );
    server.abort();
}

#[tokio::test]
async fn get_hangs_up_on_an_empty_or_unparseable_body() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    for body in [&b""[..], b"{", br#"{"Name":7}"#] {
        assert_hangs_up(&socket, "/VolumeDriver.Get", body).await;
    }
    server.abort();
}

#[tokio::test]
async fn get_hangs_up_when_pool_or_dataset_state_is_unsure() {
    for (pools, markers) in [
        ("", &["root", "volume"][..]),
        (USABLE_POOL, &["root", "volume", "descendant"][..]),
        (USABLE_POOL, &["root", "volume", "unbounded-volume"][..]),
        (USABLE_POOL, &["volume"][..]),
    ] {
        let test = TestDir::new();
        for marker in markers {
            fs::write(test.0.join(marker), "").unwrap();
        }
        let (zpool, zfs) = fake_zfs(&test.0, pools);
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        assert_hangs_up(&socket, "/VolumeDriver.Get", br#"{"Name":"data"}"#).await;
        server.abort();
    }
}

#[tokio::test]
async fn busy_executable_retries_are_bounded_and_do_not_repeat_commands() {
    let test = TestDir::new();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    let writer = fs::OpenOptions::new().write(true).open(&zpool).unwrap();
    let mut command = std::pin::pin!(checked_command(&zpool, &["list"]));
    let first = futures_util::poll!(command.as_mut());
    assert!(
        first.is_pending(),
        "busy executable should wait for retry: {first:?}"
    );
    drop(writer);
    assert_eq!(command.await.unwrap(), "");
    assert_eq!(
        fs::read_to_string(test.0.join("commands")).unwrap(),
        "zpool list\n"
    );

    let writer = fs::OpenOptions::new().write(true).open(&zpool).unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        checked_command(&zpool, &["list"]),
    )
    .await
    .expect("busy retries must be bounded")
    .unwrap_err();
    assert!(error.to_string().contains("Text file busy"));
    assert_eq!(
        fs::read_to_string(test.0.join("commands")).unwrap(),
        "zpool list\n"
    );
    drop(writer);
    let error = checked_command(&zfs, &["invalid"]).await.unwrap_err();
    assert!(
        error.to_string().contains("unexpected fake zfs command"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(test.0.join("commands")).unwrap(),
        "zpool list\nzfs invalid\n"
    );
}

#[tokio::test]
async fn remove_waits_for_a_list_holding_the_storage_mutation() {
    let test = TestDir::new();
    for marker in ["root", "volume", "hold-list"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    let list = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/VolumeDriver.List", json!({})).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.0.join("list-held").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let remove = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !remove.is_finished(),
        "Remove answered while List held the mutation: {}",
        remove.await.unwrap()
    );
    fs::remove_file(test.0.join("hold-list")).unwrap();

    assert_eq!(error(&list.await.unwrap()), "");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), remove)
            .await
            .unwrap()
            .unwrap(),
        json!({"Err":""})
    );
    assert!(
        fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs destroy -r tank/ployz/data")
    );
    server.abort();
}
