//! `ployz login` / `ployz logout` against a fake Cloud: the JSON contract, resumable
//! login, `--wait`, and failures that name the command that fixes them.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

const TOKEN_NAME: &str = "soak\nspoof\t\u{1b}[31mX café";

#[derive(Clone, Copy, PartialEq)]
enum Browser {
    Waiting,
    Approved,
    Denied,
}

/// Cloud's device flow for one code; records every route it serves.
fn fake_cloud(browser: Arc<Mutex<Browser>>, routes: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0.to_owned();
            routes.lock().unwrap().push(route.clone());
            let (status, body) = match route.as_str() {
                "POST /api/auth/device/code" => (
                    200,
                    json!({
                        "device_code": "device-secret",
                        "user_code": "ABCD2345",
                        "verification_uri": "http://cloud/device",
                        "verification_uri_complete": "http://cloud/device?user_code=ABCD2345",
                        "expires_in": 1800,
                        "interval": 1,
                    }),
                ),
                "POST /api/auth/device/token" => {
                    let mut browser = browser.lock().unwrap();
                    match *browser {
                        // A person approves while the CLI waits.
                        Browser::Waiting => {
                            *browser = Browser::Approved;
                            (400, json!({ "error": "authorization_pending" }))
                        }
                        Browser::Approved => (200, json!({ "access_token": "session-token" })),
                        Browser::Denied => (400, json!({ "error": "access_denied" })),
                    }
                }
                "GET /api/auth/get-session" => (
                    200,
                    json!({
                        "user": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
                        "session": { "activeOrganizationId": "o1", "activeOrganizationSlug": "acme" },
                    }),
                ),
                "POST /api/cli/logout" => (
                    200,
                    json!({
                        "signed_out": { "id": "d1" },
                        "servers": { "confirmed": ["1".repeat(32)], "unconfirmed": ["2".repeat(32)] },
                    }),
                ),
                "POST /api/cli/tokens" => (
                    200,
                    json!({ "token": {
                        "id": "t1", "name": TOKEN_NAME, "organization": "acme",
                        "expires_at": "2030-01-01", "secret": "test-token-secret",
                    } }),
                ),
                "GET /api/cli/tokens" => (
                    200,
                    json!({
                        "tokens": [{ "id": "t1", "name": TOKEN_NAME,
                            "created_at": "2026-01-01", "expires_at": "2030-01-01",
                            "expired": false, "current": false }],
                        "devices": [], "revoking": [],
                    }),
                ),
                _ => (404, json!({})),
            };
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    url
}

fn ployz(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(["--ployz-config", config.to_str().unwrap()])
        .args(args)
        .env_remove("PLOYZ_CLOUD_URL")
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
fn token_names_cannot_inject_terminal_controls_but_json_preserves_them() {
    let cloud = fake_cloud(Arc::new(Mutex::new(Browser::Approved)), Arc::default());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for args in [
        vec!["login", "--json", "--cloud-url", &cloud],
        vec!["login", "--json"],
    ] {
        assert!(ployz(&config, &args).status.success());
    }

    for args in [vec!["token", "new", TOKEN_NAME], vec!["token", "ls"]] {
        let human = ployz(&config, &args);
        assert!(human.status.success());
        let stdout = String::from_utf8(human.stdout).unwrap();
        assert!(
            stdout.contains(&TOKEN_NAME.escape_debug().to_string()),
            "{stdout:?}"
        );
        assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
        let json = ployz(&config, &[args.as_slice(), &["--json"]].concat());
        assert!(json.status.success());
        let pointer = if args.get(1) == Some(&"new") {
            "/token/name"
        } else {
            "/tokens/0/name"
        };
        assert_eq!(json_of(&json).pointer(pointer).unwrap(), TOKEN_NAME);
    }
}

#[test]
fn json_login_returns_the_code_at_once_and_finishes_on_the_next_run() {
    let browser = Arc::new(Mutex::new(Browser::Waiting));
    let routes = Arc::new(Mutex::new(Vec::new()));
    let cloud = fake_cloud(browser.clone(), routes.clone());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");

    let started = ployz(&config, &["login", "--json", "--cloud-url", &cloud]);
    assert!(started.status.success());
    let mut pending = json_of(&started);
    let expires_in = pending
        .as_object_mut()
        .unwrap()
        .remove("expires_in")
        .unwrap();
    assert!(expires_in.as_u64().unwrap() > 1700, "{expires_in}");
    assert_eq!(
        pending,
        json!({
            "status": "pending",
            "url": "http://cloud/device?user_code=ABCD2345",
            "code": "ABCD2345",
            "next": "ployz login --wait",
        })
    );

    // The browser approves; running login again (Cloud remembered) finishes it.
    *browser.lock().unwrap() = Browser::Approved;
    let finished = ployz(&config, &["login", "--json"]);
    assert!(finished.status.success());
    assert_eq!(
        json_of(&finished),
        json!({
            "status": "signed_in",
            "cloud": cloud,
            "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
            "organization": { "id": "o1", "slug": "acme" },
        })
    );
    assert!(!String::from_utf8_lossy(&finished.stdout).contains("session-token"));

    let signed_out = ployz(&config, &["logout", "--json"]);
    // A Server hasn't confirmed its Clear yet: the result is partial.
    assert_eq!(signed_out.status.code(), Some(3));
    assert_eq!(
        json_of(&signed_out),
        json!({
            "signed_out": true,
            "cloud": cloud,
            "device": "d1",
            "servers": { "confirmed": ["1".repeat(32)], "unconfirmed": ["2".repeat(32)] },
            "next": "ployz token rm d1",
        })
    );
    let again = ployz(&config, &["logout", "--json"]);
    assert_eq!(
        json_of(&again),
        json!({ "signed_out": false, "cloud": null, "device": null, "servers": null })
    );
    assert_eq!(
        routes.lock().unwrap().as_slice(),
        [
            "POST /api/auth/device/code",
            "POST /api/auth/device/token",
            "GET /api/auth/get-session",
            "POST /api/cli/logout",
        ]
    );
}

#[test]
fn wait_and_human_login_block_until_the_browser_decides() {
    for args in [&["login", "--json", "--wait"][..], &["login"]] {
        let browser = Arc::new(Mutex::new(Browser::Waiting));
        let cloud = fake_cloud(browser, Arc::default());
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");

        let output = ployz(&config, &[args, &["--cloud-url", &cloud]].concat());
        assert!(output.status.success(), "{args:?}");
        let human = String::from_utf8_lossy(if args.contains(&"--json") {
            &output.stderr
        } else {
            &output.stdout
        })
        .into_owned();
        assert!(
            human.contains("http://cloud/device?user_code=ABCD2345"),
            "{human}"
        );
        assert!(human.contains("ABCD2345"), "{human}");
        if args.contains(&"--json") {
            assert_eq!(
                json_of(&output).pointer("/account/email").unwrap(),
                "dev@example.test"
            );
        } else {
            assert!(
                human.contains("as dev@example.test in Organization acme"),
                "{human}"
            );
        }
    }
}

#[test]
fn a_denied_login_fails_with_the_command_to_start_over() {
    let browser = Arc::new(Mutex::new(Browser::Denied));
    let cloud = fake_cloud(browser, Arc::default());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");

    let denied = ployz(
        &config,
        &["login", "--json", "--wait", "--cloud-url", &cloud],
    );
    assert_eq!(denied.status.code(), Some(1));
    assert_eq!(
        json_of(&denied),
        json!({ "error": {
            "code": "unauthenticated",
            "message": "the sign-in was denied in the browser",
            "details": { "next": "ployz login" },
        } })
    );
}

#[test]
fn a_cloud_without_cli_sign_in_is_unsupported() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let output = ployz(
        &dir.path().join("config.yaml"),
        &["login", "--json", "--cloud-url", &cloud],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        json_of(&output).pointer("/error/code").unwrap(),
        "unsupported"
    );
}
