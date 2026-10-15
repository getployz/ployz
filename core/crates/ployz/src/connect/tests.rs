use std::{fs, os::unix::fs::symlink, process::Command as StdCommand, time::Duration};

use super::*;
use crate::context::SshDestination;
use ployz_core::{ONE_TARGET_BINARY_HEADER, ONE_TARGET_HEADER, Rpc};
use tokio::io::AsyncWriteExt;

#[test]
fn setup_retry_classifies_ssh_and_preserves_aggregate_cause() {
    use std::os::unix::process::ExitStatusExt;
    for (detail, retry, aggregate) in [
        ("Connection timed out", Retry::Now, Retry::AfterRestart),
        ("Connection refused", Retry::Now, Retry::AfterRestart),
        ("Permission denied (publickey)", Retry::Never, Retry::Never),
        ("Host key verification failed", Retry::Never, Retry::Never),
        (
            "user@jump: Permission denied (publickey).\nConnection closed by UNKNOWN port 65535",
            Retry::Never,
            Retry::Never,
        ),
    ] {
        let ssh_error = ConnectError::SshProbe {
            target: "host".to_owned(),
            status: std::process::ExitStatus::from_raw(255 << 8),
            failure: SshFailure::of(detail),
            detail: detail.to_owned(),
        };
        assert_eq!(ssh_error.retry(), retry);
        let error = ConnectError::AllFailed {
            selection: ConnectionSource::Direct,
            attempts: 1,
            retry: aggregate,
            last: Some(Box::new(ssh_error)),
        };
        assert_eq!(error.retry(), aggregate);
        let text = crate::ui::chain_text(&error);
        assert!(detail.lines().all(|line| text.contains(line)), "{text}");
        assert_eq!(
            text.contains("ssh-add"),
            detail.contains("Permission denied"),
            "{detail}"
        );
    }
}

#[tokio::test]
async fn setup_retry_preserves_transient_failures_in_either_connection_order() {
    let program = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ssh");
    for (hosts, retryable) in [
        (["user@transient", "user@permanent"], true),
        (["user@permanent", "user@transient"], true),
        (["user@permanent", "user@permanent"], false),
    ] {
        let selected = SelectedConnections {
            source: ConnectionSource::Context("prod".into()),
            connections: hosts
                .into_iter()
                .map(|host| Connection::ssh(SshDestination::parse(host).unwrap()))
                .collect(),
        };
        let error =
            match connect_selected_with(selected, Arc::new(SystemConnector::new(program))).await {
                Err(error) => error,
                Ok(_) => panic!("script must reject every connection"),
            };
        assert_eq!(error.retry() != Retry::Never, retryable, "{error}");
        if !retryable {
            assert!(
                crate::ui::chain_text(&error).contains("unlock your key with ssh-add"),
                "{error}"
            );
        }
    }
}

#[tokio::test]
async fn target_timeout_becomes_a_typed_partial_failure() {
    let error = apply_timeout(
        Some(Duration::from_millis(1)),
        std::future::pending::<Result<(), ConnectError>>(),
    )
    .await
    .map_err(rpc_error)
    .unwrap_err();

    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "target Machine RPC timed out");
}

#[test]
fn non_ascii_machine_targets_use_binary_metadata() {
    let target = MachineTarget::parse("München edge").unwrap();
    let request = target_request(ployz_core::OpaquePayload::new(Vec::new()), Some(&target));

    assert!(request.metadata().get(ONE_TARGET_HEADER).is_none());
    assert_eq!(
        request
            .metadata()
            .get_bin(ONE_TARGET_BINARY_HEADER)
            .unwrap()
            .to_bytes()
            .unwrap(),
        target.as_str()
    );
}

#[test]
fn system_ssh_command_uses_noninteractive_authentication() {
    let destination = SshDestination::parse("deploy@example.com:2222").unwrap();

    let args = ssh_args(
        &destination,
        Some(Path::new("/keys/deploy")),
        None,
        SystemConnector::default().ssh_timeout,
    );

    assert_eq!(
        args,
        [
            "-o",
            "ConnectTimeout=20",
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-T",
            "-p",
            "2222",
            "-i",
            "/keys/deploy",
            "deploy@example.com",
            "ployzd",
            "dial-stdio",
        ]
    );
    assert!(!args.iter().any(|arg| arg.contains("id_*")));
    assert!(args.contains(&"BatchMode=yes".to_owned()));
}

#[test]
fn transport_predicates_follow_the_grpc_code() {
    #[rustfmt::skip]
    let cases = [
        (tonic::Code::Ok,                 false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Cancelled,          false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Unknown,            false, false, false, RpcErrorCode::Internal),
        (tonic::Code::InvalidArgument,    false, false, false, RpcErrorCode::InvalidArgument),
        (tonic::Code::DeadlineExceeded,   true,  false, false, RpcErrorCode::Unavailable),
        (tonic::Code::NotFound,           false, false, true,  RpcErrorCode::NotFound),
        (tonic::Code::AlreadyExists,      false, false, false, RpcErrorCode::Conflict),
        (tonic::Code::PermissionDenied,   false, false, false, RpcErrorCode::Internal),
        (tonic::Code::ResourceExhausted,  false, false, false, RpcErrorCode::Internal),
        (tonic::Code::FailedPrecondition, false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Aborted,            false, false, false, RpcErrorCode::Conflict),
        (tonic::Code::OutOfRange,         false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Unimplemented,      false, false, false, RpcErrorCode::Unsupported),
        (tonic::Code::Internal,           false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Unavailable,        true,  true,  false, RpcErrorCode::Unavailable),
        (tonic::Code::DataLoss,           false, false, false, RpcErrorCode::Internal),
        (tonic::Code::Unauthenticated,    false, false, false, RpcErrorCode::Unauthenticated),
    ];

    for (code, retryable, unavailable, not_found, rpc) in cases {
        let error = TransportError::from(tonic::Status::new(code, "x"));
        assert_eq!(
            error.to_rpc_error().code.retry() == Retry::Now,
            retryable,
            "{code:?}"
        );
        assert_eq!(error.is_unavailable(), unavailable, "{code:?}");
        assert_eq!(error.is_not_found(), not_found, "{code:?}");
        assert_eq!(error.to_rpc_error().code, rpc, "{code:?}");
    }
}

#[test]
fn machine_rpc_status_prints_the_message_not_transport_metadata() {
    let status = tonic::Status::invalid_argument("invalid log time \"notatime\"");
    let leaked = status.to_string();
    assert!(leaked.contains("MetadataMap"), "{leaked}");
    assert!(leaked.contains("InvalidArgument"), "{leaked}");

    let error = TransportError::from(status);
    assert_eq!(error.to_string(), "invalid log time \"notatime\"");
    assert_eq!(
        crate::ui::chain_text(&ConnectError::Rpc(error)),
        "Machine RPC failed: invalid log time \"notatime\""
    );
}

#[test]
fn a_daemon_status_carries_its_causes_to_cause_lines() {
    let sent = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "transport error".into(),
        details: Value::Null,
        cause: vec!["keep-alive timed out".into()],
    };
    let status = ployz_core::rpc::caused_status(tonic::Code::Unavailable, &sent);
    let error = TransportError::from(status);
    assert_eq!(error.to_string(), "transport error");
    assert_eq!(crate::ui::causes(&error), ["keep-alive timed out"]);
    let rpc = error.to_rpc_error();
    assert_eq!(rpc.cause, ["keep-alive timed out"]);
    assert_eq!(rpc.details, Value::Null);
}

#[test]
fn reached_target_cleanup_rejections_are_not_unreachable_fallbacks() {
    assert!(
        ConnectError::Rpc(TransportError::from(tonic::Status::unavailable(
            "route failed"
        )))
        .is_unreachable()
    );
    assert!(
        !ConnectError::Rpc(TransportError::from(tonic::Status::unimplemented(
            "older daemon"
        )))
        .is_unreachable()
    );
    assert!(
        !ConnectError::Remote(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Docker is unavailable".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        })
        .is_unreachable()
    );
}

#[test]
fn setup_retry_waits_for_an_answer_but_not_past_one() {
    let dropped = io::Error::new(io::ErrorKind::BrokenPipe, "connection closed");
    for (status, retry) in [
        // The client made this status: the daemon never answered.
        (
            tonic::Status::from_error(Box::new(dropped)),
            Retry::AfterRestart,
        ),
        // The daemon answered with this status.
        (
            tonic::Status::unknown("upgrade record is unreadable"),
            Retry::Never,
        ),
    ] {
        let error = ConnectError::from(status);
        assert_eq!(error.retry(), retry, "{error}");
    }
}

#[test]
fn each_connect_failure_names_one_retry_class() {
    let remote = |code| {
        ConnectError::Remote(RpcError {
            code,
            message: "x".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        })
    };
    for (error, retry) in [
        (
            ConnectError::Io(io::ErrorKind::ConnectionRefused.into()),
            Retry::Now,
        ),
        (
            ConnectError::Io(io::Error::from_raw_os_error(11)),
            Retry::Now,
        ),
        (
            ConnectError::Io(io::ErrorKind::PermissionDenied.into()),
            Retry::Never,
        ),
        (
            ConnectError::from(tonic::Status::unavailable("x")),
            Retry::Now,
        ),
        (
            ConnectError::from(tonic::Status::cancelled("x")),
            Retry::AfterRestart,
        ),
        (
            ConnectError::from(tonic::Status::not_found("x")),
            Retry::Never,
        ),
        (ConnectError::EntryNotReady, Retry::AfterRestart),
        (remote(RpcErrorCode::Unavailable), Retry::Never),
        (ConnectError::ClientRefused, Retry::Never),
        (ConnectError::SshClientMissing, Retry::Never),
    ] {
        assert_eq!(error.retry(), retry, "{error}");
    }
}

#[test]
fn deadline_exceeded_is_retryable_not_unreachable() {
    let error = ConnectError::Rpc(TransportError::from(tonic::Status::deadline_exceeded(
        "timed out",
    )));
    assert_eq!(error.retry(), Retry::Now);
    assert!(!error.is_unreachable());
}

#[test]
fn only_a_spent_budget_or_the_router_reports_the_target_unanswered() {
    let dropped = io::Error::new(io::ErrorKind::BrokenPipe, "connection closed");
    for (error, unanswered) in [
        (
            ConnectError::Rpc(TransportError::budget_expired(
                "target Machine RPC timed out",
            )),
            true,
        ),
        (
            ConnectError::from(ployz_core::rpc::target_unanswered_status(&io::Error::new(
                io::ErrorKind::TimedOut,
                "operation timed out",
            ))),
            true,
        ),
        // A daemon sent these: something answered.
        (
            ConnectError::from(tonic::Status::deadline_exceeded("timed out")),
            false,
        ),
        (
            ConnectError::from(tonic::Status::unavailable("Machine is not participating")),
            false,
        ),
        // The client made this status: the entry dropped, not the target.
        (
            ConnectError::from(tonic::Status::from_error(Box::new(dropped))),
            false,
        ),
        (
            ConnectError::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused")),
            false,
        ),
        (ConnectError::EntryNotReady, false),
    ] {
        assert_eq!(error.target_unanswered(), unanswered, "{error}");
    }
}

#[tokio::test]
async fn cancelling_ssh_establishment_returns_without_waiting_for_ssh() {
    let root = std::env::temp_dir().join(format!("ployz-ssh-cancel-{}", std::process::id()));
    let program = root.join("ssh");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    symlink(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ssh"),
        &program,
    )
    .unwrap();
    let connector = SystemConnector::new(&program);
    let connection = Connection::ssh(SshDestination::parse("user@cancel").unwrap());

    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        // Finishing the readiness branch drops the pending connection and its SSH child.
        tokio::select! {
            result = connector.connect(&connection) => panic!("SSH finished before cancellation: {result:?}"),
            pid = async {
                loop {
                    if let Ok(contents) = fs::read_to_string(program.with_extension("pid"))
                        && let Ok(pid) = contents.trim().parse::<u32>()
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => pid,
        }
    })
    .await
    .expect("fake SSH did not publish its PID");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while StdCommand::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        assert!(tokio::time::Instant::now() < deadline, "ssh was not killed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn missing_ssh_program_names_the_local_client() {
    let connector = SystemConnector::new("/ployz-missing-ssh-client");
    let connection = Connection::ssh(SshDestination::parse("user@example.com").unwrap());
    let message = connector
        .connect(&connection)
        .await
        .expect_err("missing ssh program must fail");
    assert!(
        matches!(message, ConnectError::SshClientMissing),
        "{message:?}"
    );
    assert_eq!(
        message.to_string(),
        "local ssh client not found; install an ssh client"
    );
    assert!(!message.to_string().contains("os error"), "{message}");
}

#[tokio::test]
async fn missing_ssh_client_survives_connection_selection() {
    let error = match connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::ssh(
                SshDestination::parse("user@example.com").unwrap(),
            )],
        },
        Arc::new(SystemConnector::new("/ployz-missing-ssh-client")),
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("missing ssh program must fail"),
    };
    assert!(matches!(error, ConnectError::SshClientMissing), "{error:?}");
    let failure = crate::failure::Failure::from(error);
    assert_eq!(
        failure.to_string(),
        "local ssh client not found; install an ssh client"
    );
    assert!(!failure.to_string().contains("os error"), "{failure}");
}

#[tokio::test]
async fn missing_ssh_client_does_not_skip_later_non_ssh_connections() {
    let error = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![
                Connection::ssh(SshDestination::parse("user@example.com").unwrap()),
                Connection::unix("/ployz-missing-rpc.sock").unwrap(),
            ],
        },
        Arc::new(SystemConnector::new("/ployz-missing-ssh-client")),
    )
    .await
    .err()
    .expect("both unavailable connections must fail");
    assert!(
        matches!(error, ConnectError::AllFailed { attempts: 2, .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn stalled_ssh_probe_obeys_configured_timeout() {
    let program = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ssh");
    let connector = SystemConnector::new(program).with_ssh_timeout(Duration::from_millis(100));
    let connection = Connection::ssh(SshDestination::parse("user@timeout").unwrap());
    let result = tokio::time::timeout(Duration::from_secs(2), connector.connect(&connection))
        .await
        .expect("SSH setup must stop at its own deadline");
    assert!(
        matches!(result, Err(ConnectError::Io(ref error)) if error.kind() == io::ErrorKind::TimedOut),
        "{result:?}"
    );
}

#[test]
fn ssh_timeout_flag_is_global_and_reaches_transport_arguments() {
    for (args, seconds) in [
        (vec!["ployz", "ps"], 20),
        (vec!["ployz", "--ssh-timeout", "17", "ps"], 17),
        (vec!["ployz", "ps", "--ssh-timeout", "17"], 17),
        (vec!["ployz", "server", "ls", "--ssh-timeout", "17"], 17),
    ] {
        let root = crate::cli::command().try_get_matches_from(args).unwrap();
        let mut matches = &root;
        loop {
            let timeout = crate::cli::ssh_timeout(matches);
            assert_eq!(timeout, Duration::from_secs(seconds));
            let connector = SystemConnector::default().with_ssh_timeout(timeout);
            let args = ssh_args(
                &SshDestination::parse("user@host").unwrap(),
                None,
                None,
                connector.ssh_timeout,
            );
            assert!(args.contains(&format!("ConnectTimeout={seconds}")));
            let Some((_, child)) = matches.subcommand() else {
                break;
            };
            matches = child;
        }
    }
    for value in ["0", "-1", "abc", "1.5", "4294967296"] {
        assert!(
            crate::cli::command()
                .try_get_matches_from(["ployz", "ls", "--ssh-timeout", value])
                .is_err()
        );
    }
}

#[tokio::test]
async fn child_stream_preserves_half_close_and_reaps_on_cancellation() {
    use tokio::io::AsyncReadExt;
    let mut stream = spawn_child(
        Path::new("sh"),
        &["-c".into(), "cat; printf response-after-eof".into()],
    )
    .unwrap();
    stream.write_all(b"request").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut result = String::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_string(&mut result))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, "requestresponse-after-eof");
    assert!(stream._child.wait().await.unwrap().success());

    let child = spawn_child(Path::new("cat"), &[]).unwrap();
    let pid = child._child.id().unwrap();
    drop(child);
    tokio::time::timeout(Duration::from_secs(3), async {
        while StdCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled helper was not reaped");
}

#[tokio::test(start_paused = true)]
async fn local_socket_that_accepts_but_never_serves_fails_as_starting() {
    // systemd's ployz.socket queues connects while ployz.service is still starting.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ployz.sock");
    let _backlog = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let selected = SelectedConnections {
        source: ConnectionSource::Direct,
        connections: vec![Connection::unix(&path).unwrap()],
    };
    // Paused time: the confirm timeout elapses virtually. No outer guard, since
    // auto-advance would jump to it while the socket I/O is in flight.
    let error = match connect_selected_with(selected, Arc::new(SystemConnector::default())).await {
        Err(error) => error,
        Ok(_) => panic!("a daemon that never serves must not be confirmed"),
    };
    assert!(
        crate::ui::chain_text(&error).contains("may still be starting"),
        "{error}"
    );
    assert_eq!(error.retry(), Retry::AfterRestart, "{error}");
}

#[test]
fn a_dropped_connection_keeps_the_network_reason_as_the_deepest_cause() {
    #[derive(Debug, thiserror::Error)]
    #[error("transport error")]
    struct Transport(#[source] std::io::Error);

    let mut status = tonic::Status::unavailable("transport error");
    status.set_source(std::sync::Arc::new(Transport(std::io::Error::new(
        std::io::ErrorKind::ConnectionReset,
        "connection reset by peer",
    ))));
    let error = TransportError::from(status);
    assert_eq!(error.to_string(), "transport error");
    assert_eq!(crate::ui::causes(&error), ["connection reset by peer"]);
}

async fn prepare_volumes_answered_with(answer: Result<RpcError, tonic::Status>) -> RpcError {
    let (client, server) = crate::connect::test_support::rpc_client(move |_| {
        let answer = answer.clone();
        async move {
            let error = answer?;
            Ok(tonic::Response::new(
                ployz_core::RpcResponse::from(error).encode().unwrap(),
            ))
        }
    })
    .await;
    let error = client
        .invoke::<ployz_core::op::PrepareVolumes>(
            ployz_core::PrepareVolumesRequest { specs: Vec::new() },
            &MachineTarget::parse("machine-2").unwrap(),
            None,
        )
        .await
        .unwrap_err();
    server.abort();
    error
}

#[tokio::test]
async fn a_bare_unimplemented_status_names_the_capability_and_the_target() {
    let error = prepare_volumes_answered_with(Err(tonic::Status::unimplemented(""))).await;
    assert_eq!(error.code, RpcErrorCode::Unsupported);
    assert_eq!(
        error.details,
        json!({ "capability": "ployz.storage.prepare.v1", "target": "machine-2" })
    );

    let tail = TransportError::from(tonic::Status::unimplemented("")).name_refusal(
        ployz_core::op::TailLogs::PATH,
        &MachineTarget::parse("machine-2").unwrap(),
    );
    assert_eq!(
        tail.to_rpc_error().details.get("capability"),
        Some(&json!("ployz.logs.tail.v1"))
    );
}

#[tokio::test]
async fn a_daemon_refusal_keeps_its_own_sentence() {
    let refusal = RpcError {
        code: RpcErrorCode::Unsupported,
        message: "this Machine cannot host a mounted Provisioned Volume".into(),
        details: Value::Null,
        cause: Vec::new(),
    };
    assert_eq!(
        prepare_volumes_answered_with(Ok(refusal.clone())).await,
        refusal
    );
}

#[test]
fn other_statuses_and_unknown_routes_stay_unnamed() {
    let target = MachineTarget::parse("machine-2").unwrap();
    let unavailable = TransportError::from(tonic::Status::unavailable("down"))
        .name_refusal(ployz_core::op::PrepareVolumes::PATH, &target);
    assert_eq!(unavailable.to_rpc_error().details, Value::Null);

    let unknown_route = TransportError::from(tonic::Status::unimplemented(""))
        .name_refusal("/other.Service/Inspect", &target);
    assert_eq!(unknown_route.to_rpc_error().details, Value::Null);
}

#[tokio::test]
async fn the_transport_child_leaves_the_terminal_process_group() {
    use tokio::io::AsyncReadExt as _;
    let mut child = spawn_child(
        Path::new("sh"),
        &["-c".to_owned(), "ps -o pid=,pgid= -p $$".to_owned()],
    )
    .unwrap();
    let mut out = String::new();
    child.read_to_string(&mut out).await.unwrap();
    let ids: Vec<&str> = out.split_whitespace().collect();
    assert_eq!(ids.len(), 2, "pid and process group: {out:?}");
    assert_eq!(
        ids.first(),
        ids.last(),
        "the child leads its own process group"
    );
}

#[test]
fn only_a_target_reply_outlasts_a_later_silence() {
    let silent = || {
        ConnectError::Rpc(TransportError::budget_expired(
            "target Machine RPC timed out",
        ))
    };
    let dropped = || ConnectError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));

    // The entry dropped, then the target stayed silent: that is silence.
    let mut reply = None;
    let error = keep_target_reply(&mut reply, dropped());
    assert!(!error.target_replied());
    assert!(keep_target_reply(&mut reply, silent()).target_unanswered());

    // The target replied, the entry dropped, then silence: the reply stands.
    let mut reply = Some(ConnectError::from(tonic::Status::unavailable(
        "Machine is not participating",
    )));
    assert!(!keep_target_reply(&mut reply, dropped()).target_unanswered());
    let error = keep_target_reply(&mut reply, silent());
    assert!(error.target_replied(), "{error}");
}
