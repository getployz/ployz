//! SetVolumeSize over fake ZFS: `data` holds a 1 GiB bound on its root, or a 3 GiB bound
//! on its mirror slot and its landed copy, and grows in place or shrinks when allowed.

use super::copy_tests::{
    SLOT, SLOT_FS, Source, eventually, landed, poll, pull, puller, set_snapshots,
};
use super::first_pool_tests::start_pool_with_data;
use super::inspect_tests::{set_property, start};
use super::*;

const GIB: u64 = 1024 * 1024 * 1024;
const ROOT: &str = "tank/ployz/data";
/// A `tank` twice the size of [`USABLE_POOL`], with room above the 3 GiB slot bound.
const ROOMY_POOL: &str = "tank\t8589934592\t0\t8589934592\tONLINE\toff\n";

/// A mirror slot whose landed copy carries the slot's 3 GiB bound, as a receive or a
/// cut leaves it.
fn mirror(test: &TestDir, pool: &str) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let started = start(test, pool, &["root", "mirror", "mirror-fs"]);
    set_property(test, SLOT_FS, "refquota", "3221225472");
    started
}

fn grow(maximum_bytes: u64) -> Value {
    json!({"name": "data", "maximum_bytes": maximum_bytes})
}

fn shrink(maximum_bytes: u64) -> Value {
    json!({"name": "data", "maximum_bytes": maximum_bytes, "allow_shrink": true})
}

fn refquota_sets(test: &TestDir) -> Vec<String> {
    commands(test)
        .lines()
        .filter(|line| line.starts_with("zfs set refquota="))
        .map(str::to_owned)
        .collect()
}

fn refusal(reply: &Value) -> &str {
    reply
        .pointer("/Err/details/reason")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{reply}"))
}

#[tokio::test]
async fn a_grow_raises_the_bound_and_answers_what_zfs_holds() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let reply = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    let listed = post(&socket, "/VolumeDriver.List", json!({})).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 2 * GIB}}));
    assert_eq!(
        property(&test, "tank/ployz/data", "refquota").as_deref(),
        Some("2147483648")
    );
    assert!(commands(&test).contains("zfs set refquota=2147483648 tank/ployz/data\n"));
    assert_eq!(
        listed.pointer("/Volumes/0/Status/bound_bytes"),
        Some(&json!(2 * GIB))
    );
}

#[tokio::test]
async fn a_grow_on_a_mirror_raises_the_slot_once_and_then_its_copy() {
    let test = TestDir::new();
    let (socket, server) = mirror(&test, ROOMY_POOL);

    let same = post(&socket, "/Volume.Size.Set", grow(3 * GIB)).await;
    assert_eq!(same, json!({"Ok": {"bound_bytes": 3 * GIB}}));
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));

    let reply = post(&socket, "/Volume.Size.Set", grow(4 * GIB)).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 4 * GIB}}));
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("4294967296")
    );
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("4294967296")
    );
    assert_eq!(
        refquota_sets(&test),
        [
            format!("zfs set refquota=4294967296 {SLOT}"),
            format!("zfs set refquota=4294967296 {SLOT_FS}"),
        ]
    );
}

/// A grow that raised the slot and stopped before its copy leaves the copy behind; the
/// resend finds the slot at the request and raises the copy with nothing more to commit.
#[tokio::test]
async fn a_resend_raises_a_copy_a_grow_left_behind_without_committing_again() {
    let test = TestDir::new();
    let (socket, server) = mirror(&test, USABLE_POOL);
    set_property(&test, SLOT, "refquota", "4294967296");

    let reply = post(&socket, "/Volume.Size.Set", grow(4 * GIB)).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 4 * GIB}}));
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("4294967296")
    );
    assert!(
        !commands(&test).contains(&format!("zfs set refquota=4294967296 {SLOT}\n")),
        "{}",
        commands(&test)
    );
}

#[tokio::test]
async fn a_grown_mirror_promoted_to_writer_holds_the_new_bound() {
    let test = TestDir::new();
    let (socket, server) = mirror(&test, ROOMY_POOL);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);

    let grown = post(&socket, "/Volume.Size.Set", grow(4 * GIB)).await;
    assert_eq!(grown, json!({"Ok": {"bound_bytes": 4 * GIB}}));
    let promoted = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "11", "epoch": 1}),
    )
    .await;
    assert_eq!(
        promoted.pointer("/Ok/kind"),
        Some(&json!("root")),
        "{promoted}"
    );

    let again = post(&socket, "/Volume.Size.Set", grow(4 * GIB)).await;
    server.abort();

    assert_eq!(
        property(&test, ROOT, "refquota").as_deref(),
        Some("4294967296")
    );
    assert_eq!(again, json!({"Ok": {"bound_bytes": 4 * GIB}}));
}

/// A pull mark stays on a slot until a later pull settles, and a cut mark on its copy
/// until a pull or a promote clears it; neither says a step is in progress.
#[tokio::test]
async fn a_slot_with_a_leftover_mark_accepts_a_grow() {
    for (dataset, mark, value) in [(SLOT_FS, "ployz:cut", "on"), (SLOT, "ployz:pull", "w-1-1")] {
        let test = TestDir::new();
        let (socket, server) = mirror(&test, ROOMY_POOL);
        set_property(&test, dataset, mark, value);

        let reply = post(&socket, "/Volume.Size.Set", grow(4 * GIB)).await;
        server.abort();

        assert_eq!(reply, json!({"Ok": {"bound_bytes": 4 * GIB}}), "{mark}");
        assert_eq!(
            property(&test, SLOT, "refquota").as_deref(),
            Some("4294967296"),
            "{mark}"
        );
        assert_eq!(
            property(&test, SLOT_FS, "refquota").as_deref(),
            Some("4294967296"),
            "{mark}"
        );
    }
}

#[tokio::test]
async fn a_slot_receiving_is_refused_until_the_receive_lands() {
    let source = Source::start(Some(GIB), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/stall-read"), "").unwrap();
    let started = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(
        started.pointer("/Ok/pull/target"),
        Some(&json!("w-1-1")),
        "{started}"
    );

    let shrink_refused = post(&socket, "/Volume.Size.Set", grow(GIB / 2)).await;
    assert_eq!(refusal(&shrink_refused), "shrink", "{shrink_refused}");
    let shrink_receiving = post(&socket, "/Volume.Size.Set", shrink(GIB / 2)).await;
    assert_eq!(
        refusal(&shrink_receiving),
        "switching",
        "{shrink_receiving}"
    );
    let refused = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    assert_eq!(refusal(&refused), "switching", "{refused}");
    assert_eq!(refused.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    let held = post(&socket, "/Volume.Size.Set", grow(GIB)).await;
    assert_eq!(held, json!({"Ok": {"bound_bytes": GIB}}));
    assert!(
        !commands(&test).contains("zfs set refquota="),
        "{}",
        commands(&test)
    );

    fs::remove_file(test.0.join("props/stall-read")).unwrap();
    landed(&socket, "w-1-1").await;
    let reply = poll(async || {
        let reply = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
        reply.get("Ok").is_some().then_some(reply)
    })
    .await
    .expect("the grow never answered Ok");
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 2 * GIB}}));
    assert_eq!(
        property(&test, SLOT, "ployz:pull").as_deref(),
        Some("w-1-1")
    );
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("2147483648")
    );
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("2147483648")
    );
}

#[tokio::test]
async fn a_bound_already_at_the_request_is_answered_without_a_write() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let reply = post(&socket, "/Volume.Size.Set", grow(GIB)).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": GIB}}));
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

#[tokio::test]
async fn a_smaller_bound_is_refused_with_the_bound_held() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let reply = post(&socket, "/Volume.Size.Set", grow(GIB / 2)).await;
    server.abort();

    assert_eq!(refusal(&reply), "shrink");
    assert_eq!(reply.pointer("/Err/code").unwrap(), "conflict");
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

#[tokio::test]
async fn a_volume_held_by_a_run_is_refused_until_the_run_finishes() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "readonly-volume"]);

    let reply = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    server.abort();

    assert_eq!(refusal(&reply), "switching");
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

#[tokio::test]
async fn an_absent_or_unbounded_volume_is_refused_by_name() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root"]);
    let absent = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    server.abort();
    assert_eq!(refusal(&absent), "not_found");
    assert_eq!(absent.pointer("/Err/code").unwrap(), "not_found");

    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let no_pool = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    server.abort();
    assert_eq!(refusal(&no_pool), "not_found");

    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "unbounded-volume"]);
    let unbounded = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    server.abort();
    assert_eq!(refusal(&unbounded), "not_provisioned");
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

#[tokio::test]
async fn a_grow_the_pool_cannot_hold_is_refused_before_any_write() {
    let test = TestDir::new();
    let (socket, server) = start_pool_with_data(&test).await;
    fs::write(test.0.join("insufficient"), "").unwrap();

    let reply = post(&socket, "/Volume.Size.Set", grow(3 * GIB)).await;
    server.abort();

    assert_eq!(refusal(&reply), "no_capacity");
    assert_eq!(reply.pointer("/Err/code").unwrap(), "conflict");
    assert_eq!(
        reply.pointer("/Err/details/capacity/code").unwrap(),
        "insufficient_storage"
    );
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    assert_eq!(
        fs::read_to_string(test.0.join("volume-bound"))
            .unwrap()
            .trim(),
        "1073741824"
    );
    assert!(
        !commands(&test).contains("zfs set refquota"),
        "{}",
        commands(&test)
    );
}

#[tokio::test]
async fn a_grow_a_fixed_pool_cannot_hold_is_refused_before_any_write() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let reply = post(&socket, "/Volume.Size.Set", grow(7 * GIB / 2)).await;
    server.abort();

    assert_eq!(refusal(&reply), "no_capacity");
    assert_eq!(reply.pointer("/Err/code").unwrap(), "conflict");
    assert_eq!(
        reply.pointer("/Err/details/capacity/code").unwrap(),
        "pool_cannot_grow"
    );
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

/// Wraps `zfs` so a first receive waits before it makes the slot's copy while
/// `<test>/receive-gate` exists, and the Pool listing waits after its read while
/// `<test>/list-gate` exists, touching `<test>/list-reached`.
fn gated_zfs(test: &TestDir, zfs: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let wrapper = test.0.join("gated-zfs");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in\n\
             \t'receive -u -s '*) while [ -e '{root}/receive-gate' ]; do sleep 0.01; done ;;\n\
             \t'list -Hp -o name,refquota,used,usedbydataset,referenced,mountpoint,mounted,readonly -r tank')\n\
             \t\tif [ -e '{root}/list-gate' ]; then\n\
             \t\t\tout=$('{zfs}' \"$@\") || exit $?\n\
             \t\t\ttouch '{root}/list-reached'\n\
             \t\t\twhile [ -e '{root}/list-gate' ]; do sleep 0.01; done\n\
             \t\t\tprintf '%s\\n' \"$out\"\n\
             \t\t\texit 0\n\
             \t\tfi ;;\n\
             esac\n\
             exec '{zfs}' \"$@\"\n",
            root = test.0.display(),
            zfs = zfs.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

#[tokio::test]
async fn a_grow_a_first_receive_lands_during_raises_the_copy_it_landed() {
    let source = Source::start(Some(GIB), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, ROOMY_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, gated_zfs(&test, &zfs));
    storage.send_port = source.port;
    let receives = storage.receives.clone();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    let name: DockerVolumeName = "data".parse().unwrap();

    fs::write(test.0.join("receive-gate"), "").unwrap();
    let started = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(
        started.pointer("/Ok/pull/target"),
        Some(&json!("w-1-1")),
        "{started}"
    );
    assert!(receives.running(&name));

    fs::write(test.0.join("list-gate"), "").unwrap();
    let grow_during = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await }
    });
    eventually("the grow's list of the Pool", || {
        test.0.join("list-reached").exists()
    })
    .await;
    fs::remove_file(test.0.join("receive-gate")).unwrap();
    eventually("the receive's finish", || !receives.running(&name)).await;
    fs::remove_file(test.0.join("list-gate")).unwrap();
    let reply = grow_during.await.unwrap();
    server.abort();

    assert_eq!(
        reply,
        json!({"Ok": {"bound_bytes": 2 * GIB}}),
        "{}",
        commands(&test)
    );
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("2147483648")
    );
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("2147483648")
    );
}

/// `data` holds 2 GiB beside a 1 GiB `sibling` on the fixed 4 GiB Pool, which leaves no
/// room for the sibling to grow until `data` gives some back.
#[tokio::test]
async fn a_shrink_lowers_the_bound_and_releases_it_to_the_pool() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "sibling"]);
    set_property(&test, ROOT, "refquota", "2147483648");
    set_property(&test, ROOT, "referenced", "0");
    let grow_sibling = json!({"name": "sibling", "maximum_bytes": 2 * GIB});

    let full = post(&socket, "/Volume.Size.Set", grow_sibling.clone()).await;
    assert_eq!(refusal(&full), "no_capacity", "{full}");

    let reply = post(&socket, "/Volume.Size.Set", shrink(GIB)).await;
    assert_eq!(reply, json!({"Ok": {"bound_bytes": GIB}}));
    assert_eq!(
        property(&test, ROOT, "refquota").as_deref(),
        Some("1073741824")
    );

    let grown = post(&socket, "/Volume.Size.Set", grow_sibling).await;
    server.abort();
    assert_eq!(grown, json!({"Ok": {"bound_bytes": 2 * GIB}}));
}

#[tokio::test]
async fn a_shrink_below_the_floor_is_refused_with_the_floor_and_the_bound_held() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let reply = post(&socket, "/Volume.Size.Set", shrink(GIB / 2)).await;
    server.abort();

    assert_eq!(refusal(&reply), "below_floor", "{reply}");
    assert_eq!(reply.pointer("/Err/code").unwrap(), "conflict");
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    assert_eq!(
        reply.pointer("/Err/details/floor_bytes").unwrap(),
        966_367_642 + 1_000_000_000u64
    );
    assert!(!commands(&test).contains("zfs set "), "{}", commands(&test));
}

#[tokio::test]
async fn a_shrink_of_a_root_held_by_a_run_is_refused_and_writes_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume", "readonly-volume"]);

    let reply = post(&socket, "/Volume.Size.Set", shrink(GIB / 2)).await;
    server.abort();

    assert_eq!(refusal(&reply), "switching", "{reply}");
    assert_eq!(reply.pointer("/Err/details/bound_bytes").unwrap(), GIB);
    let log = commands(&test);
    assert!(!log.contains("zfs set "), "{log}");
}

#[tokio::test]
async fn a_shrink_on_a_mirror_lowers_the_copy_then_the_slot_and_a_resend_writes_nothing() {
    let test = TestDir::new();
    let (socket, server) = mirror(&test, USABLE_POOL);

    let reply = post(&socket, "/Volume.Size.Set", shrink(2 * GIB)).await;
    assert_eq!(reply, json!({"Ok": {"bound_bytes": 2 * GIB}}));
    let lowered = [
        format!("zfs set refquota=2147483648 {SLOT_FS}"),
        format!("zfs set refquota=2147483648 {SLOT}"),
    ];
    assert_eq!(refquota_sets(&test), lowered);

    let again = post(&socket, "/Volume.Size.Set", shrink(2 * GIB)).await;
    server.abort();

    assert_eq!(again, json!({"Ok": {"bound_bytes": 2 * GIB}}));
    assert_eq!(refquota_sets(&test), lowered);
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("2147483648")
    );
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("2147483648")
    );
}

#[tokio::test]
async fn a_mirror_shrink_takes_its_floor_from_what_the_copy_references() {
    let test = TestDir::new();
    let (socket, server) = mirror(&test, USABLE_POOL);
    set_property(&test, SLOT_FS, "referenced", "1900000000");

    let below = post(&socket, "/Volume.Size.Set", shrink(2 * GIB)).await;
    assert_eq!(refusal(&below), "below_floor", "{below}");
    assert_eq!(
        below.pointer("/Err/details/floor_bytes").unwrap(),
        2_900_000_000u64
    );
    assert!(refquota_sets(&test).is_empty());

    let reply = post(&socket, "/Volume.Size.Set", shrink(3_000_000_000)).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 3_000_000_000u64}}));
    assert_eq!(
        property(&test, SLOT_FS, "refquota").as_deref(),
        Some("3000000000")
    );
}

#[tokio::test]
async fn a_slot_with_no_copy_shrinks_to_the_floor_of_an_empty_volume() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "mirror"]);

    let below = post(&socket, "/Volume.Size.Set", shrink(999_999_999)).await;
    assert_eq!(refusal(&below), "below_floor", "{below}");
    assert_eq!(
        below.pointer("/Err/details/floor_bytes").unwrap(),
        1_000_000_000u64
    );

    let reply = post(&socket, "/Volume.Size.Set", shrink(1_000_000_000)).await;
    server.abort();

    assert_eq!(reply, json!({"Ok": {"bound_bytes": 1_000_000_000u64}}));
    assert_eq!(
        refquota_sets(&test),
        [format!("zfs set refquota=1000000000 {SLOT}")]
    );
}

/// ZFS refuses a `refquota` below what the dataset references, as when a writer races
/// past the floor between the read and the write.
#[tokio::test]
async fn a_shrink_zfs_refuses_is_an_error_and_leaves_the_bound_held() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);
    set_property(&test, ROOT, "refquota", "2147483648");
    set_property(&test, ROOT, "referenced", "0");
    fs::write(
        test.0.join("props/fail-command"),
        "set refquota=1073741824 tank/ployz/data",
    )
    .unwrap();

    let reply = post(&socket, "/Volume.Size.Set", shrink(GIB)).await;
    let held = post(&socket, "/Volume.Size.Set", grow(2 * GIB)).await;
    server.abort();

    assert_eq!(reply.pointer("/Err/code").unwrap(), "internal", "{reply}");
    assert_eq!(held, json!({"Ok": {"bound_bytes": 2 * GIB}}));
    assert_eq!(
        property(&test, ROOT, "refquota").as_deref(),
        Some("2147483648")
    );
}
