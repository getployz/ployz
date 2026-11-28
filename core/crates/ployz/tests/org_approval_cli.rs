//! `org rm`, `server forget` of every Server and `cluster name` against a fake Cloud that
//! asks a person first. Under `--json` each answers `approval_required` naming the retry,
//! the retry carries the digest, and a digest of a plan that moved since runs nothing.
//! An `org rm` a Server leaves partial names a retry that carries the digest it ran with.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

/// Cloud's ask before `verb`, naming `digest` as what it would do.
fn asked(verb: &str, name: &str, digest: &str, effects: &Value) -> String {
    json!({ "error": {
        "code": "approval_required",
        "message": format!("This {verb} of {name} needs a yes."),
        "details": {
            "approval": digest,
            "effects": effects,
            "operation": { "verb": verb, "name": name },
            "organization": "acme",
        },
    }})
    .to_string()
}

/// Each gated request's route, with the digest it carried.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Cloud that asks a yes before it removes Organization acme, forgets its Servers or
/// renames its Cluster, recording each such request in `seen`. Unless `confirms`, a Server
/// never confirms unpairing, so the removal stays partial.
fn fake_cloud(seen: Seen, confirms: bool) -> String {
    fake_cloud_planning(seen, confirms, "abc", false)
}

/// [`fake_cloud`] whose plans have digests ending in `plan`, as `verb:plan`. With
/// `refuses_logout`, it answers a bare `403` to a sign-out, as Cloud does to a sign-in that
/// left its Organization.
fn fake_cloud_planning(
    seen: Seen,
    confirms: bool,
    plan: &'static str,
    refuses_logout: bool,
) -> String {
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
            let said_yes =
                |verb: &str| approval.as_deref() == Some(format!("{verb}:{plan}").as_str());
            let (status, reply) = match route.as_str() {
                "DELETE /api/cli/organizations/o1" if said_yes("remove") => (
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
                "POST /api/cli/logout" if refuses_logout => ("403 Forbidden", "{}".to_owned()),
                "DELETE /api/cli/organizations/o1" => (
                    "409 Conflict",
                    asked(
                        "remove",
                        "acme",
                        &format!("remove:{plan}"),
                        &json!([{ "kind": "removes_organization", "name": "acme", "node": "o1", "path": "organizations/o1" }]),
                    ),
                ),
                "POST /api/cli/forget-servers" if said_yes("forget") => (
                    "200 OK",
                    json!({ "organization": "acme", "servers": [], "volumes": [], "cancelled": [] }).to_string(),
                ),
                "POST /api/cli/forget-servers" => (
                    "409 Conflict",
                    asked(
                        "forget",
                        "acme",
                        &format!("forget:{plan}"),
                        &json!([{ "kind": "removes_server", "name": "web-1", "node": "b", "path": "servers/b" }]),
                    ),
                ),
                "POST /api/cli/run/cluster_rename" if said_yes("set_cluster_name") => (
                    "200 OK",
                    json!({ "output": { "name": "beta" }, "message": "Saved beta as the Cluster's name. It applies when a Server is online." })
                        .to_string(),
                ),
                "POST /api/cli/run/cluster_rename" => (
                    "409 Conflict",
                    asked("set_cluster_name", "beta", &format!("set_cluster_name:{plan}"), &json!([])),
                ),
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
                || route == "POST /api/cli/run/cluster_rename"
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
    command.arg("--why=test");
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
fn org_rm_forgetting_every_server_and_a_cluster_rename_ask_and_their_retry_carries_the_digest() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), true);
    let cases: [(&[&str], &str, &str); 3] = [
        (
            &["org", "rm", "acme", "--confirm", "acme"],
            "DELETE /api/cli/organizations/o1",
            "remove:abc",
        ),
        (
            &["server", "forget", "--confirm", "acme"],
            "POST /api/cli/forget-servers",
            "forget:abc",
        ),
        (
            &["cluster", "name", "beta"],
            "POST /api/cli/run/cluster_rename",
            "set_cluster_name:abc",
        ),
    ];
    for (args, route, digest) in cases {
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
        assert!(
            named.ends_with(&format!("{} --approval {digest}", args.join(" "))),
            "{named}"
        );
        assert!(named.starts_with("ployz --ployz-config "), "{named}");

        let approved = ployz(
            &cloud,
            &[&["--json"], args, &["--approval", digest]].concat(),
        );
        assert!(approved.status.success(), "{}", json_of(&approved));
        assert_eq!(
            seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
            [
                (route.to_owned(), None),
                (route.to_owned(), Some(digest.to_owned()))
            ],
        );
    }
}

/// A yes names what was shown. Once the plan moved, Cloud asks again, and the CLI runs
/// nothing and asks for no second yes on its own.
#[test]
fn a_yes_to_a_plan_that_moved_runs_nothing() {
    let seen: Seen = Arc::default();
    let cloud = fake_cloud_planning(Arc::clone(&seen), true, "moved", false);
    let args = [
        "org",
        "rm",
        "acme",
        "--confirm",
        "acme",
        "--approval",
        "remove:abc",
    ];

    let stale = ployz(&cloud, &args);
    assert_eq!(stale.status.code(), Some(1), "{stale:?}");
    let stderr = String::from_utf8_lossy(&stale.stderr);
    assert!(
        stderr.contains(
            "It changed after your yes, so nothing removed. Run it again to see what it does now."
        ),
        "{stderr}"
    );

    let json = json_of(&ployz(&cloud, &[&["--json"], &args[..]].concat()));
    assert_eq!(
        json.pointer("/error/code"),
        Some(&json!("approval_required")),
        "{json}"
    );
    let retry = json
        .pointer("/error/details/retry")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(retry.ends_with("--approval remove:moved"), "{retry}");
    assert_eq!(
        seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
        [
            (
                "DELETE /api/cli/organizations/o1".to_owned(),
                Some("remove:abc".to_owned())
            ),
            (
                "DELETE /api/cli/organizations/o1".to_owned(),
                Some("remove:abc".to_owned())
            ),
        ],
        "nothing ran past the first ask"
    );
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

    let forgot = ployz(
        &cloud,
        &[
            "server",
            "forget",
            "web-1",
            "--confirm",
            "dead",
            "--approval",
            "forget:abc",
        ],
    );
    assert!(forgot.status.success(), "{forgot:?}");
    let stderr = String::from_utf8_lossy(&forgot.stderr);
    assert_eq!(
        stderr
            .matches("Asking Cloud which Servers to forget…")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("Asking Cloud to forget the Servers…")
            .count(),
        1,
        "{stderr}"
    );
    // Ployz no longer warns about what a forgotten Server keeps on its disk.
    assert!(!stderr.contains("! "), "{stderr}");
}

/// Without a terminal nothing says yes: the forget shows what it destroys, runs nothing,
/// and names the retry that carries the digest.
#[test]
fn forgetting_every_server_without_a_terminal_shows_what_it_destroys_and_names_the_retry() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), true);

    let asked = ployz(&cloud, &["server", "forget", "--confirm", "acme"]);
    assert_eq!(asked.status.code(), Some(2), "{asked:?}");
    let stderr = String::from_utf8_lossy(&asked.stderr);
    assert_eq!(
        stderr
            .matches("Asking Cloud to forget the Servers…")
            .count(),
        1,
        "{stderr}"
    );
    assert!(stderr.contains("removes Server web-1"), "{stderr}");
    assert!(
        stderr.contains("Nothing forgotten. Saying yes needs a terminal."),
        "{stderr}"
    );
    let retry = stderr
        .lines()
        .find_map(|line| line.strip_prefix("retry: "))
        .unwrap_or_default();
    assert!(
        retry.ends_with("server forget --confirm acme --approval forget:abc"),
        "{stderr}"
    );
    assert_eq!(seen.lock().unwrap().drain(..).count(), 1);

    let json = ployz(
        &cloud,
        &[
            "--json",
            "server",
            "forget",
            "--confirm",
            "acme",
            "--approval",
            "forget:abc",
        ],
    );
    assert!(json.status.success(), "{}", json_of(&json));
    let stderr = String::from_utf8_lossy(&json.stderr);
    assert!(!stderr.contains("Asking Cloud"), "{stderr}");
}

#[test]
fn a_partial_org_rm_names_a_retry_that_carries_its_digest() {
    let seen = Arc::default();
    let cloud = fake_cloud(Arc::clone(&seen), false);
    let args = ["org", "rm", "acme", "--confirm", "acme"];

    let passed = ployz(
        &cloud,
        &[&["--json"], &args[..], &["--approval", "remove:abc"]].concat(),
    );
    assert_eq!(passed.status.code(), Some(3), "{}", json_of(&passed));
    let next = json_of(&passed);
    let next = next
        .pointer("/next")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        next.ends_with("org rm acme --confirm acme --approval remove:abc"),
        "{next}"
    );
    assert_eq!(
        seen.lock().unwrap().drain(..).collect::<Vec<_>>(),
        [(
            "DELETE /api/cli/organizations/o1".to_owned(),
            Some("remove:abc".to_owned())
        )],
    );
}

#[test]
fn logging_out_works_though_cloud_refuses_a_sign_in_that_left_its_organization() {
    let cloud = fake_cloud_planning(Arc::default(), true, "abc", true);
    let dir = tempfile::tempdir().unwrap();
    let logout = signed_in(&cloud, dir.path(), &["logout"]);
    assert!(logout.status.success(), "{logout:?}");
    let stderr = String::from_utf8_lossy(&logout.stderr);
    assert!(
        stderr.contains("Cloud refused to sign this device out: this sign-in's Organization acme is no longer one of yours. Signed out here only:"),
        "{stderr}"
    );
    assert!(!stderr.contains("Could not reach"), "{stderr}");
    assert!(!dir.path().join("cloud.json").exists());

    let config = dir.path().join("config.yaml");
    let again = run(&config, &["org", "rm", "acme", "--confirm", "acme"])
        .output()
        .unwrap();
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
        "--why=test",
        "org",
        "rm",
        "acme",
        "--confirm",
        "acme",
        "--json",
    ]));
    let retry = asked
        .pointer("/error/details/retry")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(!retry.contains("--context"), "{retry}");
    let words = shell_words::split(retry).unwrap();
    // A retry withholds the typed reason, which the person types again.
    let args = words
        .get(1..)
        .unwrap()
        .iter()
        .map(|word| match word.as_str() {
            "<REASON>" => "test",
            word => word,
        })
        .collect::<Vec<_>>();
    let approved = execute(&args);
    assert!(approved.status.success(), "{}", json_of(&approved));
}
