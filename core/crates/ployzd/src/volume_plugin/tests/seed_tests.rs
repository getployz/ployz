//! SeedVolume over fake ZFS: a restore point of `data` becomes the root of `seed`.

use std::time::Duration;

use super::lease_tests::at;
use super::lease_tests::{set_property, start};
use super::*;

const POINT: &str = "tank/ployz/data@w-1-1\t77\t1700000000";

fn seed(guid: u64) -> Value {
    at(
        1,
        1,
        1,
        1,
        json!({"into": "seed", "guid": guid.to_string()}),
    )
}

async fn settled(socket: &Path) -> Value {
    for _ in 0..200 {
        let view = post(
            socket,
            "/Volume.InspectReceive",
            json!({"name": "seed", "round": 1}),
        )
        .await;
        if view
            .pointer("/Ok/status/state")
            .unwrap_or_else(|| panic!("{view}"))
            != "running"
        {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("seed copy never settled");
}

#[tokio::test]
async fn seed_copies_the_point_into_a_new_root_on_the_next_call() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);

    let started = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        started.pointer("/Ok/decision").unwrap(),
        "adopt",
        "{started}"
    );
    assert_eq!(
        started.pointer("/Ok/copy/kind").unwrap(),
        "root",
        "{started}"
    );
    let view = settled(&socket).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done", "{view}");
    assert!(
        test.0.join("seed-fs").exists(),
        "the copy lives under the mirror parent"
    );
    assert!(
        !test.0.join("seed-volume").exists(),
        "nothing is a root until the next call lands it"
    );
    let log = commands(&test);
    assert!(log.contains("zfs send tank/ployz/data@w-1-1\n"), "{log}");
    assert!(
        log.contains(
            "zfs receive -u -s -o readonly=on -o refquota=1073741824 tank/ployz-mirror/seed/fs\n"
        ),
        "the copy carries the source's bound: {log}"
    );

    let landed = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        landed.pointer("/Ok/decision").unwrap(),
        "replay",
        "{landed}"
    );
    assert!(
        test.0.join("seed-volume").exists(),
        "the copy is the root of seed"
    );
    assert!(
        !test.0.join("seed-slot").exists(),
        "the mirror parent is gone"
    );
    assert_eq!(
        property(&test, "tank/ployz/seed", "ployz:seed").as_deref(),
        Some("77")
    );
    assert_eq!(
        property(&test, "tank/ployz/seed", "ployz:promote").as_deref(),
        Some("1")
    );
    assert_eq!(
        property(&test, "tank/ployz/seed", "readonly").as_deref(),
        Some("off")
    );
    assert!(property(&test, "tank/ployz/seed", "mountpoint").is_none());
    assert_eq!(
        snapshot_names(&test, "tank/ployz/seed"),
        Vec::<String>::new(),
        "the received snapshot's name belongs to the source"
    );
    assert_eq!(
        snapshot_names(&test, "tank/ployz/data"),
        ["w-1-1"],
        "the source keeps its point"
    );

    let renames = commands(&test).matches("zfs rename ").count();
    let again = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(again.pointer("/Ok/decision").unwrap(), "replay", "{again}");
    assert_eq!(
        commands(&test).matches("zfs rename ").count(),
        renames,
        "a landed seed is done"
    );
}

#[tokio::test]
async fn seed_replay_finishes_an_interrupted_landing() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        settled(&socket).await.pointer("/Ok/status/state").unwrap(),
        "done"
    );
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "mountpoint",
        "/var/lib/ployz-mirror/seed/fs",
    );
    fs::write(test.0.join("inherit-fails"), "").unwrap();

    let interrupted = post(&socket, "/Volume.Seed", seed(77)).await;
    assert!(interrupted.get("Err").is_some(), "{interrupted}");
    assert!(
        test.0.join("seed-volume").exists(),
        "the landing stopped after the rename"
    );

    fs::remove_file(test.0.join("inherit-fails")).unwrap();
    let replayed = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        replayed.pointer("/Ok/decision").unwrap(),
        "replay",
        "{replayed}"
    );
    assert!(
        !test.0.join("seed-slot").exists(),
        "the mirror parent is gone"
    );
    assert_eq!(
        property(&test, "tank/ployz/seed", "readonly").as_deref(),
        Some("off")
    );
    assert!(property(&test, "tank/ployz/seed", "mountpoint").is_none());
    assert_eq!(
        snapshot_names(&test, "tank/ployz/seed"),
        Vec::<String>::new(),
        "the received snapshot goes"
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
    assert!(
        !commands(&test).contains("zfs set ployz:lease."),
        "a refusal before the record leaves no lease behind"
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
async fn seed_is_busy_while_its_copy_runs() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("stall-read"), "").unwrap();

    let started = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        started.pointer("/Ok/decision").unwrap(),
        "adopt",
        "{started}"
    );
    let response = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "busy",
        "{response}"
    );
}
