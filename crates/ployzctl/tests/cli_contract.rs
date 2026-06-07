use std::process::{Command, Output};

use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT, OperationEventReplayLimit,
    OperationIdempotencyKey, OperationLeaseExpiresAt, OperationOwnerLease, ReplayedOperationEvent,
};
use ployz_sdk_types::{
    AcceptedOperation, MachineAddGateway, MachineJoinBundle, MachineJoinPloyzdArtifact,
};
use ployzctl::commands::deploy::{DetachedDeployCommand, DetachedDeployOutput};
use ployzctl::commands::init::{FirstNodeGateway, FirstNodeInitOutput, first_node_process_set};
use ployzctl::commands::machine::{
    MachineAddOutput, MachineBootstrapUrl, MachineJoinToken, MachineName,
};
use ployzctl::commands::ops::WatchOutput;
use ployzctl::commands::{
    PloyzctlCliError, PloyzctlCommand, USAGE, parse_command, parse_invocation,
};

#[test]
fn init_first_node_reports_supervised_product_roles() {
    let node_id = NodeId::try_new("node_1").expect("valid node id");
    let output = FirstNodeInitOutput::summary(node_id.clone(), FirstNodeGateway::Skip).render();

    assert_eq!(
        output,
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node\n"
    );
    assert_eq!(
        first_node_process_set(&node_id, FirstNodeGateway::Skip).roles(),
        &[
            ployz_core::roles::DaemonProcessRole::Tunnel(ployz_core::roles::TunnelSide::Core),
            ployz_core::roles::DaemonProcessRole::Control,
            ployz_core::roles::DaemonProcessRole::Node(node_id),
        ]
    );
}

#[test]
fn cli_init_can_emit_keeper_first_node_install_command() {
    let command = parse_command(init_with_keeper_install_args()).expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };

    assert_eq!(command.node_id(), &node_id("node_1"));
    assert_eq!(command.gateway(), FirstNodeGateway::Install);
    assert_eq!(
        command.render(),
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\ninstall ployz-keeper first-node-install --node 'node_1' --ployzd-version '0.1.0' --ployzd-source '/tmp/ployzd' --ployzd-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --ployzd-install-path '/usr/local/bin/ployzd' --nats-binary '/usr/local/bin/nats-server' --nats-config '/etc/nats/nats-server.conf' --gateway\n"
    );
}

#[test]
fn cli_init_requires_complete_keeper_install_inputs() {
    assert!(matches!(
        parse_command(
            [
                "init",
                "--node",
                "node_1",
                "--emit-keeper-install",
                "--ployzd-version",
                "0.1.0"
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--ployzd-source"
    ));
}

#[test]
fn cli_init_requires_explicit_keeper_install_mode() {
    assert!(matches!(
        parse_command(
            [
                "init",
                "--node",
                "node_1",
                "--ployzd-version",
                "0.1.0"
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--emit-keeper-install"
    ));
}

#[test]
fn cli_init_validates_keeper_install_inputs_before_rendering() {
    assert!(matches!(
        parse_command(
            [
                "init",
                "--node",
                "node_1",
                "--emit-keeper-install",
                "--ployzd-version",
                "0.1.0",
                "--ployzd-source",
                "relative/ployzd",
                "--ployzd-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ployzd-install-path",
                "/usr/local/bin/ployzd",
                "--nats-binary",
                "/usr/local/bin/nats-server",
                "--nats-config",
                "/etc/nats/nats-server.conf",
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::InvalidValue { flag, .. })
            if flag == "--ployzd-source"
    ));
}

#[test]
fn cli_dispatches_init_first_node() {
    let command = parse_command(["init", "--gateway", "--node", "node_1"].map(str::to_owned))
        .expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };
    assert_eq!(
        command.render(),
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\n"
    );
}

#[test]
fn cli_rejects_init_without_node() {
    assert!(matches!(
        parse_command(["init"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--node"
    ));
}

#[test]
fn cli_rejects_option_like_init_node_values() {
    assert!(matches!(
        parse_command(["init", "--node", "--gateway"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingValue { flag }) if flag == "--node"
    ));
    assert!(matches!(
        parse_command(["init", "--node", "--help"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingValue { flag }) if flag == "--node"
    ));
}

#[test]
fn cli_renders_help_for_no_args() {
    assert_eq!(
        parse_command(std::iter::empty::<String>()).expect("no args renders help"),
        PloyzctlCommand::Help
    );
}

#[test]
fn cli_dispatches_detached_deploy_request() {
    let command = parse_command(detached_deploy_args()).expect("deploy command parses");

    let PloyzctlCommand::Deploy(command) = command else {
        panic!("expected deploy command");
    };
    assert_eq!(command, detached_deploy_command());
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
fn cli_dispatches_ops_watch_request() {
    let command =
        parse_command(["ops", "watch", "op_deploy"].map(str::to_owned)).expect("ops watch parses");

    let PloyzctlCommand::OpsWatch(command) = command else {
        panic!("expected ops watch command");
    };
    let request = command.into_request();

    assert_eq!(request.operation_id, operation_id("op_deploy"));
    assert_eq!(request.start_sequence, event_sequence(1));
    assert_eq!(
        request.limit,
        OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT)
            .expect("valid replay limit")
    );
}

#[test]
fn cli_requires_ops_watch_operation_id() {
    assert!(matches!(
        parse_command(["ops", "watch"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "<operation_id>"
    ));
}

#[test]
fn cli_dispatches_machine_add_request() {
    let command =
        parse_command(machine_add_args_with_gateway()).expect("machine add command parses");

    let PloyzctlCommand::MachineAdd(command) = command else {
        panic!("expected machine add command");
    };
    assert_eq!(command.operation_id, operation_id("op_machine"));
    assert_eq!(
        command.idempotency_key,
        OperationIdempotencyKey::try_new("idem_machine").expect("valid idempotency key")
    );
    assert_eq!(command.node_id, node_id("node_2"));
    assert_eq!(
        command.name,
        MachineName::try_new("edge_2").expect("valid machine name")
    );
    assert_eq!(command.gateway, MachineAddGateway::Install);
    assert_eq!(command.join_bundle, machine_join_bundle());
}

#[test]
fn cli_parses_global_nats_url() {
    let invocation = parse_invocation(
        ["--nats", "nats://127.0.0.1:4222"]
            .into_iter()
            .chain(machine_add_arg_refs())
            .map(str::to_owned),
    )
    .expect("invocation parses");

    assert_eq!(
        invocation.nats_url.as_deref(),
        Some("nats://127.0.0.1:4222")
    );
    assert!(matches!(invocation.command, PloyzctlCommand::MachineAdd(_)));
}

#[test]
fn cli_requires_machine_add_operation_id() {
    assert!(matches!(
        parse_command(
            machine_add_arg_refs()
                .filter(|value| *value != "--operation" && *value != "op_machine")
                .map(str::to_owned)
        ),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--operation"
    ));
}

#[test]
fn cli_requires_machine_add_idempotency_key() {
    assert!(matches!(
        parse_command(
            machine_add_arg_refs()
                .filter(|value| *value != "--idempotency-key" && *value != "idem_machine")
                .map(str::to_owned)
        ),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--idempotency-key"
    ));
}

#[test]
fn binary_help_only_advertises_implemented_commands() {
    let output = run_ployzctl(&[]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), format!("{USAGE}\n"));
    assert!(stdout(&output).contains("ployzctl [--nats <url>] <command>"));
    assert!(stdout(&output).contains(
        "ployzctl init --node <id> [--gateway] [--emit-keeper-install --ployzd-version <version> --ployzd-source <path> --ployzd-sha256 <sha256> --ployzd-install-path <path> --nats-binary <path> --nats-config <path>]"
    ));
    assert!(stdout(&output).contains(
        "ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n> --operation <id> --idempotency-key <key>"
    ));
    assert!(stdout(&output).contains(
        "ployzctl machine add --node <id> --name <name> --operation <id> --idempotency-key <key> --cluster <name> --ployzd-version <version> --ployzd-source <path-or-url> --ployzd-sha256 <sha256> --ployzd-install-path <path> [--gateway]"
    ));
    assert!(stdout(&output).contains("ployzctl ops watch <operation_id>"));
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_dispatches_init_first_node() {
    let output = run_ployzctl(&["init", "--node", "node_1", "--gateway"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        stdout(&output),
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\n"
    );
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_init_can_print_keeper_first_node_install_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .args(init_with_keeper_install_arg_refs())
        .output()
        .expect("ployzctl binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("install ployz-keeper first-node-install"));
    assert!(stdout(&output).contains("--node 'node_1'"));
    assert!(stdout(&output).contains("--gateway"));
    assert_eq!(stderr(&output), "");
}

#[test]
fn binary_rejects_unimplemented_commands() {
    let output = run_ployzctl(&["service"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "unexpected argument: service\n");
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
fn binary_machine_add_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(machine_add_arg_refs())
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "--nats or PLOYZ_NATS_URL is required\n");
}

#[test]
fn binary_ops_watch_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(["ops", "watch", "op_deploy"])
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "--nats or PLOYZ_NATS_URL is required\n");
}

#[test]
fn init_first_node_can_include_gateway_role() {
    let output = FirstNodeInitOutput::summary(
        NodeId::try_new("node_1").expect("valid node id"),
        FirstNodeGateway::Install,
    )
    .render();

    assert_eq!(
        output,
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\n"
    );
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

#[test]
fn ops_watch_renders_persisted_operation_events() {
    let output = WatchOutput {
        events: vec![
            replayed(
                1,
                ployz_core::ops::OperationEvent::DeploySubmitted {
                    operation_id: operation_id("op_123"),
                    target: deploy_request(),
                },
            ),
            replayed(
                2,
                ployz_core::ops::OperationEvent::DeployCompleted {
                    operation_id: operation_id("op_123"),
                },
            ),
        ],
    }
    .render();

    assert_eq!(output, "1 deploy.submitted\n2 deploy.completed\n");
}

#[test]
fn ops_watch_renders_no_output_when_no_events_are_replayed() {
    let output = WatchOutput { events: Vec::new() }.render();

    assert_eq!(output, "");
}

#[test]
fn machine_add_prints_bootstrap_command_without_nats_credentials() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    }
    .render();

    assert!(output.contains("operation op_machine"));
    assert!(output.contains("node node_2"));
    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains("--join-token 'join_once_123'"));
    assert!(!output.contains("nats"));
    assert!(!output.contains("creds"));
}

#[test]
fn machine_add_debug_redacts_join_token() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    };

    let debug = format!("{output:?}");

    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("join_once_123"));
}

#[test]
fn machine_add_shell_quotes_join_material() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh/bootstrap?x='quoted'")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join'quoted'").expect("valid join token"),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh/bootstrap?x='\\''quoted'\\'''"));
    assert!(output.contains("--join-token 'join'\\''quoted'\\'''"));
}

#[test]
fn machine_add_rejects_bootstrap_url_that_curl_could_treat_as_an_option() {
    assert!(MachineBootstrapUrl::try_new("--help").is_err());
    assert!(MachineBootstrapUrl::try_new("http://get.ployz.sh").is_err());
}

#[test]
fn machine_add_rejects_join_tokens_with_shell_invisible_characters() {
    assert!(MachineJoinToken::try_new("").is_err());
    assert!(MachineJoinToken::try_new("join token").is_err());
    assert!(MachineJoinToken::try_new("join\ntoken").is_err());
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

fn replayed(sequence: u64, event: ployz_core::ops::OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        event,
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
    }
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
    }
}

fn detached_deploy_args() -> impl Iterator<Item = String> {
    detached_deploy_arg_refs().into_iter().map(str::to_owned)
}

fn detached_deploy_arg_refs() -> [&'static str; 14] {
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
}

fn machine_add_args_with_gateway() -> impl Iterator<Item = String> {
    machine_add_arg_refs()
        .chain(["--gateway"])
        .map(str::to_owned)
}

fn machine_add_arg_refs() -> impl Iterator<Item = &'static str> {
    [
        "machine",
        "add",
        "--node",
        "node_2",
        "--name",
        "edge_2",
        "--operation",
        "op_machine",
        "--idempotency-key",
        "idem_machine",
        "--cluster",
        "prod",
        "--ployzd-version",
        "0.1.0",
        "--ployzd-source",
        "/tmp/ployzd",
        "--ployzd-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ployzd-install-path",
        "/usr/local/bin/ployzd",
    ]
    .into_iter()
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
            .expect("valid cluster name"),
        ployzd: MachineJoinPloyzdArtifact {
            version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                .expect("valid version"),
            source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployzd")
                .expect("valid source"),
            sha256: ployz_core::install::InstallSha256Digest::try_new(PLOYZ_NEWLINE_SHA256)
                .expect("valid digest"),
            install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                "/usr/local/bin/ployzd",
            )
            .expect("valid install path"),
        },
    }
}

const PLOYZ_NEWLINE_SHA256: &str =
    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e";

fn init_with_keeper_install_args() -> impl Iterator<Item = String> {
    init_with_keeper_install_arg_refs()
        .into_iter()
        .map(str::to_owned)
}

fn init_with_keeper_install_arg_refs() -> [&'static str; 17] {
    [
        "init",
        "--node",
        "node_1",
        "--gateway",
        "--emit-keeper-install",
        "--ployzd-version",
        "0.1.0",
        "--ployzd-source",
        "/tmp/ployzd",
        "--ployzd-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ployzd-install-path",
        "/usr/local/bin/ployzd",
        "--nats-binary",
        "/usr/local/bin/nats-server",
        "--nats-config",
        "/etc/nats/nats-server.conf",
    ]
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn run_ployzctl(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .args(args)
        .output()
        .expect("ployzctl binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
