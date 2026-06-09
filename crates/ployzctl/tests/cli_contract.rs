use std::fs;
use std::process::{Command, Output};

use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployRunningStage, EventSequence, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    OperationEventReplayLimit, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationOwnershipStatus, OperationStatus, OperationStatusSnapshot,
    ReplayedOperationEvent,
};
use ployz_core::state::{
    ActiveMachineState, ActiveServiceState, GatewayServingStatus, GatewayStatusObservation,
    NodePublicIpObservation,
};
use ployz_sdk_types::{
    AcceptedOperation, LogsTailLines, MachineAddGateway, MachineSnapshot, ServiceSnapshot,
};
use ployzctl::commands::init::{FirstNodeGateway, FirstNodeInitOutput, first_node_process_set};
use ployzctl::commands::machine::{
    MachineAddOutput, MachineBootstrapUrl, MachineInspectOutput, MachineJoinRuntimeNatsUrl,
    MachineJoinToken, MachineListOutput, MachineName,
};
use ployzctl::commands::ops::{StatusOutput, WatchOutput};
use ployzctl::commands::service::{ServiceInspectOutput, ServiceListOutput};
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
fn cli_init_activate_first_node_is_explicit_subcommand() {
    let command = parse_command(
        [
            "init",
            "activate-first-node",
            "--node",
            "node_1",
            "--gateway",
        ]
        .map(str::to_owned),
    )
    .expect("activation-only init command parses");

    let PloyzctlCommand::InitFirstNodeActivate(command) = command else {
        panic!("expected first-node activation command");
    };

    assert_eq!(command.node_id, node_id("node_1"));
    assert_eq!(command.gateway, FirstNodeGateway::Install);
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
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\ninstall ployz-keeper first-node-install --node 'node_1' --ployzd-version '0.1.0' --ployzd-source '/tmp/ployzd' --ployzd-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --ployzd-install-path '/usr/local/bin/ployzd' --ebpf-bytecode-version '0.1.0' --ebpf-bytecode-source '/tmp/ployz-ebpf-tc' --ebpf-bytecode-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --ebpf-bytecode-install-path '/usr/local/lib/ployz/ebpf/ployz-ebpf-tc' --ebpf-ctl-version '0.1.0' --ebpf-ctl-source '/tmp/ployz-ebpf-ctl' --ebpf-ctl-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --ebpf-ctl-install-path '/usr/local/bin/ployz-ebpf-ctl' --nats-version '2.12.0' --nats-source '/tmp/nats-server' --nats-sha256 '0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e' --nats-binary '/usr/local/bin/nats-server' --nats-config '/etc/nats/nats-server.conf' --gateway --machine-join-template-file '/etc/ployz/machine-join-template.json'\n"
    );
}

#[test]
fn cli_init_can_pass_first_node_public_ip_to_keeper_install() {
    let command = parse_command(
        init_with_keeper_install_arg_refs()
            .into_iter()
            .chain(["--node-public-ip", "203.0.113.10"])
            .map(str::to_owned),
    )
    .expect("init command parses");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };

    assert!(command.render().contains("--node-public-ip '203.0.113.10'"));
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
            if flag == "--emit-keeper-install or --run-keeper-install"
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
                "--ebpf-bytecode-version",
                "0.1.0",
                "--ebpf-bytecode-source",
                "/tmp/ployz-ebpf-tc",
                "--ebpf-bytecode-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-bytecode-install-path",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                "--ebpf-ctl-version",
                "0.1.0",
                "--ebpf-ctl-source",
                "/tmp/ployz-ebpf-ctl",
                "--ebpf-ctl-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-ctl-install-path",
                "/usr/local/bin/ployz-ebpf-ctl",
                "--nats-version",
                "2.12.0",
                "--nats-source",
                "/tmp/nats-server",
                "--nats-sha256",
                PLOYZ_NEWLINE_SHA256,
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
fn cli_dispatches_ops_status_request() {
    let command = parse_command(["ops", "status", "op_deploy"].map(str::to_owned))
        .expect("ops status parses");

    let PloyzctlCommand::OpsStatus(command) = command else {
        panic!("expected ops status command");
    };

    assert_eq!(
        command.into_request().operation_id,
        operation_id("op_deploy")
    );
}

#[test]
fn cli_requires_ops_status_operation_id() {
    assert!(matches!(
        parse_command(["ops", "status"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "<operation_id>"
    ));
}

#[test]
fn cli_requires_ops_watch_operation_id() {
    assert!(matches!(
        parse_command(["ops", "watch"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "<operation_id>"
    ));
}

#[test]
fn cli_dispatches_backup_create_request() {
    let command = parse_command(
        [
            "backup",
            "create",
            "--operation",
            "op_backup",
            "--idempotency-key",
            "idem_backup",
        ]
        .map(str::to_owned),
    )
    .expect("backup create parses");

    let PloyzctlCommand::BackupCreate(command) = command else {
        panic!("expected backup create command");
    };
    let request = command.into_request();

    assert_eq!(request.operation_id, operation_id("op_backup"));
    assert_eq!(
        request.idempotency_key,
        OperationIdempotencyKey::try_new("idem_backup").expect("valid idempotency key")
    );
}

#[test]
fn cli_dispatches_backup_restore_plan() {
    let command = parse_command(["backup", "restore", "--plan"].map(str::to_owned))
        .expect("backup restore plan parses");

    assert!(matches!(command, PloyzctlCommand::BackupRestorePlan(_)));
}

#[test]
fn cli_requires_backup_restore_plan_flag() {
    assert!(matches!(
        parse_command(["backup", "restore"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "--plan"
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
}

#[test]
fn cli_dispatches_machine_list_request() {
    let command =
        parse_command(["machine", "list"].map(str::to_owned)).expect("machine list command parses");

    let PloyzctlCommand::MachineList(command) = command else {
        panic!("expected machine list command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::MachineListRequest {}
    );
}

#[test]
fn cli_dispatches_machine_inspect_request() {
    let command = parse_command(["machine", "inspect", "node_2"].map(str::to_owned))
        .expect("machine inspect command parses");

    let PloyzctlCommand::MachineInspect(command) = command else {
        panic!("expected machine inspect command");
    };

    assert_eq!(command.into_request().node_id, node_id("node_2"));
}

#[test]
fn cli_dispatches_service_list_request() {
    let command =
        parse_command(["service", "list"].map(str::to_owned)).expect("service list command parses");

    let PloyzctlCommand::ServiceList(command) = command else {
        panic!("expected service list command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::ServiceListRequest {}
    );
}

#[test]
fn cli_dispatches_service_inspect_request() {
    let command = parse_command(["service", "inspect", "svc_api"].map(str::to_owned))
        .expect("service inspect command parses");

    let PloyzctlCommand::ServiceInspect(command) = command else {
        panic!("expected service inspect command");
    };

    assert_eq!(
        command.into_request().service_id,
        ServiceId::try_new("svc_api").expect("valid service id")
    );
}

#[test]
fn cli_dispatches_logs_tail_request() {
    let command = parse_command(
        ["logs", "ctr_failed", "--node", "node_a", "--tail", "50"].map(str::to_owned),
    )
    .expect("logs tail command parses");

    let PloyzctlCommand::LogsTail(command) = command else {
        panic!("expected logs tail command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::LogsTailRequest {
            container_id: ContainerId::try_new("ctr_failed").expect("valid container id"),
            node_id: Some(node_id("node_a")),
            tail_lines: Some(LogsTailLines::try_new(50).expect("valid logs tail lines")),
        }
    );
}

#[test]
fn cli_requires_service_inspect_service_id() {
    assert!(matches!(
        parse_command(["service", "inspect"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "<service_id>"
    ));
}

#[test]
fn cli_requires_machine_inspect_node_id() {
    assert!(matches!(
        parse_command(["machine", "inspect"].map(str::to_owned)),
        Err(PloyzctlCliError::MissingRequiredArgument { flag }) if flag == "<node_id>"
    ));
}

#[test]
fn cli_parses_global_nats_url() {
    let invocation = parse_invocation(
        ["--nats", "nats://127.0.0.1:4222"]
            .into_iter()
            .map(str::to_owned)
            .chain(machine_add_arg_refs()),
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
        parse_command(machine_add_args_without("--operation")),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--operation"
    ));
}

#[test]
fn cli_requires_machine_add_idempotency_key() {
    assert!(matches!(
        parse_command(machine_add_args_without("--idempotency-key")),
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
    assert!(stdout(&output).contains("ployzctl init activate-first-node --node <id> [--gateway]"));
    assert!(stdout(&output).contains(
        "ployzctl init --node <id> [--gateway] (--emit-keeper-install | --run-keeper-install)"
    ));
    assert!(stdout(&output).contains("ployzctl init join-template --cluster <name>"));
    assert!(stdout(&output).contains("--ebpf-bytecode-install-path <path>"));
    assert!(stdout(&output).contains("--ebpf-ctl-install-path <path>"));
    assert!(stdout(&output).contains(
        "ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n> --operation <id> --idempotency-key <key>"
    ));
    assert!(
        stdout(&output).contains("ployzctl backup create --operation <id> --idempotency-key <key>")
    );
    assert!(stdout(&output).contains("ployzctl backup restore --plan"));
    assert!(stdout(&output).contains(
        "ployzctl machine add --node <id> --name <name> --operation <id> --idempotency-key <key> [--gateway]"
    ));
    assert!(stdout(&output).contains("ployzctl machine list"));
    assert!(stdout(&output).contains("ployzctl machine inspect <node_id>"));
    assert!(stdout(&output).contains("ployzctl ops status <operation_id>"));
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
fn binary_init_can_run_keeper_first_node_install_command() {
    let temp = temp_dir("ployzctl-fake-keeper");
    let keeper = temp.join("ployz-keeper");
    let captured_args = temp.join("keeper-args");
    fs::write(
        &keeper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'keeper installed\\n'\n",
            captured_args.display()
        ),
    )
    .expect("fake keeper can be written");
    make_executable(&keeper);

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .args(init_with_keeper_run_arg_refs(
            keeper.to_str().expect("keeper path is utf-8"),
        ))
        .output()
        .expect("ployzctl binary runs");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(stdout(&output), "keeper installed\n");
    assert_eq!(stderr(&output), "");
    assert_eq!(
        fs::read_to_string(captured_args).expect("fake keeper captured args"),
        "first-node-install\n--node\nnode_1\n--ployzd-version\n0.1.0\n--ployzd-source\n/tmp/ployzd\n--ployzd-sha256\n0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e\n--ployzd-install-path\n/usr/local/bin/ployzd\n--ebpf-bytecode-version\n0.1.0\n--ebpf-bytecode-source\n/tmp/ployz-ebpf-tc\n--ebpf-bytecode-sha256\n0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e\n--ebpf-bytecode-install-path\n/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\n--ebpf-ctl-version\n0.1.0\n--ebpf-ctl-source\n/tmp/ployz-ebpf-ctl\n--ebpf-ctl-sha256\n0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e\n--ebpf-ctl-install-path\n/usr/local/bin/ployz-ebpf-ctl\n--nats-version\n2.12.0\n--nats-source\n/tmp/nats-server\n--nats-sha256\n0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e\n--nats-binary\n/usr/local/bin/nats-server\n--nats-config\n/etc/nats/nats-server.conf\n--gateway\n--machine-join-template-file\n/etc/ployz/machine-join-template.json\n"
    );
}

#[test]
fn binary_init_succeeds_when_keeper_output_is_truncated() {
    let temp = temp_dir("ployzctl-verbose-keeper");
    let keeper = temp.join("ployz-keeper");
    fs::write(
        &keeper,
        "#!/bin/sh\npython3 - <<'PY'\nprint('x' * 70000)\nPY\n",
    )
    .expect("fake verbose keeper can be written");
    make_executable(&keeper);

    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .args(init_with_keeper_run_arg_refs(
            keeper.to_str().expect("keeper path is utf-8"),
        ))
        .output()
        .expect("ployzctl binary runs");

    assert!(
        output.status.success(),
        "stdout length: {}\nstdout:\n{}\nstderr:\n{}",
        output.stdout.len(),
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(output.stdout.len(), 64 * 1024);
    assert_eq!(stderr(&output), "");
}

#[test]
fn cli_init_rejects_emit_and_run_together() {
    assert!(matches!(
        parse_command(
            [
                "init",
                "--node",
                "node_1",
                "--emit-keeper-install",
                "--run-keeper-install",
                "--ployzd-version",
                "0.1.0",
                "--ployzd-source",
                "/tmp/ployzd",
                "--ployzd-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ployzd-install-path",
                "/usr/local/bin/ployzd",
                "--ebpf-bytecode-version",
                "0.1.0",
                "--ebpf-bytecode-source",
                "/tmp/ployz-ebpf-tc",
                "--ebpf-bytecode-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-bytecode-install-path",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                "--ebpf-ctl-version",
                "0.1.0",
                "--ebpf-ctl-source",
                "/tmp/ployz-ebpf-ctl",
                "--ebpf-ctl-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-ctl-install-path",
                "/usr/local/bin/ployz-ebpf-ctl",
                "--nats-version",
                "2.12.0",
                "--nats-source",
                "/tmp/nats-server",
                "--nats-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--nats-binary",
                "/usr/local/bin/nats-server",
                "--nats-config",
                "/etc/nats/nats-server.conf",
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::ConflictingArguments { first, second })
            if first == "--emit-keeper-install" && second == "--run-keeper-install"
    ));
}

#[test]
fn cli_init_accepts_keeper_binary_before_run_flag() {
    let command = parse_command(
        [
            "init",
            "--node",
            "node_1",
            "--keeper-binary",
            "/tmp/ployz-keeper",
            "--run-keeper-install",
            "--ployzd-version",
            "0.1.0",
            "--ployzd-source",
            "/tmp/ployzd",
            "--ployzd-sha256",
            PLOYZ_NEWLINE_SHA256,
            "--ployzd-install-path",
            "/usr/local/bin/ployzd",
            "--ebpf-bytecode-version",
            "0.1.0",
            "--ebpf-bytecode-source",
            "/tmp/ployz-ebpf-tc",
            "--ebpf-bytecode-sha256",
            PLOYZ_NEWLINE_SHA256,
            "--ebpf-bytecode-install-path",
            "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            "--ebpf-ctl-version",
            "0.1.0",
            "--ebpf-ctl-source",
            "/tmp/ployz-ebpf-ctl",
            "--ebpf-ctl-sha256",
            PLOYZ_NEWLINE_SHA256,
            "--ebpf-ctl-install-path",
            "/usr/local/bin/ployz-ebpf-ctl",
            "--nats-version",
            "2.12.0",
            "--nats-source",
            "/tmp/nats-server",
            "--nats-sha256",
            PLOYZ_NEWLINE_SHA256,
            "--nats-binary",
            "/usr/local/bin/nats-server",
            "--nats-config",
            "/etc/nats/nats-server.conf",
        ]
        .map(str::to_owned),
    )
    .expect("init command accepts order-independent keeper binary");

    let PloyzctlCommand::Init(command) = command else {
        panic!("expected init command");
    };
    assert_eq!(command.node_id(), &node_id("node_1"));
}

#[test]
fn cli_init_rejects_old_activation_flag() {
    assert!(matches!(
        parse_command(["init", "--node", "node_1", "--activate-first-node"].map(str::to_owned)),
        Err(PloyzctlCliError::UnexpectedArgument { value }) if value == "--activate-first-node"
    ));
}

#[test]
fn cli_init_rejects_keeper_binary_with_emit_mode() {
    assert!(matches!(
        parse_command(
            [
                "init",
                "--node",
                "node_1",
                "--emit-keeper-install",
                "--keeper-binary",
                "/tmp/ployz-keeper",
                "--ployzd-version",
                "0.1.0",
                "--ployzd-source",
                "/tmp/ployzd",
                "--ployzd-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ployzd-install-path",
                "/usr/local/bin/ployzd",
                "--ebpf-bytecode-version",
                "0.1.0",
                "--ebpf-bytecode-source",
                "/tmp/ployz-ebpf-tc",
                "--ebpf-bytecode-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-bytecode-install-path",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                "--ebpf-ctl-version",
                "0.1.0",
                "--ebpf-ctl-source",
                "/tmp/ployz-ebpf-ctl",
                "--ebpf-ctl-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--ebpf-ctl-install-path",
                "/usr/local/bin/ployz-ebpf-ctl",
                "--nats-version",
                "2.12.0",
                "--nats-source",
                "/tmp/nats-server",
                "--nats-sha256",
                PLOYZ_NEWLINE_SHA256,
                "--nats-binary",
                "/usr/local/bin/nats-server",
                "--nats-config",
                "/etc/nats/nats-server.conf",
            ]
            .map(str::to_owned)
        ),
        Err(PloyzctlCliError::MissingRequiredArgument { flag })
            if flag == "--run-keeper-install"
    ));
}

#[test]
fn binary_rejects_unimplemented_commands() {
    let output = run_ployzctl(&["service"]);

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "unexpected argument: service\n");
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
fn binary_ops_status_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(["ops", "status", "op_deploy"])
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "--nats or PLOYZ_NATS_URL is required\n");
}

#[test]
fn binary_machine_list_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(["machine", "list"])
        .output()
        .expect("ployzctl binary runs");

    assert!(!output.status.success());
    assert_eq!(stdout(&output), "");
    assert_eq!(stderr(&output), "--nats or PLOYZ_NATS_URL is required\n");
}

#[test]
fn binary_machine_inspect_requires_nats_url() {
    let output = Command::new(env!("CARGO_BIN_EXE_ployzctl"))
        .env_remove("PLOYZ_NATS_URL")
        .args(["machine", "inspect", "node_2"])
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
                    outcome: ployz_core::ops::DeployCompletionOutcome::Completed,
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
fn ops_status_renders_operation_state_and_owner_lease() {
    let output = StatusOutput::new(OperationStatusSnapshot::new(
        OperationStatus::Deploy {
            id: operation_id("op_deploy"),
            service_id: ServiceId::try_new("svc_api").expect("valid service id"),
            state: DeployOperationState::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
            last_event_sequence: event_sequence(7),
        },
        OperationOwnershipStatus::Owned {
            lease: OperationOwnerLease::new(
                operation_id("op_deploy"),
                OperationOwnerId::try_new("control").expect("valid owner id"),
                OperationLeaseExpiresAt::try_new(120).expect("valid lease expiry"),
            ),
        },
    ))
    .render();

    assert_eq!(
        output,
        "operation op_deploy\nkind deploy\nservice svc_api\nstate running:waiting-for-health\nlast-event 7\nownership owned control expires-at 120\n"
    );
}

#[test]
fn ops_status_renders_unclaimed_machine_add() {
    let output = StatusOutput::new(OperationStatusSnapshot::new(
        OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            state: ployz_core::machine::MachineAddOperationState::Completed,
            last_event_sequence: event_sequence(9),
        },
        OperationOwnershipStatus::Unclaimed,
    ))
    .render();

    assert_eq!(
        output,
        "operation op_machine\nkind machine-add\nnode node_2 name edge_2 gateway skip\nstate completed\nlast-event 9\nownership unclaimed\n"
    );
}

#[test]
fn service_list_renders_service_summaries() {
    let output = ServiceListOutput {
        services: vec![
            service_snapshot("svc_api", "rev_2"),
            service_snapshot("svc_worker", "rev_1"),
        ],
    }
    .render();

    assert_eq!(
        output,
        "svc_api active-revision rev_2\nsvc_worker active-revision rev_1\n"
    );
}

#[test]
fn service_list_renders_no_output_without_services() {
    let output = ServiceListOutput {
        services: Vec::new(),
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn service_inspect_renders_active_revision() {
    let output = ServiceInspectOutput::new(service_snapshot("svc_api", "rev_2")).render();

    assert_eq!(output, "service svc_api\nactive-revision rev_2\n");
}

#[test]
fn machine_add_prints_bootstrap_command_without_nats_credentials() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
            .expect("valid runtime NATS URL"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    }
    .render();

    assert!(output.contains("operation op_machine"));
    assert!(output.contains("node node_2"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains(" | PLOYZ_NATS_URL='nats://127.0.0.1:7422' sh -s -- "));
    assert!(output.contains("--join-token 'join_once_123'"));
    assert!(!output.contains("creds"));
}

#[test]
fn machine_add_prints_runtime_nats_url_from_accepted_response() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7423")
            .expect("valid runtime NATS URL"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains(" | PLOYZ_NATS_URL='nats://127.0.0.1:7423' sh -s -- "));
    assert!(output.contains("--join-token 'join_once_123'"));
}

#[test]
fn machine_add_debug_redacts_join_token() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
            .expect("valid runtime NATS URL"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    };

    let debug = format!("{output:?}");

    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("join_once_123"));
    assert!(debug.contains("127.0.0.1"));
}

#[test]
fn machine_add_shell_quotes_join_material() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh/bootstrap?x='quoted'")
            .expect("valid bootstrap url"),
        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
            .expect("valid runtime NATS URL"),
        join_token: MachineJoinToken::try_new("join'quoted'").expect("valid join token"),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh/bootstrap?x='\\''quoted'\\'''"));
    assert!(output.contains("PLOYZ_NATS_URL='nats://127.0.0.1:7422'"));
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

#[test]
fn machine_list_renders_machine_summaries() {
    let output = MachineListOutput {
        machines: vec![machine_snapshot(
            "node_1",
            Some(GatewayServingStatus::Current),
        )],
    }
    .render();

    assert_eq!(
        output,
        "node_1 edge_1 public-ip 203.0.113.10 gateway current 127.0.0.1:8080 routes 2 containers 3\n"
    );
}

#[test]
fn machine_list_renders_no_output_without_machines() {
    let output = MachineListOutput {
        machines: Vec::new(),
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn machine_inspect_renders_machine_detail() {
    let output = MachineInspectOutput::new(machine_snapshot(
        "node_1",
        Some(GatewayServingStatus::LastKnownGood),
    ))
    .render();

    assert_eq!(
        output,
        "node node_1\nname edge_1\nactivated-by op_machine\npublic-ip 203.0.113.10\ngateway last-known-good 127.0.0.1:8080 routes 2\ncontainers 3\n"
    );
}

#[test]
fn machine_inspect_renders_missing_observations_as_unknown() {
    let mut machine = machine_snapshot("node_1", None);
    machine.public_ip = None;

    let output = MachineInspectOutput::new(machine).render();

    assert_eq!(
        output,
        "node node_1\nname edge_1\nactivated-by op_machine\npublic-ip unknown\ngateway none\ncontainers 3\n"
    );
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

fn service_snapshot(service_id: &str, revision_id: &str) -> ServiceSnapshot {
    ServiceSnapshot {
        active: ActiveServiceState {
            service_id: ServiceId::try_new(service_id).expect("valid service id"),
            active_revision: RevisionId::try_new(revision_id).expect("valid revision id"),
        },
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        route: None,
    }
}

fn machine_snapshot(node_id: &str, gateway: Option<GatewayServingStatus>) -> MachineSnapshot {
    let node_id = self::node_id(node_id);
    MachineSnapshot {
        active: ActiveMachineState {
            node_id: node_id.clone(),
            name: MachineName::try_new("edge_1").expect("valid machine name"),
            activated_by: operation_id("op_machine"),
        },
        public_ip: Some(NodePublicIpObservation {
            node_id: node_id.clone(),
            public_ip: "203.0.113.10".parse().expect("valid public ip"),
        }),
        gateway: gateway.map(|serving| GatewayStatusObservation {
            node_id,
            listen_addr: "127.0.0.1:8080".parse().expect("valid listen addr"),
            serving,
            route_count: 2,
        }),
        observed_container_count: 3,
    }
}

fn machine_add_args_with_gateway() -> impl Iterator<Item = String> {
    machine_add_arg_refs()
        .into_iter()
        .chain(["--gateway".to_owned()])
}

fn machine_add_args_without(flag: &str) -> Vec<String> {
    let mut args = machine_add_arg_refs();
    let Some(index) = args.iter().position(|value| value == flag) else {
        panic!("test machine add args include {flag}");
    };
    args.drain(index..=index + 1);
    args
}

fn machine_add_arg_refs() -> Vec<String> {
    vec![
        "machine".to_owned(),
        "add".to_owned(),
        "--node".to_owned(),
        "node_2".to_owned(),
        "--name".to_owned(),
        "edge_2".to_owned(),
        "--operation".to_owned(),
        "op_machine".to_owned(),
        "--idempotency-key".to_owned(),
        "idem_machine".to_owned(),
    ]
}

const PLOYZ_NEWLINE_SHA256: &str =
    "0cae9f85a05ca2a47cb515ab3554b071dc64fb3616abda8b3685d9141da11f2e";

fn init_with_keeper_install_args() -> impl Iterator<Item = String> {
    init_with_keeper_install_arg_refs()
        .into_iter()
        .map(str::to_owned)
}

fn init_with_keeper_install_arg_refs() -> [&'static str; 41] {
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
        "--ebpf-bytecode-version",
        "0.1.0",
        "--ebpf-bytecode-source",
        "/tmp/ployz-ebpf-tc",
        "--ebpf-bytecode-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ebpf-bytecode-install-path",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "--ebpf-ctl-version",
        "0.1.0",
        "--ebpf-ctl-source",
        "/tmp/ployz-ebpf-ctl",
        "--ebpf-ctl-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ebpf-ctl-install-path",
        "/usr/local/bin/ployz-ebpf-ctl",
        "--nats-version",
        "2.12.0",
        "--nats-source",
        "/tmp/nats-server",
        "--nats-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--nats-binary",
        "/usr/local/bin/nats-server",
        "--nats-config",
        "/etc/nats/nats-server.conf",
        "--machine-join-template-file",
        "/etc/ployz/machine-join-template.json",
    ]
}

fn init_with_keeper_run_arg_refs(keeper_binary: &str) -> Vec<&str> {
    vec![
        "init",
        "--node",
        "node_1",
        "--gateway",
        "--run-keeper-install",
        "--ployzd-version",
        "0.1.0",
        "--ployzd-source",
        "/tmp/ployzd",
        "--ployzd-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ployzd-install-path",
        "/usr/local/bin/ployzd",
        "--ebpf-bytecode-version",
        "0.1.0",
        "--ebpf-bytecode-source",
        "/tmp/ployz-ebpf-tc",
        "--ebpf-bytecode-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ebpf-bytecode-install-path",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "--ebpf-ctl-version",
        "0.1.0",
        "--ebpf-ctl-source",
        "/tmp/ployz-ebpf-ctl",
        "--ebpf-ctl-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--ebpf-ctl-install-path",
        "/usr/local/bin/ployz-ebpf-ctl",
        "--nats-version",
        "2.12.0",
        "--nats-source",
        "/tmp/nats-server",
        "--nats-sha256",
        PLOYZ_NEWLINE_SHA256,
        "--nats-binary",
        "/usr/local/bin/nats-server",
        "--nats-config",
        "/etc/nats/nats-server.conf",
        "--machine-join-template-file",
        "/etc/ployz/machine-join-template.json",
        "--keeper-binary",
        keeper_binary,
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

fn temp_dir(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{unique}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir can be created");
    dir
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .expect("fake keeper metadata can be read")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fake keeper can be executable");
}

#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}
