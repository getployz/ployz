use std::process::{Command, Output};

use ployz_core::deploy::{DeployRoute, DeployServiceSpec, ImageReference, ReplicaCount};
use ployz_core::ids::{NamespaceId, NamespaceRevisionId, ServiceId};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_sdk_types::AcceptedOperation;
use ployz_test_support::ids::{event_sequence, operation_id};
use ployzctl::commands::deploy::{DeployCommand, DeployOutput};
use ployzctl::commands::{PloyzctlCommand, parse_command};

#[test]
fn cli_dispatches_deploy_request() {
    let command = parse_command(deploy_args()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_deploy_fixture(&command);
}

#[test]
fn cli_dispatches_deploy_request_with_route() {
    let command = parse_command(deploy_args_with_route()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(
        first_service(&command).routes,
        vec![DeployRoute {
            target: RouteTarget {
                hostname: RouteHostname::try_new("api.example.com").expect("valid route hostname"),
                port: RoutePort::try_new(443).expect("valid route port"),
            },
            endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
        }]
    );
}

#[test]
fn cli_requires_route_port_when_deploy_route_hostname_is_set() {
    let args = deploy_arg_refs()
        .chain(["--route-hostname", "api.example.com"])
        .map(str::to_owned);

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_requires_endpoint_port_when_deploy_route_is_set() {
    let args = deploy_arg_refs()
        .chain(["--route-hostname", "api.example.com", "--route-port", "443"])
        .map(str::to_owned);

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_accepts_deploy_submit_request() {
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
    ]
    .map(str::to_owned);

    let command = parse_command(args).expect("deploy parses");
    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_deploy_fixture(&command);
}

// ---------------------------------------------------------------------------
// Quick-start deploy shorthand (U7)
// ---------------------------------------------------------------------------

/// AE4: `deploy --image ghcr.io/acme/web:latest --route app.example.com:8000`
/// is sufficient — one replica, route `app.example.com` on public HTTP port
/// 80 to container endpoint port 8000, ids derived from the command intent.
#[test]
fn cli_deploy_shorthand_derives_full_request() {
    let command = parse_command(quick_start_deploy_args()).expect("shorthand deploy parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(
        first_service(&command).service_id,
        ServiceId::try_new("web").expect("valid service id")
    );
    assert_eq!(
        first_service(&command).image,
        ImageReference::try_new("ghcr.io/acme/web:latest").expect("valid image")
    );
    assert_eq!(
        first_service(&command).replicas,
        ReplicaCount::try_new(1).expect("valid replicas")
    );
    assert_eq!(
        first_service(&command).routes,
        vec![DeployRoute {
            target: RouteTarget {
                hostname: RouteHostname::try_new("app.example.com").expect("valid route hostname"),
                port: RoutePort::try_new(80).expect("valid route port"),
            },
            endpoint_port: RoutePort::try_new(8000).expect("valid endpoint port"),
        }]
    );
    assert!(
        command.namespace_revision_id.as_str().starts_with("rev_latest_"),
        "derived revision id carries the image tag: {}",
        command.namespace_revision_id.as_str()
    );
    assert!(
        command.operation_id.as_str().starts_with("op_deploy_web_"),
        "derived operation id carries the command intent: {}",
        command.operation_id.as_str()
    );
}

/// KTD9: two identical shorthand invocations must not collide on generated ids.
#[test]
fn cli_deploy_shorthand_generates_collision_resistant_ids() {
    let first = shorthand_deploy_command(quick_start_deploy_args());
    let second = shorthand_deploy_command(quick_start_deploy_args());

    assert_ne!(first.operation_id, second.operation_id);
    assert_ne!(first.namespace_revision_id, second.namespace_revision_id);
}

/// R12: explicit expert flags override every derived value.
#[test]
fn cli_explicit_flags_override_shorthand_derivations() {
    let args = quick_start_deploy_args().chain(
        [
            "--service",
            "svc_custom",
            "--revision",
            "rev_pinned",
            "--replicas",
            "3",
        ]
        .map(str::to_owned),
    );

    let command = shorthand_deploy_command(args);

    assert_eq!(
        first_service(&command).service_id,
        ServiceId::try_new("svc_custom").expect("valid service id")
    );
    assert_eq!(
        command.namespace_revision_id,
        NamespaceRevisionId::try_new("rev_pinned").expect("valid revision id")
    );
    assert_eq!(
        first_service(&command).replicas,
        ReplicaCount::try_new(3).expect("valid replicas")
    );
}

#[test]
fn cli_deploy_shorthand_route_conflicts_with_explicit_route_flags() {
    let args = quick_start_deploy_args()
        .chain(
            [
                "--route-hostname",
                "api.example.com",
                "--route-port",
                "443",
                "--endpoint-port",
                "8080",
            ]
            .map(str::to_owned),
        )
        .collect::<Vec<_>>();

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_deploy_route_without_port_fails_clearly() {
    let error = parse_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--route",
            "app.example.com",
        ]
        .map(str::to_owned),
    )
    .expect_err("route without port is rejected");

    let message = error.to_string();
    assert!(
        message.contains("--route") && message.contains("HOST:PORT"),
        "error names the flag and the expected shape: {message}"
    );
}

#[test]
fn cli_deploy_route_with_non_numeric_port_fails_clearly() {
    let error = parse_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--route",
            "app.example.com:http",
        ]
        .map(str::to_owned),
    )
    .expect_err("non-numeric endpoint port is rejected");

    let message = error.to_string();
    assert!(
        message.contains("--route") && message.contains("\"http\""),
        "error names the flag and the bad port: {message}"
    );
}

#[test]
fn cli_deploy_route_with_zero_port_fails_clearly() {
    let error = parse_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--route",
            "app.example.com:0",
        ]
        .map(str::to_owned),
    )
    .expect_err("zero endpoint port is rejected");

    assert!(error.to_string().contains("--route"));
}

#[test]
fn cli_deploy_route_with_invalid_hostname_fails_clearly() {
    let error = parse_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--route",
            "app_example:8000",
        ]
        .map(str::to_owned),
    )
    .expect_err("invalid route hostname is rejected");

    let message = error.to_string();
    assert!(
        message.contains("--route") && message.contains("hostname"),
        "error names the flag and the hostname problem: {message}"
    );
}

/// Image references with registry ports and bare official images still
/// derive the repository leaf as the service id.
#[test]
fn cli_deploy_shorthand_derives_service_from_image_shapes() {
    let cases = [
        ("ghcr.io/acme/web:latest", "web"),
        ("localhost:5000/web", "web"),
        ("redis:7", "redis"),
        ("nginx", "nginx"),
        (
            "ghcr.io/acme/web@sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "web",
        ),
    ];

    for (image, expected_service) in cases {
        let command = shorthand_deploy_command(["deploy", "--image", image].map(str::to_owned));
        assert_eq!(
            first_service(&command).service_id,
            ServiceId::try_new(expected_service).expect("valid service id"),
            "image {image} derives service {expected_service}"
        );
    }
}

/// Untagged images still derive a generated revision id.
#[test]
fn cli_deploy_shorthand_derives_revision_for_untagged_image() {
    let command =
        shorthand_deploy_command(["deploy", "--image", "ghcr.io/acme/web"].map(str::to_owned));

    assert!(
        command.namespace_revision_id.as_str().starts_with("rev_"),
        "derived revision id is rev-prefixed: {}",
        command.namespace_revision_id.as_str()
    );
}

/// Image tags with dots (semver) are sanitized into the generated revision
/// id rather than failing the deploy.
#[test]
fn cli_deploy_shorthand_sanitizes_dotted_tag_into_revision() {
    let command = shorthand_deploy_command(
        ["deploy", "--image", "ghcr.io/acme/web:1.2.3"].map(str::to_owned),
    );

    assert!(
        command.namespace_revision_id.as_str().starts_with("rev_1-2-3_"),
        "dotted tag is sanitized: {}",
        command.namespace_revision_id.as_str()
    );
}

/// A repository leaf that is not a valid service id fails with the
/// `--service` escape hatch instead of installing a rewritten name.
#[test]
fn cli_deploy_shorthand_with_dotted_leaf_suggests_service_flag() {
    let error = parse_command(["deploy", "--image", "ghcr.io/acme/my.app:1"].map(str::to_owned))
        .expect_err("dotted repository leaf cannot derive a service id");

    let message = error.to_string();
    assert!(
        message.contains("my.app") && message.contains("--service"),
        "error names the leaf and the escape hatch: {message}"
    );

    let command = shorthand_deploy_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/my.app:1",
            "--service",
            "my-app",
        ]
        .map(str::to_owned),
    );
    assert_eq!(
        first_service(&command).service_id,
        ServiceId::try_new("my-app").expect("valid service id")
    );
}

/// The quick-start deploy without any cluster context tells the operator to
/// run `machine init` (R10 failure path).
#[test]
fn binary_quick_start_deploy_without_context_points_at_machine_init() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args([
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--route",
            "app.example.com:8000",
        ])
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

fn quick_start_deploy_args() -> impl Iterator<Item = String> {
    [
        "deploy",
        "--image",
        "ghcr.io/acme/web:latest",
        "--route",
        "app.example.com:8000",
    ]
    .into_iter()
    .map(str::to_owned)
}

fn shorthand_deploy_command(args: impl IntoIterator<Item = String>) -> DeployCommand {
    let command = parse_command(args).expect("deploy command parses");
    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    command
}

#[test]
fn binary_deploy_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args(deploy_arg_refs())
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployzctl machine init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
    );
}

#[test]
fn deploy_prints_operation_id_without_runtime_details() {
    let output = DeployOutput {
        accepted: accepted_operation("op_deploy"),
    }
    .render();

    assert_eq!(
        output,
        "operation op_deploy\nwatch ployzctl ops watch op_deploy\n"
    );
}

fn assert_deploy_fixture(command: &DeployCommand) {
    assert!(
        command
            .operation_id
            .as_str()
            .starts_with("op_deploy_svc_api_")
    );
    assert_eq!(
        command.namespace_id,
        NamespaceId::try_new("default").expect("valid namespace id")
    );
    assert_eq!(
        command.namespace_revision_id,
        NamespaceRevisionId::try_new("rev_2").expect("valid revision id")
    );
    assert_eq!(
        command.services,
        vec![DeployServiceSpec {
            service_id: ServiceId::try_new("svc_api").expect("valid service id"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            replicas: ReplicaCount::try_new(1).expect("valid replicas"),
            routes: Vec::new(),
        }]
    );
    assert!(!command.detach);
}

fn first_service(command: &DeployCommand) -> &DeployServiceSpec {
    command.first_service().expect("deploy has a service")
}

fn deploy_args() -> impl Iterator<Item = String> {
    deploy_arg_refs().map(str::to_owned)
}

fn deploy_args_with_route() -> impl Iterator<Item = String> {
    deploy_arg_refs()
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

fn deploy_arg_refs() -> impl Iterator<Item = &'static str> {
    [
        "deploy",
        "--service",
        "svc_api",
        "--revision",
        "rev_2",
        "--image",
        "ghcr.io/acme/api:rev-2",
        "--replicas",
        "1",
    ]
    .into_iter()
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
