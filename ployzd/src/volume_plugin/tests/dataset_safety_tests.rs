//! Provisioned Volume dataset safety behavior through Docker plugin routes.

use super::*;

#[tokio::test]
async fn create_rejects_read_only_roots_and_volumes() {
    for (markers, expected_dataset) in [
        (&["root", "readonly-root"][..], "tank/ployz"),
        (
            &["root", "volume", "readonly-volume"][..],
            "tank/ployz/data",
        ),
    ] {
        let test = TestDir::new();
        for marker in markers {
            fs::write(test.0.join(marker), "").unwrap();
        }
        let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
        let socket = test.0.join("plugin.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

        let response = post(
            &socket,
            "/VolumeDriver.Create",
            json!({"Name":"data","Opts":{"size":"1g"}}),
        )
        .await;

        let message = error(&response);
        assert!(message.contains(expected_dataset));
        assert!(message.contains("read-only"));
        assert!(
            !fs::read_to_string(test.0.join("commands"))
                .unwrap()
                .contains("zfs create")
        );
        server.abort();
    }
}

#[tokio::test]
async fn mount_and_path_reject_a_read_only_volume() {
    let test = TestDir::new();
    for marker in ["root", "volume", "readonly-volume"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    for route in ["/VolumeDriver.Mount", "/VolumeDriver.Path"] {
        let response = post(&socket, route, json!({"Name":"data","ID":"container"})).await;
        assert_eq!(response.get("Mountpoint").and_then(Value::as_str), Some(""));
        let message = error(&response);
        assert!(message.contains("tank/ployz/data"));
        assert!(message.contains("read-only"));
    }
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs mount")
    );
    server.abort();
}

#[tokio::test]
async fn mount_and_path_reject_a_volume_with_a_cleared_refquota() {
    let test = TestDir::new();
    for marker in ["root", "volume", "unbounded-volume"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    for route in ["/VolumeDriver.Mount", "/VolumeDriver.Path"] {
        let response = post(&socket, route, json!({"Name":"data","ID":"container"})).await;
        assert_eq!(response.get("Mountpoint").and_then(Value::as_str), Some(""));
        let message = error(&response);
        assert!(message.contains("tank/ployz/data"));
        assert!(message.contains("no Provisioned Volume bound"));
    }
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs mount")
    );
    server.abort();
}

#[tokio::test]
async fn create_rejects_a_volume_with_a_descendant_dataset() {
    let test = TestDir::new();
    for marker in ["root", "volume", "descendant"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, "tank\tONLINE\toff\n");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name":"data","Opts":{"size":"1g"}}),
    )
    .await;

    let message = error(&response);
    assert!(message.contains("tank/ployz/data/child"));
    assert!(message.contains("descendant"));
    assert!(
        !fs::read_to_string(test.0.join("commands"))
            .unwrap()
            .contains("zfs create")
    );
    server.abort();
}
