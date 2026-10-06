//! The mirror verbs over fake ZFS: what each one changes, refuses and leaves alone.

use super::fake_zfs::SLOT_BOUND_BYTES;
use super::lease_tests::{set_property, start};
use super::*;

const FAR_FUTURE: i64 = 4_102_444_800;

/// A request at `lease`, step `seq.round.sub`, with the verb's own fields merged in.
pub(super) fn at(lease: u64, seq: u16, round: u32, sub: u8, fields: Value) -> Value {
    let mut request = json!({
        "switch": {
            "lease": lease,
            "pos": {"seq": seq, "round": round, "sub": sub},
            "not_after_unix_seconds": FAR_FUTURE,
        },
        "name": "data",
    });
    for (key, value) in fields.as_object().unwrap() {
        request
            .as_object_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    request
}

pub(super) fn commands(test: &TestDir) -> String {
    fs::read_to_string(test.0.join("commands")).unwrap_or_default()
}

pub(super) fn property(test: &TestDir, dataset: &str, property: &str) -> Option<String> {
    fs::read_to_string(test.0.join("props").join(dataset).join(property))
        .ok()
        .map(|value| value.trim_end().to_owned())
}

pub(super) fn snapshot_names(test: &TestDir, dataset: &str) -> Vec<String> {
    property(test, dataset, "snapshots")
        .unwrap_or_default()
        .lines()
        .map(|line| line.split(['@', '\t']).nth(1).unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn declare_mirror_creates_a_read_only_bounded_slot_parent() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root"]);

    let request = at(1, 3, 0, 0, json!({"refquota_bytes": SLOT_BOUND_BYTES}));
    let response = post(&socket, "/Volume.DeclareMirror", request).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "decision": "adopt",
            "lease": {"lease": 1, "pos": {"seq": 3, "round": 0, "sub": 0}, "cycle": "closed"},
            "copy": {
                "kind": "slot",
                "mirror": {"phase": "idle"},
                "readonly": true,
                "newest": null,
                "resume_token": null,
            },
        }})
    );
    let log = commands(&test);
    assert!(log.contains(
        "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror\n"
    ), "{log}");
    assert!(
        log.contains(&format!(
            "zfs create -o canmount=off -o readonly=on -o refquota={SLOT_BOUND_BYTES} tank/ployz-mirror/data\n"
        )),
        "{log}"
    );
    assert!(
        log.contains("zfs set ployz:mirror=idle tank/ployz-mirror/data\n"),
        "{log}"
    );
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("1:3.0.0:closed")
    );
    server.abort();
}

#[tokio::test]
async fn declare_mirror_refuses_a_declared_slot_unless_replayed() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:3.0.0:closed");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror"]);

    let replay = at(1, 3, 0, 0, json!({"refquota_bytes": SLOT_BOUND_BYTES}));
    let response = post(&socket, "/Volume.DeclareMirror", replay).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "replay");
    assert_eq!(response.pointer("/Ok/copy/kind").unwrap(), "slot");
    assert!(!commands(&test).contains("zfs create"));

    let again = at(1, 4, 0, 0, json!({"refquota_bytes": SLOT_BOUND_BYTES}));
    let response = post(&socket, "/Volume.DeclareMirror", again).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition"
    );
    assert!(!commands(&test).contains("zfs create"));
    server.abort();
}

#[tokio::test]
async fn declare_mirror_refuses_the_writer_machine_and_a_full_pool() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let request = at(1, 3, 0, 0, json!({"refquota_bytes": SLOT_BOUND_BYTES}));
    let response = post(&socket, "/Volume.DeclareMirror", request).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition"
    );
    assert!(!commands(&test).contains("zfs create"));
    server.abort();

    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "sibling"]);
    let request = at(1, 3, 0, 0, json!({"refquota_bytes": 3_500_000_000_u64}));
    let response = post(&socket, "/Volume.DeclareMirror", request).await;
    let message = response.pointer("/Err/message").unwrap().as_str().unwrap();
    assert!(message.contains("automatic growth"), "{message}");
    assert!(!commands(&test).contains("zfs create"));
    server.abort();
}

#[tokio::test]
async fn begin_round_aborts_a_partial_receive_and_clears_the_record() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:4.0.5:closed");
    set_property(&test, "tank/ployz-mirror/data", "ployz:mirror", "final:9");
    set_property(
        &test,
        "tank/ployz-mirror/data",
        "ployz:receive",
        "1:4.0.3:w-1-2",
    );
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "receive_resume_token",
        "token-1",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

    let response = post(&socket, "/Volume.BeginRound", at(1, 4, 1, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "admit");
    assert_eq!(
        response.pointer("/Ok/copy").unwrap(),
        &json!({
            "kind": "slot",
            "mirror": {"phase": "idle"},
            "readonly": true,
            "newest": null,
            "resume_token": null,
        })
    );
    let log = commands(&test);
    assert!(
        log.contains("zfs receive -A tank/ployz-mirror/data/fs\n"),
        "{log}"
    );
    assert!(
        log.contains("zfs inherit ployz:receive tank/ployz-mirror/data\n"),
        "{log}"
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/data", "ployz:receive"),
        None
    );
    server.abort();
}

#[tokio::test]
async fn begin_round_leaves_a_clean_slot_alone() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);
    let response = post(&socket, "/Volume.BeginRound", at(1, 4, 1, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "adopt");
    let log = commands(&test);
    assert!(!log.contains("zfs receive"), "{log}");
    assert!(!log.contains("zfs inherit"), "{log}");
    assert!(!log.contains("ployz:mirror="), "{log}");
    server.abort();
}

#[tokio::test]
async fn begin_round_needs_a_declared_slot() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let response = post(&socket, "/Volume.BeginRound", at(1, 4, 1, 0, json!({}))).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition"
    );
    server.abort();
}

#[tokio::test]
async fn a_refused_verb_leaves_the_record_and_its_retry_is_admitted() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:4.0.5:closed");
    let (socket, server) = start(&test, USABLE_POOL, &["root"]);
    for refused in [at(1, 4, 1, 0, json!({})), at(2, 4, 1, 0, json!({}))] {
        let response = post(&socket, "/Volume.BeginRound", refused).await;
        assert_eq!(
            response.pointer("/Err/details/reason").unwrap(),
            "precondition"
        );
        assert_eq!(
            property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
            Some("1:4.0.5:closed")
        );
    }

    fs::write(test.0.join("mirror"), "").unwrap();
    let response = post(&socket, "/Volume.BeginRound", at(1, 4, 1, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "admit");
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("1:4.1.0:closed")
    );
    server.abort();
}

#[tokio::test]
async fn commit_keeps_the_mirror_newest_and_destroys_older_run_snapshots() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-3\t3\t1700000003\ntank/ployz/data@w-1-2\t2\t1700000002\ntank/ployz/data@w-1-1\t1\t1700000001\ntank/ployz/data@manual\t9\t1600000000",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let request = at(1, 4, 1, 1, json!({"mirror_newest": 2}));
    let response = post(&socket, "/Volume.CommitSnapshots", request).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "adopt");
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-3");
    assert_eq!(
        snapshot_names(&test, "tank/ployz/data"),
        ["w-1-3", "w-1-2", "manual"]
    );
    let log = commands(&test);
    assert!(log.contains("zfs destroy tank/ployz/data@w-1-1\n"), "{log}");
    assert!(!log.contains("zfs destroy tank/ployz/data@w-1-2"), "{log}");
    assert!(!log.contains("zfs destroy tank/ployz/data@w-1-3"), "{log}");
    server.abort();
}

#[tokio::test]
async fn commit_refuses_a_newest_the_writer_does_not_hold() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-2\t2\t1700000002\ntank/ployz/data@w-1-1\t1\t1700000001",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let request = at(1, 4, 1, 1, json!({"mirror_newest": 77}));
    let response = post(&socket, "/Volume.CommitSnapshots", request).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition"
    );
    assert!(!commands(&test).contains("zfs destroy"));
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-2", "w-1-1"]);
    server.abort();
}

#[tokio::test]
async fn warm_snapshots_the_root_only_when_it_changed() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.WarmSnapshot", at(1, 4, 1, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-1");
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-1"]);

    let response = post(&socket, "/Volume.WarmSnapshot", at(1, 4, 2, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-2");
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-2", "w-1-1"]);

    set_property(&test, "tank/ployz/data", "written@w-1-2", "0");
    let response = post(&socket, "/Volume.WarmSnapshot", at(1, 4, 3, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-2");
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-2", "w-1-1"]);
    assert!(commands(&test).contains("zfs get -Hp -o value written@w-1-2 tank/ployz/data\n"));
    server.abort();
}

#[tokio::test]
async fn warm_replayed_answers_with_the_snapshot_it_took() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    post(&socket, "/Volume.WarmSnapshot", at(1, 4, 1, 2, json!({}))).await;
    let response = post(&socket, "/Volume.WarmSnapshot", at(1, 4, 1, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "replay");
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-1");
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-1"]);
    server.abort();
}

#[tokio::test]
async fn warm_replayed_after_dying_before_its_snapshot_takes_it() {
    let test = TestDir::new();
    // Round 1 took w-1-1; round 2 recorded 4.2.2 and the plugin died before `zfs snapshot`.
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-1\t1\t1700000001",
    );
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:4.2.2:closed");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.WarmSnapshot", at(1, 4, 2, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "replay");
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-1-2");
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["w-1-2", "w-1-1"]);
    server.abort();
}

#[tokio::test]
async fn warm_numbers_snapshots_within_its_own_lease() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@w-1-7\t7\t1700000007",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    let response = post(&socket, "/Volume.WarmSnapshot", at(2, 4, 1, 2, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy/newest/name").unwrap(), "w-2-1");
    server.abort();
}

#[tokio::test]
async fn prune_keeps_the_newest_and_the_receive_in_flight() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz-mirror/data",
        "ployz:receive",
        "1:4.1.3:w-1-2",
    );
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "snapshots",
        "tank/ployz-mirror/data/fs@w-1-3\t3\t1700000003\ntank/ployz-mirror/data/fs@w-1-2\t2\t1700000002\ntank/ployz-mirror/data/fs@w-1-1\t1\t1700000001",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

    let response = post(&socket, "/Volume.PruneMirror", at(1, 4, 1, 4, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy/newest/guid").unwrap(), 3);
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/data/fs"),
        ["w-1-3", "w-1-2"]
    );
    server.abort();
}

#[tokio::test]
async fn prune_without_a_receive_keeps_only_the_newest() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "snapshots",
        "tank/ployz-mirror/data/fs@w-1-3\t3\t1700000003\ntank/ployz-mirror/data/fs@w-1-2\t2\t1700000002",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);
    post(&socket, "/Volume.PruneMirror", at(1, 4, 1, 4, json!({}))).await;
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/data/fs"),
        ["w-1-3"]
    );
    server.abort();
}

#[tokio::test]
async fn destroy_removes_the_slot_and_is_idempotent() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz-mirror/data", "ployz:mirror", "final:9");
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);

    let response = post(&socket, "/Volume.DestroyMirror", at(1, 5, 0, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy").unwrap(), &Value::Null);
    assert!(commands(&test).contains("zfs destroy -r tank/ployz-mirror/data\n"));
    assert!(!test.0.join("mirror").exists());

    let response = post(&socket, "/Volume.DestroyMirror", at(1, 6, 0, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy").unwrap(), &Value::Null);
    assert_eq!(commands(&test).matches("zfs destroy -r").count(), 1);
    server.abort();
}

#[tokio::test]
async fn forget_destroys_every_run_snapshot_and_nothing_else() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@f-1\t5\t1700000005\ntank/ployz/data@w-1-2\t2\t1700000002\ntank/ployz/data@manual\t9\t1600000000",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(
        &socket,
        "/Volume.ForgetSnapshots",
        at(1, 6, 0, 0, json!({})),
    )
    .await;
    assert_eq!(response.pointer("/Ok/copy/newest").unwrap(), &Value::Null);
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["manual"]);
    server.abort();
}

#[tokio::test]
async fn every_mirror_verb_is_fenced() {
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "5:2.0.0:closed");
    set_property(
        &test,
        "tank/ployz-mirror/data",
        "ployz:receive",
        "5:2.0.3:w-5-1",
    );
    let (socket, server) = start(
        &test,
        USABLE_POOL,
        &["root", "volume", "mirror", "mirror-fs"],
    );

    for (route, fields) in [
        ("/Volume.DeclareMirror", json!({"refquota_bytes": 1})),
        ("/Volume.BeginRound", json!({})),
        ("/Volume.CommitSnapshots", json!({"mirror_newest": 1})),
        ("/Volume.WarmSnapshot", json!({})),
        ("/Volume.PruneMirror", json!({})),
        ("/Volume.DestroyMirror", json!({})),
        ("/Volume.ForgetSnapshots", json!({})),
    ] {
        let response = post(&socket, route, at(4, 9, 9, 9, fields)).await;
        assert_eq!(
            response.pointer("/Err/details/reason").unwrap(),
            "stale_lease",
            "{route}: {response}"
        );
    }
    let log = commands(&test);
    for effect in [
        "zfs create",
        "zfs destroy",
        "zfs snapshot",
        "zfs set",
        "zfs inherit",
    ] {
        assert!(
            !log.contains(effect),
            "{effect} ran at a stale lease:\n{log}"
        );
    }
    server.abort();
}
