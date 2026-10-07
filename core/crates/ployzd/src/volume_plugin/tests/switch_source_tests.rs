//! Source switch effects and crash replay through the Docker plugin routes.

use std::os::unix::fs::PermissionsExt;

use super::lease_tests::set_property;
use super::mirror_tests::{at, commands, property, snapshot_names};
use super::*;

pub(super) const CONTAINER: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub(super) fn programs(test: &TestDir) -> VolumeStorage {
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    fs::write(test.0.join("holders"), format!("{CONTAINER}\n")).unwrap();
    fs::write(test.0.join("registered"), "data\n").unwrap();
    let docker = test.0.join("docker");
    fs::write(&docker, format!(r#"#!/bin/sh
set -eu
fixture='{}'
printf 'docker %s\n' "$*" >> "$fixture/commands"
case "$1 $2" in
  'ps --all') cat "$fixture/holders" ;;
  'stop '*)
    curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data"}}' http://localhost/VolumeDriver.Get > "$fixture/stop-get"
    curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data","ID":"opaque-mount"}}' http://localhost/VolumeDriver.Unmount > /dev/null
    rm -f "$fixture/running" ;;
  'start '*)
    touch "$fixture/starting"
    while [ -e "$fixture/hold-start" ]; do sleep 0.01; done
    response=$(curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data"}}' http://localhost/VolumeDriver.Get)
    printf '%s' "$response" | python3 -c 'import json,sys; sys.exit(bool(json.load(sys.stdin)["Err"]))'
    response=$(curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data","ID":"opaque-mount"}}' http://localhost/VolumeDriver.Mount)
    printf '%s' "$response" > "$fixture/mount-reply"
    printf '%s' "$response" | python3 -c 'import json,sys; sys.exit(bool(json.load(sys.stdin)["Err"]))'
    touch "$fixture/running" ;;
  'rm '*)
    : > "$fixture/holders"
    touch "$fixture/holders-removed"
    while [ -e "$fixture/hold-holder-removal" ]; do sleep 0.01; done ;;
  'volume ls')
    curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{}}' http://localhost/VolumeDriver.List | python3 -c 'import json,sys; print("\n".join(v["Name"] for v in json.load(sys.stdin)["Volumes"]))' ;;
  'volume rm')
    exec 8>"$fixture/docker-volume.lock"
    flock 8
    response=$(curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data"}}' http://localhost/VolumeDriver.Get)
    if printf '%s' "$response" | python3 -c 'import json,sys; sys.exit("Volume" not in json.load(sys.stdin))'; then
      response=$(curl --max-time 5 -sS --unix-socket "$fixture/plugin.sock" -H 'Content-Type: application/json' -d '{{"Name":"data"}}' http://localhost/VolumeDriver.Remove)
      printf '%s' "$response" | python3 -c 'import json,sys; sys.exit(bool(json.load(sys.stdin)["Err"]))'
    fi
    rm -f "$fixture/registered" ;;
  *) exit 2 ;;
esac
"#, test.0.display())).unwrap();
    fs::set_permissions(docker, fs::Permissions::from_mode(0o755)).unwrap();
    VolumeStorage::with_programs(zpool, zfs)
}

pub(super) fn serve_storage(
    test: &TestDir,
    storage: VolumeStorage,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let socket = test.0.join("plugin.sock");
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).unwrap();
    (socket, tokio::spawn(serve(listener, storage)))
}

fn setup(
    writer: &str,
    readonly: bool,
) -> (TestDir, PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    let test = TestDir::new();
    for marker in ["root", "volume", "mounted"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    if readonly {
        fs::write(test.0.join("readonly-volume"), "").unwrap();
    }
    set_property(
        &test,
        "tank/ployz/data",
        "readonly",
        if readonly { "on" } else { "off" },
    );
    set_property(&test, "tank/ployz/data", "ployz:writer", writer);
    set_property(
        &test,
        "tank/ployz/data",
        "snapshots",
        "tank/ployz/data@f-1\t900\t1700000000",
    );
    let storage = programs(&test);
    let (socket, server) = serve_storage(&test, storage);
    (test, socket, server)
}

fn source(seq: u16) -> Value {
    at(1, seq, 0, 0, json!({"container_id": CONTAINER}))
}

#[tokio::test]
async fn withdraw_and_mark_stopping_open_the_cycle_and_replay() {
    for route in ["/Volume.Withdraw", "/Volume.MarkContainerStopping"] {
        let (test, socket, server) = setup("idle", false);
        for decision in ["adopt", "replay"] {
            let response = post(&socket, route, source(5)).await;
            assert_eq!(
                response.pointer("/Ok/decision"),
                Some(&json!(decision)),
                "{response}"
            );
            assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("open")));
            assert_eq!(
                response.pointer("/Ok/copy/writer/phase"),
                Some(&json!("stopping"))
            );
        }
        assert_eq!(
            property(&test, "tank/ployz/data", "ployz:source-container").as_deref(),
            Some(CONTAINER)
        );
        server.abort();
    }
}

#[tokio::test]
async fn freeze_stops_the_holder_and_keeps_one_final_snapshot_on_replay() {
    let (test, socket, server) = setup("stopping", false);
    for decision in ["adopt", "replay"] {
        let response = post(&socket, "/Volume.Freeze", source(6)).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!(decision)),
            "{response}"
        );
        assert_eq!(
            response.pointer("/Ok/copy/writer"),
            Some(&json!({"phase":"frozen","guid":"900"}))
        );
        assert_eq!(response.pointer("/Ok/copy/readonly"), Some(&json!(true)));
    }
    assert!(!test.0.join("mounted").exists());
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["f-1"]);
    server.abort();
}

#[tokio::test]
async fn freeze_creates_the_final_snapshot_after_unmounting() {
    let (test, socket, server) = setup("stopping", false);
    fs::remove_file(test.0.join("props/tank/ployz/data/snapshots")).unwrap();
    let response = post(&socket, "/Volume.Freeze", source(6)).await;
    assert_eq!(
        response.pointer("/Ok/copy/writer"),
        Some(&json!({"phase":"frozen","guid":"1000"})),
        "{response}"
    );
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["f-1"]);
    assert!(!test.0.join("mounted").exists());
    server.abort();
}

#[tokio::test]
async fn freeze_on_busy_mount_restores_writability_and_refuses() {
    let (test, socket, server) = setup("stopping", false);
    fs::write(test.0.join("props/busy-mount"), "").unwrap();
    let response = post(&socket, "/Volume.Freeze", source(6)).await;
    assert_eq!(
        response.pointer("/Err/details/reason"),
        Some(&json!("busy")),
        "{response}"
    );
    assert_eq!(
        property(&test, "tank/ployz/data", "readonly").as_deref(),
        Some("off")
    );
    assert_eq!(
        property(&test, "tank/ployz/data", "ployz:writer").as_deref(),
        Some("stopping")
    );
    assert!(test.0.join("mounted").exists());
    assert!(!commands(&test).contains("zfs snapshot"));
    server.abort();
}

#[tokio::test]
async fn hand_over_matches_the_final_guid_and_thaw_is_refused_without_changing_record() {
    let (test, socket, server) = setup("frozen:900", true);
    let bad = post(
        &socket,
        "/Volume.HandOver",
        at(1, 8, 0, 0, json!({"guid":"901"})),
    )
    .await;
    assert_eq!(
        bad.pointer("/Err/details/reason"),
        Some(&json!("precondition"))
    );
    for decision in ["adopt", "replay"] {
        let response = post(
            &socket,
            "/Volume.HandOver",
            at(1, 8, 0, 0, json!({"guid":"900"})),
        )
        .await;
        assert_eq!(response.pointer("/Ok/decision"), Some(&json!(decision)));
        assert_eq!(
            response.pointer("/Ok/copy/writer/phase"),
            Some(&json!("handed"))
        );
    }
    let response = post(&socket, "/Volume.Thaw", source(13)).await;
    assert_eq!(
        response.pointer("/Err/details/reason"),
        Some(&json!("precondition")),
        "{response}"
    );
    assert_eq!(
        property(&test, "tank/ployz", "ployz:lease.data").as_deref(),
        Some("1:8.0.0:open")
    );
    assert_eq!(
        property(&test, "tank/ployz/data", "readonly").as_deref(),
        Some("on")
    );
    assert!(!commands(&test).contains("docker start"));
    server.abort();
}

#[tokio::test]
async fn thaw_restarts_through_mount_while_open_then_closes_and_preserves_final() {
    let (test, socket, server) = setup("frozen:900", true);
    fs::remove_file(test.0.join("mounted")).unwrap();
    for decision in ["adopt", "replay"] {
        let response = post(&socket, "/Volume.Thaw", source(13)).await;
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
    }
    assert!(test.0.join("running").exists());
    assert_eq!(snapshot_names(&test, "tank/ployz/data"), ["f-1"]);
    assert!(!commands(&test).contains("zfs destroy"));
    server.abort();
}

#[tokio::test]
async fn completed_thaw_replay_does_not_reverse_a_later_container_stop() {
    let (test, socket, server) = setup("frozen:900", true);
    assert!(
        post(&socket, "/Volume.Thaw", source(13))
            .await
            .get("Ok")
            .is_some()
    );
    fs::remove_file(test.0.join("running")).unwrap();
    fs::write(test.0.join("holders"), "").unwrap();
    let response = post(&socket, "/Volume.Thaw", source(13)).await;
    assert_eq!(
        response.pointer("/Ok/decision"),
        Some(&json!("replay")),
        "{response}"
    );
    assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
    assert!(!test.0.join("running").exists());
    server.abort();
}

#[tokio::test]
async fn mount_refuses_an_open_cycle_without_a_live_grant() {
    let (test, socket, server) = setup("idle", false);
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:13.0.0:open");
    let response = post(
        &socket,
        "/VolumeDriver.Mount",
        json!({"Name":"data", "ID":"foreign"}),
    )
    .await;
    assert!(error(&response).contains("VolumeSwitching"), "{response}");
    server.abort();
}

#[tokio::test]
async fn a_foreign_holder_cannot_mount_during_the_thaw_grant() {
    let (test, socket, server) = setup("frozen:900", true);
    fs::write(test.0.join("hold-start"), "").unwrap();
    let thaw_socket = socket.clone();
    let thaw = tokio::spawn(async move { post(&thaw_socket, "/Volume.Thaw", source(13)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.0.join("starting").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fs::write(test.0.join("holders"), format!("{CONTAINER}\nforeign\n")).unwrap();
    let response = post(
        &socket,
        "/VolumeDriver.Mount",
        json!({"Name":"data","ID":"foreign"}),
    )
    .await;
    assert!(error(&response).contains("VolumeSwitching"), "{response}");
    fs::write(test.0.join("holders"), format!("{CONTAINER}\n")).unwrap();
    fs::remove_file(test.0.join("hold-start")).unwrap();
    assert!(thaw.await.unwrap().get("Ok").is_some());
    server.abort();
}

#[tokio::test]
async fn close_replay_leaves_the_reverse_slot_with_its_final_snapshot() {
    let (test, socket, server) = setup("handed:900", true);
    for decision in ["adopt", "replay"] {
        let response = post(&socket, "/Volume.Close", at(1, 12, 0, 0, json!({}))).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!(decision)),
            "{response}"
        );
        assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
        assert_eq!(response.pointer("/Ok/copy/kind"), Some(&json!("slot")));
        assert_eq!(response.pointer("/Ok/copy/readonly"), Some(&json!(true)));
    }
    assert_eq!(snapshot_names(&test, "tank/ployz-mirror/data/fs"), ["f-1"]);
    assert!(!test.0.join("registered").exists());
    assert!(!test.0.join("volume").exists());
    assert!(!commands(&test).contains("zfs destroy"));
    server.abort();
}

#[cfg(feature = "verify-faults")]
#[tokio::test]
async fn kill_after_record_worker() {
    let Some(fixture) = std::env::var_os("PLOYZ_SOURCE_TEST_FIXTURE") else {
        return;
    };
    let path = PathBuf::from(fixture);
    let storage = VolumeStorage::with_programs(path.join("zpool"), path.join("zfs"));
    let listener = UnixListener::bind(path.join("plugin.sock")).unwrap();
    let server = tokio::spawn(serve(listener, storage));
    let route = std::env::var("PLOYZ_SOURCE_TEST_ROUTE").unwrap();
    let request: Value =
        serde_json::from_str(&std::env::var("PLOYZ_SOURCE_TEST_REQUEST").unwrap()).unwrap();
    let response = post(&path.join("plugin.sock"), &route, request).await;
    server.abort();
    panic!("the fault did not kill after record: {response}");
}

#[cfg(feature = "verify-faults")]
#[tokio::test]
async fn every_source_verb_finishes_after_a_real_kill_after_record() {
    use std::os::unix::process::ExitStatusExt;

    for (verb, writer, readonly, request, phase, cycle) in [
        ("Withdraw", "idle", false, source(5), "stopping", "open"),
        (
            "MarkContainerStopping",
            "idle",
            false,
            source(5),
            "stopping",
            "open",
        ),
        ("Freeze", "stopping", false, source(6), "frozen", "open"),
        (
            "HandOver",
            "frozen:900",
            true,
            at(1, 8, 0, 0, json!({"guid":"900"})),
            "handed",
            "open",
        ),
        ("Thaw", "frozen:900", true, source(13), "idle", "closed"),
        (
            "Close",
            "handed:900",
            true,
            at(1, 12, 0, 0, json!({})),
            "idle",
            "closed",
        ),
    ] {
        let (test, socket, server) = setup(writer, readonly);
        server.abort();
        server.await.unwrap_err();
        fs::remove_file(&socket).unwrap();
        let route = format!("/Volume.{verb}");
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "volume_plugin::tests::switch_source_tests::kill_after_record_worker",
                "--nocapture",
            ])
            .env("PLOYZD_FAULT", format!("kill-after-record:{verb}"))
            .env("PLOYZ_SOURCE_TEST_FIXTURE", &test.0)
            .env("PLOYZ_SOURCE_TEST_ROUTE", &route)
            .env("PLOYZ_SOURCE_TEST_REQUEST", request.to_string())
            .output()
            .await
            .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(6),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            property(&test, "tank/ployz/data", "ployz:writer").as_deref(),
            Some(writer)
        );
        let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
        let (socket, server) = serve_storage(&test, storage);
        let response = post(&socket, &route, request).await;
        assert_eq!(
            response.pointer("/Ok/decision"),
            Some(&json!("replay")),
            "{verb}: {response}"
        );
        assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!(cycle)));
        let actual = response
            .pointer("/Ok/copy/writer/phase")
            .or_else(|| response.pointer("/Ok/copy/mirror/phase"));
        assert_eq!(actual, Some(&json!(phase)), "{verb}: {response}");
        if verb == "Close" {
            assert_eq!(snapshot_names(&test, "tank/ployz-mirror/data/fs"), ["f-1"]);
        }
        server.abort();
    }
}

#[tokio::test]
async fn concurrent_docker_remove_refuses_without_blocking_close() {
    let (test, socket, server) = setup("handed:900", true);
    fs::write(test.0.join("hold-holder-removal"), "").unwrap();
    let close = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Volume.Close", at(1, 12, 0, 0, json!({}))).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.0.join("holders-removed").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let get = post(&socket, "/VolumeDriver.Get", json!({"Name":"data"})).await;
    assert!(
        get.get("Volume").is_some(),
        "concurrent Docker Remove resolved the root before rename"
    );
    let removed = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::process::Command::new("flock")
            .arg(test.0.join("docker-volume.lock"))
            .args(["curl", "--max-time", "5", "-sS", "--unix-socket"])
            .arg(&socket)
            .args([
                "-H",
                "Content-Type: application/json",
                "-d",
                r#"{"Name":"data"}"#,
                "http://localhost/VolumeDriver.Remove",
            ])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    fs::remove_file(test.0.join("hold-holder-removal")).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(7), close)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        closed.pointer("/Ok/lease/cycle"),
        Some(&json!("closed")),
        "{closed}"
    );
    let removed = removed
        .expect("Docker Remove must refuse before Close awaits Docker's per-volume lock")
        .unwrap();
    assert!(removed.status.success());
    let response: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert!(error(&response).contains("VolumeSwitching"), "{response}");
    assert_eq!(snapshot_names(&test, "tank/ployz-mirror/data/fs"), ["f-1"]);
    server.abort();
}

#[tokio::test]
async fn docker_remove_queued_behind_close_refuses_once_close_runs_docker() {
    let (test, socket, server) = setup("handed:900", true);
    for marker in ["hold-list", "hold-holder-removal"] {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let list = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/VolumeDriver.List", json!({})).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test.0.join("list-held").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let close = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Volume.Close", at(1, 12, 0, 0, json!({}))).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let remove = tokio::process::Command::new("flock")
        .arg(test.0.join("docker-volume.lock"))
        .args(["curl", "--max-time", "5", "-sS", "--unix-socket"])
        .arg(&socket)
        .args([
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"Name":"data"}"#,
            "http://localhost/VolumeDriver.Remove",
        ])
        .kill_on_drop(true)
        .output();
    let remove = tokio::spawn(remove);
    tokio::time::sleep(Duration::from_millis(300)).await;
    fs::remove_file(test.0.join("hold-list")).unwrap();
    assert_eq!(error(&list.await.unwrap()), "");

    let removed = tokio::time::timeout(Duration::from_secs(2), remove)
        .await
        .expect("Remove must refuse once Close holds the mutation and runs Docker")
        .unwrap()
        .unwrap();
    fs::remove_file(test.0.join("hold-holder-removal")).unwrap();
    assert!(removed.status.success());
    let response: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert!(
        error(&response).contains("has an active storage mutation"),
        "{response}"
    );
    let closed = tokio::time::timeout(Duration::from_secs(7), close)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        closed.pointer("/Ok/lease/cycle"),
        Some(&json!("closed")),
        "{closed}"
    );
    server.abort();
}

#[cfg(feature = "verify-faults")]
#[tokio::test]
async fn close_finishes_docker_deregistration_after_a_kill_between_rename_and_remove() {
    use std::os::unix::process::ExitStatusExt;

    let (test, socket, server) = setup("handed:900", true);
    server.abort();
    server.await.unwrap_err();
    fs::remove_file(&socket).unwrap();
    let request = at(1, 12, 0, 0, json!({}));
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "volume_plugin::tests::switch_source_tests::kill_after_record_worker",
            "--nocapture",
        ])
        .env("PLOYZD_FAULT", "kill-daemon:Close")
        .env("PLOYZ_SOURCE_TEST_FIXTURE", &test.0)
        .env("PLOYZ_SOURCE_TEST_ROUTE", "/Volume.Close")
        .env("PLOYZ_SOURCE_TEST_REQUEST", request.to_string())
        .output()
        .await
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!test.0.join("volume").exists());
    assert!(test.0.join("registered").exists());
    assert_eq!(snapshot_names(&test, "tank/ployz-mirror/data/fs"), ["f-1"]);
    let storage = VolumeStorage::with_programs(test.0.join("zpool"), test.0.join("zfs"));
    let (socket, server) = serve_storage(&test, storage);
    let response = post(&socket, "/Volume.Close", request).await;
    assert_eq!(
        response.pointer("/Ok/decision"),
        Some(&json!("replay")),
        "{response}"
    );
    assert_eq!(response.pointer("/Ok/lease/cycle"), Some(&json!("closed")));
    assert!(!test.0.join("registered").exists());
    assert_eq!(snapshot_names(&test, "tank/ployz-mirror/data/fs"), ["f-1"]);
    server.abort();
}
