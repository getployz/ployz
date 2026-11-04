//! Volume.Inspect through the plugin route, and the helpers other plugin tests start from.

use super::*;

/// Serves the plugin over fake ZFS with the given marker files present.
pub(super) fn start(
    test: &TestDir,
    pools: &str,
    markers: &[&str],
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, pools);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(serve(listener, VolumeStorage::with_programs(zpool, zfs)));
    (socket, server)
}

/// Writes a ZFS user property the fake will answer for `dataset`.
pub(super) fn set_property(test: &TestDir, dataset: &str, property: &str, value: &str) {
    let directory = test.0.join("props").join(dataset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(property), format!("{value}\n")).unwrap();
}

#[tokio::test]
async fn inspect_reports_a_root_with_its_newest_snapshot() {
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@f-2\t42\t1700000000\ntank/ployz/data@zfs-auto-snap\t7\t1600000000",
    );
    let (socket, server) = start(&test, USABLE_POOL, &["root", "volume"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(
        response,
        json!({"Ok": {
            "copy": {
                "kind": "root",
                "readonly": false,
                "newest": {"name": "f-2", "guid": "42", "created_unix_seconds": 1_700_000_000},
                "epoch": 0,
                "seed": false,
            },
        }})
    );
    assert!(
        !commands(&test).contains("ployz:cut"),
        "a writable root's cut is read"
    );
    server.abort();
}

#[tokio::test]
async fn inspect_reports_a_slot_and_nothing_for_an_unknown_name() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["root", "slot"]);

    let response = post(&socket, "/Volume.Inspect", json!({"name": "copy"})).await;
    assert_eq!(
        response.pointer("/Ok/copy").unwrap(),
        &json!({
            "kind": "slot",
            "slot": "1",
            "readonly": true,
            "newest": null,
            "resume_token": null,
            "pull": null,
            "role": "mirror",
        })
    );
    let response = post(&socket, "/Volume.Inspect", json!({"name": "missing"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null}}));
    server.abort();
}

#[tokio::test]
async fn inspect_without_a_pool_holds_nothing() {
    let test = TestDir::new();
    let (socket, server) = start(&test, "", &[]);
    let response = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(response, json!({"Ok": {"copy": null}}));
    server.abort();
}

/// Every combination of `readonly` and the marks parses to the root this table names. Its
/// reply keeps the `readonly` and `seed` the root read as before the parse (`seed` is
/// `ployz:promote` and `ployz:seed` together) and only adds `mark`. These wants and expects
/// plan the reply as they plan it without `mark`, and `Held::switching` answers `readonly`,
/// as it did.
#[test]
fn every_root_mark_combination_parses_to_one_copy() {
    use ployz_core::{
        Drain, ExpectedCopy, RootMark, SlotId, SlotRole, SnapshotGuid, VolumeCopy, WantedCopy,
        WriterEpoch, plan,
    };

    use super::super::{
        inspect::{PromoteMark, RootCopy, RootMarks},
        storage::Held,
    };

    // readonly, ployz:cut, ployz:promote, ployz:seed => root, mark.
    let table = [
        (
            (false, false, false, false),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, false, false, true),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, false, true, false),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, false, true, true),
            RootCopy::Writable { seed: true },
            None,
        ),
        (
            (false, true, false, false),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, true, false, true),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, true, true, false),
            RootCopy::Writable { seed: false },
            None,
        ),
        (
            (false, true, true, true),
            RootCopy::Writable { seed: true },
            None,
        ),
        ((true, false, false, false), RootCopy::Unmarked, None),
        ((true, false, false, true), RootCopy::Unmarked, None),
        (
            (true, false, true, false),
            RootCopy::Promote { seed: false },
            Some(RootMark::Promote),
        ),
        (
            (true, false, true, true),
            RootCopy::Promote { seed: true },
            Some(RootMark::Promote),
        ),
        (
            (true, true, false, false),
            RootCopy::Cut { seed: false },
            Some(RootMark::Cut),
        ),
        (
            (true, true, false, true),
            RootCopy::Cut { seed: false },
            Some(RootMark::Cut),
        ),
        (
            (true, true, true, false),
            RootCopy::Cut { seed: false },
            Some(RootMark::Cut),
        ),
        (
            (true, true, true, true),
            RootCopy::Cut { seed: true },
            Some(RootMark::Cut),
        ),
    ];
    let epoch = WriterEpoch::new(2);
    let wants = [
        WantedCopy::Writer {
            at: SnapshotGuid::new(13),
            epoch,
            start: None,
        },
        WantedCopy::Writer {
            at: SnapshotGuid::new(13),
            epoch: WriterEpoch::new(3),
            start: None,
        },
        WantedCopy::Mirror {
            from: None,
            target: "f-2".parse().unwrap(),
            drain: Drain::NONE,
        },
        WantedCopy::Snapshot {
            name: "f-2".parse().unwrap(),
        },
        WantedCopy::Forget,
        WantedCopy::Floor,
    ];
    let expects = [
        ExpectedCopy::Writer { epoch },
        ExpectedCopy::Mirror {
            newest: SnapshotGuid::new(13),
            slot: SlotId::new(1),
            role: Some(SlotRole::Mirror),
        },
        ExpectedCopy::None,
    ];
    let newest = || {
        Some(ployz_core::Snapshot {
            name: "f-1".parse().unwrap(),
            guid: SnapshotGuid::new(13),
            created_unix_seconds: 1_700_000_000,
        })
    };
    for ((readonly, cut, promote, seed_mark), expected_root, expected_mark) in table {
        let marks = RootMarks {
            cut,
            promote: if promote {
                PromoteMark::Set { seed: seed_mark }
            } else {
                PromoteMark::Unset
            },
        };
        let dataset = Dataset::parse(
            &format!(
                "tank/ployz/data\t1073741824\t0\t0\t0\t/var/lib/ployz-volumes/data\tno\t{}",
                if readonly { "on" } else { "off" }
            ),
            "tank",
        )
        .unwrap();
        let root = RootCopy::parse(&dataset, marks);
        assert_eq!(root, expected_root, "{marks:?} readonly={readonly}");
        let copy = root.view(newest(), epoch);
        let VolumeCopy::Root {
            readonly: replied,
            seed,
            mark,
            ..
        } = copy
        else {
            panic!("{copy:?}");
        };
        assert_eq!(replied, readonly, "{marks:?}");
        assert_eq!(seed, promote && seed_mark, "{marks:?}");
        assert_eq!(mark, expected_mark, "readonly={readonly} {marks:?}");
        assert_eq!(Held::Root(&dataset).switching(), readonly);

        let before = VolumeCopy::Root {
            readonly,
            newest: newest(),
            epoch,
            seed,
            mark: None,
        };
        for want in &wants {
            for expect in &expects {
                for running in [false, true] {
                    assert_eq!(
                        plan(Some(&copy), running, expect, want),
                        plan(Some(&before), running, expect, want),
                        "{copy:?} {want:?} {expect:?}"
                    );
                }
            }
        }
    }
}
