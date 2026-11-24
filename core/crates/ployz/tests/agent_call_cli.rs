//! A coding agent says why it runs a command, a run with no agent behind it is untouched,
//! and the report of a run never changes what the run prints or how it exits.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REASON_REQUIRED: &str =
    "Say why you are running this. Add `--why \"<the goal this command serves>\"`.";

/// `ployz` as `agent` sees it. `AI_AGENT` is always set, so the result never depends on
/// which markers the test runner's own environment carries: `off` beats them all.
fn ployz(home: &std::path::Path, agent: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("AI_AGENT", agent)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.join("config.yaml"))
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_CONTEXT")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn an_agent_is_refused_until_it_says_why() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        ["status"].as_slice(),
        ["status", "--why", ""].as_slice(),
        ["status", "--why", "   "].as_slice(),
    ] {
        let refused = ployz(home.path(), "some-agent", args);
        assert_eq!(refused.status.code(), Some(2), "{args:?}");
        let stderr = text(&refused.stderr);
        assert!(stderr.contains(REASON_REQUIRED), "{args:?}: {stderr}");
        assert!(
            stderr.contains("next: ployz --why '<the goal this command serves>' status"),
            "{args:?}: {stderr}"
        );
        assert!(refused.stdout.is_empty(), "{args:?}: nothing ran");
    }
    let json = ployz(home.path(), "some-agent", &["status", "--json"]);
    assert_eq!(json.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument", "{error}");
    assert_eq!(
        error["error"]["details"]["next"],
        "ployz --json --why '<the goal this command serves>' status",
        "{error}"
    );

    let ran = ployz(
        home.path(),
        "some-agent",
        &["status", "--why", "see what is signed in"],
    );
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).contains("signed in as"),
        "{}",
        text(&ran.stdout)
    );
}

#[test]
fn the_line_a_refusal_prints_runs_as_printed_for_a_command_with_trailing_words() {
    let home = tempfile::tempdir().unwrap();
    let refused = ployz(
        home.path(),
        "some-agent",
        &["exec", "web", "--", "echo", "hi"],
    );
    let stderr = text(&refused.stderr);
    let line = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("next: "))
        .unwrap_or_else(|| panic!("no next line in {stderr}"));
    assert_eq!(
        line,
        "ployz --why '<the goal this command serves>' exec web -- echo hi"
    );

    let ran = ployz(
        home.path(),
        "some-agent",
        &[
            "--why",
            "look at the web container",
            "exec",
            "web",
            "--",
            "echo",
            "hi",
        ],
    );
    let stderr = text(&ran.stderr);
    assert!(!stderr.contains(REASON_REQUIRED), "{stderr}");
    assert!(!stderr.contains("--why"), "{stderr}");
}

#[test]
fn version_help_completion_and_internal_verbs_need_no_reason_from_an_agent() {
    let home = tempfile::tempdir().unwrap();
    let version = ployz(home.path(), "some-agent", &["--version"]);
    assert!(version.status.success(), "{}", text(&version.stderr));
    assert_eq!(
        text(&version.stdout),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );

    let help = ployz(home.path(), "some-agent", &[]);
    assert!(help.status.success(), "{}", text(&help.stderr));
    assert!(text(&help.stdout).contains("Usage: ployz"));

    let completion = ployz(home.path(), "some-agent", &["completion", "bash"]);
    assert!(completion.status.success(), "{}", text(&completion.stderr));
    assert!(text(&completion.stdout).contains("_clap_complete_ployz"));

    // `install.sh` runs `server checkin` where no agent can add `--why`.
    let checkin = ployz(home.path(), "some-agent", &["server", "checkin"]);
    assert!(
        !text(&checkin.stderr).contains(REASON_REQUIRED),
        "{}",
        text(&checkin.stderr)
    );

    // `ployz build` is Ployz running itself on a build runner: it fails on its own
    // arguments, not on a missing reason.
    let build = ployz(
        home.path(),
        "some-agent",
        &[
            "build",
            "--grant",
            "nope",
            "--commit",
            "abc",
            "--deployment",
            "d",
            "--fingerprint",
            "f",
        ],
    );
    let stderr = text(&build.stderr);
    assert!(stderr.contains("Build Grant"), "{stderr}");
    assert!(!stderr.contains(REASON_REQUIRED), "{stderr}");
}

/// A Cloud that accepts the connection and answers `status`, or nothing at all. It
/// records each request it reads, headers and body, as one string.
fn cloud(status: Option<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let Some(status) = status else {
                // Answer nothing, and hold the connection until the sender gives up and closes it.
                let _ = stream.read_to_end(&mut Vec::new());
                continue;
            };
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                request.extend_from_slice(buffer.get(..read).unwrap_or_default());
                let sent = text(&request).to_ascii_lowercase();
                let whole = sent.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                    let length = head
                        .split("content-length: ")
                        .nth(1)
                        .and_then(|rest| rest.split("\r\n").next()?.parse::<usize>().ok());
                    body.len() >= length.unwrap_or(0)
                });
                if read == 0 || whole {
                    break;
                }
            }
            seen.lock().unwrap().push(text(&request));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
    });
    (url, requests)
}

fn ploy_to_cloud(home: &std::path::Path, agent: &str, cloud: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("AI_AGENT", agent)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.join("config.yaml"))
        .env("PLOYZ_TOKEN", "ployz_test_token")
        .env("PLOYZ_CLOUD_URL", cloud)
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_run_with_no_agent_runs_as_it_always_did_and_reports_nothing() {
    let home = tempfile::tempdir().unwrap();
    let (cloud, requests) = cloud(Some(204));
    // No terminal and no `--why`: only an agent is asked for a reason.
    let output = ploy_to_cloud(home.path(), "off", &cloud, &["ctx", "ls"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).contains("NAME\tCURRENT"),
        "{}",
        text(&output.stdout)
    );
    assert!(!text(&output.stderr).contains(REASON_REQUIRED));
    let reported = requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.contains("/api/cli/agent/calls"));
    assert!(!reported, "{:?}", requests.lock().unwrap());
}

#[test]
fn an_agent_run_is_reported_with_its_reason_and_name() {
    let home = tempfile::tempdir().unwrap();
    let (cloud, requests) = cloud(Some(204));
    let output = ploy_to_cloud(
        home.path(),
        "some-agent",
        &cloud,
        &["ctx", "ls", "--why", "list the contexts"],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let requests = requests.lock().unwrap();
    let report = requests
        .iter()
        .find(|request| request.starts_with("POST /api/cli/agent/calls "))
        .unwrap_or_else(|| panic!("no report in {requests:?}"));
    assert!(
        report.contains("\"intent\":\"list the contexts\""),
        "{report}"
    );
    assert!(report.contains("\"name\":\"some-agent\""), "{report}");
    assert!(report.contains("\"tool\":\"ctx ls\""), "{report}");
}

#[test]
fn a_report_cloud_cannot_take_changes_nothing_the_run_shows() {
    let home = tempfile::tempdir().unwrap();
    // An older Cloud without the route, one that never answers, and no Cloud at all.
    for cloud in [
        cloud(Some(404)).0,
        cloud(None).0,
        "http://127.0.0.1:9".to_owned(),
    ] {
        let started = Instant::now();
        let output = ploy_to_cloud(
            home.path(),
            "some-agent",
            &cloud,
            &["ctx", "ls", "--why", "list the contexts"],
        );
        assert!(output.status.success(), "{cloud}: {}", text(&output.stderr));
        assert!(
            text(&output.stdout).contains("NAME\tCURRENT"),
            "{cloud}: {}",
            text(&output.stdout)
        );
        assert!(
            !text(&output.stderr).contains("127.0.0.1"),
            "{cloud}: nothing about the report is printed: {}",
            text(&output.stderr)
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{cloud}: the report waits two seconds at most"
        );
    }
}
