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
            relayed: false,
            selection: ConnectionSource::Direct,
            attempts: 1,
            server: None,
            retry: aggregate,
            reached: false,
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

/// ssh takes an IPv6 host bare and its port from `-p`; scp still needs the brackets (#3437).
#[test]
fn a_bracketed_ipv6_destination_reaches_ssh_without_its_brackets() {
    for (written, port) in [
        ("root@[2001:db8::1]:2222", Some(2222)),
        ("root@[2001:db8::1]", None),
        ("root@2001:db8::1", None),
    ] {
        let destination = SshDestination::parse(written).unwrap();
        assert_eq!(destination.target(), "root@2001:db8::1", "{written}");
        assert_eq!(destination.copy_target(), "root@[2001:db8::1]", "{written}");
        assert_eq!(destination.port(), port, "{written}");
        assert_eq!(destination.as_str(), written, "{written}");
    }
    for refused in [
        "root@[2001:db8::1]:0",
        "root@[example.com]:22",
        "root@[2001:db8::1]x",
    ] {
        assert!(SshDestination::parse(refused).is_err(), "{refused}");
    }
}

#[test]
fn system_ssh_command_uses_noninteractive_authentication() {
    let destination = SshDestination::parse("deploy@example.com:2222").unwrap();

    let args = ssh_args(
        &destination,
        Some(Path::new("/keys/deploy")),
        None,
        SshMaster::Share,
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
        (tonic::Code::PermissionDenied,   false, false, false, RpcErrorCode::from("forbidden")),
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
        "Ployz's call to your Server's daemon failed: invalid log time \"notatime\""
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
        (ConnectError::OlderMachine, Retry::Never),
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
fn only_a_status_the_router_sent_says_the_target_was_never_reached() {
    let reset = || io::Error::new(io::ErrorKind::ConnectionReset, "stream reset");
    let mut marked_then_reset = ployz_core::rpc::target_unreached_status(&reset());
    marked_then_reset.set_source(std::sync::Arc::new(reset()));
    let lost = crate::sdk::CallError::from(ConnectError::from(marked_then_reset));
    assert!(lost.unanswered, "{lost}");
    assert!(!lost.target_unreached, "{lost}");

    let undialed = crate::sdk::CallError::from(ConnectError::from(
        ployz_core::rpc::target_unreached_status(&io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "tcp connect error",
        )),
    ));
    assert!(!undialed.unanswered, "{undialed}");
    assert!(undialed.target_unreached, "{undialed}");
}

#[test]
fn only_a_router_status_on_a_working_channel_says_the_entry_answered() {
    let silent = || io::Error::new(io::ErrorKind::TimedOut, "deadline has elapsed");
    let routed = ConnectError::from(ployz_core::rpc::target_unanswered_status(&silent()));
    assert!(routed.routed_by_entry());
    let undialed = ConnectError::from(ployz_core::rpc::target_unreached_status(&silent()));
    assert!(undialed.routed_by_entry());

    let mut marked_then_reset = ployz_core::rpc::target_unanswered_status(&silent());
    marked_then_reset.set_source(std::sync::Arc::new(io::Error::new(
        io::ErrorKind::ConnectionReset,
        "stream reset",
    )));
    assert!(!ConnectError::from(marked_then_reset).routed_by_entry());
    assert!(!ConnectError::from(tonic::Status::unavailable("restarting")).routed_by_entry());
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

#[test]
fn only_a_call_that_got_no_status_back_is_unreplied() {
    let dropped = io::Error::new(io::ErrorKind::BrokenPipe, "connection closed");
    for (error, unreplied) in [
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
        (
            ConnectError::from(tonic::Status::from_error(Box::new(dropped))),
            true,
        ),
        // A daemon sent these, so it refused before acting.
        (
            ConnectError::from(tonic::Status::unavailable("Cluster is not available")),
            false,
        ),
        (
            ConnectError::Remote(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Cluster store is not available".into(),
                details: Value::Null,
                cause: Vec::new(),
            }),
            false,
        ),
    ] {
        assert_eq!(error.unreplied(), unreplied, "{error}");
    }
}

async fn poll_os<T>(mut ready: impl FnMut() -> Option<T>) -> T {
    loop {
        if let Some(value) = ready() {
            return value;
        }
        // real-wait: polls the OS for a fake SSH child's pid file or a process's exit; neither emits an event a test can await
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn exited(pid: u32) {
    poll_os(|| {
        let alive = StdCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        (!alive).then_some(())
    })
    .await;
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
            pid = poll_os(|| {
                fs::read_to_string(program.with_extension("pid"))
                    .ok()
                    .and_then(|contents| contents.trim().parse::<u32>().ok())
            }) => pid,
        }
    })
    .await
    .expect("fake SSH did not publish its PID");
    tokio::time::timeout(Duration::from_secs(1), exited(pid))
        .await
        .expect("ssh was not killed");

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_ssh_dial_beside_another_starts_no_master_to_outlive_it() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("ssh");
    symlink(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ssh"),
        &program,
    )
    .unwrap();
    let connector = SystemConnector::new(&program);
    let connection = Connection::ssh(SshDestination::parse("user@record").unwrap());
    // Without a control directory no dial multiplexes, so none starts a master.
    let multiplexed = control_path().is_some();
    for beside in [false, true] {
        let _ = fs::remove_file(program.with_extension("args"));
        // The session child is held open until the channel drops, so it has
        // time to record its arguments.
        let _channel = if beside {
            connector.connect_beside(&connection).await
        } else {
            connector.connect(&connection).await
        };
        let invocations = tokio::time::timeout(
            Duration::from_secs(5),
            poll_os(|| {
                let recorded = fs::read_to_string(program.with_extension("args")).unwrap();
                (recorded.lines().count() == 2).then_some(recorded)
            }),
        )
        .await
        .expect("the probe and the session both ran");
        for invocation in invocations.lines() {
            assert_eq!(
                invocation.contains("ControlMaster=auto"),
                multiplexed && !beside,
                "{invocation}"
            );
            assert_eq!(
                invocation.contains("ControlMaster=no"),
                multiplexed && beside,
                "{invocation}"
            );
        }
    }
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

/// A daemon on `socket` that refuses discovery with `status`.
async fn refusing_daemon(socket: &std::path::Path, status: fn() -> tonic::Status) {
    use ployz_core::{MachineRpcServer, OpaquePayload};
    use ployz_testkit::fake_machine::{Fake, FakeMachine};

    struct Daemon(fn() -> tonic::Status);

    #[tonic::async_trait]
    impl FakeMachine for Daemon {
        async fn describe_contract(
            &self,
            _request: tonic::Request<OpaquePayload>,
        ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
            Err((self.0)())
        }
    }

    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(MachineRpcServer::new(Fake(Daemon(status))))
            .serve_with_incoming(tokio_stream::wrappers::UnixListenerStream::new(listener)),
    );
}

#[tokio::test]
async fn missing_ssh_client_keeps_an_earlier_daemons_answer() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("refusing.sock");
    refusing_daemon(&socket, || tonic::Status::unauthenticated("not authorized")).await;
    let error = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![
                Connection::unix(&socket).unwrap(),
                Connection::ssh(SshDestination::parse("user@example.com").unwrap()),
            ],
        },
        Arc::new(SystemConnector::new("/ployz-missing-ssh-client")),
    )
    .await
    .err()
    .expect("a refusal and a missing ssh client must fail");
    assert!(
        matches!(
            error,
            ConnectError::AllFailed {
                attempts: 2,
                reached: true,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.unreached(), "{error:?}");
}

#[tokio::test]
async fn several_refusing_daemons_name_the_server_tried_last() {
    let dir = tempfile::tempdir().unwrap();
    let sockets = ["first.sock", "second.sock"].map(|name| dir.path().join(name));
    for socket in &sockets {
        refusing_daemon(socket, || tonic::Status::unauthenticated("not authorized")).await;
    }
    let ids = ["a", "b"].map(|digit| MachineId::parse(digit.repeat(32)).unwrap());
    let [_, last] = ids;
    let refused = |names: [Option<&'static str>; 2]| {
        let connections = sockets
            .iter()
            .zip(ids)
            .zip(names)
            .map(|((socket, id), name)| {
                let connection = Connection::unix(socket).unwrap().with_machine_id(id);
                match name {
                    Some(name) => connection.with_name(name),
                    None => connection,
                }
            })
            .collect();
        async move {
            connect_selected_with(
                SelectedConnections {
                    source: ConnectionSource::Cloud,
                    connections,
                },
                Arc::new(SystemConnector::default()),
            )
            .await
            .err()
            .expect("every daemon refuses")
            .to_string()
        }
    };

    assert_eq!(
        refused([Some("web-1"), Some("web-2")]).await,
        "A Server's daemon answered, but the 2 connections from Cloud all failed. \
         Server web-2, tried last, refused the connection."
    );
    // Cloud never learned their names: the Machine ID says which.
    assert_eq!(
        refused([None, None]).await,
        format!(
            "A Server's daemon answered, but the 2 connections from Cloud all failed. \
             Server {}, tried last, refused the connection.",
            last
        )
    );
    // A connection that says no Server, as `--connect` gives, names none.
    let error = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: sockets
                .iter()
                .map(|socket| Connection::unix(socket).unwrap())
                .collect(),
        },
        Arc::new(SystemConnector::default()),
    )
    .await
    .err()
    .expect("every daemon refuses");
    assert_eq!(
        error.to_string(),
        "A Server's daemon answered, but the 2 connections from the given connection all failed."
    );
}

#[tokio::test]
async fn a_cancelled_status_a_daemon_sent_is_an_answer() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("cancelling.sock");
    refusing_daemon(&socket, || tonic::Status::cancelled("shutting down")).await;
    let error = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::unix(&socket).unwrap()],
        },
        Arc::new(SystemConnector::new("/ployz-missing-ssh-client")),
    )
    .await
    .err()
    .expect("a cancelled discovery must fail");
    assert!(!error.unreached(), "{error:?}");
    assert_eq!(error.retry(), Retry::AfterRestart, "{error:?}");
    let mut local = tonic::Status::cancelled("connection closed");
    local.set_source(Arc::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "connection closed",
    )));
    assert!(ConnectError::Rpc(TransportError::from(local)).unreached());
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
                SshMaster::Share,
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
    tokio::time::timeout(Duration::from_secs(3), exited(pid))
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

enum Scripted {
    Confirms,
    ConfirmsAfter(Box<dyn Fn() -> tokio::time::Sleep + Send + Sync>),
    Silent,
    Fails,
}

#[derive(Debug, PartialEq, Eq)]
struct Dialled {
    index: usize,
    at: Duration,
    beside: bool,
}

/// Dials each entry as its script says, recording every dial and every dial
/// dropped before it finished.
struct ScriptedConnector {
    entries: Vec<(Connection, Scripted)>,
    epoch: tokio::time::Instant,
    dials: std::sync::Mutex<Vec<Dialled>>,
    abandoned: std::sync::Mutex<Vec<usize>>,
}

struct Unfinished<'a>(&'a std::sync::Mutex<Vec<usize>>, usize);

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(self.1);
    }
}

impl ScriptedConnector {
    fn new(entries: Vec<(Connection, Scripted)>) -> Arc<Self> {
        Arc::new(Self {
            entries,
            epoch: tokio::time::Instant::now(),
            dials: Default::default(),
            abandoned: Default::default(),
        })
    }

    fn selected(&self) -> SelectedConnections {
        SelectedConnections {
            source: ConnectionSource::Cloud,
            connections: self
                .entries
                .iter()
                .map(|(entry, _)| entry.clone())
                .collect(),
        }
    }

    async fn dial(&self, connection: &Connection, beside: bool) -> Result<Channel, ConnectError> {
        let (index, (_, script)) = self
            .entries
            .iter()
            .enumerate()
            .find(|(_, (entry, _))| entry == connection)
            .unwrap();
        self.dials.lock().unwrap().push(Dialled {
            index,
            at: self.epoch.elapsed(),
            beside,
        });
        let unfinished = Unfinished(&self.abandoned, index);
        let dialled = match script {
            Scripted::Confirms => Ok(test_support::confirming_channel().await),
            Scripted::ConfirmsAfter(delay) => {
                delay().await;
                Ok(test_support::confirming_channel().await)
            }
            Scripted::Silent => std::future::pending().await,
            Scripted::Fails => Err(ConnectError::Attempt("scripted failure".into())),
        };
        std::mem::forget(unfinished);
        dialled
    }
}

#[tonic::async_trait]
impl Connector for ScriptedConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.dial(connection, false).await
    }

    async fn connect_beside(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.dial(connection, true).await
    }

    async fn dial_proxy(
        &self,
        _: &Connection,
        _: &str,
        _: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        unreachable!("connect opens no proxy")
    }
}

/// One entry of each transport, with the stagger it is given before the next
/// entry is dialled beside it.
fn staggered_entries() -> [(Connection, Duration); 3] {
    let capability = ployz_core::ManagementCapability::new(
        ployz_core::ManagementIdentity::from_bytes([1; 32]),
        [2; 32],
    );
    [
        (
            Connection::management(capability.to_secret_string()).unwrap(),
            Duration::from_secs(1),
        ),
        (
            Connection::unix("/scripted/entry.sock").unwrap(),
            Duration::from_secs(1),
        ),
        (
            Connection::ssh(SshDestination::parse("user@scripted").unwrap()),
            Duration::from_secs(5),
        ),
    ]
}

fn answering_entry() -> Connection {
    Connection::unix("/scripted/answering.sock").unwrap()
}

#[tokio::test(start_paused = true)]
async fn an_entry_that_confirms_within_its_stagger_is_never_raced() {
    for (first, stagger) in staggered_entries() {
        let connector = ScriptedConnector::new(vec![
            (
                first.clone(),
                Scripted::ConfirmsAfter(Box::new(move || {
                    tokio::time::sleep(stagger - Duration::from_millis(100))
                })),
            ),
            (answering_entry(), Scripted::Confirms),
        ]);
        let client = connect_selected_with(connector.selected(), connector.clone())
            .await
            .unwrap();
        assert_eq!(client.connection(), &first);
        assert_eq!(
            *connector.dials.lock().unwrap(),
            [Dialled {
                index: 0,
                at: Duration::ZERO,
                beside: false
            }],
            "{first}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_silent_entry_is_raced_after_its_stagger_and_dropped_once_another_confirms() {
    for (first, stagger) in staggered_entries() {
        let connector = ScriptedConnector::new(vec![
            (first.clone(), Scripted::Silent),
            (answering_entry(), Scripted::Confirms),
        ]);
        let client = connect_selected_with(connector.selected(), connector.clone())
            .await
            .unwrap();
        assert_eq!(client.connection(), &answering_entry());
        assert_eq!(
            *connector.dials.lock().unwrap(),
            [
                Dialled {
                    index: 0,
                    at: Duration::ZERO,
                    beside: false
                },
                Dialled {
                    index: 1,
                    at: stagger,
                    beside: true
                },
            ],
            "{first}"
        );
        assert_eq!(*connector.abandoned.lock().unwrap(), [0], "{first}");
    }
}

#[tokio::test(start_paused = true)]
async fn in_order_a_slow_entry_is_never_raced() {
    let connector = ScriptedConnector::new(vec![
        (
            Connection::unix("/scripted/0.sock").unwrap(),
            Scripted::ConfirmsAfter(Box::new(|| tokio::time::sleep(Duration::from_secs(4)))),
        ),
        (answering_entry(), Scripted::Confirms),
    ]);
    let client = connect_selected_in_order(connector.selected(), connector.clone())
        .await
        .unwrap();
    assert_eq!(
        client.connection(),
        &Connection::unix("/scripted/0.sock").unwrap()
    );
    assert_eq!(
        *connector.dials.lock().unwrap(),
        [Dialled {
            index: 0,
            at: Duration::ZERO,
            beside: false
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_entry_hands_over_at_once_and_only_dials_beside_a_live_one() {
    let connector = ScriptedConnector::new(vec![
        (
            Connection::unix("/scripted/0.sock").unwrap(),
            Scripted::Fails,
        ),
        (
            Connection::unix("/scripted/1.sock").unwrap(),
            Scripted::Silent,
        ),
        (
            Connection::unix("/scripted/2.sock").unwrap(),
            Scripted::Fails,
        ),
        (
            answering_entry(),
            Scripted::ConfirmsAfter(Box::new(|| tokio::time::sleep(Duration::from_secs(3)))),
        ),
    ]);
    let client = connect_selected_with(connector.selected(), connector.clone())
        .await
        .unwrap();
    assert_eq!(client.connection(), &answering_entry());
    let second = Duration::from_secs(1);
    assert_eq!(
        *connector.dials.lock().unwrap(),
        [
            Dialled {
                index: 0,
                at: Duration::ZERO,
                beside: false
            },
            Dialled {
                index: 1,
                at: Duration::ZERO,
                beside: false
            },
            Dialled {
                index: 2,
                at: second,
                beside: true
            },
            Dialled {
                index: 3,
                at: second,
                beside: true
            },
        ]
    );
    assert_eq!(*connector.abandoned.lock().unwrap(), [1]);
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

#[test]
fn a_call_that_lost_its_answer_outlasts_a_retry_that_never_reached_the_target() {
    let lost = || {
        ConnectError::from(tonic::Status::from_error(Box::new(io::Error::new(
            io::ErrorKind::NotConnected,
            "connection lost",
        ))))
    };
    let unreached = || ConnectError::Io(io::Error::new(io::ErrorKind::TimedOut, "SSH timed out"));

    let error = keep_lost_call(Some(lost()), unreached());
    assert!(error.unreplied(), "{error}");

    let answered = keep_lost_call(
        Some(lost()),
        ConnectError::from(tonic::Status::unavailable("Machine is not participating")),
    );
    assert!(
        answered.target_replied(),
        "a later answer stands: {answered}"
    );
    assert!(!keep_lost_call(None, unreached()).unreplied());

    let refused_key = {
        use std::os::unix::process::ExitStatusExt;
        ConnectError::SshProbe {
            target: "host".to_owned(),
            status: std::process::ExitStatus::from_raw(255 << 8),
            failure: SshFailure::Auth,
            detail: "Permission denied (publickey)".into(),
        }
    };
    let kept = keep_lost_call(Some(lost()), refused_key);
    assert_eq!(kept.retry(), ployz_core::Retry::Never, "{kept}");
}

#[test]
fn a_closed_session_does_not_outlast_a_later_silence() {
    let closed = || {
        ConnectError::Rpc(TransportError {
            session_closed: true,
            ..TransportError::from(tonic::Status::unavailable("transport error"))
        })
    };
    let budget_spent = || {
        ConnectError::Rpc(TransportError::budget_expired(
            "target Machine RPC timed out",
        ))
    };
    let routed_silence = || {
        ConnectError::from(ployz_core::rpc::target_unanswered_status(&io::Error::from(
            io::ErrorKind::TimedOut,
        )))
    };
    let machine = MachineId::parse("b".repeat(32)).unwrap();
    for silence in [budget_spent(), routed_silence()] {
        // As `call_retried`: the closed session first, then the redialled target is silent.
        let mut reply = None;
        let error = keep_target_reply(&mut reply, closed());
        assert!(!error.target_replied(), "no daemon sent it");
        assert!(
            !error.unreplied(),
            "it never left, so it cannot have landed"
        );
        if error.target_replied() {
            reply = Some(error);
        }
        let kept = keep_target_reply(&mut reply, silence);
        assert!(kept.target_unanswered(), "{kept}");
        let error = crate::cluster::answered_by(&machine, kept);
        assert!(!on_closed_session(&error), "{error:?}");
        assert_eq!(
            error.code.retry(),
            Retry::Now,
            "the Deploy waits out the target"
        );
    }
}

#[test]
fn a_named_context_without_a_config_is_not_found_rather_than_unconfigured() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let socket = dir.path().join("ployzd.sock");
    fs::write(&socket, "").unwrap();
    let error = resolve_connections(&config, None, Some("foo"), &socket).unwrap_err();
    assert!(
        matches!(
            &error,
            ConnectError::Context(ContextError::ContextNotFound { name, path })
                if name == "foo" && path == &config
        ),
        "{error:?}"
    );
    let selected = resolve_connections(&config, None, None, &socket).unwrap();
    assert_eq!(selected.source, ConnectionSource::LocalSocket);
}

/// Reads the client's HTTP/2 stream until a request's HEADERS frame arrives.
async fn request_headers(daemon: &mut (impl tokio::io::AsyncRead + Unpin)) {
    use tokio::io::AsyncReadExt as _;
    let mut preface = [0; 24];
    daemon.read_exact(&mut preface).await.unwrap();
    loop {
        let mut header = [0; 9];
        daemon.read_exact(&mut header).await.unwrap();
        let length = u32::from_be_bytes([0, header[0], header[1], header[2]]);
        let mut payload = vec![0; length as usize];
        daemon.read_exact(&mut payload).await.unwrap();
        if header[3] == 0x1 {
            return;
        }
    }
}

#[tokio::test]
async fn a_call_after_the_stream_closed_is_marked_and_stays_unavailable() {
    let (client, mut daemon) = tokio::io::duplex(1024);
    let channel = connect_stream(client, Duration::from_secs(5))
        .await
        .unwrap();
    let mut rpc = ployz_core::MachineRpcClient::new(channel);
    let describe = || {
        ployz_core::op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
            .encode()
            .unwrap()
    };
    let mut in_flight = rpc.clone();
    let in_flight = tokio::spawn(async move { in_flight.describe_contract(describe()).await });
    request_headers(&mut daemon).await;
    drop(daemon);
    let lost = TransportError::from(in_flight.await.unwrap().unwrap_err());
    assert!(!lost.session_closed, "{lost:?}");
    assert!(
        lost.unanswered,
        "the first call may have reached the daemon"
    );

    let closed = TransportError::from(rpc.describe_contract(describe()).await.unwrap_err());
    assert!(closed.session_closed, "{closed:?}");
    assert!(!closed.unanswered, "a call on a closed session never left");
    let connect = ConnectError::Rpc(closed);
    assert_eq!(connect.retry(), Retry::Now, "a redial opens a new stream");
    assert!(!connect.target_replied(), "no daemon sent it");
    let error = rpc_error(connect);
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert!(on_closed_session(&error), "{error:?}");
    assert_eq!(error.details, closed_session_error().details);
    assert!(
        crate::ui::chain_text(&error).contains("connection to the daemon closed"),
        "{error:?}"
    );
}

#[test]
fn a_broken_pipe_is_not_a_closed_session() {
    let status = tonic::Status::from_error(Box::new(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "stream closed because of a broken pipe",
    )));
    let error = TransportError::from(status);
    assert!(!error.session_closed, "{error:?}");
    assert!(error.unanswered);
    assert!(!on_closed_session(&error.to_rpc_error()));
}

#[test]
fn a_restarting_target_behind_a_live_entry_is_still_resent() {
    let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
    for status in [
        ployz_core::rpc::target_unanswered_status(&refused),
        ployz_core::rpc::target_unreached_status(&refused),
        tonic::Status::unavailable("daemon is restarting"),
    ] {
        let error = rpc_error(ConnectError::from(status));
        assert_eq!(error.code.retry(), Retry::Now, "{error:?}");
    }
}

/// Holds one call open on `client` for up to an hour: how long it took to fail, or
/// `None` if it was still open.
async fn held_call(client: tokio::io::DuplexStream) -> Option<Duration> {
    let channel = connect_stream(client, Duration::from_secs(5))
        .await
        .unwrap();
    let mut rpc = ployz_core::MachineRpcClient::new(channel);
    let request =
        ployz_core::op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
            .encode()
            .unwrap();
    let started = tokio::time::Instant::now();
    let held = tokio::time::timeout(Duration::from_secs(3600), rpc.describe_contract(request));
    held.await.ok().map(|reply| {
        assert!(reply.is_err(), "a held call cannot succeed");
        started.elapsed()
    })
}

#[tokio::test(start_paused = true)]
async fn a_call_to_a_daemon_that_went_silent_fails_within_its_keepalive_bound() {
    let (client, _silent_daemon) = tokio::io::duplex(64 * 1024);
    let failed_after = held_call(client)
        .await
        .expect("a silent daemon held the call for an hour");
    assert!(
        (KEEP_ALIVE_TIMEOUT..=KEEP_ALIVE_INTERVAL + KEEP_ALIVE_TIMEOUT).contains(&failed_after),
        "failed after {failed_after:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_resend_after_a_keepalive_timeout_ends_on_the_closed_session() {
    let (client, _silent_daemon) = tokio::io::duplex(64 * 1024);
    let channel = connect_stream(client, Duration::from_secs(5))
        .await
        .unwrap();
    let mut rpc = ployz_core::MachineRpcClient::new(channel);
    let describe = || {
        ployz_core::op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
            .encode()
            .unwrap()
    };
    let timed_out = rpc_error(ConnectError::from(
        rpc.describe_contract(describe()).await.unwrap_err(),
    ));
    assert!(
        crate::ui::chain_text(&timed_out).contains("keep-alive timed out"),
        "{timed_out:?}"
    );
    let resent = rpc_error(ConnectError::from(
        rpc.describe_contract(describe()).await.unwrap_err(),
    ));
    assert!(on_closed_session(&resent), "{resent:?}");
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_answers_pings_may_hold_a_call_for_an_hour() {
    let (client, daemon) = tokio::io::duplex(64 * 1024);
    tokio::spawn(
        hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .serve_connection(
                TokioIo::new(daemon),
                hyper::service::service_fn(|_| {
                    std::future::pending::<Result<http::Response<String>, io::Error>>()
                }),
            ),
    );
    assert_eq!(held_call(client).await, None);
}

/// Moves bytes from `from` to `to` at `bytes_per_second`, as a slow link drains the
/// buffers in front of it.
async fn drain_slowly(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    bytes_per_second: usize,
) -> io::Result<()> {
    use tokio::io::AsyncReadExt;
    let per_tick = bytes_per_second / 10;
    loop {
        let mut chunk = vec![0; per_tick];
        let read = from.read(&mut chunk).await?;
        if read == 0 {
            return to.shutdown().await;
        }
        chunk.truncate(read);
        to.write_all(&chunk).await?;
        let share = u32::try_from(read * 1000 / per_tick).expect("a read fits its buffer");
        tokio::time::sleep(Duration::from_micros(100) * share).await;
    }
}

/// A client end and a daemon end joined by 4 MiB of buffering each way, as ssh and TCP
/// buffer, with one direction slowed to `bytes_per_second`.
fn slow_link(
    slow: Direction,
    bytes_per_second: usize,
) -> (tokio::io::DuplexStream, tokio::io::DuplexStream) {
    let (client, client_far) = tokio::io::duplex(4 * 1024 * 1024);
    let (daemon, daemon_far) = tokio::io::duplex(4 * 1024 * 1024);
    let (mut uplink, mut downlink) = tokio::io::split(client_far);
    let (mut from_daemon, mut to_daemon) = tokio::io::split(daemon_far);
    match slow {
        Direction::Up => {
            tokio::spawn(drain_slowly(uplink, to_daemon, bytes_per_second));
            tokio::spawn(async move { tokio::io::copy(&mut from_daemon, &mut downlink).await });
        }
        Direction::Down => {
            tokio::spawn(async move { tokio::io::copy(&mut uplink, &mut to_daemon).await });
            tokio::spawn(drain_slowly(from_daemon, downlink, bytes_per_second));
        }
    }
    (client, daemon)
}

enum Direction {
    Up,
    Down,
}

/// Serves `daemon` with the window the daemon advertises.
fn serve<F, R>(daemon: tokio::io::DuplexStream, handle: F)
where
    F: Fn(http::Request<hyper::body::Incoming>) -> R + Send + 'static,
    R: Future<Output = Result<http::Response<http_body_util::Full<bytes::Bytes>>, io::Error>>
        + Send
        + 'static,
{
    tokio::spawn(
        hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
            .initial_connection_window_size(ployz_core::RPC_CONNECTION_WINDOW_BYTES)
            .initial_stream_window_size(ployz_core::RPC_STREAM_WINDOW_BYTES)
            .serve_connection(TokioIo::new(daemon), hyper::service::service_fn(handle)),
    );
}

/// Sends `body` on a channel held for the whole call, as the CLI holds its channel:
/// once every handle is gone, hyper stops pinging and a test would prove nothing.
async fn call(
    client: tokio::io::DuplexStream,
    body: Vec<u8>,
) -> (Channel, http::Response<tonic::body::Body>) {
    let mut channel = connect_stream(client, Duration::from_secs(5))
        .await
        .unwrap();
    let response = send(&mut channel, "/call", body).await;
    (channel, response)
}

/// Sends `body` to `path` on an open channel.
async fn send(
    channel: &mut Channel,
    path: &str,
    body: Vec<u8>,
) -> http::Response<tonic::body::Body> {
    use http_body_util::BodyExt;
    let body = http_body_util::Full::new(bytes::Bytes::from(body)).map_err(|never| match never {});
    let request = http::Request::post(format!("http://[::]:50051{path}"))
        .body(tonic::body::Body::new(body))
        .unwrap();
    let ready = tower::ServiceExt::ready(channel).await.unwrap();
    tower::Service::call(ready, request).await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn an_upload_over_a_20_kb_per_second_uplink_finishes() {
    use http_body_util::BodyExt;
    const UPLOAD: usize = 2 * 1024 * 1024;
    let (client, daemon) = slow_link(Direction::Up, 20 * 1024);
    serve(daemon, |request| async {
        let received = request.into_body().collect().await.unwrap().to_bytes();
        Ok(http::Response::new(received.len().to_string().into()))
    });
    let (_channel, response) = call(client, vec![0; UPLOAD]).await;
    let received = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(received, UPLOAD.to_string());
}

#[tokio::test(start_paused = true)]
async fn a_download_left_unread_on_a_20_kb_per_second_downlink_finishes() {
    use http_body_util::BodyExt;
    const DOWNLOAD: usize = 3 * 1024 * 1024;
    let (client, daemon) = slow_link(Direction::Down, 20 * 1024);
    serve(daemon, |_| async {
        Ok(http::Response::new(vec![0; DOWNLOAD].into()))
    });
    let (_channel, response) = call(client, Vec::new()).await;
    tokio::time::sleep(Duration::from_secs(120)).await;
    let received = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(received.len(), DOWNLOAD);
}

#[tokio::test(start_paused = true)]
async fn a_log_stream_left_unread_leaves_room_for_the_next_call() {
    use http_body_util::BodyExt;
    let (client, daemon) = tokio::io::duplex(16 * 1024 * 1024);
    serve(daemon, |request| async move {
        let size = if request.uri().path() == "/logs" {
            8 * 1024 * 1024
        } else {
            1
        };
        Ok(http::Response::new(vec![0; size].into()))
    });
    let mut channel = connect_stream(client, Duration::from_secs(5))
        .await
        .unwrap();
    let _unread_logs = send(&mut channel, "/logs", Vec::new()).await;
    let grant = tokio::time::timeout(Duration::from_secs(10), async {
        send(&mut channel, "/grant", Vec::new())
            .await
            .into_body()
            .collect()
            .await
    })
    .await;
    assert!(
        matches!(grant, Ok(Ok(_))),
        "the unread log stream starved the next call: {grant:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_server_unanswered_through_cloud_may_be_offline_or_filtered_not_only_stopped() {
    let [(relayed, _), (socket, _), _] = staggered_entries();
    for (entry, said) in [
        (
            relayed,
            "Server machine-1 isn't answering through Ployz Cloud. Check that it is online, its \
             Ployz daemon is running, and its outgoing https isn't filtered, then retry.",
        ),
        (
            socket,
            "Server machine-1 isn't answering. Check that its Ployz daemon is running, then retry.",
        ),
    ] {
        let connector =
            ScriptedConnector::new(vec![(entry.with_name("machine-1"), Scripted::Fails)]);
        let Err(error) = connect_selected_with(connector.selected(), connector.clone()).await
        else {
            panic!("a failing entry connected");
        };
        assert_eq!(error.to_string(), said);
    }
}
