//! A copy goes under a name only when every Container that mounts the name, running or
//! stopped, expects the origin it carries, and only while Docker can say which those are.

use std::time::{Duration, Instant};

use ployz_core::{DockerVolumeName, VolumeOrigin};
use tokio::io::AsyncWriteExt as _;

use super::super::holders::{Holder, Holders};
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
    let fixed = Holders::none();
    fixed.set(holders);
    start_listing(test, markers, fixed)
}

/// A plugin over fake ZFS that lists Containers from `holders`.
fn start_listing(test: &TestDir, markers: &[&str], holders: Holders) -> PathBuf {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.holders = holders;
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
    let test = test_dir();
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
        let test = test_dir();
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
    let test = test_dir();
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

/// A Docker on `socket` that reads each request, writes `reply`, and then holds the
/// connection open without another byte.
fn stalled_docker(socket: &Path, reply: &'static [u8]) {
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut request).await;
                let _ = stream.write_all(reply).await;
                std::future::pending::<()>().await;
                drop(stream);
            });
        }
    });
}

/// Docker sent the list's headers and stalled in its body. The promote is still refused
/// as unavailable, so admission is not held past the bound.
#[tokio::test]
async fn a_promote_while_docker_stalls_in_the_list_body_is_refused() {
    let test = test_dir();
    let docker = test.0.join("docker.sock");
    stalled_docker(
        &docker,
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n[",
    );
    let socket = start_listing(
        &test,
        &["root", "mirror", "mirror-fs"],
        Holders::Socket(docker),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let reply = tokio::time::timeout(Duration::from_secs(8), promote(&socket))
        .await
        .expect("the promote waited on Docker's body past its bound");
    assert_eq!(
        reply.pointer("/Err/code"),
        Some(&json!("unavailable")),
        "{reply}"
    );
    assert_still_in_its_slot(&test);
}

/// The CLI and Cloud give a copy call 10 s. A Docker that never answers is refused well
/// inside that, so the caller reads "Docker is not answering" instead of a timeout that
/// may have changed the copy.
#[tokio::test]
async fn a_promote_while_docker_never_answers_is_refused_inside_a_callers_budget() {
    let test = test_dir();
    let docker = test.0.join("docker.sock");
    stalled_docker(&docker, b"");
    let socket = start_listing(
        &test,
        &["root", "mirror", "mirror-fs"],
        Holders::Socket(docker),
    );
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let started = Instant::now();
    let reply = promote(&socket).await;
    let took = started.elapsed();
    assert_eq!(
        reply.pointer("/Err/code"),
        Some(&json!("unavailable")),
        "{reply}"
    );
    assert_eq!(
        reply.pointer("/Err/details/reason"),
        Some(&json!("docker_unavailable")),
        "the refusal says the copy stayed, so the caller knows nothing changed: {reply}"
    );
    assert!(
        reply
            .pointer("/Err/message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("Docker is not answering")),
        "{reply}"
    );
    assert!(
        took < Duration::from_secs(5),
        "refused after {took:?}, too close to the caller's 10 s"
    );
    assert_still_in_its_slot(&test);
}

async fn create(socket: &Path, origin: Option<&str>) -> String {
    let mut options = serde_json::Map::from_iter([("size".to_owned(), json!("1g"))]);
    if let Some(origin) = origin {
        options.insert(
            ployz_core::PROVISIONED_ORIGIN_OPTION.to_owned(),
            json!(origin),
        );
    }
    let reply = post(
        socket,
        "/VolumeDriver.Create",
        json!({"Name": "data", "Opts": options}),
    )
    .await;
    reply
        .get("Err")
        .and_then(Value::as_str)
        .expect("Create answers Err")
        .to_owned()
}

/// A Container kept from an earlier Volume `data` still mounts the name after its copy was
/// forgotten. A fresh root of other data under the name would open to it on its next
/// start, so none is made: not a born root under an unlabelled Container, and not a
/// Legacy or other born root under a born one.
#[tokio::test]
async fn a_fresh_root_under_a_holder_expecting_other_data_is_refused() {
    let other = "b".repeat(64);
    for (held, requested) in [
        (None, Some(BORN)),
        (Some(BORN), None),
        (Some(BORN), Some(other.as_str())),
    ] {
        let test = test_dir();
        let socket = start_holding(&test, &["root"], Ok(vec![holder("data", held)]));

        let error = create(&socket, requested).await;
        assert!(
            error.contains(crate::volume_copy::HELD_FOR_OTHER_DATA),
            "{held:?} took {requested:?}: {error}"
        );
        assert!(!test.0.join("volume").exists(), "no root was made");
        assert!(
            !commands(&test).contains("zfs create"),
            "{}",
            commands(&test)
        );
    }
}

/// The control: a fresh root whose origin every holder expects is made, and a Docker that
/// cannot list holders makes none.
#[tokio::test]
async fn a_fresh_root_is_made_only_under_holders_expecting_it() {
    for (held, requested) in [(Some(BORN), Some(BORN)), (None, None)] {
        let test = test_dir();
        let label = held.map_or_else(|| legacy("data"), str::to_owned);
        let socket = start_holding(&test, &["root"], Ok(vec![holder("data", Some(&label))]));

        assert_eq!(create(&socket, requested).await, "", "{held:?}");
        assert!(test.0.join("volume").exists(), "the root was made");
        assert_eq!(property(&test, ROOT, "ployz:origin").as_deref(), requested);
    }

    let test = test_dir();
    let socket = start_holding(&test, &["root"], Err("connection refused".into()));
    let error = create(&socket, None).await;
    assert!(error.contains("Docker is not answering"), "{error}");
    assert!(!test.0.join("volume").exists(), "no root was made");
}
