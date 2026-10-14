//! `ployz mcp` as a host runs it: closing its pipes or signalling it stops it, even mid-call.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn mcp(cloud: &str, home: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_ployz"))
        .arg("mcp")
        .env("HOME", home)
        .env("PLOYZ_CONFIG", home.join("config.toml"))
        .env("PLOYZ_TOKEN", "ployz_test_token")
        .env("PLOYZ_CLOUD_URL", cloud)
        .env("NO_PROXY", "127.0.0.1")
        .env("no_proxy", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn send(stdin: &mut ChildStdin, frame: &Value) {
    stdin.write_all(format!("{frame}\n").as_bytes()).unwrap();
    stdin.flush().unwrap();
}

fn initialize(stdin: &mut ChildStdin, stdout: &mut BufReader<ChildStdout>) {
    send(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }),
    );
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.contains("\"serverInfo\""), "{line}");
    send(
        stdin,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
}

fn exited_within(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < limit {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Signalling before the handler is installed would stop the server whatever it does.
fn catches(pid: u32, signal: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
        status
            .lines()
            .find_map(|line| line.strip_prefix("SigCgt:"))
            .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            .is_some_and(|mask| mask & (1 << (signal - 1)) != 0)
    })
}

#[test]
fn a_signal_before_initialize_stops_the_server() {
    let home = tempfile::tempdir().unwrap();
    for (signal, number) in [("TERM", 15), ("INT", 2), ("HUP", 1)] {
        let mut server = mcp("http://127.0.0.1:9", home.path());
        let start = Instant::now();
        while !catches(server.id(), number) {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the server never caught SIG{signal}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let status = Command::new("kill")
            .args([&format!("-{signal}"), &server.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let exited = exited_within(&mut server, Duration::from_secs(5));
        assert!(
            exited.is_some(),
            "SIG{signal} before initialize left the server running"
        );
    }
}

/// A server whose one call waits on a Cloud that accepted the connection and never answers.
struct Stuck {
    server: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _held: (TcpListener, std::net::TcpStream, tempfile::TempDir),
}

fn accepted(cloud: &TcpListener) -> std::net::TcpStream {
    cloud.set_nonblocking(true).unwrap();
    let start = Instant::now();
    loop {
        match cloud.accept() {
            Ok((held, _)) => return held,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "the call never reached Cloud"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
}

fn stuck_call() -> Stuck {
    let home = tempfile::tempdir().unwrap();
    let cloud = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut server = mcp(
        &format!("http://{}", cloud.local_addr().unwrap()),
        home.path(),
    );
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    initialize(&mut stdin, &mut stdout);
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "server_show", "arguments": {} },
        }),
    );
    let held = accepted(&cloud);
    Stuck {
        server,
        stdin,
        stdout,
        _held: (cloud, held, home),
    }
}

#[test]
fn closing_stdin_stops_the_server_and_the_call_it_is_running() {
    let Stuck {
        mut server, stdin, ..
    } = stuck_call();
    drop(stdin);
    exited_within(&mut server, Duration::from_secs(5))
        .expect("the server stops once its stdin is closed");
}

#[test]
fn closing_stdout_stops_the_server_and_the_call_it_is_running() {
    let Stuck {
        mut server,
        mut stdin,
        stdout,
        ..
    } = stuck_call();
    drop(stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {} }),
    );
    let status = exited_within(&mut server, Duration::from_secs(5))
        .expect("the server stops once its stdout is closed");
    assert_eq!(
        status.signal(),
        None,
        "the server died of a signal: {status}"
    );
}

#[test]
fn closing_stdout_before_initialize_stops_the_server_quietly() {
    let home = tempfile::tempdir().unwrap();
    let mut server = mcp("http://127.0.0.1:9", home.path());
    drop(server.stdout.take());
    let mut stdin = server.stdin.take().unwrap();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }),
    );
    let status = exited_within(&mut server, Duration::from_secs(5))
        .expect("the server stops once its stdout is closed");
    assert_eq!(status.code(), Some(0), "{status}");
}

/// `ployz` run in `dir` on the in-process Store in `home`, away from any ambient scope.
fn local(home: &std::path::Path, dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .current_dir(dir)
        .env("HOME", home)
        .env("PLOYZ_CONFIG", home.join("config.toml"))
        .env(
            "PLOYZ_STORE",
            format!("sqlite:{}", home.join("store.db").display()),
        )
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    command
}

#[test]
fn each_call_acts_where_the_directory_is_linked_when_it_starts() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ployz = |args: &[&str]| {
        let output = local(home.path(), dir.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    ployz(&["project", "new", "shop"]);
    ployz(&["env", "new", "staging"]);
    ployz(&["link"]);
    let mut server = local(home.path(), dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    initialize(&mut stdin, &mut stdout);
    let mut shown = |id: u64| {
        send(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": "environment_show", "arguments": {} },
            }),
        );
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        reply
            .pointer("/result/structuredContent/at")
            .cloned()
            .unwrap_or_else(|| panic!("{reply}"))
    };
    assert_eq!(shown(2), "shop/production");
    ployz(&["link", "--env", "staging"]);
    assert_eq!(shown(3), "shop/staging");
    drop(stdin);
    assert!(exited_within(&mut server, Duration::from_secs(10)).is_some());
}

#[test]
fn a_renamed_project_keeps_the_directory_linked_to_it() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ployz = |args: &[&str]| {
        let output = local(home.path(), dir.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    ployz(&["project", "new", "shop"]);
    ployz(&["env", "new", "staging"]);
    ployz(&["link", "--env", "staging"]);
    let mut server = local(home.path(), dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    initialize(&mut stdin, &mut stdout);
    let mut call = |id: u64, name: &str, arguments: Value| {
        send(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": name, "arguments": arguments },
            }),
        );
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        reply
            .pointer("/result/structuredContent")
            .cloned()
            .unwrap_or_else(|| panic!("{reply}"))
    };
    let renamed = call(
        2,
        "project_rename",
        json!({ "project": "shop", "to": "store" }),
    );
    assert_eq!(renamed.get("project"), Some(&json!("store")), "{renamed}");
    let shown = call(3, "environment_show", json!({}));
    assert_eq!(shown.get("at"), Some(&json!("store/staging")), "{shown}");
    drop(stdin);
    assert!(exited_within(&mut server, Duration::from_secs(10)).is_some());
}
