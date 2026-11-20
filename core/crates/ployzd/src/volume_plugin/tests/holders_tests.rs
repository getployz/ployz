//! A copy goes under a name only when every Container that mounts the name, running or
//! stopped, expects the origin it carries, and only while Docker can say which those are.

use ployz_core::{DockerVolumeName, VolumeOrigin};

use super::super::holders::Holder;
use super::copy_tests::{BORN, SLOT_FS, poll, set_snapshots};
use super::inspect_tests::set_property;
use super::*;

const ROOT: &str = "tank/ployz/data";

/// A plugin over fake ZFS whose Docker lists `holders` for every name.
fn start_holding(
    test: &TestDir,
    markers: &[&str],
    holders: std::result::Result<Vec<Holder>, String>,
) -> PathBuf {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let storage = VolumeStorage::with_programs(zpool, zfs);
    storage.holders.set(holders);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    tokio::spawn(serve(listener, storage));
    socket
}

/// A Container whose label says `name` expects `origin`; `None` is one from before labels.
fn holder(name: &str, origin: Option<&str>) -> Holder {
    Holder {
        origins: origin.map(|origin| format!(r#"{{"{name}":"{origin}"}}"#)),
    }
}

fn legacy(name: &str) -> String {
    VolumeOrigin::legacy(&DockerVolumeName::parse(name).unwrap()).to_string()
}

async fn promote(socket: &Path) -> Value {
    post(
        socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await
}

fn assert_still_in_its_slot(test: &TestDir) {
    assert!(
        test.0.join("mirror-fs").exists(),
        "the copy stays in its slot"
    );
    assert!(!test.0.join("volume").exists(), "no root was made");
    assert_eq!(property(test, SLOT_FS, "ployz:promote"), None);
    assert!(!commands(test).contains("zfs rename"), "{}", commands(test));
}

/// A stale Mirror of a reused name: the Container that mounts the name was approved for
/// another Volume's data, so the old copy never becomes its root.
#[tokio::test]
async fn a_promote_under_a_holder_expecting_other_data_is_refused() {
    let test = TestDir::new();
    let socket = start_holding(
        &test,
        &["root", "mirror", "mirror-fs"],
        Ok(vec![holder("data", Some(BORN))]),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let reply = promote(&socket).await;
    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!("foreign")),
        "{reply}"
    );
    assert_still_in_its_slot(&test);
}

/// A Container from before origins expects data from before them, so a born copy never
/// replaces what it mounts, even while it is stopped.
#[tokio::test]
async fn a_promote_of_born_data_under_an_unlabelled_holder_is_refused() {
    let test = TestDir::new();
    let socket = start_holding(
        &test,
        &["root", "mirror", "mirror-fs"],
        Ok(vec![holder("data", None)]),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "ployz:origin", BORN);

    let reply = promote(&socket).await;
    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!("foreign")),
        "{reply}"
    );
    assert_still_in_its_slot(&test);
}

/// A label that does not list the name says nothing the copy could match.
#[tokio::test]
async fn a_promote_under_a_label_that_omits_the_name_is_refused() {
    let test = TestDir::new();
    let socket = start_holding(
        &test,
        &["root", "mirror", "mirror-fs"],
        Ok(vec![holder("other", Some(&legacy("other")))]),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let reply = promote(&socket).await;
    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!("foreign")),
        "{reply}"
    );
    assert_still_in_its_slot(&test);
}

/// Docker down means the holders are unknown, so nothing moves under the name.
#[tokio::test]
async fn a_promote_while_docker_cannot_list_holders_is_refused() {
    let test = TestDir::new();
    let socket = start_holding(
        &test,
        &["root", "mirror", "mirror-fs"],
        Err("connection refused".into()),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let reply = promote(&socket).await;
    assert_eq!(
        reply.pointer("/Err/code"),
        Some(&json!("unavailable")),
        "{reply}"
    );
    assert_still_in_its_slot(&test);
}

/// Holders that expect what the copy carries, born or Legacy, let it through.
#[tokio::test]
async fn a_promote_under_holders_expecting_its_data_lands() {
    for (origin, label) in [(Some(BORN), BORN.to_owned()), (None, legacy("data"))] {
        let test = TestDir::new();
        let socket = start_holding(
            &test,
            &["root", "mirror", "mirror-fs"],
            Ok(vec![
                holder("data", Some(&label)),
                Holder {
                    origins: Some(format!(r#"{{"data":"{label}","logs":"{BORN}"}}"#)),
                },
            ]),
        );
        set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);
        if let Some(origin) = origin {
            set_property(&test, SLOT_FS, "ployz:origin", origin);
        }

        let reply = promote(&socket).await;
        assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
        assert_eq!(
            property(&test, ROOT, "ployz:origin").as_deref(),
            origin,
            "the root keeps the origin its copy carried"
        );
    }
}

/// A Container already mounts the Seed's new name and expects other data, so the seeded
/// copy stays in its slot.
#[tokio::test]
async fn a_seed_landing_under_a_holder_expecting_other_data_is_refused() {
    let test = TestDir::new();
    let socket = start_holding(
        &test,
        &["root", "volume"],
        Ok(vec![holder("seed", Some(BORN))]),
    );
    set_property(
        &test,
        ROOT,
        "snapshots",
        "tank/ployz/data@w-1-1\t77\t1700000000",
    );

    let request = json!({"name": "data", "into": "seed", "guid": "77"});
    let refused = poll(async || {
        let reply = post(&socket, "/Volume.Seed", request.clone()).await;
        reply.get("Err").is_some().then_some(reply)
    })
    .await
    .expect("the landing was never refused");
    assert_eq!(
        refused.pointer("/Err/details/reason"),
        Some(&json!("foreign")),
        "{refused}"
    );
    assert!(!test.0.join("seed-volume").exists(), "no root of seed");
    assert!(
        test.0.join("seed-slot").exists(),
        "the copy stays in its slot"
    );
}
