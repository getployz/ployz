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

/// One `--events` line: each names its Deployment, so a replacement's lines say which.
fn event(number: u32, status: &str) -> Value {
    let id = if number == 7 {
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    } else {
        "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    };
    json!({"type": "deployment", "id": id, "number": number, "status": status, "nodes": []})
}

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
    interrupt: Option<Interrupt>,
    delay: Duration,
}

enum Interrupt {
    DuringRead,
    AfterObserved(Duration),
    /// Cloud takes the request and closes the connection without answering.
    HangUp,
}

struct Exchange {
    requested: Instant,
    responded: Instant,
}

struct Run {
    output: Output,
    events: Vec<Value>,
    exchanges: Vec<Exchange>,
    signal_to_exit: Option<Duration>,
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
        interrupt: None,
        delay: Duration::ZERO,
    }
}

fn observed(number: u32, status: DeploymentStatus) -> Reply {
    Reply {
        path: "/api/config/read",
        kind: "deployment",
        status: 200,
        body: serde_json::to_value(View::Deployment(Box::new(view(number, status)))).unwrap(),
        interrupt: None,
        delay: Duration::ZERO,
    }
}

fn replacement() -> Reply {
    let newer = view(8, DeploymentStatus::Running);
    Reply {
        path: "/api/config/read",
        kind: "deployments",
        status: 200,
        body: serde_json::to_value(View::Deployments(Box::new(DeploymentsView {
            environment: newer.environment,
            deployments: vec![newer.deployment.clone()],
            next_cursor: None,
            latest: Some(newer.deployment.clone()),
            in_flight: vec![newer.deployment],
        })))
        .unwrap(),
        interrupt: None,
        delay: Duration::ZERO,
    }
}

fn real_wait(duration: Duration) {
    // real-wait: the CLI process and its signal owner report nothing this test can await
    std::thread::sleep(duration);
}

fn interrupt(pid: u32) -> Instant {
    let sent = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    sent
}

/// What the fake Cloud answers, one request at a time.
trait Script: Send + 'static {
    /// The reply to a request of `kind` (the query or command it names).
    fn reply(&mut self, kind: &str) -> Reply;
    /// Checked once the CLI exits.
    fn finished(&self);
}

/// Replies in order: each request takes the next one.
impl Script for VecDeque<Reply> {
    fn reply(&mut self, _: &str) -> Reply {
        self.pop_front()
            .expect("unexpected request, possibly remote Cancel")
    }

    fn finished(&self) {
        assert!(self.is_empty(), "CLI skipped a required request");
    }
}

fn serve(
    mut stream: TcpStream,
    script: &mut dyn Script,
    pid: u32,
    events: &std::path::Path,
) -> (Exchange, Option<Instant>) {
    let requested = Instant::now();
    // An accepted socket inherits the listener's nonblocking mode on macOS.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
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
    let reply = script.reply(kind.unwrap_or_default());
    assert_eq!(first.split_whitespace().nth(1), Some(reply.path));
    assert_eq!(kind, Some(reply.kind), "{body}");
    if matches!(reply.interrupt, Some(Interrupt::HangUp)) {
        return (
            Exchange {
                requested,
                responded: Instant::now(),
            },
            None,
        );
    }
    let mut sent = None;
    if matches!(reply.interrupt, Some(Interrupt::DuringRead)) {
        sent = Some(interrupt(pid));
        // The response races with the independently driven signal owner, not another key press.
        real_wait(Duration::from_millis(50));
    }
    real_wait(reply.delay);
    let body = reply.body.to_string();
    write!(stream, "HTTP/1.1 {} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", reply.status, body.len()).unwrap();
    stream.flush().unwrap();
    let responded = Instant::now();
    if let Some(Interrupt::AfterObserved(delay)) = reply.interrupt {
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::metadata(events).map_or(0, |events| events.len()) == 0 {
            assert!(Instant::now() < deadline, "CLI did not consume its view");
            real_wait(Duration::from_millis(1));
        }
        real_wait(delay);
        sent = Some(interrupt(pid));
    }
    (
        Exchange {
            requested,
            responded,
        },
        sent,
    )
}

fn run(replies: Vec<Reply>) -> (Output, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    run_at(replies, root.path())
}

fn run_at(replies: Vec<Reply>, root: &std::path::Path) -> (Output, Vec<Value>) {
    let run = measured_run(replies, root);
    (run.output, run.events)
}

fn measured_run(replies: Vec<Reply>, root: &std::path::Path) -> Run {
    let events = root.join("events.ndjson");
    let mut deploy = vec![
        "deploy",
        "--project",
        "shop",
        "--env",
        "production",
        "--events",
    ];
    deploy.push(events.to_str().unwrap());
    cli_run(replies, root, &deploy)
}

fn cli_run(replies: Vec<Reply>, root: &std::path::Path, args: &[&str]) -> Run {
    let args: Vec<&str> = std::iter::once("--json")
        .chain(args.iter().copied())
        .collect();
    scripted_run(
        VecDeque::from(replies),
        root,
        &args,
        &[],
        Duration::from_secs(20),
    )
}

/// Runs the CLI against a fake Cloud answering from `script` for up to `deadline`.
fn scripted_run(
    mut script: impl Script,
    root: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: Duration,
) -> Run {
    let events = root.join("events.ndjson");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let child = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .current_dir(root)
        .env("PLOYZ_CONFIG", root.join("default.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_CONNECT")
        .env_remove("PLOYZ_CONTEXT")
        .env("PLOYZ_TOKEN", "test-follow-token")
        .env("PLOYZ_CLOUD_URL", url)
        .envs(env.iter().copied())
        .arg("--ployz-config")
        .arg(root.join("selected team.yaml"))
        .arg("--color=never")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let (done, stopped) = mpsc::channel();
    let server_events = events.clone();
    let server = std::thread::spawn(move || {
        let mut exchanges = Vec::new();
        let mut signal = None;
        let deadline = Instant::now() + deadline;
        while stopped.try_recv().is_err() {
            assert!(
                Instant::now() < deadline,
                "CLI did not finish its HTTP script"
            );
            match listener.accept() {
                Ok((stream, _)) => {
                    let (exchange, sent) = serve(stream, &mut script, pid, &server_events);
                    exchanges.push(exchange);
                    signal = signal.or(sent);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    real_wait(Duration::from_millis(5))
                }
                Err(error) => panic!("{error}"),
            }
        }
        script.finished();
        (exchanges, signal)
    });
    let (exit, exited) = mpsc::channel();
    std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = exit.send((Instant::now(), output));
    });
    let Ok((exited, output)) = exited.recv_timeout(deadline + Duration::from_secs(2)) else {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
        let _ = done.send(());
        let _ = server.join();
        panic!("CLI exceeded its follow deadline");
    };
    let output = output.unwrap();
    done.send(()).unwrap();
    let (exchanges, signal) = server.join().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert!(
        output.status.signal().is_none(),
        "CLI terminated by signal {:?}: {}",
        output.status.signal(),
        String::from_utf8_lossy(&output.stderr)
    );
    let events = std::fs::read_to_string(events)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    Run {
        output,
        events,
        exchanges,
        signal_to_exit: signal.map(|sent| exited.duration_since(sent)),
    }
}

#[test]
fn cloud_replacement_resets_event_dedup_without_changing_the_ndjson_shape() {
    let root = tempfile::tempdir().unwrap();
    let mut delayed = observed(7, DeploymentStatus::Running);
    delayed.delay = Duration::from_millis(1200);
    let Run {
        output,
        events,
        exchanges,
        ..
    } = measured_run(
        vec![
            admitted(),
            observed(7, DeploymentStatus::Running),
            delayed,
            observed(7, DeploymentStatus::Superseded),
            replacement(),
            observed(8, DeploymentStatus::Running),
            observed(8, DeploymentStatus::Applied),
        ],
        root.path(),
    );
    for next in [2, 3, 6] {
        let gap = exchanges
            .get(next)
            .unwrap()
            .requested
            .duration_since(exchanges.get(next - 1).unwrap().responded);
        assert!(
            gap >= Duration::from_millis(950),
            "follow read burst after {gap:?}"
        );
        assert!(
            gap < Duration::from_millis(1500),
            "follow read delayed by {gap:?}"
        );
    }
    for next in [1, 4, 5] {
        let gap = exchanges
            .get(next)
            .unwrap()
            .requested
            .duration_since(exchanges.get(next - 1).unwrap().responded);
        assert!(
            gap < Duration::from_millis(500),
            "immediate follow delayed by {gap:?}"
        );
    }
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
            event(7, "running"),
            event(8, "running"),
            event(8, "applied"),
        ]
    );
}

#[test]
fn deploy_reports_a_server_gap_once_and_keeps_json_warnings() {
    use ployz_core::{
        DeployWarning, MachineId, MachineName, ObservationGap, ObservationGapReason,
        ObservationKind,
    };
    use ployz_store::DeploymentWarning;
    let machine_id = MachineId::parse("2".repeat(32)).unwrap();
    let warnings = vec![
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Container,
            machine_id,
            gap: Some(ObservationGap {
                machine_name: MachineName::parse("machine-2").unwrap(),
                reason: ObservationGapReason::Down,
            }),
        },
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Volume,
            machine_id,
            gap: None,
        },
        DeployWarning::MixedAfterReturn {
            machine: "machine-2".into(),
        },
    ];
    let mut applied = view(7, DeploymentStatus::Applied);
    applied.deployment.warnings = warnings
        .iter()
        .cloned()
        .map(|warning| DeploymentWarning::Deploy { warning })
        .collect();
    let mut reply = observed(7, DeploymentStatus::Applied);
    reply.body = serde_json::to_value(View::Deployment(Box::new(applied))).unwrap();
    let (output, _) = run(vec![admitted(), reply]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(
        stderr
            .matches("deployment observations are incomplete")
            .count(),
        1,
        "{stderr}"
    );
    assert!(!stderr.contains("observation omitted"), "{stderr}");
    assert!(!stderr.contains(machine_id.as_str()), "{stderr}");
    assert!(stderr.contains("If machine-2 restarts"), "{stderr}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result.pointer("/warnings"),
        Some(
            &serde_json::to_value(
                warnings
                    .into_iter()
                    .map(|warning| DeploymentWarning::Deploy { warning })
                    .collect::<Vec<_>>()
            )
            .unwrap()
        )
    );
}

#[test]
fn cloud_interrupt_during_follow_wait_exits_without_another_read_or_remote_cancel() {
    for delay in [
        Duration::ZERO,
        Duration::from_millis(100),
        Duration::from_millis(850),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut running = observed(7, DeploymentStatus::Running);
        running.interrupt = Some(Interrupt::AfterObserved(delay));
        let Run {
            output,
            events,
            signal_to_exit,
            ..
        } = measured_run(vec![admitted(), running], root.path());
        assert_eq!(output.status.code(), Some(130), "{output:?}");
        let elapsed = signal_to_exit.unwrap();
        assert!(
            elapsed < Duration::from_millis(500),
            "wait interruption took {elapsed:?}"
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.get("number"), Some(&json!(7)));
        assert_eq!(result.get("status"), Some(&json!("running")));
        assert_eq!(events, vec![event(7, "running")]);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("The Cloud Deployment continues.")
        );
    }
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
        late.interrupt = Some(Interrupt::DuringRead);
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
fn cloud_interrupt_before_wait_keeps_successful_read_evidence() {
    for first_read in [true, false] {
        let mut replies = vec![admitted()];
        if !first_read {
            replies.push(observed(7, DeploymentStatus::Queued));
        }
        let mut late = observed(7, DeploymentStatus::Running);
        late.interrupt = Some(Interrupt::DuringRead);
        replies.push(late);
        let (output, events) = run(replies);
        assert_eq!(output.status.code(), Some(130), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.get("status"), Some(&json!("running")));
        let mut expected = Vec::new();
        if !first_read {
            expected.push(event(7, "queued"));
        }
        expected.push(event(7, "running"));
        assert_eq!(events, expected);
    }
    let mut late = replacement();
    late.interrupt = Some(Interrupt::DuringRead);
    let (output, events) = run(vec![
        admitted(),
        observed(7, DeploymentStatus::Superseded),
        late,
    ]);
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("number"), Some(&json!(7)));
    assert_eq!(result.get("status"), Some(&json!("superseded")));
    assert!(events.is_empty());
}

#[test]
fn cloud_completion_racing_interrupt_keeps_terminal_result_and_exit_130() {
    let mut terminal = observed(7, DeploymentStatus::Applied);
    terminal.interrupt = Some(Interrupt::DuringRead);
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
    *failed.get_mut("nodes").unwrap() = json!([{"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "web", "outcome": "failed", "rows": [{"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "edge", "state": "failed", "reason": "StartContainer failed", "cause": ["denied"], "step": "start", "log": null, "history": null, "started_at": 1, "finished_at": 2}]}]);
    let mut reply = observed(7, DeploymentStatus::Failed);
    reply.body = serde_json::to_value(View::Deployment(Box::new(
        serde_json::from_value(failed.clone()).unwrap(),
    )))
    .unwrap();
    let (output, events) = run_at(vec![admitted(), reply], root.path());
    assert_eq!(output.status.code(), Some(1), "{output:?}");
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
                "web",
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

fn hung_up(mut reply: Reply) -> Reply {
    reply.interrupt = Some(Interrupt::HangUp);
    reply
}

/// The one `inspect` command `error` names, without the `--ployz-config` every hint carries.
fn inspected(error: &Value) -> String {
    let [command] = error
        .pointer("/details/inspect")
        .and_then(Value::as_array)
        .unwrap()
        .as_slice()
    else {
        panic!("{error}");
    };
    let mut words = shell_words::split(command.as_str().unwrap()).unwrap();
    if let Some(at) = words.iter().position(|word| word == "--ployz-config") {
        words.drain(at..at + 2);
    }
    words.join(" ")
}

fn failure(output: &Output) -> Value {
    let printed = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    serde_json::from_slice::<Value>(printed)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(printed)))
        .get("error")
        .cloned()
        .unwrap_or_else(|| panic!("{output:?}"))
}

#[test]
fn a_write_cloud_took_but_never_answered_says_it_may_have_applied() {
    let root = tempfile::tempdir().unwrap();
    let add = Reply {
        path: "/api/config/write",
        kind: "create_service",
        status: 200,
        body: Value::Null,
        interrupt: Some(Interrupt::HangUp),
        delay: Duration::ZERO,
    };
    let Run { output, .. } = cli_run(
        vec![add],
        root.path(),
        &[
            "service",
            "add",
            "frz",
            "--image",
            "nginx:alpine",
            "--project",
            "shop",
            "--env",
            "production",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = failure(&output);
    assert_eq!(error.get("code"), Some(&json!("unavailable")), "{error}");
    assert!(
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap()
            .ends_with("stopped answering after it got this change, so it may have applied."),
        "{error}"
    );
    assert_eq!(
        error.pointer("/details/writes"),
        Some(&json!("unknown")),
        "{error}"
    );
    assert_eq!(
        inspected(&error),
        "ployz diff --project shop --env production"
    );
}

#[test]
fn a_deploy_cloud_took_but_never_answered_points_at_its_deployments() {
    let (output, _) = run(vec![hung_up(admitted())]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = failure(&output);
    assert_eq!(
        error.pointer("/details/writes"),
        Some(&json!("unknown")),
        "{error}"
    );
    assert_eq!(
        inspected(&error),
        "ployz deployment ls --project shop --env production"
    );
}

#[test]
fn losing_cloud_while_following_says_the_deployment_keeps_running() {
    let (output, _) = run(vec![
        admitted(),
        hung_up(observed(7, DeploymentStatus::Running)),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = failure(&output);
    assert_eq!(
        error.get("message"),
        Some(&json!(
            "Stopped following Deployment #7; it keeps running in Ployz Cloud."
        )),
        "{error}"
    );
    assert_eq!(
        inspected(&error),
        "ployz deployment show 7 --project shop --env production"
    );
}

#[test]
fn losing_cloud_after_an_observation_failure_prints_the_notice() {
    use ployz_core::{DeployWarning, MachineId, ObservationKind};
    use ployz_store::DeploymentWarning;

    let mut running = view(7, DeploymentStatus::Running);
    running.builds = serde_json::from_value(json!([
        {"service": "web", "commit": null, "status": "building", "message": null, "server": null}
    ]))
    .unwrap();
    running.deployment.warnings = vec![DeploymentWarning::Deploy {
        warning: DeployWarning::ObservationFailed {
            kind: ObservationKind::Volume,
            machine_id: MachineId::parse("2".repeat(32)).unwrap(),
            machine_name: None,
            message: "volume query timed out".into(),
            gap: None,
        },
    }];
    let mut failed = observed(7, DeploymentStatus::Running);
    failed.status = 500;
    failed.body = json!({"error": {"code": "unavailable", "message": "Store read failed", "cause": [], "details": null}});
    let (output, _) = run(vec![
        admitted(),
        Reply {
            body: serde_json::to_value(View::Deployment(Box::new(running))).unwrap(),
            ..observed(7, DeploymentStatus::Running)
        },
        failed,
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        stderr.contains("! volume observation failed on"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("volume query timed out").count(),
        1,
        "{stderr}"
    );
}

#[test]
fn losing_cloud_while_looking_for_a_replacement_keeps_the_superseded_status() {
    let (output, _) = run(vec![
        admitted(),
        observed(7, DeploymentStatus::Superseded),
        hung_up(replacement()),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = failure(&output);
    assert_eq!(
        error.get("message"),
        Some(&json!(
            "Deployment #7 was superseded, and Ployz Cloud stopped answering before its replacement could be read."
        )),
        "{error}"
    );
    assert_eq!(
        inspected(&error),
        "ployz deployment ls --project shop --env production"
    );
}

#[test]
fn losing_cloud_after_a_replacement_names_the_deployment_being_followed() {
    let (output, _) = run(vec![
        admitted(),
        observed(7, DeploymentStatus::Superseded),
        replacement(),
        hung_up(observed(8, DeploymentStatus::Running)),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error = failure(&output);
    assert_eq!(
        error.get("message"),
        Some(&json!(
            "Stopped following Deployment #8; it keeps running in Ployz Cloud."
        )),
        "{error}"
    );
    assert_eq!(
        inspected(&error),
        "ployz deployment show 8 --project shop --env production"
    );
}

/// Deployment `number` named `named`, with a Service node of each `(name, outcome)`.
fn narrowed(
    number: u32,
    status: DeploymentStatus,
    named: &str,
    nodes: &[(&str, &str)],
) -> DeploymentView {
    let mut view = view(number, status);
    view.deployment.services = serde_json::from_value(json!([named])).unwrap();
    view.nodes = nodes
        .iter()
        .enumerate()
        .map(|(n, (name, outcome))| {
            serde_json::from_value(json!({
                "type": "service", "id": format!("dddddddd-dddd-4ddd-8ddd-dddddddddd{n:02}"),
                "name": name, "outcome": outcome,
            }))
            .unwrap()
        })
        .collect();
    view
}

fn read(view: &DeploymentView) -> Reply {
    Reply {
        body: serde_json::to_value(View::Deployment(Box::new(view.clone()))).unwrap(),
        ..observed(7, DeploymentStatus::Running)
    }
}

fn listed(newer: &DeploymentView) -> Reply {
    Reply {
        body: serde_json::to_value(View::Deployments(Box::new(DeploymentsView {
            environment: newer.environment.clone(),
            deployments: vec![newer.deployment.clone()],
            next_cursor: None,
            latest: Some(newer.deployment.clone()),
            in_flight: if newer.deployment.status.in_flight() {
                vec![newer.deployment.clone()]
            } else {
                Vec::new()
            },
        })))
        .unwrap(),
        ..replacement()
    }
}

/// `deploy web`, superseded by `newer`, which the CLI reads back as `candidate`;
/// `follows` scripts the reads that follow `newer` to its end.
fn superseded_by(newer: &DeploymentView, candidate: Reply, follows: bool) -> Output {
    let root = tempfile::tempdir().unwrap();
    let ours = narrowed(7, DeploymentStatus::Queued, "web", &[("web", "pending")]);
    let superseded = narrowed(
        7,
        DeploymentStatus::Superseded,
        "web",
        &[("web", "pending")],
    );
    let mut replies = vec![
        Reply {
            body: serde_json::to_value(Written::Deployment(ours.deployment)).unwrap(),
            ..admitted()
        },
        read(&superseded),
        listed(newer),
        candidate,
    ];
    if follows {
        let mut applied = newer.clone();
        applied.deployment.status = DeploymentStatus::Applied;
        replies.extend([read(newer), read(&applied)]);
    }
    let events = root.path().join("events.ndjson");
    let Run { output, .. } = cli_run(
        replies,
        root.path(),
        &[
            "deploy",
            "web",
            "--project",
            "shop",
            "--env",
            "production",
            "--events",
            events.to_str().unwrap(),
        ],
    );
    output
}

fn ended_superseded(output: &Output) {
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("number"), Some(&json!(7)), "{result}");
    assert_eq!(result.get("status"), Some(&json!("superseded")), "{result}");
}

#[test]
fn a_superseded_narrowed_deploy_follows_a_newer_one_that_ships_its_service_by_reference() {
    let newer = narrowed(
        8,
        DeploymentStatus::Running,
        "client",
        &[("client", "pending"), ("web", "pending")],
    );
    let output = superseded_by(&newer, read(&newer), true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.get("number"), Some(&json!(8)), "{result}");
    assert_eq!(result.get("status"), Some(&json!("applied")), "{result}");
}

#[test]
fn a_superseded_narrowed_deploy_stops_when_the_newer_one_leaves_its_service_out() {
    let newer = narrowed(
        8,
        DeploymentStatus::Running,
        "client",
        &[("client", "pending")],
    );
    ended_superseded(&superseded_by(&newer, read(&newer), false));
}

#[test]
fn a_superseded_narrowed_deploy_stops_when_the_newer_one_left_its_service_unchanged() {
    for outcome in ["unchanged", "not_attempted"] {
        let newer = narrowed(
            8,
            DeploymentStatus::Applied,
            "client",
            &[("client", "deployed"), ("web", outcome)],
        );
        ended_superseded(&superseded_by(&newer, read(&newer), false));
    }
}

#[test]
fn a_superseded_narrowed_deploy_keeps_its_result_when_reading_the_newer_one_fails() {
    let newer = narrowed(
        8,
        DeploymentStatus::Running,
        "client",
        &[("client", "pending"), ("web", "pending")],
    );
    let failed = Reply {
        status: 500,
        body: json!({"error": {"code": "internal", "message": "boom"}}),
        ..read(&newer)
    };
    ended_superseded(&superseded_by(&newer, failed, false));
}

#[test]
fn a_superseded_narrowed_deploy_stops_when_the_newer_one_ships_none_of_its_nodes() {
    for nodes in [
        &[("client", "unchanged")][..],
        &[("client", "unchanged"), ("web", "unchanged")][..],
    ] {
        let newer = narrowed(8, DeploymentStatus::Applied, "client", nodes);
        ended_superseded(&superseded_by(&newer, read(&newer), false));
    }
}

/// Cloud keeps Deployment #7 queued until the CLI reads the Environment's
/// Deployments, which list `ahead` with it; then #7 applies.
struct Unclaimed {
    ahead: Option<DeploymentView>,
    listed: bool,
    admitted: Option<Instant>,
}

impl Script for Unclaimed {
    fn reply(&mut self, kind: &str) -> Reply {
        match kind {
            "admit" => {
                self.admitted = Some(Instant::now());
                admitted()
            }
            "deployments" => {
                self.listed = true;
                let ours = view(7, DeploymentStatus::Queued);
                let mut in_flight = vec![ours.deployment.clone()];
                in_flight.extend(self.ahead.iter().map(|ahead| ahead.deployment.clone()));
                Reply {
                    body: serde_json::to_value(View::Deployments(Box::new(DeploymentsView {
                        environment: ours.environment,
                        deployments: in_flight.clone(),
                        next_cursor: None,
                        latest: Some(ours.deployment),
                        in_flight,
                    })))
                    .unwrap(),
                    ..replacement()
                }
            }
            // Past the CLI's 1s wait and a poll, a CLI that never lists the Deployments still ends.
            _ if self.listed
                || self
                    .admitted
                    .is_some_and(|admitted| admitted.elapsed() > Duration::from_secs(4)) =>
            {
                observed(7, DeploymentStatus::Applied)
            }
            _ => observed(7, DeploymentStatus::Queued),
        }
    }

    fn finished(&self) {}
}

fn left_queued(ahead: Option<DeploymentView>) -> String {
    let root = tempfile::tempdir().unwrap();
    let Run { output, .. } = scripted_run(
        Unclaimed {
            ahead,
            listed: false,
            admitted: None,
        },
        root.path(),
        &["deploy", "--project", "shop", "--env", "production"],
        &[("PLOYZ_DEPLOY_UNCLAIMED_SECONDS", "1")],
        Duration::from_secs(15),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    stderr
}

#[test]
fn a_deploy_queued_behind_one_in_flight_says_which_it_waits_for() {
    let stderr = left_queued(Some(view(6, DeploymentStatus::Running)));
    assert!(
        stderr.contains("Deployment #7 waits for Deployment #6 to finish."),
        "{stderr}"
    );
    assert!(
        stderr.contains("ployz deployment show 6 --project shop --env production"),
        "{stderr}"
    );
    assert!(!stderr.contains("No Server has started"), "{stderr}");
    assert!(!stderr.contains("server ls"), "{stderr}");
}

#[test]
fn a_deploy_queued_with_nothing_ahead_says_no_server_started_it() {
    let stderr = left_queued(None);
    assert!(
        stderr.contains("No Server has started Deployment #7 yet; it stays queued until one does."),
        "{stderr}"
    );
    assert!(stderr.contains("ployz server ls"), "{stderr}");
}

/// Deployment #7, cancelled: the cancel stopped web, and api's create failed on its
/// own when `api_failed`. `marker` is the Store's `cancelled` on the outcome, which a
/// Store from before it never writes.
fn cancelled(reason: &str, cause: &[&str], marker: bool, api_failed: bool) -> DeploymentView {
    let mut cancelled = serde_json::to_value(view(7, DeploymentStatus::Cancelled)).unwrap();
    let mut outcome = json!({"type": "executed", "summary": {}, "reason": reason, "cause": cause});
    if marker {
        outcome
            .as_object_mut()
            .unwrap()
            .insert("cancelled".to_owned(), json!(true));
    }
    *cancelled.get_mut("outcome").unwrap() = outcome;
    let api = if api_failed {
        json!({"type": "service", "id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "name": "api", "outcome": "failed", "rows": [{"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "fsn-1", "state": "failed", "reason": "create Container failed", "cause": ["no space left on device"], "step": "create", "log": null, "history": null, "started_at": 1, "finished_at": 2}]})
    } else {
        json!({"type": "service", "id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "name": "api", "outcome": "deployed", "rows": [{"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "fsn-1", "state": "completed", "log": null, "history": null, "started_at": 1, "finished_at": 2}]})
    };
    *cancelled.get_mut("nodes").unwrap() = json!([
        api,
        {"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "web", "outcome": "failed", "cancelled": true, "rows": [{"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "fsn-1", "state": "failed", "reason": "Cancelled", "cause": [], "step": "create", "cancelled": true, "log": null, "history": null, "started_at": 1, "finished_at": 2}]}
    ]);
    serde_json::from_value(cancelled).unwrap()
}

/// What a person reads from `deployment show` and from `deploy` following `view` to its end.
fn shown_and_followed(view: &DeploymentView) -> (String, String) {
    let root = tempfile::tempdir().unwrap();
    let Run { output, .. } = scripted_run(
        VecDeque::from([read(view)]),
        root.path(),
        &["deployment", "show", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"],
        &[],
        Duration::from_secs(20),
    );
    assert!(output.status.success(), "{output:?}");
    let shown = String::from_utf8(output.stdout).unwrap();
    let Run { output, .. } = scripted_run(
        VecDeque::from([admitted(), read(view)]),
        root.path(),
        &["deploy", "--project", "shop", "--env", "production"],
        &[],
        Duration::from_secs(20),
    );
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    (shown, String::from_utf8(output.stderr).unwrap())
}

#[test]
fn a_cancelled_deployment_shows_no_reason_that_only_repeats_its_cancel() {
    let (shown, followed) = shown_and_followed(&cancelled("web: Cancelled", &[], true, false));
    assert!(
        shown.starts_with("Deployment #7 of shop/production: cancelled\n"),
        "{shown}"
    );
    assert!(!shown.contains("web: Cancelled"), "{shown}");
    assert!(!followed.contains("web: Cancelled"), "{followed}");
    assert!(
        followed.contains("error: Deployment #7 was cancelled.\n"),
        "{followed}"
    );
}

#[test]
fn a_cancelled_deployment_still_names_a_real_failure_and_an_older_stores_reason() {
    let (shown, followed) = shown_and_followed(&cancelled(
        "api: create Container failed",
        &["no space left on device"],
        false,
        true,
    ));
    assert!(
        shown.contains("  api: create Container failed\n    cause: no space left on device\n"),
        "{shown}"
    );
    assert!(
        followed
            .contains("error: api: create Container failed.\n  cause: no space left on device\n"),
        "{followed}"
    );
    // A Store from before the marker: the reason reads as it always has.
    let (shown, followed) = shown_and_followed(&cancelled("web: Cancelled", &[], false, false));
    assert!(shown.contains("\n  web: Cancelled\n"), "{shown}");
    assert!(followed.contains("error: web: Cancelled.\n"), "{followed}");
}

#[test]
fn deployment_show_reads_a_node_the_cancel_stopped_cancelled_beside_a_real_failure() {
    let (shown, _) = shown_and_followed(&cancelled("web: Cancelled", &[], true, false));
    assert!(
        shown.contains("\n  api: deployed\n  web: cancelled\n"),
        "{shown}"
    );
    let (shown, _) = shown_and_followed(&cancelled(
        "api: create Container failed",
        &["no space left on device"],
        false,
        true,
    ));
    assert!(
        shown.contains("\n  api: failed\n  web: cancelled\n"),
        "{shown}"
    );
    // A Store from before Cancelled nodes marks no node, so web reads failed.
    let mut older = cancelled("web: Cancelled", &[], false, false);
    for node in &mut older.nodes {
        node.cancelled = false;
    }
    let (shown, _) = shown_and_followed(&older);
    assert!(
        shown.contains("\n  api: deployed\n  web: failed\n"),
        "{shown}"
    );
}
