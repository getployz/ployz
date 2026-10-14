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
async fn destroy_drops_a_landed_seed_docker_never_adopted() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    let landed = settled(&socket).await;
    assert_eq!(
        landed.pointer("/Ok/copy/seed"),
        Some(&json!(true)),
        "{landed}"
    );
    let log = commands(&test);

    let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await;

    assert_eq!(reply, json!({"Ok": null}));
    assert!(
        !test.0.join("seed-volume").exists(),
        "the seed root is gone"
    );
    assert!(test.0.join("volume").exists(), "the source stays");
    assert_eq!(
        effects_since(&test, &log),
        ["zfs destroy -r tank/ployz/seed"]
    );
    assert_eq!(
        post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await,
        reply,
        "a resend finds nothing left"
    );
}

#[tokio::test]
async fn destroy_drops_a_seed_and_the_slot_parent_a_crashed_landing_left() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    settled(&socket).await;
    std::fs::write(test.0.join("seed-slot"), "").unwrap();
    let log = commands(&test);

    let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await;

    assert_eq!(reply, json!({"Ok": null}));
    assert!(
        !test.0.join("seed-volume").exists(),
        "the seed root is gone"
    );
    assert!(
        !test.0.join("seed-slot").exists(),
        "the slot parent is gone"
    );
    assert!(test.0.join("volume").exists(), "the source stays");
    assert_eq!(
        effects_since(&test, &log),
        [
            "zfs destroy -r tank/ployz/seed",
            "zfs destroy -r tank/ployz-mirror/seed"
        ]
    );
}

fn effects_since(test: &TestDir, log: &str) -> Vec<String> {
    commands(test)[log.len()..]
        .lines()
        .filter(|line| !line.starts_with("zfs list") && !line.starts_with("zfs get"))
        .filter(|line| !line.starts_with("zpool "))
        .map(str::to_owned)
        .collect()
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
    fs::write(
        test.0.join("props/fail-command"),
        "inherit mountpoint tank/ployz/seed",
    )
    .unwrap();

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

#[tokio::test]
async fn departure_during_a_seed_copy_stops_it_and_destroys_the_slot() {
    let test = TestDir::new();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("stall-read"), "").unwrap();
    post(&socket, "/Volume.Seed", seed(77)).await;
    let pid = test.0.join("props/receive-pid");
    for _ in 0..300 {
        if pid.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid = fs::read_to_string(pid).unwrap();
    assert_eq!(
        property(&test, "tank/ployz-mirror/seed/fs", "readonly").as_deref(),
        Some("on"),
        "a receive sets its -o properties before any data arrives"
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/seed/fs", "ployz:seed"),
        None,
        "a partial seed carries no mark of a completed one"
    );

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data", "seed"], "stuck": []}})
    );
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
    assert!(!test.0.join("seed-slot").exists());
}

/// Seeds `into` from `point` of `data` with the fake naming `into` as its seed Volume,
/// and waits until the receive has exited and the copy is being verified.
async fn completed_seed(test: &TestDir, into: &str, point: &str, guid: &str) -> PathBuf {
    for marker in ["root", "volume"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    set_property(test, "tank/ployz/data", "snapshots", point);
    let (zpool, zfs) = fake_zfs_seeding(&test.0, USABLE_POOL, into);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    let request = json!({"name": "data", "into": into, "guid": guid});
    post(&socket, "/Volume.Seed", request).await;
    let verified = format!("get -Hp -o value refquota tank/ployz-mirror/{into}/fs");
    for _ in 0..300 {
        if commands(test).contains(&verified) {
            return socket;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the seed of {into} never finished its receive");
}

#[tokio::test]
async fn departure_keeps_a_completed_seed_whose_source_pruned_its_point() {
    // `base` is judged before `data` and `seed` after it.
    for into in ["base", "seed"] {
        let test = TestDir::new();
        let socket = completed_seed(&test, into, POINT, "77").await;
        set_property(
            &test,
            "tank/ployz/data",
            "snapshots",
            "tank/ployz/data@w-1-2\t78\t1700000100",
        );

        let reply = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": ["data"], "stuck": []}}),
            "{into}"
        );
        assert!(test.0.join("seed-fs").exists(), "{into}: the seed is kept");
        assert_eq!(
            snapshot_names(&test, &format!("tank/ployz-mirror/{into}/fs")),
            vec!["w-1-1"],
            "{into}: it holds the only copy of the point"
        );
    }
}

#[tokio::test]
async fn departure_keeps_a_completed_seed_of_a_departure_snapshot() {
    let test = TestDir::new();
    let point = "tank/ployz/data@dep-1700000000\t77\t1700000000";
    let socket = completed_seed(&test, "seed", point, "77").await;

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(reply, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/seed/fs"),
        vec!["dep-1700000000"]
    );
}

/// A seed's receive can exit whole while its run never marks or lands it: the daemon stops,
/// or a later zfs call fails. Its source may since have pruned the point.
#[tokio::test]
async fn departure_keeps_a_seed_whose_receive_exited_before_its_run_finished() {
    let test = TestDir::new();
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/fail-command"),
        "set ployz:seed=77 tank/ployz-mirror/seed/fs",
    )
    .unwrap();
    fs::write(
        test.0.join("props/stall-command"),
        "set readonly=on tank/ployz-mirror/seed/fs",
    )
    .unwrap();
    for marker in ["root", "volume"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    let (socket, _server) = start(&test, USABLE_POOL, &[]);
    post(&socket, "/Volume.Seed", seed(77)).await;
    let landed = test.0.join("props/tank/ployz-mirror/seed/fs/snapshots");
    for _ in 0..300 {
        if fs::read_to_string(&landed).is_ok_and(|snapshots| snapshots.contains("@w-1-1")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-2\t78\t1700000100",
    );

    let departure = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Storage.Demote", json!(null)).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = fs::remove_file(test.0.join("props/stall-command"));
    let reply = departure.await.unwrap();
    assert_eq!(reply, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert!(test.0.join("seed-fs").exists(), "the seed is kept");
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/seed/fs"),
        vec!["w-1-1"],
        "it holds the only copy of the point"
    );
}

#[tokio::test]
async fn departure_keeps_a_seed_whose_landing_stopped_before_the_rename() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(&test, "tank/ployz-mirror/seed", "ployz:pull", "w-1-1");
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    // A landing sets its seed mark before its promote mark, so a stop between them
    // leaves only the seed mark.
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:seed", "77");
    let (socket, _server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(reply, json!({"Ok": {"demoted": ["data"], "stuck": []}}));
    assert!(test.0.join("seed-fs").exists());
}

#[tokio::test]
async fn departure_demotes_the_others_and_names_a_seed_whose_receive_will_not_stop() {
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    fs::write(test.0.join("volume"), "").unwrap();
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    let stuck = "set readonly=on tank/ployz-mirror/seed/fs";
    fs::write(test.0.join("props/stall-command"), stuck).unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.stop_deadline = Duration::from_millis(300);
    let server = tokio::spawn(serve(listener, storage));
    post(&socket, "/Volume.Seed", seed(77)).await;
    for _ in 0..300 {
        if commands(&test).contains(stuck) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        post(&socket, "/Storage.Demote", json!(null)),
    )
    .await
    .expect("the departure waits on a receive that will not stop");
    fs::remove_file(test.0.join("props/stall-command")).unwrap();
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": ["seed"]}}),
        "a stuck seed leaves every other Volume to be demoted"
    );
    let again = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        again.get("Ok").is_some(),
        "the departure released admission: {again}"
    );
    server.abort();
}
