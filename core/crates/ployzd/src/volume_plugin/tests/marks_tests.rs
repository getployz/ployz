//! The `ployz:*` dataset properties over fake ZFS: the exact ordered `zfs` argv each
//! transition runs against them. The names and the order of every set and inherit are on
//! disk on live Machines, so a change to either fails here before it ships.

use super::copy_tests::{SLOT, SLOT_FS, Source, poll, pull, puller, set_snapshots};
use super::inspect_tests::{set_property, start};
use super::*;

const ROOT: &str = "tank/ployz/data";

/// Resends the Pull until it answers no pull in flight, which it does once the slot holds
/// the target and the pull's own mark is cleared.
async fn finished(socket: &Path, target: &str) -> Value {
    poll(async || {
        let reply = post(socket, "/Volume.Copy.Pull", pull(target)).await;
        (reply.pointer("/Ok/pull") == Some(&Value::Null)).then_some(reply)
    })
    .await
    .unwrap_or_else(|| panic!("the pull of {target} never finished"))
}

/// Every `zfs` call that reads or writes a mark, and the steps whose position among them
/// matters, in the order the plugin ran them.
fn trace(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| {
            line.starts_with("zfs ")
                && ["ployz:", "receive_resume_token", "receive -A", "rename "]
                    .iter()
                    .any(|needle| line.contains(needle))
        })
        .map(str::to_owned)
        .collect()
}

/// Marks a test planted as inherited from a parent: effective reads see them, `-s local`
/// reads do not.
fn inherit_property(test: &TestDir, dataset: &str, property: &str, value: &str) {
    let directory = test.0.join("props/inherited").join(dataset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(property), format!("{value}\n")).unwrap();
}

fn writer_root(
    test: &TestDir,
    snapshots: &[(&str, u64)],
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let started = start(test, USABLE_POOL, &["root", "volume", "mounted"]);
    set_snapshots(test, ROOT, snapshots);
    started
}

fn slot_copy(
    test: &TestDir,
    snapshots: &[(&str, u64)],
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let started = start(test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);
    set_snapshots(test, SLOT_FS, snapshots);
    started
}

#[tokio::test]
async fn copy_writer_promotes_a_pulled_slot_in_this_order() {
    let test = TestDir::new();
    let (socket, server) = slot_copy(&test, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "receive_resume_token", "token-1");
    set_property(&test, SLOT_FS, "ployz:pulled", "13");
    set_property(&test, SLOT_FS, "ployz:seed", "11");

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value receive_resume_token tank/ployz-mirror/data/fs",
            "zfs receive -A tank/ployz-mirror/data/fs",
            "zfs inherit ployz:pulled tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:seed tank/ployz-mirror/data/fs",
            "zfs inherit ployz:seed tank/ployz-mirror/data/fs",
            "zfs set ployz:epoch=2 tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:cut tank/ployz-mirror/data/fs",
            "zfs set ployz:promote=on tank/ployz-mirror/data/fs",
            "zfs rename tank/ployz-mirror/data/fs tank/ployz/data",
            "zfs get -H -o value ployz:cut tank/ployz/data",
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:seed tank/ployz/data",
            "zfs get -H -o value ployz:epoch tank/ployz/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn copy_writer_leaves_a_cut_slot_registered_and_drops_the_cut_after_the_rename() {
    let test = TestDir::new();
    let (socket, server) = slot_copy(&test, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 0}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value receive_resume_token tank/ployz-mirror/data/fs",
            "zfs inherit ployz:pulled tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:seed tank/ployz-mirror/data/fs",
            "zfs set ployz:epoch=0 tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:cut tank/ployz-mirror/data/fs",
            "zfs rename tank/ployz-mirror/data/fs tank/ployz/data",
            "zfs get -H -o value ployz:cut tank/ployz/data",
            "zfs inherit ployz:cut tank/ployz/data",
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:epoch tank/ployz/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn copy_writer_raises_a_roots_epoch_before_it_reopens_it() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[("f-1", 13)]);
    set_property(&test, ROOT, "ployz:epoch", "1");
    set_property(&test, ROOT, "ployz:cut", "on");
    fs::write(test.0.join("readonly-volume"), "").unwrap();

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(
        reply.pointer("/Ok/readonly"),
        Some(&json!(false)),
        "{reply}"
    );
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value ployz:epoch tank/ployz/data",
            "zfs set ployz:epoch=2 tank/ployz/data",
            "zfs get -H -o value ployz:cut tank/ployz/data",
            "zfs inherit ployz:cut tank/ployz/data",
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:epoch tank/ployz/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn copy_cut_marks_the_root_cut_before_it_parks_it_as_an_old_copy() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[("w-1-2", 12), ("w-1-1", 11)]);

    let reply = post(
        &socket,
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")), "{reply}");
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs set ployz:cut=on tank/ployz/data",
            "zfs set ployz:role=old_copy tank/ployz-mirror/data",
            "zfs rename tank/ployz/data tank/ployz-mirror/data/fs",
            "zfs get -H -o value receive_resume_token tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:pull tank/ployz-mirror/data",
            "zfs get -H -o value ployz:role tank/ployz-mirror/data",
        ],
    );
    server.abort();
}

/// A pull that lands a snapshot the slot did not hold clears every mark that describes the
/// slot's own past, then records the guid it received. The parent's role and pull target
/// are set before the receive and the pull target is cleared once the slot is pruned.
#[tokio::test]
async fn a_landed_pull_marks_the_slot_in_this_order() {
    let source = Source::start(Some(1_073_741_824), "snapshot w-1-2 12\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    for (mark, value) in [
        ("ployz:cut", "on"),
        ("ployz:epoch", "3"),
        ("ployz:promote", "on"),
        ("ployz:seed", "11"),
    ] {
        set_property(&test, SLOT_FS, mark, value);
    }
    set_property(&test, SLOT, "ployz:role", "old_copy");

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    finished(&socket, "w-1-2").await;

    let writes: Vec<String> = trace(&commands(&test))
        .into_iter()
        .filter(|line| line.starts_with("zfs set ") || line.starts_with("zfs inherit "))
        .collect();
    assert_eq!(
        writes,
        [
            "zfs set ployz:role=mirror tank/ployz-mirror/data",
            "zfs set ployz:pull=w-1-2 tank/ployz-mirror/data",
            "zfs inherit ployz:cut tank/ployz-mirror/data/fs",
            "zfs inherit ployz:epoch tank/ployz-mirror/data/fs",
            "zfs inherit ployz:promote tank/ployz-mirror/data/fs",
            "zfs inherit ployz:seed tank/ployz-mirror/data/fs",
            "zfs set ployz:pulled=12 tank/ployz-mirror/data/fs",
            "zfs inherit ployz:pull tank/ployz-mirror/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_of_the_slots_own_newest_sets_only_the_role_and_clears_the_pull() {
    let source = Source::start(Some(1_073_741_824), "snapshot w-1-1 11\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");
    set_property(&test, SLOT, "ployz:role", "old_copy");
    set_property(&test, SLOT, "ployz:pull", "w-1-1");

    finished(&socket, "w-1-1").await;

    let writes: Vec<String> = trace(&commands(&test))
        .into_iter()
        .filter(|line| line.starts_with("zfs set ") || line.starts_with("zfs inherit "))
        .collect();
    assert_eq!(
        writes,
        [
            "zfs set ployz:role=mirror tank/ployz-mirror/data",
            "zfs inherit ployz:pull tank/ployz-mirror/data",
        ]
    );
    server.abort();
}

fn volume_names(reply: &Value) -> Vec<&str> {
    reply
        .pointer("/Volumes")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|volume| volume.pointer("/Name").and_then(Value::as_str).unwrap())
        .collect()
}

const SEED_POINT: &str = "tank/ployz/data@w-1-1\t77\t1700000000";

fn seed_request() -> Value {
    json!({"name": "data", "into": "seed", "guid": "77"})
}

/// A completed copy in the slot lands as the root: its pulled mark goes, its seed and
/// promote marks are set, and only then does the rename make it the root.
#[tokio::test]
async fn a_landing_seed_marks_the_slot_before_it_renames_it_the_root() {
    let test = TestDir::new();
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );
    set_property(&test, ROOT, "snapshots", SEED_POINT);
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-1-1\t77\t1700000000",
    );
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled", "77");

    let reply = post(&socket, "/Volume.Seed", seed_request()).await;
    assert_eq!(
        reply.pointer("/Ok/copy/kind"),
        Some(&json!("root")),
        "{reply}"
    );
    assert_eq!(
        trace(&commands(&test)).get(..6).unwrap(),
        [
            "zfs get -H -o value ployz:pull tank/ployz-mirror/seed",
            "zfs inherit ployz:pulled tank/ployz-mirror/seed/fs",
            "zfs set ployz:seed=77 tank/ployz-mirror/seed/fs",
            "zfs set ployz:promote=on tank/ployz-mirror/seed/fs",
            "zfs rename tank/ployz-mirror/seed/fs tank/ployz/seed",
            "zfs get -H -o value ployz:promote tank/ployz/seed",
        ],
    );
    server.abort();
}

/// What a departure reads before it destroys or keeps a slot's copy: the four marks one
/// at a time, stopping at the first set, then `ployz:pulled` set on the copy itself.
async fn departure_trace(setup: impl FnOnce(&TestDir)) -> (Vec<String>, String) {
    let test = TestDir::new();
    set_property(
        &test,
        SLOT_FS,
        "snapshots",
        "tank/ployz-mirror/data/fs@w-3-1\t901\t1690000001",
    );
    setup(&test);
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);
    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    server.abort();
    (trace(&commands(&test)), reply.to_string())
}

const KEPT: &str = r#"{"Ok":{"demoted":[],"failed":[],"stuck":[]}}"#;
const DESTROYED: &str = r#"{"Ok":{"demoted":["data"],"failed":[],"stuck":[]}}"#;

#[tokio::test]
async fn a_departure_reads_the_four_marks_in_order_then_pulled_locally() {
    let (trace, reply) = departure_trace(|test| {
        set_property(test, SLOT_FS, "ployz:pulled", "901");
    })
    .await;
    assert_eq!(reply, DESTROYED);
    assert_eq!(
        trace,
        [
            "zfs get -H -o value ployz:seed tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:promote tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:cut tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:epoch tank/ployz-mirror/data/fs",
            "zfs get -H -s local -o value ployz:pulled tank/ployz-mirror/data/fs",
        ]
    );
}

#[tokio::test]
async fn a_departure_stops_reading_marks_at_the_first_one_set() {
    for (mark, reads) in [
        ("ployz:seed", 1),
        ("ployz:promote", 2),
        ("ployz:cut", 3),
        ("ployz:epoch", 4),
    ] {
        let (trace, reply) = departure_trace(|test| {
            set_property(test, SLOT_FS, mark, "1");
            set_property(test, SLOT_FS, "ployz:pulled", "901");
        })
        .await;
        assert_eq!(reply, KEPT, "{mark}");
        let wanted = ["ployz:seed", "ployz:promote", "ployz:cut", "ployz:epoch"];
        assert_eq!(
            trace,
            wanted
                .get(..reads)
                .unwrap()
                .iter()
                .map(|name| format!("zfs get -H -o value {name} tank/ployz-mirror/data/fs"))
                .collect::<Vec<_>>(),
            "{mark}"
        );
    }
}

/// A mark a slot inherits counts as set: the four are read as effective values.
#[tokio::test]
async fn a_departure_reads_an_inherited_mark_as_set() {
    let (trace, reply) = departure_trace(|test| {
        inherit_property(test, SLOT_FS, "ployz:epoch", "3");
        set_property(test, SLOT_FS, "ployz:pulled", "901");
    })
    .await;
    assert_eq!(reply, KEPT);
    assert_eq!(trace.len(), 4, "{trace:?}");
}

/// `ployz:pulled` is read where set on the copy itself, so one inherited from a parent
/// never tells a departure the copy was received from a writer elsewhere.
#[tokio::test]
async fn an_inherited_pulled_mark_is_not_read_as_written_here() {
    let (trace, reply) = departure_trace(|test| {
        inherit_property(test, SLOT_FS, "ployz:pulled", "901");
    })
    .await;
    assert_eq!(
        reply, KEPT,
        "an inherited pulled mark let the departure destroy the slot"
    );
    assert_eq!(
        trace.last().map(String::as_str),
        Some("zfs get -H -s local -o value ployz:pulled tank/ployz-mirror/data/fs"),
    );
    assert!(
        trace
            .iter()
            .all(|line| !line.contains("get -H -o value ployz:pulled")),
        "{trace:?}"
    );
}

#[tokio::test]
async fn a_departure_that_parks_a_root_sets_its_role_before_the_rename() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[("w-3-1", 901)]);

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(reply.to_string(), DESTROYED);
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs set ployz:role=old_copy tank/ployz-mirror/data",
            "zfs rename tank/ployz/data tank/ployz-mirror/data/fs",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn remove_reads_promote_as_an_effective_value() {
    for inherited in [false, true] {
        let test = TestDir::new();
        let (socket, server) = writer_root(&test, &[]);
        if inherited {
            inherit_property(&test, ROOT, "ployz:promote", "on");
        }

        let reply = post(&socket, "/VolumeDriver.Remove", json!({"Name": "data"})).await;
        let log = commands(&test);
        assert_eq!(
            trace(&log),
            ["zfs get -H -o value ployz:promote tank/ployz/data"],
            "{inherited}"
        );
        assert_eq!(
            log.contains("zfs destroy -r tank/ployz/data"),
            !inherited,
            "{reply}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn create_of_an_existing_unregistered_root_clears_its_promote() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[]);
    set_property(&test, ROOT, "ployz:promote", "on");

    let reply = post(
        &socket,
        "/VolumeDriver.Create",
        json!({"Name": "data", "Opts": {"size": "1g"}}),
    )
    .await;
    assert_eq!(reply, json!({"Err": ""}));
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs inherit ployz:promote tank/ployz/data",
        ]
    );
    assert_eq!(property(&test, ROOT, "ployz:promote"), None);
    server.abort();
}

#[tokio::test]
async fn get_hides_a_root_whose_promote_is_set_here_or_inherited() {
    for inherited in [false, true] {
        let test = TestDir::new();
        let (socket, server) = writer_root(&test, &[]);
        if inherited {
            inherit_property(&test, ROOT, "ployz:promote", "on");
        } else {
            set_property(&test, ROOT, "ployz:promote", "on");
        }

        let reply = post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await;
        assert!(reply.get("Volume").is_none(), "{inherited}: {reply}");
        assert_eq!(
            trace(&commands(&test)),
            ["zfs get -H -o value ployz:promote tank/ployz/data"],
            "{inherited}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn get_of_a_cut_volume_reads_cut_on_the_slot_copy() {
    let test = TestDir::new();
    let (socket, server) = slot_copy(&test, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");

    let reply = post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await;
    assert!(reply.get("Volume").is_some(), "{reply}");
    assert_eq!(
        trace(&commands(&test)),
        ["zfs get -H -o value ployz:cut tank/ployz-mirror/data/fs"]
    );
    server.abort();
}

#[tokio::test]
async fn list_reads_promote_once_per_volume_in_listing_order() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "sibling"]);
    set_property(&test, "tank/ployz/sibling", "ployz:promote", "on");

    let reply = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(volume_names(&reply), ["data"], "{reply}");
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:promote tank/ployz/sibling",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn list_hides_a_volume_whose_promote_is_inherited() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "sibling"]);
    inherit_property(&test, "tank/ployz/sibling", "ployz:promote", "on");

    let reply = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(volume_names(&reply), ["data"], "{reply}");
    server.abort();
}

/// A zfs error on one Volume's mark read fails the whole List, and a Volume that vanished
/// between the listing and its read does the same, as ZFS answers a get of a dataset that
/// is gone with an error. The answer is a List error and no Volumes.
#[tokio::test]
async fn list_fails_whole_when_a_volumes_mark_read_fails() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "sibling"]);
    fs::write(
        test.0.join("props/fail-command"),
        "get -H -o value ployz:promote tank/ployz/sibling",
    )
    .unwrap_or_else(|_| {
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(
            test.0.join("props/fail-command"),
            "get -H -o value ployz:promote tank/ployz/sibling",
        )
        .unwrap();
    });

    let reply = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(reply.pointer("/Volumes"), Some(&json!([])), "{reply}");
    assert!(
        reply
            .pointer("/Err")
            .and_then(Value::as_str)
            .unwrap()
            .contains("get -H -o value ployz:promote tank/ployz/sibling failed"),
        "{reply}"
    );
    server.abort();
}

#[tokio::test]
async fn list_fails_whole_when_a_volume_vanishes_after_the_listing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "sibling"]);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/stall-command"),
        "get -H -o value ployz:promote tank/ployz/data",
    )
    .unwrap();

    let list = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/VolumeDriver.List", json!({})).await }
    });
    poll(async || test.0.join("props/stalled").exists().then_some(()))
        .await
        .expect("the first mark read never began");
    fs::remove_file(test.0.join("sibling")).unwrap();
    fs::remove_file(test.0.join("props/stall-command")).unwrap();

    let reply = list.await.unwrap();
    assert_eq!(reply.pointer("/Volumes"), Some(&json!([])), "{reply}");
    assert!(
        reply
            .pointer("/Err")
            .and_then(Value::as_str)
            .unwrap()
            .contains("dataset does not exist"),
        "{reply}"
    );
    server.abort();
}

#[tokio::test]
async fn inspect_reads_the_marks_of_a_read_only_root_in_order() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[("f-1", 13)]);
    fs::write(test.0.join("readonly-volume"), "").unwrap();
    for (mark, value) in [
        ("ployz:promote", "on"),
        ("ployz:seed", "77"),
        ("ployz:cut", "on"),
    ] {
        set_property(&test, ROOT, mark, value);
    }

    let reply = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        reply.pointer("/Ok/copy/mark"),
        Some(&json!("cut")),
        "{reply}"
    );
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:seed tank/ployz/data",
            "zfs get -H -o value ployz:cut tank/ployz/data",
            "zfs get -H -o value ployz:epoch tank/ployz/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn inspect_of_an_unmarked_root_reads_promote_alone() {
    let test = TestDir::new();
    let (socket, server) = writer_root(&test, &[("f-1", 13)]);

    post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value ployz:promote tank/ployz/data",
            "zfs get -H -o value ployz:epoch tank/ployz/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn inspect_of_a_slot_reads_its_role_pull_and_resume_token() {
    let test = TestDir::new();
    let (socket, server) = slot_copy(&test, &[("f-1", 13)]);

    post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        trace(&commands(&test)),
        [
            "zfs get -H -o value receive_resume_token tank/ployz-mirror/data/fs",
            "zfs get -H -o value ployz:pull tank/ployz-mirror/data",
            "zfs get -H -o value ployz:role tank/ployz-mirror/data",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn copy_destroy_reads_promote_then_seed_before_it_destroys_a_root() {
    for (marks, expected) in [
        (
            &[][..],
            &["zfs get -H -o value ployz:promote tank/ployz/data"][..],
        ),
        (
            &[("ployz:promote", "on")][..],
            &[
                "zfs get -H -o value ployz:promote tank/ployz/data",
                "zfs get -H -o value ployz:seed tank/ployz/data",
            ][..],
        ),
        (
            &[("ployz:promote", "on"), ("ployz:seed", "77")][..],
            &[
                "zfs get -H -o value ployz:promote tank/ployz/data",
                "zfs get -H -o value ployz:seed tank/ployz/data",
            ][..],
        ),
    ] {
        let test = TestDir::new();
        let (socket, server) = writer_root(&test, &[("w-1-1", 11)]);
        for (mark, value) in marks {
            set_property(&test, ROOT, mark, value);
        }

        post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await;
        let trace = trace(&commands(&test));
        assert_eq!(trace.get(..expected.len()).unwrap(), expected, "{marks:?}");
        server.abort();
    }
}

#[tokio::test]
async fn a_stale_seed_slot_reads_the_four_marks_before_it_clears() {
    let test = TestDir::new();
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "seed-slot", "seed-fs"],
    );
    set_property(&test, ROOT, "snapshots", SEED_POINT);
    set_property(
        &test,
        "tank/ployz-mirror/seed/fs",
        "snapshots",
        "tank/ployz-mirror/seed/fs@w-0-0\t55\t1690000000",
    );
    set_property(&test, "tank/ployz-mirror/seed/fs", "ployz:pulled", "55");
    inherit_property(&test, "tank/ployz-mirror/seed/fs", "ployz:epoch", "3");

    let reply = post(&socket, "/Volume.Seed", seed_request()).await;
    assert!(reply.get("Err").is_some(), "{reply}");
    let trace = trace(&commands(&test));
    assert_eq!(
        trace.get(..5).unwrap(),
        [
            "zfs get -H -o value ployz:pull tank/ployz-mirror/seed",
            "zfs get -H -o value ployz:seed tank/ployz-mirror/seed/fs",
            "zfs get -H -o value ployz:promote tank/ployz-mirror/seed/fs",
            "zfs get -H -o value ployz:cut tank/ployz-mirror/seed/fs",
            "zfs get -H -o value ployz:epoch tank/ployz-mirror/seed/fs",
        ],
        "{trace:?}"
    );
    server.abort();
}
