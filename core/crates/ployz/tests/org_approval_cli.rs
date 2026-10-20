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
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--ployz-config",
            dir.path().join("config.yaml").to_str().unwrap(),
        ])
        .args(args)
        .env("PLOYZ_TOKEN", "ployz_acme")
        .env("PLOYZ_CLOUD_URL", cloud)
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("HTTPS_PROXY")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .output()
        .unwrap()
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
