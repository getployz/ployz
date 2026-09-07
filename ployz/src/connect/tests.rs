use std::{fs, os::unix::fs::PermissionsExt, process::Command as StdCommand, time::Duration};

use super::*;
use crate::context::SshDestination;
use ployz_core::{ONE_TARGET_BINARY_HEADER, ONE_TARGET_HEADER};

#[test]
fn setup_retry_classifies_ssh_and_preserves_aggregate_cause() {
    use std::os::unix::process::ExitStatusExt;
    for (detail, retry) in [
        ("Connection timed out", true),
        ("Connection refused", true),
        ("Permission denied (publickey)", false),
        ("Host key verification failed", false),
    ] {
        let ssh_error = ConnectError::SshProbe {
            target: "host".to_owned(),
            status: std::process::ExitStatus::from_raw(255 << 8),
            detail: detail.to_owned(),
        };
        assert_eq!(ssh_error.is_setup_retryable(), retry);
        let error = ConnectError::AllFailed {
            source: ConnectionSource::Direct,
            attempts: 1,
            setup_retryable: retry,
            last: Some(Box::new(ssh_error)),
        };
        assert_eq!(error.is_setup_retryable(), retry);
        assert!(error.to_string().contains(detail));
    }
}

#[tokio::test]
async fn setup_retry_preserves_transient_failures_in_either_connection_order() {
    let root = std::env::temp_dir().join(format!("ployz-ssh-retry-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let program = root.join("ssh");
    fs::write(&program, "#!/bin/sh\ncase \"$*\" in *transient*) echo 'Connection refused' >&2;; *) echo 'Permission denied (publickey)' >&2;; esac\nexit 255\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
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
            match connect_selected_with(selected, Arc::new(SystemConnector::new(&program))).await {
                Err(error) => error,
                Ok(_) => panic!("script must reject every connection"),
            };
        assert_eq!(error.is_setup_retryable(), retryable, "{error}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn target_timeout_becomes_a_typed_partial_failure() {
    let error = apply_timeout(
        Some(Duration::from_millis(1)),
        std::future::pending::<Result<(), ConnectError>>(),
    )
    .await
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
fn system_ssh_command_delegates_identity_and_passphrase_handling() {
    let destination = SshDestination::parse("deploy@example.com:2222").unwrap();

    let args = ssh_args(&destination, Some(Path::new("/keys/deploy")), None);

    assert_eq!(
        args,
        [
            "-o",
            "ConnectTimeout=5",
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
    assert!(!args.iter().any(|arg| arg.contains("BatchMode")));
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
        assert_eq!(error.is_retryable(), retryable, "{code:?}");
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
        ConnectError::Rpc(error).to_string(),
        "Machine RPC failed: invalid log time \"notatime\""
    );
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
        })
        .is_unreachable()
    );
}

#[test]
fn deadline_exceeded_is_retryable_not_unreachable() {
    let error = ConnectError::Rpc(TransportError::from(tonic::Status::deadline_exceeded(
        "timed out",
    )));
    assert!(error.is_retryable());
    assert!(!error.is_unreachable());
}

#[tokio::test]
async fn cancelling_ssh_establishment_returns_without_waiting_for_ssh() {
    let root = std::env::temp_dir().join(format!("ployz-ssh-cancel-{}", std::process::id()));
    let program = root.join("ssh");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(&program, "#!/bin/sh\necho $$ > \"$0.pid\"\nexec sleep 30\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let connector = SystemConnector::new(&program);
    let connection = Connection::ssh(SshDestination::parse("user@example.com").unwrap());

    let result =
        tokio::time::timeout(Duration::from_millis(50), connector.connect(&connection)).await;
    assert!(result.is_err(), "{result:?}");
    let pid = fs::read_to_string(program.with_extension("pid")).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while StdCommand::new("kill")
        .args(["-0", pid.trim()])
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
        matches!(message, ConnectError::SshClientMissing(_)),
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
    assert!(
        matches!(error, ConnectError::SshClientMissing(_)),
        "{error:?}"
    );
    let failure = crate::failure::Failure::from(error);
    assert_eq!(
        failure.to_string(),
        "local ssh client not found; install an ssh client"
    );
    assert!(!failure.to_string().contains("os error"), "{failure}");
}
