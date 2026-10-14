//! Departure over fake ZFS: every root becomes a slot, and the roles Storage.Inspect and
//! Mount derive from the markers.

use super::inspect_tests::{set_property, start};
use super::*;

#[tokio::test]
async fn departure_demotes_each_root_to_a_slot() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@dep-100\t900\t1690000000\ntank/ployz/data@w-3-1\t901\t1690000001",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mounted"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": ["data"], "stuck": []}}));

    let log = commands(&test);
    let order = [
        "zfs unmount tank/ployz/data\n",
        "zfs destroy tank/ployz/data@dep-100\n",
        "zfs snapshot tank/ployz/data@dep-",
        "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror\n",
        "zfs create -o canmount=off -o readonly=on -o refquota=1073741824 tank/ployz-mirror/data\n",
        "zfs rename tank/ployz/data tank/ployz-mirror/data/fs\n",
        "zfs set readonly=on tank/ployz-mirror/data/fs\n",
    ];
    let mut cursor = 0;
    for step in order {
        let at = log[cursor..]
            .find(step)
            .unwrap_or_else(|| panic!("{step:?} missing after offset {cursor} in {log}"));
        cursor += at + step.len();
    }
    assert!(!log.contains("zfs destroy tank/ployz/data@w-3-1"), "{log}");
    let kept = snapshot_names(&test, "tank/ployz-mirror/data/fs");
    assert!(kept.iter().any(|name| name.starts_with("dep-")), "{kept:?}");
    assert!(kept.contains(&"w-3-1".to_owned()), "{kept:?}");

    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(capacity.pointer("/Ok/volumes").unwrap(), &json!({}));
    assert_eq!(
        capacity.pointer("/Ok/copies/data/role").unwrap(),
        &json!("slot")
    );
    let listed = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(listed.pointer("/Volumes").unwrap(), &json!([]));
    server.abort();
}

#[tokio::test]
async fn a_departed_root_without_run_snapshots_restores_from_its_departure_snapshot() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mounted"]);
    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": ["data"], "stuck": []}}));

    let copy = post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await;
    assert_eq!(copy.pointer("/Ok/kind"), Some(&json!("slot")), "{copy}");
    let newest = copy.pointer("/Ok/newest").cloned().unwrap_or_default();
    assert!(
        newest
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.starts_with("dep-")),
        "{copy}"
    );

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": newest.get("guid"), "epoch": 1}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    assert_eq!(
        reply.pointer("/Ok/readonly"),
        Some(&json!(false)),
        "{reply}"
    );
    assert_eq!(reply.pointer("/Ok/newest"), Some(&newest), "{reply}");
    server.abort();
}

#[tokio::test]
async fn departure_without_a_pool_demotes_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": [], "stuck": []}}));
    server.abort();
}

#[tokio::test]
async fn departure_on_a_machine_without_zfs_demotes_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    fs::remove_file(test.0.join("zpool")).unwrap();
    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": [], "stuck": []}}));
    let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
    assert!(capacity.get("Ok").is_some(), "{capacity}");
    server.abort();
}

#[tokio::test]
async fn departure_destroys_a_received_mirror_and_the_emptied_mirror_root() {
    for markers in [
        &["root", "mirror-root", "mirror", "mirror-fs"][..],
        &["root", "mirror-root", "mirror"],
    ] {
        let test = TestDir::new();
        set_property(
            &test,
            "tank/ployz-mirror/data/fs",
            "snapshots",
            "tank/ployz-mirror/data/fs@w-3-1\t901\t1690000001",
        );
        let (socket, server) = start(&test, USABLE_POOL, markers);

        let response = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            response,
            json!({"Ok": {"demoted": ["data"], "stuck": []}}),
            "{markers:?}"
        );
        let log = commands(&test);
        assert!(
            log.contains("zfs destroy -r tank/ployz-mirror/data\n"),
            "{markers:?}: {log}"
        );
        assert!(!test.0.join("mirror").exists(), "{markers:?}");
        assert!(
            log.contains("zfs destroy tank/ployz-mirror\n"),
            "{markers:?}: {log}"
        );
        assert!(!test.0.join("mirror-root").exists(), "{markers:?}");
        let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(
            capacity.pointer("/Ok/copies").unwrap(),
            &json!({}),
            "{markers:?}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn departure_keeps_a_mirror_root_a_slot_appeared_in() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror-root", "mirror"]);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/mirror-child-appears"), "").unwrap();

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert!(test.0.join("mirror-root").exists());
    server.abort();
}

/// The empty mirror root holds no data, so failing to destroy it fails nothing: ZFS words
/// "has children" in the Machine's locale, and a busy or vanished root errs otherwise.
#[tokio::test]
async fn departure_answers_what_it_demoted_when_the_empty_mirror_root_stays() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror-root", "mirror"]);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/fail-command"),
        "destroy tank/ployz-mirror",
    )
    .unwrap();

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert!(
        !test.0.join("props/fail-command").exists(),
        "the root destroy never ran"
    );
    assert!(!test.0.join("mirror").exists());
    assert!(test.0.join("mirror-root").exists());
    server.abort();
}

#[tokio::test]
async fn departure_keeps_a_copy_this_machine_wrote() {
    for (property, value) in [
        (
            "snapshots",
            "tank/ployz-mirror/data/fs@dep-100\t900\t1690000000",
        ),
        ("ployz:cut", "on"),
        ("ployz:epoch", "3"),
    ] {
        let test = TestDir::new();
        set_property(&test, "tank/ployz-mirror/data/fs", property, value);
        let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

        let response = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            response,
            json!({"Ok": {"demoted": [], "stuck": []}}),
            "{property}"
        );
        let log = commands(&test);
        assert!(!log.contains("zfs destroy"), "{property}: {log}");
        assert!(test.0.join("mirror-fs").exists(), "{property}");
        server.abort();
    }
}

async fn assert_departed_into_a_slot(test: &TestDir, socket: &Path) {
    assert!(!test.0.join("volume").exists(), "the root is still there");
    assert!(test.0.join("mirror-fs").exists(), "the slot holds no fs");
    assert_eq!(
        property(test, "tank/ployz-mirror/data/fs", "readonly").as_deref(),
        Some("on")
    );
    let kept = snapshot_names(test, "tank/ployz-mirror/data/fs");
    assert!(kept.iter().any(|name| name.starts_with("dep-")), "{kept:?}");
    let capacity = post(socket, "/Storage.Inspect", json!(null)).await;
    assert_eq!(
        capacity.pointer("/Ok/copies/data/role").unwrap(),
        &json!("slot"),
        "{capacity}"
    );
}

fn error_message(response: &Value) -> &str {
    response
        .pointer("/Err/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn departure_moves_a_root_into_the_empty_slot_beside_it() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mirror"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(response, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert_departed_into_a_slot(&test, &socket).await;
    let log = commands(&test);
    assert!(
        !log.contains("zfs create -o canmount=off -o readonly=on"),
        "{log}"
    );
    server.abort();
}

#[tokio::test]
async fn departure_refuses_a_root_beside_a_slot_that_holds_a_copy() {
    let test = TestDir::new();
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "mirror", "mirror-fs"],
    );

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        error_message(&response).contains("both a writer and a mirror"),
        "{response}"
    );
    let log = commands(&test);
    assert!(!log.contains("zfs rename"), "{log}");
    assert!(!log.contains("zfs snapshot"), "{log}");
    assert!(test.0.join("volume").exists());
    server.abort();
}

#[tokio::test]
async fn departure_interrupted_before_the_rename_finishes_on_retry() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "rename-fails"]);

    let interrupted = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        error_message(&interrupted).contains("rename interrupted"),
        "{interrupted}"
    );
    assert!(
        test.0.join("mirror").exists(),
        "the slot parent was not made"
    );
    fs::remove_file(test.0.join("rename-fails")).unwrap();

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(retried, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert_departed_into_a_slot(&test, &socket).await;
    server.abort();
}

#[tokio::test]
async fn departure_interrupted_after_the_rename_seals_the_slot_on_retry() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    fs::create_dir_all(test.0.join("props")).unwrap();
    let toggle = test.0.join("props").join("fail-properties");
    fs::write(&toggle, "").unwrap();

    let interrupted = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        error_message(&interrupted).contains("property unavailable"),
        "{interrupted}"
    );
    assert!(test.0.join("mirror-fs").exists(), "no rename");
    fs::remove_file(&toggle).unwrap();

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_departed_into_a_slot(&test, &socket).await;
    assert_eq!(retried, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    server.abort();
}

#[tokio::test]
async fn storage_inspection_derives_each_copy_role() {
    struct Case {
        markers: &'static [&'static str],
        role: &'static str,
        listed: bool,
    }
    let cases = [
        Case {
            markers: &["root", "volume"],
            role: "writer",
            listed: true,
        },
        Case {
            markers: &["root", "volume", "readonly-volume"],
            role: "switching",
            listed: true,
        },
        Case {
            markers: &["root", "mirror", "mirror-fs"],
            role: "slot",
            listed: false,
        },
    ];
    for case in cases {
        let test = TestDir::new();
        let (socket, server) = start(&test, USABLE_POOL, case.markers);
        let capacity = post(&socket, "/Storage.Inspect", json!(null)).await;
        assert_eq!(
            capacity.pointer("/Ok/copies/data/role").unwrap(),
            &json!(case.role),
            "{:?}: {capacity}",
            case.markers
        );
        assert_eq!(
            capacity.pointer("/Ok/volumes/data").is_some(),
            case.listed,
            "{:?}: {capacity}",
            case.markers
        );
        server.abort();
    }
}

#[tokio::test]
async fn mount_admits_a_writable_root_and_refuses_an_absent_or_readonly_one() {
    for (markers, refusal) in [
        (&["root", "volume"][..], None),
        (
            &["root", "volume", "readonly-volume"][..],
            Some("VolumeSwitching:"),
        ),
        (
            &["root"][..],
            Some("Provisioned Volume data does not exist"),
        ),
    ] {
        let test = TestDir::new();
        let (socket, server) = start(&test, USABLE_POOL, markers);
        let response = post(
            &socket,
            "/VolumeDriver.Mount",
            json!({"Name": "data", "ID": "container"}),
        )
        .await;
        let error = response
            .get("Err")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match refusal {
            None => {
                assert_eq!(error, "", "{markers:?}: {response}");
                assert_eq!(
                    response.get("Mountpoint").and_then(Value::as_str),
                    Some("/var/lib/ployz-volumes/data")
                );
            }
            Some(refusal) => assert!(error.starts_with(refusal), "{markers:?}: {response}"),
        }
        server.abort();
    }
}
