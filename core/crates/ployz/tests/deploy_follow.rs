//! The actual CLI follows Cloud over HTTP, including signals during Store reads.
#![cfg(unix)]

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use ployz_store::{DeploymentStatus, DeploymentView, DeploymentsView, View, Written};
use serde_json::{Value, json};

fn view(number: u32, status: DeploymentStatus) -> DeploymentView {
    serde_json::from_value(json!({
        "id": if number == 7 { "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" } else { "cccccccc-cccc-4ccc-8ccc-cccccccccccc" },
        "environment_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "number": number,
        "status": status, "saved": 1, "services": [], "remove": false, "admitted_at": 1,
        "in_flight": status.in_flight(), "outcome": null,
        "environment": {"id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "project": "shop", "name": "production", "revision": 1},
        "namespace": "shop-production", "preview": null, "builds": [], "runtime_names": {}, "nodes": []
    })).unwrap()
}

struct Reply {
    path: &'static str,
    kind: &'static str,
    status: u16,
    body: Value,
    interrupt: bool,
}

fn admitted() -> Reply {
    Reply {
        path: "/api/config/write",
        kind: "admit",
        status: 200,
        body: serde_json::to_value(Written::Deployment(
            view(7, DeploymentStatus::Queued).deployment,
        ))
        .unwrap(),
        interrupt: false,
    }
}

fn observed(number: u32, status: DeploymentStatus) -> Reply {
    Reply {
        path: "/api/config/read",
        kind: "deployment",
        status: 200,
        body: serde_json::to_value(View::Deployment(Box::new(view(number, status)))).unwrap(),
        interrupt: false,
    }
}

fn replacement() -> Reply {
    let newer = view(8, DeploymentStatus::Running);
    Reply {
        path: "/api/config/read",
        kind: "deployments",
        status: 200,
        body: serde_json::to_value(View::Deployments(DeploymentsView {
            environment: newer.environment,
            deployments: vec![newer.deployment],
            next_cursor: None,
        }))
        .unwrap(),
        interrupt: false,
    }
}

fn serve(mut stream: TcpStream, reply: Reply, pid: u32) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert_eq!(first.split_whitespace().nth(1), Some(reply.path));
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let kind = body
        .get("query")
        .or_else(|| body.get("command"))
        .and_then(Value::as_str);
    assert_eq!(kind, Some(reply.kind), "{body}");
    if reply.interrupt {
        assert!(
            Command::new("kill")
                .args(["-INT", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        // The response races with the independently driven signal owner, not another key press.
        std::thread::sleep(Duration::from_millis(50));
    }
    let body = reply.body.to_string();
    write!(stream, "HTTP/1.1 {} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", reply.status, body.len()).unwrap();
}

fn run(replies: Vec<Reply>) -> (Output, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    run_at(replies, root.path())
}

fn run_at(replies: Vec<Reply>, root: &std::path::Path) -> (Output, Vec<Value>) {
    let events = root.join("events.ndjson");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(root)
        .env("PLOYZ_CONFIG", root.join("default.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_CONTEXT")
        .env("PLOYZ_TOKEN", "test-follow-token")
        .env("PLOYZ_CLOUD_URL", url)
        .arg("--ployz-config")
        .arg(root.join("selected team.yaml"))
        .args([
            "--json",
            "--color=never",
            "deploy",
            "--project",
            "shop",
            "--env",
            "production",
            "--events",
        ])
        .arg(&events)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (done, stopped) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut replies = VecDeque::from(replies);
        let deadline = Instant::now() + Duration::from_secs(20);
        while stopped.try_recv().is_err() {
            assert!(
                Instant::now() < deadline,
                "CLI did not finish its HTTP script"
            );
            match listener.accept() {
                Ok((stream, _)) => serve(
                    stream,
                    replies
                        .pop_front()
                        .expect("unexpected request, possibly remote Cancel"),
                    pid,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("{error}"),
            }
        }
        assert!(replies.is_empty(), "CLI skipped a required request");
    });
    let deadline = Instant::now() + Duration::from_secs(22);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            let _ = done.send(());
            let _ = server.join();
            panic!("CLI exceeded its follow deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    done.send(()).unwrap();
    server.join().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert!(
        output.status.signal().is_none(),
        "CLI terminated by signal {:?}: {}",
        output.status.signal(),
        String::from_utf8_lossy(&output.stderr)
    );
    let events = std::fs::read_to_string(events)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (output, events)
}

#[test]
fn cloud_replacement_resets_event_dedup_without_changing_the_ndjson_shape() {
    let (output, events) = run(vec![
        admitted(),
        observed(7, DeploymentStatus::Running),
        observed(7, DeploymentStatus::Running),
        observed(7, DeploymentStatus::Superseded),
        replacement(),
        observed(8, DeploymentStatus::Running),
        observed(8, DeploymentStatus::Applied),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("number"), Some(&json!(8)));
    assert_eq!(
        events,
        vec![
            json!({"type":"deployment", "status":"running", "nodes":[]}),
            json!({"type":"deployment", "status":"running", "nodes":[]}),
            json!({"type":"deployment", "status":"applied", "nodes":[]}),
        ]
    );
}

#[test]
fn cloud_interrupt_preserves_last_known_result_over_a_late_read_error() {
    for replacing in [false, true] {
        let mut replies = vec![admitted(), observed(7, DeploymentStatus::Running)];
        if replacing {
            replies.push(observed(7, DeploymentStatus::Superseded));
        }
        let mut late = if replacing {
            replacement()
        } else {
            observed(7, DeploymentStatus::Running)
        };
        late.interrupt = true;
        late.status = 500;
        late.body = json!({"error":{"code":"unavailable", "message":"late read error", "cause":[], "details":null}});
        replies.push(late);
        let (output, _) = run(replies);
        assert_eq!(
            output.status.code(),
            Some(130),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.get("number"), Some(&json!(7)));
        assert_eq!(
            result.get("status"),
            Some(&json!(if replacing { "superseded" } else { "running" }))
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("late read error"));
        if !replacing {
            assert!(stderr.contains("The Cloud Deployment continues."));
        }
    }
}

#[test]
fn cloud_completion_racing_interrupt_keeps_terminal_result_and_exit_130() {
    let mut terminal = observed(7, DeploymentStatus::Applied);
    terminal.interrupt = true;
    let (output, events) = run(vec![
        admitted(),
        observed(7, DeploymentStatus::Running),
        terminal,
    ]);
    assert_eq!(output.status.code(), Some(130));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("status"), Some(&json!("applied")));
    assert_eq!(
        events.last().unwrap().get("status"),
        Some(&json!("applied"))
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Deployment continues"));
}

#[test]
fn cloud_failure_recovery_keeps_selected_config_and_actual_runtime_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut failed = serde_json::to_value(view(7, DeploymentStatus::Failed)).unwrap();
    *failed.get_mut("runtime_names").unwrap() = json!({"web": "private-web"});
    *failed.get_mut("outcome").unwrap() =
        json!({"type": "executed", "summary": {}, "reason": "web failed", "cause": ["denied"]});
    *failed.get_mut("nodes").unwrap() = json!([{"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "web", "outcome": "failed", "rows": [{"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "edge", "state": "failed", "reason": "CreateContainer failed", "cause": ["denied"], "log": [], "started_at": 1, "finished_at": 2}]}]);
    let mut reply = observed(7, DeploymentStatus::Failed);
    reply.body = serde_json::to_value(View::Deployment(Box::new(
        serde_json::from_value(failed.clone()).unwrap(),
    )))
    .unwrap();
    let (output, events) = run_at(vec![admitted(), reply], root.path());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("status"), Some(&json!("failed")));
    assert_eq!(result.get("nodes"), failed.get("nodes"));
    assert_eq!(events.len(), 1);
    let stderr = String::from_utf8(output.stderr).unwrap();
    let hints = stderr
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("inspect: ")
                .or_else(|| line.trim().strip_prefix("retry: "))
        })
        .map(|line| shell_words::split(line).unwrap())
        .collect::<Vec<_>>();
    let selected = root.path().join("selected team.yaml");
    let config = selected.to_str().unwrap();
    assert_eq!(
        hints,
        [
            vec![
                "ployz",
                "--ployz-config",
                config,
                "logs",
                "shop-production/private-web",
                "--machine",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--project",
                "shop",
                "--env",
                "production"
            ],
            vec![
                "ployz",
                "--ployz-config",
                config,
                "deploy",
                "--project",
                "shop",
                "--env",
                "production"
            ],
        ],
        "{stderr}"
    );
}
