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
            let mut sent = vec![0; length];
            reader.read_exact(&mut sent).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0.to_owned();
            // Every run reports itself; the routes under test are the command's own.
            if route != "POST /api/cli/agent/calls" {
                routes.lock().unwrap().push(route.clone());
            }
            if route == "POST /api/cli/run/org_leave" {
                let sent: Value = serde_json::from_slice(&sent).unwrap();
                routes
                    .lock()
                    .unwrap()
                    .push(format!("org_leave input {}", sent.get("input").unwrap()));
            }
            if route == "POST /api/auth/organization/set-active" {
                let sent: Value = serde_json::from_slice(&sent).unwrap();
                routes.lock().unwrap().push(format!("set-active {sent}"));
            }
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
                        "scope": "Full as you",
                    } }),
                ),
                "GET /api/cli/tokens" => (
                    200,
                    json!({
                        "tokens": [{ "id": "t1", "name": TOKEN_NAME,
                            "created_at": "2026-01-01", "expires_at": "2030-01-01",
                            "expired": false, "current": false, "scope": "Full as you" },
                            { "id": "t2", "name": "grace-ci",
                            "created_at": "2026-01-01", "expires_at": "2030-01-01",
                            "expired": false, "current": false, "scope": "Full as Grace",
                            "owner": { "id": "u2", "name": "Grace", "email": "grace@example.test", "you": false } }],
                        "devices": [{ "id": "d1", "created_at": "2026-10-18T02:49:32.724Z",
                            "expires_at": "2026-10-25T02:49:32.724Z", "current": true }],
                        "revoking": [],
                    }),
                ),
                "DELETE /api/cli/tokens/t2" => (
                    200,
                    json!({
                        "removed": { "id": "t2", "kind": "token" },
                        "servers": { "confirmed": ["1".repeat(32)], "unconfirmed": ["2".repeat(32)] },
                    }),
                ),
                "GET /api/cli/organizations" => (
                    200,
                    json!({ "organizations": [
                        { "id": "o1", "slug": "acme", "name": "Acme", "current": true },
                        { "id": "o2", "slug": "dev", "name": "Dev", "current": false },
                    ] }),
                ),
                "POST /api/cli/run/org_leave" => {
                    (200, json!({ "output": { "organization": "acme" } }))
                }
                "POST /api/auth/organization/set-active" => {
                    (200, json!({ "id": "o2", "slug": "dev" }))
                }
                "DELETE /api/cli/organizations/o1" => (
                    200,
                    json!({
                        "organization": "acme",
                        "removed": true,
                        "servers": { "confirmed": [], "unconfirmed": [] },
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
        .arg("--why=test")
        .args(["--ployz-config", config.to_str().unwrap()])
        .args(args)
        .env_remove("PLOYZ_CLOUD_URL")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env_remove("HTTP_PROXY")
        .env_remove("http_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
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
        let shown = [human.stdout, human.stderr].concat();
        let shown = String::from_utf8(shown).unwrap();
        assert!(
            shown.contains(&TOKEN_NAME.escape_debug().to_string()),
            "{shown:?}"
        );
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
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
fn token_ls_says_which_login_each_device_is_and_what_it_can_do() {
    let cloud = fake_cloud(Arc::new(Mutex::new(Browser::Approved)), Arc::default());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for args in [
        vec!["login", "--json", "--cloud-url", &cloud],
        vec!["login", "--json"],
    ] {
        assert!(ployz(&config, &args).status.success());
    }
    let listed = ployz(&config, &["token", "ls"]);
    assert!(listed.status.success());
    let stdout = String::from_utf8(listed.stdout).unwrap();
    assert!(
        stdout.lines().any(|line| line
            == "device\td1\tployz login 2026-10-18\tyou\tFull as you\t2026-10-25T02:49:32.724Z\tcurrent"),
        "{stdout}"
    );
}

#[test]
fn token_ls_names_whose_each_token_is() {
    let cloud = fake_cloud(Arc::new(Mutex::new(Browser::Approved)), Arc::default());
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for args in [
        vec!["login", "--json", "--cloud-url", &cloud],
        vec!["login", "--json"],
    ] {
        assert!(ployz(&config, &args).status.success());
    }
    let listed = ployz(&config, &["token", "ls"]);
    assert!(listed.status.success());
    let stdout = String::from_utf8(listed.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .any(|line| line == "cli\tt2\tgrace-ci\tGrace\tFull as Grace\t2030-01-01\t-"),
        "{stdout}"
    );
    let json = ployz(&config, &["--json", "token", "ls"]);
    assert!(json.status.success());
    let json = json_of(&json);
    assert_eq!(json.pointer("/tokens/1/owner/name").unwrap(), "Grace");
    assert_eq!(
        json.pointer("/tokens/0/owner"),
        None,
        "an older Cloud names no owner"
    );
}

#[test]
fn token_rm_revokes_a_token_by_the_name_token_ls_shows() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let (_dir, config) = signed_in(routes.clone());
    routes.lock().unwrap().clear();

    let removed = ployz(&config, &["--json", "token", "rm", "grace-ci"]);
    // A Server hasn't confirmed its Clear yet: the result is partial, and the rerun names
    // the ID, which still finds the token once it leaves the list.
    assert_eq!(removed.status.code(), Some(3), "{removed:?}");
    assert_eq!(
        json_of(&removed),
        json!({
            "removed": { "id": "t2", "kind": "token" },
            "servers": { "confirmed": ["1".repeat(32)], "unconfirmed": ["2".repeat(32)] },
            "next": shell_words::join([
                "ployz",
                "--ployz-config",
                config.to_str().unwrap(),
                "--json",
                "--why",
                "<REASON>",
                "token",
                "rm",
                "t2",
            ]),
        })
    );
    assert_eq!(
        routes.lock().unwrap().as_slice(),
        ["GET /api/cli/tokens", "DELETE /api/cli/tokens/t2"]
    );
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
            "next": shell_words::join([
                "ployz",
                "--ployz-config",
                config.to_str().unwrap(),
                "token",
                "rm",
                "d1",
            ]),
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
        let human = String::from_utf8_lossy(&output.stderr).into_owned();
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
            let result = String::from_utf8_lossy(&output.stdout);
            assert!(
                result.contains("as dev@example.test in Organization acme"),
                "{result}"
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
            "cause": [],
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
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        json_of(&output).pointer("/error/code").unwrap(),
        "unsupported"
    );
}

fn signed_in(routes: Arc<Mutex<Vec<String>>>) -> (tempfile::TempDir, std::path::PathBuf) {
    let cloud = fake_cloud(Arc::new(Mutex::new(Browser::Approved)), routes);
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for args in [
        vec!["login", "--json", "--cloud-url", &cloud],
        vec!["login", "--json"],
    ] {
        assert!(ployz(&config, &args).status.success());
    }
    (dir, config)
}

#[test]
fn org_leave_with_yes_leaves_then_moves_the_device_to_another_organization() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let (_dir, config) = signed_in(routes.clone());
    routes.lock().unwrap().clear();

    let left = ployz(&config, &["org", "leave", "--yes", "--json"]);

    assert!(
        left.status.success(),
        "{}",
        String::from_utf8_lossy(&left.stderr)
    );
    assert_eq!(
        json_of(&left),
        json!({ "organization": "acme", "acting": "dev" })
    );
    let routes = routes.lock().unwrap().clone();
    let at = |route: &str| {
        routes
            .iter()
            .position(|seen| seen == route)
            .unwrap_or_else(|| panic!("{route} in {routes:?}"))
    };
    assert!(
        at("GET /api/cli/organizations") < at("POST /api/cli/run/org_leave"),
        "lists the others while still a member"
    );
    assert!(
        at("POST /api/cli/run/org_leave") < at("POST /api/auth/organization/set-active"),
        "moves only once it left"
    );
    at(r#"org_leave input {"organization":"o1"}"#);
    at(r#"set-active {"organizationId":"o2"}"#);
    let human = ployz(&config, &["org", "leave", "--yes"]);
    assert!(
        String::from_utf8_lossy(&human.stderr)
            .contains("This device now acts in Organization dev."),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
}

#[test]
fn org_rm_removes_the_acting_organization_by_its_id_then_moves_the_device() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let (_dir, config) = signed_in(routes.clone());
    routes.lock().unwrap().clear();

    let removed = ployz(
        &config,
        &["org", "rm", "acme", "--confirm", "acme", "--json"],
    );

    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let removed = json_of(&removed);
    assert_eq!(removed.get("organization"), Some(&json!("acme")));
    assert_eq!(removed.get("removed"), Some(&json!(true)));
    assert_eq!(removed.get("acting"), Some(&json!("dev")));
    let routes = routes.lock().unwrap().clone();
    let at = |route: &str| {
        routes
            .iter()
            .position(|seen| seen == route)
            .unwrap_or_else(|| panic!("{route} in {routes:?}"))
    };
    assert!(
        at("DELETE /api/cli/organizations/o1") < at("POST /api/auth/organization/set-active"),
        "moves only once it is gone"
    );
    at(r#"set-active {"organizationId":"o2"}"#);
}

#[test]
fn org_rm_of_another_organization_refuses_before_cloud_removes_anything() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let (_dir, config) = signed_in(routes.clone());

    let refused = ployz(&config, &["org", "rm", "dev", "--confirm", "dev", "--json"]);

    assert_eq!(refused.status.code(), Some(2));
    let refused = json_of(&refused);
    assert_eq!(refused.pointer("/error/code").unwrap(), "invalid_argument");
    let unknown = json_of(&ployz(
        &config,
        &["org", "rm", "nosuch", "--confirm", "nosuch", "--json"],
    ));
    assert_eq!(
        unknown.pointer("/error/message").unwrap(),
        "no Organization nosuch of yours"
    );
    assert_eq!(
        unknown.pointer("/error/details/next").unwrap(),
        "ployz org ls"
    );
    assert!(
        !routes
            .lock()
            .unwrap()
            .iter()
            .any(|route| route.starts_with("DELETE "))
    );
}

#[test]
fn org_leave_without_yes_and_no_terminal_refuses_before_cloud_hears_of_it() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let (_dir, config) = signed_in(routes.clone());

    let refused = ployz(&config, &["org", "leave", "--json"]);

    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("confirmation_required"),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        !routes
            .lock()
            .unwrap()
            .iter()
            .any(|route| route.contains("org_leave"))
    );
}

/// A login that fails names the Cloud by its scheme, host and port, with each other part
/// it was given marked in place: no user info, path, query or fragment reaches stdout,
/// stderr or `--json`, whether the URL is refused before anything is asked of it (exit 2),
/// this device is signed in to another Cloud, or the one named doesn't answer (exit 1).
#[test]
fn a_failed_login_names_the_cloud_without_its_secret() {
    let (_signed_in, elsewhere) = signed_in(Arc::default());
    let fresh = tempfile::tempdir().unwrap();
    let fresh = fresh.path().join("config.yaml");
    for (config, cloud, named, code) in [
        (
            &elsewhere,
            "https://SENTINEL:SENTINEL@other.example:8443/SENTINEL?t=SENTINEL#SENTINEL",
            "https://<USER>:<PASSWORD>@other.example:8443/<PATH>?<QUERY>#<FRAGMENT>",
            2,
        ),
        (
            &elsewhere,
            "https://SENTINEL@other.example/?next=SENTINEL",
            "https://<USER>@other.example?<QUERY>",
            2,
        ),
        (
            &fresh,
            "http://SENTINEL:SENTINEL@127.0.0.1:9/SENTINEL?token=SENTINEL",
            "http://<USER>:<PASSWORD>@127.0.0.1:9/<PATH>?<QUERY>",
            2,
        ),
        (
            &elsewhere,
            "https://other.example/SENTINEL",
            "https://other.example/<PATH>",
            1,
        ),
        (
            &fresh,
            "http://127.0.0.1:9/SENTINEL",
            "http://127.0.0.1:9/<PATH>",
            1,
        ),
    ] {
        let human = ployz(config, &["login", "--cloud-url", cloud]);
        let json = ployz(config, &["login", "--json", "--cloud-url", cloud]);
        for output in [&human, &json] {
            assert_eq!(output.status.code(), Some(code), "{cloud}");
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(said.contains(named), "{cloud}: {said}");
            assert!(!said.contains("SENTINEL"), "{cloud}: {said}");
        }
        assert!(
            json_of(&json).pointer("/error/message").is_some(),
            "{cloud}"
        );
    }
}

/// A Cloud URL is stored with the sign-in and printed on success, so one with user info, a
/// query or a fragment is refused before Cloud is asked anything and nothing is stored,
/// whether `--cloud-url` or `PLOYZ_CLOUD_URL` gave it, for login and for the commands that
/// hand a Cloud to a Server. A retry's `<CLOUD_URL>` pasted as printed is refused too.
#[test]
fn a_cloud_url_that_holds_a_credential_is_refused_before_it_is_stored() {
    let routes = Arc::new(Mutex::new(Vec::new()));
    let cloud = fake_cloud(Arc::new(Mutex::new(Browser::Approved)), routes.clone());
    let with_user = cloud.replace("http://", "http://SENTINEL:SENTINEL@");
    let with_query = format!("{cloud}/?token=SENTINEL");
    for (args, variable) in [
        (vec!["login", "--cloud-url", with_user.as_str()], None),
        (
            vec![
                "login",
                "--json",
                "--wait",
                "--cloud-url",
                with_user.as_str(),
            ],
            None,
        ),
        (vec!["login", "--cloud-url", with_query.as_str()], None),
        (vec!["login"], Some(with_user.as_str())),
        (vec!["login", "--cloud-url", "<CLOUD_URL>"], None),
        (
            vec![
                "server",
                "add",
                "root@203.0.113.7",
                "--cloud-url",
                with_user.as_str(),
            ],
            None,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .arg("--why=test")
            .args(["--ployz-config", config.to_str().unwrap()])
            .args(&args)
            .env_remove("PLOYZ_CLOUD_URL")
            .env_remove("PLOYZ_TOKEN")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null());
        if let Some(variable) = variable {
            command.env("PLOYZ_CLOUD_URL", variable);
        }
        let output = command.output().unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.code(), Some(2), "{args:?}: {said}");
        assert!(!said.contains("SENTINEL"), "{args:?}: {said}");
        assert!(
            said.contains(if variable.is_some() {
                "PLOYZ_CLOUD_URL"
            } else {
                "--cloud-url"
            }),
            "{args:?}: {said}"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "{args:?}: stored something"
        );
    }
    assert_eq!(
        *routes.lock().unwrap(),
        Vec::<String>::new(),
        "Cloud was asked"
    );
}

/// A sign-in stored before Ployz refused a Cloud URL with user info still holds one, and
/// logout names that Cloud without it.
#[test]
fn logout_names_a_stored_cloud_without_its_user_info() {
    let stored = json!({
        "state": "pending",
        "cloud": "http://user:SENTINEL@127.0.0.1:9",
        "device_code": "device",
        "code": "ABCD",
        "url": "http://user:SENTINEL@127.0.0.1:9/device?code=ABCD",
        "expires_at": 9_999_999_999_u64,
        "interval_secs": 5,
    });
    for args in [&["logout"][..], &["--json", "logout"]] {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        let cloud = dir.path().join("cloud.json");
        std::fs::write(&cloud, stored.to_string()).unwrap();
        std::fs::set_permissions(&cloud, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let output = ployz(&config, args);
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!said.contains("SENTINEL"), "{args:?}: {said}");
        assert!(said.contains("127.0.0.1:9"), "{args:?}: {said}");
    }
}

/// `--help` names `PLOYZ_CLOUD_URL` without its value, which may carry a token in its user
/// info, path or query.
#[test]
fn help_never_prints_the_cloud_urls_value() {
    let home = tempfile::tempdir().unwrap();
    for command in [&["login"][..], &["server", "add"]] {
        let help = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .env("HOME", home.path())
            .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
            .env(
                "PLOYZ_CLOUD_URL",
                "https://EXAMPLE:EXAMPLE@cloud.invalid/EXAMPLE?token=EXAMPLE",
            )
            .env("NO_COLOR", "1")
            .args(command)
            .arg("--help")
            .output()
            .unwrap();
        let help = String::from_utf8_lossy(&help.stdout);
        assert!(help.contains("PLOYZ_CLOUD_URL"), "{command:?}: {help}");
        assert!(!help.contains("EXAMPLE"), "{command:?}: {help}");
    }
}
