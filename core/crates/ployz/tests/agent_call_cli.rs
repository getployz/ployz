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

/// Every file under `dir` with its contents, to tell whether a run wrote anything.
fn files(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                found.push((path.clone(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    found.sort();
    found
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

/// The command a refusal prints shows a typed value only once it parsed as its argument's
/// kind. Any value that didn't, or whose kind may hold a secret (a command line, a command a
/// Setting runs, a Setting value its type refuses, a repository URL, a Cloud URL with
/// anything but where it points), shows as a placeholder, and no part of it reaches stdout,
/// stderr or `--json`.
#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn a_refusal_repeats_no_value_that_did_not_parse_as_its_kind() {
    let home = tempfile::tempdir().unwrap();
    for (command, next) in [
        (
            &["exec", "web", "--", "env", "AUTH_TOKEN=SENTINEL", "true"][..],
            "exec web -- '<COMMAND>'",
        ),
        (
            &[
                "env",
                "branch",
                "fix",
                "--setup",
                "web=curl -H 'Bearer SENTINEL'",
            ],
            "env branch fix --setup '<SERVICE=COMMAND>'",
        ),
        (
            &["set", "web.startCommand=serve --token SENTINEL"],
            "set 'web.startCommand=<VALUE>'",
        ),
        (
            &["set", "web.preDeployCommand=migrate --password SENTINEL"],
            "set 'web.preDeployCommand=<VALUE>'",
        ),
        (
            &["set", "setup.web=seed SENTINEL"],
            "set 'setup.web=<VALUE>'",
        ),
        (
            &["set", "web.env.KEY=SENTINEL"],
            "set 'web.env.KEY=<VALUE>'",
        ),
        // A number Setting given text: its value didn't validate as its type.
        (
            &["set", "web.replicas=SENTINEL"],
            "set 'web.replicas=<VALUE>'",
        ),
        (
            &["set", "web", "--patch", r#"{"replicas":"SENTINEL"}"#],
            "set web --patch '<JSON>'",
        ),
        // A repository URL with a token, refused before the repository name parses.
        (
            &[
                "service",
                "add",
                "web",
                "--repo",
                "https://x-access-token:SENTINEL@github.com/acme/web",
            ],
            "service add web --repo '<OWNER/REPO[@BRANCH]>'",
        ),
        (
            &[
                "login",
                "--cloud-url",
                "https://SENTINEL:SENTINEL@cloud.example:8443/SENTINEL?t=SENTINEL#SENTINEL",
            ],
            "login --cloud-url '<CLOUD_URL>'",
        ),
        // A Cloud served under a path: the host alone would reach another endpoint.
        (
            &["login", "--cloud-url", "https://example.test/SENTINEL"],
            "login --cloud-url '<CLOUD_URL>'",
        ),
    ] {
        let refused = ployz(home.path(), "some-agent", command);
        let stderr = text(&refused.stderr);
        let line = stderr
            .lines()
            .find_map(|line| line.trim().strip_prefix("next: "))
            .unwrap_or_else(|| panic!("{command:?}: no next line in {stderr}"));
        assert_eq!(
            line,
            format!("ployz --why '<the goal this command serves>' {next}"),
            "{command:?}"
        );
        let said = format!("{stderr}{}", text(&refused.stdout));
        assert!(!said.contains("SENTINEL"), "{command:?}: {said}");

        let json = ployz(
            home.path(),
            "some-agent",
            &[&["--json"][..], command].concat(),
        );
        let error: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(
            error["error"]["details"]["next"],
            format!("ployz --json --why '<the goal this command serves>' {next}"),
            "{command:?}"
        );
        let said = format!("{}{}", text(&json.stdout), text(&json.stderr));
        assert!(!said.contains("SENTINEL"), "{command:?}: {said}");

        // Pasted as printed with a reason, the line is refused before anything runs: each
        // placeholder stands for a value, and none is ever sent as one.
        let mut words = shell_words::split(line).unwrap();
        let why = words.iter().position(|word| word == "--why").unwrap() + 1;
        *words.get_mut(why).unwrap() = "try the retry".to_owned();
        let words: Vec<&str> = words.iter().skip(1).map(String::as_str).collect();
        let before = files(home.path());
        let pasted = ployz(home.path(), "some-agent", &words);
        assert_eq!(pasted.status.code(), Some(2), "{words:?}");
        let said = format!("{}{}", text(&pasted.stderr), text(&pasted.stdout));
        assert!(said.contains("holds a placeholder"), "{words:?}: {said}");
        assert_eq!(files(home.path()), before, "{words:?}: wrote something");
    }

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

/// `ployz` as `some-agent` sees it, with `env` on top of [`ployz`]'s.
fn ployz_with(home: &std::path::Path, args: &[&str], env: &[(&str, String)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(args)
        .env("AI_AGENT", "some-agent")
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.join("config.yaml"))
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_CONTEXT")
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .env("NO_COLOR", "1")
        .envs(env.iter().map(|(name, value)| (*name, value.as_str())))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// A credential typed where a command takes something else never reaches output: not an
/// error, a hint, `--help`, or what ssh said. Each row types `SENTINEL` into a value that
/// refuses it, or one an error repeats, and no part of stdout, stderr or `--json` shows it.
#[test]
fn no_output_repeats_a_credential_typed_where_it_does_not_parse() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: 127.0.0.1:1\n",
    )
    .unwrap();
    // An ssh whose stderr names its user alone, as a session token used as the user would.
    let bin = tempfile::tempdir().unwrap();
    let ssh = bin.path().join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\necho 'SENTINEL' >&2\necho 'Connection to host closed by SENTINEL.' >&2\nexit 255\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let why = ["--why", "check what output repeats"];
    for (args, env) in [
        // A URL given as the config: the agent's `next` line repeated it.
        (
            vec![
                "--ployz-config",
                "https://user:SENTINEL@host.example/config.yaml",
                "status",
            ],
            vec![],
        ),
        (
            [
                &why[..],
                &["--context", "https://u:SENTINEL@x.example", "ps"],
            ]
            .concat(),
            vec![],
        ),
        (
            [&why[..], &["ctx", "use", "SENTINEL:SENTINEL@x"]].concat(),
            vec![],
        ),
        (
            vec!["status", "--help"],
            vec![("PLOYZ_CONTEXT", "SENTINEL".to_owned())],
        ),
        (
            vec!["server", "add", "--help"],
            vec![("PLOYZ_DAEMON_VERSION", "SENTINEL".to_owned())],
        ),
        (
            vec!["status", "--help"],
            vec![
                ("PLOYZ_PROJECT", "SENTINEL".to_owned()),
                ("PLOYZ_ENV", "SENTINEL".to_owned()),
                ("PLOYZ_AUTO_CONFIRM", "SENTINEL".to_owned()),
            ],
        ),
        (
            [&why[..], &["--connect", "ssh://SENTINEL@203.0.113.7", "ps"]].concat(),
            vec![("PATH", path)],
        ),
        (
            [&why[..], &["--connect", "unix://SENTINEL", "ps"]].concat(),
            vec![],
        ),
        (
            [&why[..], &["--ssh-timeout", "SENTINEL", "status"]].concat(),
            vec![],
        ),
        ([&why[..], &["status", "SENTINEL"]].concat(), vec![]),
        ([&why[..], &["SENTINEL"]].concat(), vec![]),
        ([&why[..], &["ps", "--SENTINEL"]].concat(), vec![]),
        ([&why[..], &["schema", "u:SENTINEL@x"]].concat(), vec![]),
        (
            [&why[..], &["logs", "web", "--tail", "SENTINEL"]].concat(),
            vec![],
        ),
        // A URL that doesn't parse, for its port, given as the config.
        (
            vec![
                "--ployz-config",
                "https://user:SENTINEL@example.invalid:99999/config.yaml",
                "status",
            ],
            vec![],
        ),
        // A connection no context holds, named in its not-found error.
        (
            [
                &why[..],
                &[
                    "ctx",
                    "use",
                    "lab",
                    "--connection",
                    "ssh://user:SENTINEL@x.example",
                ],
            ]
            .concat(),
            vec![],
        ),
        // A Server name a suggestion would make a DNS label of, keeping the password.
        (
            [
                &why[..],
                &["server", "add", "--name", "https://user:sentinel@x.example"],
            ]
            .concat(),
            vec![("PLOYZ_ENROLL_TOKEN", "enroll-test".to_owned())],
        ),
        // A docs page that doesn't exist, and a search whose result repeats its query.
        (
            [
                &why[..],
                &["docs", "https://user:SENTINEL@x.example/missing.md"],
            ]
            .concat(),
            vec![],
        ),
        (
            [
                &why[..],
                &["docs", "https://user:SENTINEL@x.example/domains"],
            ]
            .concat(),
            vec![],
        ),
        // A token file that can't be read, named in the error.
        (
            [
                &why[..],
                &[
                    "server",
                    "add",
                    "--token-file",
                    "https://user:SENTINEL@x.example",
                ],
            ]
            .concat(),
            vec![],
        ),
    ] {
        for json in [false, true] {
            let args = [if json { &["--json"][..] } else { &[] }, &args].concat();
            let output = ployz_with(home.path(), &args, &env);
            let said = format!("{}{}", text(&output.stdout), text(&output.stderr));
            assert!(
                !said.to_ascii_lowercase().contains("sentinel"),
                "{args:?} {env:?}: {said}"
            );
            assert!(!said.is_empty(), "{args:?}: said nothing");
        }
    }
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
