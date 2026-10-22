//! `org rm` and `server forget` of every Server against a fake Cloud that asks a human
//! first: under `--json` each answers `approval_required` naming the retry, and the
//! retry carries the approval. An `org rm` a Server leaves partial names a retry that
//! carries the approval it ran with.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

const ASKED_REMOVE: &str = r#"{"error":{"code":"approval_required","message":"A human must approve this remove of acme before it runs.","details":{"approval_id":"apr_9","approval":"remove:abc","effects":[{"kind":"removes_organization","name":"acme","node":"o1","path":"organizations/o1"}],"operation":{"verb":"remove","name":"acme"},"organization":"acme"}}}"#;
const ASKED_FORGET: &str = r#"{"error":{"code":"approval_required","message":"A human must approve this forget of acme before it runs.","details":{"approval_id":"apr_9","approval":"forget:abc","effects":[{"kind":"removes_server","name":"web-1","node":"b","path":"servers/b"}],"operation":{"verb":"forget","name":"acme"},"organization":"acme"}}}"#;

/// Each approval-gated request's route, with the approval it carried.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// How Cloud answers a read of approval `apr_9`.
#[derive(Clone, Copy)]
enum Answer {
    Approved,
    /// Drops the connection unanswered, as a Cloud gone mid-wait does.
    Gone,
    /// Answers `404 {}`, as a Cloud without the approvals route does.
    Missing,
    /// Answers `503` with a body, as a Cloud failing for the moment does.
    Failing,
    /// Answers `401`, as Cloud does to a refused token or an ended sign-in.
    Unauthorized,
    /// Answers a bare `403`, as Cloud does to a sign-in that left its Organization.
    Forbidden,
}

/// Cloud that asks for approval `apr_9` before it removes Organization acme or forgets
/// its Servers, recording each such request in `seen`. Unless `confirms`, a Server
/// never confirms unpairing, so the approved removal stays partial.
fn fake_cloud(seen: Seen, confirms: bool) -> String {
    fake_cloud_answering(seen, confirms, Answer::Approved)
}

fn fake_cloud_answering(seen: Seen, confirms: bool, answer: Answer) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut length, mut approval) = (0, None);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if let Some(value) = lower.strip_prefix("x-ployz-approval:") {
                    approval = Some(value.trim().to_owned());
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let route = line.rsplit_once(' ').unwrap().0.to_owned();
            if matches!(answer, Answer::Gone) && route == "GET /api/cli/approvals/apr_9" {
                continue;
            }
            let approved = approval.as_deref() == Some("apr_9");
            let (status, reply) = match route.as_str() {
                "DELETE /api/cli/organizations/o1" if approved => (
                    "200 OK",
                    json!({
                        "organization": "acme",
                        "removed": confirms,
                        "servers": {
                            "confirmed": [],
                            "unconfirmed": if confirms { vec![] } else { vec!["0123456789abcdef0123456789abcdef"] },
                        },
                    })
                    .to_string(),
                ),
                "GET /api/cli/approvals/apr_9" if matches!(answer, Answer::Missing) => {
                    ("404 Not Found", "{}".to_owned())
                }
                "GET /api/cli/approvals/apr_9" if matches!(answer, Answer::Failing) => {
                    ("503 Service Unavailable", "upstream down".to_owned())
                }
                "GET /api/cli/approvals/apr_9" if matches!(answer, Answer::Unauthorized) => {
                    ("401 Unauthorized", "{}".to_owned())
                }
                "GET /api/cli/approvals/apr_9" | "POST /api/cli/logout"
                    if matches!(answer, Answer::Forbidden) =>
                {
                    ("403 Forbidden", "{}".to_owned())
                }
                "GET /api/cli/approvals/apr_9" => (
                    "200 OK",
                    json!({ "approval": { "status": "approved" } }).to_string(),
                ),
                "DELETE /api/cli/organizations/o1" => ("409 Conflict", ASKED_REMOVE.to_owned()),
                "POST /api/cli/forget-servers" if approved => (
                    "200 OK",
                    json!({ "organization": "acme", "servers": [], "volumes": [], "cancelled": [] }).to_string(),
                ),
                "POST /api/cli/forget-servers" => ("409 Conflict", ASKED_FORGET.to_owned()),
                "POST /api/cli/server-access" => (
                    "200 OK",
                    json!({ "connections": [], "unreachable": ["a".repeat(32)] }).to_string(),
                ),
                "GET /api/cli/forget-servers" => (
                    "200 OK",
                    json!({
                        "organization": "acme",
                        "servers": [{ "id": "a".repeat(32), "name": "web-1", "reach": "didnt_answer" }],
                        "volumes": [],
                    })
                    .to_string(),
                ),
                "POST /api/auth/device/code" => (
                    "200 OK",
                    json!({
                        "device_code": "device-secret",
                        "user_code": "ABCD2345",
                        "verification_uri": "http://cloud/device",
                        "verification_uri_complete": "http://cloud/device?user_code=ABCD2345",
                        "expires_in": 1800,
                        "interval": 1,
                    })
                    .to_string(),
                ),
                "POST /api/auth/device/token" => (
                    "200 OK",
                    json!({ "access_token": "session-token" }).to_string(),
                ),
                "GET /api/auth/get-session" => (
                    "200 OK",
                    json!({
                        "user": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
                        "session": { "activeOrganizationId": "o1", "activeOrganizationSlug": "acme" },
                    })
                    .to_string(),
                ),
                "GET /api/cli/organizations" => (
                    "200 OK",
                    json!({ "organizations": [{ "id": "o1", "slug": "acme", "name": "Acme", "current": true }] })
                        .to_string(),
                ),
                _ => ("404 Not Found", "{}".to_owned()),
            };
            if route.starts_with("DELETE /api/cli/organizations/")
                || route == "POST /api/cli/forget-servers"
            {
                seen.lock().unwrap().push((route, approval));
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    url
}

fn ployz(cloud: &str, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let mut command = run(&dir.path().join("config.yaml"), args);
    command
        .env("PLOYZ_TOKEN", "ployz_acme")
        .env("PLOYZ_CLOUD_URL", cloud)
        .output()
        .unwrap()
}

/// `ployz` signed in to `cloud` as device `d1` of acme, its config under `dir`.
fn signed_in(cloud: &str, dir: &std::path::Path, args: &[&str]) -> Output {
    let config = dir.join("config.yaml");
    for login in [
        &["login", "--json", "--cloud-url", cloud][..],
        &["login", "--json"],
    ] {
        let output = run(&config, login).output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    run(&config, args).output().unwrap()
}

fn run(config: &std::path::Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .args(["--ployz-config", config.to_str().unwrap()])
        .args(args)
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CLOUD_URL")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy");
    command
}

fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn org_rm_and_forgetting_every_server_wait_on_an_approval_their_retry_carries() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), true);
    let cases: [(&[&str], &str, &str); 2] = [
        (
            &["org", "rm", "acme", "--confirm", "acme"],
            "DELETE /api/cli/organizations/o1",
            "org rm acme --confirm acme --approval apr_9",
        ),
        (
            &["server", "forget", "--confirm", "acme"],
            "POST /api/cli/forget-servers",
            "server forget --confirm acme --approval apr_9",
        ),
    ];
    for (args, route, retry) in cases {
        let asked = json_of(&ployz(&cloud, &[&["--json"], args].concat()));
        assert_eq!(
            asked.pointer("/error/code"),
            Some(&json!("approval_required")),
            "{asked}"
        );
        let named = asked
            .pointer("/error/details/retry")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(named.ends_with(retry), "{named}");
        assert!(named.starts_with("ployz --ployz-config "), "{named}");

        let approved = ployz(
            &cloud,
            &[&["--json"], args, &["--approval", "apr_9"]].concat(),
        );
        assert!(approved.status.success(), "{}", json_of(&approved));
        assert_eq!(
            seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
            [
                (route.to_owned(), None),
                (route.to_owned(), Some("apr_9".to_owned()))
            ],
        );
    }
}

#[test]
fn forgetting_every_server_says_it_is_asking_cloud_except_under_json() {
    let cloud = fake_cloud(Arc::default(), true);

    let asked = ployz(&cloud, &["server", "forget"]);
    assert_eq!(asked.status.code(), Some(2), "{asked:?}");
    let stderr = String::from_utf8_lossy(&asked.stderr);
    assert!(
        stderr.starts_with("Asking Cloud which Servers to forget…\n"),
        "{stderr}"
    );
    assert!(stderr.contains("Organization acme (web-1)"), "{stderr}");

    let json = ployz(&cloud, &["--json", "server", "forget"]);
    assert_eq!(
        json_of(&json).pointer("/error/code"),
        Some(&json!("confirmation_required"))
    );
    let stderr = String::from_utf8_lossy(&json.stderr);
    assert!(!stderr.contains("Asking Cloud"), "{stderr}");
}

#[test]
fn forgetting_a_lone_server_that_does_not_answer_says_it_is_asking_cloud() {
    let cloud = fake_cloud(Arc::default(), true);

    let asked = ployz(&cloud, &["server", "forget", "web-1"]);
    assert_eq!(asked.status.code(), Some(2), "{asked:?}");
    let stderr = String::from_utf8_lossy(&asked.stderr);
    assert_eq!(
        stderr
            .matches("Asking Cloud which Servers to forget…")
            .count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("It needs dead typed"), "{stderr}");

    let forgot = ployz(&cloud, &["server", "forget", "web-1", "--confirm", "dead"]);
    assert!(forgot.status.success(), "{forgot:?}");
    let stderr = String::from_utf8_lossy(&forgot.stderr);
    assert_eq!(
        stderr
            .matches("Asking Cloud which Servers to forget…")
            .count(),
        1,
        "{stderr}"
    );
    asks_to_forget_around_the_approval(&stderr);
    assert!(
        stderr.contains(
            "A forgotten Server doesn't know it was forgotten. If one runs again as it is, its \
             services start again on its own copy of each Volume"
        ),
        "{stderr}"
    );
}

#[test]
fn forgetting_every_server_says_it_asks_cloud_to_forget_them_before_each_send_except_under_json() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), true);

    let forgot = ployz(&cloud, &["server", "forget", "--confirm", "acme"]);
    assert!(forgot.status.success(), "{forgot:?}");
    asks_to_forget_around_the_approval(&String::from_utf8_lossy(&forgot.stderr));
    assert_eq!(seen.lock().unwrap().drain(..).count(), 2);

    let approved = ployz(
        &cloud,
        &[
            "server",
            "forget",
            "--confirm",
            "acme",
            "--approval",
            "apr_9",
        ],
    );
    assert!(approved.status.success(), "{approved:?}");
    let stderr = String::from_utf8_lossy(&approved.stderr);
    assert_eq!(
        stderr
            .matches("Asking Cloud to forget the Servers…")
            .count(),
        1,
        "{stderr}"
    );

    let json = ployz(
        &cloud,
        &[
            "--json",
            "server",
            "forget",
            "--confirm",
            "acme",
            "--approval",
            "apr_9",
        ],
    );
    assert!(json.status.success(), "{}", json_of(&json));
    let stderr = String::from_utf8_lossy(&json.stderr);
    assert!(!stderr.contains("Asking Cloud"), "{stderr}");
}

/// The forget note before the first send, then the approval line, then the note again
/// before the send that carries the approval.
fn asks_to_forget_around_the_approval(stderr: &str) {
    let note = "Asking Cloud to forget the Servers…";
    let (before, after) = stderr
        .split_once("Waiting for approval in Ployz Cloud… approved.")
        .unwrap_or_else(|| panic!("no approval line: {stderr}"));
    assert_eq!(before.matches(note).count(), 1, "{stderr}");
    assert_eq!(after.matches(note).count(), 1, "{stderr}");
}

#[test]
fn a_partial_org_rm_names_a_retry_that_carries_its_approval() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), false);
    let args = ["org", "rm", "acme", "--confirm", "acme"];

    let passed = ployz(
        &cloud,
        &[&["--json"], &args[..], &["--approval", "apr_9"]].concat(),
    );
    assert_eq!(passed.status.code(), Some(3), "{}", json_of(&passed));
    let next = json_of(&passed);
    let next = next
        .pointer("/next")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        next.ends_with("org rm acme --confirm acme --approval apr_9"),
        "{next}"
    );

    let settled = ployz(&cloud, &args);
    assert_eq!(settled.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&settled.stderr);
    assert!(
        stderr.contains("org rm acme --confirm acme --approval apr_9"),
        "{stderr}"
    );
    assert_eq!(
        seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
        [
            (
                "DELETE /api/cli/organizations/o1".to_owned(),
                Some("apr_9".to_owned())
            ),
            ("DELETE /api/cli/organizations/o1".to_owned(), None),
            (
                "DELETE /api/cli/organizations/o1".to_owned(),
                Some("apr_9".to_owned())
            ),
        ],
    );
}

#[test]
fn waiting_for_an_approval_names_it_and_where_to_answer_it() {
    let cloud = fake_cloud(Arc::default(), true);
    let waited = ployz(&cloud, &["org", "rm", "acme", "--confirm", "acme"]);
    assert!(waited.status.success(), "{waited:?}");
    let stderr = String::from_utf8_lossy(&waited.stderr);
    assert!(
        stderr.contains(&format!(
            "Approve apr_9 under Waiting on you in the Ployz agent at {cloud}/cloud/acme?chat="
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("next: ployz org approval approve apr_9"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Waiting for approval in Ployz Cloud… approved."),
        "{stderr}"
    );
}

#[test]
fn losing_cloud_while_waiting_leaves_the_approval_pending_and_names_the_retry() {
    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Gone);
    let lost = ployz(&cloud, &["org", "rm", "acme", "--confirm", "acme"]);
    assert_eq!(lost.status.code(), Some(1), "{lost:?}");
    let stderr = String::from_utf8_lossy(&lost.stderr);
    assert!(
        stderr.contains("Stopped waiting for approval apr_9; it stays pending in Ployz Cloud."),
        "{stderr}"
    );
    assert!(stderr.contains("\n  cause: "), "{stderr}");
    let retry = stderr
        .lines()
        .find_map(|line| line.strip_prefix("retry: "))
        .unwrap_or_default();
    assert!(
        retry.ends_with("org rm acme --confirm acme --approval apr_9"),
        "{stderr}"
    );

    let json = json_of(&ployz(
        &cloud,
        &["--json", "org", "rm", "acme", "--confirm", "acme"],
    ));
    assert_eq!(
        json.pointer("/error/code"),
        Some(&json!("approval_required")),
        "{json}"
    );
}

#[test]
fn an_answered_error_while_waiting_names_the_approval_without_calling_it_pending() {
    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Missing);
    let missing = ployz(&cloud, &["org", "rm", "acme", "--confirm", "acme"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("error: Couldn't check approval apr_9."),
        "{stderr}"
    );
    assert!(stderr.contains("does not offer CLI access"), "{stderr}");
    assert!(!stderr.contains("stays pending"), "{stderr}");
    assert!(!stderr.contains("retry: "), "{stderr}");

    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Failing);
    let failing = ployz(&cloud, &["org", "rm", "acme", "--confirm", "acme"]);
    assert_eq!(failing.status.code(), Some(1), "{failing:?}");
    let stderr = String::from_utf8_lossy(&failing.stderr);
    assert!(
        stderr.contains("error: Couldn't check approval apr_9."),
        "{stderr}"
    );
    assert!(!stderr.contains("stays pending"), "{stderr}");
    let retry = stderr
        .lines()
        .find_map(|line| line.strip_prefix("retry: "))
        .unwrap_or_default();
    assert!(
        retry.ends_with("org rm acme --confirm acme --approval apr_9"),
        "{stderr}"
    );
}

/// Cloud refused the credential reading approval `apr_9`: that answers nothing about the
/// approval, and the same credential can't retry it.
fn assert_unchecked(refused: &Output, cause: &str, next: &str) {
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("error: Couldn't check approval apr_9."),
        "{stderr}"
    );
    assert!(stderr.contains(cause), "{stderr}");
    assert!(stderr.lines().any(|line| line == next), "{stderr}");
    assert!(!stderr.contains("stays pending"), "{stderr}");
    assert!(!stderr.contains("retry: "), "{stderr}");
}

const ORG_RM: [&str; 5] = ["org", "rm", "acme", "--confirm", "acme"];

#[test]
fn a_refused_token_while_waiting_leaves_the_approval_unchecked_without_a_retry() {
    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Unauthorized);
    assert_unchecked(
        &ployz(&cloud, &ORG_RM),
        "Cloud refused PLOYZ_TOKEN",
        "next: unset PLOYZ_TOKEN && ployz login",
    );
}

#[test]
fn an_ended_sign_in_while_waiting_leaves_the_approval_unchecked_without_a_retry() {
    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Unauthorized);
    let dir = tempfile::tempdir().unwrap();
    assert_unchecked(
        &signed_in(&cloud, dir.path(), &ORG_RM),
        "Cloud ended this device's sign-in",
        "next: ployz login",
    );
}

#[test]
fn leaving_the_organization_while_waiting_leaves_the_approval_unchecked_without_a_retry() {
    let cloud = fake_cloud_answering(Arc::default(), true, Answer::Forbidden);
    let dir = tempfile::tempdir().unwrap();
    assert_unchecked(
        &signed_in(&cloud, dir.path(), &ORG_RM),
        "Organization acme is no longer one of yours",
        "next: ployz logout",
    );

    // The `next:` step works though Cloud refuses this sign-in's logout too.
    let config = dir.path().join("config.yaml");
    let logout = run(&config, &["logout"]).output().unwrap();
    assert!(logout.status.success(), "{logout:?}");
    let stderr = String::from_utf8_lossy(&logout.stderr);
    assert!(
        stderr.contains("Cloud refused to sign this device out: this sign-in's Organization acme is no longer one of yours. Signed out here only:"),
        "{stderr}"
    );
    assert!(!stderr.contains("Could not reach"), "{stderr}");
    assert!(!dir.path().join("cloud.json").exists());

    let again = run(&config, &ORG_RM).output().unwrap();
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(!stderr.contains("no longer one of yours"), "{stderr}");
    assert!(
        stderr.lines().any(|line| line == "next: ployz login"),
        "{stderr}"
    );
}

#[test]
fn context_config_org_removal_hint_ignores_the_context_environment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let cloud = fake_cloud(Arc::default(), true);
    let execute = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .env_clear()
            .env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path())
            .env("PLOYZ_CONFIG", &path)
            .env("PLOYZ_CONTEXT", "b")
            .env("PLOYZ_TOKEN", "ployz_acme")
            .env("PLOYZ_CLOUD_URL", &cloud)
            .output()
            .unwrap()
    };
    let asked = json_of(&execute(&[
        "org",
        "rm",
        "acme",
        "--confirm",
        "acme",
        "--json",
    ]));
    let retry = asked["error"]["details"]["retry"].as_str().unwrap();
    assert!(!retry.contains("--context"), "{retry}");
    let words = shell_words::split(retry).unwrap();
    let args = words[1..]
        .iter()
        .map(String::as_str)
        .chain(["--json"])
        .collect::<Vec<_>>();
    let approved = execute(&args);
    assert!(approved.status.success(), "{}", json_of(&approved));
}
