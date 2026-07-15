use std::process::{Command, Output};

use ployz::commands::{PloyzctlCommand, parse_command};
use ployz::deploy::command::{
    DeployCommand, DeployOutput, DeployRollbackCommand, DeployRollbackSelection,
};
use ployz_core::deploy::{
    DeployOrigin, DeployRoute, DeployRouteTarget, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceId, OperationId, ServiceId};
use ployz_core::operation::{RouteHostname, RoutePort};
use ployz_sdk_types::AcceptedOperation;
use ployz_test_support::ids::{event_sequence, operation_id};

#[test]
fn cli_dispatches_deploy_request() {
    let command = parse_command(deploy_args()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_deploy_fixture(&command);
}

#[test]
fn cli_deploy_accepts_origin_caption() {
    let command = shorthand_deploy_command(
        quick_start_deploy_args().chain(["--origin", "release 2026-07-10"].map(str::to_owned)),
    );

    assert_eq!(
        command.target.origin,
        Some(DeployOrigin::try_new("release 2026-07-10").expect("valid origin"))
    );
}

#[test]
fn cli_deploy_history_defaults_to_default_namespace() {
    let command =
        parse_command(["deploy", "history"].map(str::to_owned)).expect("deploy history parses");

    let PloyzctlCommand::DeployHistory(command) = command else {
        panic!("expected deploy history command");
    };
    assert_eq!(
        command.namespace_id,
        NamespaceId::try_new("default").expect("valid namespace")
    );
}

#[test]
fn cli_deploy_history_accepts_namespace() {
    let command =
        parse_command(["deploy", "history", "--namespace", "production"].map(str::to_owned))
            .expect("deploy history parses");

    let PloyzctlCommand::DeployHistory(command) = command else {
        panic!("expected deploy history command");
    };
    assert_eq!(
        command.namespace_id,
        NamespaceId::try_new("production").expect("valid namespace")
    );
}

#[test]
fn cli_deploy_rollback_defaults_to_interactive_default_namespace() {
    let command =
        parse_command(["deploy", "rollback"].map(str::to_owned)).expect("deploy rollback parses");

    let PloyzctlCommand::DeployRollback(command) = command else {
        panic!("expected deploy rollback command");
    };
    assert_eq!(
        command,
        DeployRollbackCommand {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace"),
            selection: DeployRollbackSelection::Interactive,
        }
    );
}

#[test]
fn cli_deploy_rollback_accepts_namespace() {
    let command =
        parse_command(["deploy", "rollback", "--namespace", "production"].map(str::to_owned))
            .expect("deploy rollback parses");

    let PloyzctlCommand::DeployRollback(command) = command else {
        panic!("expected deploy rollback command");
    };
    assert_eq!(
        command.namespace_id,
        NamespaceId::try_new("production").expect("valid namespace")
    );
}

#[test]
fn cli_deploy_rollback_accepts_operation() {
    let command = parse_command(["deploy", "rollback", "--to", "op_deploy_123"].map(str::to_owned))
        .expect("deploy rollback parses");

    let PloyzctlCommand::DeployRollback(command) = command else {
        panic!("expected deploy rollback command");
    };
    assert_eq!(
        command.selection,
        DeployRollbackSelection::Operation(
            OperationId::try_new("op_deploy_123").expect("valid operation id")
        )
    );
}

#[test]
fn cli_deploy_rollback_accepts_last_good() {
    let command = parse_command(["deploy", "rollback", "--last-good"].map(str::to_owned))
        .expect("deploy rollback parses");

    let PloyzctlCommand::DeployRollback(command) = command else {
        panic!("expected deploy rollback command");
    };
    assert_eq!(command.selection, DeployRollbackSelection::LastGood);
}

#[test]
fn cli_deploy_rollback_rejects_operation_with_last_good() {
    let result = parse_command(
        ["deploy", "rollback", "--to", "op_deploy_123", "--last-good"].map(str::to_owned),
    );

    assert!(result.is_err());
}

#[test]
fn cli_deploy_rollback_rejects_invalid_operation() {
    let error =
        parse_command(["deploy", "rollback", "--to", "not an operation"].map(str::to_owned))
            .expect_err("invalid rollback operation is rejected");

    assert!(error.to_string().contains("--to"));
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
            target: DeployRouteTarget::Hostname {
                hostname: RouteHostname::try_new("api.example.com").expect("valid route hostname"),
            },
            endpoint_port: RoutePort::try_new(8080).expect("valid endpoint port"),
        }]
    );
}

#[test]
fn cli_requires_endpoint_port_when_deploy_route_hostname_is_set() {
    let args = deploy_arg_refs()
        .chain(["--route-hostname", "api.example.com"])
        .map(str::to_owned);

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_requires_route_hostname_when_endpoint_port_is_set() {
    let args = deploy_arg_refs()
        .chain(["--endpoint-port", "8080"])
        .map(str::to_owned);

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_accepts_deploy_submit_request() {
    let args = [
        "deploy",
        "--service",
        "svc_api",
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
            target: DeployRouteTarget::Hostname {
                hostname: RouteHostname::try_new("app.example.com").expect("valid route hostname"),
            },
            endpoint_port: RoutePort::try_new(8000).expect("valid endpoint port"),
        }]
    );
    assert!(
        command
            .idempotency_key
            .as_str()
            .starts_with("idem_deploy_web_"),
        "derived idempotency key carries the command intent: {}",
        command.idempotency_key.as_str()
    );
}

/// KTD9: two identical shorthand invocations must not collide on generated ids.
#[test]
fn cli_deploy_shorthand_generates_collision_resistant_ids() {
    let first = shorthand_deploy_command(quick_start_deploy_args());
    let second = shorthand_deploy_command(quick_start_deploy_args());

    assert_ne!(first.idempotency_key, second.idempotency_key);
}

/// R12: explicit expert flags override derived service settings.
#[test]
fn cli_explicit_flags_override_shorthand_derivations() {
    let args = quick_start_deploy_args()
        .chain(["--service", "svc_custom", "--replicas", "3"].map(str::to_owned));

    let command = shorthand_deploy_command(args);

    assert_eq!(
        first_service(&command).service_id,
        ServiceId::try_new("svc_custom").expect("valid service id")
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
                "--endpoint-port",
                "8080",
            ]
            .map(str::to_owned),
        )
        .collect::<Vec<_>>();

    assert!(parse_command(args).is_err());
}

#[test]
fn cli_rejects_allow_unsupported_without_compose_file() {
    let error = parse_command(
        [
            "deploy",
            "--image",
            "ghcr.io/acme/web:latest",
            "--allow-unsupported",
        ]
        .map(str::to_owned),
    )
    .expect_err("allow unsupported requires compose file");

    assert!(
        error
            .to_string()
            .contains("--allow-unsupported requires -f")
    );
}

#[test]
fn cli_allows_unsupported_compose_when_flag_is_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let compose_path = dir.path().join("compose.yaml");
    std::fs::write(
        &compose_path,
        r#"
        name: default
        services:
          web:
            image: nginx
            mystery: true
        "#,
    )
    .expect("write compose");

    let command = parse_command([
        "deploy".to_owned(),
        "-f".to_owned(),
        compose_path.display().to_string(),
        "--allow-unsupported".to_owned(),
        "--detach".to_owned(),
    ])
    .expect("allow unsupported compose parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(command.target.services.len(), 1);
    assert!(
        command
            .warnings
            .first()
            .expect("one compose warning")
            .contains("services.web.mystery")
    );
}

#[test]
fn compose_check_and_deploy_render_identical_diagnostics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let compose_path = dir.path().join("compose.yaml");
    std::fs::write(
        &compose_path,
        "name: parity\nservices:\n  web:\n    image: nginx\n    mystery: true\n",
    )
    .expect("write compose");
    let path = compose_path.display().to_string();

    let deploy = parse_command(["deploy", "-f", &path].map(str::to_owned))
        .expect_err("deploy rejects unsupported compose");
    let check = parse_command(["compose", "check", &path].map(str::to_owned))
        .expect_err("compose check rejects unsupported compose");

    assert_eq!(check.to_string(), deploy.to_string());

    let PloyzctlCommand::Deploy(deploy) = parse_command(
        ["deploy", "-f", &path, "--allow-unsupported", "--detach"].map(str::to_owned),
    )
    .expect("deploy accepts unsupported compose") else {
        panic!("expected deploy command");
    };
    let PloyzctlCommand::ComposeCheck(check) =
        parse_command(["compose", "check", &path, "--allow-unsupported"].map(str::to_owned))
            .expect("compose check accepts unsupported compose")
    else {
        panic!("expected compose check command");
    };

    assert_eq!(check.diagnostics, deploy.warnings);
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
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
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
        .expect("ployz binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployz init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
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
    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .env_remove("PLOYZ_NATS_URL")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .args(deploy_arg_refs())
        .output()
        .expect("ployz binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "no cluster context: run `ployz init USER@HOST` to create one, pass --nats, or set PLOYZ_NATS_URL\n"
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
        "operation op_deploy\nwatch ployz ops watch op_deploy\n"
    );
}

fn assert_deploy_fixture(command: &DeployCommand) {
    assert!(
        command
            .idempotency_key
            .as_str()
            .starts_with("idem_deploy_svc_api_")
    );
    assert_eq!(
        command.target.namespace_id,
        NamespaceId::try_new("default").expect("valid namespace id")
    );
    assert_eq!(command.target.origin, None);
    assert_eq!(
        command.target.services,
        vec![DeployServiceSpec {
            keep: None,
            service_id: ServiceId::try_new("svc_api").expect("valid service id"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replicas"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }]
    );
    assert!(command.warnings.is_empty());
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
        watch_subject: format!("plz.v1.progress.namespace.default.operation.{operation_id}.>"),
        start_sequence: event_sequence(1),
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
