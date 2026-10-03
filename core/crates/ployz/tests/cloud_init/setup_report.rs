//! The best-effort setup report `server add --token` sends Cloud after it succeeds or fails.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::harness::{
    self, EnrollListen, JoinDaemon, PAIRING, ReportReply, TOKEN, registration, serve_local_machine,
};

fn at<'value>(value: &'value Value, pointer: &str) -> &'value Value {
    value.pointer(pointer).unwrap_or(&Value::Null)
}

fn step_names(report: &Value) -> Vec<&str> {
    at(report, "/steps")
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            assert!(at(step, "/seconds").as_f64().unwrap() >= 0.0, "{step}");
            at(step, "/name").as_str().unwrap()
        })
        .collect()
}

/// Join on this host through its local socket, as the pasted command does.
async fn join_locally(do_not_track: bool) -> (std::process::Output, EnrollListen) {
    let mut registration = registration();
    registration.assigned_machine.accepts_ingress = false;
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "none",
        "pairing": { "secret": PAIRING },
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration);
    let (connect, socket, _) = serve_local_machine(daemon).await;
    let mut command = harness::cli();
    command.args([
        "--connect",
        &connect,
        "server",
        "add",
        "--token",
        TOKEN,
        "--cloud-url",
        &enroll.url,
        "--name",
        "joiner",
        "--accepts-ingress=false",
        "--yes",
    ]);
    if do_not_track {
        command.env("DO_NOT_TRACK", "1");
    }
    let output = command.output().await.unwrap();
    let _ = std::fs::remove_file(socket);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (output, enroll)
}

#[tokio::test]
async fn success_reports_the_profile_and_every_step_after_the_callback() {
    let (_, enroll) = join_locally(false).await;

    let reports = enroll.reports();
    let [(completions, report)] = reports.as_slice() else {
        panic!("expected one report: {reports:?}");
    };
    assert_eq!(*completions, 1, "the report follows the final callback");
    assert_eq!(at(report, "/outcome"), "succeeded");
    assert_eq!(step_names(report), ["install", "enroll", "storage", "join"]);
    assert!(at(report, "/totalSeconds").as_f64().unwrap() >= 0.0);
    assert!(report.get("failedStep").is_none(), "{report}");
    let profile = at(report, "/profile");
    assert_eq!(at(profile, "/ployzVersion"), env!("CARGO_PKG_VERSION"));
    assert_eq!(at(profile, "/storage"), "none");
    assert_eq!(at(profile, "/founder"), false);
    assert_eq!(at(profile, "/arch"), std::env::consts::ARCH);
    assert!(at(profile, "/cpuCount").as_u64().unwrap() >= 1, "{profile}");
    assert!(at(profile, "/provider").is_string(), "{profile}");

    let sent = report.to_string();
    let machine = registration().assigned_machine;
    for private in ["joiner", machine.id.as_str(), PAIRING, TOKEN, "127.0.0.1"] {
        assert!(!sent.contains(private), "{private} leaked: {sent}");
    }
}

#[tokio::test]
async fn do_not_track_sends_no_report() {
    let (_, enroll) = join_locally(true).await;

    assert_eq!(enroll.callbacks().len(), 1);
    assert!(enroll.reports().is_empty(), "{:?}", enroll.reports());
}

#[tokio::test]
async fn a_run_dialing_another_host_reports_no_host_profile() {
    let enroll = EnrollListen::start(json!({ "kind": "join" })).await;
    // Nothing listens on port 1: dialing fails at once, before any step finishes.
    let output = harness::cli()
        .args([
            "--connect",
            "tcp://127.0.0.1:1",
            "server",
            "add",
            "--token",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--yes",
        ])
        .output()
        .await
        .unwrap();

    assert!(!output.status.success());
    let reports = enroll.reports();
    let [(_, report)] = reports.as_slice() else {
        panic!("expected one report: {reports:?}");
    };
    assert_eq!(at(report, "/outcome"), "failed");
    assert_eq!(at(report, "/failedStep"), "install");
    let profile = at(report, "/profile");
    for host in [
        "provider",
        "osId",
        "kernel",
        "arch",
        "virtualization",
        "cpuCount",
    ] {
        assert!(profile.get(host).is_none(), "{host} sent: {profile}");
    }
}

/// Enroll in-process with a storage step that refuses ZFS, against `reply`.
async fn refuse_zfs(reply: ReportReply) -> (String, Vec<(usize, Value)>, Duration) {
    let mut registration = registration();
    registration.assigned_machine.accepts_ingress = false;
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "storage": "zfs",
        "pairing": { "secret": PAIRING },
        "registration": registration,
    }))
    .await;
    enroll.set_report_reply(reply);
    let daemon = JoinDaemon::new(registration);
    let (connect, socket, _) = serve_local_machine(daemon).await;
    let matches = ployz::cli::command()
        .try_get_matches_from([
            "ployz",
            "--connect",
            &connect,
            "server",
            "add",
            "--token",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "joiner",
            "--accepts-ingress=false",
            "--yes",
        ])
        .unwrap();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        ployz::handlers::cloud_enroll_with_installer(&matches, &|_| {
            std::future::ready(Err(ployz::handlers::Error::usage("ZFS refused")))
        })
    })
    .await
    .unwrap();
    let elapsed = started.elapsed();
    let _ = std::fs::remove_file(socket);
    (result.unwrap_err().to_string(), enroll.reports(), elapsed)
}

#[tokio::test]
async fn failed_storage_reports_the_step_and_error_once() {
    let (error, reports, _) = refuse_zfs(ReportReply::Status(200)).await;

    assert_eq!(error, "ZFS refused");
    let [(completions, report)] = reports.as_slice() else {
        panic!("expected one report: {reports:?}");
    };
    assert_eq!(*completions, 0);
    assert_eq!(at(report, "/outcome"), "failed");
    assert_eq!(at(report, "/failedStep"), "storage");
    assert!(at(report, "/failedStepSeconds").as_f64().unwrap() >= 0.0);
    assert_eq!(at(report, "/error"), "ZFS refused");
    assert_eq!(step_names(report), ["install", "enroll"]);
    assert_eq!(at(report, "/profile/storage"), "zfs");
}

#[tokio::test]
async fn a_rejected_or_hung_report_leaves_the_outcome_unchanged() {
    for reply in [ReportReply::Status(500), ReportReply::Hang] {
        let (error, reports, elapsed) = refuse_zfs(reply).await;

        assert_eq!(error, "ZFS refused");
        assert_eq!(reports.len(), 1);
        assert!(
            elapsed < Duration::from_secs(6),
            "a hung report must give up quickly: {elapsed:?}"
        );
    }
}
