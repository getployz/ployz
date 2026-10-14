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
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );

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
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );

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
    assert_eq!(
        response,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    server.abort();
}

#[tokio::test]
async fn departure_on_a_machine_without_zfs_demotes_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    fs::remove_file(test.0.join("zpool")).unwrap();
    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        response,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
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
        set_property(&test, "tank/ployz-mirror/data/fs", "ployz:pulled", "901");
        let (socket, server) = start(&test, USABLE_POOL, markers);

        let response = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            response,
            json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
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
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
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
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
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
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{property}"
        );
        let log = commands(&test);
        assert!(!log.contains("zfs destroy"), "{property}: {log}");
        assert!(test.0.join("mirror-fs").exists(), "{property}");
        server.abort();
    }
}

/// Slots no head mirror pull marked: what v0.3.0-beta.1 and beta.2 leave, and what a head
/// writer path leaves when it stops between two zfs calls. Each may hold the only copy of
/// its point, or only looks like a mirror, so each is kept; a mirror never refreshed since
/// the upgrade leaks one point.
#[tokio::test]
async fn departure_keeps_every_slot_no_landed_pull_marked() {
    const NEWEST: &str = "tank/ployz-mirror/data/fs@w-1-2\t12\t1700000002";
    let cases: &[(&str, &[(&str, &str, &str)])] = &[
        ("a released mirror", &[]),
        (
            "a released mirror whose parent names its newest",
            &[("tank/ployz-mirror/data", "ployz:pull", "w-1-2")],
        ),
        (
            "a pulled slot whose restore set its epoch before its promote",
            &[
                ("tank/ployz-mirror/data", "ployz:pull", "w-1-2"),
                ("tank/ployz-mirror/data/fs", "ployz:epoch", "3"),
            ],
        ),
        (
            "a cut slot whose same-newest resend wrote its pull",
            &[
                ("tank/ployz-mirror/data", "ployz:pull", "w-1-2"),
                ("tank/ployz-mirror/data/fs", "ployz:cut", "on"),
            ],
        ),
        (
            "a beta.1 promote stopped before its rename",
            &[
                ("tank/ployz-mirror/data", "ployz:pull", "w-1-2"),
                ("tank/ployz-mirror/data/fs", "ployz:epoch", "3"),
                ("tank/ployz-mirror/data/fs", "ployz:promote", "on"),
            ],
        ),
        (
            "a stale cut whose pull was inherited away",
            &[("tank/ployz-mirror/data/fs", "ployz:cut", "on")],
        ),
        (
            "a stale epoch whose pull was inherited away",
            &[("tank/ployz-mirror/data/fs", "ployz:epoch", "3")],
        ),
        (
            "a pulled mark naming an older point",
            &[("tank/ployz-mirror/data/fs", "ployz:pulled", "11")],
        ),
        (
            "a landed pull that kept a cut",
            &[
                ("tank/ployz-mirror/data/fs", "ployz:pulled", "12"),
                ("tank/ployz-mirror/data/fs", "ployz:cut", "on"),
            ],
        ),
        (
            "a landed pull that kept a seed",
            &[
                ("tank/ployz-mirror/data/fs", "ployz:pulled", "12"),
                ("tank/ployz-mirror/data/fs", "ployz:seed", "12"),
            ],
        ),
    ];
    for (case, props) in cases {
        let test = TestDir::new();
        set_property(&test, "tank/ployz-mirror/data/fs", "snapshots", NEWEST);
        for (dataset, property, value) in *props {
            set_property(&test, dataset, property, value);
        }
        let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

        let response = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            response,
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{case}"
        );
        assert!(!commands(&test).contains("zfs destroy"), "{case}");
        assert!(test.0.join("mirror-fs").exists(), "{case}");
        server.abort();
    }
}

/// Only a local mark counts: a value the slot inherits was set on another dataset.
#[tokio::test]
async fn departure_keeps_a_slot_that_only_inherits_a_pulled_mark() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "snapshots",
        "tank/ployz-mirror/data/fs@w-1-2\t12\t1700000002",
    );
    set_property(
        &test,
        "inherited/tank/ployz-mirror/data/fs",
        "ployz:pulled",
        "12",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        response,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

/// v0.3.0-beta.1 seeded without a mark: its parent names the point and the copy is whole,
/// but the run failed before landing it and the source may since have pruned the point.
#[tokio::test]
async fn departure_keeps_a_released_seed_that_never_landed() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-2\t78\t1700000100",
    );
    set_property(&test, "tank/ployz-mirror/seed", "ployz:pull", "w-1-1");
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("seed-fs").exists(), "the seed is kept");
    server.abort();
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

/// Why the departure answered it could not demote its one failed Volume.
fn failure_reason(response: &Value) -> &str {
    response
        .pointer("/Ok/failed/0/reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[tokio::test]
async fn departure_moves_a_root_into_the_empty_slot_beside_it() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "mirror"]);

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        response,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert_departed_into_a_slot(&test, &socket).await;
    let log = commands(&test);
    assert!(
        !log.contains("zfs create -o canmount=off -o readonly=on"),
        "{log}"
    );
    server.abort();
}

/// No ployz step makes a root beside a slot that holds a copy: Create refuses a slot, and
/// a pull or a seed refuses a root. Which of the two is newer is unknown, so the departure
/// leaves both and fails that Volume alone.
#[tokio::test]
async fn departure_leaves_a_root_beside_a_slot_that_holds_a_copy_and_demotes_the_rest() {
    let test = TestDir::new();
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "mirror", "mirror-fs", "seed-volume"],
    );

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    let reason = failure_reason(&response);
    assert!(
        reason.contains("both a writer root and a slot"),
        "{response}"
    );
    assert_eq!(
        response.pointer("/Ok/failed/0/name"),
        Some(&json!("data")),
        "{response}"
    );
    assert_eq!(
        response.pointer("/Ok/demoted"),
        Some(&json!(["seed"])),
        "{response}"
    );
    let log = commands(&test);
    assert!(!log.contains("zfs rename tank/ployz/data "), "{log}");
    assert!(!log.contains("zfs snapshot tank/ployz/data@"), "{log}");
    assert!(test.0.join("volume").exists());
    assert!(test.0.join("seed-fs").exists());
    server.abort();
}

/// A Volume whose demote fails after another was demoted still answers that one, so the
/// daemon forgets it: a retry judges it departed and answers nothing for it.
#[tokio::test]
async fn departure_answers_what_it_demoted_before_a_volume_failed() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "seed-volume"]);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/fail-command"),
        "rename tank/ployz/seed tank/ployz-mirror/seed/fs",
    )
    .unwrap();

    let response = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        response.pointer("/Ok/demoted"),
        Some(&json!(["data"])),
        "{response}"
    );
    assert_eq!(
        response.pointer("/Ok/stuck"),
        Some(&json!([])),
        "{response}"
    );
    assert_eq!(
        response.pointer("/Ok/failed/0/name"),
        Some(&json!("seed")),
        "{response}"
    );
    let reason = failure_reason(&response);
    assert!(reason.contains("injected failure"), "{response}");

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        retried,
        json!({"Ok": {"demoted": ["seed"], "stuck": [], "failed": []}})
    );
    server.abort();
}

#[tokio::test]
async fn departure_interrupted_before_the_rename_finishes_on_retry() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "rename-fails"]);

    let interrupted = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        failure_reason(&interrupted).contains("rename interrupted"),
        "{interrupted}"
    );
    assert!(
        test.0.join("mirror").exists(),
        "the slot parent was not made"
    );
    fs::remove_file(test.0.join("rename-fails")).unwrap();

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        retried,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
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
        failure_reason(&interrupted).contains("property unavailable"),
        "{interrupted}"
    );
    assert!(test.0.join("mirror-fs").exists(), "no rename");
    fs::remove_file(&toggle).unwrap();

    let retried = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_departed_into_a_slot(&test, &socket).await;
    assert_eq!(
        retried,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
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
