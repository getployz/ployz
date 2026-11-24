//! SeedVolume over fake ZFS: a restore point of `data` becomes the root of `seed`. Every
//! call answers the copy of `seed`, and the same request resent converges.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use super::copy_tests::{BORN, eventually, let_requests_act, poll, receive_ready};
use super::inspect_tests::{set_property, start};
use super::*;

const POINT: &str = "tank/ployz/data@w-1-1\t77\t1700000000";

fn seed(guid: u64) -> Value {
    json!({"name": "data", "into": "seed", "guid": guid.to_string()})
}

/// Resends the seed until its reply stops showing a copy in flight.
async fn settled(socket: &Path) -> Value {
    poll(async || {
        let reply = post(socket, "/Volume.Seed", seed(77)).await;
        let copy = reply
            .pointer("/Ok/copy")
            .unwrap_or_else(|| panic!("{reply}"));
        let copying = copy.get("kind").is_some_and(|kind| kind == "slot")
            && copy.pointer("/pull/failure").is_none_or(Value::is_null);
        (!copying).then_some(reply)
    })
    .await
    .expect("seed copy never settled")
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
    let test = test_dir();
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

/// A Move cuts a root without clearing the pulled mark, as no root holds one, so a mirror
/// a seed lands in place must lose its mark before it becomes the root.
#[tokio::test]
async fn a_seed_landed_over_a_pulled_mirror_leaves_its_root_unmarked() {
    let test = test_dir();
    let (socket, _server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled", "77");

    let landed = settled(&socket).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_eq!(property(&test, "tank/ployz/seed", "ployz:pulled"), None);
}

#[tokio::test]
async fn destroy_drops_a_landed_seed_docker_never_adopted() {
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    post(&socket, "/Volume.Seed", seed(77)).await;
    eventually("the seed's receive", || {
        snapshot_names(&test, "tank/ployz-mirror/seed/fs") == ["w-1-1"]
    })
    .await;
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
    let test = test_dir();
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

    let interrupted = poll(async || {
        let reply = post(&socket, "/Volume.Seed", seed(77)).await;
        reply.get("Err").is_some().then_some(reply)
    })
    .await;
    assert!(interrupted.is_some(), "the landing never failed");
    assert!(
        test.0.join("seed-volume").exists(),
        "the landing stopped after the rename"
    );

    let resent = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(resent.pointer("/Ok/copy/kind").unwrap(), "root", "{resent}");
    assert_landed(&test);
}

/// A root renamed out of a slot whose parent still holds a record was never stamped with
/// the origin the record names. Its landing is refused before any write: the root stays
/// read-only with its received snapshot, and the parent keeps its record.
#[tokio::test]
async fn a_renamed_seed_whose_parent_holds_a_record_is_refused() {
    let test = test_dir();
    let (socket, _server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-volume", "seed-slot"],
    );
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(
        &test,
        "tank/ployz/seed",
        "snapshots",
        "tank/ployz/seed@w-1-1\t77\t1700000000",
    );
    set_property(&test, "tank/ployz/seed", "ployz:seed", "77");
    set_property(&test, "tank/ployz/seed", "ployz:promote", "on");
    set_property(&test, "tank/ployz/seed", "readonly", "on");
    set_property(&test, "tank/ployz-mirror/seed", "ployz:pull-origin", BORN);
    let log = commands(&test);

    let reply = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!("precondition")),
        "{reply}"
    );
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
    assert!(test.0.join("seed-slot").exists(), "the parent stays");
    assert_eq!(
        property(&test, "tank/ployz-mirror/seed", "ployz:pull-origin").as_deref(),
        Some(BORN)
    );
    assert_eq!(
        property(&test, "tank/ployz/seed", "readonly").as_deref(),
        Some("on")
    );
    assert_eq!(snapshot_names(&test, "tank/ployz/seed"), ["w-1-1"]);
}

#[tokio::test]
async fn a_failed_seed_copy_is_answered_once_and_the_next_call_starts_over() {
    let test = test_dir();
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
    assert!(failure.contains("zfs send -t …"), "{failure}");
    assert!(!failure.contains("token-1"), "{failure}");

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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("stall-read"), "").unwrap();
    post(&socket, "/Volume.Seed", seed(77)).await;
    let pid = receive_ready(&test).await;
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
        json!({"Ok": {"demoted": ["data", "seed"], "stuck": [], "failed": []}})
    );
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(!test.0.join("seed-slot").exists());
}

/// A seed resent after a departure stopped its copy starts the copy over, with no stop to
/// answer first. The departure fails to demote the source, so the Machine stays
/// Participating and the source is still the writer for the resend.
#[tokio::test]
async fn a_seed_resent_after_a_failed_departure_stopped_its_copy_starts_it_over() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("stall-read"), "").unwrap();
    post(&socket, "/Volume.Seed", seed(77)).await;
    receive_ready(&test).await;
    fs::write(test.0.join("rename-fails"), "").unwrap();
    let seal = post(&socket, "/Storage.Seal", json!(null)).await;
    assert_eq!(seal.pointer("/Ok/stuck"), Some(&json!([])), "{seal}");
    assert_eq!(
        seal.pointer("/Ok/failed/0/name"),
        Some(&json!("data")),
        "{seal}"
    );

    fs::remove_file(test.0.join("props").join("stall-read")).unwrap();
    let resent = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        resent.pointer("/Ok/copy/pull/failure"),
        Some(&json!(null)),
        "{resent}"
    );
    assert_eq!(commands(&test).matches("zfs send ").count(), 2);
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
    eventually(&format!("the seed of {into}'s receive"), || {
        commands(test).contains(&verified)
    })
    .await;
    socket
}

#[tokio::test]
async fn departure_keeps_a_completed_seed_whose_source_pruned_its_point() {
    // `base` is judged before `data` and `seed` after it.
    for into in ["base", "seed"] {
        let test = test_dir();
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
            json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
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
    let test = test_dir();
    let point = "tank/ployz/data@dep-1700000000\t77\t1700000000";
    let socket = completed_seed(&test, "seed", point, "77").await;

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/seed/fs"),
        vec!["dep-1700000000"]
    );
}

/// A seed's receive can exit whole while its run never marks or lands it: the daemon stops,
/// or a later zfs call fails. Its source may since have pruned the point.
#[tokio::test]
async fn departure_keeps_a_seed_whose_receive_exited_before_its_run_finished() {
    let test = test_dir();
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
    eventually("the seed's receive", || {
        fs::read_to_string(&landed).is_ok_and(|snapshots| snapshots.contains("@w-1-1"))
    })
    .await;
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
    let_requests_act(Duration::from_millis(200)).await;
    let _ = fs::remove_file(test.0.join("props/stall-command"));
    let reply = departure.await.unwrap();
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("seed-fs").exists(), "the seed is kept");
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/seed/fs"),
        vec!["w-1-1"],
        "it holds the only copy of the point"
    );
}

#[tokio::test]
async fn departure_demotes_the_others_and_names_a_seed_whose_receive_will_not_stop() {
    let test = test_dir();
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
    eventually("the stalled command", || commands(&test).contains(stuck)).await;

    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        post(&socket, "/Storage.Demote", json!(null)),
    )
    .await
    .expect("the departure waits on a receive that will not stop");
    fs::remove_file(test.0.join("props/stall-command")).unwrap();
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": ["seed"], "failed": []}}),
        "a stuck seed leaves every other Volume to be demoted"
    );
    let again = post(&socket, "/Storage.Demote", json!(null)).await;
    assert!(
        again.get("Ok").is_some(),
        "the departure released admission: {again}"
    );
    server.abort();
}

#[tokio::test]
async fn a_seed_refuses_a_slot_holding_a_copy_this_machine_wrote() {
    for (property, value) in [
        (
            "snapshots",
            "tank/ployz-mirror/seed/fs@dep-100\t900\t1690000000",
        ),
        ("ployz:cut", "on"),
        ("ployz:epoch", "3"),
    ] {
        let test = test_dir();
        let (socket, _server) = start(
            &test,
            USABLE_POOL,
            &["root", "volume", "seed-slot", "seed-fs"],
        );
        set_property(&test, "tank/ployz/data", "snapshots", POINT);
        set_property(&test, "tank/ployz-mirror/seed/fs", property, value);

        let reply = post(&socket, "/Volume.Seed", seed(77)).await;
        assert!(reply.get("Err").is_some(), "{property}: {reply}");
        let log = commands(&test);
        assert!(!log.contains("zfs destroy"), "{property}: {log}");
        assert!(test.0.join("seed-fs").exists(), "{property}");
    }
}

/// A slot at another point that no mirror pull marked: a mirror v0.3.0-beta.2 or earlier
/// left, or a seed's completed copy whose source may since have pruned its point. Nothing
/// proves a writer elsewhere holds it, so the seed keeps it.
#[tokio::test]
async fn a_seed_refuses_a_slot_no_mirror_pull_marked() {
    for pulling in [false, true] {
        let test = test_dir();
        let (socket, _server) = start(
            &test,
            USABLE_POOL,
            &["root", "volume", "seed-slot", "seed-fs"],
        );
        set_property(&test, "tank/ployz/data", "snapshots", POINT);
        set_property(
            &test,
            "tank/ployz-mirror/seed/fs",
            "snapshots",
            "tank/ployz-mirror/seed/fs@w-9-1\t900\t1690000000",
        );
        if pulling {
            set_property(&test, "tank/ployz-mirror/seed", "ployz:pull", "w-9-1");
        }

        let reply = post(&socket, "/Volume.Seed", seed(77)).await;
        assert!(reply.get("Err").is_some(), "pulling {pulling}: {reply}");
        let log = commands(&test);
        assert!(!log.contains("zfs destroy"), "pulling {pulling}: {log}");
        assert!(test.0.join("seed-fs").exists(), "pulling {pulling}");
    }
}

/// A mirror whose newest snapshot a mirror pull marked holds a copy a writer elsewhere
/// made, and a slot with no snapshot holds no point at all: the seed replaces either.
#[tokio::test]
async fn a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point() {
    for pulled in [true, false] {
        let test = test_dir();
        let (socket, _server) = start(
            &test,
            USABLE_POOL,
            &["root", "volume", "seed-slot", "seed-fs"],
        );
        set_property(&test, "tank/ployz/data", "snapshots", POINT);
        if pulled {
            set_property(
                &test,
                "tank/ployz-mirror/seed/fs",
                "snapshots",
                "tank/ployz-mirror/seed/fs@w-9-1\t900\t1690000000",
            );
            set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled", "900");
        }

        let landed = settled(&socket).await;
        assert_eq!(
            landed.pointer("/Ok/copy/kind").unwrap(),
            "root",
            "pulled {pulled}: {landed}"
        );
        let log = commands(&test);
        assert!(
            log.contains("zfs destroy -r tank/ployz-mirror/seed\n"),
            "pulled {pulled}: {log}"
        );
        assert_landed(&test);
    }
}

/// Docker adopts a landed seed, a departure parks its root as an old copy, and a Writer
/// promotes that copy back. The result is a writer root, not a seed Docker never adopted.
async fn promoted_back_seed(test: &TestDir, socket: &Path) -> Value {
    set_property(test, "tank/ployz/data", "snapshots", POINT);
    settled(socket).await;
    let created = post(
        socket,
        "/VolumeDriver.Create",
        json!({"Name": "seed", "Opts": {"size": "1g"}}),
    )
    .await;
    assert_eq!(created, json!({"Err": ""}));
    assert_eq!(
        property(test, "tank/ployz/seed", "ployz:promote"),
        None,
        "Docker adopted the root"
    );
    let demoted = post(socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        demoted,
        json!({"Ok": {"demoted": ["data", "seed"], "stuck": [], "failed": []}})
    );
    let parked = post(socket, "/Volume.Copy.Inspect", json!({"name": "seed"})).await;
    let guid = parked
        .pointer("/Ok/newest/guid")
        .cloned()
        .unwrap_or_else(|| panic!("{parked}"));
    json!({"name": "seed", "at": guid, "epoch": 1})
}

#[tokio::test]
async fn a_seeded_volume_promoted_back_after_a_departure_is_a_writer() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let writer = promoted_back_seed(&test, &socket).await;

    let reply = post(&socket, "/Volume.Copy.Writer", writer).await;

    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    assert_eq!(reply.pointer("/Ok/seed"), Some(&json!(false)), "{reply}");
    assert_eq!(property(&test, "tank/ployz/seed", "ployz:seed"), None);
    let destroyed = post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await;
    assert_eq!(
        destroyed.pointer("/Err/details/reason"),
        Some(&json!("no_transition")),
        "{destroyed}"
    );
    assert!(test.0.join("seed-volume").exists(), "the writer root stays");
}

#[tokio::test]
async fn a_seed_resent_to_a_volume_promoted_back_after_a_departure_is_refused() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let writer = promoted_back_seed(&test, &socket).await;
    let promoted = post(&socket, "/Volume.Copy.Writer", writer).await;
    assert_eq!(
        promoted.pointer("/Ok/kind"),
        Some(&json!("root")),
        "{promoted}"
    );
    let log = commands(&test);

    let resent = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(
        resent.pointer("/Err/details/reason"),
        Some(&json!("precondition")),
        "{resent}"
    );
    assert_eq!(
        resent.pointer("/Err/message"),
        Some(&json!("Volume seed already exists on this Machine")),
        "{resent}"
    );
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
}

#[tokio::test]
async fn a_writer_whose_seed_mark_clear_failed_promotes_a_writer_on_resend() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let writer = promoted_back_seed(&test, &socket).await;
    fs::write(
        test.0.join("props/fail-command"),
        "inherit ployz:seed tank/ployz-mirror/seed/fs",
    )
    .unwrap();

    let failed = post(&socket, "/Volume.Copy.Writer", writer.clone()).await;
    assert!(failed.get("Err").is_some(), "{failed}");
    let parked = post(&socket, "/Volume.Copy.Inspect", json!({"name": "seed"})).await;
    assert_eq!(parked.pointer("/Ok/kind"), Some(&json!("slot")), "{parked}");

    let resent = post(&socket, "/Volume.Copy.Writer", writer).await;
    assert_eq!(resent.pointer("/Ok/kind"), Some(&json!("root")), "{resent}");
    assert_eq!(resent.pointer("/Ok/seed"), Some(&json!(false)), "{resent}");
    assert_eq!(property(&test, "tank/ployz/seed", "ployz:seed"), None);
}

/// `land` marks the slot's `fs` and then renames it. A stop between leaves a whole copy
/// that carries both marks, and the resend lands it.
#[tokio::test]
async fn a_seed_whose_landing_stopped_before_its_rename_lands_on_resend() {
    let test = test_dir();
    let (socket, _server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(&test, "tank/ployz-mirror/seed", "ployz:pull", "w-1-1");
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:seed", "77");
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:promote", "on");

    let landed = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(
        landed.pointer("/Ok/copy/kind"),
        Some(&json!("root")),
        "{landed}"
    );
    assert_eq!(commands(&test).matches("zfs send ").count(), 0);
    assert_landed(&test);
}

#[tokio::test]
async fn a_seed_replaces_a_slot_parent_with_no_fs() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume", "seed-slot"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);

    let landed = settled(&socket).await;

    assert_eq!(
        landed.pointer("/Ok/copy/kind"),
        Some(&json!("root")),
        "{landed}"
    );
    let log = commands(&test);
    assert!(
        log.contains("zfs destroy -r tank/ployz-mirror/seed\n"),
        "{log}"
    );
    assert_landed(&test);
}

/// Docker's Create leaves `ployz:seed` on the root it adopts, so the same Seed resent
/// still finds its own root.
#[tokio::test]
async fn a_seed_resent_after_docker_adopted_its_root_answers_the_root() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    settled(&socket).await;
    let created = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name": "seed", "Opts": {"size": "1g"}}),
    )
    .await;
    assert_eq!(created, json!({"Err": ""}));
    let log = commands(&test);

    let resent = post(&socket, "/Volume.Seed", seed(77)).await;

    assert_eq!(
        resent.pointer("/Ok/copy/kind"),
        Some(&json!("root")),
        "{resent}"
    );
    assert_eq!(
        resent.pointer("/Ok/copy/seed"),
        Some(&json!(false)),
        "{resent}"
    );
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
}

/// A failed copy read only after its receive ended is answered by the Seed that takes the
/// failure from the registry, since no receive is left to show it.
#[tokio::test]
async fn a_seed_answers_once_a_failed_copy_whose_receive_already_ended() {
    let test = test_dir();
    let (socket, _server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    fs::write(test.0.join("props").join("receive-fails"), "").unwrap();
    post(&socket, "/Volume.Seed", seed(77)).await;
    let ended = poll(async || {
        let copy = post(&socket, "/Volume.Copy.Inspect", json!({"name": "seed"})).await;
        (copy.pointer("/Ok/pull_state/kind") == Some(&json!("failed"))).then_some(copy)
    })
    .await;
    assert!(ended.is_some(), "the seed's receive never failed");
    fs::remove_file(test.0.join("props").join("receive-fails")).unwrap();

    let failed = post(&socket, "/Volume.Seed", seed(77)).await;
    let failure = failed
        .pointer("/Ok/copy/pull/failure")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{failed}"));
    assert!(failure.contains("out of space"), "{failure}");
    assert_eq!(commands(&test).matches("zfs send ").count(), 1);

    let restarted = post(&socket, "/Volume.Seed", seed(77)).await;
    assert_eq!(
        restarted.pointer("/Ok/copy/pull/failure"),
        Some(&json!(null)),
        "{restarted}"
    );
    assert_eq!(commands(&test).matches("zfs send ").count(), 2);
}

/// A point carrying an origin a receive wrote was not stamped here, so the Seed refuses
/// it before it clears the destination, and the Mirror there keeps its bytes and marks.
/// The same value stamped here seeds, and the root carries it.
#[tokio::test]
async fn a_seed_of_a_received_origin_refuses_and_keeps_the_mirror_in_its_slot() {
    let test = test_dir();
    let (socket, _server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    set_property(&test, "tank/ployz/data", "ployz:origin", BORN);
    let received = test.0.join("props/received/tank/ployz/data");
    fs::create_dir_all(received.parent().unwrap()).unwrap();
    fs::write(&received, "").unwrap();
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-0-0\t55\t1690000000",
    );
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled", "55");
    set_property(&test, "tank/ployz-mirror/seed", "ployz:pull", "w-0-0");

    let reply = post(&socket, "/Volume.Seed", seed(77)).await;
    let message = reply.pointer("/Err/message").and_then(Value::as_str);
    assert!(
        message.is_some_and(|message| message.contains("did not stamp")),
        "{reply}"
    );
    assert!(test.0.join("seed-fs").exists(), "the Mirror survives");
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/seed/fs"),
        ["w-0-0"]
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled").as_deref(),
        Some("55")
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/seed", "ployz:pull").as_deref(),
        Some("w-0-0")
    );
    assert!(!commands(&test).contains("zfs destroy"));

    fs::remove_file(&received).unwrap();
    let landed = settled(&socket).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_eq!(
        property(&test, "tank/ployz/seed", "ployz:origin").as_deref(),
        Some(BORN)
    );
}

/// The origin a Copy step's birth gives the new Volume, distinct from any source's.
const BIRTH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// `data`'s origin: it carries none, so it is Legacy.
fn legacy_data() -> String {
    ployz_core::VolumeOrigin::legacy(&ployz_core::DockerVolumeName::parse("data").unwrap())
        .to_string()
}

/// The digest of the Copy step the tests' seeds ride on.
const STEP: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// A window that holds for any clock a test runs on.
const OPEN: u64 = 4_102_444_800_000;

/// The daemon found the window open, closing at `closes` milliseconds.
fn open(closes: u64) -> Value {
    json!({"open": {"closes": closes}})
}

/// The daemon found the window closed.
fn closed() -> Value {
    json!("closed")
}

/// A SeedCopy of `data`'s point 77 into `seed` under [`STEP`], bound to [`BIRTH`].
fn copy(window: Value) -> Value {
    copy_of(&legacy_data(), 1_700_000_000, window)
}

fn copy_of(source: &str, taken_seconds: u64, window: Value) -> Value {
    json!({
        "seed": seed(77),
        "copy": {
            "birth": BIRTH, "source": source, "taken_at": taken_seconds * 1000,
            "step": STEP, "window": window,
        },
    })
}

/// The command that makes the step's slot of `seed`, bounded as `data` is.
const MAKE_SLOT: &str = "create -o canmount=off -o readonly=on -o refquota=1073741824 -o ployz:pull-origin=1111111111111111111111111111111111111111111111111111111111111111 -o ployz:copy-step=2222222222222222222222222222222222222222222222222222222222222222 -o ployz:pull=w-1-1 tank/ployz-mirror/seed";

/// Resends `request` until its reply stops showing a copy in flight.
async fn copy_settled(socket: &Path, request: &Value) -> Value {
    poll(async || {
        let reply = post(socket, "/Volume.SeedCopy", request.clone()).await;
        let copy = reply.pointer("/Ok/copy")?;
        let copying = copy.get("kind").is_some_and(|kind| kind == "slot")
            && copy.pointer("/pull/failure").is_none_or(Value::is_null);
        (!copying).then_some(reply)
    })
    .await
    .expect("seed copy never settled")
}

fn refused(reply: &Value, reason: &str) {
    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!(reason)),
        "{reply}"
    );
}

fn assert_copy_landed(test: &TestDir) {
    assert!(test.0.join("seed-volume").exists(), "the copy is the root");
    assert_eq!(
        property(test, "tank/ployz/seed", "ployz:origin").as_deref(),
        Some(BIRTH)
    );
    assert_eq!(
        property(test, "tank/ployz/seed", "ployz:copy-step").as_deref(),
        Some(STEP),
        "the root still names its step"
    );
}

/// A plugin serving `test`'s fixture, as a new plugin process does: the datasets and the
/// tombstones are the fixture's, and the receive registry is new. Its clock reads `clock`
/// in milliseconds when one is given.
fn plugin(
    test: &TestDir,
    clock: Option<&Arc<AtomicU64>>,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let (zpool, zfs) = if test.0.join("zfs").exists() {
        (test.0.join("zpool"), test.0.join("zfs"))
    } else {
        fake_zfs(&test.0, USABLE_POOL)
    };
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    if let Some(clock) = clock {
        let clock = Arc::clone(clock);
        storage.now = Arc::new(move || {
            ployz_core::authority::Timestamp::from_millis(clock.load(Ordering::SeqCst)).unwrap()
        });
    }
    let socket = test.0.join("plugin.sock");
    let _ = fs::remove_file(&socket);
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    (socket, server)
}

/// A fixture holding `data`'s point, with `markers` standing for its other datasets.
fn fixture(markers: &[&str]) -> TestDir {
    let test = test_dir();
    for marker in ["root", "volume"].iter().chain(markers) {
        fs::write(test.0.join(marker), "").unwrap();
    }
    set_property(&test, "tank/ployz/data", "snapshots", POINT);
    test
}

/// The slot the step made, holding a receive a restart killed: its parent names the step
/// and records the birth, and its `fs` holds no point yet.
fn killed_copy() -> TestDir {
    let test = fixture(&["seed-slot", "seed-fs"]);
    let parent = "tank/ployz-mirror/seed";
    set_property(&test, parent, "ployz:copy-step", STEP);
    set_property(&test, parent, "ployz:pull-origin", BIRTH);
    set_property(&test, parent, "ployz:pull", "w-1-1");
    test
}

async fn stop(server: tokio::task::JoinHandle<io::Result<()>>) {
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn a_copy_lands_carrying_its_birth_and_its_step() {
    let test = fixture(&[]);
    let (socket, _server) = plugin(&test, None);

    let landed = copy_settled(&socket, &copy(open(OPEN))).await;

    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_landed(&test);
    assert_copy_landed(&test);
}

#[tokio::test]
async fn a_copy_of_a_point_the_step_did_not_approve_is_refused_before_any_effect() {
    let test = fixture(&[]);
    let (socket, _server) = plugin(&test, None);
    let log = commands(&test);

    let foreign = post(
        &socket,
        "/Volume.SeedCopy",
        copy_of(BORN, 1_700_000_000, open(OPEN)),
    )
    .await;
    let other_time = post(
        &socket,
        "/Volume.SeedCopy",
        copy_of(&legacy_data(), 1_700_000_001, open(OPEN)),
    )
    .await;

    refused(&foreign, "precondition");
    refused(&other_time, "precondition");
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
}

/// Once the daemon found the window closed, the plugin starts no copy and starts none over,
/// however early its own clock reads, but it finishes the copy underway.
#[tokio::test]
async fn a_closed_window_finishes_a_copy_underway_and_starts_none() {
    let early = Arc::new(AtomicU64::new(1_000));
    let fresh = fixture(&[]);
    let (socket, _server) = plugin(&fresh, Some(&early));
    let log = commands(&fresh);
    refused(
        &post(&socket, "/Volume.SeedCopy", copy(closed())).await,
        "expired",
    );
    refused(
        &post(&socket, "/Volume.SeedCopy", copy(open(500))).await,
        "expired",
    );
    assert_eq!(effects_since(&fresh, &log), Vec::<String>::new());

    let killed = killed_copy();
    let (socket, _server) = plugin(&killed, Some(&early));
    let log = commands(&killed);
    refused(
        &post(&socket, "/Volume.SeedCopy", copy(closed())).await,
        "expired",
    );
    assert_eq!(effects_since(&killed, &log), Vec::<String>::new());
    assert!(killed.0.join("seed-fs").exists());

    let underway = fixture(&[]);
    let (socket, _server) = plugin(&underway, None);
    let started = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;
    assert_eq!(
        started.pointer("/Ok/copy/kind").unwrap(),
        "slot",
        "{started}"
    );
    let landed = copy_settled(&socket, &copy(closed())).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_copy_landed(&underway);
}

/// However the new Volume is deleted, a resend of the same Copy inside its window to a
/// restarted plugin is refused, makes nothing, and the Volume stays gone.
#[tokio::test]
async fn a_copy_never_makes_its_deleted_volume_again() {
    for delete in ["forget the landed root", "docker rm", "forget the slot"] {
        let test = if delete == "forget the slot" {
            killed_copy()
        } else {
            fixture(&[])
        };
        let (socket, server) = plugin(&test, None);
        if delete != "forget the slot" {
            copy_settled(&socket, &copy(open(OPEN))).await;
        }
        let deleted = if delete == "docker rm" {
            let created = post(
                &socket,
                "/VolumeDriver.Create",
                json!({"Name": "seed", "Opts": {"size": "1g"}}),
            )
            .await;
            assert_eq!(created, json!({"Err": ""}), "{delete}");
            post(&socket, "/VolumeDriver.Remove", json!({"Name": "seed"})).await
        } else {
            post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await
        };
        assert!(
            deleted.get("Err").is_none_or(|error| error == ""),
            "{delete}: {deleted}"
        );
        stop(server).await;
        let (socket, _server) = plugin(&test, None);
        let log = commands(&test);

        let reply = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;

        refused(&reply, "precondition");
        assert_eq!(effects_since(&test, &log), Vec::<String>::new(), "{delete}");
        for gone in ["seed-volume", "seed-slot", "seed-fs"] {
            assert!(!test.0.join(gone).exists(), "{delete}: {gone}");
        }
    }
}

/// A call that fails before any effect, here refused by an upgrade holding storage, leaves
/// the step free: the same Copy resent begins the copy.
#[tokio::test]
async fn a_copy_refused_before_any_effect_begins_on_resend() {
    let test = fixture(&[]);
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let storage = VolumeStorage::with_programs(zpool, zfs);
    let installation = storage.installation.clone();
    let socket = test.0.join("plugin.sock");
    let _server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    installation.mark_active("upgrade").unwrap();
    let log = commands(&test);

    let refused_first = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;
    assert!(refused_first.get("Err").is_some(), "{refused_first}");
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
    installation.clear_active("upgrade").unwrap();

    let landed = copy_settled(&socket, &copy(open(OPEN))).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_copy_landed(&test);
}

/// ZFS fails to make the step's slot, as a plugin that stops right there leaves it: the
/// plugin restarts, and the same Copy resent inside its window makes the slot and lands.
#[tokio::test]
async fn a_copy_interrupted_making_its_slot_lands_on_resend() {
    let test = fixture(&[]);
    let (socket, server) = plugin(&test, None);
    fs::write(test.0.join("props/fail-command"), MAKE_SLOT).unwrap();

    let failed = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;
    assert!(failed.get("Err").is_some(), "{failed}");
    assert!(!test.0.join("seed-slot").exists());
    stop(server).await;
    let (socket, _server) = plugin(&test, None);

    let landed = copy_settled(&socket, &copy(open(OPEN))).await;

    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_copy_landed(&test);
}

/// A copy a restart killed starts over in the slot it made, which it keeps: interrupted
/// as it clears the old receive, the plugin restarts, and the resend lands.
#[tokio::test]
async fn a_copy_interrupted_starting_over_keeps_its_slot_and_lands_on_resend() {
    let test = killed_copy();
    let (socket, server) = plugin(&test, None);
    fs::write(
        test.0.join("props/fail-command"),
        "destroy -r tank/ployz-mirror/seed/fs",
    )
    .unwrap();

    let failed = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;
    assert!(failed.get("Err").is_some(), "{failed}");
    assert!(test.0.join("seed-slot").exists());
    stop(server).await;
    let (socket, _server) = plugin(&test, None);

    let landed = copy_settled(&socket, &copy(open(OPEN))).await;

    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_copy_landed(&test);
    let log = commands(&test);
    let parent_destroyed = log
        .lines()
        .position(|line| line == "zfs destroy -r tank/ployz-mirror/seed");
    assert_eq!(parent_destroyed, None, "{log}");
}

/// A Copy whose window closes while it waits for the plugin's mutation starts nothing:
/// the window is read under the mutation, not when the request arrived.
#[tokio::test]
async fn a_copy_queued_past_its_window_starts_nothing() {
    let test = fixture(&[]);
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.now = {
        let clock = Arc::clone(&clock);
        Arc::new(move || {
            ployz_core::authority::Timestamp::from_millis(clock.load(Ordering::SeqCst)).unwrap()
        })
    };
    let request: crate::volume_copy::SeedCopy =
        serde_json::from_value(copy(open(1_000_500))).unwrap();
    let held = storage.mutation.lock().await;
    let log = commands(&test);

    let seeding = storage.seed_volume(&request.seed, Some(&request.copy));
    let mut seeding = std::pin::pin!(seeding);
    std::future::poll_fn(|context| {
        assert!(
            seeding.as_mut().poll(context).is_pending(),
            "the seed waits for the mutation"
        );
        std::task::Poll::Ready(())
    })
    .await;
    clock.store(1_001_000, Ordering::SeqCst);
    drop(held);
    let reply = seeding.await;

    let error = reply.expect_err("the window closed while the seed waited");
    assert_eq!(
        serde_json::to_value(&error)
            .unwrap()
            .pointer("/details/reason"),
        Some(&json!("expired")),
        "{error:?}"
    );
    assert_eq!(effects_since(&test, &log), Vec::<String>::new());
}

/// Sends the Copy to a plugin whose clock reads 1,000,000 ms against a window closing at
/// 1,000,500, holds `stalled` until the clock has passed the window, and answers the reply.
async fn closing_during(test: &TestDir, stalled: &str) -> Value {
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let (socket, _server) = plugin(test, Some(&clock));
    fs::write(test.0.join("props/stall-command"), stalled).unwrap();
    let reply =
        tokio::spawn(async move { post(&socket, "/Volume.SeedCopy", copy(open(1_000_500))).await });
    let waiting = test.0.join("props/stalled");
    eventually("the stalled command", || {
        fs::read_to_string(&waiting).is_ok_and(|line| line.trim_end() == stalled)
    })
    .await;
    clock.store(1_001_000, Ordering::SeqCst);
    fs::remove_file(test.0.join("props/stall-command")).unwrap();
    reply.await.unwrap()
}

/// The window closes while the copy makes room for itself, here the mirror root: no slot
/// is made, and the step, which made nothing, stays free to begin.
#[tokio::test]
async fn a_copy_whose_window_closes_while_it_prepares_makes_no_slot() {
    let test = fixture(&[]);

    let reply = closing_during(
        &test,
        "create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror",
    )
    .await;

    refused(&reply, "expired");
    assert!(!test.0.join("seed-slot").exists());
    let (socket, _server) = plugin(&test, None);
    let landed = copy_settled(&socket, &copy(open(OPEN))).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
}

/// The window closes while the copy reads whether its old receive holds a point written
/// here: the slot stays as it was, receive and all.
#[tokio::test]
async fn a_copy_whose_window_closes_while_it_reads_its_slot_keeps_it() {
    let test = killed_copy();

    let reply = closing_during(
        &test,
        "get -H -o value ployz:epoch tank/ployz-mirror/seed/fs",
    )
    .await;

    refused(&reply, "expired");
    assert!(test.0.join("seed-slot").exists());
    assert!(test.0.join("seed-fs").exists());
}

/// The window closes as the copy clears its old receive: no receive starts, and the slot
/// it made stays, so a call inside a window starts the copy over in it and lands.
#[tokio::test]
async fn a_copy_whose_window_closes_after_it_clears_its_slot_starts_no_receive() {
    let test = killed_copy();

    let reply = closing_during(&test, "destroy -r tank/ployz-mirror/seed/fs").await;

    refused(&reply, "expired");
    assert!(test.0.join("seed-slot").exists());
    assert!(!test.0.join("seed-fs").exists());
    assert!(!commands(&test).contains("zfs receive"));
    let (socket, _server) = plugin(&test, None);
    let landed = copy_settled(&socket, &copy(open(OPEN))).await;
    assert_eq!(landed.pointer("/Ok/copy/kind").unwrap(), "root", "{landed}");
    assert_copy_landed(&test);
}

/// An empty slot parent of `seed`, such as an interrupted mirror pull leaves, is no copy
/// this step made: whether or not the step's Volume was made and then deleted, a resend
/// refuses it and leaves it as it is.
#[tokio::test]
async fn a_copy_never_clears_an_empty_slot_parent_it_did_not_make() {
    for made in [false, true] {
        let test = fixture(&[]);
        let (socket, _server) = plugin(&test, None);
        if made {
            copy_settled(&socket, &copy(open(OPEN))).await;
            post(&socket, "/Volume.Copy.Destroy", json!({"name": "seed"})).await;
        }
        fs::write(test.0.join("seed-slot"), "").unwrap();
        let log = commands(&test);

        let reply = post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await;

        refused(&reply, "precondition");
        assert_eq!(
            effects_since(&test, &log),
            Vec::<String>::new(),
            "made {made}"
        );
        assert!(test.0.join("seed-slot").exists(), "made {made}");
    }
}

/// A slot holding the point under another origin, and a root holding it under another
/// origin, are other copies of `seed`: a copy refuses both and leaves them as they are.
#[tokio::test]
async fn a_copy_never_adopts_another_copy_of_its_volume() {
    let slot = fixture(&["seed-slot", "seed-fs"]);
    let (socket, _server) = plugin(&slot, None);
    set_property(
        &slot,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    set_property(&slot, "tank/ployz-mirror/seed", "ployz:pull-origin", BORN);
    let log = commands(&slot);
    refused(
        &post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await,
        "precondition",
    );
    assert_eq!(effects_since(&slot, &log), Vec::<String>::new());

    let root = fixture(&["seed-volume"]);
    let (socket, _server) = plugin(&root, None);
    set_property(&root, "tank/ployz/seed", "ployz:seed", "77");
    set_property(&root, "tank/ployz/seed", "ployz:origin", BORN);
    let log = commands(&root);
    refused(
        &post(&socket, "/Volume.SeedCopy", copy(open(OPEN))).await,
        "precondition",
    );
    assert_eq!(effects_since(&root, &log), Vec::<String>::new());
}
