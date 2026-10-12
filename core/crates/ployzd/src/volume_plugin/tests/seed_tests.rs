//! SeedVolume over fake ZFS: a restore point of `data` becomes the root of `seed`. Every
//! call answers the copy of `seed`, and the same request resent converges.

use std::time::Duration;

use super::inspect_tests::{set_property, start};
use super::*;

const POINT: &str = "tank/ployz/data@w-1-1\t77\t1700000000";

fn seed(guid: u64) -> Value {
    json!({"name": "data", "into": "seed", "guid": guid.to_string()})
}

/// Resends the seed until its reply stops showing a copy in flight.
async fn settled(socket: &Path) -> Value {
    for _ in 0..200 {
        let reply = post(socket, "/Volume.Seed", seed(77)).await;
        let copy = reply
            .pointer("/Ok/copy")
            .unwrap_or_else(|| panic!("{reply}"));
        let copying = copy.get("kind").is_some_and(|kind| kind == "slot")
            && copy.pointer("/pull/failure").is_none_or(Value::is_null);
        if !copying {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("seed copy never settled");
}

fn assert_landed(test: &TestDir) {
    assert!(
        test.0.join("seed-volume").exists(),
        "the copy is the root of seed"
    );
    assert!(
        !test.0.join("seed-slot").exists(),
        "the slot parent is gone"
    );
    assert_eq!(
        property(test, "tank/ployz/seed", "ployz:seed").as_deref(),
        Some("77")
    );
    assert_eq!(
        property(test, "tank/ployz/seed", "ployz:promote").as_deref(),
        Some("on"),
        "the root stays unregistered until Docker's Create adopts it"
    );
    assert_eq!(
        property(test, "tank/ployz/seed", "readonly").as_deref(),
        Some("off")
    );
    assert!(property(test, "tank/ployz/seed", "mountpoint").is_none());
    assert_eq!(
        snapshot_names(test, "tank/ployz/seed"),
        Vec::<String>::new(),
        "the received snapshot's name belongs to the source"
    );
}

#[tokio::test]
async fn seed_copies_the_point_into_a_slot_then_lands_it_as_a_root() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);

    let started = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        started.pointer("/Ok/copy/kind").unwrap(),
        "slot",
        "{started}"
    );
    assert_eq!(
        started.pointer("/Ok/copy/pull/target").unwrap(),
        "w-1-1",
        "{started}"
    );
    let landed = settled(&socket).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    let log = commands(&test);
    assert!(log.contains("zfs send tank/ployz/data@w-1-1\n"), "{log}");
    assert!(
        log.contains(
            "zfs receive -u -s -o readonly=on -o refquota=1073741824 tank/ployz-mirror/seed/fs\n"
        ),
        "the copy carries the source's bound: {log}"
    );
    assert_landed(&test);
    assert_eq!(
        snapshot_names(&test, "tank/ployz/data"),
        ["w-1-1"],
        "the source keeps its point"
    );
}

#[tokio::test]
async fn the_same_seed_twice_has_one_effect_and_the_same_reply() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    let landed = settled(&socket).await;
    let log = commands(&test);

    let again = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(again, landed, "a landed seed answers as it did");
    let after = commands(&test);
    let effects = after[log.len()..]
        .lines()
        .filter(|line| !line.starts_with("zfs list") && !line.starts_with("zfs get"))
        .filter(|line| !line.starts_with("zpool "))
        .collect::<Vec<_>>();
    assert_eq!(effects, Vec::<&str>::new(), "a landed seed changes nothing");
}

#[tokio::test]
async fn a_seed_whose_plugin_restarted_after_the_receive_lands_on_resend() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    post(&socket, "/Volume.Seed", seed(77)).await;
    for _ in 0..200 {
        if snapshot_names(&test, "tank/ployz-mirror/seed/fs") == ["w-1-1"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    server.abort();
    let _ = server.await;
    fs::remove_file(&socket).unwrap();
    assert!(test.0.join("seed-fs").exists(), "the receive finished");
    assert!(!test.0.join("seed-volume").exists(), "nothing landed yet");

    let listener = UnixListener::bind(&socket).unwrap();
    let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
    let _server = tokio::spawn(serve(listener, storage));
    let landed = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_eq!(
        commands(&test).matches("zfs send ").count(),
        1,
        "the resend lands the copy it finds rather than copying again"
    );
    assert_landed(&test);
}

#[tokio::test]
async fn seed_finishes_an_interrupted_landing_on_resend() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "mountpoint",
        "/var/lib/ployz-mirror/seed/fs",
    );
    fs::write(test.0.join("inherit-fails"), "").unwrap();

    let mut interrupted = Value::Null;
    for _ in 0..200 {
        interrupted = post(&socket, "/Volume.Seed", seed(77)).await;
        if interrupted.get("Err").is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(interrupted.get("Err").is_some(), "{interrupted}");
    assert!(
        test.0.join("seed-volume").exists(),
        "the landing stopped after the rename"
    );

    fs::remove_file(test.0.join("inherit-fails")).unwrap();
    let resent = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(resent.pointer("/Ok/copy/kind").unwrap(), "root", "{resent}");
    assert_landed(&test);
}

#[tokio::test]
async fn a_failed_seed_copy_is_answered_once_and_the_next_call_starts_over() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("receive-fails"), "").unwrap();
    post(&socket, "/Volume.Seed", seed(77)).await;

    let failed = settled(&socket).await;
    let failure = failed
        .pointer("/Ok/copy/pull/failure")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{failed}"));
    assert!(failure.contains("out of space"), "{failure}");

    fs::remove_file(test.0.join("props").join("receive-fails")).unwrap();
    let restarted = post(&socket, "/Volume.Seed", seed(77)).await;
    assert!(
        restarted
            .pointer("/Ok/copy/pull/failure")
            .unwrap()
            .is_null(),
        "{restarted}"
    );
    assert_eq!(commands(&test).matches("zfs send ").count(), 2);
    assert_eq!(
        settled(&socket).await.pointer("/Ok/copy/kind").unwrap(),
        "root"
    );
}

#[tokio::test]
async fn seed_refuses_a_point_the_writer_no_longer_holds() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);

    let response = post(&socket, "/Volume.Seed", seed(78)).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "restore_point_gone",
        "{response}"
    );
    assert!(
        !test.0.join("seed-slot").exists(),
        "nothing was made for a point that is gone"
    );
}

#[tokio::test]
async fn seed_refuses_a_root_it_did_not_make() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume", "seed-volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);

    let response = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition",
        "{response}"
    );
}

#[tokio::test]
async fn a_seed_resent_while_its_copy_runs_answers_the_slot_and_copies_once() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("stall-read"), "").unwrap();

    post(&socket, "/Volume.Seed", seed(77)).await;
    let resent = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(resent.pointer("/Ok/copy/kind").unwrap(), "slot", "{resent}");
    assert_eq!(commands(&test).matches("zfs send ").count(), 1);
}
