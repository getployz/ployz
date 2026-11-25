//! The Trash: a delete moves the data, a restore puts it back only where nothing else
//! claims it, and the sweep destroys an entry only after 168 hours the Server counted.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use super::super::holders::{Holder, Holders};
use super::copy_tests::BORN;
use super::inspect_tests::set_property;
use super::*;

const ROOT: &str = "tank/ployz/data";
const HOUR_MS: u64 = 3_600_000;
/// The clock the tests start at: 2023-11-14.
const START_MS: u64 = 1_700_000_000_000;

struct Plugin {
    socket: PathBuf,
    clock: Arc<AtomicU64>,
    server: tokio::task::JoinHandle<io::Result<()>>,
}

impl Plugin {
    /// A plugin over fake ZFS with `markers`, Docker listing `holders` for every name.
    fn start(
        test: &TestDir,
        markers: &[&str],
        holders: std::result::Result<Vec<Holder>, String>,
    ) -> Self {
        for marker in markers {
            fs::write(test.0.join(marker), "").unwrap();
        }
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let mut storage = VolumeStorage::with_programs(zpool, zfs);
        let listed = Holders::none();
        listed.set(holders);
        storage.holders = listed;
        let clock = Arc::new(AtomicU64::new(START_MS));
        storage.now = {
            let clock = Arc::clone(&clock);
            Arc::new(move || {
                ployz_core::authority::Timestamp::from_millis(clock.load(Ordering::SeqCst)).unwrap()
            })
        };
        let socket = test.0.join("plugin.sock");
        let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
        Self {
            socket,
            clock,
            server,
        }
    }

    async fn post(&self, route: &str, body: Value) -> Value {
        post(&self.socket, route, body).await
    }

    async fn create(&self, origin: &str) -> Value {
        self.post(
            "/VolumeDriver.Create",
            json!({"Name": "data", "Opts": {"size": "1g", ployz_core::PROVISIONED_ORIGIN_OPTION: origin}}),
        )
        .await
    }

    /// A restore as the CLI runs it: a root entry is pinned, then promoted.
    async fn restore(&self, entry: &str) -> Value {
        let pinned = self.call(entry, false).await;
        if pinned.pointer("/Ok/state/kind") == Some(&json!("pinned")) {
            return self.call(entry, true).await;
        }
        pinned
    }

    async fn call(&self, entry: &str, promote: bool) -> Value {
        let id = entry.rsplit('/').next().unwrap();
        self.post(
            "/Ployz.Trash.Restore",
            json!({"id": id, "promote": promote}),
        )
        .await
    }

    async fn sweep(&self) -> Value {
        self.post("/Ployz.Trash.Sweep", json!(null)).await
    }

    fn later(&self, hours: u64) {
        self.clock.fetch_add(hours * HOUR_MS, Ordering::SeqCst);
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A plugin holding a root of `data` born as [`BORN`], which a Remove has moved to the
/// Trash; returns the plugin and the entry.
async fn deleted_root(test: &TestDir) -> (Plugin, String) {
    let plugin = Plugin::start(test, &["root", "volume"], Ok(vec![]));
    set_property(test, ROOT, "ployz:origin", BORN);
    let removed = plugin
        .post("/VolumeDriver.Remove", json!({"Name": "data"}))
        .await;
    assert_eq!(removed, json!({"Err": ""}));
    let entry = trashed_one(test, "root");
    (plugin, entry)
}

fn refusal(reply: &Value) -> String {
    reply
        .pointer("/Err/message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a refusal: {reply}"))
        .to_owned()
}

#[tokio::test]
async fn the_trash_lists_what_a_remove_moved_with_its_name_origin_and_expiry() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;

    let listed = plugin.post("/Ployz.Trash.List", json!(null)).await;

    let id = entry.rsplit('/').next().unwrap();
    assert_eq!(
        listed.pointer("/Ok/entries/0/id"),
        Some(&json!(id)),
        "{listed}"
    );
    assert_eq!(listed.pointer("/Ok/entries/0/name"), Some(&json!("data")));
    assert_eq!(listed.pointer("/Ok/entries/0/place"), Some(&json!("root")));
    assert_eq!(listed.pointer("/Ok/entries/0/origin"), Some(&json!(BORN)));
    assert_eq!(
        listed.pointer("/Ok/entries/0/expires_at"),
        Some(&json!(START_MS / 1000 + 168 * 3600))
    );
}

#[tokio::test]
async fn a_restored_root_stays_hidden_until_a_create_of_its_own_data_registers_it() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;

    assert_eq!(
        plugin.restore(&entry).await.pointer("/Ok/place"),
        Some(&json!("root"))
    );
    assert!(test.0.join("volume").exists());
    assert_eq!(trashed(&test), Vec::<String>::new());
    assert_eq!(property(&test, ROOT, "ployz:trash-name"), None);
    assert_eq!(property(&test, ROOT, "ployz:origin").as_deref(), Some(BORN));
    let get = plugin
        .post("/VolumeDriver.Get", json!({"Name": "data"}))
        .await;
    assert!(get.get("Volume").is_none(), "{get}");
    let list = plugin.post("/VolumeDriver.List", json!({})).await;
    assert_eq!(list.pointer("/Volumes"), Some(&json!([])), "{list}");

    let other = plugin.create(&"1".repeat(64)).await;
    assert!(error(&other).contains("restored from the Trash"), "{other}");
    assert!(
        plugin
            .post("/VolumeDriver.Get", json!({"Name": "data"}))
            .await
            .get("Volume")
            .is_none()
    );

    assert_eq!(plugin.create(BORN).await, json!({"Err": ""}));
    assert_eq!(property(&test, ROOT, "ployz:restored"), None);
    let get = plugin
        .post("/VolumeDriver.Get", json!({"Name": "data"}))
        .await;
    assert_eq!(get.pointer("/Volume/Name"), Some(&json!("data")), "{get}");
}

#[tokio::test]
async fn a_restore_refuses_a_name_that_has_a_copy_again() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    let fresh = "1".repeat(64);
    assert_eq!(plugin.create(&fresh).await, json!({"Err": ""}));

    let reply = plugin.restore(&entry).await;

    assert!(refusal(&reply).contains("already has a copy"), "{reply}");
    assert_eq!(trashed(&test), [entry]);
    assert_eq!(property(&test, ROOT, "ployz:origin"), Some(fresh));
}

#[tokio::test]
async fn a_restore_refuses_under_a_container_expecting_other_data_or_when_docker_is_silent() {
    for (holders, refused) in [
        (
            Ok(vec![Holder {
                origins: Some(format!(r#"{{"data":"{}"}}"#, "1".repeat(64))),
            }]),
            "data",
        ),
        (Err("Docker is not answering".to_owned()), "Docker"),
    ] {
        let test = test_dir();
        let plugin = Plugin::start(&test, &["root"], holders);
        let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
        set_property(&test, &entry, "ployz:origin", BORN);

        let reply = plugin.restore(&entry).await;

        assert!(refusal(&reply).contains(refused), "{reply}");
        assert!(!test.0.join("volume").exists());
        assert_eq!(trashed(&test), [entry]);
    }
}

#[tokio::test]
async fn a_restore_refuses_a_copy_the_pool_cannot_hold() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    set_property(&test, &entry, "refquota", &(8u64 << 30).to_string());

    let reply = plugin.restore(&entry).await;

    assert!(!test.0.join("volume").exists(), "{reply}");
    assert!(reply.get("Err").is_some(), "{reply}");
    assert_eq!(trashed(&test), [entry]);
}

/// A slot whose parent still carries its Copy step is a copy a restore was making: its
/// step restarts on its own, past its window, so the Trash never puts it back.
#[tokio::test]
async fn a_restore_refuses_the_slot_of_a_copy_that_never_landed() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "slot", 0, true);
    set_property(&test, &entry, "ployz:copy-step", &"7".repeat(64));

    let reply = plugin.restore(&entry).await;

    assert!(
        refusal(&reply).contains("approve that restore again"),
        "{reply}"
    );
    assert!(!test.0.join("mirror").exists());
    assert_eq!(trashed(&test), [entry]);
}

/// An old copy of a Volume a restore made: its origin rides on the slot's record, not the
/// copy, so a Container expecting that origin lets it back as an old copy.
#[tokio::test]
async fn an_old_copy_of_a_seed_born_volume_restores_as_an_old_copy() {
    let test = test_dir();
    let plugin = Plugin::start(
        &test,
        &["root"],
        Ok(vec![Holder {
            origins: Some(format!(r#"{{"data":"{BORN}"}}"#)),
        }]),
    );
    let entry = plant_trash(&test, &trash_id(1), "data", "slot", 0, true);
    set_property(&test, &entry, "ployz:pull-origin", BORN);
    set_property(
        &test,
        &format!("{entry}/fs"),
        "snapshots",
        &format!("{entry}/fs@w-1-1\t11\t1690000000"),
    );

    let reply = plugin.restore(&entry).await;

    assert_eq!(reply.pointer("/Ok/place"), Some(&json!("slot")), "{reply}");
    assert!(test.0.join("mirror").exists() && test.0.join("mirror-fs").exists());
    assert_eq!(
        property(&test, "tank/ployz-mirror/data", "ployz:role").as_deref(),
        Some("old_copy")
    );
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/data/fs"),
        ["w-1-1"]
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/data", "ployz:trash-place"),
        None
    );
    assert_eq!(trashed(&test), Vec::<String>::new());
}

/// A delete stopped before its rename leaves the Volume where it was and mountable, with
/// only inert marks on it, and a delete after takes it whole.
#[tokio::test]
async fn a_delete_stopped_before_its_rename_leaves_a_working_volume() {
    let steps = [
        "set ployz:trash-name=data tank/ployz/data",
        "set ployz:trash-place=root tank/ployz/data",
        "set ployz:trashed-hour=0 tank/ployz/data",
        "rename",
    ];
    for step in steps {
        let test = test_dir();
        let plugin = Plugin::start(&test, &["root", "volume"], Ok(vec![]));
        if step == "rename" {
            fs::write(test.0.join("rename-fails"), "").unwrap();
        } else {
            set_property(&test, "", "fail-command", step);
        }

        let removed = plugin
            .post("/VolumeDriver.Remove", json!({"Name": "data"}))
            .await;

        assert!(!error(&removed).is_empty(), "{step}: {removed}");
        assert!(test.0.join("volume").exists(), "{step}");
        assert_eq!(trashed(&test), Vec::<String>::new(), "{step}");
        assert_eq!(property(&test, ROOT, "canmount"), None, "{step}");
        let mounted = plugin
            .post("/VolumeDriver.Mount", json!({"Name": "data", "ID": "c"}))
            .await;
        assert_eq!(error(&mounted), "", "{step}: {mounted}");

        let _ = fs::remove_file(test.0.join("rename-fails"));
        plugin
            .post("/VolumeDriver.Unmount", json!({"Name": "data", "ID": "c"}))
            .await;
        let removed = plugin
            .post("/VolumeDriver.Remove", json!({"Name": "data"}))
            .await;
        assert_eq!(removed, json!({"Err": ""}), "{step}");
        trashed_one(&test, "root");
    }
}

/// A Pool with no space left refuses a rename though it takes a destroy, so a delete gives
/// up the Trash's slack to make room, and a sweep with room takes it back.
#[tokio::test]
async fn a_delete_on_a_full_pool_moves_the_volume_with_the_trash_slack() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root", "volume"], Ok(vec![]));
    plugin.sweep().await;
    assert_eq!(
        property(&test, "tank/ployz-trash", "refreservation").as_deref(),
        Some("1M")
    );
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/pool-full"), "").unwrap();

    let removed = plugin
        .post("/VolumeDriver.Remove", json!({"Name": "data"}))
        .await;

    assert_eq!(removed, json!({"Err": ""}));
    trashed_one(&test, "root");
    assert_eq!(
        property(&test, "tank/ployz-trash", "refreservation").as_deref(),
        Some("none")
    );
    plugin.later(1);
    plugin.sweep().await;
    assert_eq!(
        property(&test, "tank/ployz-trash", "refreservation").as_deref(),
        Some("none")
    );
    fs::remove_file(test.0.join("props/pool-full")).unwrap();
    plugin.later(1);
    plugin.sweep().await;
    assert_eq!(
        property(&test, "tank/ployz-trash", "refreservation").as_deref(),
        Some("1M")
    );
}

/// The sweep counts one hour per hour of wall time it saw pass, and destroys an entry
/// once 168 of them passed since its delete.
#[tokio::test]
async fn the_sweep_keeps_an_entry_167_hours_and_destroys_it_at_168() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    let first = plugin.sweep().await;
    assert_eq!(
        first.pointer("/Ok/advanced"),
        Some(&json!(false)),
        "{first}"
    );
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "166");

    plugin.later(1);
    let kept = plugin.sweep().await;
    assert_eq!(kept.pointer("/Ok/hours"), Some(&json!(167)), "{kept}");
    assert_eq!(trashed(&test).as_slice(), std::slice::from_ref(&entry));

    plugin.later(1);
    let swept = plugin.sweep().await;
    assert_eq!(swept.pointer("/Ok/hours"), Some(&json!(168)), "{swept}");
    let id = entry.rsplit('/').next().unwrap();
    assert_eq!(swept.pointer("/Ok/destroyed"), Some(&json!([id])));
    assert_eq!(trashed(&test), Vec::<String>::new());
}

/// A clock that jumps a year ahead adds one hour, and one that steps back adds none: 167
/// hours after a delete, nothing goes.
#[tokio::test]
async fn a_clock_jump_adds_at_most_one_sweep_hour() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    plugin.sweep().await;
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "166");

    plugin.later(24 * 365);
    let ahead = plugin.sweep().await;
    assert_eq!(ahead.pointer("/Ok/hours"), Some(&json!(167)), "{ahead}");
    plugin.later(1);
    plugin.clock.fetch_sub(48 * HOUR_MS, Ordering::SeqCst);
    let back = plugin.sweep().await;
    assert_eq!(back.pointer("/Ok/hours"), Some(&json!(167)), "{back}");

    assert_eq!(trashed(&test), [entry]);
}

#[tokio::test]
async fn the_sweep_never_destroys_an_entry_whose_marks_do_not_read() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let markless = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    fs::remove_file(
        test.0
            .join("props")
            .join(&markless)
            .join("ployz:trashed-hour"),
    )
    .unwrap();
    let unreadable = plant_trash(&test, &trash_id(2), "data", "root", 0, false);
    set_property(&test, &unreadable, "ployz:trash-place", "elsewhere");
    plugin.sweep().await;
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "5000");
    plugin.later(1);

    let swept = plugin.sweep().await;

    assert_eq!(swept.pointer("/Ok/destroyed"), Some(&json!([])), "{swept}");
    assert_eq!(trashed(&test).len(), 2);
    let listed = plugin.post("/Ployz.Trash.List", json!(null)).await;
    assert_eq!(
        listed
            .pointer("/Ok/unreadable")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2),
        "{listed}"
    );
}

#[tokio::test]
async fn empty_destroys_the_entries_it_names_and_keeps_one_whose_marks_do_not_read() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let named = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    let kept = plant_trash(&test, &trash_id(2), "logs", "root", 0, false);
    let unreadable = plant_trash(&test, &trash_id(3), "data", "root", 0, false);
    fs::remove_file(
        test.0
            .join("props")
            .join(&unreadable)
            .join("ployz:trash-name"),
    )
    .unwrap();

    let reply = plugin
        .post(
            "/Ployz.Trash.Empty",
            json!({"ids": [trash_id(1), trash_id(3), trash_id(4)]}),
        )
        .await;

    assert_eq!(
        reply.pointer("/Ok/emptied"),
        Some(&json!([trash_id(1)])),
        "{reply}"
    );
    assert_eq!(reply.pointer("/Ok/missing"), Some(&json!([trash_id(4)])));
    assert_eq!(reply.pointer("/Ok/failed/0/id"), Some(&json!(trash_id(3))));
    assert_eq!(trashed(&test), [kept, unreadable]);
    assert!(
        !test
            .0
            .join("trash")
            .join(named.rsplit('/').next().unwrap())
            .exists()
    );
}

/// Writes the fake's one Pool, `tank`, as 4 GiB holding `used` bytes.
pub(super) fn pool_using(test: &TestDir, used: u64) {
    let free = (4u64 << 30) - used;
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/pools"),
        format!("tank\t{}\t{used}\t{free}\tONLINE\toff\n", 4u64 << 30),
    )
    .unwrap();
}

/// A restored copy's snapshots stay on the Pool beside it, as a live copy's do: a restore
/// counts them, so it refuses an entry the Pool holds only while it is in the Trash.
#[tokio::test]
async fn a_restore_counts_the_snapshots_the_entry_brings_back() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    set_property(&test, &entry, "used", &(3u64 << 30).to_string());
    set_property(&test, &entry, "usedbydataset", &(1u64 << 29).to_string());
    pool_using(&test, 3 << 30);

    let reply = plugin.restore(&entry).await;

    assert!(refusal(&reply).contains("Machine Pool needs"), "{reply}");
    assert!(!test.0.join("volume").exists());
    assert_eq!(trashed(&test), [entry]);
}

/// A restore of a copy taken from a slot makes the slot first and moves the copy into it
/// after; one stopped between the two takes up the slot it made when run again.
#[tokio::test]
async fn a_restore_of_a_copy_stopped_after_its_slot_finishes_when_run_again() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "copy", 0, false);
    set_property(
        &test,
        "",
        "fail-command",
        &format!("rename {entry} tank/ployz-mirror/data/fs"),
    );

    let stopped = plugin.restore(&entry).await;
    assert!(stopped.get("Err").is_some(), "{stopped}");
    assert!(test.0.join("mirror").exists() && !test.0.join("mirror-fs").exists());
    assert_eq!(trashed(&test).as_slice(), std::slice::from_ref(&entry));

    let again = plugin.restore(&entry).await;

    assert_eq!(again.pointer("/Ok/place"), Some(&json!("copy")), "{again}");
    assert!(test.0.join("mirror-fs").exists());
    assert_eq!(trashed(&test), Vec::<String>::new());
    let parent = "tank/ployz-mirror/data";
    assert_eq!(
        property(&test, parent, "ployz:role").as_deref(),
        Some("old_copy")
    );
    assert_eq!(property(&test, parent, "ployz:trash-restoring"), None);
    assert_eq!(
        property(&test, "tank/ployz-mirror/data/fs", "ployz:trash-name"),
        None
    );
}

/// Docker may keep a record of a deleted name and mount it without a Create, so a root
/// restored from the Trash mounts only once a Create of its own data registers it.
#[tokio::test]
async fn a_restored_root_mounts_only_once_a_create_registers_it() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.restore(&entry).await.get("Ok").is_some());

    for route in ["/VolumeDriver.Mount", "/VolumeDriver.Path"] {
        let refused = plugin.post(route, json!({"Name": "data", "ID": "c"})).await;
        assert!(
            error(&refused).contains("restored from the Trash"),
            "{route}: {refused}"
        );
    }
    assert!(!test.0.join("mounted").exists());

    assert_eq!(plugin.create(BORN).await, json!({"Err": ""}));
    let mounted = plugin
        .post("/VolumeDriver.Mount", json!({"Name": "data", "ID": "c"}))
        .await;
    assert_eq!(error(&mounted), "", "{mounted}");
}

/// A restored root no Create took up can be deleted again, back into the Trash.
#[tokio::test]
async fn a_restored_root_no_create_took_up_goes_back_to_the_trash() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.restore(&entry).await.get("Ok").is_some());

    let removed = plugin
        .post("/VolumeDriver.Remove", json!({"Name": "data"}))
        .await;

    assert_eq!(removed, json!({"Err": ""}));
    assert!(!test.0.join("volume").exists());
    let again = trashed_one(&test, "root");
    assert_eq!(
        property(&test, &again, "ployz:origin").as_deref(),
        Some(BORN)
    );
}

/// A Create for other data refused at a restored root changes nothing on it.
#[tokio::test]
async fn a_create_refused_at_a_restored_root_leaves_its_marks() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.restore(&entry).await.get("Ok").is_some());

    let other = plugin.create(&"1".repeat(64)).await;

    assert!(error(&other).contains("restored from the Trash"), "{other}");
    assert_eq!(
        property(&test, ROOT, "ployz:restored").as_deref(),
        Some("on")
    );
    let id = entry.rsplit('/').next().unwrap();
    assert_eq!(
        property(&test, ROOT, "ployz:trash-restored").as_deref(),
        Some(id)
    );
}

/// A Machine whose Pool isn't imported doesn't know what its Trash holds, so List and
/// Empty refuse rather than answer an empty Trash.
#[tokio::test]
async fn list_and_empty_refuse_while_a_held_pool_is_not_imported() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &[], Ok(vec![]));
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/pools"), "").unwrap();
    fs::create_dir(test.0.join("ployz-volumes")).unwrap();

    let listed = plugin.post("/Ployz.Trash.List", json!(null)).await;
    let emptied = plugin
        .post("/Ployz.Trash.Empty", json!({"ids": [trash_id(1)]}))
        .await;

    for reply in [listed, emptied] {
        assert!(refusal(&reply).contains("not imported"), "{reply}");
    }
}

/// A sweep that runs late counts the hour after the last one it counted, so sweeps half
/// an hour off the hour still count every hour.
#[tokio::test]
async fn a_late_sweep_loses_no_hour() {
    let test = test_dir();
    let (plugin, _) = deleted_root(&test).await;
    plugin.sweep().await;

    plugin.clock.fetch_add(HOUR_MS * 3 / 2, Ordering::SeqCst);
    let late = plugin.sweep().await;
    plugin.clock.fetch_add(HOUR_MS / 2, Ordering::SeqCst);
    let next = plugin.sweep().await;

    assert_eq!(late.pointer("/Ok/hours"), Some(&json!(1)), "{late}");
    assert_eq!(next.pointer("/Ok/hours"), Some(&json!(2)), "{next}");
}

fn id_of(entry: &str) -> &str {
    entry.rsplit('/').next().unwrap()
}

/// A pin only marks the entry: it stays in the Trash, its 7 days start again, and it takes
/// the epoch after the one it held.
#[tokio::test]
async fn a_pin_keeps_a_deleted_root_in_the_trash_and_restarts_its_seven_days() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    set_property(&test, &entry, "ployz:epoch", "4");
    plugin.sweep().await;
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "100");

    let pinned = plugin.call(&entry, false).await;

    assert_eq!(
        pinned.pointer("/Ok/state"),
        Some(&json!({"kind": "pinned", "epoch": 5})),
        "{pinned}"
    );
    assert_eq!(trashed(&test).as_slice(), std::slice::from_ref(&entry));
    assert!(!test.0.join("volume").exists());
    assert_eq!(
        property(&test, &entry, "ployz:trash-pin"),
        Some(format!("{}:5", id_of(&entry)))
    );
    assert_eq!(
        property(&test, &entry, "ployz:trashed-hour").as_deref(),
        Some("100")
    );
    let listed = plugin.post("/Ployz.Trash.List", json!(null)).await;
    assert_eq!(
        listed.pointer("/Ok/entries/0/restoring"),
        Some(&json!(true)),
        "{listed}"
    );
}

/// A promote makes the pinned entry the Volume's root at its pinned epoch, writable, with
/// its `canmount` back and no trash marks; it keeps the entry's ID until a Create.
#[tokio::test]
async fn a_promote_makes_the_pinned_root_the_volume_at_its_epoch() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    set_property(&test, &entry, "readonly", "on");
    set_property(&test, &entry, "ployz:cut", "on");

    let restored = plugin.restore(&entry).await;

    assert_eq!(
        restored.pointer("/Ok/state"),
        Some(&json!({"kind": "restored", "epoch": 1})),
        "{restored}"
    );
    assert_eq!(trashed(&test), Vec::<String>::new());
    assert_eq!(property(&test, ROOT, "ployz:epoch").as_deref(), Some("1"));
    assert_eq!(property(&test, ROOT, "readonly").as_deref(), Some("off"));
    assert_eq!(property(&test, ROOT, "canmount").as_deref(), Some("on"));
    assert_eq!(property(&test, ROOT, "ployz:cut"), None);
    for mark in ["ployz:trash-pin", "ployz:trash-was", "ployz:trash-name"] {
        assert_eq!(property(&test, ROOT, mark), None, "{mark}");
    }
    assert_eq!(
        property(&test, ROOT, "ployz:trash-restored").as_deref(),
        Some(id_of(&entry))
    );
}

/// A promote stopped at any step finishes when run again, by the entry while it is in the
/// Trash and by the root once the rename committed; once finished, a call run again still
/// answers that the restore committed.
#[tokio::test]
async fn a_promote_stopped_at_any_step_finishes_when_run_again() {
    let id = trash_id(1);
    let entry = format!("tank/ployz-trash/{id}");
    let steps = [
        format!("set ployz:restored=on {entry}"),
        format!("set readonly=off {entry}"),
        format!("rename {entry} {ROOT}"),
        format!("set canmount=on {ROOT}"),
        format!("inherit ployz:trash-was {ROOT}"),
        format!("inherit ployz:trash-pin {ROOT}"),
        // Nothing stops: the reply is lost.
        String::new(),
    ];
    for step in steps {
        let test = test_dir();
        let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
        plant_trash(&test, &id, "data", "root", 0, false);
        assert!(
            plugin.call(&entry, false).await.get("Ok").is_some(),
            "{step}"
        );
        if !step.is_empty() {
            set_property(&test, "", "fail-command", &step);
            let stopped = plugin.call(&entry, true).await;
            assert!(stopped.get("Err").is_some(), "{step}: {stopped}");
        } else {
            assert!(plugin.call(&entry, true).await.get("Ok").is_some());
        }

        let again = plugin.call(&entry, true).await;

        assert_eq!(
            again.pointer("/Ok/state/kind"),
            Some(&json!("restored")),
            "{step}: {again}"
        );
        assert!(test.0.join("volume").exists(), "{step}");
        assert_eq!(trashed(&test), Vec::<String>::new(), "{step}");
        assert_eq!(
            property(&test, ROOT, "canmount").as_deref(),
            Some("on"),
            "{step}"
        );
        for mark in ["ployz:trash-pin", "ployz:trash-was", "ployz:trashed-at"] {
            assert_eq!(property(&test, ROOT, mark), None, "{step}: {mark}");
        }
    }
}

/// A Create that takes up a restored root whose promote stopped before its `canmount` came
/// back finishes that first, so the Volume mounts.
#[tokio::test]
async fn a_create_finishes_a_restore_that_stopped_before_its_canmount_came_back() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.call(&entry, false).await.get("Ok").is_some());
    set_property(
        &test,
        "",
        "fail-command",
        &format!("set canmount=on {ROOT}"),
    );
    assert!(plugin.call(&entry, true).await.get("Err").is_some());
    assert_eq!(property(&test, ROOT, "canmount").as_deref(), Some("off"));

    assert_eq!(plugin.create(BORN).await, json!({"Err": ""}));

    assert_eq!(property(&test, ROOT, "canmount").as_deref(), Some("on"));
    for mark in [
        "ployz:trash-was",
        "ployz:trash-pin",
        "ployz:restored",
        "ployz:trash-restored",
    ] {
        assert_eq!(property(&test, ROOT, mark), None, "{mark}");
    }
}

/// A promote needs its entry's own pin, and refuses while another entry of the Volume on
/// this Server is pinned: of two restores of one Volume, only one finishes.
#[tokio::test]
async fn a_promote_refuses_without_its_pin_or_beside_another_pinned_entry_of_the_volume() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let first = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    let second = plant_trash(&test, &trash_id(2), "data", "root", 0, false);

    let unpinned = plugin.call(&first, true).await;
    assert!(
        refusal(&unpinned).contains("isn't being restored any more"),
        "{unpinned}"
    );
    // A pin naming another entry is left from elsewhere.
    set_property(
        &test,
        &first,
        "ployz:trash-pin",
        &format!("{}:1", trash_id(2)),
    );
    assert!(plugin.call(&first, true).await.get("Err").is_some());

    assert!(plugin.call(&first, false).await.get("Ok").is_some());
    assert!(plugin.call(&second, false).await.get("Ok").is_some());
    let beside = plugin.call(&first, true).await;

    assert_eq!(beside.pointer("/Err/code"), Some(&json!("conflict")));
    assert!(refusal(&beside).contains(&trash_id(2)), "{beside}");
    assert!(!test.0.join("volume").exists());
    assert_eq!(trashed(&test), [first.clone(), second]);

    // Emptied by name, the other restore no longer stands in the way.
    plugin
        .post("/Ployz.Trash.Empty", json!({"ids": [trash_id(2)]}))
        .await;
    assert_eq!(
        plugin.call(&first, true).await.pointer("/Ok/state/kind"),
        Some(&json!("restored"))
    );
}

/// A promote whose entry was emptied or expired meanwhile says so, and restores nothing.
#[tokio::test]
async fn a_promote_after_its_entry_was_emptied_finds_nothing() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.call(&entry, false).await.get("Ok").is_some());
    plugin
        .post("/Ployz.Trash.Empty", json!({"ids": [id_of(&entry)]}))
        .await;

    let late = plugin.call(&entry, true).await;

    assert_eq!(
        late.pointer("/Err/code"),
        Some(&json!("not_found")),
        "{late}"
    );
    assert!(!test.0.join("volume").exists());
}

/// Emptying the whole Trash keeps an entry a restore pinned; naming it deletes it.
#[tokio::test]
async fn a_bulk_empty_keeps_a_pinned_entry_and_a_named_one_deletes_it() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let pinned = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    let other = plant_trash(&test, &trash_id(2), "logs", "root", 0, false);
    assert!(plugin.call(&pinned, false).await.get("Ok").is_some());
    let all = json!({"ids": [trash_id(1), trash_id(2)], "bulk": true});

    let bulk = plugin.post("/Ployz.Trash.Empty", all).await;

    assert_eq!(
        bulk.pointer("/Ok/emptied"),
        Some(&json!([trash_id(2)])),
        "{bulk}"
    );
    assert_eq!(bulk.pointer("/Ok/restoring"), Some(&json!([trash_id(1)])));
    assert_eq!(trashed(&test).as_slice(), std::slice::from_ref(&pinned));
    assert!(!trashed(&test).contains(&other));

    let named = plugin
        .post("/Ployz.Trash.Empty", json!({"ids": [trash_id(1)]}))
        .await;
    assert_eq!(
        named.pointer("/Ok/emptied"),
        Some(&json!([trash_id(1)])),
        "{named}"
    );
    assert_eq!(trashed(&test), Vec::<String>::new());
}

/// A pin doesn't keep an entry past its 7 days: one no restore tried again expires.
#[tokio::test]
async fn an_abandoned_pin_expires_seven_days_after_its_last_attempt() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    plugin.sweep().await;
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "50");
    assert!(plugin.call(&entry, false).await.get("Ok").is_some());
    set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "217");

    plugin.later(1);
    let swept = plugin.sweep().await;

    assert_eq!(
        swept.pointer("/Ok/destroyed"),
        Some(&json!([id_of(&entry)])),
        "{swept}"
    );
}

/// A Volume restored and deleted again goes back with none of the first restore's marks,
/// so its new entry pins and restores on its own.
#[tokio::test]
async fn a_restored_root_deleted_again_carries_no_restore_marks_into_its_entry() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.restore(&entry).await.get("Ok").is_some());

    let removed = plugin
        .post("/VolumeDriver.Remove", json!({"Name": "data"}))
        .await;

    assert_eq!(removed, json!({"Err": ""}));
    let again = trashed_one(&test, "root");
    for mark in [
        "ployz:trash-pin",
        "ployz:trash-restored",
        "ployz:trash-restoring",
    ] {
        assert_eq!(property(&test, &again, mark), None, "{mark}");
    }
    assert_eq!(
        plugin.restore(&again).await.pointer("/Ok/state/kind"),
        Some(&json!("restored"))
    );
}

/// A pin stopped between its two writes never leaves the entry pinned on its old clock,
/// where the next sweep would destroy a restore in progress.
#[tokio::test]
async fn a_pin_stopped_between_its_writes_leaves_no_pin_on_the_old_clock() {
    let id = trash_id(1);
    let entry = format!("tank/ployz-trash/{id}");
    for step in [
        format!("set ployz:trashed-hour=50 {entry}"),
        format!("set ployz:trash-pin={id}:1 {entry}"),
    ] {
        let test = test_dir();
        let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
        plant_trash(&test, &id, "data", "root", 0, false);
        set_property(&test, "tank/ployz-trash", "ployz:trash-hours", "50");
        set_property(&test, "", "fail-command", &step);

        let stopped = plugin.call(&entry, false).await;

        assert!(stopped.get("Err").is_some(), "{step}: {stopped}");
        if property(&test, &entry, "ployz:trash-pin").is_some() {
            assert_eq!(
                property(&test, &entry, "ployz:trashed-hour").as_deref(),
                Some("50"),
                "{step}"
            );
        }
    }
}

/// A promote stopped before `canmount` came back leaves the root `canmount=off` with its
/// `trash-was`. Deleted again, the new entry keeps the `trash-was` the first delete took, so
/// its restore brings `canmount` back rather than keeping it off for good.
#[tokio::test]
async fn a_half_restored_root_deleted_again_keeps_the_canmount_it_was_deleted_with() {
    let test = test_dir();
    let (plugin, entry) = deleted_root(&test).await;
    assert!(plugin.call(&entry, false).await.get("Ok").is_some());
    set_property(
        &test,
        "",
        "fail-command",
        &format!("set canmount=on {ROOT}"),
    );
    assert!(plugin.call(&entry, true).await.get("Err").is_some());
    assert_eq!(property(&test, ROOT, "canmount").as_deref(), Some("off"));
    let removed = plugin
        .post("/VolumeDriver.Remove", json!({"Name": "data"}))
        .await;
    assert_eq!(removed, json!({"Err": ""}));
    let again = trashed_one(&test, "root");

    assert!(plugin.restore(&again).await.get("Ok").is_some());

    assert_eq!(property(&test, ROOT, "canmount").as_deref(), Some("on"));
}

/// A restore's proof asks each Server which Pool it read and what a restore pinned there,
/// so two restores of one Volume on two Servers each see the other's pin.
#[tokio::test]
async fn a_proof_lists_each_entry_of_the_volume_a_restore_pinned() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let first = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    plant_trash(&test, &trash_id(2), "data", "root", 0, false);
    plant_trash(&test, &trash_id(3), "other", "root", 0, false);
    let prove = || plugin.post("/Volume.Inspect", json!({"name": "data", "prove": true}));

    assert_eq!(prove().await.pointer("/Ok/pinned"), Some(&json!([])));
    plugin.call(&first, false).await;
    plugin.call(&trash_id(3), false).await;
    let proof = prove().await;

    assert_eq!(
        proof,
        json!({"Ok": {
            "copy": null,
            "read": {"kind": "pool", "name": "tank", "health": "ONLINE", "by": "sole"},
            "pinned": [trash_id(1)],
        }})
    );
}

/// A Pool that turned `DEGRADED` after the pin proves nothing, so its promote refuses and
/// the entry stays pinned.
#[tokio::test]
async fn a_promote_refuses_once_its_pool_is_degraded() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    plugin.call(&entry, false).await;
    fs::write(
        test.0.join("props/pools"),
        USABLE_POOL.replace("ONLINE", "DEGRADED"),
    )
    .unwrap();

    let promoted = plugin.call(&entry, true).await;

    assert_eq!(
        promoted.pointer("/Err/code"),
        Some(&json!("conflict")),
        "{promoted}"
    );
    assert!(
        refusal(&promoted).contains("its Pool tank is DEGRADED"),
        "{promoted}"
    );
    assert!(property(&test, &entry, "ployz:trash-pin").is_some());
}

/// A proof reads a pin as absent only from a source ZFS prints. A row cut partway through
/// its source reads as nothing, where it once read as not pinned.
#[tokio::test]
async fn a_proof_fails_where_a_pin_reads_with_a_source_zfs_never_prints() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    plugin.call(&entry, false).await;
    let cut = test
        .0
        .join("props/source-cut")
        .join(format!("tank/ployz-trash/{}", trash_id(1)));
    fs::create_dir_all(cut.parent().unwrap()).unwrap();
    fs::write(&cut, "").unwrap();

    let proof = plugin
        .post("/Volume.Inspect", json!({"name": "data", "prove": true}))
        .await;

    assert!(proof.get("Err").is_some(), "{proof}");
}

/// A listing of the Pool without the Pool's own row saw nothing, so the proof fails
/// rather than answer that there's no copy and no pin.
#[tokio::test]
async fn a_proof_fails_on_a_dataset_listing_without_the_pool() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/datasets-empty"), "").unwrap();

    let proof = plugin
        .post("/Volume.Inspect", json!({"name": "data", "prove": true}))
        .await;

    assert!(proof.get("Err").is_some(), "{proof}");
}

/// A proof says no restore is pinned only from a row ZFS printed: a read cut short, which
/// once read as not pinned, fails the proof.
#[tokio::test]
async fn a_proof_fails_where_zfs_cut_short_the_read_of_a_pin() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    plugin.call(&entry, false).await;
    let dataset = format!("tank/ployz-trash/{}", trash_id(1));
    let cut = test.0.join("props/get-cut").join(&dataset);
    fs::create_dir_all(cut.parent().unwrap()).unwrap();
    fs::write(&cut, "").unwrap();

    let proof = plugin
        .post("/Volume.Inspect", json!({"name": "data", "prove": true}))
        .await;

    assert!(proof.get("Err").is_some(), "{proof}");
}

/// A Server that can't tell which attached Pool is its own answers that it can't prove
/// anything, and pins or promotes nothing: the root would come back in a Pool no proof read.
#[tokio::test]
async fn a_server_that_cant_tell_its_own_pool_proves_nothing_and_restores_nothing() {
    let test = test_dir();
    let plugin = Plugin::start(&test, &["root"], Ok(vec![]));
    let entry = plant_trash(&test, &trash_id(1), "data", "root", 0, false);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(
        test.0.join("props/importable"),
        "   pool: ployz-device\n     id: 1\n  state: ONLINE\n action: The pool can be imported using its name or numeric identifier.\n config:\n\n\tployz-device  ONLINE\n\t  sdb       ONLINE\n",
    )
    .unwrap();

    let proof = plugin
        .post("/Volume.Inspect", json!({"name": "data", "prove": true}))
        .await;
    let pinned = plugin.call(&entry, false).await;

    assert_eq!(
        proof.pointer("/Ok/read/kind"),
        Some(&json!("unproven")),
        "{proof}"
    );
    assert_eq!(proof.pointer("/Ok/pinned"), None, "{proof}");
    assert_eq!(
        pinned.pointer("/Err/code"),
        Some(&json!("conflict")),
        "{pinned}"
    );
    assert!(refusal(&pinned).contains("ployz-device"), "{pinned}");
    assert_eq!(property(&test, &entry, "ployz:trash-pin"), None);
}
