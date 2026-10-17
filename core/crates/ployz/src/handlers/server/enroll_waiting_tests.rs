//! Selection and wire contracts for waiting enrollment.
use super::*;

fn host(id: &str, name: &str, at: &str) -> Waiting {
    let mut host = super::super::super::waiting::tests::waiting("waiting", None);
    host.row.id = id.into();
    host.row.name = name.into();
    *host.raw.pointer_mut("/report/hostname").unwrap() = json!(name);
    *host.raw.get_mut("first_seen_at").unwrap() = json!(at);
    host
}

fn matches(args: &[&str]) -> ArgMatches {
    crate::cli::command()
        .try_get_matches_from(["ployz", "server", "enroll"].iter().chain(args))
        .unwrap()
}

#[test]
fn enroll_waiting_matches_names_and_globs_never_ids() {
    let hosts = [
        host("opaque-id", "host-a", "2026-01-01"),
        host("other", "db", "2026-01-02"),
    ];
    assert_eq!(
        selected(&hosts, &Selection::parse(Some("host-*")).unwrap(), None)
            .first()
            .unwrap()
            .row
            .name,
        "host-a"
    );
    assert!(selected(&hosts, &Selection::parse(Some("opaque-id")).unwrap(), None).is_empty());
    assert!(Selection::parse(Some("[")).is_err());
    assert!(selected(&hosts, &Selection::parse(Some("none")).unwrap(), None).is_empty());
}

#[test]
fn enroll_waiting_first_means_the_oldest_match() {
    let hosts = [
        host("new", "host-new", "2026-01-01T00:00:00-01:00"),
        host("old", "host-old", "2026-01-01T00:30:00Z"),
    ];
    let selected = selected(&hosts, &Selection::parse(None).unwrap(), Some(""));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected.first().unwrap().row.id, "old");
}

#[test]
fn enroll_waiting_sends_names_and_storage_to_cloud_without_planning_locally() {
    let hosts = [
        host("old", "host-old", "2026-01-01"),
        host("new", "host-new", "2026-01-02"),
    ];
    let selected = hosts.iter().collect::<Vec<_>>();
    let parsed = matches(&[
        "host-*",
        "--name",
        "web-{n}",
        "--no-spare-disk",
        "--no-fallback",
        "--yes",
    ]);
    let body = request(leaf_matches(&parsed), &selected).unwrap();
    assert_eq!(
        body,
        json!({ "ids": ["old", "new"], "first": "old", "names": { "kind": "pattern", "pattern": "web-{n}" }, "storage": { "kind": "zfs", "use_spare_disk": false, "fallback_to_docker": false }, "runs": { "services": true, "ingress": true, "builds": true } })
    );
    let parsed = matches(&["--all", "--storage", "none", "--first", "host-new"]);
    let body = request(leaf_matches(&parsed), &selected).unwrap();
    assert_eq!(body.get("first").unwrap(), "new");
    assert_eq!(body.get("names").unwrap(), &json!({ "kind": "hostnames" }));
    assert_eq!(body.get("storage").unwrap(), &json!({ "kind": "none" }));
    let parsed = matches(&["--all", "--first", "missing"]);
    assert!(request(leaf_matches(&parsed), &selected).is_err());
}

fn plan() -> Plan {
    serde_json::from_value(json!({ "plan": [{ "waiting_server_id": "a", "hostname": "host-a", "role": "founder", "name": "web-1", "storage": { "kind": "zfs" } }, { "waiting_server_id": "b", "hostname": "host-b", "role": "joiner", "name": "web-2", "storage": { "kind": "none" } }], "skipped": [{ "id": "c", "reason": "Port 443 is in use" }], "spread": { "providers": 2, "networks": 1 } })).unwrap()
}

#[test]
fn enroll_waiting_prints_clouds_founder_names_storage_and_skip_reasons() {
    let hosts = [
        host("a", "host-a", "1"),
        host("b", "host-b", "2"),
        host("c", "host-c", "3"),
    ];
    let selected = hosts.iter().collect::<Vec<_>>();
    let mut output = Vec::new();
    plan_table(&plan(), &selected)
        .write(&mut output, false)
        .unwrap();
    let output = String::from_utf8(output).unwrap();
    for expected in [
        "host-a",
        "203.0.113.9",
        "Founder",
        "web-1",
        "web-2",
        "ZFS on main disk",
        "Docker volumes",
        "host-c",
        "Skipped",
        "Port 443 is in use",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    assert!(!output.contains("awaiting_founder"));
}

#[test]
fn enroll_waiting_yes_bypasses_the_plan_question() {
    let plan = plan();
    assert!(review(&plan, &[], true, false).is_ok());
    if !ui::can_prompt() {
        let error = review(&plan, &[], false, false).unwrap_err();
        assert_eq!(error.report().code, RpcErrorCode::ConfirmationRequired);
        assert!(error.json().to_string().contains("web-1"));
    }
}

fn attempt(state: &str, stage: Option<&str>, queued: bool, ahead: Option<u64>) -> Attempt {
    serde_json::from_value(json!({ "id": "attempt", "waiting_server_id": "a", "name": "web-1", "role": "joiner", "state": state, "stage": stage, "queued": queued, "ahead": ahead })).unwrap()
}

#[test]
fn enroll_waiting_storage_readiness_and_join_line_are_distinct() {
    assert_eq!(
        attempt("instructed", None, true, None).label(),
        "Queued · Preparing storage"
    );
    assert_eq!(
        attempt("running", Some("queued"), false, Some(3)).label(),
        "In line · 3 servers ahead"
    );
    assert_eq!(
        attempt("running", Some("queued"), false, Some(0)).label(),
        "In line · 0 servers ahead"
    );
    assert_eq!(
        attempt("running", Some("queued"), false, None).label(),
        "Enrolling · Join position unknown"
    );
    assert_eq!(
        attempt("running", Some("enrolling"), false, None).label(),
        "Enrolling · Join position unknown"
    );
    assert_eq!(
        attempt("awaiting_founder", None, false, None).label(),
        "Waiting for the first server"
    );
    assert_eq!(
        attempt("joined", Some("queued"), false, Some(3)).label(),
        "Joined"
    );
    let mut failed = attempt("failed", Some("storage"), false, None);
    failed.failure = Some(Failure {
        message: "ZFS install refused".into(),
    });
    assert_eq!(failed.label(), "Failed · ZFS install refused");
}

#[tokio::test]
async fn enroll_waiting_plan_route_has_no_trailing_slash_and_start_refusal_survives() {
    let cloud = crate::cloud_login::tests::fake_cloud(|route, _| match route {
        "POST /api/cli/enrollments/plan" => (200, serde_json::to_value(plan()).unwrap()),
        "POST /api/cli/enrollments" => (
            409,
            json!({ "error": { "code": "conflict", "message": "Cloud can't remove an earlier server's pairing. Use Forget Servers.", "details": {} } }),
        ),
        other => panic!("unexpected {other}"),
    });
    let credential = Credential::Token {
        cloud,
        token: crate::cloud_login::Secret::new("ployz_secret".into()),
    };
    let _: Plan = cloud_account::refusable(
        &credential,
        Method::POST,
        "enrollments/plan",
        Some(&json!({})),
    )
    .await
    .unwrap();
    let error = cloud_account::refusable::<Value>(
        &credential,
        Method::POST,
        "enrollments",
        Some(&json!({})),
    )
    .await
    .unwrap_err();
    let error = Error::from(error);
    assert_eq!(error.report().code, RpcErrorCode::Conflict);
    assert!(error.to_string().contains("Use Forget Servers"), "{error}");
}

#[test]
fn enroll_waiting_unknown_state_ends_follow_instead_of_waiting_forever() {
    let row = attempt("future_state", None, false, None);
    assert!(row.ended());
    assert!(row.state != State::Joined);
}
