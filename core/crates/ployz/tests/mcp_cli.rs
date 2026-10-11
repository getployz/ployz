//! `ployz mcp` as a host runs it: stopping the server stops it, and the calls it is running.
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

fn children_of(parent: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit(") ")
                    .next()
                    .and_then(|rest| rest.split(' ').nth(1))
                    == Some(&parent.to_string())
            })
        })
        .collect()
}

fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !stat
            .rsplit(") ")
            .next()
            .is_some_and(|rest| rest.starts_with('Z'))
    })
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
    calls: Vec<u32>,
    _held: (TcpListener, std::net::TcpStream, tempfile::TempDir),
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
            "params": { "name": "project_ls", "arguments": {} },
        }),
    );
    let (held, _) = cloud.accept().unwrap();
    let calls = children_of(server.id());
    assert_eq!(calls.len(), 1, "one call is in flight: {calls:?}");
    Stuck {
        server,
        stdin,
        stdout,
        calls,
        _held: (cloud, held, home),
    }
}

fn assert_gone(calls: &[u32]) {
    let start = Instant::now();
    while calls.iter().any(|pid| running(*pid)) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the call outlived the server: {calls:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn closing_stdin_stops_the_server_and_the_call_it_is_running() {
    let Stuck {
        mut server,
        stdin,
        calls,
        ..
    } = stuck_call();
    drop(stdin);
    exited_within(&mut server, Duration::from_secs(5))
        .expect("the server stops once its stdin is closed");
    assert_gone(&calls);
}

#[test]
fn closing_stdout_stops_the_server_and_the_call_it_is_running() {
    let Stuck {
        mut server,
        mut stdin,
        stdout,
        calls,
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
    assert_gone(&calls);
}
