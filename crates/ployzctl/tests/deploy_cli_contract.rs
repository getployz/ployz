use std::process::{Command, Output};

use ployz_core::deploy::{DeployRoute, ImageReference, ReplicaCount};
use ployz_core::ids::{OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    EventSequence, OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease,
    RouteHostname, RoutePort, RouteTarget,
};
use ployz_sdk_types::AcceptedOperation;
use ployzctl::commands::deploy::{DetachedDeployCommand, DetachedDeployOutput};
use ployzctl::commands::{PloyzctlCliError, PloyzctlCommand, parse_command};

#[test]
fn cli_dispatches_detached_deploy_request() {
    let command = parse_command(detached_deploy_args()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(command, detached_deploy_command());
}

#[test]
fn cli_dispatches_detached_deploy_request_with_route() {
    let command = parse_command(detached_deploy_args_with_route()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(
        command.route,
        Some(DeployRoute {
            target: RouteTarget {
                hostname: RouteHostname::try_new("api.example.com").expect("valid route hostname"),
                port: RoutePort::try_new(443).expect("valid route port"),
            },
            endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
        })
    );
}

#[test]
fn cli_requires_route_port_when_deploy_route_hostname_is_set() {
    let args = detached_deploy_arg_refs()
        .chain(["--route-hostname", "api.example.com"])
        .map(str::to_owned);

    assert!(matches!(
        parse_command(args),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--route-port"
    ));
}

#[test]
fn cli_requires_endpoint_port_when_deploy_route_is_set() {
    let args = detached_deploy_arg_refs()
        .chain(["--route-hostname", "api.example.com", "--route-port", "443"])
        .map(str::to_owned);

    assert!(matches!(
        parse_command(args),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--endpoint-port"
    ));
}

#[test]
fn cli_requires_detached_deploy_mode() {
    let args = [
        "deploy",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--operation",
        "op_deploy",
        "--idempotency-key",
        "idem_deploy",
    ]
    .map(str::to_owned);

    assert!(matches!(
        parse_command(args),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--detach"
    ));
}

#[test]
fn cli_requires_deploy_idempotency_key() {
    let args = [
        "deploy",
        "--detach",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--operation",
        "op_deploy",
    ]
    .map(str::to_owned);

    assert!(matches!(
        parse_command(args),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--idempotency-key"
    ));
}

#[test]
fn binary_deploy_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(detached_deploy_arg_refs())
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "--nats or PLOYZ_NATS_URL is required\n");
}

#[test]
fn deploy_detach_prints_operation_id_without_runtime_details() {
    let output = DetachedDeployOutput {
        accepted: accepted_operation("op_deploy"),
    }
    .render();

    assert_eq!(
        output,
        "operation op_deploy\nwatch ployzctl ops watch op_deploy\n"
    );
}

fn detached_deploy_command() -> DetachedDeployCommand {
    DetachedDeployCommand {
        operation_id: operation_id("op_deploy"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_deploy")
            .expect("valid idempotency key"),
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
        revision_id: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replicas"),
        route: None,
    }
}

fn detached_deploy_args() -> impl Iterator<Item = String> {
    detached_deploy_arg_refs().map(str::to_owned)
}

fn detached_deploy_args_with_route() -> impl Iterator<Item = String> {
    detached_deploy_arg_refs()
        .chain([
            "--route-hostname",
            "api.example.com",
            "--route-port",
            "443",
            "--endpoint-port",
            "8080",
        ])
        .map(str::to_owned)
}

fn detached_deploy_arg_refs() -> impl Iterator<Item = &'static str> {
    [
        "deploy",
        "--detach",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
        "--operation",
        "op_deploy",
        "--idempotency-key",
        "idem_deploy",
    ]
    .into_iter()
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
        owner_lease: OperationOwnerLease::new(
            self::operation_id(operation_id),
            OperationOwnerId::try_new("control").expect("valid owner id"),
            OperationLeaseExpiresAt::try_new(120).expect("valid lease expiry"),
        ),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
