//! Docker Volume removal and lookup behavior through plugin routes.

use super::*;

#[tokio::test]
async fn docker_can_remove_a_provisioned_volume() {
    let test = TestDir::new();
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
            .contains("zfs destroy tank/ployz/data")
    );
    assert!(!test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn removing_an_unknown_volume_is_idempotent_and_keeps_siblings() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("sibling"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("dataset is busy"));
    assert!(test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn remove_never_destroys_an_unbounded_dataset() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    fs::write(test.0.join("unbounded-volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
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
async fn get_returns_the_minimal_exact_volume_identity() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await,
        json!({
            "Volume":{"Name":"data","Mountpoint":"/var/lib/ployz-volumes/data"},
            "Err":""
        })
    );
    assert!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"missing"}))
            .await
            .get("Volume")
            .is_none()
    );
    server.abort();
}

#[tokio::test]
async fn get_rejects_a_volume_with_a_descendant_dataset() {
    let test = TestDir::new();
    for marker in ["root", "volume", "descendant"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await;

    assert!(response.get("Volume").is_none());
    let message = error(&response);
    assert!(message.contains("tank/ployz/data/child"));
    assert!(message.contains("descendant"));
    server.abort();
}
