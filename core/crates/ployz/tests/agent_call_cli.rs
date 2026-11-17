//! A run without a terminal says why it runs, and the report of a run never changes what
//! the run prints or how it exits.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const REASON_REQUIRED: &str =
    "Say why you are running this. Add `--why \"<the goal this command serves>\"`.";

fn ployz(home: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
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
fn a_run_without_a_terminal_is_refused_until_it_says_why() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        ["status"].as_slice(),
        ["status", "--why", ""].as_slice(),
        ["status", "--why", "   "].as_slice(),
    ] {
        let refused = ployz(home.path(), args);
        assert_eq!(refused.status.code(), Some(2), "{args:?}");
        let stderr = text(&refused.stderr);
        assert!(stderr.contains(REASON_REQUIRED), "{args:?}: {stderr}");
        assert!(
            stderr.contains("next: ployz status --why \"<the goal this command serves>\""),
            "{args:?}: {stderr}"
        );
        assert!(refused.stdout.is_empty(), "{args:?}: nothing ran");
    }
    let json = ployz(home.path(), &["status", "--json"]);
    assert_eq!(json.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_argument", "{error}");
    assert_eq!(
        error["error"]["details"]["next"],
        "ployz status --json --why \"<the goal this command serves>\"",
        "{error}"
    );

    let ran = ployz(home.path(), &["status", "--why", "see what is signed in"]);
    assert!(ran.status.success(), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).contains("signed in as"),
        "{}",
        text(&ran.stdout)
    );
}

#[test]
fn version_help_completion_and_internal_verbs_need_no_reason() {
    let home = tempfile::tempdir().unwrap();
    let version = ployz(home.path(), &["--version"]);
    assert!(version.status.success(), "{}", text(&version.stderr));
    assert_eq!(
        text(&version.stdout),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );

    let help = ployz(home.path(), &[]);
    assert!(help.status.success(), "{}", text(&help.stderr));
    assert!(text(&help.stdout).contains("Usage: ployz"));

    let completion = ployz(home.path(), &["completion", "bash"]);
    assert!(completion.status.success(), "{}", text(&completion.stderr));
    assert!(text(&completion.stdout).contains("_clap_complete_ployz"));

    // `ployz build` is Ployz running itself on a build runner: it fails on its own
    // arguments, not on a missing reason.
    let build = ployz(
        home.path(),
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

/// A Cloud that accepts the connection and answers `status`, or nothing at all.
fn cloud(status: Option<u16>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let Some(status) = status else {
                // Hold the connection open past the sender's patience.
                std::thread::sleep(Duration::from_secs(10));
                continue;
            };
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
    });
    url
}

#[test]
fn a_report_cloud_cannot_take_changes_nothing_the_run_shows() {
    let home = tempfile::tempdir().unwrap();
    // An older Cloud without the route, one that never answers, and no Cloud at all.
    for cloud in [
        cloud(Some(404)),
        cloud(None),
        "http://127.0.0.1:9".to_owned(),
    ] {
        let started = Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(["ctx", "ls", "--why", "list the contexts"])
            .env("HOME", home.path())
            .env_remove("XDG_CONFIG_HOME")
            .env("PLOYZ_CONFIG", home.path().join("config.yaml"))
            .env("PLOYZ_TOKEN", "ployz_test_token")
            .env("PLOYZ_CLOUD_URL", &cloud)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
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
