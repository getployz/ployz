//! Docker Volume removal and lookup behavior through plugin routes.

use super::copy_tests::{eventually, let_requests_act};
use super::inspect_tests::set_property;
use super::*;

const ROOT: &str = "tank/ployz/data";

#[tokio::test]
async fn docker_remove_moves_the_data_to_the_trash_and_a_create_after_makes_a_fresh_root() {
    let test = test_dir();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    let create = |origin: &str| json!({"Name":"data","Opts":{"size":"1g", ployz_core::PROVISIONED_ORIGIN_OPTION: origin}});

    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Create",
            create(super::copy_tests::BORN)
        )
        .await,
        json!({"Err":""})
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await,
        json!({"Err":""})
    );
    assert!(!test.0.join("volume").exists());
    assert!(
        !commands(&test).contains("zfs destroy"),
        "{}",
        commands(&test)
    );
    let [entry] = &trashed(&test)[..] else {
        panic!("one Trash entry: {:?}", trashed(&test));
    };
    assert_eq!(
        property(&test, entry, "ployz:trash-name").as_deref(),
        Some("data")
    );
    assert_eq!(
        property(&test, entry, "ployz:trash-place").as_deref(),
        Some("root")
    );
    assert_eq!(
        property(&test, entry, "ployz:origin").as_deref(),
        Some(super::copy_tests::BORN)
    );
    assert_eq!(property(&test, entry, "canmount").as_deref(), Some("off"));

    let other = "1".repeat(64);
    assert_eq!(
        post(&socket, "/VolumeDriver.Create", create(&other)).await,
        json!({"Err":""})
    );
    assert!(test.0.join("volume").exists());
    assert_eq!(property(&test, ROOT, "ployz:origin"), Some(other));
    assert_eq!(property(&test, ROOT, "ployz:trash-name"), None);
    assert_eq!(trashed(&test).as_slice(), std::slice::from_ref(entry));
    server.abort();
}

#[tokio::test]
async fn removing_an_unknown_volume_is_idempotent_and_keeps_siblings() {
    let test = test_dir();
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
    let test = test_dir();
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
        let test = test_dir();
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
async fn a_remove_of_a_busy_volume_is_refused_and_leaves_it_mounted_and_mountable() {
    let test = test_dir();
    for marker in ["root", "volume", "mounted", "trash"] {
        fs::create_dir_all(test.0.join("props")).unwrap();
        if marker == "trash" {
            fs::create_dir(test.0.join(marker)).unwrap();
        } else {
            fs::write(test.0.join(marker), "").unwrap();
        }
    }
    fs::write(test.0.join("props/busy-mount"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("busy"), "{response}");
    assert!(test.0.join("volume").exists());
    assert!(test.0.join("mounted").exists());
    assert_eq!(trashed(&test), Vec::<String>::new());
    assert_eq!(property(&test, ROOT, "canmount"), None);
    fs::remove_file(test.0.join("props/busy-mount")).unwrap();
    assert_eq!(
        post(
            &socket,
            "/VolumeDriver.Mount",
            json!({"Name":"data","ID":"c"})
        )
        .await,
        json!({"Err":"","Mountpoint":"/var/lib/ployz-volumes/data"})
    );
    server.abort();
}

#[tokio::test]
async fn remove_refuses_a_promoted_root_docker_has_not_registered() {
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
        (USABLE_POOL, &["root", "volume", "descendant"][..]),
        (USABLE_POOL, &["root", "volume", "unbounded-volume"][..]),
        (USABLE_POOL, &["volume"][..]),
    ] {
        let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    eventually("the List's hold", || test.0.join("list-held").exists()).await;

    let remove = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await }
    });
    let_requests_act(Duration::from_millis(300)).await;
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
    assert!(!test.0.join("volume").exists());
    assert_eq!(trashed(&test).len(), 1);
    server.abort();
}

#[tokio::test]
async fn docker_lookups_and_mounts_recover_existing_pools_without_create_or_capacity() {
    use std::os::unix::fs::PermissionsExt;

    for pool in ["ployz-device", "ployz"] {
        for endpoint in ["Get", "Mount", "Path", "List", "Remove"] {
            let test = test_dir();
            let (zpool, zfs) = fake_zfs(&test.0, "");
            let script = test.0.join("fake-zfs");
            fs::write(
                &script,
                fs::read_to_string(&script).unwrap().replace("tank", pool),
            )
            .unwrap();
            fs::remove_file(&zpool).unwrap();
            fs::write(
                &zpool,
                format!(
                    r#"#!/bin/sh
set -eu
case "$*" in
 'list -Hp -o name,size,allocated,free,health,readonly')
  [ ! -e '{root}/imported' ] || printf '{pool}\t4294967296\t0\t4294967296\tONLINE\toff\n' ;;
 import|'import -d {root}/machine-pool') printf 'pool: {pool}\n' ;;
 'import -N ployz-device'|'import -d {root}/machine-pool -f -N ployz') touch '{root}/imported' ;;
 *) exit 97 ;;
esac
"#,
                    root = test.0.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&zpool, fs::Permissions::from_mode(0o755)).unwrap();
            if pool == "ployz" {
                fs::write(test.0.join("machine-pool"), "retained file Pool").unwrap();
            }
            fs::write(test.0.join("root"), "").unwrap();
            fs::write(test.0.join("volume"), "").unwrap();
            let socket = test.0.join("plugin.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

            let reply = post(
                &socket,
                &format!("/VolumeDriver.{endpoint}"),
                json!({"Name":"data"}),
            )
            .await;
            assert_eq!(
                reply.get("Err"),
                Some(&json!("")),
                "{pool} {endpoint}: {reply}"
            );
            match endpoint {
                "Get" => assert_eq!(reply.pointer("/Volume/Name"), Some(&json!("data"))),
                "List" => assert_eq!(reply.pointer("/Volumes/0/Name"), Some(&json!("data"))),
                "Remove" => assert!(
                    fs::read_to_string(test.0.join("commands"))
                        .unwrap()
                        .contains(&format!("zfs rename {pool}/ployz/data {pool}/ployz-trash/")),
                    "{pool}: {reply}"
                ),
                _ => assert_eq!(
                    reply.get("Mountpoint"),
                    Some(&json!("/var/lib/ployz-volumes/data"))
                ),
            }
            assert!(test.0.join("imported").exists());
            if pool == "ployz-device" {
                assert_eq!(
                    fs::read_to_string(test.0.join("machine-pool.device")).unwrap(),
                    crate::machine_pool::CONFIRMED_DEVICE_POOL_MARKER
                );
                assert!(!test.0.join("machine-pool").exists());
            } else {
                assert!(!test.0.join("machine-pool.device").exists());
                assert_eq!(
                    fs::read_to_string(test.0.join("machine-pool")).unwrap(),
                    "retained file Pool"
                );
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn get_answers_absent_without_a_pool_and_hangs_up_while_one_is_not_imported() {
    let test = test_dir();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await,
        json!({"Err":"Provisioned Volume data does not exist"})
    );

    fs::create_dir(test.0.join("ployz-volumes")).unwrap();
    assert_hangs_up(&socket, "/VolumeDriver.Get", br#"{"Name":"data"}"#).await;

    fs::remove_dir(test.0.join("ployz-volumes")).unwrap();
    fs::write(test.0.join("machine-pool"), "").unwrap();
    assert_hangs_up(&socket, "/VolumeDriver.Get", br#"{"Name":"data"}"#).await;
    server.abort();
}

#[tokio::test]
async fn remove_refuses_while_a_held_pool_is_not_imported() {
    let test = test_dir();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    fs::create_dir(test.0.join("ployz-volumes")).unwrap();
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    let response = post(&socket, "/VolumeDriver.Remove", json!({"Name":"data"})).await;

    assert!(error(&response).contains("not imported"), "{response}");
    server.abort();
}

/// A restore asks every Server for its copy, and one whose Pool isn't imported may still hold
/// one, so it answers unavailable, never none.
#[tokio::test]
async fn inspect_refuses_while_a_held_pool_is_not_imported() {
    let test = test_dir();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    let inspect = || post(&socket, "/Volume.Inspect", json!({"name": "data"}));

    assert_eq!(inspect().await, json!({"Ok": {"copy": null}}));
    fs::create_dir(test.0.join("ployz-volumes")).unwrap();
    let held = inspect().await;

    assert_eq!(
        held.pointer("/Err/code"),
        Some(&json!("unavailable")),
        "{held}"
    );
    server.abort();
}

#[tokio::test]
async fn get_refuses_while_a_held_pool_is_not_imported() {
    let test = test_dir();
    let (zpool, zfs) = fake_zfs(&test.0, "");
    fs::create_dir(test.0.join("ployz-volumes")).unwrap();
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));

    assert_hangs_up(&socket, "/VolumeDriver.Get", br#"{"Name":"data"}"#).await;
    server.abort();
}
