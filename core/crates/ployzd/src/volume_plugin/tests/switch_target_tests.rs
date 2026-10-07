//! Target switch effects and crash replay through the Docker plugin routes.

use ployz_core::Lease;

use super::lease_tests::set_property;
use super::mirror_tests::{at, commands, property, snapshot_names};
use super::switch_source_tests::{CONTAINER, programs, serve_storage};
use super::*;

const RECORD: (&str, &str) = ("tank/ployz", "ployz:lease.data");
const ROOT: &str = "tank/ployz/data";
const SLOT: &str = "tank/ployz-mirror/data";
const FS: &str = "tank/ployz-mirror/data/fs";

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// B holding the handed-over mirror: a read-only slot whose newest snapshot has `guid`.
fn mirror(marker: &str, guid: u64) -> (TestDir, PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let test = TestDir::new();
    for file in ["root", "mirror", "mirror-fs"] {
        fs::write(test.0.join(file), "").unwrap();
    }
    set_property(&test, SLOT, "ployz:mirror", marker);
    set_property(&test, FS, "readonly", "on");
    set_property(
        &test,
        FS,
        "snapshots",
        &format!("{FS}@f-1\t{guid}\t1700000000"),
    );
    let storage = programs(&test);
    fs::write(test.0.join("holders"), "").unwrap();
    let (socket, server) = serve_storage(&test, storage);
    (test, socket, server)
}

/// B after Promote: a writable, unmounted root whose newest snapshot is the handed-over one.
fn promoted() -> (TestDir, PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let test = TestDir::new();
    for file in ["root", "volume"] {
        fs::write(test.0.join(file), "").unwrap();
    }
    set_property(&test, ROOT, "readonly", "off");
    set_property(&test, ROOT, "ployz:writer", "idle");
    set_property(&test, ROOT, "ployz:handoff", "900");
    set_property(
        &test,
        ROOT,
        "snapshots",
        &format!("{ROOT}@f-1\t900\t1700000000"),
    );
    let storage = programs(&test);
    let (socket, server) = serve_storage(&test, storage);
    (test, socket, server)
}

fn reason(response: &Value) -> Option<&Value> {
    response.pointer("/Err/details/reason")
}

async fn promoted_by_task(test: &TestDir) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.0.join("volume").exists() || property(test, ROOT, "ployz:task").is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the Promote task finishes");
}

#[tokio::test]
async fn accept_refuses_a_mirror_without_the_handed_over_snapshot() {
    let (test, socket, server) = mirror("idle", 899);
    let response = post(
        &socket,
        "/Volume.AcceptHandOff",
        at(1, 9, 0, 0, json!({"guid":900})),
    )
    .await;
    assert_eq!(
        reason(&response),
        Some(&json!("precondition")),
        "{response}"
    );
    assert_eq!(property(&test, RECORD.0, RECORD.1), None);
    assert_eq!(
        property(&test, SLOT, "ployz:mirror").as_deref(),
        Some("idle")
    );
    server.abort();
}

#[tokio::test]
async fn accept_marks_the_final_mirror_handed_in_and_replays() {
    let (test, socket, server) = mirror("final:900", 900);
    for decision in ["adopt", "replay"] {
        let response = post(
            &socket,
            "/Volume.AcceptHandOff",
            at(1, 9, 0, 0, json!({"guid":900})),
        )
        .await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!(decision)),
            "{response}"
        );
        assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("open")));
    }
    assert_eq!(
        property(&test, SLOT, "ployz:mirror").as_deref(),
        Some("handed_in:900")
    );
    assert_eq!(property(&test, FS, "readonly").as_deref(), Some("on"));
    server.abort();
}

#[tokio::test]
async fn clear_final_returns_a_final_mirror_to_idle_and_refuses_a_handed_in_one() {
    let (test, socket, server) = mirror("final:900", 900);
    for decision in ["adopt", "replay"] {
        let response = post(&socket, "/Volume.ClearFinal", at(1, 9, 0, 0, json!({}))).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!(decision)),
            "{response}"
        );
    }
    assert_eq!(
        property(&test, SLOT, "ployz:mirror").as_deref(),
        Some("idle")
    );
    set_property(&test, SLOT, "ployz:mirror", "handed_in:900");
    let response = post(&socket, "/Volume.ClearFinal", at(1, 10, 0, 0, json!({}))).await;
    assert_eq!(
        reason(&response),
        Some(&json!("precondition")),
        "{response}"
    );
    server.abort();
}

#[tokio::test]
async fn promote_renames_the_handed_in_copy_over_the_root_and_replays() {
    let (test, socket, server) = mirror("handed_in:900", 900);
    let response = post(&socket, "/Volume.Promote", at(1, 10, 0, 0, json!({}))).await;
    assert_eq!(
        response.pointer("/Ok/decision"),
        Some(&json!("adopt")),
        "{response}"
    );
    promoted_by_task(&test).await;
    assert!(!test.0.join("mirror").exists());
    assert!(!test.0.join("mirror-fs").exists());
    assert!(!test.0.join("readonly-volume").exists());
    assert_eq!(property(&test, ROOT, "readonly").as_deref(), Some("off"));
    assert_eq!(
        property(&test, ROOT, "ployz:handoff").as_deref(),
        Some("900")
    );
    assert_eq!(snapshot_names(&test, ROOT), ["f-1"]);
    let response = post(&socket, "/Volume.Promote", at(1, 10, 0, 0, json!({}))).await;
    assert_eq!(
        response.pointer("/Ok/decision"),
        Some(&json!("replay")),
        "{response}"
    );
    assert_eq!(
        response.pointer("/Ok/copy/kind"),
        Some(&json!("root")),
        "{response}"
    );
    assert_eq!(
        property(&test, RECORD.0, RECORD.1).as_deref(),
        Some("1:10.0.0:open")
    );
    server.abort();
}

#[tokio::test]
async fn the_promote_task_stops_when_the_record_names_a_later_lease() {
    let (test, _socket, server) = mirror("handed_in:900", 900);
    set_property(&test, FS, "ployz:task", &format!("1:{}", now()));
    set_property(&test, RECORD.0, RECORD.1, "2:3.0.0:open");
    let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
    let error = storage
        .finish_promote(&"data".parse().unwrap(), Lease::new(1))
        .await
        .unwrap_err();
    assert_eq!(
        error.details.get("reason"),
        Some(&json!("stale_lease")),
        "{error:?}"
    );
    assert!(test.0.join("mirror-fs").exists());
    assert_eq!(property(&test, FS, "readonly").as_deref(), Some("on"));
    assert!(!commands(&test).contains("zfs rename"));
    server.abort();
}

#[tokio::test]
async fn the_promote_task_stops_past_its_budget() {
    let (test, _socket, server) = mirror("handed_in:900", 900);
    set_property(&test, FS, "ployz:task", &format!("1:{}", now() - 601));
    set_property(&test, RECORD.0, RECORD.1, "1:10.0.0:open");
    let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
    let error = storage
        .finish_promote(&"data".parse().unwrap(), Lease::new(1))
        .await
        .unwrap_err();
    assert_eq!(
        error.details.get("reason"),
        Some(&json!("expired")),
        "{error:?}"
    );
    assert!(test.0.join("mirror-fs").exists());
    assert_eq!(property(&test, FS, "readonly").as_deref(), Some("on"));
    server.abort();
}

#[tokio::test]
async fn promote_admission_refuses_a_task_past_its_budget() {
    let (test, socket, server) = mirror("handed_in:900", 900);
    set_property(&test, FS, "ployz:task", &format!("1:{}", now() - 601));
    let response = post(&socket, "/Volume.Promote", at(1, 10, 0, 0, json!({}))).await;
    assert_eq!(reason(&response), Some(&json!("expired")), "{response}");
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

#[tokio::test]
async fn start_mounts_through_the_grant_while_open_then_closes_and_replays() {
    let (test, socket, server) = promoted();
    let admitted = post(
        &socket,
        "/Volume.AdmitHandedStart",
        at(1, 11, 0, 0, json!({})),
    )
    .await;
    assert_eq!(
        admitted.pointer("/Ok/copy/newest/guid"),
        Some(&json!(900)),
        "{admitted}"
    );
    assert_eq!(admitted.pointer("/Ok/lease/cycle"), Some(&json!("open")));
    assert!(property(&test, ROOT, "ployz:task").is_some());
    let started = post(
        &socket,
        "/Volume.StartHandedContainer",
        at(1, 11, 0, 0, json!({"container_id": CONTAINER})),
    )
    .await;
    assert_eq!(
        started.pointer("/Ok/lease/cycle"),
        Some(&json!("closed")),
        "{started}"
    );
    assert!(test.0.join("running").exists());
    assert!(test.0.join("mounted").exists());
    assert_eq!(property(&test, ROOT, "ployz:task"), None);
    let replay = post(
        &socket,
        "/Volume.AdmitHandedStart",
        at(1, 11, 0, 0, json!({})),
    )
    .await;
    assert_eq!(
        replay.pointer("/Ok/decision"),
        Some(&json!("replay")),
        "{replay}"
    );
    assert_eq!(replay.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
    assert_eq!(commands(&test).matches("docker start").count(), 1);
    server.abort();
}

#[tokio::test]
async fn start_stops_before_docker_start_when_the_record_names_a_later_lease() {
    let (test, socket, server) = promoted();
    let admitted = post(
        &socket,
        "/Volume.AdmitHandedStart",
        at(1, 11, 0, 0, json!({})),
    )
    .await;
    assert!(admitted.get("Ok").is_some(), "{admitted}");
    set_property(&test, RECORD.0, RECORD.1, "2:3.0.0:open");
    let response = post(
        &socket,
        "/Volume.StartHandedContainer",
        at(1, 11, 0, 0, json!({"container_id": CONTAINER})),
    )
    .await;
    assert_eq!(reason(&response), Some(&json!("stale_lease")), "{response}");
    assert!(!commands(&test).contains("docker start"));
    server.abort();
}

#[tokio::test]
async fn start_stops_before_docker_start_past_its_budget() {
    let (test, socket, server) = promoted();
    let admitted = post(
        &socket,
        "/Volume.AdmitHandedStart",
        at(1, 11, 0, 0, json!({})),
    )
    .await;
    assert!(admitted.get("Ok").is_some(), "{admitted}");
    set_property(&test, ROOT, "ployz:task", &format!("1:{}", now() - 601));
    let response = post(
        &socket,
        "/Volume.StartHandedContainer",
        at(1, 11, 0, 0, json!({"container_id": CONTAINER})),
    )
    .await;
    assert_eq!(reason(&response), Some(&json!("expired")), "{response}");
    assert!(!commands(&test).contains("docker start"));
    server.abort();
}

#[tokio::test]
async fn start_admission_refuses_a_root_whose_newest_snapshot_is_not_handed_over() {
    let (test, socket, server) = promoted();
    set_property(&test, ROOT, "ployz:handoff", "899");
    let response = post(
        &socket,
        "/Volume.AdmitHandedStart",
        at(1, 11, 0, 0, json!({})),
    )
    .await;
    assert_eq!(
        reason(&response),
        Some(&json!("precondition")),
        "{response}"
    );
    assert_eq!(property(&test, RECORD.0, RECORD.1), None);
    server.abort();
}

#[tokio::test]
async fn restore_makes_the_mirror_the_writable_root_with_one_restore_snapshot() {
    let (test, socket, server) = mirror("final:900", 900);
    set_property(
        &test,
        FS,
        "snapshots",
        &format!("{FS}@restore-5\t800\t1700000001\n{FS}@f-1\t900\t1700000000"),
    );
    for decision in ["adopt", "replay"] {
        let response = post(&socket, "/Volume.Restore", at(1, 14, 0, 0, json!({}))).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!(decision)),
            "{response}"
        );
        assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
        assert_eq!(
            response.pointer("/Ok/copy/writer/phase"),
            Some(&json!("idle"))
        );
        assert_eq!(response.pointer("/Ok/copy/readonly"), Some(&json!(false)));
    }
    assert!(!test.0.join("mirror").exists());
    let snapshots = snapshot_names(&test, ROOT);
    let [restore, finale] = snapshots.as_slice() else {
        panic!("{snapshots:?}");
    };
    assert!(restore.starts_with("restore-") && restore != "restore-5");
    assert_eq!(finale, "f-1");
    assert!(!commands(&test).contains("docker start"));
    server.abort();
}

#[tokio::test]
async fn restore_reopens_a_handed_root_and_keeps_one_restore_snapshot() {
    let (test, socket, server) = promoted();
    fs::write(test.0.join("readonly-volume"), "").unwrap();
    set_property(&test, ROOT, "readonly", "on");
    set_property(&test, ROOT, "ployz:writer", "handed:900");
    for round in 0..3 {
        let response = post(&socket, "/Volume.Restore", at(1, 14, round, 0, json!({}))).await;
        assert_eq!(
            response.pointer("/Ok/copy/writer/phase"),
            Some(&json!("idle")),
            "{response}"
        );
        set_property(&test, ROOT, "ployz:writer", "handed:900");
    }
    let restores = snapshot_names(&test, ROOT)
        .into_iter()
        .filter(|name| name.starts_with("restore-"))
        .count();
    assert_eq!(restores, 1);
    assert!(!test.0.join("readonly-volume").exists());
    server.abort();
}

#[cfg(feature = "verify-faults")]
#[tokio::test]
async fn kill_after_record_worker() {
    let Some(fixture) = std::env::var_os("PLOYZ_TARGET_TEST_FIXTURE") else {
        return;
    };
    let path = PathBuf::from(fixture);
    let storage = VolumeStorage::with_programs(path.join("zpool"), path.join("zfs"));
    let listener = UnixListener::bind(path.join("plugin.sock")).unwrap();
    let server = tokio::spawn(serve(listener, storage));
    let route = std::env::var("PLOYZ_TARGET_TEST_ROUTE").unwrap();
    let request: Value =
        serde_json::from_str(&std::env::var("PLOYZ_TARGET_TEST_REQUEST").unwrap()).unwrap();
    let response = post(&path.join("plugin.sock"), &route, request).await;
    server.abort();
    panic!("the fault did not kill after record: {response}");
}

#[cfg(feature = "verify-faults")]
#[tokio::test]
async fn every_target_verb_finishes_after_a_real_kill_after_record() {
    use std::os::unix::process::ExitStatusExt;

    for verb in [
        "AcceptHandOff",
        "ClearFinal",
        "Promote",
        "StartHandedContainer",
        "Restore",
    ] {
        let (route, request) = match verb {
            "AcceptHandOff" => ("AcceptHandOff", at(1, 9, 0, 0, json!({"guid":900}))),
            "StartHandedContainer" => ("AdmitHandedStart", at(1, 11, 0, 0, json!({}))),
            _ => (verb, at(1, 10, 0, 0, json!({}))),
        };
        let marker = if verb == "Promote" {
            "handed_in:900"
        } else {
            "final:900"
        };
        let (test, socket, server) = if verb == "StartHandedContainer" {
            promoted()
        } else {
            mirror(marker, 900)
        };
        server.abort();
        server.await.unwrap_err();
        fs::remove_file(&socket).unwrap();
        let route = format!("/Volume.{route}");
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "volume_plugin::tests::switch_target_tests::kill_after_record_worker",
                "--nocapture",
            ])
            .env("PLOYZD_FAULT", format!("kill-after-record:{verb}"))
            .env("PLOYZ_TARGET_TEST_FIXTURE", &test.0)
            .env("PLOYZ_TARGET_TEST_ROUTE", &route)
            .env("PLOYZ_TARGET_TEST_REQUEST", request.to_string())
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(6),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(property(&test, RECORD.0, RECORD.1).is_some(), "{verb}");
        let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
        let (socket, server) = serve_storage(&test, storage);
        let response = post(&socket, &route, request).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!("replay")),
            "{verb}: {response}"
        );
        match verb {
            "AcceptHandOff" => assert_eq!(
                property(&test, SLOT, "ployz:mirror").as_deref(),
                Some("handed_in:900")
            ),
            "ClearFinal" => {
                assert_eq!(
                    property(&test, SLOT, "ployz:mirror").as_deref(),
                    Some("idle")
                );
            }
            "Promote" => {
                promoted_by_task(&test).await;
                assert_eq!(property(&test, ROOT, "readonly").as_deref(), Some("off"));
            }
            "StartHandedContainer" => {
                let started = post(
                    &socket,
                    "/Volume.StartHandedContainer",
                    at(1, 11, 0, 0, json!({"container_id": CONTAINER})),
                )
                .await;
                assert_eq!(
                    started.pointer("/Ok/lease/cycle"),
                    Some(&json!("closed")),
                    "{started}"
                );
                assert!(test.0.join("running").exists());
            }
            _ => {
                assert!(test.0.join("volume").exists(), "{response}");
                assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
            }
        }
        server.abort();
    }
}
